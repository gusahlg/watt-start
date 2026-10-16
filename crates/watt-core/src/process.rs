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

use anyhow::Result;

use crate::events::Reporter;

/// The running stderr capture. Dropping it restores the original stderr and joins the reader
/// after draining what is left.
pub struct StderrCapture {
    _private: (),
}

/// Replace fd 2 with a pipe whose lines become events on `reporter`; every raw line is also
/// appended to `log` when given. Cargo progress lines become `Progress` events in the current
/// phase; when `progress::is_final_link` says so and the current phase is `Compile`, the reader
/// switches the reporter to `Phase::Link`. Unix only (on other platforms: a no-op that returns a
/// capture doing nothing).
pub fn capture_stderr(reporter: Reporter, log: Option<std::fs::File>) -> Result<StderrCapture> {
    let _ = (reporter, log);
    todo!("worker B")
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
