//! The `Maintenance` and `CheckUpdates` ops.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, bail};

use crate::events::{Change, Phase, Reporter};
use crate::locks::FileLock;
use crate::meta::{Binding, Meta};
use crate::ops::{OpRequest, OpResult};
use crate::paths::{InstancePaths, Paths, Repo};
use crate::settings::Settings;

pub(super) fn run(
    paths: &Paths,
    _settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Maintenance {
        clean_cache,
        gc_snapshots,
        gc_store,
        dry_run,
    } = request
    else {
        unreachable!("ops::run dispatches Maintenance requests only")
    };

    reporter.phase(Phase::Cleanup, false);
    let mut summary = Vec::new();

    if *gc_store {
        let removed = gc_store_impl(paths, *dry_run)?;
        summary.push(format!(
            "{removed} store {} removed",
            if removed == 1 { "package" } else { "packages" }
        ));
    }
    if *gc_snapshots {
        let removed = gc_snapshots_impl(paths, *dry_run)?;
        summary.push(format!(
            "{removed} snapshot{} removed",
            if removed == 1 { "" } else { "s" }
        ));
    }
    if *clean_cache {
        if any_instance_busy(paths) {
            bail!("another task is running on an instance; cache clean refused");
        }
        let target_lock = paths.pwc.target().join(crate::build::TARGET_LOCK);
        if FileLock::is_held(&target_lock) {
            bail!("a build is using the target directory; cache clean refused");
        }
        if !*dry_run {
            pwc_builder::clean(&paths.pwc.builds(), &paths.pwc.target())
                .context("cannot clean the build cache")?;
        }
        summary.push("build cache cleaned".to_owned());
    }
    if summary.is_empty() {
        summary.push("nothing to do".to_owned());
    }
    Ok(summary.join(", "))
}

fn any_instance_busy(paths: &Paths) -> bool {
    let root = paths.pwc.instances();
    let Ok(items) = std::fs::read_dir(&root) else {
        return false;
    };
    items
        .flatten()
        .any(|item| FileLock::is_held(&InstancePaths::new(item.path()).op_lock()))
}

fn gc_store_impl(paths: &Paths, dry_run: bool) -> Result<usize> {
    let mut keep = pwc_instance::referenced_hashes(&paths.pwc)?;
    let root = paths.pwc.instances();
    if let Ok(items) = std::fs::read_dir(&root) {
        for item in items.flatten() {
            let generations_dir = InstancePaths::new(item.path()).generations();
            let Ok(entries) = std::fs::read_dir(&generations_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("lock")
                    && let Ok(lock) = pwc_manifest::Lockfile::from_file(&entry.path())
                {
                    keep.extend(lock.packages.into_iter().map(|p| p.hash));
                }
            }
        }
    }
    let store = pwc_store::Store::open(paths.pwc.store())?;
    let removed = store.gc(&keep, dry_run)?;
    Ok(removed.len())
}

fn gc_snapshots_impl(paths: &Paths, dry_run: bool) -> Result<usize> {
    let mut keep_dirs: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut keep_game: BTreeSet<String> = BTreeSet::new();
    let mut keep_engine: BTreeSet<String> = BTreeSet::new();
    let mut keep_mods: BTreeSet<String> = BTreeSet::new();

    fn note_pin(
        paths: &Paths,
        pin: &crate::meta::Pin,
        keep_dirs: &mut BTreeSet<std::path::PathBuf>,
        keep_game: &mut BTreeSet<String>,
        keep_engine: &mut BTreeSet<String>,
        keep_mods: &mut BTreeSet<String>,
    ) {
        keep_dirs.insert(crate::snapshots::game_snapshot(paths, &pin.game, &pin.engine).dir);
        keep_dirs.insert(crate::snapshots::mods_snapshot(paths, &pin.mods));
        keep_game.insert(pin.game.clone());
        keep_engine.insert(pin.engine.clone());
        keep_mods.insert(pin.mods.clone());
    }

    let root = paths.pwc.instances();
    if let Ok(items) = std::fs::read_dir(&root) {
        for item in items.flatten() {
            let dir = item.path();
            if let Ok(meta) = Meta::load(&dir) {
                if let Binding::Managed { pin: Some(pin), .. } = &meta.source {
                    note_pin(
                        paths,
                        pin,
                        &mut keep_dirs,
                        &mut keep_game,
                        &mut keep_engine,
                        &mut keep_mods,
                    );
                }
                for generation in &meta.generations {
                    if let Binding::Managed { pin: Some(pin), .. } = &generation.source {
                        note_pin(
                            paths,
                            pin,
                            &mut keep_dirs,
                            &mut keep_game,
                            &mut keep_engine,
                            &mut keep_mods,
                        );
                    }
                }
            }
            if let Ok(lock) = pwc_manifest::Lockfile::from_file(&dir.join("pwc.lock"))
                && lock.pwc.source.starts_with(paths.sources())
                && lock.pwc.source.file_name() == Some(std::ffi::OsStr::new(Repo::Game.dir_name()))
                && let Some(snapshot_dir) = lock.pwc.source.parent()
            {
                keep_dirs.insert(snapshot_dir.to_path_buf());
            }
        }
    }

    let removed = crate::snapshots::gc(paths, &keep_dirs, dry_run)?;
    if !dry_run {
        crate::git::unpin_except(&paths.mirror(Repo::Game), &keep_game)?;
        crate::git::unpin_except(&paths.mirror(Repo::Engine), &keep_engine)?;
        crate::git::unpin_except(&paths.mirror(Repo::Mods), &keep_mods)?;
    }
    Ok(removed.len())
}

pub(super) fn check_updates(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::CheckUpdates { instance } = request else {
        unreachable!("ops::run dispatches CheckUpdates requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let targets: Vec<(String, Binding)> = match instance {
        Some(name) => {
            let meta = Meta::load(&paths.instance_dir(name))?;
            vec![(name.clone(), meta.source)]
        }
        None => {
            let mut targets = Vec::new();
            if let Ok(items) = std::fs::read_dir(paths.pwc.instances()) {
                for item in items.flatten() {
                    let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if let Ok(meta) = Meta::load(&item.path())
                        && matches!(meta.source, Binding::Managed { .. })
                    {
                        targets.push((name, meta.source));
                    }
                }
            }
            targets
        }
    };

    reporter.phase(Phase::Fetch, true);
    {
        let _git_lock = FileLock::acquire(&paths.git_lock())?;
        for repo in Repo::ALL {
            reporter.check_cancelled()?;
            let mirror = paths.mirror(repo);
            let remote = settings.remote(repo);
            let label = repo.label();
            let mut on_progress = |p: crate::git::GitProgress| {
                reporter.progress(
                    u64::from(p.percent),
                    Some(100),
                    format!("{label}: {}", p.stage),
                );
            };
            let mut on_line = |line: String| reporter.tool_log(line);
            crate::git::ensure_mirror(&mirror, &remote, &mut on_progress, &mut on_line)
                .with_context(|| format!("cannot set up the {label} mirror"))?;
            crate::git::fetch(&mirror, &mut on_progress, &mut on_line)
                .with_context(|| format!("cannot fetch the {label} mirror"))?;
        }
    }

    let mut total = 0usize;
    for (name, binding) in &targets {
        let Binding::Managed {
            channel,
            engine_ref,
            mods_ref,
            pin,
        } = binding
        else {
            continue;
        };
        let game_mirror = paths.mirror(Repo::Game);
        let engine_mirror = paths.mirror(Repo::Engine);
        let mods_mirror = paths.mirror(Repo::Mods);
        let Some(game_sha) = crate::git::resolve(&game_mirror, channel)? else {
            continue;
        };
        let (engine_sha, _) = super::update::resolve_ref(&engine_mirror, engine_ref, channel)?;
        let (mods_sha, _) = super::update::resolve_ref(&mods_mirror, mods_ref, channel)?;

        let mut whats_new = Vec::new();
        for (repo, mirror, old, new) in [
            (
                Repo::Game,
                &game_mirror,
                pin.as_ref().map(|p| p.game.as_str()),
                game_sha.as_str(),
            ),
            (
                Repo::Engine,
                &engine_mirror,
                pin.as_ref().map(|p| p.engine.as_str()),
                engine_sha.as_str(),
            ),
            (
                Repo::Mods,
                &mods_mirror,
                pin.as_ref().map(|p| p.mods.as_str()),
                mods_sha.as_str(),
            ),
        ] {
            if old == Some(new) {
                continue;
            }
            if let Ok(log) = crate::git::log_between(repo, mirror, old, new, 50) {
                whats_new.push(log);
            }
        }
        if !whats_new.is_empty() {
            total += whats_new.iter().map(|l| l.commits.len()).sum::<usize>();
            reporter.changed(Change::UpdatesAvailable {
                instance: name.clone(),
                whats_new,
            });
        }
    }

    Ok(if total == 0 {
        "Already up to date".to_owned()
    } else {
        format!(
            "{total} new commit{} available",
            if total == 1 { "" } else { "s" }
        )
    })
}
