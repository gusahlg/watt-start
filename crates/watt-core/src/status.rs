//! What an instance needs: is it playable, does it need a build, is something running on it.
//! Read-only and cheap enough to recompute every few seconds off the UI thread (it never runs
//! `rustc`, `nix` or Cargo; it compares the lock with the generations and the binding).

use std::path::PathBuf;

use anyhow::Result;

use crate::catalog::InstanceCard;
use crate::meta::Generation;
use crate::paths::Paths;

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

/// Compute an instance's status. `NeedsBuild` when: the lock's environment differs from the
/// newest generation with a build; the lock's `[pwc].source` differs from what the binding
/// resolves to (drift after a `pwc` CLI relock, or an unpinned managed instance); the binding is
/// linked/pwc-default and the checkout's HEAD or dirty state moved since that build; or the
/// generation's executable is gone. `Broken` when the binding cannot be resolved (missing
/// checkout). The running game comes from `watt-start/game.toml` plus `process::is_alive`.
pub fn compute(paths: &Paths, card: &InstanceCard) -> Result<InstanceStatus> {
    let _ = (paths, card);
    todo!("worker B")
}
