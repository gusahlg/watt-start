//! The `Update` op, and the shared managed-sources flow `create` and `set-source` reuse.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use pwc_instance::{Instance, LockOptions};
use pwc_manifest::{Version, VersionReq};

use crate::binding::{self, ResolvedSource};
use crate::events::{Change, Phase, RepoLog, Reporter};
use crate::git;
use crate::locks::FileLock;
use crate::meta::{AUTO, Binding, Pin};
use crate::ops::{OpRequest, OpResult};
use crate::paths::{InstancePaths, Paths, Repo, now_secs};
use crate::settings::Settings;
use crate::snapshots;

use super::common;

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Update {
        instance: name,
        build,
        reset_lock,
    } = request
    else {
        unreachable!("ops::run dispatches Update requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    let meta = crate::meta::Meta::load(&instance.dir)?;

    let (resolved, lock_report, summary_prefix) = match meta.source.clone() {
        Binding::Managed {
            channel,
            engine_ref,
            mods_ref,
            pin: old_pin,
        } => {
            let outcome = update_managed(
                paths,
                settings,
                &instance,
                &channel,
                &engine_ref,
                &mods_ref,
                old_pin.as_ref(),
                true,
                reporter,
            )?;
            crate::meta::update(&instance.dir, |m| {
                if let Binding::Managed { pin, .. } = &mut m.source {
                    *pin = Some(outcome.new_pin.clone());
                }
                Ok(())
            })?;
            reporter.changed(Change::Pin {
                old: old_pin,
                new: outcome.new_pin.clone(),
                whats_new: outcome.whats_new.clone(),
            });
            let summary_prefix = if outcome.pin_changed {
                format!("Updated to {}", outcome.new_pin.label())
            } else {
                "Already up to date".to_owned()
            };
            (outcome.resolved, outcome.lock_report, summary_prefix)
        }
        other => {
            let (report, resolved) =
                lock_only(paths, settings, &instance, &other, *reset_lock, reporter)?;
            let summary_prefix = if report.changes.is_empty() {
                "Already up to date".to_owned()
            } else {
                "Updated mods".to_owned()
            };
            (resolved, report, summary_prefix)
        }
    };

    let mod_changes = lock_report.changes.len();
    let mut summary = summary_prefix;
    if mod_changes > 0 {
        summary = format!(
            "{summary} \u{b7} {mod_changes} mod{} changed",
            if mod_changes == 1 { "" } else { "s" }
        );
    }

    if build.unwrap_or(settings.rebuild_after_update) {
        reporter.check_cancelled()?;
        let meta = crate::meta::Meta::load(&instance.dir)?;
        let built =
            crate::build::build(paths, settings, &instance, &resolved, None, false, reporter)?;
        common::record_generation(settings, &instance.dir, &meta.source, &built, reporter)?;
        summary.push_str(if built.cached {
            " \u{b7} built (cached)"
        } else {
            " \u{b7} built"
        });
    }

    Ok(summary)
}

/// Lock an instance whose binding is not managed (`Linked`/`PwcDefault`), auto-resolving a
/// content-changed error only when `reset_lock` was asked for.
pub(super) fn lock_only(
    paths: &Paths,
    settings: &Settings,
    instance: &Instance,
    binding: &Binding,
    reset_lock: bool,
    reporter: &Reporter,
) -> Result<(pwc_instance::LockReport, ResolvedSource)> {
    reporter.phase(Phase::Lock, false);
    let resolved = binding::resolve(paths, settings, binding)
        .context("cannot resolve the instance's sources")?;
    let options = LockOptions {
        update_all: true,
        ..Default::default()
    };
    match crate::build::lock(paths, instance, &resolved, &options, reporter) {
        Ok(report) => Ok((report, resolved)),
        Err(e) if reset_lock && is_contents_changed(&e) => {
            reporter.warn(format!("{e:#}"));
            let _ = std::fs::rename(instance.lock_path(), instance.dir.join("pwc.lock.prev"));
            let report = crate::build::lock(paths, instance, &resolved, &options, reporter)?;
            Ok((report, resolved))
        }
        Err(e) => Err(e),
    }
}

/// What a managed update produced.
pub(super) struct ManagedUpdate {
    pub lock_report: pwc_instance::LockReport,
    pub resolved: ResolvedSource,
    pub new_pin: Pin,
    pub pin_changed: bool,
    pub whats_new: Vec<RepoLog>,
}

/// Fetch, resolve the channel/refs, make sure the snapshots exist, optionally back up worlds,
/// then lock against the new pin. Shared by `update`, `create` (managed instances) and
/// `set-source`.
#[allow(clippy::too_many_arguments)]
pub(super) fn update_managed(
    paths: &Paths,
    settings: &Settings,
    instance: &Instance,
    channel: &str,
    engine_ref: &str,
    mods_ref: &str,
    old_pin: Option<&Pin>,
    do_backup: bool,
    reporter: &Reporter,
) -> Result<ManagedUpdate> {
    reporter.phase(Phase::Fetch, true);
    {
        let _git_lock = FileLock::acquire(&paths.git_lock())?;
        for repo in Repo::ALL {
            reporter.check_cancelled()?;
            let mirror = paths.mirror(repo);
            let remote = settings.remote(repo);
            let label = repo.label();
            let mut on_progress = |p: git::GitProgress| {
                reporter.progress(
                    u64::from(p.percent),
                    Some(100),
                    format!("{label}: {}", p.stage),
                );
            };
            let mut on_line = |line: String| reporter.tool_log(line);
            git::ensure_mirror(&mirror, &remote, &mut on_progress, &mut on_line)
                .with_context(|| format!("cannot set up the {label} mirror"))?;
            git::fetch(&mirror, &mut on_progress, &mut on_line)
                .with_context(|| format!("cannot fetch the {label} mirror"))?;
        }
    }
    reporter.check_cancelled()?;

    let game_mirror = paths.mirror(Repo::Game);
    let engine_mirror = paths.mirror(Repo::Engine);
    let mods_mirror = paths.mirror(Repo::Mods);

    let game_sha = git::resolve(&game_mirror, channel)?
        .ok_or_else(|| anyhow!("the game has no branch or tag `{channel}`"))?;
    let (engine_sha, engine_ref_used) = resolve_ref(&engine_mirror, engine_ref, channel)?;
    let (mods_sha, mods_ref_used) = resolve_ref(&mods_mirror, mods_ref, channel)?;

    check_engine_compatibility(
        &game_mirror,
        &game_sha,
        &engine_mirror,
        &engine_sha,
        reporter,
    )?;

    reporter.phase(Phase::Checkout, true);
    {
        let mut on_progress = |_repo: Repo, p: git::GitProgress| {
            reporter.progress(u64::from(p.percent), Some(100), p.stage);
        };
        let mut on_line = |line: String| reporter.tool_log(line);
        snapshots::ensure_game(
            paths,
            &game_sha,
            &engine_sha,
            &mut on_progress,
            &mut on_line,
        )
        .context("cannot prepare the game snapshot")?;
        snapshots::ensure_mods(paths, &mods_sha, &mut on_progress, &mut on_line)
            .context("cannot prepare the mods snapshot")?;
    }
    reporter.check_cancelled()?;

    let game_sha_changed = old_pin.map(|p| p.game != game_sha).unwrap_or(true);
    if do_backup && game_sha_changed && settings.backup_before_update {
        let game_dir = instance.game_dir();
        let non_empty = std::fs::read_dir(&game_dir)
            .map(|mut items| items.next().is_some())
            .unwrap_or(false);
        if non_empty {
            reporter.phase(Phase::Backup, false);
            let backups = InstancePaths::new(&instance.dir).backups();
            let dest = backups.join(now_secs().to_string());
            common::copy_dir_recursive(&game_dir, &dest)
                .context("cannot back up the instance's worlds")?;
            common::prune_dir_entries(&backups, settings.backups_to_keep)?;
        }
    }

    reporter.phase(Phase::Lock, false);
    let new_pin = Pin {
        game: game_sha.clone(),
        engine: engine_sha.clone(),
        mods: mods_sha.clone(),
        game_ref: channel.to_owned(),
        engine_ref: engine_ref_used,
        mods_ref: mods_ref_used,
    };
    let new_binding = Binding::Managed {
        channel: channel.to_owned(),
        engine_ref: engine_ref.to_owned(),
        mods_ref: mods_ref.to_owned(),
        pin: Some(new_pin.clone()),
    };
    let resolved = binding::resolve(paths, settings, &new_binding)
        .context("cannot resolve the newly downloaded sources")?;
    let options = LockOptions {
        update_all: true,
        ..Default::default()
    };
    let lock_report = match crate::build::lock(paths, instance, &resolved, &options, reporter) {
        Ok(report) => report,
        Err(e) if is_contents_changed(&e) => {
            reporter.warn(format!("{e:#}"));
            let _ = std::fs::rename(instance.lock_path(), instance.dir.join("pwc.lock.prev"));
            crate::build::lock(paths, instance, &resolved, &options, reporter)?
        }
        Err(e) => return Err(e),
    };

    let pin_changed = old_pin != Some(&new_pin);
    let mut whats_new = Vec::new();
    if pin_changed {
        for (repo, mirror, old_sha, new_sha) in [
            (
                Repo::Game,
                &game_mirror,
                old_pin.map(|p| p.game.as_str()),
                game_sha.as_str(),
            ),
            (
                Repo::Engine,
                &engine_mirror,
                old_pin.map(|p| p.engine.as_str()),
                engine_sha.as_str(),
            ),
            (
                Repo::Mods,
                &mods_mirror,
                old_pin.map(|p| p.mods.as_str()),
                mods_sha.as_str(),
            ),
        ] {
            if old_sha == Some(new_sha) {
                continue;
            }
            if let Ok(log) = git::log_between(repo, mirror, old_sha, new_sha, 50) {
                whats_new.push(log);
            }
        }
    }

    Ok(ManagedUpdate {
        lock_report,
        resolved,
        new_pin,
        pin_changed,
        whats_new,
    })
}

/// Resolve an engine/mods ref: `auto` tries the channel name first, then the mirror's default
/// branch; anything else is resolved directly.
pub(super) fn resolve_ref(mirror: &Path, want: &str, channel: &str) -> Result<(String, String)> {
    if want == AUTO {
        if let Some(sha) = git::resolve(mirror, channel)? {
            return Ok((sha, channel.to_owned()));
        }
        let default = git::default_branch(mirror)?;
        let sha = git::resolve(mirror, &default)?
            .ok_or_else(|| anyhow!("{} has no default branch `{default}`", mirror.display()))?;
        Ok((sha, default))
    } else {
        let sha = git::resolve(mirror, want)?
            .ok_or_else(|| anyhow!("no branch or tag `{want}` in {}", mirror.display()))?;
        Ok((sha, want.to_owned()))
    }
}

fn check_engine_compatibility(
    game_mirror: &Path,
    game_sha: &str,
    engine_mirror: &Path,
    engine_sha: &str,
    reporter: &Reporter,
) -> Result<()> {
    let (Some(game_cargo), Some(engine_cargo)) = (
        git::file_at(game_mirror, game_sha, "Cargo.toml")?,
        git::file_at(engine_mirror, engine_sha, "Cargo.toml")?,
    ) else {
        return Ok(());
    };
    let Some(requirement) = engine_requirement(&game_cargo) else {
        return Ok(());
    };
    let Some(version) = package_version(&engine_cargo) else {
        return Ok(());
    };
    if let (Ok(req), Ok(version)) = (VersionReq::parse(&requirement), Version::parse(&version))
        && !req.matches(&version)
    {
        reporter.warn(format!(
            "the game wants voxel_engine {requirement}, but the engine at {} is {version}",
            crate::meta::short(engine_sha)
        ));
    }
    Ok(())
}

fn engine_requirement(cargo_toml: &str) -> Option<String> {
    let table: toml::Table = cargo_toml.parse().ok()?;
    match table.get("dependencies")?.get("voxel_engine")? {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t.get("version")?.as_str().map(str::to_owned),
        _ => None,
    }
}

fn package_version(cargo_toml: &str) -> Option<String> {
    let table: toml::Table = cargo_toml.parse().ok()?;
    table
        .get("package")?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

pub(super) fn is_contents_changed(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<pwc_instance::InstanceError>(),
            Some(pwc_instance::InstanceError::ContentsChanged { .. })
        )
    })
}
