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

use std::path::{Path, PathBuf};

use anyhow::Result;

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
    let _ = (mirror, remote, on_progress, on_line);
    todo!("worker A")
}

/// `git fetch --prune --progress origin` in the mirror. Returns whether any ref changed.
pub fn fetch(
    mirror: &Path,
    on_progress: &mut dyn FnMut(GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<bool> {
    let _ = (mirror, on_progress, on_line);
    todo!("worker A")
}

/// Resolve a branch, tag or (abbreviated) commit to a full commit hash; `None` if unknown.
/// Branches win over tags with the same name.
pub fn resolve(mirror: &Path, reference: &str) -> Result<Option<String>> {
    let _ = (mirror, reference);
    todo!("worker A")
}

/// The branch `HEAD` points at in the mirror ("main").
pub fn default_branch(mirror: &Path) -> Result<String> {
    let _ = mirror;
    todo!("worker A")
}

/// Every branch, newest tip first.
pub fn branches(mirror: &Path) -> Result<Vec<BranchInfo>> {
    let _ = mirror;
    todo!("worker A")
}

/// Every tag name, newest first (by creator date).
pub fn tags(mirror: &Path) -> Result<Vec<String>> {
    let _ = mirror;
    todo!("worker A")
}

/// Keep `sha` alive: `refs/pins/<sha>` → `sha`. Idempotent.
pub fn pin(mirror: &Path, sha: &str) -> Result<()> {
    let _ = (mirror, sha);
    todo!("worker A")
}

/// Delete every `refs/pins/*` whose commit is not in `keep`.
pub fn unpin_except(mirror: &Path, keep: &std::collections::BTreeSet<String>) -> Result<()> {
    let _ = (mirror, keep);
    todo!("worker A")
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
    let _ = (repo, mirror, old, new, max);
    todo!("worker A")
}

/// The contents of `path` at `sha` (`git show sha:path`), `None` if the file does not exist there.
pub fn file_at(mirror: &Path, sha: &str, path: &str) -> Result<Option<String>> {
    let _ = (mirror, sha, path);
    todo!("worker A")
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
    let _ = (mirror, sha, dest, on_progress, on_line);
    todo!("worker A")
}

/// `git rev-parse HEAD` and whether `git status --porcelain` is empty, in a checkout.
pub fn head_and_clean(checkout: &Path) -> Result<(String, bool)> {
    let _ = checkout;
    todo!("worker A")
}

/// The `git` executable and its version line, or an error saying git is missing.
pub fn version() -> Result<String> {
    todo!("worker A")
}

/// Where a mirror's objects are, for disk-usage displays.
pub fn objects_dir(mirror: &Path) -> PathBuf {
    mirror.join("objects")
}
