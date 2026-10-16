//! The `Duplicate`, `Remove` and `SetSource` ops.

use anyhow::{Context as _, bail};
use pwc_instance::{Instance, LockOptions};
use serde::Deserialize;

use crate::events::{Change, Phase, Reporter};
use crate::meta::Binding;
use crate::ops::{OpRequest, OpResult, SourceSpec};
use crate::paths::{InstancePaths, Paths, now_secs};
use crate::settings::Settings;

use super::{common, update};

pub(super) fn duplicate(
    paths: &Paths,
    _settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Duplicate {
        instance: name,
        new_name,
        display_name,
        with_worlds,
    } = request
    else {
        unreachable!("ops::run dispatches Duplicate requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let source = common::open_instance(paths, name)?;

    let new_instance_name = pwc_manifest::InstanceName::parse(new_name)
        .with_context(|| format!("invalid instance name `{new_name}`"))?;
    let mut new_manifest = source.manifest.clone();
    new_manifest.name = new_instance_name;
    let new_instance = Instance::create(&paths.pwc, new_manifest)
        .with_context(|| format!("cannot create instance `{new_name}`"))?;

    if let Ok(lock_text) = std::fs::read_to_string(source.lock_path()) {
        crate::paths::write_atomic(&new_instance.lock_path(), lock_text.as_bytes())?;
    }

    let source_meta = crate::meta::Meta::load(&source.dir)?;
    crate::meta::update(&new_instance.dir, |m| {
        m.display_name = display_name
            .clone()
            .or_else(|| source_meta.display_name.clone());
        m.color = source_meta.color.clone();
        m.group = source_meta.group.clone();
        m.created = Some(now_secs());
        m.source = source_meta.source.clone();
        Ok(())
    })?;

    if *with_worlds {
        let from = source.game_dir();
        if from.is_dir() {
            common::copy_dir_recursive(&from, &new_instance.game_dir())
                .context("cannot copy the instance's worlds")?;
        }
    }
    reporter.changed(Change::Instance {
        name: new_name.clone(),
        exists: true,
    });

    Ok(format!("Duplicated `{name}` as `{new_name}`"))
}

pub(super) fn remove(
    paths: &Paths,
    _settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Remove { instance: name } = request else {
        unreachable!("ops::run dispatches Remove requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    if let Some(pid) = running_pid(&instance.dir) {
        bail!("`{name}` is running (pid {pid}); stop it first");
    }
    instance
        .remove()
        .with_context(|| format!("cannot remove instance `{name}`"))?;
    reporter.changed(Change::Instance {
        name: name.clone(),
        exists: false,
    });
    Ok(format!("Removed `{name}`"))
}

pub(super) fn set_source(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::SetSource {
        instance: name,
        source,
        build,
    } = request
    else {
        unreachable!("ops::run dispatches SetSource requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance = common::open_instance(paths, name)?;
    let new_binding = common::binding_from_spec(source);

    crate::meta::update(&instance.dir, |m| {
        m.source = new_binding.clone();
        Ok(())
    })?;

    let (resolved, lock_report) = match &new_binding {
        Binding::Managed {
            channel,
            engine_ref,
            mods_ref,
            ..
        } => {
            let outcome = update::update_managed(
                paths, settings, &instance, channel, engine_ref, mods_ref, None, false, reporter,
            )?;
            crate::meta::update(&instance.dir, |m| {
                if let Binding::Managed { pin, .. } = &mut m.source {
                    *pin = Some(outcome.new_pin.clone());
                }
                Ok(())
            })?;
            reporter.changed(Change::Pin {
                old: None,
                new: outcome.new_pin.clone(),
                whats_new: outcome.whats_new.clone(),
            });
            (outcome.resolved, outcome.lock_report)
        }
        other => {
            reporter.phase(Phase::Lock, false);
            let resolved = crate::binding::resolve(paths, settings, other)
                .context("cannot resolve the new sources")?;
            let options = LockOptions {
                update_all: true,
                ..Default::default()
            };
            let report = crate::build::lock(paths, &instance, &resolved, &options, reporter)?;
            (resolved, report)
        }
    };
    let _ = lock_report.lock.packages.len();

    let mut summary = format!("`{name}` now builds from {}", source_label(source));
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

fn source_label(spec: &SourceSpec) -> String {
    match spec {
        SourceSpec::Managed { channel, .. } => format!("the managed `{channel}` channel"),
        SourceSpec::Linked { pwc_source, .. } => pwc_source.display().to_string(),
        SourceSpec::PwcDefault => "pwc's configured source".to_owned(),
    }
}

#[derive(Deserialize)]
struct RunningGame {
    pid: u32,
}

fn running_pid(instance_dir: &std::path::Path) -> Option<u32> {
    let path = InstancePaths::new(instance_dir).running();
    let text = std::fs::read_to_string(path).ok()?;
    let running: RunningGame = toml::from_str(&text).ok()?;
    crate::process::is_alive(running.pid).then_some(running.pid)
}
