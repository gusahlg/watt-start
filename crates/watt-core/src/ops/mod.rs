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

use crate::events::{Cancelled, Event, OpKind, OpOutcome, PhaseOutcome, Recovery, Reporter};
use crate::locks::FileLock;
use crate::meta::LastOp;
use crate::paths::{InstancePaths, Paths, now_secs};
use crate::settings::Settings;

mod build;
mod common;
mod create;
mod instance;
mod maintenance;
mod mods;
mod play;
mod rollback;
mod update;

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
        /// Ids to move to their newest compatible version. Absent in older requests.
        #[serde(default)]
        update: Vec<String>,
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

    /// The arguments after `watt-start` that run this request by hand: `op --events json --request
    /// <json>`. The GUI writes the same JSON to the worker's stdin instead
    /// (`op --events json --request-stdin`), so a long mod list is not a command-line argument.
    /// (The human CLI has friendlier subcommands; the JSON form is the stable contract between
    /// the launcher and its own worker.)
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
    let op = request.kind();
    let instance_name = request.instance().map(str::to_owned);
    reporter.emit(Event::OpStarted {
        op,
        instance: instance_name.clone(),
        phases: common::planned_phases(request),
        log: Some(paths.op_log(reporter.op_id())),
    });

    // Every instance op but `Create` takes the lock up front (`Create` creates the directory,
    // and so the lock file, as its first step). Held for the rest of this call.
    let pre_lock = match &instance_name {
        // Taking the lock creates `watt-start/`, so never do it for an instance that does not
        // exist: a typo must not leave a ghost directory behind.
        Some(name)
            if !matches!(request, OpRequest::Create { .. })
                && !paths.instance_dir(name).join("instance.toml").is_file() =>
        {
            Err(format!("there is no instance named `{name}`"))
        }
        Some(name) if !matches!(request, OpRequest::Create { .. }) => {
            let lock_path = InstancePaths::new(paths.instance_dir(name)).op_lock();
            match FileLock::try_acquire(&lock_path) {
                Ok(Some(lock)) => Ok(Some(lock)),
                Ok(None) => Err(format!("another task is running on `{name}`")),
                Err(e) => Err(format!("{e:#}")),
            }
        }
        _ => Ok(None),
    };
    let _pre_lock = match pre_lock {
        Ok(lock) => lock,
        Err(error) => {
            let outcome = OpOutcome::Failed {
                phase: None,
                error,
                recovery: vec![Recovery::Retry],
            };
            reporter.emit(Event::OpFinished {
                outcome: outcome.clone(),
                duration_ms: reporter.elapsed_ms(),
            });
            return outcome;
        }
    };

    let result: Result<String> = match request {
        OpRequest::Create { .. } => create::run(paths, settings, request, reporter),
        OpRequest::Mods { .. } => mods::run(paths, settings, request, reporter),
        OpRequest::Update { .. } => update::run(paths, settings, request, reporter),
        OpRequest::Build { .. } => build::run(paths, settings, request, reporter),
        OpRequest::Play { .. } => play::run(paths, settings, request, reporter),
        OpRequest::Rollback { .. } => rollback::run(paths, settings, request, reporter),
        OpRequest::Duplicate { .. } => instance::duplicate(paths, settings, request, reporter),
        OpRequest::Remove { .. } => instance::remove(paths, settings, request, reporter),
        OpRequest::SetSource { .. } => instance::set_source(paths, settings, request, reporter),
        OpRequest::CheckUpdates { .. } => {
            maintenance::check_updates(paths, settings, request, reporter)
        }
        OpRequest::Maintenance { .. } => maintenance::run(paths, settings, request, reporter),
    };

    finish(paths, instance_name.as_deref(), request, reporter, result)
}

/// Turn the op body's result into an [`OpOutcome`], close the running phase, record
/// `meta.last_op` (unless this was a `Remove`, or there is no `instance.toml` to record it in),
/// and emit the single `OpFinished` every op ends with.
fn finish(
    paths: &Paths,
    instance_name: Option<&str>,
    request: &OpRequest,
    reporter: &Reporter,
    result: Result<String>,
) -> OpOutcome {
    let outcome = match result {
        Ok(summary) => {
            reporter.finish_phase(PhaseOutcome::Ok);
            OpOutcome::Ok { summary }
        }
        Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
            reporter.finish_phase(PhaseOutcome::Failed);
            OpOutcome::Cancelled
        }
        Err(e) => {
            let phase = reporter.current_phase();
            let recovery = common::recovery_for(paths, instance_name, &e);
            reporter.finish_phase(PhaseOutcome::Failed);
            OpOutcome::Failed {
                phase,
                error: format!("{e:#}"),
                recovery,
            }
        }
    };

    if let Some(name) = instance_name
        && !matches!(request, OpRequest::Remove { .. })
    {
        let dir = paths.instance_dir(name);
        if dir.join("instance.toml").is_file() {
            let (ok, error) = match &outcome {
                OpOutcome::Ok { .. } => (true, None),
                OpOutcome::Cancelled => (false, Some("cancelled".to_owned())),
                OpOutcome::Failed { error, .. } => (false, Some(error.clone())),
            };
            let _ = crate::meta::update(&dir, |m| {
                m.last_op = Some(LastOp {
                    op: request.kind(),
                    finished: now_secs(),
                    ok,
                    error,
                    log: Some(paths.op_log(reporter.op_id())),
                });
                Ok(())
            });
        }
    }

    reporter.emit(Event::OpFinished {
        outcome: outcome.clone(),
        duration_ms: reporter.elapsed_ms(),
    });
    outcome
}

/// Shared by the op implementations: the result type of one op body (its success summary).
pub type OpResult = Result<String>;

#[cfg(test)]
mod tests {
    use super::OpRequest;

    /// Older launcher builds omit `update`. It deserialises as an empty set and round-trips.
    #[test]
    fn mods_update_defaults_when_absent() {
        let missing = r#"{"op":"mods","instance":"survival","add":[],"remove":[],"build":false}"#;
        let parsed: OpRequest = serde_json::from_str(missing).expect("update is optional");
        match &parsed {
            OpRequest::Mods { update, build, .. } => {
                assert!(update.is_empty());
                assert_eq!(*build, Some(false));
            }
            other => panic!("expected Mods, got {other:?}"),
        }
        let again: OpRequest =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(parsed, again);
    }
}
