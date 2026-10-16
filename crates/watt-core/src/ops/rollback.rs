//! The `Rollback` op: re-activate an earlier generation.

use anyhow::Context as _;

use crate::events::{Phase, Reporter};
use crate::meta::Binding;
use crate::ops::{OpRequest, OpResult};
use crate::paths::{InstancePaths, Paths, Repo};
use crate::settings::Settings;

use super::common;

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Rollback {
        instance: name,
        generation,
    } = request
    else {
        unreachable!("ops::run dispatches Rollback requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    let meta = crate::meta::Meta::load(&instance.dir)?;

    let current_environment = instance
        .lockfile()
        .context("cannot read pwc.lock")?
        .map(|l| l.environment);
    let target = match generation {
        Some(id) => meta
            .generations
            .iter()
            .find(|g| g.id == *id)
            .with_context(|| format!("`{name}` has no generation {id}"))?,
        None => meta
            .generations
            .iter()
            .find(|g| Some(g.environment.as_str()) != current_environment.as_deref())
            .or_else(|| meta.generations.first())
            .with_context(|| format!("`{name}` has no generations to roll back to"))?,
    };
    let target_id = target.id;
    let target_source = target.source.clone();

    crate::meta::update(&instance.dir, |m| {
        m.source = target_source.clone();
        Ok(())
    })?;

    if let Binding::Managed { pin: Some(pin), .. } = &target_source {
        reporter.phase(Phase::Checkout, true);
        let game = crate::snapshots::game_snapshot(paths, &pin.game, &pin.engine);
        let mods_dir = crate::snapshots::mods_snapshot(paths, &pin.mods);
        if !crate::snapshots::is_complete(&game.dir) || !crate::snapshots::is_complete(&mods_dir) {
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
            )
            .context("cannot restore the game snapshot")?;
            crate::snapshots::ensure_mods(paths, &pin.mods, &mut on_progress, &mut on_line)
                .context("cannot restore the mods snapshot")?;
        }
    }

    reporter.phase(Phase::Lock, false);
    let generation_lock_path = InstancePaths::new(&instance.dir).generation_lock(target_id);
    let lock_text = std::fs::read_to_string(&generation_lock_path)
        .with_context(|| format!("cannot read {}", generation_lock_path.display()))?;
    crate::paths::write_atomic(&instance.lock_path(), lock_text.as_bytes())?;

    let resolved = crate::binding::resolve(paths, settings, &target_source)
        .context("cannot resolve the restored sources")?;

    let built = crate::build::build(paths, settings, &instance, &resolved, None, false, reporter)?;
    common::record_generation(settings, &instance.dir, &target_source, &built, reporter)?;

    Ok(format!("Rolled back `{name}` to generation {target_id}"))
}
