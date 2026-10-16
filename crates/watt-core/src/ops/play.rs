//! The `Play` op: build if needed (or reuse the last good build), then launch the game detached.

use anyhow::{Context as _, bail};
use pwc_builder::wrapped_command;
use serde::{Deserialize, Serialize};

use crate::events::{Change, Phase, Reporter};
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

    if let Some(pid) = running_pid(&instance.dir) {
        bail!("`{name}` is already running (pid {pid})");
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
        crate::build::lock(
            paths,
            &instance,
            &resolved,
            &pwc_instance::LockOptions::default(),
            reporter,
        )?;
        let built =
            crate::build::build(paths, settings, &instance, &resolved, None, false, reporter)?;
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
    let pid =
        crate::process::launch_detached(&mut command, &log).context("cannot start the game")?;

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

#[derive(Serialize, Deserialize)]
struct RunningGame {
    pid: u32,
    #[serde(default)]
    started: u64,
    #[serde(default)]
    log: std::path::PathBuf,
}

fn running_pid(instance_dir: &std::path::Path) -> Option<u32> {
    let path = InstancePaths::new(instance_dir).running();
    let text = std::fs::read_to_string(path).ok()?;
    let running: RunningGame = toml::from_str(&text).ok()?;
    crate::process::is_alive(running.pid).then_some(running.pid)
}
