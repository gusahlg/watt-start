//! Launcher settings: `<pwc config>/watt-start.toml`.
//!
//! Every field has a default, unknown keys are ignored (a newer launcher's file still loads), and
//! a missing file means "all defaults".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{Repo, write_atomic};

/// The current file format.
pub const SETTINGS_FORMAT: u32 = 1;

/// Launcher settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// [`SETTINGS_FORMAT`].
    pub format: u32,
    /// Rebuild right after an update.
    pub rebuild_after_update: bool,
    /// Rebuild right after confirming mod changes.
    pub rebuild_after_mod_change: bool,
    /// Fetch and look for updates when the launcher starts.
    pub check_updates_on_start: bool,
    /// Copy an instance's worlds aside before an update changes its game revision.
    pub backup_before_update: bool,
    /// How many world backups to keep per instance.
    pub backups_to_keep: u32,
    /// Cargo jobs; 0 = automatic (one per 2 GiB of RAM, at most one per CPU).
    pub build_jobs: u32,
    /// `nice` level for builds (0 = normal priority, 19 = lowest).
    pub build_nice: i32,
    /// Hide the launcher window while the game runs.
    pub close_on_play: bool,
    /// Generations kept per instance for rollback.
    pub generations_to_keep: u32,
    /// The branch new managed instances follow.
    pub default_channel: String,
    /// Where each repository is fetched from (missing = the public GitHub repository). A local
    /// checkout path works too, which is handy for developers.
    pub remotes: BTreeMap<Repo, String>,
    /// Package repositories searched in addition to the managed mods snapshot.
    pub extra_repositories: Vec<PathBuf>,
    /// How builds and the game are wrapped (`nix develop` on Nix systems).
    pub wrapper: WrapperSetting,
    /// Visual preferences.
    pub appearance: Appearance,
}

/// How Cargo and the game are wrapped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WrapperSetting {
    /// Whatever pwc's `config.toml` says (`auto` = `nix develop <game> --command` when the game
    /// has a flake and `nix` is installed). Honours `PWC_NO_WRAPPER`.
    FollowPwc,
    /// Never wrap: use the system's Rust and libraries.
    None,
}

/// Visual preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Appearance {
    /// Use the dark theme.
    pub dark: bool,
    /// Animations (the build forge, spinners). Off = static progress bars.
    pub animations: bool,
    /// UI scale factor.
    pub scale: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            dark: true,
            animations: true,
            scale: 1.0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: SETTINGS_FORMAT,
            rebuild_after_update: true,
            rebuild_after_mod_change: true,
            check_updates_on_start: true,
            backup_before_update: true,
            backups_to_keep: 3,
            build_jobs: 0,
            build_nice: 10,
            close_on_play: false,
            generations_to_keep: 5,
            default_channel: "main".to_owned(),
            remotes: BTreeMap::new(),
            extra_repositories: Vec::new(),
            wrapper: WrapperSetting::FollowPwc,
            appearance: Appearance::default(),
        }
    }
}

impl Settings {
    /// Load, or the defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("cannot read {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Save atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("cannot serialise the settings")?;
        write_atomic(path, text.as_bytes())
    }

    /// The remote a repository is fetched from.
    pub fn remote(&self, repo: Repo) -> String {
        self.remotes
            .get(&repo)
            .cloned()
            .unwrap_or_else(|| repo.default_remote())
    }

    /// Cargo `-j` for this machine (resolves `build_jobs = 0`).
    pub fn effective_jobs(&self) -> u32 {
        if self.build_jobs > 0 {
            return self.build_jobs;
        }
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        // Same truncation as dividing `/proc/meminfo`'s kibibytes by 1024²: whole gibibytes.
        let mem_gib =
            crate::process::total_memory_bytes().map(|bytes| (bytes / (1024 * 1024 * 1024)) as u32);
        match mem_gib {
            Some(gib) => (gib / 2).clamp(1, cpus),
            None => cpus,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_and_partial_files_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("watt-start.toml");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let mut s = Settings {
            rebuild_after_update: false,
            ..Settings::default()
        };
        s.remotes.insert(Repo::Engine, "/src/voxel-engine".into());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        std::fs::write(&path, "rebuild-after-mod-change = false\nfuture-key = 1\n").unwrap();
        let s = Settings::load(&path).unwrap();
        assert!(!s.rebuild_after_mod_change);
        assert!(s.rebuild_after_update);
        assert_eq!(s.remote(Repo::Game), Repo::Game.default_remote());
        assert!(s.effective_jobs() >= 1);
    }
}
