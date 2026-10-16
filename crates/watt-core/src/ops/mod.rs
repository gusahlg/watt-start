//! The ops: everything that changes instances, sources or builds.
//!
//! An op is a plain function over [`Paths`], [`Settings`] and a [`Reporter`]. Front ends run ops
//! in a child process (`watt-start op <verb> ...`, built from [`OpRequest::to_args`]) and read the
//! events; tests call [`run`] directly with a `VecSink`.
//!
//! Rules every op follows:
//! - **One op per instance**: an instance op holds `watt-start/op.lock` for its whole run and
//!   fails at once with "busy" when another op holds it.
//! - **Crash safety**: an op can be killed at any point (cancel = SIGTERM/SIGKILL of its process
//!   group). Every file is written atomically and in an order that leaves a consistent state:
//!   sources are created before anything points at them; `pwc.lock` is written before the binding
//!   that it was made from is recorded (a lock newer than the binding is drift that the next op
//!   repairs by relocking); a generation is recorded only after its build finished.
//! - **Binding is truth**: the lock is derived from `meta.toml`'s binding and relocked whenever
//!   its `[pwc].source` does not match it.
//! - **Metadata** is only changed through `meta::update`.
//! - Every op ends with `meta.last_op` recorded (instance ops) and exactly one `OpFinished`.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::events::{OpKind, OpOutcome, Reporter};
use crate::paths::Paths;
use crate::settings::Settings;

/// Where a new instance's (or a re-bound instance's) sources come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceSpec {
    /// Launcher-managed sources following a game branch or tag.
    Managed {
        /// Game branch or tag.
        channel: String,
        /// Engine ref or `auto`.
        engine_ref: String,
        /// Package manager ref or `auto`.
        mods_ref: String,
    },
    /// A game checkout on disk.
    Linked {
        /// The checkout.
        pwc_source: PathBuf,
        /// Package repositories (empty = pwc's).
        repositories: Vec<PathBuf>,
    },
    /// pwc's global configuration.
    PwcDefault,
}

/// One op invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum OpRequest {
    /// Create an instance; managed sources are downloaded and locked right away. Mods are
    /// specs like `pwc mod add` takes (`id`, `id@req`, a source directory, a `.pwcmod`); a bare
    /// id is required as `*` (always the newest compatible version).
    Create {
        /// Directory name: `[a-z0-9][a-z0-9-]{0,63}`.
        name: String,
        /// Shown name.
        display_name: Option<String>,
        /// `#rrggbb`.
        color: Option<String>,
        /// Sidebar group.
        group: Option<String>,
        /// Sources.
        source: SourceSpec,
        /// Mod specs.
        mods: Vec<String>,
        /// Build right away.
        build: bool,
    },
    /// Add and remove mods, relock (restoring `instance.toml` if locking fails), then build when
    /// `build` (default: `settings.rebuild_after_mod_change`).
    Mods {
        /// Instance.
        instance: String,
        /// Specs to add.
        add: Vec<String>,
        /// Ids to remove.
        remove: Vec<String>,
        /// Override the setting.
        build: Option<bool>,
    },
    /// Managed: fetch, move the pin to the channel's newest revisions, back up worlds when the
    /// game changes, relock preferring the newest mods. Linked/pwc-default: relock preferring the
    /// newest mods. Then build when `build` (default: `settings.rebuild_after_update`).
    /// `reset_lock` resolves from scratch (needed when a mod changed without a version bump; for
    /// managed instances that is done automatically, with a warning).
    Update {
        /// Instance.
        instance: String,
        /// Override the setting.
        build: Option<bool>,
        /// Ignore the previous lock.
        reset_lock: bool,
    },
    /// Relock conservatively (repairs drift), then build; `force` rebuilds even when cached.
    Build {
        /// Instance.
        instance: String,
        /// Rebuild even if cached.
        force: bool,
        /// "release" or "dev" (default: the instance's profile).
        profile: Option<String>,
    },
    /// Start the game. Builds first when needed, unless `last_good` (then the newest generation
    /// whose executable exists runs, with that generation's assets, wrapper and environment).
    Play {
        /// Instance.
        instance: String,
        /// Run the last working build instead of building.
        last_good: bool,
        /// Extra arguments for the game.
        args: Vec<String>,
    },
    /// Re-activate a generation (default: the newest one that differs from the current lock):
    /// restore its binding and lock, recreate its sources if needed, then build (usually a cache
    /// hit).
    Rollback {
        /// Instance.
        instance: String,
        /// Generation id.
        generation: Option<u64>,
    },
    /// Copy an instance (manifest, lock, metadata; worlds when `with_worlds`).
    Duplicate {
        /// Source instance.
        instance: String,
        /// New directory name.
        new_name: String,
        /// New display name.
        display_name: Option<String>,
        /// Copy `game/` too.
        with_worlds: bool,
    },
    /// Delete an instance (refused while it runs or is busy). Store entries and snapshots stay
    /// until maintenance.
    Remove {
        /// Instance.
        instance: String,
    },
    /// Re-bind an instance's sources, then lock (and download, for managed) and build per
    /// `build` (default: `settings.rebuild_after_update`).
    SetSource {
        /// Instance.
        instance: String,
        /// The new sources.
        source: SourceSpec,
        /// Override the setting.
        build: Option<bool>,
    },
    /// Fetch the mirrors managed instances use and report available updates
    /// (`Change::UpdatesAvailable`). Changes no instance.
    CheckUpdates {
        /// One instance, or all managed ones.
        instance: Option<String>,
    },
    /// Housekeeping. `clean_cache` deletes pwc's build cache and target directory (refused while
    /// any op or build runs); `gc_snapshots` deletes snapshots no instance or generation uses and
    /// unpins their commits; `gc_store` deletes store packages no lock or generation lock uses.
    Maintenance {
        /// `pwc cache clean`.
        clean_cache: bool,
        /// Snapshot garbage collection.
        gc_snapshots: bool,
        /// Store garbage collection.
        gc_store: bool,
        /// Only report.
        dry_run: bool,
    },
}

impl OpRequest {
    /// Which op.
    pub fn kind(&self) -> OpKind {
        match self {
            Self::Create { .. } => OpKind::Create,
            Self::Mods { .. } => OpKind::Mods,
            Self::Update { .. } => OpKind::Update,
            Self::Build { .. } => OpKind::Build,
            Self::Play { .. } => OpKind::Play,
            Self::Rollback { .. } => OpKind::Rollback,
            Self::Duplicate { .. } => OpKind::Duplicate,
            Self::Remove { .. } => OpKind::Remove,
            Self::SetSource { .. } => OpKind::SetSource,
            Self::CheckUpdates { .. } => OpKind::CheckUpdates,
            Self::Maintenance { .. } => OpKind::Maintenance,
        }
    }

    /// The instance the op works on (the source instance for duplicate).
    pub fn instance(&self) -> Option<&str> {
        match self {
            Self::Create { name, .. } => Some(name),
            Self::Mods { instance, .. }
            | Self::Update { instance, .. }
            | Self::Build { instance, .. }
            | Self::Play { instance, .. }
            | Self::Rollback { instance, .. }
            | Self::Duplicate { instance, .. }
            | Self::Remove { instance }
            | Self::SetSource { instance, .. } => Some(instance),
            Self::CheckUpdates { instance } => instance.as_deref(),
            Self::Maintenance { .. } => None,
        }
    }

    /// The arguments after `watt-start` that run this request: `op --events json --request
    /// <json>`. (The human CLI has friendlier subcommands; the JSON form is the stable contract
    /// between the launcher and its own worker.)
    pub fn to_args(&self) -> Vec<String> {
        vec![
            "op".to_owned(),
            "--events".to_owned(),
            "json".to_owned(),
            "--request".to_owned(),
            serde_json::to_string(self).expect("requests serialise"),
        ]
    }
}

/// Run one op to completion: emits `OpStarted` ... `OpFinished` on `reporter` and returns the
/// outcome (also when it failed: errors become [`OpOutcome::Failed`] with recovery suggestions;
/// `events::Cancelled` becomes [`OpOutcome::Cancelled`]).
pub fn run(
    paths: &Paths,
    settings: &Settings,
    request: &OpRequest,
    reporter: &Reporter,
) -> OpOutcome {
    let _ = (paths, settings, request, reporter);
    todo!("worker B")
}

/// Shared by the op implementations: the result type of one op body (its success summary).
pub type OpResult = Result<String>;
