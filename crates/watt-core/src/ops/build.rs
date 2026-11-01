//! The `Build` op: relock conservatively (repairing drift), build, record the generation.

use anyhow::{Result, bail};
use pwc_instance::LockOptions;
use pwc_manifest::BuildProfile;

use crate::binding::ResolvedSource;
use crate::events::{Phase, Reporter};
use crate::meta::{Binding, Meta};
use crate::ops::{OpRequest, OpResult};
use crate::paths::{Paths, Repo};
use crate::settings::Settings;

use super::common;

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Build {
        instance: name,
        force,
        profile,
    } = request
    else {
        unreachable!("ops::run dispatches Build requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    let meta = Meta::load(&instance.dir)?;
    let resolved = ensure_sources(paths, settings, &meta, reporter)?;

    reporter.phase(Phase::Lock, false);
    let lock_report = crate::build::lock(
        paths,
        &instance,
        &resolved,
        &LockOptions::default(),
        reporter,
    )?;

    let profile = match profile.as_deref() {
        Some("release") => Some(BuildProfile::Release),
        Some("dev") => Some(BuildProfile::Dev),
        Some(other) => bail!("`{other}` is not a build profile (\"release\" or \"dev\")"),
        None => None,
    };

    let built = crate::build::build(
        paths,
        settings,
        &instance,
        &resolved,
        &lock_report.lock,
        profile,
        *force,
        reporter,
    )?;
    let generation_id =
        common::record_generation(settings, &instance.dir, &meta.source, &built, reporter)?;

    Ok(format!(
        "Built `{name}` (generation {generation_id}){}",
        if built.cached { " \u{b7} cached" } else { "" }
    ))
}

/// Make sure a managed instance's pinned snapshots exist (fetching the mirrors only if a
/// pinned commit turns out to be missing from them), then resolve the binding.
pub(super) fn ensure_sources(
    paths: &Paths,
    settings: &Settings,
    meta: &Meta,
    reporter: &Reporter,
) -> Result<ResolvedSource> {
    if let Binding::Managed { pin: Some(pin), .. } = &meta.source {
        let game = crate::snapshots::game_snapshot(paths, &pin.game, &pin.engine);
        let modules_dir = crate::snapshots::modules_snapshot(paths, &pin.modules);
        if !crate::snapshots::is_complete(&game.dir) || !crate::snapshots::is_complete(&modules_dir)
        {
            reporter.phase(Phase::Checkout, true);
            let try_checkout = || -> Result<()> {
                let mut on_progress = |_repo: Repo, p: crate::git::GitProgress| {
                    reporter.progress(u64::from(p.percent), Some(100), p.stage);
                };
                let mut on_line = |line: String| reporter.tool_log(line);
                crate::snapshots::ensure_game(
                    paths,
                    &pin.game,
                    &pin.engine,
                    &mut on_progress,
                    &mut on_line,
                )?;
                crate::snapshots::ensure_modules(
                    paths,
                    &pin.modules,
                    &mut on_progress,
                    &mut on_line,
                )?;
                Ok(())
            };
            if let Err(e) = try_checkout() {
                reporter.warn(format!("{e:#}; fetching to recover the pinned commits"));
                reporter.phase(Phase::Fetch, true);
                {
                    let _git_lock = crate::locks::FileLock::acquire(&paths.git_lock())?;
                    for repo in Repo::ALL {
                        let mirror = paths.mirror(repo);
                        let remote = settings.remote(repo);
                        let mut on_progress = |p: crate::git::GitProgress| {
                            reporter.progress(u64::from(p.percent), Some(100), p.stage);
                        };
                        let mut on_line = |line: String| reporter.tool_log(line);
                        crate::git::ensure_mirror(
                            &mirror,
                            &remote,
                            &mut on_progress,
                            &mut on_line,
                        )?;
                        crate::git::fetch(&mirror, &mut on_progress, &mut on_line)?;
                    }
                }
                reporter.phase(Phase::Checkout, true);
                try_checkout()?;
            }
        }
    }
    crate::binding::resolve(paths, settings, &meta.source)
}
