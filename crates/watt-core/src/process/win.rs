//! Windows process primitives. Nothing here is compiled on Unix.
//!
//! # Op cancellation
//!
//! The GUI puts each op child in a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and
//! `JOB_OBJECT_LIMIT_BREAKAWAY_OK`. The job handle is owned by `OpHandle` and is not inheritable,
//! so a child cannot keep the job alive or escape it by accident. Cancelling calls
//! `TerminateJobObject`. Dropping the handle kills whatever is still in the job, which is how a
//! closed launcher does not leave `cargo` or `rustc` behind.
//!
//! `std::process::Command` does not return the primary thread handle, and assigning a running
//! process to a job races the children it has already spawned. The op is created with
//! `CREATE_SUSPENDED`. While that thread is suspended the process has exactly one thread, so
//! `CreateToolhelp32Snapshot` / `Thread32First` can find it. The process is assigned to the job
//! and only then resumed with `ResumeThread` / `OpenThread`.
//!
//! There is no soft "SIGTERM" for a `CREATE_NO_WINDOW` process (it has no console, and Ctrl-C
//! handling is a follow-up). Cancel therefore calls `TerminateJobObject` immediately and only
//! then waits for the leader to exit.
//!
//! # The game lock
//!
//! `LockFileEx` (what `File::lock` uses) is tied to the handle that took it, not to an inherited
//! file description the way Linux `flock` is. The op cannot hand the game a lock that outlives
//! the op. Instead the op starts the game with `CREATE_BREAKAWAY_FROM_JOB` (so closing the op's
//! job does not kill it) and starts a tiny guard, `watt-start op game-guard`, with the same
//! breakaway flag plus `DETACHED_PROCESS`. The guard opens the game (`OpenProcess(SYNCHRONIZE)`),
//! locks `game.lock`, writes `ok` to stdout, and blocks on `WaitForSingleObject`. When the game
//! exits the guard exits and the lock goes with it. The op keeps its own handle to the game until
//! the ack, so the pid the guard opens cannot have been reused, and it waits for the ack for at
//! most 15 seconds; on anything but `ok` it kills both. The op's instance `op.lock` is held the
//! whole time, so no second Play can slip in between the launch and the guard's lock.
//!
//! # Handle inheritance
//!
//! `std::process::Command` always passes a child its standard handles as fresh inheritable
//! duplicates, and calls `CreateProcess` with `bInheritHandles`, which also copies every *other*
//! inheritable handle. The op therefore clears the inherit bit on its own standard handles at
//! startup (`make_std_handles_private`), and the stderr pipe is created non-inheritable:
//! otherwise the detached game would hold the GUI's stdout/stderr pipes open for as long as it
//! runs.
//!
//! # Stderr
//!
//! Inside the op, `CreatePipe` plus `SetStdHandle(STD_ERROR_HANDLE)` redirects the Windows
//! standard error handle, and `_open_osfhandle` / `_dup2` redirect CRT fd 2 so the same pipe
//! receives Rust/CRT writes and the stderr of children (`cargo`, `rustc`, `git`). Stdout stays
//! the JSON protocol. See `capture_stderr` for who owns which handle. The symbols are declared
//! `extern "C"` and are provided by both the MSVC CRT (UCRT) and MinGW's CRT; `cargo check` does
//! not link them, so a real Windows link is still the proof.

use std::ffi::c_void;
use std::io::Read as _;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::Path;
use std::process::Child;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE, FreeLibrary, GENERIC_WRITE, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    OPEN_EXISTING, SYNCHRONIZE,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_SUSPENDED, GetCurrentProcess, IDLE_PRIORITY_CLASS,
    INFINITE, NORMAL_PRIORITY_CLASS, OpenProcess, OpenThread, ResumeThread, SetPriorityClass,
    THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
};

/// Re-exported so `process` can build creation-flag words. Each is a `u32` alias
/// (`PROCESS_CREATION_FLAGS`). `CREATE_NO_WINDOW` is `0x0800_0000`, matching the literal in
/// [`super::command_with_flags`].
pub(super) use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

use super::handle_line;
use crate::events::Reporter;

/// How long Play waits for the guard to confirm it holds `game.lock`.
const GUARD_ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// CRT flags. `_O_BINARY` so the pipe is not translated to CRLF.
const O_WRONLY: i32 = 0x0001;
const O_BINARY: i32 = 0x8000;

// The C runtime behind both the MSVC and MinGW toolchains Rust links. `_open_osfhandle` takes
// ownership of the handle on success and leaves it with the caller on failure. A doc comment
// cannot attach to an `extern` block.
unsafe extern "C" {
    fn _open_osfhandle(osfhandle: isize, flags: i32) -> i32;
    fn _dup(fd: i32) -> i32;
    fn _dup2(fd1: i32, fd2: i32) -> i32;
    fn _close(fd: i32) -> i32;
    fn _get_osfhandle(fd: i32) -> isize;
}

/// A non-inheritable job handle. Closing the last one kills processes still in the job.
pub struct Job(HANDLE);

// SAFETY: the handle is owned only here, and the job APIs may be called from any thread.
unsafe impl Send for Job {}
// SAFETY: `TerminateJobObject` and `CloseHandle` synchronise on the kernel object.
unsafe impl Sync for Job {}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the job handle `create_job` owns. Closing it is what
        // `KILL_ON_JOB_CLOSE` is for; it is not used again.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

impl Job {
    /// Kill every process still in the job. A no-op when the job is already empty.
    pub fn terminate(&self) {
        // SAFETY: `self.0` is a live job handle. The exit code is only what killed processes
        // report; nothing in this process is written.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

/// `KILL_ON_JOB_CLOSE` so the job dies with its last handle, `BREAKAWAY_OK` so the game and the
/// guard can leave it.
pub(super) fn create_job() -> Result<Job> {
    // SAFETY: a null name and null security attributes create an unnamed, non-inheritable job.
    // The returned handle is owned by the `Job` below.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error()).context("cannot create a job object");
    }
    // SAFETY: `job` was just created and is not inherited by anything yet. Clearing the inherit
    // bit is belt and braces; the default attributes are already non-inheritable.
    unsafe {
        SetHandleInformation(job, HANDLE_FLAG_INHERIT, 0);
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    // SAFETY: `info` is a correctly sized, fully zeroed structure with only `LimitFlags` set,
    // which is the documented way to set these two limits. `job` is the handle just created.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: we own `job` and are abandoning it.
        unsafe {
            CloseHandle(job);
        }
        return Err(err).context("cannot configure the job object");
    }
    Ok(Job(job))
}

/// Assign `child` (created suspended) to `job`, then resume its only thread.
/// On failure the process is terminated; the caller still has to reap `child`.
pub(super) fn assign_and_resume(child: &Child, job: &Job) -> Result<()> {
    let process = child.as_raw_handle() as HANDLE;
    // SAFETY: `process` is the child's process handle, owned by `child` (not closed here).
    // `job.0` is the job handle. Assigning does not transfer ownership of either.
    let assigned = unsafe { AssignProcessToJobObject(job.0, process) };
    if assigned == 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: `process` is the suspended child. Terminating it stops it escaping the job
        // we failed to put it in. `Child` still owns the handle.
        unsafe {
            TerminateProcess(process, 1);
        }
        return Err(err).context("cannot assign the op to its job");
    }
    if let Err(err) = resume_main_thread(child.id()) {
        // SAFETY: as above; the process is in the job but must not keep running unsuspended
        // supervision-free when we cannot resume it cleanly either. Terminate and report.
        unsafe {
            TerminateProcess(process, 1);
        }
        return Err(err);
    }
    Ok(())
}

/// The suspended process has one thread. Snapshot, open it, and resume it.
///
/// `CreateToolhelp32Snapshot` can fail with `ERROR_BAD_LENGTH` when the thread list changes
/// under it, and the new process's thread can be absent from a snapshot taken at the wrong
/// moment. Both are retried for up to two seconds; the last error is what the caller sees.
fn resume_main_thread(pid: u32) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let err = match find_thread(pid) {
            Ok(Some(thread_id)) => return resume_thread(thread_id),
            Ok(None) => anyhow::anyhow!("cannot find the suspended main thread of process {pid}"),
            Err(err) => err,
        };
        if std::time::Instant::now() >= deadline {
            return Err(err);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn resume_thread(thread_id: u32) -> Result<()> {
    // SAFETY: `OpenThread` with a zero inherit flag returns an owned handle or null.
    // `ResumeThread` drops the suspend count `CREATE_SUSPENDED` started at 1. The handle is
    // closed right after; the thread object stays with the process.
    unsafe {
        let thread = OpenThread(THREAD_SUSPEND_RESUME, FALSE, thread_id);
        if thread.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("cannot open the op's main thread");
        }
        let previous = ResumeThread(thread);
        // Capture before `CloseHandle`: it can overwrite the thread error.
        let resume_err = (previous == u32::MAX).then(std::io::Error::last_os_error);
        CloseHandle(thread);
        if let Some(err) = resume_err {
            return Err(err).context("cannot resume the op's main thread");
        }
    }
    Ok(())
}

/// `Ok(None)` when the snapshot worked but the process has no thread yet.
fn find_thread(pid: u32) -> Result<Option<u32>> {
    // SAFETY: a thread snapshot ignores the process id argument (pass 0). The returned snapshot
    // handle is closed before this function returns.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snap.is_null() || snap == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("cannot snapshot threads");
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut found = None;
    // SAFETY: `entry.dwSize` is set, which is what `Thread32First`/`Thread32Next` require.
    // `snap` is the snapshot just opened. Both calls only write into `entry`.
    unsafe {
        if Thread32First(snap, &mut entry) != 0 {
            loop {
                if entry.th32OwnerProcessID == pid {
                    found = Some(entry.th32ThreadID);
                    break;
                }
                entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    Ok(found)
}

/// Flags OR'd into [`super::command_with_flags`] for an op child.
pub(super) fn worker_flags(build_nice: Option<i32>) -> u32 {
    let mut flags = CREATE_SUSPENDED;
    if let Some(nice) = build_nice {
        flags |= priority_class(nice);
    }
    flags
}

/// 0 → normal, 1–9 → below normal, 10 and above → idle. Negative is treated as 0.
pub(super) fn priority_class(nice: i32) -> u32 {
    if nice <= 0 {
        NORMAL_PRIORITY_CLASS
    } else if nice < 10 {
        BELOW_NORMAL_PRIORITY_CLASS
    } else {
        IDLE_PRIORITY_CLASS
    }
}

pub(super) fn set_priority_class(nice: i32) {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that must not be closed.
    // `SetPriorityClass` changes this process only; children created afterwards inherit the class.
    unsafe {
        SetPriorityClass(GetCurrentProcess(), priority_class(nice));
    }
}

pub(super) fn total_memory_bytes() -> Option<u64> {
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `dwLength` is set to the struct size, which is the documented contract.
    // The call only writes into `status`.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then_some(status.ullTotalPhys)
}

pub(super) fn disk_free_bytes(path: &Path) -> Option<u64> {
    let wide = wide_path(path)?;
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated. The free-bytes out-parameter is a live `u64`.
    // The total-byte pointers are null, which the API allows when the caller does not want them.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

/// `true` when the Vulkan loader DLL is present. The driver installs it; the SDK is not required.
pub(super) fn vulkan_loader_present() -> bool {
    let name: Vec<u16> = "vulkan-1.dll"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 DLL name. `LoadLibraryW` returns a module handle
    // this function closes with `FreeLibrary` on success, and null (nothing to close) on failure.
    unsafe {
        let module = LoadLibraryW(name.as_ptr());
        if module.is_null() {
            false
        } else {
            FreeLibrary(module);
            true
        }
    }
}

pub(super) fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` returns an owned handle or null. `WaitForSingleObject` with a zero
    // timeout does not block: `WAIT_TIMEOUT` means the process is still running, `WAIT_OBJECT_0`
    // means it has exited. The handle is closed either way.
    unsafe {
        let process = OpenProcess(SYNCHRONIZE, FALSE, pid);
        if process.is_null() {
            return false;
        }
        let wait = WaitForSingleObject(process, 0);
        CloseHandle(process);
        wait == WAIT_TIMEOUT
    }
}

/// Hold `lock` until the process `pid` exits, after telling the parent `ok` on stdout.
pub(super) fn run_game_guard(pid: u32, lock_path: &Path) -> Result<()> {
    // Open the game first. The op keeps its own handle to the game until this guard has
    // answered, so the pid cannot have been reused yet: this handle is the game.
    // SAFETY: owned handle, closed on every return path below.
    let process = unsafe { OpenProcess(SYNCHRONIZE, FALSE, pid) };
    if process.is_null() {
        let err = std::io::Error::last_os_error();
        ack(&format!("cannot open the game process: {err}"));
        bail!("cannot open the game process: {err}");
    }
    let lock = match crate::locks::FileLock::try_acquire(lock_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            ack("busy");
            // SAFETY: `process` is the handle opened above and is not used again.
            unsafe { CloseHandle(process) };
            bail!("game lock is already held");
        }
        Err(err) => {
            ack(&format!("cannot lock: {err:#}"));
            // SAFETY: as above.
            unsafe { CloseHandle(process) };
            return Err(err);
        }
    };
    ack("ok");
    // SAFETY: `process` is the handle opened above. `INFINITE` blocks until the game exits.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        CloseHandle(process);
    }
    drop(lock);
    Ok(())
}

/// One line to the op. A closed pipe (the op gave up waiting) is not an error worth a panic.
fn ack(line: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Start `watt-start op game-guard` for `game` and wait until it has the lock. On failure, kill
/// the guard and the game. `game` stays open until the guard has answered, which is what makes
/// the guard's `OpenProcess(pid)` refer to this game and not to a later reuse of its pid.
pub(super) fn await_game_guard(game: &mut Child, lock: &Path) -> Result<()> {
    let pid = game.id();
    let fail = |game: &mut Child, err: anyhow::Error| -> Result<()> {
        let _ = game.kill();
        Err(err)
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return fail(
                game,
                anyhow::Error::new(err).context("cannot find watt-start"),
            );
        }
    };
    let mut guard = super::command_with_flags(exe, CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS);
    guard
        .args(["op", "game-guard", "--pid", &pid.to_string(), "--lock"])
        .arg(lock)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = match guard.spawn() {
        Ok(child) => child,
        Err(err) => {
            let err = breakaway_context(err, "cannot start the game guard");
            return fail(game, err);
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return fail(game, anyhow::anyhow!("the game guard has no stdout pipe"));
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        let mut line = String::new();
        let _ = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let failed = match rx.recv_timeout(GUARD_ACK_TIMEOUT) {
        Ok(line) if line.trim() == "ok" => None,
        Ok(line) if line.trim().is_empty() => {
            Some("the game guard exited without taking the lock".to_owned())
        }
        Ok(line) => Some(format!(
            "the game guard did not take the lock ({})",
            line.trim()
        )),
        Err(_) => Some(format!(
            "the game guard did not acknowledge the lock within {}s",
            GUARD_ACK_TIMEOUT.as_secs()
        )),
    };
    if let Some(message) = failed {
        let _ = child.kill();
        let _ = child.wait();
        return fail(game, anyhow::anyhow!(message));
    }
    // The guard holds the lock and is outside the op's job (`CREATE_BREAKAWAY_FROM_JOB`).
    // Dropping `child` only closes our handle to it.
    Ok(())
}

/// `CREATE_BREAKAWAY_FROM_JOB` fails with access denied when the launcher itself runs inside a
/// job that does not allow breakaway. Say so instead of a bare "Access is denied".
pub(super) fn breakaway_context(err: std::io::Error, what: &str) -> anyhow::Error {
    const ERROR_ACCESS_DENIED: i32 = 5;
    if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
        anyhow::Error::new(err).context(format!(
            "{what}: Windows refused to let it leave the launcher's job; the launcher was \
             probably started inside a job object that does not allow breakaway"
        ))
    } else {
        anyhow::Error::new(err).context(what.to_owned())
    }
}

/// Clear the inherit bit on this process's three standard handles.
///
/// `std::process::Command` always hands a child its standard handles through `STARTUPINFO`, as
/// fresh inheritable duplicates, so the inherit bit on our own copies is never what a child
/// needs. It is what leaks: `CreateProcess` with `bInheritHandles` copies every inheritable
/// handle, so a game started by the op would otherwise keep the op's stdout pipe (the GUI's
/// JSON stream) and stderr pipe open until the game exits, and the GUI would see the op as
/// running for that long. The handles the op was started with are inheritable because they were
/// inherited.
pub(super) fn make_std_handles_private() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `GetStdHandle` only reads the slot. `SetHandleInformation` on a valid handle
        // changes only its inherit flag; a null or invalid value is skipped.
        unsafe {
            let handle = GetStdHandle(id);
            if valid(handle) {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn valid(handle: HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

/// A non-inheritable duplicate of `handle` in this process, or `None`.
fn duplicate(handle: HANDLE) -> Option<HANDLE> {
    if !valid(handle) {
        return None;
    }
    let mut copy: HANDLE = std::ptr::null_mut();
    // SAFETY: `handle` is a live handle of this process. The duplicate is a new handle owned by
    // the caller.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut copy,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    (ok != 0 && valid(copy)).then_some(copy)
}

/// Put CRT fd 2 back to what `saved_fd` (a `_dup(2)`, or `-1` when there was no fd 2) names,
/// consuming `saved_fd`. This closes the CRT's handle to whatever fd 2 held.
///
/// # Safety
/// `saved_fd` is a descriptor the caller owns, or negative.
unsafe fn restore_crt_stderr(saved_fd: i32) {
    // SAFETY: per the contract. `_dup2` closes fd 2 before reusing it; `_close(2)` on a closed
    // fd 2 just fails.
    unsafe {
        if saved_fd >= 0 {
            _dup2(saved_fd, 2);
            _close(saved_fd);
        } else {
            _close(2);
        }
    }
}

/// Redirect stderr for the life of the returned guard. See the module docs.
///
/// Ownership, which is the part that is easy to get wrong:
/// - The CRT's fd 2 and the Win32 `STD_ERROR_HANDLE` slot normally hold the *same* handle value
///   (the CRT adopts the inherited handle without duplicating it). `_dup2` over fd 2 closes it,
///   so the slot's value cannot be restored afterwards; we restore a duplicate made up front.
/// - The CRT gets its own duplicate of the pipe's write end (`_open_osfhandle` adopts it); the
///   capture keeps the original write end and installs that one in the slot.
/// - Some CRTs call `SetStdHandle` from inside `_dup2` / `_close` for fds 0–2 (the UCRT does for
///   console-subsystem programs), some do not. The CRT is therefore always changed first and
///   `SetStdHandle` called last, so the slot ends up holding a handle we know is open.
/// - Nothing here is inheritable. Rust's `Command` gives a child an inheritable *duplicate* of
///   the current standard handle, so cargo, rustc and git still write into the pipe.
pub(super) fn capture_stderr(
    reporter: Reporter,
    log: Option<std::fs::File>,
) -> Result<StderrCapture> {
    use std::io::Write as _;
    let _ = std::io::stderr().flush();

    let mut read_end: HANDLE = std::ptr::null_mut();
    let mut write_end: HANDLE = std::ptr::null_mut();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: FALSE,
    };
    // SAFETY: both out-handles are live pointers. `attributes` asks for non-inheritable ends.
    // On failure neither handle is written, so there is nothing to close.
    if unsafe { CreatePipe(&mut read_end, &mut write_end, &attributes, 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create a pipe");
    }
    // SAFETY (used below): `read_end` and `write_end` are owned here until handed on.
    let close_pipe = || unsafe {
        CloseHandle(read_end);
        CloseHandle(write_end);
    };

    let Some(crt_handle) = duplicate(write_end) else {
        let err = std::io::Error::last_os_error();
        close_pipe();
        return Err(err).context("cannot duplicate the stderr pipe");
    };
    // SAFETY: `_dup(2)` duplicates CRT fd 2 when it is open; `-1` means there was none.
    // `GetStdHandle` only reads the slot.
    let saved_fd = unsafe { _dup(2) };
    let saved_std = duplicate(unsafe { GetStdHandle(STD_ERROR_HANDLE) });
    // SAFETY (used below): `saved_fd` and `saved_std` are owned here.
    let drop_saved = || unsafe {
        if saved_fd >= 0 {
            _close(saved_fd);
        }
        if let Some(handle) = saved_std {
            CloseHandle(handle);
        }
    };

    // SAFETY: `_open_osfhandle` adopts `crt_handle` on success; on failure it is still ours.
    // `_O_BINARY` avoids CRLF translation on the pipe.
    let crt_fd = unsafe { _open_osfhandle(crt_handle as isize, O_WRONLY | O_BINARY) };
    if crt_fd < 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: not adopted, so still owned here. fd 2 and the slot are untouched.
        unsafe { CloseHandle(crt_handle) };
        drop_saved();
        close_pipe();
        return Err(err).context("cannot redirect stderr");
    }
    // SAFETY: `crt_fd` is the descriptor just created. `_dup2` points fd 2 at a duplicate of its
    // handle (closing fd 2's previous handle); `_close(crt_fd)` then drops the extra descriptor.
    let redirected = unsafe { _dup2(crt_fd, 2) };
    let dup2_err = (redirected < 0).then(std::io::Error::last_os_error);
    unsafe { _close(crt_fd) };
    if let Some(err) = dup2_err {
        // A failed `_dup2` leaves fd 2 and the slot as they were.
        drop_saved();
        close_pipe();
        return Err(err).context("cannot redirect stderr");
    }
    // SAFETY: `write_end` stays open (owned by the capture) for as long as it is installed.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, write_end) } == 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: undo the CRT change and give the slot the original back.
        unsafe {
            restore_crt_stderr(saved_fd);
            SetStdHandle(STD_ERROR_HANDLE, saved_std.unwrap_or(std::ptr::null_mut()));
        }
        close_pipe();
        return Err(err).context("cannot redirect stderr");
    }

    // SAFETY: `read_end` is the pipe read handle, not inheritable, and is not closed anywhere
    // else from here. `File` owns it.
    let pipe = unsafe { std::fs::File::from_raw_handle(read_end) };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let log = std::sync::Mutex::new(log);
    let thread_reporter = reporter.clone();
    let handle = std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut splitter = crate::progress::LineSplitter::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for line in splitter.feed(&buf[..n]) {
                        handle_line(&thread_reporter, &log, &line);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                // `ERROR_BROKEN_PIPE`: every write end is closed.
                Err(_) => break,
            }
        }
        if let Some(line) = splitter.finish() {
            handle_line(&thread_reporter, &log, &line);
        }
        let _ = tx.send(());
    });

    Ok(StderrCapture {
        saved_fd,
        saved_std: saved_std.unwrap_or(std::ptr::null_mut()),
        write_end,
        done: rx,
        handle: Some(handle),
    })
}

/// Restores stderr on drop. See [`capture_stderr`].
pub struct StderrCapture {
    /// `_dup(2)` of the original CRT stderr, or `-1` when there was none.
    saved_fd: i32,
    /// Non-inheritable duplicate of the original `STD_ERROR_HANDLE`, or null. Installed back in
    /// the slot on drop, and owned by the slot from then on (never closed here).
    saved_std: HANDLE,
    /// The pipe's write end, installed as `STD_ERROR_HANDLE` while the capture lives.
    write_end: HANDLE,
    done: std::sync::mpsc::Receiver<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StderrCapture {
    fn drop(&mut self) {
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        // SAFETY: the fields are the handles `capture_stderr` handed over. CRT first (that
        // closes the CRT's copy of the write end and may itself touch the slot), then the slot,
        // then our write end, after which only children that inherited a copy keep the pipe open.
        unsafe {
            restore_crt_stderr(self.saved_fd);
            self.saved_fd = -1;
            SetStdHandle(STD_ERROR_HANDLE, self.saved_std);
            CloseHandle(self.write_end);
        }
        // Same bound as the Unix capture: a grandchild that inherited the write end must not
        // stall the op. Detach (drop the join handle) if the reader is still blocked.
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok()
            && let Some(handle) = self.handle.take()
        {
            let _ = handle.join();
        }
    }
}

/// For a GUI-subsystem binary started from a terminal: attach the parent's console and give
/// stdout and stderr a console handle where they have none. A handle that is already there (a
/// redirect to a file, the op child's JSON pipe, the console of a console-subsystem debug build)
/// is left alone, and when both are there nothing is attached at all.
///
/// Only the Win32 slots are set: Rust's `println!` / `eprintln!` look them up on every write.
/// CRT fds 1 and 2 are not redirected; nothing the CLI runs in-process prints through the CRT.
pub(super) fn attach_parent_console() {
    // SAFETY: `GetStdHandle` only reads the process's standard-handle slots.
    let missing = |id| !valid(unsafe { GetStdHandle(id) });
    if !missing(STD_OUTPUT_HANDLE) && !missing(STD_ERROR_HANDLE) {
        return;
    }
    // SAFETY: attaches to the parent's console. Failure (no parent console: Explorer, a
    // shortcut) is ignored; the output then goes nowhere rather than into a new console window.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // Attaching can fill empty slots by itself on newer Windows; only fill what is still empty.
        if missing(id)
            && let Some(console) = open_conout()
        {
            // SAFETY: `console` is a fresh `CONOUT$` handle; the slot owns it for the rest of
            // the process.
            unsafe { SetStdHandle(id, console) };
        }
    }
}

fn open_conout() -> Option<HANDLE> {
    let name: Vec<u16> = std::ffi::OsStr::new("CONOUT$")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `name` is NUL-terminated. The share mode lets us open a console another process
    // already has. The handle is owned by the caller.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        None
    } else {
        Some(handle)
    }
}

fn wide_path(path: &Path) -> Option<Vec<u16>> {
    let mut wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().collect();
    if wide.contains(&0) {
        return None;
    }
    wide.push(0);
    Some(wide)
}
