//! Helpers shared by the op implementations: phase planning, recovery suggestions, mod-spec
//! parsing and generation bookkeeping.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use pwc_instance::Instance;
use pwc_manifest::{ModManifest, ModRequirement, ModSource, PackageId, VersionReq};
use pwc_package::Package;

use crate::events::{Change, Phase, Recovery, Reporter};
use crate::meta::{Binding, Generation};
use crate::ops::{OpRequest, SourceSpec};
use crate::paths::{InstancePaths, Paths, now_secs, write_atomic};
use crate::settings::Settings;

/// The phases an op is expected to go through, best effort (some may end up skipped). Used only
/// for [`crate::events::Event::OpStarted`]; phases that run without being listed here still show
/// up (see `view::OpView`).
pub(super) fn planned_phases(request: &OpRequest) -> Vec<Phase> {
    use Phase::*;
    let build_phases = [Toolchain, WaitForBuild, Compile, Link, Install];
    match request {
        OpRequest::Create { source, build, .. } => {
            let mut phases = vec![Prepare];
            if matches!(source, SourceSpec::Managed { .. }) {
                phases.extend([Fetch, Checkout]);
            }
            phases.push(Lock);
            if *build {
                phases.extend(build_phases);
            }
            phases
        }
        OpRequest::Mods { build, .. } => {
            let mut phases = vec![Prepare, Lock];
            if build.unwrap_or(true) {
                phases.extend(build_phases);
            }
            phases
        }
        OpRequest::Update { build, .. } => {
            let mut phases = vec![Prepare, Fetch, Checkout, Backup, Lock];
            if build.unwrap_or(true) {
                phases.extend(build_phases);
            }
            phases
        }
        OpRequest::Build { .. } => {
            let mut phases = vec![Prepare, Lock];
            phases.extend(build_phases);
            phases
        }
        OpRequest::Play { last_good, .. } => {
            if *last_good {
                vec![Prepare, Launch]
            } else {
                let mut phases = vec![Prepare, Lock];
                phases.extend(build_phases);
                phases.push(Launch);
                phases
            }
        }
        OpRequest::Rollback { .. } => {
            let mut phases = vec![Prepare, Checkout, Lock];
            phases.extend(build_phases);
            phases
        }
        OpRequest::Duplicate { .. } | OpRequest::Remove { .. } => vec![Prepare],
        OpRequest::SetSource { source, build, .. } => {
            let mut phases = vec![Prepare];
            if matches!(source, SourceSpec::Managed { .. }) {
                phases.extend([Fetch, Checkout]);
            }
            phases.push(Lock);
            if build.unwrap_or(true) {
                phases.extend(build_phases);
            }
            phases
        }
        OpRequest::CheckUpdates { .. } => vec![Prepare, Fetch],
        OpRequest::Maintenance { .. } => vec![Cleanup],
    }
}

/// What to offer after a failure: an `InstanceError::ContentsChanged` suggests resetting the
/// lock; a failed Cargo run suggests retrying, forcing a rebuild, or playing the last good build
/// when there is one; a toolchain problem points at the doctor; anything else just offers a retry,
/// plus a rollback when there is somewhere to roll back to.
pub(super) fn recovery_for(
    paths: &Paths,
    instance_name: Option<&str>,
    e: &anyhow::Error,
) -> Vec<Recovery> {
    let contents_changed = e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<pwc_instance::InstanceError>(),
            Some(pwc_instance::InstanceError::ContentsChanged { .. })
        )
    });
    if contents_changed {
        return vec![Recovery::ResetLock, Recovery::Retry];
    }
    if let Some(build_error) = e
        .chain()
        .find_map(|c| c.downcast_ref::<pwc_builder::BuildError>())
    {
        match build_error {
            pwc_builder::BuildError::Cargo(_) => {
                let mut recovery = vec![Recovery::Retry, Recovery::ForceRebuild];
                if has_last_good(paths, instance_name) {
                    recovery.push(Recovery::PlayLastGood);
                }
                return recovery;
            }
            pwc_builder::BuildError::Toolchain(_) => {
                return vec![Recovery::Doctor, Recovery::Retry];
            }
            _ => {}
        }
    }
    let mut recovery = vec![Recovery::Retry];
    if generation_count(paths, instance_name) >= 2 {
        recovery.push(Recovery::Rollback);
    }
    recovery
}

fn load_meta_for(paths: &Paths, instance_name: Option<&str>) -> Option<crate::meta::Meta> {
    crate::meta::Meta::load(&paths.instance_dir(instance_name?)).ok()
}

fn has_last_good(paths: &Paths, instance_name: Option<&str>) -> bool {
    load_meta_for(paths, instance_name).is_some_and(|m| m.last_good().is_some())
}

fn generation_count(paths: &Paths, instance_name: Option<&str>) -> usize {
    load_meta_for(paths, instance_name).map_or(0, |m| m.generations.len())
}

/// A `SourceSpec` as a fresh (unpinned) [`Binding`].
pub(super) fn binding_from_spec(spec: &SourceSpec) -> Binding {
    match spec {
        SourceSpec::Managed {
            channel,
            engine_ref,
            mods_ref,
        } => Binding::Managed {
            channel: channel.clone(),
            engine_ref: engine_ref.clone(),
            mods_ref: mods_ref.clone(),
            pin: None,
        },
        SourceSpec::Linked {
            pwc_source,
            repositories,
        } => Binding::Linked {
            pwc_source: pwc_source.clone(),
            repositories: repositories.clone(),
        },
        SourceSpec::PwcDefault => Binding::PwcDefault,
    }
}

/// One parsed `pwc mod add` argument.
pub(super) struct ParsedModSpec {
    /// The package id.
    pub id: PackageId,
    /// What to write into `instance.toml`.
    pub requirement: ModRequirement,
}

/// Port of pwc-cli's `parse_mod_spec`: a bare id (`*`), `id@req`, a source directory (`mod.toml`)
/// or a `.pwcmod` file. Relative paths are relative to `instance_dir`.
pub(super) fn parse_mod_spec(instance_dir: &Path, spec: &str) -> Result<ParsedModSpec> {
    let raw = Path::new(spec);
    if raw.exists() {
        let resolved = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            instance_dir.join(raw)
        };
        let resolved = std::fs::canonicalize(&resolved).unwrap_or(resolved);
        if resolved.is_dir() {
            let manifest =
                ModManifest::from_file(&resolved.join("mod.toml")).with_context(|| {
                    format!("{} is not a package source directory", resolved.display())
                })?;
            return Ok(ParsedModSpec {
                id: manifest.id().clone(),
                requirement: ModRequirement {
                    version: VersionReq::STAR,
                    source: ModSource::Path(resolved),
                },
            });
        }
        let package = Package::from_archive_file(&resolved)
            .with_context(|| format!("{} is not a valid .pwcmod", resolved.display()))?;
        return Ok(ParsedModSpec {
            id: package.manifest().id().clone(),
            requirement: ModRequirement {
                version: VersionReq::STAR,
                source: ModSource::File(resolved),
            },
        });
    }
    if spec.contains('/') || spec.contains(std::path::MAIN_SEPARATOR) || spec.ends_with(".pwcmod") {
        bail!("{spec}: no such file or directory");
    }
    let (id, req) = match spec.split_once('@') {
        Some((id, req)) => (id, Some(req)),
        None => (spec, None),
    };
    let id = PackageId::parse(id).with_context(|| {
        format!("`{spec}` is not a package id, id@requirement, source directory or .pwcmod file")
    })?;
    let version = match req {
        Some(req) => VersionReq::parse(req)
            .with_context(|| format!("invalid version requirement `{req}`"))?,
        None => VersionReq::STAR,
    };
    Ok(ParsedModSpec {
        id,
        requirement: ModRequirement {
            version,
            source: ModSource::Registry,
        },
    })
}

/// Record a generation after a successful build: write `generations/<id>.lock`, update the
/// newest generation in place if it has the same environment and PWC source, else insert a new
/// one at the front; prune beyond `settings.generations_to_keep` (keeping at least the one just
/// written). Emits `Change::Generation`.
pub(super) fn record_generation(
    settings: &Settings,
    instance_dir: &Path,
    binding: &Binding,
    build_result: &crate::build::BuildResult,
    reporter: &Reporter,
) -> Result<u64> {
    let instance_paths = InstancePaths::new(instance_dir);
    let lock = &build_result.lock;
    let pwc_source = lock.pwc.source.clone();
    let environment = lock.environment.clone();
    let packages: Vec<String> = lock
        .packages
        .iter()
        .map(|p| format!("{} {}", p.id, p.version))
        .collect();
    let game_version = lock.pwc.version.to_string();
    let lock_text = lock.to_toml_string();

    let meta = crate::meta::update(instance_dir, |meta| {
        let reuse = meta
            .generations
            .first()
            .is_some_and(|g| g.environment == environment && g.pwc_source == pwc_source);
        if reuse {
            let g = meta.generations.first_mut().expect("checked above");
            g.created = now_secs();
            g.source = binding.clone();
            g.game_version = game_version.clone();
            g.packages = packages.clone();
            g.build = Some(build_result.built.clone());
        } else {
            let id = meta.next_generation_id();
            meta.generations.insert(
                0,
                Generation {
                    id,
                    created: now_secs(),
                    source: binding.clone(),
                    pwc_source: pwc_source.clone(),
                    environment: environment.clone(),
                    game_version: game_version.clone(),
                    packages: packages.clone(),
                    build: Some(build_result.built.clone()),
                },
            );
        }
        let id = meta.generations.first().expect("just written").id;
        write_atomic(&instance_paths.generation_lock(id), lock_text.as_bytes())?;

        let keep = settings.generations_to_keep.max(1) as usize;
        while meta.generations.len() > keep {
            if let Some(old) = meta.generations.pop() {
                let _ = std::fs::remove_file(instance_paths.generation_lock(old.id));
            }
        }
        Ok(())
    })?;
    let id = meta.generations.first().map(|g| g.id).unwrap_or(0);
    reporter.changed(Change::Generation { id });
    Ok(id)
}

/// Open an instance by directory name.
pub(super) fn open_instance(paths: &Paths, name: &str) -> Result<Instance> {
    let instance_name = pwc_manifest::InstanceName::parse(name)
        .with_context(|| format!("invalid instance name `{name}`"))?;
    Ok(Instance::open(&paths.pwc, &instance_name)?)
}

/// Copy a directory tree (used for world backups).
pub(super) fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in
        std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))?
    {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)
                .with_context(|| format!("cannot copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Keep only the `keep` newest (by name, which backups name by Unix seconds) entries of `dir`.
pub(super) fn prune_dir_entries(dir: &Path, keep: u32) -> Result<()> {
    let mut names: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(items) => items.filter_map(|i| i.ok()).map(|i| i.path()).collect(),
        Err(_) => return Ok(()),
    };
    names.sort();
    let keep = keep as usize;
    if names.len() > keep {
        for doomed in &names[..names.len() - keep] {
            let _ = std::fs::remove_dir_all(doomed);
        }
    }
    Ok(())
}
