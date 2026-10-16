//! Source snapshots: immutable, content-addressed checkouts that instances build from.
//!
//! A game snapshot is `sources/v1/pwc-<game12>-<engine12>/` holding `project_watt_cubed/` and
//! `voxel-engine/` side by side (the game's `Cargo.toml` uses `path = "../voxel-engine"`). A mods
//! snapshot is `sources/v1/mods-<mods12>/pwc-package-manager/`. Both are standalone local clones
//! of the mirrors (`git::clone_at`), detached at the pinned commit.
//!
//! The same revisions always give the same path, so instances pinned alike share one snapshot
//! and therefore one set of Cargo artifacts and Build IDs (pwc hashes the absolute path of the
//! engine into the Build ID, and Cargo keys path dependencies by path). Updating one instance
//! never touches another instance's snapshot.
//!
//! A snapshot directory counts only once it holds a `.complete` marker: it is created under a
//! temporary name, both checkouts are verified (HEAD = the pinned commit, clean), the marker is
//! written, and only then is it renamed into place. All of this happens under `Paths::git_lock`.
//! Snapshots are never edited after creation; nothing may build inside them with tools that
//! write tracked files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::git::GitProgress;
use crate::paths::{Paths, Repo};

/// The marker file of a finished snapshot.
pub const COMPLETE: &str = ".complete";

/// Paths of a game snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameSnapshot {
    /// `sources/v1/pwc-<g12>-<e12>`.
    pub dir: PathBuf,
    /// `<dir>/project_watt_cubed`: the pwc source.
    pub game: PathBuf,
    /// `<dir>/voxel-engine`.
    pub engine: PathBuf,
}

/// The game snapshot for these revisions (whether or not it exists).
pub fn game_snapshot(paths: &Paths, game_sha: &str, engine_sha: &str) -> GameSnapshot {
    let dir = paths
        .sources()
        .join(format!("pwc-{}-{}", key(game_sha), key(engine_sha)));
    GameSnapshot {
        game: dir.join(Repo::Game.dir_name()),
        engine: dir.join(Repo::Engine.dir_name()),
        dir,
    }
}

/// The mods snapshot directory for this revision (`sources/v1/mods-<m12>`; the checkout is its
/// `pwc-package-manager/` child).
pub fn mods_snapshot(paths: &Paths, mods_sha: &str) -> PathBuf {
    paths.sources().join(format!("mods-{}", key(mods_sha)))
}

/// The 12-character key of a commit hash.
pub fn key(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Whether a snapshot directory is finished.
pub fn is_complete(dir: &Path) -> bool {
    dir.join(COMPLETE).is_file()
}

/// Make sure the game snapshot for `(game_sha, engine_sha)` exists and is complete, creating it
/// from the mirrors if needed (both commits must be in the mirrors; pins them there). A leftover
/// incomplete directory is deleted and recreated. Takes `Paths::git_lock` itself.
pub fn ensure_game(
    paths: &Paths,
    game_sha: &str,
    engine_sha: &str,
    on_progress: &mut dyn FnMut(Repo, GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<GameSnapshot> {
    let _ = (paths, game_sha, engine_sha, on_progress, on_line);
    todo!("worker A")
}

/// [`ensure_game`] for the package manager snapshot; returns the snapshot directory.
pub fn ensure_mods(
    paths: &Paths,
    mods_sha: &str,
    on_progress: &mut dyn FnMut(Repo, GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<PathBuf> {
    let _ = (paths, mods_sha, on_progress, on_line);
    todo!("worker A")
}

/// One snapshot on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// Its directory.
    pub dir: PathBuf,
    /// Whether it is complete.
    pub complete: bool,
    /// Bytes on disk (excluding hardlinked git objects shared with the mirror is not required;
    /// an apparent size is fine).
    pub bytes: u64,
}

/// Every directory under `sources/v1`.
pub fn list(paths: &Paths) -> Result<Vec<SnapshotInfo>> {
    let _ = paths;
    todo!("worker A")
}

/// Delete every snapshot directory not in `keep` (incomplete ones included), under the git lock.
/// Returns what was (or with `dry_run`, would be) deleted. Directories are first renamed to a
/// hidden name so a half-deleted snapshot is never mistaken for a usable one.
pub fn gc(paths: &Paths, keep: &BTreeSet<PathBuf>, dry_run: bool) -> Result<Vec<PathBuf>> {
    let _ = (paths, keep, dry_run);
    todo!("worker A")
}
