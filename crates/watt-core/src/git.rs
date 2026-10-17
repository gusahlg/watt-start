//! Git: bare mirrors of the three repositories, revision lookup, pins and history.
//!
//! Every function runs the `git` executable (no libgit2): `git` is required anyway, since pwc
//! records revisions with it. Commands run with `GIT_TERMINAL_PROMPT=0` (never block on a password
//! prompt), `LC_ALL=C` (parseable messages) and stdin closed.
//!
//! Mirror layout (`Paths::mirror(repo)`): a bare repository with `origin` = the configured remote
//! and fetch refspecs `+refs/heads/*:refs/heads/*` and `+refs/tags/*:refs/tags/*`, so branch and
//! tag names resolve directly. Pinned revisions are kept alive under `refs/pins/<sha>`, which
//! `fetch --prune` leaves alone (a force-pushed branch never takes a pinned commit with it).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use crate::events::RepoLog;
use crate::paths::Repo;

/// Progress of a git transfer, parsed from `--progress` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitProgress {
    /// "Receiving objects", "Resolving deltas", "Updating files"...
    pub stage: String,
    /// 0..=100.
    pub percent: u8,
    /// Objects done, when printed.
    pub done: Option<u64>,
    /// Objects in total, when printed.
    pub total: Option<u64>,
}

/// One branch of a mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// Branch name ("main", "grok/morph").
    pub name: String,
    /// Tip commit.
    pub sha: String,
    /// Tip commit time, Unix seconds.
    pub time: i64,
    /// Tip commit summary line.
    pub summary: String,
}

/// How many trailing stderr lines are kept for error messages.
const TAIL_LINES: usize = 20;

/// A `git` command with the environment every call needs.
///
/// `-c` is before the subcommand, so a failure that quotes `args` still names the operation the
/// caller asked for. `core.autocrlf` / `core.eol` keep snapshot bytes identical on Windows.
/// `core.longpaths` matters only on Windows and is ignored elsewhere.
fn base_command(args: &[&str]) -> Command {
    let mut cmd = crate::process::command("git");
    cmd.args([
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-c",
        "core.longpaths=true",
    ]);
    cmd.args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("LC_ALL", "C");
    cmd.stdin(Stdio::null());
    cmd
}

/// Run `git` with `args` in `cwd` (the process's own directory when `None`), returning stdout.
/// On failure, the error names the command and the last lines of stderr.
fn run(cwd: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = base_command(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd
        .output()
        .with_context(|| format!("cannot run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("exit status {}", output.status)).with_context(|| {
            format!(
                "`git {}` failed:\n{}",
                args.join(" "),
                tail_lines(&stderr, TAIL_LINES)
            )
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `git` with `args` in `cwd`, streaming its stderr as progress and log lines. The last lines
/// of stderr are kept so a failure's error names the command and them.
fn run_with_progress(
    cwd: Option<&Path>,
    args: &[&str],
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<()> {
    let mut cmd = base_command(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run `git {}`", args.join(" ")))?;
    let mut stderr = child.stderr.take().expect("stderr was configured as piped");
    let mut splitter = crate::progress::LineSplitter::new();
    let mut tail: Vec<String> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stderr
            .read(&mut buf)
            .with_context(|| format!("cannot read the output of `git {}`", args.join(" ")))?;
        if n == 0 {
            break;
        }
        for line in splitter.feed(&buf[..n]) {
            dispatch_line(line, on_progress, on_line, &mut tail);
        }
    }
    if let Some(line) = splitter.finish() {
        dispatch_line(line, on_progress, on_line, &mut tail);
    }
    let status = child
        .wait()
        .with_context(|| format!("cannot wait for `git {}`", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow::anyhow!("exit status {status}"))
            .with_context(|| format!("`git {}` failed:\n{}", args.join(" "), tail.join("\n")));
    }
    Ok(())
}

/// Classify one stderr line as progress or a plain log line, keeping it in `tail`.
fn dispatch_line(
    line: String,
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
    tail: &mut Vec<String>,
) {
    tail.push(line.clone());
    if tail.len() > TAIL_LINES {
        tail.remove(0);
    }
    match crate::progress::parse_git_progress(&line) {
        Some(progress) => on_progress(progress),
        None => on_line(line),
    }
}

/// The last (at most) `n` lines of `text`, joined back with `\n`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// `git rev-parse --verify --quiet <rev>` in `mirror`: the full hash, or `None` if `rev` does not
/// resolve. `--quiet` makes "not found" silent, so any non-zero exit means "not found".
fn rev_parse_verify(mirror: &Path, rev: &str) -> Result<Option<String>> {
    let mut cmd = base_command(&["rev-parse", "--verify", "--quiet", rev]);
    cmd.current_dir(mirror);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd
        .output()
        .with_context(|| format!("cannot run `git rev-parse --verify --quiet {rev}`"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

/// Create the mirror of `repo` at `mirror` if it does not exist (bare clone of `remote`, with the
/// refspecs above), or point its `origin` at `remote` if that changed. Does not fetch an existing
/// mirror. Progress lines go to `on_progress`; other git output lines go to `on_line`.
/// Creation is atomic: clone into a temporary sibling, then rename.
pub fn ensure_mirror(
    mirror: &Path,
    remote: &str,
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<()> {
    if mirror.is_dir() {
        let current = run(Some(mirror), &["config", "--get", "remote.origin.url"])
            .with_context(|| format!("cannot read the origin of {}", mirror.display()))?;
        if current.trim() != remote {
            run(Some(mirror), &["remote", "set-url", "origin", remote])
                .with_context(|| format!("cannot point {} at {remote}", mirror.display()))?;
        }
        return Ok(());
    }
    let parent = mirror
        .parent()
        .with_context(|| format!("{} has no parent directory", mirror.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let file_name = mirror
        .file_name()
        .with_context(|| format!("{} has no file name", mirror.display()))?
        .to_string_lossy()
        .into_owned();
    let tmp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    if tmp.exists() {
        remove_dir_all_writable(&tmp)?;
    }
    let tmp_str = tmp.to_string_lossy().into_owned();
    run_with_progress(
        None,
        &["clone", "--bare", "--progress", remote, &tmp_str],
        on_progress,
        on_line,
    )
    .with_context(|| format!("cannot clone {remote} into {}", tmp.display()))?;
    run(
        Some(&tmp),
        &[
            "config",
            "--replace-all",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ],
    )
    .with_context(|| format!("cannot configure the fetch refspec of {}", tmp.display()))?;
    run(
        Some(&tmp),
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    )
    .with_context(|| format!("cannot configure the fetch refspec of {}", tmp.display()))?;
    std::fs::rename(&tmp, mirror)
        .with_context(|| format!("cannot rename {} to {}", tmp.display(), mirror.display()))?;
    Ok(())
}

/// `git fetch --prune --progress origin` in the mirror. Returns whether any ref changed.
pub fn fetch(
    mirror: &Path,
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<bool> {
    let before = run(Some(mirror), &["for-each-ref"])
        .with_context(|| format!("cannot list the refs of {}", mirror.display()))?;
    run_with_progress(
        Some(mirror),
        &["fetch", "--prune", "--progress", "origin"],
        on_progress,
        on_line,
    )
    .with_context(|| format!("cannot fetch {}", mirror.display()))?;
    let after = run(Some(mirror), &["for-each-ref"])
        .with_context(|| format!("cannot list the refs of {}", mirror.display()))?;
    Ok(before != after)
}

/// Resolve a branch, tag or (abbreviated) commit to a full commit hash; `None` if unknown.
/// Branches win over tags with the same name.
pub fn resolve(mirror: &Path, reference: &str) -> Result<Option<String>> {
    for candidate in [
        format!("refs/heads/{reference}"),
        format!("refs/tags/{reference}^{{commit}}"),
        format!("{reference}^{{commit}}"),
    ] {
        if let Some(sha) = rev_parse_verify(mirror, &candidate)
            .with_context(|| format!("cannot resolve `{reference}` in {}", mirror.display()))?
        {
            return Ok(Some(sha));
        }
    }
    Ok(None)
}

/// The branch `HEAD` points at in the mirror ("main").
pub fn default_branch(mirror: &Path) -> Result<String> {
    let out = run(Some(mirror), &["symbolic-ref", "--short", "HEAD"])
        .with_context(|| format!("cannot read the default branch of {}", mirror.display()))?;
    Ok(out.trim().to_owned())
}

/// Every branch, newest tip first.
pub fn branches(mirror: &Path) -> Result<Vec<BranchInfo>> {
    let out = run(
        Some(mirror),
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%1f%(objectname)%1f%(committerdate:unix)%1f%(contents:subject)",
            "refs/heads",
        ],
    )
    .with_context(|| format!("cannot list the branches of {}", mirror.display()))?;
    let mut branches = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(4, '\u{1f}');
        let name = parts.next().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let sha = parts.next().unwrap_or_default();
        let time = parts.next().unwrap_or_default().parse().unwrap_or(0);
        let summary = parts.next().unwrap_or_default();
        branches.push(BranchInfo {
            name: name.to_owned(),
            sha: sha.to_owned(),
            time,
            summary: summary.to_owned(),
        });
    }
    Ok(branches)
}

/// Every tag name, newest first (by creator date).
pub fn tags(mirror: &Path) -> Result<Vec<String>> {
    let out = run(
        Some(mirror),
        &[
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname:short)",
            "refs/tags",
        ],
    )
    .with_context(|| format!("cannot list the tags of {}", mirror.display()))?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Keep `sha` alive: `refs/pins/<sha>` → `sha`. Idempotent.
pub fn pin(mirror: &Path, sha: &str) -> Result<()> {
    let refname = format!("refs/pins/{sha}");
    run(Some(mirror), &["update-ref", &refname, sha])
        .with_context(|| format!("cannot pin {sha} in {}", mirror.display()))?;
    Ok(())
}

/// Delete every `refs/pins/*` whose commit is not in `keep`.
pub fn unpin_except(mirror: &Path, keep: &std::collections::BTreeSet<String>) -> Result<()> {
    let out = run(
        Some(mirror),
        &[
            "for-each-ref",
            "--format=%(refname)%1f%(objectname)",
            "refs/pins",
        ],
    )
    .with_context(|| format!("cannot list the pins of {}", mirror.display()))?;
    for line in out.lines() {
        let mut parts = line.splitn(2, '\u{1f}');
        let refname = parts.next().unwrap_or_default();
        let sha = parts.next().unwrap_or_default();
        if refname.is_empty() || keep.contains(sha) {
            continue;
        }
        run(Some(mirror), &["update-ref", "-d", refname])
            .with_context(|| format!("cannot delete {refname} in {}", mirror.display()))?;
    }
    Ok(())
}

/// Commits in `new` but not in `old` (`git log old..new`), newest first, at most `max`
/// (`truncated` says whether there were more). `old = None` lists the newest `max` commits of
/// `new`. An `old` that is not an ancestor (force push, branch switch) still works: it is the
/// symmetric `old..new` set git defines.
pub fn log_between(
    repo: Repo,
    mirror: &Path,
    old: Option<&str>,
    new: &str,
    max: usize,
) -> Result<RepoLog> {
    let range = match old {
        Some(old) => format!("{old}..{new}"),
        None => new.to_owned(),
    };
    let n = (max + 1).to_string();
    let out = run(
        Some(mirror),
        &["log", "--format=%H%x1f%s%x1f%an%x1f%ct", "-n", &n, &range],
    )
    .with_context(|| format!("cannot list commits {range} in {}", mirror.display()))?;
    let mut commits = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(4, '\u{1f}');
        let sha = parts.next().unwrap_or_default();
        if sha.is_empty() {
            continue;
        }
        let summary = parts.next().unwrap_or_default();
        let author = parts.next().unwrap_or_default();
        let time = parts.next().unwrap_or_default().parse().unwrap_or(0);
        commits.push(crate::events::Commit {
            sha: sha.to_owned(),
            summary: summary.to_owned(),
            author: author.to_owned(),
            time,
        });
    }
    let truncated = commits.len() > max;
    commits.truncate(max);
    Ok(RepoLog {
        repo,
        commits,
        truncated,
    })
}

/// The contents of `path` at `sha` (`git show sha:path`), `None` if the file does not exist there.
pub fn file_at(mirror: &Path, sha: &str, path: &str) -> Result<Option<String>> {
    if rev_parse_verify(mirror, &format!("{sha}^{{commit}}"))
        .with_context(|| format!("cannot verify {sha} in {}", mirror.display()))?
        .is_none()
    {
        bail!("unknown revision `{sha}` in {}", mirror.display());
    }
    let spec = format!("{sha}:{path}");
    match run(Some(mirror), &["show", &spec]) {
        Ok(content) => Ok(Some(content)),
        // `sha` is known valid (checked above), so a failure here means the path is missing.
        Err(_) => Ok(None),
    }
}

/// Make `dest` (which must not exist) a standalone clone of `mirror` with `sha` checked out,
/// detached: `git clone --local --no-checkout` (hardlinks objects: no extra space), then
/// `git checkout --detach <sha>` with progress. The clone's `origin` stays the mirror. Fails if
/// `sha` is not in the mirror.
pub fn clone_at(
    mirror: &Path,
    sha: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<()> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mirror_str = mirror.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    run_with_progress(
        None,
        &[
            "clone",
            "--local",
            "--no-checkout",
            "--progress",
            &mirror_str,
            &dest_str,
        ],
        on_progress,
        on_line,
    )
    .with_context(|| format!("cannot clone {} into {}", mirror.display(), dest.display()))?;
    run_with_progress(
        Some(dest),
        &["checkout", "--progress", "--detach", sha],
        on_progress,
        on_line,
    )
    .with_context(|| format!("cannot check out {sha} in {}", dest.display()))?;
    Ok(())
}

/// `git rev-parse HEAD` and whether `git status --porcelain` is empty, in a checkout.
pub fn head_and_clean(checkout: &Path) -> Result<(String, bool)> {
    let head = run(Some(checkout), &["rev-parse", "HEAD"])
        .with_context(|| format!("cannot read HEAD of {}", checkout.display()))?
        .trim()
        .to_owned();
    let status = run(Some(checkout), &["status", "--porcelain"])
        .with_context(|| format!("cannot read the status of {}", checkout.display()))?;
    Ok((head, status.trim().is_empty()))
}

/// The `git` executable and its version line, or an error saying git is missing.
pub fn version() -> Result<String> {
    let mut cmd = base_command(&["--version"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().context("git is not installed")?;
    if !output.status.success() {
        bail!("git is not installed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Where a mirror's objects are, for disk-usage displays.
pub fn objects_dir(mirror: &Path) -> PathBuf {
    mirror.join("objects")
}

/// Delete `dir` recursively, making files writable first if a plain `remove_dir_all` is refused
/// (git objects are written read-only).
pub(crate) fn remove_dir_all_writable(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            make_writable_recursive(dir)?;
            std::fs::remove_dir_all(dir).with_context(|| format!("cannot remove {}", dir.display()))
        }
        Err(e) => Err(e).with_context(|| format!("cannot remove {}", dir.display())),
    }
}

#[cfg(unix)]
fn make_writable_recursive(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            make_writable_recursive(&path)?;
        } else if let Ok(meta) = std::fs::symlink_metadata(&path) {
            let mut perm = meta.permissions();
            perm.set_mode(perm.mode() | 0o200);
            let _ = std::fs::set_permissions(&path, perm);
        }
    }
    if let Ok(meta) = std::fs::metadata(dir) {
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o700);
        let _ = std::fs::set_permissions(dir, perm);
    }
    Ok(())
}

/// Clear the read-only bit git sets on objects. Reparse points (symlinks, junctions) are skipped
/// so a link cannot walk the deletion out of this tree.
#[cfg(windows)]
fn make_writable_recursive(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if is_reparse_point(&meta) {
            continue;
        }
        if meta.is_dir() {
            make_writable_recursive(&path)?;
        } else {
            let mut perm = meta.permissions();
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&path, perm);
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        let mut perm = meta.permissions();
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(dir, perm);
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}
