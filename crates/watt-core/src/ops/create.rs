//! The `Create` op.

use anyhow::Context as _;
use pwc_instance::{Instance, LockOptions};
use pwc_manifest::{InstanceManifest, VersionReq};

use crate::events::{Change, Phase, Reporter};
use crate::locks::FileLock;
use crate::meta::Binding;
use crate::ops::{OpRequest, OpResult};
use crate::paths::{InstancePaths, Paths, now_secs};
use crate::settings::Settings;

use super::{common, update};

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Create {
        name,
        display_name,
        color,
        group,
        source,
        mods,
        build,
    } = request
    else {
        unreachable!("ops::run dispatches Create requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let instance_name = pwc_manifest::InstanceName::parse(name)
        .with_context(|| format!("invalid instance name `{name}`"))?;
    let mut instance = Instance::create(
        &paths.pwc,
        InstanceManifest::new(instance_name, VersionReq::STAR),
    )
    .with_context(|| format!("cannot create instance `{name}`"))?;

    // The instance directory (and so its `watt-start/` subdirectory) now exists: take the lock.
    let _op_lock = FileLock::acquire(&InstancePaths::new(&instance.dir).op_lock())?;

    let binding = common::binding_from_spec(source);
    crate::meta::update(&instance.dir, |m| {
        m.display_name = display_name.clone();
        m.color = color.clone();
        m.group = group.clone();
        m.created = Some(now_secs());
        m.source = binding.clone();
        Ok(())
    })?;

    for spec in mods {
        let parsed = common::parse_mod_spec(&instance.dir, spec)?;
        instance.manifest.mods.insert(parsed.id, parsed.requirement);
    }
    instance.save().context("cannot write instance.toml")?;

    let (resolved, lock_report) = match &binding {
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
                .context("cannot resolve the instance's sources")?;
            let report = crate::build::lock(
                paths,
                &instance,
                &resolved,
                &LockOptions::default(),
                reporter,
            )?;
            (resolved, report)
        }
    };

    let n = lock_report.lock.packages.len();
    let mut summary = format!(
        "Created `{name}` ({n} package{})",
        if n == 1 { "" } else { "s" }
    );

    if *build {
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
