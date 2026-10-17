//! Read-only views for front ends: instances with their metadata, locked packages and status,
//! and the mods an instance could add. Nothing here writes; everything may be slow (it reads
//! the store, packages repositories on the fly, runs git), so front ends call it off the UI
//! thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result};
use pwc_manifest::{InstanceManifest, Lockfile, ModKind, PackageId};
use pwc_store::Store;

use crate::binding;
use crate::meta::{Binding, Meta};
use crate::paths::Paths;
use crate::settings::Settings;

/// One instance as a front end shows it.
#[derive(Clone, Debug)]
pub struct InstanceCard {
    /// Directory name (`pwc --instance <name>`).
    pub name: String,
    /// The instance directory.
    pub dir: PathBuf,
    /// `instance.toml`.
    pub manifest: InstanceManifest,
    /// `watt-start/meta.toml` (default when the launcher never touched it).
    pub meta: Meta,
    /// `pwc.lock`, if any (None also when unreadable; see `problems`).
    pub lock: Option<Lockfile>,
    /// The locked packages (empty without a lock), dependencies first as in the lock order.
    pub packages: Vec<PackageCard>,
    /// What is known about the game it builds.
    pub game: GameInfo,
    /// Whether this is pwc's active instance.
    pub active: bool,
    /// Readable problems found while loading (unreadable lock, missing snapshot...).
    pub problems: Vec<String>,
}

/// A locked package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageCard {
    /// Package id.
    pub id: String,
    /// Display name from its manifest (the id when the store entry is missing).
    pub name: String,
    /// Locked version.
    pub version: String,
    /// "mod", "library" or "bundle".
    pub kind: String,
    /// Manifest description.
    pub description: String,
    /// Requested in `instance.toml` (not only pulled in as a dependency).
    pub direct: bool,
    /// Manifest categories.
    pub categories: Vec<String>,
    /// Manifest authors.
    pub authors: Vec<String>,
    /// SPDX licence.
    pub license: String,
    /// Direct dependencies (ids).
    pub dependencies: Vec<String>,
    /// Ids of the bundles that include this package.
    pub in_bundles: Vec<String>,
    /// Where it came from (`repository:…`, `path:…`, `store`).
    pub source: String,
}

/// The game an instance builds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameInfo {
    /// Game version from the lock ("0.6.1").
    pub version: Option<String>,
    /// Mod API version from the lock.
    pub api: Option<String>,
    /// Game commit from the lock.
    pub revision: Option<String>,
    /// The lock was made from uncommitted changes.
    pub dirty: bool,
    /// The game checkout.
    pub source_dir: Option<PathBuf>,
    /// Channel or branch: the managed channel, else the checkout's current branch.
    pub branch: Option<String>,
    /// Engine commit (managed: the pin; linked/pwc-default: the sibling checkout's HEAD).
    pub engine_revision: Option<String>,
    /// Package manager commit (managed instances).
    pub mods_revision: Option<String>,
    /// The lock's environment hash.
    pub environment: Option<String>,
}

/// Instance directories with an `instance.toml`, sorted by name.
fn instance_names(paths: &Paths) -> Result<Vec<String>> {
    let root = paths.pwc.instances();
    let items = match std::fs::read_dir(&root) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut names = Vec::new();
    for item in items {
        let item = item.with_context(|| format!("cannot read {}", root.display()))?;
        if item.path().join("instance.toml").is_file()
            && let Some(name) = item.file_name().to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// What pwc's config says is the active instance, read once per scan.
enum ActiveLookup {
    /// `config.toml` loaded. `None` means no instance is active.
    Name(Option<String>),
    /// `config.toml` could not be read. The message is attached to every card.
    Failed(String),
}

fn read_active(paths: &Paths) -> ActiveLookup {
    match binding::pwc_config(paths) {
        Ok(config) => ActiveLookup::Name(config.active_instance),
        Err(e) => ActiveLookup::Failed(format!("cannot read pwc's config.toml: {e}")),
    }
}

fn open_store(paths: &Paths) -> Result<Store> {
    let store_dir = paths.pwc.store();
    Store::open(&store_dir)
        .with_context(|| format!("cannot open the store at {}", store_dir.display()))
}

/// An instance directory that did not load: `(name, reason)`.
pub type LoadError = (String, String);

/// Every instance in one pass: cards that loaded, and `(name, reason)` for the ones that did not.
///
/// Parses pwc's `config.toml` once and opens the package store once. [`list_instances`] and
/// [`list_errors`] are this scan with one half dropped; front ends that need both should call
/// this so a refresh does not walk the catalog twice.
pub fn scan_instances(paths: &Paths) -> Result<(Vec<InstanceCard>, Vec<LoadError>)> {
    let names = instance_names(paths)?;
    let active = read_active(paths);
    let store = match open_store(paths) {
        Ok(store) => store,
        Err(e) => {
            let reason = format!("{e:#}");
            let errors = names
                .into_iter()
                .map(|name| (name, reason.clone()))
                .collect();
            return Ok((Vec::new(), errors));
        }
    };
    let mut cards = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        match load_instance_with(paths, &name, &active, &store) {
            Ok(card) => cards.push(card),
            Err(e) => errors.push((name, format!("{e:#}"))),
        }
    }
    cards.sort_by_key(sort_key);
    Ok((cards, errors))
}

/// Every instance, sorted: favourites first, then by group, then by title. Instances whose
/// `instance.toml` cannot be read are skipped with a problem entry on no card; use
/// [`list_errors`] to show them.
pub fn list_instances(paths: &Paths) -> Result<Vec<InstanceCard>> {
    Ok(scan_instances(paths)?.0)
}

/// `(favourite-first, group, title)`.
fn sort_key(card: &InstanceCard) -> (bool, String, String) {
    (
        !card.meta.favorite,
        card.meta.group.clone().unwrap_or_default(),
        card.meta.title(&card.name).to_owned(),
    )
}

/// Instance directories that could not be loaded, with the reason.
pub fn list_errors(paths: &Paths) -> Vec<(String, String)> {
    scan_instances(paths)
        .map(|(_, errors)| errors)
        .unwrap_or_default()
}

/// One instance by name.
pub fn load_instance(paths: &Paths, name: &str) -> Result<InstanceCard> {
    let active = read_active(paths);
    let store = open_store(paths)?;
    load_instance_with(paths, name, &active, &store)
}

fn load_instance_with(
    paths: &Paths,
    name: &str,
    active: &ActiveLookup,
    store: &Store,
) -> Result<InstanceCard> {
    let instance_name = pwc_manifest::InstanceName::parse(name)
        .with_context(|| format!("invalid instance name `{name}`"))?;
    let instance = pwc_instance::Instance::open(&paths.pwc, &instance_name)
        .with_context(|| format!("cannot open instance `{name}`"))?;
    let meta = Meta::load(&instance.dir)
        .with_context(|| format!("cannot read the metadata of `{name}`"))?;

    let mut problems = Vec::new();
    let lock = match instance.lockfile() {
        Ok(lock) => lock,
        Err(e) => {
            problems.push(format!("cannot read pwc.lock: {e}"));
            None
        }
    };

    let mut packages = Vec::new();
    if let Some(lock) = &lock {
        let mut in_bundles: BTreeMap<&PackageId, Vec<String>> = BTreeMap::new();
        for p in &lock.packages {
            if p.kind == ModKind::Bundle {
                for dep in &p.dependencies {
                    in_bundles.entry(dep).or_default().push(p.id.to_string());
                }
            }
        }
        for p in &lock.packages {
            let fallback = || {
                (
                    p.id.to_string(),
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    String::new(),
                )
            };
            let (display_name, description, categories, authors, license) = match store.get(&p.hash)
            {
                Ok(Some(entry)) => {
                    let pkg = &entry.manifest.package;
                    (
                        pkg.name.clone(),
                        pkg.description.clone(),
                        pkg.categories
                            .iter()
                            .map(|c| c.as_str().to_owned())
                            .collect(),
                        pkg.authors.clone(),
                        pkg.license.clone(),
                    )
                }
                Ok(None) => {
                    problems.push(format!("{} ({}) is missing from the store", p.id, p.hash));
                    fallback()
                }
                Err(e) => {
                    problems.push(format!("cannot read {} from the store: {e}", p.id));
                    fallback()
                }
            };
            packages.push(PackageCard {
                id: p.id.to_string(),
                name: display_name,
                version: p.version.to_string(),
                kind: p.kind.as_str().to_owned(),
                description,
                direct: instance.manifest.mods.contains_key(&p.id),
                categories,
                authors,
                license,
                dependencies: p.dependencies.iter().map(|d| d.to_string()).collect(),
                in_bundles: in_bundles.get(&p.id).cloned().unwrap_or_default(),
                source: p.source.to_string(),
            });
        }
    }

    let game = game_info(&meta, lock.as_ref());

    let is_active = match active {
        ActiveLookup::Name(current) => current.as_deref() == Some(name),
        ActiveLookup::Failed(message) => {
            problems.push(message.clone());
            false
        }
    };

    Ok(InstanceCard {
        name: name.to_owned(),
        dir: instance.dir.clone(),
        manifest: instance.manifest,
        meta,
        lock,
        packages,
        game,
        active: is_active,
        problems,
    })
}

fn game_info(meta: &Meta, lock: Option<&Lockfile>) -> GameInfo {
    let Some(lock) = lock else {
        return GameInfo::default();
    };
    let source_dir = Some(lock.pwc.source.clone());
    GameInfo {
        version: Some(lock.pwc.version.to_string()),
        api: Some(lock.pwc.api.to_string()),
        revision: (lock.pwc.revision != pwc_instance::UNKNOWN_REVISION)
            .then(|| lock.pwc.revision.clone()),
        dirty: lock.pwc.dirty,
        branch: branch_of(meta, source_dir.as_deref()),
        engine_revision: engine_revision_of(meta, source_dir.as_deref()),
        mods_revision: match &meta.source {
            Binding::Managed { pin: Some(pin), .. } => Some(pin.mods.clone()),
            _ => None,
        },
        environment: Some(lock.environment.clone()),
        source_dir,
    }
}

fn branch_of(meta: &Meta, source_dir: Option<&Path>) -> Option<String> {
    match &meta.source {
        Binding::Managed { channel, .. } => Some(channel.clone()),
        _ => source_dir.and_then(git_branch),
    }
}

fn engine_revision_of(meta: &Meta, source_dir: Option<&Path>) -> Option<String> {
    match &meta.source {
        Binding::Managed { pin: Some(pin), .. } => Some(pin.engine.clone()),
        Binding::Managed { pin: None, .. } => None,
        _ => {
            let dir = source_dir?;
            let engine_dir = dir.parent()?.join("voxel-engine");
            if !engine_dir.is_dir() {
                return None;
            }
            git_rev_parse_head(&engine_dir)
        }
    }
}

/// `git -C <dir> rev-parse --abbrev-ref HEAD`; `None` when detached or git fails.
fn git_branch(dir: &Path) -> Option<String> {
    let out = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    (!out.is_empty() && out != "HEAD").then_some(out)
}

/// `git -C <dir> rev-parse HEAD`; `None` when git fails.
fn git_rev_parse_head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"])
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A mod an instance could add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableMod {
    /// Package id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Newest version.
    pub version: String,
    /// Every known version, newest first.
    pub versions: Vec<String>,
    /// "mod", "library" or "bundle".
    pub kind: String,
    /// Description.
    pub description: String,
    /// Categories.
    pub categories: Vec<String>,
    /// Keywords.
    pub keywords: Vec<String>,
    /// Authors.
    pub authors: Vec<String>,
    /// Licence.
    pub license: String,
    /// Dependencies of the newest version (ids).
    pub dependencies: Vec<String>,
    /// Members, for bundles (its dependencies).
    pub members: Vec<String>,
    /// The `pwc-api` requirement, if any.
    pub pwc_api: Option<String>,
}

/// The mods offered to an instance: its resolved source's repositories plus the store
/// (`pwc_instance::Available::scan`), newest version per id, sorted by name. Libraries are
/// included (front ends usually hide them). Returns discovery warnings too.
pub fn available_mods(
    paths: &Paths,
    settings: &Settings,
    card: &InstanceCard,
) -> Result<(Vec<AvailableMod>, Vec<String>)> {
    let source = match binding::resolve(paths, settings, &card.meta.source) {
        Ok(source) => source,
        Err(e) => return Ok((Vec::new(), vec![format!("{e:#}")])),
    };
    let store_dir = paths.pwc.store();
    let store = Store::open(&store_dir)
        .with_context(|| format!("cannot open the store at {}", store_dir.display()))?;
    let available = pwc_instance::Available::scan(&source.config, &store)
        .context("cannot scan the configured repositories")?;
    let warnings = available.warnings.clone();

    let mut mods: Vec<AvailableMod> = available
        .by_id()
        .into_iter()
        .filter_map(|(id, versions)| {
            let newest = versions.last()?;
            let m = &newest.manifest;
            Some(AvailableMod {
                id: id.to_string(),
                name: m.package.name.clone(),
                version: m.version().to_string(),
                versions: versions
                    .iter()
                    .rev()
                    .map(|v| v.manifest.version().to_string())
                    .collect(),
                kind: m.kind().as_str().to_owned(),
                description: m.package.description.clone(),
                categories: m
                    .package
                    .categories
                    .iter()
                    .map(|c| c.as_str().to_owned())
                    .collect(),
                keywords: m.package.keywords.clone(),
                authors: m.package.authors.clone(),
                license: m.package.license.clone(),
                dependencies: m.dependencies.keys().map(|d| d.to_string()).collect(),
                members: if m.kind() == ModKind::Bundle {
                    m.dependencies.keys().map(|d| d.to_string()).collect()
                } else {
                    Vec::new()
                },
                pwc_api: m.package.pwc_api.as_ref().map(|r| r.to_string()),
            })
        })
        .collect();
    mods.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok((mods, warnings))
}
