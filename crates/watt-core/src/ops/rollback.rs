//! The `Rollback` op: re-activate an earlier generation.

use anyhow::Context as _;
use pwc_instance::LockOptions;
use pwc_manifest::Lockfile;

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

    let current_lock = instance.lockfile().context("cannot read pwc.lock")?;
    let current_identity = current_lock
        .as_ref()
        .map(|l| common::generation_identity(&l.environment, &l.pwc.source, &meta.source));
    let target = match generation {
        Some(id) => meta
            .generations
            .iter()
            .find(|g| g.id == *id)
            .with_context(|| format!("`{name}` has no generation {id}"))?,
        None => meta
            .generations
            .iter()
            .find(|g| {
                Some(common::generation_identity(
                    &g.environment,
                    &g.pwc_source,
                    &g.source,
                )) != current_identity
            })
            .or_else(|| meta.generations.first())
            .with_context(|| format!("`{name}` has no generations to roll back to"))?,
    };
    let target_id = target.id;
    let target_source = target.source.clone();

    // Prepare and validate everything the rollback needs *before* committing to it: the binding
    // is recorded last (below), so a kill in the middle of this just leaves the old binding in
    // place, paired with whatever `pwc.lock`/`instance.toml` state got written — which the next
    // op's relock repairs regardless, since the binding (still the old one) is the truth.
    if let Binding::Managed { pin: Some(pin), .. } = &target_source {
        reporter.phase(Phase::Checkout, true);
        let game = crate::snapshots::game_snapshot(paths, &pin.game, &pin.engine);
        let modules_dir = crate::snapshots::modules_snapshot(paths, &pin.modules);
        if !crate::snapshots::is_complete(&game.dir) || !crate::snapshots::is_complete(&modules_dir)
        {
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
            crate::snapshots::ensure_modules(paths, &pin.modules, &mut on_progress, &mut on_line)
                .context("cannot restore the modules snapshot")?;
        }
    }

    reporter.phase(Phase::Lock, false);
    // `instance.toml` first, then the lock: killed in between, the lock is merely stale and the
    // next op relocks it from the restored wanted modules.
    let manifest_path = InstancePaths::new(&instance.dir).generation_manifest(target_id);
    let instance = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => {
            crate::paths::write_atomic(&instance.manifest_path(), text.as_bytes())?;
            common::open_instance(paths, name)?
        }
        // Generations recorded before manifests were kept: restore the lock only.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => instance,
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", manifest_path.display()));
        }
    };
    let generation_lock_path = InstancePaths::new(&instance.dir).generation_lock(target_id);
    let lock_text = std::fs::read_to_string(&generation_lock_path)
        .with_context(|| format!("cannot read {}", generation_lock_path.display()))?;
    crate::paths::write_atomic(&instance.lock_path(), lock_text.as_bytes())?;
    let mut lock = Lockfile::parse(&lock_text).context("cannot parse the restored pwc.lock")?;

    let resolved = crate::binding::resolve(paths, settings, &target_source)
        .context("cannot resolve the restored sources")?;

    let built = match crate::build::build(
        paths, settings, &instance, &resolved, &lock, None, false, reporter,
    ) {
        Ok(built) => built,
        // `pwc store gc` (ours or the CLI's) may have removed packages only this old generation
        // referenced: relock conservatively against the restored sources (which reinstalls
        // whatever is still resolvable) and try once more, rather than leaving the rollback
        // simply unable to build what `ARCHITECTURE.md` promises it can.
        Err(e) if is_missing_package(&e) => {
            reporter.warn(format!(
                "{e:#}; some packages were garbage-collected, resolving again"
            ));
            let report = crate::build::lock(
                paths,
                &instance,
                &resolved,
                &LockOptions::default(),
                reporter,
            )?;
            lock = report.lock;
            crate::build::build(
                paths, settings, &instance, &resolved, &lock, None, false, reporter,
            )?
        }
        Err(e) => return Err(e),
    };
    common::record_generation(settings, &instance.dir, &target_source, &built, reporter)?;

    // Commit the binding last: everything it points at (sources, `instance.toml`, `pwc.lock`,
    // the rebuilt generation) already exists and was validated above.
    crate::meta::update(&instance.dir, |m| {
        m.source = target_source.clone();
        Ok(())
    })?;

    Ok(format!("Rolled back `{name}` to generation {target_id}"))
}

/// Whether `e`'s chain names a locked package missing from the store.
fn is_missing_package(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<pwc_instance::InstanceError>(),
            Some(pwc_instance::InstanceError::NotInstalled { .. })
        )
    })
}
