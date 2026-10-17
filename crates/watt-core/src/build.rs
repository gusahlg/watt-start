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
//!
//! [`build`] takes the exact `Lockfile` its caller just locked (never rereading `pwc.lock`
//! itself, which the `pwc` CLI can rewrite concurrently), holds a launcher-wide `build.lock`
//! across `prepare`/`compile` to serialise the launcher's own builds, and double-checks that
//! `pwc.lock` still matches before handing anything to Cargo.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use pwc_builder::BuildRequest;
use pwc_instance::{Instance, LockOptions, LockReport, PwcSource};
use pwc_manifest::{BuildProfile, Lockfile};
use pwc_store::Store;

use crate::binding::ResolvedSource;
use crate::events::{Change, PackageChange, Phase, PhaseOutcome, Reporter};
use crate::locks::FileLock;
use crate::meta::BuiltGame;
use crate::paths::{Paths, now_secs};
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
    let pwc = PwcSource::open(&source.pwc_dir)
        .with_context(|| format!("cannot use the PWC source at {}", source.pwc_dir.display()))?;
    let report = pwc_instance::lock_instance(&paths.pwc, &source.config, instance, &pwc, options)
        .with_context(|| format!("cannot lock instance `{}`", instance.manifest.name))?;
    for warning in &report.warnings {
        reporter.warn(warning.clone());
    }
    let changes: Vec<PackageChange> = report
        .changes
        .iter()
        .map(|(id, old, new)| PackageChange {
            id: id.to_string(),
            old: old.clone(),
            new: new.clone(),
        })
        .collect();
    reporter.changed(Change::Lock {
        changes,
        environment: report.lock.environment.clone(),
    });
    Ok(report)
}

/// Why the instance's lock does not describe it built against `source` (`None` = current):
/// no lock, a different `[pwc].source`, or `Instance::stale_reason`.
pub fn stale_reason(instance: &Instance, source: &ResolvedSource) -> Result<Option<String>> {
    let Some(lock) = instance.lockfile().context("cannot read pwc.lock")? else {
        return Ok(Some("not locked yet".to_owned()));
    };
    let canonical_source = crate::paths::canonical_path(&source.pwc_dir)
        .with_context(|| format!("cannot resolve {}", source.pwc_dir.display()))?;
    // `paths_equal` so a `\\?\` path stored by an older tool matches the canonical one.
    if !crate::paths::paths_equal(&lock.pwc.source, &canonical_source) {
        return Ok(Some("the game source changed".to_owned()));
    }
    let pwc = PwcSource::open(&source.pwc_dir)
        .with_context(|| format!("cannot use the PWC source at {}", source.pwc_dir.display()))?;
    Ok(instance.stale_reason(&lock, &pwc)?)
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

/// Build `lock` (the exact lockfile the caller just resolved — callers lock first and pass what
/// they got; never a lock reread from disk, since another tool, including the `pwc` CLI, can
/// relock the same instance concurrently against a different source while this runs). Phases:
/// Toolchain, (WaitForBuild — both for the launcher-wide build lock below and, for an uncached
/// build, the shared Cargo target lock), Compile, (Link), Install; Compile/Link/WaitForBuild are
/// finished `Cached` or skipped when the cache answers. `force` rebuilds even when cached.
/// `profile` overrides the instance's `[build] profile`. Uses `settings.effective_jobs()`.
/// Emits `Change::Build`.
///
/// Holds `paths.build_lock()` (launcher-wide, not per instance) from just before `prepare`
/// through `compile`'s completion metadata, so two launcher builds with identical inputs never
/// race `pwc-builder`'s fixed temporary file names; the pre-existing per-target-directory lock
/// probe still coordinates with the `pwc` CLI (released before `compile`, which takes it itself).
#[allow(clippy::too_many_arguments)]
pub fn build(
    paths: &Paths,
    settings: &Settings,
    instance: &Instance,
    source: &ResolvedSource,
    lock: &Lockfile,
    profile: Option<BuildProfile>,
    force: bool,
    reporter: &Reporter,
) -> Result<BuildResult> {
    reporter.phase(Phase::Toolchain, false);
    let (rustc, target) =
        pwc_builder::rustc_info(&source.wrapper).context("cannot identify the Rust toolchain")?;

    let pwc = PwcSource::open(&source.pwc_dir)
        .with_context(|| format!("cannot use the PWC source at {}", source.pwc_dir.display()))?;
    let dirty_digest = pwc
        .dirty_digest()
        .context("cannot compute the PWC source's dirty digest")?;

    let store_dir = paths.pwc.store();
    let store = Store::open(&store_dir)
        .with_context(|| format!("cannot open the store at {}", store_dir.display()))?;
    let packages = pwc_instance::installed_packages(&store, lock)
        .context("cannot read the locked packages from the store")?;

    let profile = profile.unwrap_or(instance.manifest.profile);
    let request = BuildRequest {
        lock: lock.clone(),
        packages,
        pwc_source: pwc.dir.clone(),
        pwc_dirty_digest: dirty_digest,
        profile,
        builds_dir: paths.pwc.builds(),
        target_dir: paths.pwc.target(),
        wrapper: source.wrapper.clone(),
        jobs: settings.effective_jobs(),
        force,
        // Players never run the golden harness; it would double the final fat-LTO link.
        golden: false,
    };

    let build_lock_path = paths.build_lock();
    let _build_lock = match FileLock::try_acquire(&build_lock_path)? {
        // Free: keep holding it (unlike the target-directory probe below, this lock is ours for
        // the whole build, not just a presence check).
        Some(held) => held,
        None => {
            reporter.phase(Phase::WaitForBuild, false);
            let held = FileLock::acquire(&build_lock_path)?;
            reporter.finish_phase(PhaseOutcome::Ok);
            held
        }
    };

    let prepared =
        pwc_builder::prepare(&request, &rustc, &target).context("cannot prepare the build")?;

    // Resolving the toolchain and preparing the workspace above took time during which another
    // tool (the `pwc` CLI, most likely) could have relocked this instance: building `lock`
    // against a `pwc.lock` that no longer matches it would record a generation nobody can trust.
    // A cache hit needs no such check: nothing on disk is touched for it.
    if !prepared.cached {
        let current = instance.lockfile().context("cannot read pwc.lock")?;
        if current.as_ref().map(|l| l.environment.as_str()) != Some(lock.environment.as_str()) {
            anyhow::bail!("pwc.lock changed while building (another tool relocked it)");
        }
    }

    if prepared.cached {
        reporter.phase(Phase::Compile, false);
        reporter.finish_phase(PhaseOutcome::Cached);
        reporter.skip(Phase::WaitForBuild);
        reporter.skip(Phase::Link);
    } else {
        let target_lock_path = request.target_dir.join(TARGET_LOCK);
        match FileLock::try_acquire(&target_lock_path)? {
            // Free: release immediately, `compile` takes the same lock itself.
            Some(held) => drop(held),
            None => {
                reporter.phase(Phase::WaitForBuild, false);
                drop(FileLock::acquire(&target_lock_path)?);
                reporter.finish_phase(PhaseOutcome::Ok);
            }
        }
        reporter.phase(Phase::Compile, true);
    }

    let started = std::time::Instant::now();
    let output = pwc_builder::compile(&request, &prepared).context("cannot compile the build")?;
    let duration_ms = if output.cached {
        0
    } else {
        started.elapsed().as_millis() as u64
    };

    reporter.phase(Phase::Install, false);

    let built = BuiltGame {
        build_id: output.build_id.clone(),
        game: output.game.clone(),
        profile: profile.as_str().to_owned(),
        wrapper: source.wrapper.clone(),
        duration_ms,
        finished: now_secs(),
        rustc: rustc.lines().next().unwrap_or_default().to_owned(),
    };
    reporter.changed(Change::Build {
        build_id: output.build_id,
        cached: output.cached,
        game: output.game,
    });

    Ok(BuildResult {
        built,
        lock: lock.clone(),
        cached: output.cached,
        golden: output.golden,
    })
}
