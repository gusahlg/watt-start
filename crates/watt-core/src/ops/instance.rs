//! The `Duplicate`, `Remove` and `SetSource` ops.

use anyhow::{Context as _, bail};
use pwc_instance::LockOptions;

use crate::events::{Change, Phase, Reporter};
use crate::locks::FileLock;
use crate::meta::Binding;
use crate::ops::{OpRequest, OpResult, SourceSpec};
use crate::paths::{InstancePaths, Paths, now_secs};
use crate::settings::Settings;

use super::{common, update};

/// The prefix of a duplicate's staging directory under `instances/`, so a kill partway through
/// never publishes a half-copied instance (see [`duplicate`]).
const STAGING_PREFIX: &str = ".tmp-dup-";

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
    clean_duplicate_staging(paths);

    let source = common::open_instance(paths, name)?;
    if *with_worlds && FileLock::is_held(&InstancePaths::new(&source.dir).game_lock()) {
        bail!("`{name}` is running; close the game first");
    }

    let new_instance_name = pwc_manifest::InstanceName::parse(new_name)
        .with_context(|| format!("invalid instance name `{new_name}`"))?;
    let destination = paths.instance_dir(new_name);
    if destination.exists() {
        bail!("instance `{new_name}` already exists");
    }

    // Build the whole copy in a hidden staging directory and publish it with one atomic rename,
    // so another launcher (or process) can never observe, or overwrite the worlds of, a
    // half-copied instance; a kill partway through leaves only this staging directory, which the
    // next `Duplicate` (or a future maintenance pass) cleans up.
    let staging = paths
        .pwc
        .instances()
        .join(format!("{STAGING_PREFIX}{new_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(staging.join("game"))
        .with_context(|| format!("cannot create {}", staging.display()))?;

    let mut new_manifest = source.manifest.clone();
    new_manifest.name = new_instance_name;
    crate::paths::write_atomic(
        &staging.join("instance.toml"),
        new_manifest.to_toml_string().as_bytes(),
    )?;

    if let Ok(lock_text) = std::fs::read_to_string(source.lock_path()) {
        crate::paths::write_atomic(&staging.join("pwc.lock"), lock_text.as_bytes())?;
    }

    let source_meta = crate::meta::Meta::load(&source.dir)?;
    crate::meta::update(&staging, |m| {
        // Default to "<title> (copy)" so the two are told apart in the sidebar.
        m.display_name = display_name
            .clone()
            .or_else(|| Some(format!("{} (copy)", source_meta.title(name))));
        m.color = source_meta.color.clone();
        m.group = source_meta.group.clone();
        m.created = Some(now_secs());
        m.source = source_meta.source.clone();
        Ok(())
    })?;

    if *with_worlds {
        let from = source.game_dir();
        if from.is_dir() {
            common::copy_dir_recursive(&from, &staging.join("game"))
                .context("cannot copy the instance's worlds")?;
        }
    }

    if let Err(e) = std::fs::rename(&staging, &destination) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e).with_context(|| {
            format!("cannot create instance `{new_name}` (it may already exist)")
        });
    }

    reporter.changed(Change::Instance {
        name: new_name.clone(),
        exists: true,
    });

    Ok(format!("Duplicated `{name}` as `{new_name}`"))
}

/// Delete leftover staging directories of `Duplicate`s a previous run never finished (its own
/// process is long gone, so nothing could still be writing into them).
fn clean_duplicate_staging(paths: &Paths) {
    let Ok(items) = std::fs::read_dir(paths.pwc.instances()) else {
        return;
    };
    for item in items.flatten() {
        if item
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            let _ = std::fs::remove_dir_all(item.path());
        }
    }
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
    if FileLock::is_held(&InstancePaths::new(&instance.dir).game_lock()) {
        bail!("`{name}` is running; close the game first");
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

    // Resolve, download and lock *before* committing to the new binding: a nonexistent channel,
    // or a kill partway through, must leave the previous (still working) binding in place.
    let (resolved, lock_report, pin) = match &new_binding {
        Binding::Managed {
            channel,
            engine_ref,
            modules_ref,
            ..
        } => {
            let outcome = update::update_managed(
                paths,
                settings,
                &instance,
                channel,
                engine_ref,
                modules_ref,
                None,
                false,
                reporter,
            )?;
            let pin = (outcome.new_pin, outcome.whats_new);
            (outcome.resolved, outcome.lock_report, Some(pin))
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
            (resolved, report, None)
        }
    };

    // Everything the new binding points at now exists and `pwc.lock` already reflects it:
    // commit the binding (and, for managed sources, its pin) last.
    crate::meta::update(&instance.dir, |m| {
        m.source = new_binding.clone();
        if let (Binding::Managed { pin: slot, .. }, Some((new_pin, _))) = (&mut m.source, &pin) {
            *slot = Some(new_pin.clone());
        }
        Ok(())
    })?;
    if let Some((new_pin, whats_new)) = pin {
        reporter.changed(Change::Pin {
            old: None,
            new: new_pin,
            whats_new,
        });
    }

    let mut summary = format!("`{name}` now builds from {}", source_label(source));
    if build.unwrap_or(settings.rebuild_after_update) {
        reporter.check_cancelled()?;
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
        common::record_generation(settings, &instance.dir, &new_binding, &built, reporter)?;
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
