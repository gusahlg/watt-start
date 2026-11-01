//! Helpers shared by the op implementations: phase planning, recovery suggestions, module-spec
//! parsing and generation bookkeeping.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use pwc_instance::Instance;
use pwc_manifest::{ModuleRequirement, ModuleSource, PackageId, PackageManifest, VersionReq};
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
        OpRequest::Modules { build, .. } => {
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
            modules_ref,
        } => Binding::Managed {
            channel: channel.clone(),
            engine_ref: engine_ref.clone(),
            modules_ref: modules_ref.clone(),
            pin: None,
        },
        SourceSpec::Linked {
            pwc_source,
            repositories,
        } => Binding::Linked {
            pwc_source: crate::paths::persisted_path(pwc_source),
            repositories: repositories
                .iter()
                .map(|path| crate::paths::persisted_path(path))
                .collect(),
        },
        SourceSpec::PwcDefault => Binding::PwcDefault,
    }
}

/// One parsed `pwc module add` argument.
#[derive(Debug)]
pub(super) struct ParsedModuleSpec {
    /// The package id.
    pub id: PackageId,
    /// What to write into `instance.toml`.
    pub requirement: ModuleRequirement,
}

/// Port of pwc-cli's `parse_module_spec`: a bare id (`*`), `id@req`, a source directory (`module.toml`, or the older `mod.toml`)
/// or a `.pwcmodule` (or older `.pwcmod`) file. Relative paths are relative to `instance_dir`, not the process's working
/// directory.
pub(super) fn parse_module_spec(instance_dir: &Path, spec: &str) -> Result<ParsedModuleSpec> {
    #[cfg(windows)]
    if awkward_windows_path(spec) {
        bail!(
            "{spec}: drive-relative and rooted paths are not accepted; use a full path or a path \
             relative to the instance"
        );
    }
    let raw = Path::new(spec);
    // A separator, a leading `.`, or a `.pwcmodule`/`.pwcmod` suffix is a path. A bare id is not, even when
    // the instance contains a directory of that name (`game/` is every instance's save directory).
    let path_shaped = raw.is_absolute()
        || spec.contains('/')
        || spec.contains(std::path::MAIN_SEPARATOR)
        || spec.ends_with(".pwcmodule")
        || spec.ends_with(".pwcmod")
        || spec.starts_with('.');
    // Absolute paths stand as written. A relative path is resolved against the instance and
    // checked there, never against the process's working directory (which is wherever the
    // launcher happened to be started from, e.g. an Explorer shortcut).
    let resolved = if raw.is_absolute() {
        raw.exists().then(|| raw.to_path_buf())
    } else if path_shaped {
        let under_instance = instance_dir.join(raw);
        under_instance.exists().then_some(under_instance)
    } else {
        None
    };
    if let Some(resolved) = resolved {
        let resolved = crate::paths::canonical_path(&resolved).unwrap_or(resolved);
        if resolved.is_dir() {
            // `module.toml`, or the older `mod.toml` (an error if both exist), as pwc does.
            let manifest_path = pwc_manifest::find_manifest_file(&resolved)?
                .unwrap_or_else(|| resolved.join(pwc_manifest::MANIFEST_FILE));
            let manifest = PackageManifest::from_file(&manifest_path).with_context(|| {
                format!("{} is not a package source directory", resolved.display())
            })?;
            return Ok(ParsedModuleSpec {
                id: manifest.id().clone(),
                requirement: ModuleRequirement {
                    version: VersionReq::STAR,
                    source: ModuleSource::Path(resolved),
                },
            });
        }
        let package = Package::from_archive_file(&resolved)
            .with_context(|| format!("{} is not a valid .pwcmodule", resolved.display()))?;
        return Ok(ParsedModuleSpec {
            id: package.manifest().id().clone(),
            requirement: ModuleRequirement {
                version: VersionReq::STAR,
                source: ModuleSource::File(resolved),
            },
        });
    }
    if path_shaped {
        bail!("{spec}: no such file or directory");
    }
    let (id, req) = match spec.split_once('@') {
        Some((id, req)) => (id, Some(req)),
        None => (spec, None),
    };
    let id = PackageId::parse(id).with_context(|| {
        format!("`{spec}` is not a package id, id@requirement, source directory or .pwcmodule file")
    })?;
    let version = match req {
        Some(req) => VersionReq::parse(req)
            .with_context(|| format!("invalid version requirement `{req}`"))?,
        None => VersionReq::STAR,
    };
    Ok(ParsedModuleSpec {
        id,
        requirement: ModuleRequirement {
            version,
            source: ModuleSource::Registry,
        },
    })
}

/// `C:foo` (drive-relative) and `\foo` / `/foo` (rooted, not UNC). A full `C:\foo` or `\\server\share`
/// is a normal path and is not rejected here.
#[cfg(windows)]
fn awkward_windows_path(spec: &str) -> bool {
    let bytes = spec.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return !matches!(bytes.get(2).copied(), Some(b'\\' | b'/'));
    }
    if bytes.first() == Some(&b'/') {
        return true;
    }
    bytes.first() == Some(&b'\\') && bytes.get(1) != Some(&b'\\')
}

/// A generation's identity: `(environment, PWC source, binding pin)`. The environment hash alone
/// is not enough — two managed generations can have identical game and module revisions (so equal
/// environments) while differing only in the engine commit, which the hash does not cover.
pub(super) fn generation_identity(
    environment: &str,
    pwc_source: &Path,
    binding: &Binding,
) -> (String, PathBuf, Option<crate::meta::Pin>) {
    (
        environment.to_owned(),
        pwc_source.to_path_buf(),
        binding.pin().cloned(),
    )
}

/// Record a generation after a successful build: write `generations/<id>.lock`, update the
/// newest generation in place if it has the same identity ([`generation_identity`]), else insert
/// a new one at the front; prune beyond `settings.generations_to_keep` (keeping at least the one
/// just written). The pruned generations' files are deleted only after `meta.toml` is safely
/// committed without them (a kill in between leaves metadata that still names them, which is
/// merely a dangling rollback target, never a generation `meta.toml` claims exists without
/// actually recording). Emits `Change::Generation`.
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
    let identity = generation_identity(&environment, &pwc_source, binding);
    let packages: Vec<String> = lock
        .packages
        .iter()
        .map(|p| format!("{} {}", p.id, p.version))
        .collect();
    let game_version = lock.pwc.version.to_string();
    let lock_text = lock.to_toml_string();
    // The wanted modules belong to the generation too: restoring only the lock would leave an
    // `instance.toml` that the next relock "corrects" back to the newer module set.
    let manifest_path = instance_dir.join("instance.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;

    let mut pruned: Vec<u64> = Vec::new();
    let meta = crate::meta::update(instance_dir, |meta| {
        let reuse = meta.generations.first().is_some_and(|g| {
            generation_identity(&g.environment, &g.pwc_source, &g.source) == identity
        });
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
        write_atomic(
            &instance_paths.generation_manifest(id),
            manifest_text.as_bytes(),
        )?;

        let keep = settings.generations_to_keep.max(1) as usize;
        while meta.generations.len() > keep {
            if let Some(old) = meta.generations.pop() {
                pruned.push(old.id);
            }
        }
        Ok(())
    })?;
    // `meta.toml` no longer mentions these generations; only now is it safe to delete their
    // files (a kill before this point just leaves harmless, unreferenced leftovers).
    for id in pruned {
        let _ = std::fs::remove_file(instance_paths.generation_lock(id));
        let _ = std::fs::remove_file(instance_paths.generation_manifest(id));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `game/` exists in every instance. A package id must not be parsed as that directory.
    #[test]
    fn a_bare_id_is_not_the_instance_game_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("game")).unwrap();
        let err = parse_module_spec(tmp.path(), "game").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            !text.contains("package source"),
            "game/ was treated as a module path: {text}"
        );
        let parsed = parse_module_spec(tmp.path(), "demo.blocks").unwrap();
        assert!(matches!(parsed.requirement.source, ModuleSource::Registry));
        assert_eq!(parsed.id.as_str(), "demo.blocks");
    }

    fn write_manifest(dir: &Path, file: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(file),
            "format = 1\n\n[package]\nid = \"demo.blocks\"\nname = \"Demo\"\nversion = \"1.0.0\"\n\
             description = \"d\"\nauthors = [\"T\"]\nlicense = \"AGPL-3.0-or-later\"\n\
             license-files = [\"LICENSE\"]\nkind = \"module\"\npwc-api = \"^1.0\"\n",
        )
        .unwrap();
    }

    /// Source directories hold `module.toml`, or the older `mod.toml`; both parse.
    #[test]
    fn source_directories_accept_either_manifest_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(&tmp.path().join("new"), "module.toml");
        write_manifest(&tmp.path().join("old"), "mod.toml");
        for dir in ["new", "old"] {
            let parsed = parse_module_spec(tmp.path(), &format!("./{dir}")).unwrap();
            assert_eq!(parsed.id.as_str(), "demo.blocks");
            assert!(matches!(parsed.requirement.source, ModuleSource::Path(_)));
        }
    }

    /// `.pwcmodule` and the older `.pwcmod` are file paths, not package ids.
    #[test]
    fn both_archive_suffixes_are_paths() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["a.pwcmodule", "a.pwcmod"] {
            let err = parse_module_spec(tmp.path(), name).unwrap_err();
            assert!(format!("{err:#}").contains("no such file"), "{name}");
        }
    }

    /// A relative path is resolved against the instance, including when the working directory
    /// does not contain it.
    #[test]
    fn a_missing_relative_path_is_not_a_package_id() {
        let tmp = tempfile::tempdir().unwrap();
        let err = parse_module_spec(tmp.path(), "modules/example").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("no such file"),
            "expected a missing-path error, got {text}"
        );
    }
}
