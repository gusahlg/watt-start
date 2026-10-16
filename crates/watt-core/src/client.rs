//! Running an op as a child process and reading its events: what the GUI uses.
//!
//! [`OpHandle::spawn`] starts `<exe> op --events json --request <json>` in a new process group
//! with stdout piped (events) and stderr piped (anything the worker printed before its own
//! capture started, or after it broke: forwarded as log lines). A reader thread parses envelopes
//! into a channel; [`OpHandle::poll`] drains it into an [`OpView`]. [`OpHandle::cancel`] kills
//! the whole process group (git, nix, cargo, rustc included).

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::events::{Envelope, Event, LogStream, PROTOCOL};
use crate::ops::OpRequest;
use crate::view::OpView;

/// How long [`OpHandle::cancel`] waits for a clean exit before escalating to `SIGKILL`.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// A running (or finished) op child.
pub struct OpHandle {
    child: Arc<Mutex<Child>>,
    rx: Receiver<Envelope>,
    cancelled: Arc<AtomicBool>,
}

impl OpHandle {
    /// Spawn the worker. `exe` is normally `std::env::current_exe()`.
    pub fn spawn(exe: &Path, request: &OpRequest) -> Result<Self> {
        let mut command = Command::new(exe);
        command
            .args(request.to_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::process::new_process_group(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start {}", exe.display()))?;

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (tx, rx) = channel::<Envelope>();
        // The op id of the last envelope actually parsed from stdout, shared with both reader
        // threads so a stray line (stderr, or stdout before/after the protocol) can still be
        // folded into the right op's view instead of resetting it.
        let last_op_id = Arc::new(Mutex::new(String::new()));

        let tx_out = tx.clone();
        let op_id_out = Arc::clone(&last_op_id);
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
                if tx_out.send(envelope).is_err() {
                    break;
                }
            }
        });

        let op_id_err = Arc::clone(&last_op_id);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(synthetic_log(&op_id_err, line)).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            rx,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Apply every envelope that arrived to `view`; returns whether anything changed. When the
    /// child has exited and its stream is drained without `op-finished`, marks the view died
    /// (cancelled if [`OpHandle::cancel`] was called).
    pub fn poll(&mut self, view: &mut OpView) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(envelope) => {
                    view.apply(&envelope);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if view.outcome.is_none() {
                        view.mark_died(self.cancelled.load(Ordering::SeqCst));
                        changed = true;
                    }
                    break;
                }
            }
        }
        changed
    }

    /// Kill the op's process group (SIGTERM, SIGKILL after 3 s) on a background thread.
    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let child = Arc::clone(&self.child);
        std::thread::spawn(move || {
            if let Ok(mut child) = child.lock() {
                let _ = crate::process::kill_group(&mut child, CANCEL_GRACE);
            }
        });
    }

    /// Whether the child is still running.
    pub fn running(&mut self) -> bool {
        match self.child.lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(None)),
            Err(_) => false,
        }
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

#[cfg(test)]
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
}
