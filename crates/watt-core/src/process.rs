//! Processes: capturing this process's own stderr as events, process groups, detached launches.
//!
//! The op runner is a child process of the GUI, in its own process group. pwc's builder runs
//! Cargo with stdout and stderr both going to the op's stderr, so the op captures its *own* file
//! descriptor 2 ([`capture_stderr`]): a pipe replaces fd 2, a thread reads it, splits lines and
//! turns them into `Log` events (and Cargo progress lines into `Progress` events, switching the
//! phase from `Compile` to `Link` for the final fat-LTO link). Panics and messages from any
//! library therefore reach the front end as events too.
//!
//! Cancelling an op = the front end sends SIGTERM to the op's process group (which also holds
//! git, nix, cargo and rustc), then SIGKILL after a grace period ([`kill_group`]).

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::events::Reporter;

/// The running stderr capture. Dropping it restores the original stderr and joins the reader
/// after draining what is left.
#[cfg(unix)]
pub struct StderrCapture {
    /// A duplicate of the original fd 2, restored on drop.
    original: i32,
    /// Signalled by the reader thread right before it returns.
    done: std::sync::mpsc::Receiver<()>,
    /// Joined once `done` fires; dropped (detached) if the reader is still stuck reading.
    handle: Option<std::thread::JoinHandle<()>>,
}

/// No-op capture for non-Unix targets.
#[cfg(not(unix))]
pub struct StderrCapture {
    _private: (),
}

#[cfg(unix)]
impl Drop for StderrCapture {
    fn drop(&mut self) {
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        // SAFETY: `self.original` is a fd this capture owns exclusively (a `dup` of fd 2 made in
        // `capture_stderr`). `dup2` makes fd 2 refer to it again, closing whatever fd 2 currently
        // holds (our end of the pipe) first; `close` then releases our now-unneeded duplicate.
        unsafe {
            libc::dup2(self.original, 2);
            libc::close(self.original);
        }
        // The pipe's write end is now fully closed on our side, so the reader thread should see
        // EOF shortly — unless a grandchild process (started behind a wrapper) inherited the pipe
        // and still holds it open. Never let that block the op indefinitely: wait briefly, then
        // detach (drop the `JoinHandle` without joining; the thread finishes on its own later).
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok()
            && let Some(handle) = self.handle.take()
        {
            let _ = handle.join();
        }
    }
}

#[cfg(not(unix))]
impl Drop for StderrCapture {
    fn drop(&mut self) {}
}

/// Replace fd 2 with a pipe whose lines become events on `reporter`; every raw line is also
/// appended to `log` when given. Cargo progress lines become `Progress` events in the current
/// phase; when `progress::is_final_link` says so and the current phase is `Compile`, the reader
/// switches the reporter to `Phase::Link`. Unix only (on other platforms: a no-op that returns a
/// capture doing nothing).
#[cfg(unix)]
pub fn capture_stderr(reporter: Reporter, log: Option<std::fs::File>) -> Result<StderrCapture> {
    use std::io::Write as _;

    // Flush whatever Rust's own buffered stderr writer still holds before fd 2 changes meaning.
    let _ = std::io::stderr().flush();

    let mut fds = [0i32; 2];
    // SAFETY: `pipe(2)` only fills `fds` with two freshly allocated descriptors; the buffer is a
    // valid, correctly sized `&mut [i32; 2]`.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create a pipe");
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: `dup(2)` duplicates the currently valid fd 2; the result is a new fd this function
    // owns until it is handed to the capture (which closes it on drop).
    let original = unsafe { libc::dup(2) };
    if original < 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: both fds were just created above by this function and are not used elsewhere.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(err).context("cannot duplicate stderr");
    }

    // SAFETY: `dup2(write_fd, 2)` makes fd 2 an alias of `write_fd` (closing fd 2's previous
    // target first); both fds are valid and owned by this process.
    if unsafe { libc::dup2(write_fd, 2) } < 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: all three fds were created by this function and are not used elsewhere yet.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(original);
        }
        return Err(err).context("cannot redirect stderr");
    }
    // fd 2 now refers to the same pipe as `write_fd`; our separate handle to it is no longer
    // needed (closing it does not close fd 2, which holds its own reference to the same pipe).
    // SAFETY: `write_fd` was duplicated by `dup2` above and is not used anywhere else.
    unsafe { libc::close(write_fd) };

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let log = std::sync::Mutex::new(log);
    let thread_reporter = reporter.clone();
    let handle = std::thread::spawn(move || {
        use std::os::fd::FromRawFd as _;
        // SAFETY: `read_fd` was created by `pipe(2)` above, is not closed anywhere else, and is
        // handed to this thread exclusively; wrapping it in a `File` gives it an owning drop.
        let mut pipe = unsafe { std::fs::File::from_raw_fd(read_fd) };
        let mut splitter = crate::progress::LineSplitter::new();
        let mut buf = [0u8; 8192];
        loop {
            use std::io::Read as _;
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for line in splitter.feed(&buf[..n]) {
                        handle_line(&thread_reporter, &log, &line);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if let Some(line) = splitter.finish() {
            handle_line(&thread_reporter, &log, &line);
        }
        let _ = tx.send(());
    });

    Ok(StderrCapture {
        original,
        done: rx,
        handle: Some(handle),
    })
}

/// One captured line: append it to the log (when given) and turn it into an event. The reader
/// must never itself write to stderr (it would feed its own pipe).
#[cfg(unix)]
fn handle_line(reporter: &Reporter, log: &std::sync::Mutex<Option<std::fs::File>>, line: &str) {
    use std::io::Write as _;
    if let Some(file) = log.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(file, "{line}");
    }
    match crate::progress::parse_cargo_line(line) {
        crate::progress::CargoLine::Progress {
            done,
            total,
            active,
        } => {
            if reporter.current_phase() == Some(crate::events::Phase::Compile)
                && crate::progress::is_final_link(done, total, &active)
            {
                reporter.phase(crate::events::Phase::Link, false);
            }
            reporter.progress(done, Some(total), active.join(", "));
        }
        _ => reporter.tool_log(line.to_owned()),
    }
}

/// No-op on non-Unix targets.
#[cfg(not(unix))]
pub fn capture_stderr(reporter: Reporter, log: Option<std::fs::File>) -> Result<StderrCapture> {
    let _ = (reporter, log);
    Ok(StderrCapture { _private: () })
}

/// Configure `command` to start in a new process group of its own (pgid = its pid).
pub fn new_process_group(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
}

/// SIGTERM the process group `pgid`, wait up to `grace` for `child` to exit, then SIGKILL the
/// group. Reaps `child`.
pub fn kill_group(child: &mut Child, grace: Duration) -> Result<()> {
    let pgid = child.id() as i32;
    signal_group(pgid, Signal::Term);
    let deadline = std::time::Instant::now() + grace;
    loop {
        if child.try_wait()?.is_some() {
            // The leader is gone; make sure nothing it started survives it.
            signal_group(pgid, Signal::Kill);
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, Signal::Kill);
    child.wait()?;
    Ok(())
}

enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: i32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    if pgid > 1 {
        // SAFETY: kill(2) with a negative pid signals a process group; no memory is involved.
        unsafe {
            libc::kill(-pgid, sig);
        }
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: i32, _signal: Signal) {}

/// Start `command` fully detached (new session, stdin from /dev/null, stdout and stderr appended
/// to `log`), so it outlives the launcher. Returns its pid. The child is not reaped by us: a
/// detached double-fork is not needed because the op runner exits right after launching and the
/// game is re-parented.
pub fn launch_detached(command: &mut Command, log: &Path) -> Result<u32> {
    use anyhow::Context as _;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("cannot open {}", log.display()))?;
    let err = out.try_clone()?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid() is async-signal-safe and touches no memory of the parent.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = command.spawn().context("cannot start the game")?;
    Ok(child.id())
}

/// Whether a process with this pid exists and is not a zombie.
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // `/proc/<pid>/stat`: "pid (comm) S ..."; comm may contain spaces and parentheses.
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rfind(')')
                .and_then(|i| stat[i + 1..].split_whitespace().next())
                .is_some_and(|state| state != "Z" && state != "X"),
            Err(_) => false,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Lower this process's scheduling priority (children inherit it). `nice` 0..=19.
pub fn set_nice(nice: i32) {
    #[cfg(unix)]
    // SAFETY: setpriority(2) on this process; no memory is involved.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, nice.clamp(0, 19));
    }
    #[cfg(not(unix))]
    let _ = nice;
}
