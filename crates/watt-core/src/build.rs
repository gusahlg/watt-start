//! Locking and building an instance with pwc's libraries, reporting phases.
//!
//! Locking is `pwc_instance::lock_instance` against the instance's resolved source. Building is
//! pwc-builder's pipeline, step by step so each step is a phase: `rustc_info` (Toolchain),
//! `prepare` (Build ID, cache lookup, generated workspace), a visible wait for the shared target
//! directory's lock (WaitForBuild, when another build holds it), `compile` (Compile, then Link;
//! Cargo's output reaches the op's captured stderr, see `process::capture_stderr`), Install.
//! Cargo is forced to draw its progress bar into the pipe (`CARGO_TERM_PROGRESS_WHEN=always`,
//! `CARGO_TERM_PROGRESS_WIDTH=200`, `CARGO_TERM_COLOR=never`), set in the op process's
//! environment so the wrapped cargo inherits it.

use std::path::PathBuf;

use anyhow::Result;
use pwc_instance::{Instance, LockOptions, LockReport};
use pwc_manifest::{BuildProfile, Lockfile};

use crate::binding::ResolvedSource;
use crate::events::Reporter;
use crate::meta::BuiltGame;
use crate::paths::Paths;
use crate::settings::Settings;

/// The name of pwc's target-directory lock file (`<target>/.pwc-build.lock`).
pub const TARGET_LOCK: &str = ".pwc-build.lock";

/// Lock the instance against `source` (`pwc_instance::lock_instance`), emitting a
/// `Change::Lock` with the changes and every discovery warning as a warning. The caller has
/// started the Lock phase.
pub fn lock(
    paths: &Paths,
    instance: &Instance,
    source: &ResolvedSource,
    options: &LockOptions,
    reporter: &Reporter,
) -> Result<LockReport> {
    let _ = (paths, instance, source, options, reporter);
    todo!("worker B")
}

/// Why the instance's lock does not describe it built against `source` (`None` = current):
/// no lock, a different `[pwc].source`, or `Instance::stale_reason`.
pub fn stale_reason(instance: &Instance, source: &ResolvedSource) -> Result<Option<String>> {
    let _ = (instance, source);
    todo!("worker B")
}

/// What a finished build produced.
#[derive(Clone, Debug)]
pub struct BuildResult {
    /// For the generation record.
    pub built: BuiltGame,
    /// The lock that was built.
    pub lock: Lockfile,
    /// The cache answered (no Cargo run).
    pub cached: bool,
    /// The golden-harness executable.
    pub golden: PathBuf,
}

/// Build the instance's current lock (which must exist and be current: callers lock first).
/// Phases: Toolchain, (WaitForBuild), Compile, (Link), Install; Compile/Link/WaitForBuild are
/// finished `Cached` or skipped when the cache answers. `force` rebuilds even when cached.
/// `profile` overrides the instance's `[build] profile`. Uses `settings.effective_jobs()`.
/// Emits `Change::Build`.
pub fn build(
    paths: &Paths,
    settings: &Settings,
    instance: &Instance,
    source: &ResolvedSource,
    profile: Option<BuildProfile>,
    force: bool,
    reporter: &Reporter,
) -> Result<BuildResult> {
    let _ = (paths, settings, instance, source, profile, force, reporter);
    todo!("worker B")
}
