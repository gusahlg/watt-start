//! The `Mods` op: add/remove mod requirements, relock, rebuild.

use anyhow::{Context as _, bail};
use pwc_instance::LockOptions;
use pwc_manifest::PackageId;

use crate::events::{Phase, Reporter};
use crate::ops::{OpRequest, OpResult};
use crate::paths::Paths;
use crate::settings::Settings;

use super::common;

pub(super) fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpResult {
    let OpRequest::Mods {
        instance: name,
        add,
        remove,
        build,
    } = request
    else {
        unreachable!("ops::run dispatches Mods requests only")
    };

    reporter.phase(Phase::Prepare, false);
    let mut instance = common::open_instance(paths, name)?;
    let original = instance.manifest.clone();

    for id in remove {
        let id = PackageId::parse(id).with_context(|| format!("`{id}` is not a package id"))?;
        if instance.manifest.mods.remove(&id).is_none() {
            bail!("`{id}` is not a mod of instance `{name}`");
        }
    }
    let mut added = Vec::new();
    for spec in add {
        let parsed = common::parse_mod_spec(&instance.dir, spec)?;
        added.push(parsed.id.clone());
        instance.manifest.mods.insert(parsed.id, parsed.requirement);
    }
    instance.save().context("cannot write instance.toml")?;

    let meta = crate::meta::Meta::load(&instance.dir)?;
    let resolved = crate::binding::resolve(paths, settings, &meta.source)
        .context("cannot resolve the instance's sources")?;

    reporter.phase(Phase::Lock, false);
    let lock_report = match crate::build::lock(
        paths,
        &instance,
        &resolved,
        &LockOptions::default(),
        reporter,
    ) {
        Ok(report) => report,
        Err(e) => {
            instance.manifest = original;
            instance
                .save()
                .context("cannot restore instance.toml after a failed relock")?;
            return Err(e);
        }
    };

    let mut summary = format!(
        "{} added, {} removed \u{b7} {} changed",
        added.len(),
        remove.len(),
        lock_report.changes.len()
    );

    if build.unwrap_or(settings.rebuild_after_mod_change) {
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
        common::record_generation(settings, &instance.dir, &meta.source, &built, reporter)?;
        summary.push_str(if built.cached {
            " \u{b7} built (cached)"
        } else {
            " \u{b7} built"
        });
    }

    Ok(summary)
}
