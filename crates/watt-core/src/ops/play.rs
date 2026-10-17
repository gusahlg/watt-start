//! The `Play` op: build if needed (or reuse the last good build), then launch the game detached.

use anyhow::{Context as _, bail};
use pwc_builder::wrapped_command;
use serde::{Deserialize, Serialize};

use crate::events::{Change, Phase, Reporter};
use crate::locks::FileLock;
use crate::ops::{OpRequest, OpResult};
use crate::paths::{InstancePaths, Paths, now_secs};
use crate::settings::Settings;

use super::common;

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Play {
        instance: name,
        last_good,
        args,
    } = request
    else {
        unreachable!("ops::run dispatches Play requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    let instance_paths = InstancePaths::new(&instance.dir);

    // `game.lock` (not `game.toml`, which is just a display record) is the truth: held for
    // exactly as long as the game process lives, even across a launcher crash. A game started
    // through `pwc run` holds no such lock (see ARCHITECTURE.md); the launcher cannot see it.
    if FileLock::is_held(&instance_paths.game_lock()) {
        bail!("`{name}` is running; close the game first");
    }

    let (exe, wrapper, asset_dir, environment) = if *last_good {
        let meta = crate::meta::Meta::load(&instance.dir)?;
        let generation = meta
            .last_good()
            .with_context(|| format!("`{name}` has no previous successful build"))?;
        let build = generation
            .build
            .as_ref()
            .expect("last_good only returns generations with a build");
        reporter.phase(Phase::Launch, false);
        (
            build.game.clone(),
            build.wrapper.clone(),
            generation.pwc_source.join("assets"),
            generation.environment.clone(),
        )
    } else {
        let meta = crate::meta::Meta::load(&instance.dir)?;
        let resolved = crate::binding::resolve(paths, settings, &meta.source)
            .context("cannot resolve the instance's sources")?;
        reporter.phase(Phase::Lock, false);
        let lock_report = crate::build::lock(
            paths,
            &instance,
            &resolved,
            &pwc_instance::LockOptions::default(),
            reporter,
        )?;
        let built = crate::build::build(
            paths,
            settings,
            &instance,
            &resolved,
            &lock_report.lock,
            None,
            false,
            reporter,
        )?;
        common::record_generation(settings, &instance.dir, &meta.source, &built, reporter)?;
        reporter.phase(Phase::Launch, false);
        (
            built.built.game.clone(),
            built.built.wrapper.clone(),
            resolved.asset_dir(),
            built.lock.environment.clone(),
        )
    };

    let game_dir = instance.game_dir();
    std::fs::create_dir_all(&game_dir)
        .with_context(|| format!("cannot create {}", game_dir.display()))?;

    let mut command = wrapped_command(&wrapper, &exe);
    command
        .args(args)
        .current_dir(&game_dir)
        .env("WATT_DATA_DIR", &game_dir)
        .env("WATT_ASSET_DIR", &asset_dir)
        .env("PWC_INSTANCE", name)
        .env("PWC_ENVIRONMENT", &environment);

    let log = instance_paths
        .logs()
        .join(format!("game-{}.log", now_secs()));
    let pid = launch_game(&mut command, &log, &instance_paths.game_lock())?;

    let running = RunningGame {
        pid,
        started: now_secs(),
        log: log.clone(),
    };
    let text = toml::to_string_pretty(&running).context("cannot serialise the running game")?;
    crate::paths::write_atomic(&instance_paths.running(), text.as_bytes())?;

    crate::meta::update(&instance.dir, |m| {
        m.last_played = Some(now_secs());
        Ok(())
    })?;
    reporter.changed(Change::Launched { pid, log });

    Ok(format!("Started `{name}` (pid {pid})"))
}

/// Unix: the game inherits an exclusive flock (`FD_CLOEXEC` cleared) and keeps it until it exits.
/// Windows: `LockFileEx` does not travel with an inherited handle the way `flock` does, so a
/// breakaway guard (`watt-start op game-guard`) holds `lock` until the game process exits. The
/// game itself is also started with `CREATE_BREAKAWAY_FROM_JOB`, so closing the op's job does
/// not kill it. The game is launched before the guard locks; the instance's `op.lock`, held by
/// this op throughout, keeps a second Play out of that window.
fn launch_game(
    command: &mut std::process::Command,
    log: &std::path::Path,
    lock: &std::path::Path,
) -> anyhow::Result<u32> {
    #[cfg(unix)]
    {
        let game_lock =
            crate::process::acquire_inheritable_lock(lock).context("cannot take the game lock")?;
        let pid = crate::process::launch_detached(command, log).context("cannot start the game")?;
        drop(game_lock);
        Ok(pid)
    }
    #[cfg(windows)]
    {
        // `game` (our process handle) is held until the guard has answered, so the pid it
        // opens is still this game.
        let mut game = crate::process::spawn_detached(command, log)?;
        crate::process::await_game_guard(&mut game, lock)?;
        Ok(game.id())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, log, lock);
        bail!("playing is not implemented on this platform");
    }
}

/// `watt-start/game.toml`: display-only (pid, started, log); `game.lock` is the truth.
#[derive(Serialize, Deserialize)]
struct RunningGame {
    pid: u32,
    #[serde(default)]
    started: u64,
    #[serde(default)]
    log: std::path::PathBuf,
}
