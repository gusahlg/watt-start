//! Processes: capturing this process's own stderr as events, process groups, detached launches.
//!
//! The op runner is a child process of the GUI. pwc's builder runs Cargo with stdout and stderr
//! both going to the op's stderr, so the op captures its *own* stderr ([`capture_stderr`]): a
//! pipe replaces it, a thread reads it, splits lines and turns them into `Log` events (and Cargo
//! progress lines into `Progress` events, switching the phase from `Compile` to `Link` for the
//! final fat-LTO link). Panics and messages from any library therefore reach the front end as
//! events too. Stdout stays the JSON protocol.
//!
//! On Unix the child is its own process group. Cancelling sends SIGTERM to that group (which also
//! holds git, nix, cargo and rustc), then SIGKILL after a grace period. The capture is `pipe2` /
//! `dup2` on fd 2. On Windows the child is created suspended, assigned to a job object
//! (`KILL_ON_JOB_CLOSE`, `BREAKAWAY_OK`) and then resumed; cancel calls `TerminateJobObject`.
//! The capture is `SetStdHandle` plus `_dup2` on CRT fd 2. See `win`.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

#[cfg(unix)]
use anyhow::Context as _;
use anyhow::Result;

use crate::events::Reporter;

#[cfg(windows)]
mod win;

/// `Command::new(program)`, and on Windows `CREATE_NO_WINDOW` so git, cargo, rustc and the op
/// child do not open a console. std still ORs `CREATE_UNICODE_ENVIRONMENT` at spawn.
///
/// `creation_flags` replaces the whole flag word, so anything that needs more flags (a suspended
/// worker, a breakaway game) must pass them through [`command_with_flags`] instead of calling
/// `creation_flags` again.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    command_with_flags(program, 0)
}

/// [`command`] with extra Windows creation flags OR'd in (`0` elsewhere).
pub fn command_with_flags(program: impl AsRef<OsStr>, extra_flags: u32) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // 0x08000000 CREATE_NO_WINDOW. Kept numeric so this helper does not depend on the
        // windows-sys module that the rest of the Windows process code lives in; the named
        // constants there must stay equal to this value.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW | extra_flags);
    }
    #[cfg(not(windows))]
    let _ = extra_flags;
    command
}

/// Free bytes available to this user on the volume that holds `path`.
/// Unix uses `statvfs` (`f_bavail` × `f_frsize`); Windows uses `GetDiskFreeSpaceExW`
/// (bytes available to the caller). `None` when the path cannot be queried.
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        unix_disk_free(path)
    }
    #[cfg(windows)]
    {
        win::disk_free_bytes(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

#[cfg(unix)]
fn unix_disk_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a NUL-terminated path this function owns. `st` is a live `statvfs`.
    // The call only writes into `st`.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut st) };
    if result != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

/// Whether `vulkan-1.dll` loads. The GPU driver installs it; the Vulkan SDK is not required.
#[cfg(windows)]
pub fn vulkan_loader_present() -> bool {
    win::vulkan_loader_present()
}

/// Total physical memory, in bytes. Linux reads `/proc/meminfo`; Windows calls
/// `GlobalMemoryStatusEx`. `None` when the platform does not say.
pub fn total_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib = text.lines().find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })?;
        Some(kib.saturating_mul(1024))
    }
    #[cfg(windows)]
    {
        crate::process::win::total_memory_bytes()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

/// Open (creating if needed) and exclusively lock `path`, then clear `FD_CLOEXEC` on the
/// descriptor so a process spawned right after (via [`launch_detached`]) inherits it — and with
/// it the lock, since `flock` follows the open file description: the lock is released only once
/// every holder of that description has closed its copy. Fails (rather than blocking) if the
/// lock is already held. Keep the returned file alive until after `Command::spawn()`; dropping
/// it immediately after is fine, since the spawned child's inherited copy of the descriptor
/// keeps the lock held for as long as that child lives.
#[cfg(unix)]
pub fn acquire_inheritable_lock(path: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    file.try_lock()
        .with_context(|| format!("{} is already locked", path.display()))?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid descriptor this function owns exclusively (just opened above);
    // `fcntl` with `F_GETFD`/`F_SETFD` only touches the kernel's descriptor-table entry for it,
    // never any memory of this process.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }
    Ok(file)
}

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

/// Real capture on Windows (a pipe behind `STD_ERROR_HANDLE` and CRT fd 2). See `win`.
#[cfg(windows)]
pub use win::StderrCapture;

/// No-op capture where there is neither a Unix fd 2 nor a Windows stderr handle to replace.
#[cfg(all(not(unix), not(windows)))]
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

#[cfg(all(not(unix), not(windows)))]
impl Drop for StderrCapture {
    fn drop(&mut self) {}
}

/// Replace fd 2 with a pipe whose lines become events on `reporter`; every raw line is also
/// appended to `log` when given. Cargo progress lines become `Progress` events in the current
/// phase; when `progress::is_final_link` says so and the current phase is `Compile`, the reader
/// switches the reporter to `Phase::Link`. Unix replaces fd 2. Windows replaces
/// `STD_ERROR_HANDLE` and CRT fd 2 (see `win`). Anywhere else this is a no-op.
#[cfg(unix)]
pub fn capture_stderr(reporter: Reporter, log: Option<std::fs::File>) -> Result<StderrCapture> {
    use std::io::Write as _;

    // Flush whatever Rust's own buffered stderr writer still holds before fd 2 changes meaning.
    let _ = std::io::stderr().flush();

    let mut fds = [0i32; 2];
    // SAFETY: `pipe2(2)` only fills `fds` with two freshly allocated descriptors; the buffer is a
    // valid, correctly sized `&mut [i32; 2]`. `O_CLOEXEC` keeps both ends private to this
    // process: a child spawned while the capture is active (cargo, the wrapped game) must never
    // inherit our read end or it could hold the pipe open past this process's own exit.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create a pipe");
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` duplicates the currently valid fd 2 into a close-on-exec
    // descriptor; the result is a new fd this function owns until it is handed to the capture
    // (which closes it on drop). Unlike `write_fd` below, this one must never leak into a child
    // either: it is only this process's private backup of the original stderr target.
    let original = unsafe { libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 0) };
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
    // target first); both fds are valid and owned by this process. `dup2`'s target descriptor is
    // always created without `FD_CLOEXEC` regardless of the source's flag, so fd 2 itself stays
    // inheritable — exactly the one descriptor a wrapped build or the game is meant to inherit.
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
#[cfg(any(unix, windows))]
pub(super) fn handle_line(
    reporter: &Reporter,
    log: &std::sync::Mutex<Option<std::fs::File>>,
    line: &str,
) {
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

/// Windows: a pipe replaces `STD_ERROR_HANDLE` and CRT fd 2. See `win::capture_stderr`.
#[cfg(windows)]
pub fn capture_stderr(reporter: Reporter, log: Option<std::fs::File>) -> Result<StderrCapture> {
    win::capture_stderr(reporter, log)
}

/// No-op where stderr cannot be redirected.
#[cfg(all(not(unix), not(windows)))]
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

/// The two signals ops are stopped with.
#[derive(Clone, Copy, Debug)]
pub enum Signal {
    /// Ask politely (SIGTERM).
    Term,
    /// Force (SIGKILL).
    Kill,
}

/// Send `signal` to the process group `pgid` (a no-op for pgid 0 or 1, and off Unix).
#[cfg(unix)]
pub fn signal_group(pgid: i32, signal: Signal) {
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

/// Send `signal` to the process group `pgid` (a no-op for pgid 0 or 1, and off Unix).
#[cfg(not(unix))]
pub fn signal_group(_pgid: i32, _signal: Signal) {}

/// Start `command` fully detached (new session, stdin from /dev/null, stdout and stderr appended
/// to `log`), so it outlives the launcher. Returns its pid. The child is not reaped by us: a
/// detached double-fork is not needed because the op runner exits right after launching and the
/// game is re-parented.
pub fn launch_detached(command: &mut Command, log: &Path) -> Result<u32> {
    Ok(spawn_detached(command, log)?.id())
}

/// [`launch_detached`], keeping the `Child`. On Windows its process handle is what keeps the
/// pid from being reused until the game guard has opened the game itself.
pub fn spawn_detached(command: &mut Command, log: &Path) -> Result<Child> {
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
        command.spawn().context("cannot start the game")
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Break out of the op's kill-on-close job. DETACHED_PROCESS keeps a console subsystem
        // game from attaching to the op's console; CREATE_NO_WINDOW is redundant with it (the
        // API ignores CREATE_NO_WINDOW when DETACHED_PROCESS is set) and is included because
        // that is the flag every other background spawn uses. No priority class: the game gets
        // the normal class even if the op runs lowered.
        command.creation_flags(
            win::CREATE_NO_WINDOW | win::CREATE_BREAKAWAY_FROM_JOB | win::DETACHED_PROCESS,
        );
        command
            .spawn()
            .map_err(|err| win::breakaway_context(err, "cannot start the game"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        command.spawn().context("cannot start the game")
    }
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
    #[cfg(windows)]
    {
        win::is_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Lower this process's scheduling priority (children inherit it). `nice` 0..=19.
/// On Windows the value is mapped onto a priority class: 0 stays normal, 1–9 is below normal,
/// 10 and above is idle.
pub fn set_nice(nice: i32) {
    #[cfg(unix)]
    // SAFETY: setpriority(2) on this process; no memory is involved.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, nice.clamp(0, 19));
    }
    #[cfg(windows)]
    win::set_priority_class(nice);
    #[cfg(not(any(unix, windows)))]
    let _ = nice;
}

/// Attach to the parent console when this process has no stdout handle of its own.
/// No-op off Windows, and a no-op when stdout is already a real handle (the op child's JSON
/// pipe must not be replaced).
pub fn attach_parent_console() {
    #[cfg(windows)]
    win::attach_parent_console();
}

/// Windows creation flags for an op child: suspended, so the caller can assign the job before
/// the process runs, plus the priority class when `build_nice` is set. `CREATE_NO_WINDOW` is
/// added by [`command_with_flags`].
#[cfg(windows)]
pub fn worker_creation_flags(build_nice: Option<i32>) -> u32 {
    win::worker_flags(build_nice)
}

/// The job that owns one op child. Closing the last handle kills every process still in the job.
/// The game and its guard break away and are not affected.
#[cfg(windows)]
pub use win::Job;

/// Create the op's job (`KILL_ON_JOB_CLOSE` and `BREAKAWAY_OK`, non-inheritable).
#[cfg(windows)]
pub fn create_job() -> Result<Job> {
    win::create_job()
}

/// Put a `CREATE_SUSPENDED` child into `job` and resume its primary thread.
#[cfg(windows)]
pub fn assign_and_resume(child: &Child, job: &Job) -> Result<()> {
    win::assign_and_resume(child, job)
}

/// `watt-start op game-guard`: hold `lock` until `pid` exits. The Windows play path uses this
/// because a `LockFileEx` lock does not travel with an inherited handle the way `flock` does.
pub fn run_game_guard(pid: u32, lock: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        win::run_game_guard(pid, lock)
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, lock);
        anyhow::bail!("game-guard only runs on Windows")
    }
}

/// Spawn the guard for `game` and wait until it has the lock (or fail, killing the game and the
/// guard). See `win`.
#[cfg(windows)]
pub fn await_game_guard(game: &mut Child, lock: &Path) -> Result<()> {
    win::await_game_guard(game, lock)
}

/// Windows: clear the inherit bit on this process's standard handles, so a detached game does
/// not keep the op's stdout and stderr pipes open (std hands every child fresh duplicates of
/// the standard handles anyway). A no-op elsewhere: Unix `dup2`s the game's fds 1 and 2 over.
pub fn make_std_handles_private() {
    #[cfg(windows)]
    win::make_std_handles_private();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::events::{CancelToken, VecSink};
    use std::sync::Arc;

    /// A child spawned while the capture is active, even one that redirects its own stderr
    /// elsewhere, must never inherit our pipe's read end or our saved original fd 2 (the close-
    /// on-exec fix): if it did, a long-lived child (a detached game, in production) would hold
    /// those descriptors open, and dropping the capture would have to wait for it.
    #[test]
    fn dropping_the_capture_does_not_wait_on_an_unrelated_child() {
        let sink = Arc::new(VecSink::default());
        let reporter = Reporter::new(sink, "t", CancelToken::default());
        let capture = capture_stderr(reporter, None).expect("capture_stderr");

        let mut child = Command::new("sleep")
            .arg("5")
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("cannot spawn sleep");

        let started = std::time::Instant::now();
        drop(capture);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "dropping the capture waited on an unrelated child ({:?})",
            started.elapsed()
        );

        let _ = child.kill();
        let _ = child.wait();
    }
}
