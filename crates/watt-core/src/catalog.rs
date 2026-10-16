//! Read-only views for front ends: instances with their metadata, locked packages and status,
//! and the mods an instance could add. Nothing here writes; everything may be slow (it reads
//! the store, packages repositories on the fly, runs git), so front ends call it off the UI
//! thread.

use std::path::PathBuf;

use anyhow::Result;
use pwc_manifest::{InstanceManifest, Lockfile};

use crate::meta::Meta;
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

/// Every instance, sorted: favourites first, then by group, then by title. Instances whose
/// `instance.toml` cannot be read are skipped with a problem entry on no card; use
/// [`list_errors`] to show them.
pub fn list_instances(paths: &Paths) -> Result<Vec<InstanceCard>> {
    let _ = paths;
    todo!("worker B")
}

/// Instance directories that could not be loaded, with the reason.
pub fn list_errors(paths: &Paths) -> Vec<(String, String)> {
    let _ = paths;
    todo!("worker B")
}

/// One instance by name.
pub fn load_instance(paths: &Paths, name: &str) -> Result<InstanceCard> {
    let _ = (paths, name);
    todo!("worker B")
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
    let _ = (paths, settings, card);
    todo!("worker B")
}
