//! What an instance needs: is it playable, does it need a build, is something running on it.
//! Read-only and cheap enough to recompute every few seconds off the UI thread (it never runs
//! `rustc`, `nix` or Cargo; it compares the lock with the generations and the binding).

use std::path::{Path, PathBuf};

use anyhow::Result;
use pwc_instance::PwcSource;
use pwc_manifest::Lockfile;
use serde::Deserialize;

use crate::binding::{self, ResolvedSource};
use crate::catalog::InstanceCard;
use crate::locks::FileLock;
use crate::meta::{Binding, Generation};
use crate::paths::{InstancePaths, Paths};
use crate::process;
use crate::settings::Settings;

/// The state of an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceStatus {
    /// Whether it can be played as it is.
    pub readiness: Readiness,
    /// An op holds the instance's `op.lock` right now.
    pub busy: bool,
    /// The game is running: its pid.
    pub running: Option<u32>,
    /// The newest generation whose build still exists (what "play the last working build" runs).
    pub last_good: Option<Generation>,
}

/// Whether an instance can be played.
#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    /// Never locked: needs a first build (or, for managed instances, a first download).
    New,
    /// The current lock was built and the executable exists.
    Ready {
        /// The executable.
        game: PathBuf,
    },
    /// Something changed since the last build.
    NeedsBuild {
        /// Why, for people ("3 mods changed", "the game moved to 1a2b3c4").
        reason: String,
    },
    /// The instance cannot be built as it is.
    Broken {
        /// Why.
        reason: String,
    },
}

/// `watt-start/game.toml`.
#[derive(Deserialize)]
struct RunningGame {
    pid: u32,
}

/// Compute an instance's status. `NeedsBuild` when: the lock's environment differs from the
/// newest generation with a build; the lock's `[pwc].source` differs from what the binding
/// resolves to (drift after a `pwc` CLI relock, or an unpinned managed instance); the binding is
/// linked/pwc-default and the checkout's HEAD or dirty state moved since that build; or the
/// generation's executable is gone. `Broken` when the binding cannot be resolved (missing
/// checkout). The running game comes from `watt-start/game.toml` plus `process::is_alive`.
pub fn compute(paths: &Paths, card: &InstanceCard) -> Result<InstanceStatus> {
    let instance_paths = InstancePaths::new(&card.dir);
    let busy = FileLock::is_held(&instance_paths.op_lock());
    let running = running_pid(&card.dir);
    let last_good = card.meta.last_good().cloned();

    // Only the PWC source and repositories matter for resolving the binding and checking
    // staleness; the wrapper and extra repositories (the only other settings this reads) do not
    // change either, so a temporary default is fine here even though the real settings are not
    // passed to this function.
    let settings = Settings::default();
    let resolved = binding::resolve(paths, &settings, &card.meta.source);

    let readiness = match (&card.lock, resolved) {
        (None, Ok(_)) => Readiness::New,
        (None, Err(e)) => Readiness::Broken {
            reason: format!("{e:#}"),
        },
        (Some(_), Err(e)) => Readiness::Broken {
            reason: format!("{e:#}"),
        },
        (Some(lock), Ok(source)) => readiness_for_locked(card, lock, &source),
    };

    Ok(InstanceStatus {
        readiness,
        busy,
        running,
        last_good,
    })
}

fn readiness_for_locked(
    card: &InstanceCard,
    lock: &Lockfile,
    source: &ResolvedSource,
) -> Readiness {
    let canonical_source = match std::fs::canonicalize(&source.pwc_dir) {
        Ok(p) => p,
        Err(e) => {
            return Readiness::Broken {
                reason: format!("cannot resolve {}: {e}", source.pwc_dir.display()),
            };
        }
    };
    if lock.pwc.source != canonical_source {
        return Readiness::NeedsBuild {
            reason: "the game source moved".to_owned(),
        };
    }

    if matches!(
        card.meta.source,
        Binding::Linked { .. } | Binding::PwcDefault
    ) {
        match PwcSource::open(&source.pwc_dir) {
            Ok(current) => {
                if current.revision != lock.pwc.revision || current.dirty != lock.pwc.dirty {
                    return Readiness::NeedsBuild {
                        reason: "the game checkout changed since the lock".to_owned(),
                    };
                }
            }
            Err(e) => {
                return Readiness::Broken {
                    reason: e.to_string(),
                };
            }
        }
    }

    match card.meta.generations.first() {
        None => Readiness::NeedsBuild {
            reason: "not built yet".to_owned(),
        },
        Some(generation) => {
            if generation.environment != lock.environment {
                return Readiness::NeedsBuild {
                    reason: "mods changed since the last build".to_owned(),
                };
            }
            match &generation.build {
                None => Readiness::NeedsBuild {
                    reason: "not built yet".to_owned(),
                },
                Some(build) if !build.game.is_file() => Readiness::NeedsBuild {
                    reason: "the build's executable is gone".to_owned(),
                },
                Some(build) => Readiness::Ready {
                    game: build.game.clone(),
                },
            }
        }
    }
}

fn running_pid(instance_dir: &Path) -> Option<u32> {
    let path = InstancePaths::new(instance_dir).running();
    let text = std::fs::read_to_string(path).ok()?;
    let running: RunningGame = toml::from_str(&text).ok()?;
    process::is_alive(running.pid).then_some(running.pid)
}
