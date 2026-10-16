//! Where the launcher keeps things.
//!
//! The launcher shares the package manager's directories (`pwc_instance::Dirs`, which honours
//! `PWC_HOME` and the XDG variables): instances made with `pwc` appear in the launcher and the
//! other way round, and every build shares one package store, one build cache and one Cargo
//! target directory. Its own state lives inside those roots, so `PWC_HOME` isolates a portable
//! installation completely:
//!
//! ```text
//! <pwc data>/watt-start/
//!   git/<repo>.git              bare mirrors of the game, the engine and the package manager
//!   git.lock                    held while mirrors or snapshots are modified
//!   build.lock                  held by the launcher from `prepare` through a build's metadata
//!   sources/v1/pwc-<g12>-<e12>/ game + engine snapshot (siblings, as Cargo needs)
//!   sources/v1/mods-<m12>/      package manager snapshot (its mods/ is a repository)
//!   logs/<op id>.log            one log per op
//! <pwc config>/watt-start.toml  launcher settings
//! <pwc data>/instances/<name>/watt-start/
//!   meta.toml                   launcher metadata (display name, source binding, generations)
//!   meta.lock                   held while meta.toml is read-modified-written
//!   op.lock                     held by the op working on this instance
//!   generations/<n>.lock        the pwc.lock of each recorded generation
//!   generations/<n>.instance.toml the wanted mods when that generation was built
//!   game.toml                   the running game (pid, started, log), while it runs
//!   game.lock                   held for exactly as long as the game process runs
//!   logs/game-<time>.log        game output
//! ```
//!
//! The snapshot paths end up in every lock (`[pwc].source`), every Build ID and every Cargo
//! fingerprint, so the data root is resolved to its canonical path once and never moves.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use pwc_instance::Dirs;
use serde::{Deserialize, Serialize};

/// The three repositories an instance is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Repo {
    /// `project_watt_cubed`.
    Game,
    /// `voxel-engine`.
    Engine,
    /// `pwc-package-manager` (its `mods/` directory is the first-party repository).
    Mods,
}

impl Repo {
    /// All three.
    pub const ALL: [Repo; 3] = [Repo::Game, Repo::Engine, Repo::Mods];

    /// The directory name inside a snapshot (and the GitHub repository name).
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Game => "project_watt_cubed",
            Self::Engine => "voxel-engine",
            Self::Mods => "pwc-package-manager",
        }
    }

    /// The public upstream.
    pub fn default_remote(self) -> String {
        format!("https://github.com/gusahlg/{}.git", self.dir_name())
    }

    /// For people: "Game", "Engine", "Mods".
    pub fn label(self) -> &'static str {
        match self {
            Self::Game => "Game",
            Self::Engine => "Voxel engine",
            Self::Mods => "Mods",
        }
    }
}

/// All the launcher's paths.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The package manager's directories.
    pub pwc: Dirs,
    /// `<pwc data>/watt-start`, canonical.
    pub root: PathBuf,
    /// `<pwc config>/watt-start.toml`.
    pub settings_file: PathBuf,
}

impl Paths {
    /// From the environment (`PWC_HOME`, XDG, HOME), like `pwc`.
    pub fn from_env() -> Result<Self> {
        let dirs = Dirs::from_env().context("cannot determine the pwc directories")?;
        Self::new(dirs)
    }

    /// From explicit pwc directories (tests use `Dirs::under(tmp)`). Creates the launcher root
    /// and resolves the data directory to its canonical path.
    pub fn new(mut pwc: Dirs) -> Result<Self> {
        std::fs::create_dir_all(&pwc.data)
            .with_context(|| format!("cannot create {}", pwc.data.display()))?;
        pwc.data = std::fs::canonicalize(&pwc.data)
            .with_context(|| format!("cannot resolve {}", pwc.data.display()))?;
        let root = pwc.data.join("watt-start");
        std::fs::create_dir_all(&root)
            .with_context(|| format!("cannot create {}", root.display()))?;
        let settings_file = pwc.config.join("watt-start.toml");
        Ok(Self {
            pwc,
            root,
            settings_file,
        })
    }

    /// `git/`.
    pub fn git(&self) -> PathBuf {
        self.root.join("git")
    }

    /// `git/<repo>.git`.
    pub fn mirror(&self, repo: Repo) -> PathBuf {
        self.git().join(format!("{}.git", repo.dir_name()))
    }

    /// `git.lock`: serialises mirror and snapshot changes across processes.
    pub fn git_lock(&self) -> PathBuf {
        self.root.join("git.lock")
    }

    /// `build.lock`: held by the launcher from `prepare` through the build's completion
    /// metadata, so two launcher builds with the same inputs never race `pwc-builder`'s fixed
    /// temporary file names. Outside `pwc`'s cache directories (which `pwc cache clean` deletes
    /// wholesale), so it is never pulled out from under a holder.
    pub fn build_lock(&self) -> PathBuf {
        self.root.join("build.lock")
    }

    /// `sources/v1`.
    pub fn sources(&self) -> PathBuf {
        self.root.join("sources").join("v1")
    }

    /// `logs/`.
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The log file of an op.
    pub fn op_log(&self, op_id: &str) -> PathBuf {
        self.logs().join(format!("{op_id}.log"))
    }

    /// `<pwc data>/instances/<name>`.
    pub fn instance_dir(&self, name: &str) -> PathBuf {
        self.pwc.instances().join(name)
    }
}

/// Paths inside one instance directory.
#[derive(Clone, Debug)]
pub struct InstancePaths {
    /// The pwc instance directory.
    pub dir: PathBuf,
}

impl InstancePaths {
    /// For an instance directory.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `watt-start/`.
    pub fn launcher(&self) -> PathBuf {
        self.dir.join("watt-start")
    }

    /// `watt-start/meta.toml`.
    pub fn meta(&self) -> PathBuf {
        self.launcher().join("meta.toml")
    }

    /// `watt-start/meta.lock`.
    pub fn meta_lock(&self) -> PathBuf {
        self.launcher().join("meta.lock")
    }

    /// `watt-start/op.lock`.
    pub fn op_lock(&self) -> PathBuf {
        self.launcher().join("op.lock")
    }

    /// `watt-start/generations/`.
    pub fn generations(&self) -> PathBuf {
        self.launcher().join("generations")
    }

    /// `watt-start/generations/<id>.lock`.
    pub fn generation_lock(&self, id: u64) -> PathBuf {
        self.generations().join(format!("{id}.lock"))
    }

    /// `watt-start/generations/<id>.instance.toml`: the wanted mods when the generation was built.
    pub fn generation_manifest(&self, id: u64) -> PathBuf {
        self.generations().join(format!("{id}.instance.toml"))
    }

    /// `watt-start/game.toml`.
    pub fn running(&self) -> PathBuf {
        self.launcher().join("game.toml")
    }

    /// `watt-start/game.lock`: held for exactly as long as the game runs (see
    /// `process::acquire_inheritable_lock`). This is the truth `status::compute` and `Play`/
    /// `Remove` use; `game.toml` only supplies the pid/log/started for display. Not held by a
    /// game started through the `pwc` CLI (`pwc run`): see `ARCHITECTURE.md`.
    pub fn game_lock(&self) -> PathBuf {
        self.launcher().join("game.lock")
    }

    /// `watt-start/logs/`.
    pub fn logs(&self) -> PathBuf {
        self.launcher().join("logs")
    }

    /// `watt-start/backups/`.
    pub fn backups(&self) -> PathBuf {
        self.launcher().join("backups")
    }

    /// `pwc.lock`.
    pub fn lock(&self) -> PathBuf {
        self.dir.join("pwc.lock")
    }

    /// `game/`: the game's data directory (worlds, settings).
    pub fn game_data(&self) -> PathBuf {
        self.dir.join("game")
    }
}

/// Write `bytes` to `path` atomically (temporary sibling, then rename), creating parents.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
