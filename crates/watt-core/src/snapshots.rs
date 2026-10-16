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

use anyhow::{Context as _, Result, bail};

use crate::git::GitProgress;
use crate::locks::FileLock;
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

/// Whether `checkout`'s `HEAD` is `sha` and it has no local modifications. Any error (missing
/// directory, not a git repository) counts as "no", since that means corrupt, not matching.
fn checkout_matches(checkout: &Path, sha: &str) -> bool {
    crate::git::head_and_clean(checkout)
        .map(|(head, clean)| clean && head == sha)
        .unwrap_or(false)
}

/// Remove `dir` (if it exists) and every `.tmp-<key>-*` sibling of it under `sources` — leftovers
/// from a crashed or killed op.
fn clear_leftovers(sources: &Path, dir: &Path, key: &str) -> Result<()> {
    if dir.exists() {
        crate::git::remove_dir_all_writable(dir)?;
    }
    if !sources.is_dir() {
        return Ok(());
    }
    let prefix = format!(".tmp-{key}-");
    for entry in
        std::fs::read_dir(sources).with_context(|| format!("cannot read {}", sources.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read {}", sources.display()))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            crate::git::remove_dir_all_writable(&entry.path())?;
        }
    }
    Ok(())
}

/// A temporary sibling directory under `sources` for building a snapshot named `key`, cleared of
/// any previous leftover with the same name.
fn fresh_tmp_dir(sources: &Path, key: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(sources)
        .with_context(|| format!("cannot create {}", sources.display()))?;
    let tmp = sources.join(format!(".tmp-{key}-{}", std::process::id()));
    if tmp.exists() {
        crate::git::remove_dir_all_writable(&tmp)?;
    }
    Ok(tmp)
}

/// The directory's file name, for building `.tmp-<key>-*` sibling names.
fn dir_key(dir: &Path) -> Result<String> {
    Ok(dir
        .file_name()
        .with_context(|| format!("{} has no file name", dir.display()))?
        .to_string_lossy()
        .into_owned())
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
    let _lock = FileLock::acquire(&paths.git_lock())?;
    let snapshot = game_snapshot(paths, game_sha, engine_sha);

    if is_complete(&snapshot.dir)
        && checkout_matches(&snapshot.game, game_sha)
        && checkout_matches(&snapshot.engine, engine_sha)
    {
        return Ok(snapshot);
    }

    let sources = paths.sources();
    let key = dir_key(&snapshot.dir)?;
    clear_leftovers(&sources, &snapshot.dir, &key)?;
    let tmp = fresh_tmp_dir(&sources, &key)?;

    crate::git::pin(&paths.mirror(Repo::Game), game_sha)
        .with_context(|| format!("cannot pin the game at {game_sha}"))?;
    crate::git::pin(&paths.mirror(Repo::Engine), engine_sha)
        .with_context(|| format!("cannot pin the engine at {engine_sha}"))?;

    let game_dest = tmp.join(Repo::Game.dir_name());
    let engine_dest = tmp.join(Repo::Engine.dir_name());

    crate::git::clone_at(
        &paths.mirror(Repo::Game),
        game_sha,
        &game_dest,
        &mut |p| on_progress(Repo::Game, p),
        on_line,
    )
    .with_context(|| format!("cannot check out the game at {game_sha}"))?;
    crate::git::clone_at(
        &paths.mirror(Repo::Engine),
        engine_sha,
        &engine_dest,
        &mut |p| on_progress(Repo::Engine, p),
        on_line,
    )
    .with_context(|| format!("cannot check out the engine at {engine_sha}"))?;

    if !checkout_matches(&game_dest, game_sha) {
        bail!(
            "the game checkout at {} is not clean at {game_sha}",
            game_dest.display()
        );
    }
    if !checkout_matches(&engine_dest, engine_sha) {
        bail!(
            "the engine checkout at {} is not clean at {engine_sha}",
            engine_dest.display()
        );
    }

    crate::paths::write_atomic(
        &tmp.join(COMPLETE),
        format!("{game_sha}\n{engine_sha}\n").as_bytes(),
    )?;

    std::fs::rename(&tmp, &snapshot.dir).with_context(|| {
        format!(
            "cannot rename {} to {}",
            tmp.display(),
            snapshot.dir.display()
        )
    })?;

    Ok(snapshot)
}

/// [`ensure_game`] for the package manager snapshot; returns the snapshot directory.
pub fn ensure_mods(
    paths: &Paths,
    mods_sha: &str,
    on_progress: &mut dyn FnMut(Repo, GitProgress),
    on_line: &mut dyn FnMut(String),
) -> Result<PathBuf> {
    let _lock = FileLock::acquire(&paths.git_lock())?;
    let dir = mods_snapshot(paths, mods_sha);
    let checkout = dir.join(Repo::Mods.dir_name());

    if is_complete(&dir) && checkout_matches(&checkout, mods_sha) {
        return Ok(dir);
    }

    let sources = paths.sources();
    let key = dir_key(&dir)?;
    clear_leftovers(&sources, &dir, &key)?;
    let tmp = fresh_tmp_dir(&sources, &key)?;

    crate::git::pin(&paths.mirror(Repo::Mods), mods_sha)
        .with_context(|| format!("cannot pin the mods at {mods_sha}"))?;

    let tmp_checkout = tmp.join(Repo::Mods.dir_name());
    crate::git::clone_at(
        &paths.mirror(Repo::Mods),
        mods_sha,
        &tmp_checkout,
        &mut |p| on_progress(Repo::Mods, p),
        on_line,
    )
    .with_context(|| format!("cannot check out the mods at {mods_sha}"))?;

    if !checkout_matches(&tmp_checkout, mods_sha) {
        bail!(
            "the mods checkout at {} is not clean at {mods_sha}",
            tmp_checkout.display()
        );
    }

    crate::paths::write_atomic(&tmp.join(COMPLETE), format!("{mods_sha}\n").as_bytes())?;

    std::fs::rename(&tmp, &dir)
        .with_context(|| format!("cannot rename {} to {}", tmp.display(), dir.display()))?;

    Ok(dir)
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
    let sources = paths.sources();
    let mut result = Vec::new();
    if !sources.is_dir() {
        return Ok(result);
    }
    for entry in
        std::fs::read_dir(&sources).with_context(|| format!("cannot read {}", sources.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read {}", sources.display()))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let bytes = dir_size(&path)?;
        result.push(SnapshotInfo {
            complete: is_complete(&path),
            bytes,
            dir: path,
        });
    }
    Ok(result)
}

/// The apparent size, in bytes, of every regular file under `dir` (recursively, not following
/// symlinks).
fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)
            .with_context(|| format!("cannot read {}", current.display()))?
        {
            let entry = entry.with_context(|| format!("cannot read {}", current.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("cannot stat {}", entry.path().display()))?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total += entry
                    .metadata()
                    .with_context(|| format!("cannot stat {}", entry.path().display()))?
                    .len();
            }
            // Symlinks are neither a directory nor a (followed) file here, so they are skipped.
        }
    }
    Ok(total)
}

/// Delete every snapshot directory not in `keep` (incomplete ones included), under the git lock.
/// Returns what was (or with `dry_run`, would be) deleted. Directories are first renamed to a
/// hidden name so a half-deleted snapshot is never mistaken for a usable one.
pub fn gc(paths: &Paths, keep: &BTreeSet<PathBuf>, dry_run: bool) -> Result<Vec<PathBuf>> {
    let _lock = FileLock::acquire(&paths.git_lock())?;
    gc_locked(paths, keep, dry_run)
}

/// [`gc`] for a caller that already holds `paths.git_lock()` (so it can run GC and, say,
/// `git::unpin_except` under one acquisition instead of two, with no window between them where
/// a concurrent update could publish a reference this pass would otherwise delete).
pub fn gc_locked(paths: &Paths, keep: &BTreeSet<PathBuf>, dry_run: bool) -> Result<Vec<PathBuf>> {
    let sources = paths.sources();
    let mut deleted = Vec::new();
    if !sources.is_dir() {
        return Ok(deleted);
    }

    let keep_canon: BTreeSet<PathBuf> = keep
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect();

    for entry in
        std::fs::read_dir(&sources).with_context(|| format!("cannot read {}", sources.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read {}", sources.display()))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            // Not a snapshot: a leftover `.tmp-*`/`.trash-*` from an earlier run.
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if keep_canon.contains(&canon) {
            continue;
        }
        deleted.push(path.clone());
        if !dry_run {
            let trash = sources.join(format!(
                ".trash-{}-{}",
                name.to_string_lossy(),
                std::process::id()
            ));
            std::fs::rename(&path, &trash).with_context(|| {
                format!("cannot rename {} to {}", path.display(), trash.display())
            })?;
            crate::git::remove_dir_all_writable(&trash)?;
        }
    }

    if !dry_run {
        for entry in std::fs::read_dir(&sources)
            .with_context(|| format!("cannot read {}", sources.display()))?
        {
            let entry = entry.with_context(|| format!("cannot read {}", sources.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".tmp-") || name.starts_with(".trash-") {
                crate::git::remove_dir_all_writable(&entry.path())?;
            }
        }
    }

    Ok(deleted)
}
