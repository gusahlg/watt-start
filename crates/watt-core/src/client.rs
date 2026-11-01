//! Running an op as a child process and reading its events: what the GUI uses.
//!
//! [`OpHandle::spawn`] starts `<exe> op --events json --request-stdin` and writes the request
//! JSON to the child's stdin (so a long module list is not a command-line argument). `op --request
//! <json>` remains the hand-run form ([`OpRequest::to_args`]). Stdout is piped (events) and
//! stderr is piped (anything the worker printed before its own capture started, or after it
//! broke: forwarded as log lines). A reader thread parses envelopes into a channel;
//! [`OpHandle::poll`] drains it into an [`OpView`].
//!
//! On Unix the child is its own process group and [`OpHandle::cancel`] sends SIGTERM, then
//! SIGKILL after a grace period. On Windows the child is created suspended, assigned to a job
//! object (`KILL_ON_JOB_CLOSE` and `BREAKAWAY_OK`) and then resumed; cancel calls
//! `TerminateJobObject` immediately, because a windowless process has no console to take a
//! soft signal. Neither path waits on the child while holding its mutex, so the GUI thread is
//! not blocked by [`OpHandle::poll`] or [`OpHandle::running`].

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use crate::events::{Envelope, Event, LogStream, PROTOCOL};
use crate::ops::OpRequest;
use crate::view::OpView;

/// Why a reader thread is asking the UI to wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpWake {
    /// `op-started`, a phase change, an outcome, or the reader exiting.
    Immediate,
    /// A log line, progress, a warning, or a state change. Coalesce these.
    Coalesce,
}

/// Installed by the UI after the first frame. Absent in tests that poll themselves.
pub type OpWaker = Arc<dyn Fn(OpWake) + Send + Sync>;

/// How a single event should wake the UI.
pub fn op_wake(event: &Event) -> OpWake {
    match event {
        Event::OpStarted { .. }
        | Event::PhaseStarted { .. }
        | Event::PhaseFinished { .. }
        | Event::OpFinished { .. } => OpWake::Immediate,
        Event::Log { .. }
        | Event::Progress { .. }
        | Event::Warning { .. }
        | Event::Changed { .. } => OpWake::Coalesce,
    }
}

struct WakeGate {
    waker: Option<OpWaker>,
    /// When the last coalesce wake was delivered. Further log lines inside the window share it.
    last_coalesce: Option<Instant>,
    /// A delayed coalesce is already sleeping, so a burst does not spawn a thread per line.
    follow_up: bool,
}

fn poke(gate: &Arc<Mutex<WakeGate>>, kind: OpWake) {
    let mut guard = gate.lock().unwrap_or_else(|err| err.into_inner());
    if guard.waker.is_none() {
        return;
    }
    match kind {
        OpWake::Immediate => {
            let waker = Arc::clone(guard.waker.as_ref().expect("checked above"));
            drop(guard);
            waker(OpWake::Immediate);
        }
        OpWake::Coalesce => {
            let now = Instant::now();
            let within = guard
                .last_coalesce
                .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(100));
            if within {
                if !guard.follow_up {
                    guard.follow_up = true;
                    let wait = Duration::from_millis(100).saturating_sub(
                        now.saturating_duration_since(guard.last_coalesce.unwrap_or(now)),
                    );
                    let gate = Arc::clone(gate);
                    drop(guard);
                    std::thread::spawn(move || {
                        std::thread::sleep(wait);
                        let mut guard = gate.lock().unwrap_or_else(|err| err.into_inner());
                        guard.follow_up = false;
                        guard.last_coalesce = Some(Instant::now());
                        let waker = guard.waker.clone();
                        drop(guard);
                        if let Some(waker) = waker {
                            waker(OpWake::Coalesce);
                        }
                    });
                }
                return;
            }
            guard.last_coalesce = Some(now);
            let waker = Arc::clone(guard.waker.as_ref().expect("checked above"));
            drop(guard);
            waker(OpWake::Coalesce);
        }
    }
}

/// How long [`OpHandle::cancel`] waits for a clean exit before escalating to `SIGKILL`.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// A running (or finished) op child.
pub struct OpHandle {
    child: Arc<Mutex<Child>>,
    /// The child's pid, which is also its process group id.
    #[cfg(unix)]
    pgid: i32,
    /// Kill-on-close job. The game and its guard break away; everything else dies with this handle.
    #[cfg(windows)]
    job: Arc<crate::process::Job>,
    rx: Receiver<Envelope>,
    cancelled: Arc<AtomicBool>,
    /// Shared with the reader threads. The UI installs a waker after the first frame.
    gate: Arc<Mutex<WakeGate>>,
}

impl OpHandle {
    /// Spawn the worker. `exe` is normally `std::env::current_exe()`.
    pub fn spawn(exe: &Path, request: &OpRequest) -> Result<Self> {
        Self::spawn_with_nice(exe, request, None)
    }

    /// [`OpHandle::spawn`], lowering the child's priority when `build_nice` is set and the
    /// request is a build. `None` and non-build ops (Play included) stay at normal priority.
    /// On Windows the class is a creation flag, so cargo and rustc inherit it. On Unix the op
    /// applies `set_nice` itself; the value is ignored here.
    pub fn spawn_with_nice(
        exe: &Path,
        request: &OpRequest,
        build_nice: Option<i32>,
    ) -> Result<Self> {
        let json = serde_json::to_string(request).context("cannot encode the op request")?;
        #[cfg(windows)]
        let flags = crate::process::worker_creation_flags(lowered_priority(request, build_nice));
        #[cfg(not(windows))]
        let flags = {
            let _ = (request, build_nice);
            0
        };
        let mut command = crate::process::command_with_flags(exe, flags);
        command
            .args(["op", "--events", "json", "--request-stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::process::new_process_group(&mut command);
        #[cfg(windows)]
        let job = Arc::new(crate::process::create_job()?);
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start {}", exe.display()))?;
        // Resume before anyone writes the request. A suspended child cannot drain the pipe, so
        // writing a request larger than the buffer first would deadlock.
        #[cfg(windows)]
        if let Err(err) = crate::process::assign_and_resume(&child, &job) {
            // The process was terminated inside `assign_and_resume`. Reap without blocking the
            // GUI thread if it does not exit; dropping `job` then kills whatever is left.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if child.try_wait().ok().flatten().is_some()
                    || std::time::Instant::now() >= deadline
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            return Err(err);
        }

        let mut stdin = child.stdin.take().expect("stdin is piped");
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = stdin.write_all(json.as_bytes());
            // Dropping `stdin` is the EOF the child reads to.
        });

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (tx, rx) = channel::<Envelope>();
        // The op id of the last envelope actually parsed from stdout, shared with both reader
        // threads so a stray line (stderr, or stdout before/after the protocol) can still be
        // folded into the right op's view instead of resetting it.
        let last_op_id = Arc::new(Mutex::new(String::new()));
        let gate = Arc::new(Mutex::new(WakeGate {
            waker: None,
            last_coalesce: None,
            follow_up: false,
        }));

        let tx_out = tx.clone();
        let op_id_out = Arc::clone(&last_op_id);
        let gate_out = Arc::clone(&gate);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let envelope = match serde_json::from_str::<Envelope>(&line) {
                    Ok(envelope) => {
                        *op_id_out.lock().unwrap_or_else(|e| e.into_inner()) =
                            envelope.op_id.clone();
                        envelope
                    }
                    Err(_) => synthetic_log(&op_id_out, line),
                };
                let kind = op_wake(&envelope.event);
                if tx_out.send(envelope).is_err() {
                    break;
                }
                poke(&gate_out, kind);
            }
            // Dropping the sender is what makes `poll` see `Disconnected`. Wake after that
            // so the UI reaps a child that exited without `op-finished`.
            drop(tx_out);
            poke(&gate_out, OpWake::Immediate);
        });

        let op_id_err = Arc::clone(&last_op_id);
        let gate_err = Arc::clone(&gate);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let envelope = synthetic_log(&op_id_err, line);
                let kind = op_wake(&envelope.event);
                if tx.send(envelope).is_err() {
                    break;
                }
                poke(&gate_err, kind);
            }
            drop(tx);
            poke(&gate_err, OpWake::Immediate);
        });

        #[cfg(unix)]
        let pgid = child.id() as i32;
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job,
            rx,
            cancelled: Arc::new(AtomicBool::new(false)),
            gate,
        })
    }

    /// Install (or replace) the UI waker. Reader threads that already exited have nothing to
    /// wake; [`OpHandle::poll`] still drains whatever they queued.
    pub fn set_waker(&self, waker: OpWaker) {
        self.gate
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .waker = Some(waker);
    }

    /// Whether [`OpHandle::set_waker`] has been called. Until then the UI has to poll.
    pub fn has_waker(&self) -> bool {
        self.gate
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .waker
            .is_some()
    }

    /// Apply every envelope that arrived to `view`; returns whether anything changed. When the
    /// child has exited and its stream is drained without `op-finished`, marks the view died
    /// (cancelled if [`OpHandle::cancel`] was called).
    pub fn poll(&mut self, view: &mut OpView) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(envelope) => {
                    view.apply_owned(envelope);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if view.outcome.is_none() {
                        view.mark_died(self.cancelled.load(Ordering::SeqCst));
                        changed = true;
                    }
                    // Both reader threads only exit once their pipe closes, which only happens
                    // once the child itself has exited (or exec'd away its own copies). Reap with
                    // `try_wait` so this never blocks the GUI thread. If the cancel thread holds
                    // the mutex for a moment, the next poll (or drop) reaps instead.
                    if let Ok(mut child) = self.child.try_lock() {
                        let _ = child.try_wait();
                    }
                    break;
                }
            }
        }
        changed
    }

    /// Stop the op on a background thread.
    ///
    /// Unix: SIGTERM, then SIGKILL after [`CANCEL_GRACE`]. Windows: `TerminateJobObject`
    /// immediately (there is no console to deliver a soft signal), then the same grace period
    /// to let the leader exit. The child mutex is only ever `try_lock`ed, so [`OpHandle::poll`]
    /// and [`OpHandle::running`] do not wait behind this thread.
    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let child = Arc::clone(&self.child);
        #[cfg(unix)]
        let pgid = self.pgid;
        #[cfg(windows)]
        let job = Arc::clone(&self.job);
        std::thread::spawn(move || {
            #[cfg(unix)]
            {
                use crate::process::{Signal, signal_group};
                signal_group(pgid, Signal::Term);
                let deadline = std::time::Instant::now() + CANCEL_GRACE;
                while !try_exited(&child) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                // Also when the leader exited: nothing it started may survive it.
                signal_group(pgid, Signal::Kill);
            }
            #[cfg(windows)]
            {
                job.terminate();
            }
            let deadline = std::time::Instant::now() + CANCEL_GRACE;
            while !try_exited(&child) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            // Best-effort reap. `try_wait` does not block; give the lock a moment if poll holds it.
            for _ in 0..40 {
                if let Ok(mut child) = child.try_lock() {
                    let _ = child.try_wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }

    /// Whether the child is still running. A mutex that is busy (the cancel thread looked at the
    /// same moment) counts as running, so the GUI does not treat a live op as finished.
    pub fn running(&mut self) -> bool {
        match self.child.try_lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(None)),
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(_)) => false,
        }
    }
}

/// `true` when the child has exited or the mutex is poisoned. `false` when it is still running
/// or another thread holds the mutex right now (the caller keeps waiting).
fn try_exited(child: &Mutex<Child>) -> bool {
    match child.try_lock() {
        Ok(mut child) => !matches!(child.try_wait(), Ok(None)),
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Poisoned(_)) => true,
    }
}

/// Build-like ops inherit a lowered priority. Play stays normal so the game does not.
#[cfg(windows)]
fn lowered_priority(request: &OpRequest, build_nice: Option<i32>) -> Option<i32> {
    let nice = build_nice?;
    match request {
        OpRequest::Build { .. }
        | OpRequest::Update { .. }
        | OpRequest::Modules { .. }
        | OpRequest::Create { .. }
        | OpRequest::Rollback { .. }
        | OpRequest::SetSource { .. } => Some(nice),
        _ => None,
    }
}

impl Drop for OpHandle {
    /// Guarantee the child is reaped even if the caller never calls [`OpHandle::poll`] again
    /// (or never did at all) after losing interest in this handle: a detached thread waits on it,
    /// so this never blocks the dropping thread even if the child is still running. On Windows
    /// the job handle is dropped with the struct, and `KILL_ON_JOB_CLOSE` ends anything still in
    /// the job before that wait returns.
    fn drop(&mut self) {
        let child = Arc::clone(&self.child);
        std::thread::spawn(move || {
            let mut child = loop {
                match child.try_lock() {
                    Ok(guard) => break guard,
                    Err(TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                }
            };
            let _ = child.wait();
        });
    }
}

/// An `Event::Log` envelope for a line that is not a protocol envelope (a stray stdout line, or
/// anything on stderr), tagged with the most recently known op id so it folds into the right view.
fn synthetic_log(last_op_id: &Mutex<String>, line: String) -> Envelope {
    let op_id = last_op_id.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Envelope {
        protocol: PROTOCOL,
        op_id,
        seq: 0,
        t_ms: 0,
        event: Event::Log {
            stream: LogStream::Launcher,
            line,
        },
    }
}

/// Shell scripts and `/proc` reap checks. A Windows client test (job kill, guard ack) needs a
/// machine to run on; audit item 45.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::events::{OpKind, OpOutcome, Phase};
    use crate::ops::OpRequest;
    use std::time::Instant;

    /// Writes an executable shell script and returns its path.
    fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// `OpHandle::spawn`, retrying briefly on `ETXTBSY`: on some filesystems a script that was
    /// just created and chmod'd a moment ago is transiently reported busy when another thread
    /// execs a different freshly-written file at the same time (seen under parallel test runs
    /// on overlay/tmpfs sandboxes). Production callers exec `current_exe()`, which is never in
    /// that state, so this retry lives only in the tests.
    fn spawn_with_retry(exe: &Path, request: &OpRequest) -> Result<OpHandle> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match OpHandle::spawn(exe, request) {
                Ok(handle) => return Ok(handle),
                Err(e) if Instant::now() < deadline && format!("{e:#}").contains("os error 26") => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn poll_until(handle: &mut OpHandle, view: &mut OpView, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            handle.poll(view);
            if view.outcome.is_some() || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn poll_folds_two_envelopes_into_a_finished_view() {
        let started = Envelope {
            protocol: PROTOCOL,
            op_id: "test-1".into(),
            seq: 0,
            t_ms: 0,
            event: Event::OpStarted {
                op: OpKind::Build,
                instance: Some("demo".into()),
                phases: vec![Phase::Compile],
                log: None,
            },
        };
        let finished = Envelope {
            protocol: PROTOCOL,
            op_id: "test-1".into(),
            seq: 1,
            t_ms: 10,
            event: Event::OpFinished {
                outcome: OpOutcome::Ok {
                    summary: "done".into(),
                },
                duration_ms: 10,
            },
        };
        let line1 = serde_json::to_string(&started).unwrap();
        let line2 = serde_json::to_string(&finished).unwrap();
        assert!(!line1.contains('\''));
        assert!(!line2.contains('\''));

        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(
            tmp.path(),
            "fake-op",
            &format!("echo '{line1}'\necho '{line2}'\n"),
        );

        let request = OpRequest::Build {
            instance: "demo".into(),
            force: false,
            profile: None,
        };
        let mut handle = spawn_with_retry(&script, &request).unwrap();
        let mut view = OpView::default();
        poll_until(&mut handle, &mut view, Duration::from_secs(5));

        assert_eq!(view.op, Some(OpKind::Build));
        assert_eq!(view.op_id, "test-1");
        assert!(matches!(view.outcome, Some(OpOutcome::Ok { .. })));
        assert!(!view.running());
        assert!(!handle.running());
    }

    #[test]
    fn unparsable_lines_become_log_events() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "fake-op", "echo 'not json at all'\n");
        let request = OpRequest::Build {
            instance: "demo".into(),
            force: false,
            profile: None,
        };
        let mut handle = spawn_with_retry(&script, &request).unwrap();
        let mut view = OpView::default();
        poll_until(&mut handle, &mut view, Duration::from_secs(5));

        // The child exited without ever emitting `op-finished`, so the stream is marked died.
        assert!(matches!(view.outcome, Some(OpOutcome::Failed { .. })));
        assert!(view.log.iter().any(|l| l == "not json at all"));
    }

    #[test]
    fn cancel_kills_the_child_and_marks_the_view_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "fake-op", "sleep 100\n");
        let request = OpRequest::Build {
            instance: "demo".into(),
            force: false,
            profile: None,
        };
        let mut handle = spawn_with_retry(&script, &request).unwrap();
        assert!(handle.running());

        handle.cancel();

        let mut view = OpView::default();
        poll_until(&mut handle, &mut view, Duration::from_secs(10));
        assert_eq!(view.outcome, Some(OpOutcome::Cancelled));
        assert!(!handle.running());
    }

    /// `poll` reaps the child itself once its pipes close, instead of leaving it a zombie until
    /// the launcher happens to call `running` (which GUI code using only `poll` never does).
    #[test]
    fn poll_reaps_the_child_without_running_being_called() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "fake-op", "exit 0\n");
        let request = OpRequest::Build {
            instance: "demo".into(),
            force: false,
            profile: None,
        };
        let mut handle = spawn_with_retry(&script, &request).unwrap();
        let pid = handle.child.lock().unwrap().id();

        let mut view = OpView::default();
        poll_until(&mut handle, &mut view, Duration::from_secs(5));

        // Reaped: /proc/<pid> no longer names a process at all (not merely a zombie).
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "child pid {pid} was not reaped"
        );
    }

    /// Dropping a handle nobody ever polled or cancelled must still reap its child eventually
    /// (a detached waiter thread), so abandoning a handle never leaves a zombie behind.
    #[test]
    fn drop_reaps_an_unpolled_child() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "fake-op", "exit 0\n");
        let request = OpRequest::Build {
            instance: "demo".into(),
            force: false,
            profile: None,
        };
        let handle = spawn_with_retry(&script, &request).unwrap();
        let pid = handle.child.lock().unwrap().id();
        drop(handle);

        let deadline = Instant::now() + Duration::from_secs(2);
        while std::path::Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "child pid {pid} was not reaped after drop"
        );
    }
}
