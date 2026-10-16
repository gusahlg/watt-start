//! Launcher metadata of one instance: `<instance>/watt-start/meta.toml`.
//!
//! pwc owns `instance.toml` (the wanted mods) and `pwc.lock` (the resolved set); this file holds
//! what only the launcher knows: how the instance is presented, where its sources come from (the
//! [`Binding`], which is the truth the lock is derived from) and its [`Generation`]s, the
//! known-good states it can roll back to.
//!
//! An instance without this file was made by `pwc`: it builds exactly like `pwc` would
//! ([`Binding::PwcDefault`]).
//!
//! **Write rule:** never load, keep and later save a copy. Every change goes through
//! [`update`], which re-reads the file under `meta.lock`, applies one closure and writes the
//! result, so the GUI renaming an instance and an op recording a build never overwrite each
//! other.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::locks::FileLock;
use crate::paths::{InstancePaths, write_atomic};

/// The current file format.
pub const META_FORMAT: u32 = 1;

/// The launcher's view of an instance.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Meta {
    /// [`META_FORMAT`] (0 when the file does not exist).
    pub format: u32,
    /// Shown instead of the directory name.
    pub display_name: Option<String>,
    /// Accent colour, `#rrggbb`.
    pub color: Option<String>,
    /// A group heading in the sidebar ("Survival", "Experiments").
    pub group: Option<String>,
    /// Pinned to the top.
    pub favorite: bool,
    /// Free text.
    pub notes: String,
    /// Unix seconds.
    pub created: Option<u64>,
    /// Unix seconds.
    pub last_played: Option<u64>,
    /// Where the game, engine and mods come from.
    pub source: Binding,
    /// Known-good states, newest first.
    pub generations: Vec<Generation>,
    /// The last op that ran on this instance.
    pub last_op: Option<LastOp>,
}

/// Where an instance's sources come from. The binding is the truth; `pwc.lock` is derived from
/// it and is relocked when it drifts (for example after `pwc run` relocked the instance against
/// the global `pwc-source`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Binding {
    /// Exactly like the `pwc` CLI: the global `pwc-source` and repositories from pwc's
    /// `config.toml`. Instances made with `pwc` are this.
    #[default]
    PwcDefault,
    /// A game checkout on disk that the launcher builds but never modifies (developers).
    /// Update = newest compatible mods + rebuild; pulling the checkout is up to its owner.
    Linked {
        /// The game checkout (its `../voxel-engine` sibling comes with it).
        pwc_source: PathBuf,
        /// Package repositories. Empty = pwc's configured repositories.
        #[serde(default)]
        repositories: Vec<PathBuf>,
    },
    /// Launcher-managed sources that follow a channel (a game branch or tag). Update fetches,
    /// moves the pin to the channel's newest revisions and relocks against fresh snapshots.
    Managed {
        /// Game branch or tag ("main").
        channel: String,
        /// Engine branch, tag or revision; `auto` = the same-named branch when the engine has
        /// one, else its default branch.
        #[serde(default = "auto")]
        engine_ref: String,
        /// Package manager branch, tag or revision; `auto` as for the engine.
        #[serde(default = "auto")]
        mods_ref: String,
        /// The exact revisions in use (None until the first update/create finished).
        #[serde(default)]
        pin: Option<Pin>,
    },
}

fn auto() -> String {
    AUTO.to_owned()
}

/// The `engine-ref`/`mods-ref` value that follows the channel.
pub const AUTO: &str = "auto";

/// Exact revisions of the three repositories.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Pin {
    /// Game commit (full hash).
    pub game: String,
    /// Engine commit.
    pub engine: String,
    /// Package manager commit.
    pub mods: String,
    /// The game ref it was resolved from ("main").
    #[serde(default)]
    pub game_ref: String,
    /// The engine ref it was resolved from.
    #[serde(default)]
    pub engine_ref: String,
    /// The package manager ref it was resolved from.
    #[serde(default)]
    pub mods_ref: String,
}

impl Pin {
    /// "main @ 1a2b3c4".
    pub fn label(&self) -> String {
        format!("{} @ {}", self.game_ref, short(&self.game))
    }
}

/// The first 7 characters of a hash.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// A state of the instance that built: the lock, the sources and the executable. Rollback
/// re-activates one; "play the last working build" runs its executable with its own inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Generation {
    /// 1, 2, 3... per instance.
    pub id: u64,
    /// Unix seconds.
    pub created: u64,
    /// The binding at the time (sources to restore).
    pub source: Binding,
    /// The game checkout the lock was made against (`[pwc].source`).
    pub pwc_source: PathBuf,
    /// The lock's environment hash.
    pub environment: String,
    /// Game version ("0.6.1").
    pub game_version: String,
    /// Locked packages, `id version`, for display.
    #[serde(default)]
    pub packages: Vec<String>,
    /// The build, if it finished.
    #[serde(default)]
    pub build: Option<BuiltGame>,
}

/// A finished build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuiltGame {
    /// Build ID.
    pub build_id: String,
    /// The game executable in the build cache (may be gone after `pwc cache clean`).
    pub game: PathBuf,
    /// "release" or "dev".
    pub profile: String,
    /// The wrapper the build ran behind (the game runs behind it too).
    #[serde(default)]
    pub wrapper: Vec<String>,
    /// How long the build took (0 when the cache answered).
    pub duration_ms: u64,
    /// Unix seconds.
    pub finished: u64,
    /// `rustc` release line ("rustc 1.95.0 (…)").
    #[serde(default)]
    pub rustc: String,
}

/// The last op that ran on an instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LastOp {
    /// Which op.
    pub op: crate::events::OpKind,
    /// Unix seconds.
    pub finished: u64,
    /// Whether it succeeded.
    pub ok: bool,
    /// The error, when it failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Its log file.
    #[serde(default)]
    pub log: Option<PathBuf>,
}

impl Meta {
    /// Load `meta.toml`; the default (format 0, [`Binding::PwcDefault`]) when it does not exist.
    pub fn load(instance_dir: &Path) -> Result<Self> {
        let path = InstancePaths::new(instance_dir).meta();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let meta: Self = toml::from_str(&text)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                if meta.format > META_FORMAT {
                    bail!(
                        "{} is format {}; this launcher reads format {META_FORMAT} (update the launcher)",
                        path.display(),
                        meta.format
                    );
                }
                Ok(meta)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Whether the launcher has ever written this instance's metadata.
    pub fn exists(&self) -> bool {
        self.format > 0
    }

    /// The name to show: the display name, else the directory name.
    pub fn title<'a>(&'a self, name: &'a str) -> &'a str {
        self.display_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(name)
    }

    /// The newest generation with a build whose executable still exists.
    pub fn last_good(&self) -> Option<&Generation> {
        self.generations
            .iter()
            .find(|g| g.build.as_ref().is_some_and(|b| b.game.is_file()))
    }

    /// The next generation number.
    pub fn next_generation_id(&self) -> u64 {
        self.generations.iter().map(|g| g.id).max().unwrap_or(0) + 1
    }
}

/// Read-modify-write `meta.toml` under `meta.lock`; returns the saved metadata.
pub fn update(instance_dir: &Path, change: impl FnOnce(&mut Meta) -> Result<()>) -> Result<Meta> {
    let paths = InstancePaths::new(instance_dir);
    let _lock = FileLock::acquire(&paths.meta_lock())?;
    let mut meta = Meta::load(instance_dir)?;
    change(&mut meta)?;
    meta.format = META_FORMAT;
    let text = toml::to_string_pretty(&meta).context("cannot serialise meta.toml")?;
    write_atomic(&paths.meta(), text.as_bytes())?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_pwc_default_and_updates_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let meta = Meta::load(dir).unwrap();
        assert!(!meta.exists());
        assert_eq!(meta.source, Binding::PwcDefault);
        assert_eq!(meta.title("survival"), "survival");

        let saved = update(dir, |m| {
            m.display_name = Some("Survival ⚡".into());
            m.source = Binding::Managed {
                channel: "main".into(),
                engine_ref: AUTO.into(),
                mods_ref: AUTO.into(),
                pin: Some(Pin {
                    game: "a".repeat(40),
                    engine: "b".repeat(40),
                    mods: "c".repeat(40),
                    game_ref: "main".into(),
                    engine_ref: "main".into(),
                    mods_ref: "main".into(),
                }),
            };
            m.generations.push(Generation {
                id: 1,
                created: 5,
                source: m.source.clone(),
                pwc_source: "/x".into(),
                environment: "sha256:00".into(),
                game_version: "0.6.1".into(),
                packages: vec!["pwc.hotbar 2.1.0".into()],
                build: None,
            });
            Ok(())
        })
        .unwrap();
        assert!(saved.exists());
        let loaded = Meta::load(dir).unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded.next_generation_id(), 2);
        assert_eq!(loaded.title("survival"), "Survival ⚡");

        let text = "format = 1\n[source]\nkind = \"managed\"\nchannel = \"mp-fix\"\n";
        std::fs::write(InstancePaths::new(dir).meta(), text).unwrap();
        let loaded = Meta::load(dir).unwrap();
        assert_eq!(
            loaded.source,
            Binding::Managed {
                channel: "mp-fix".into(),
                engine_ref: AUTO.into(),
                mods_ref: AUTO.into(),
                pin: None
            }
        );

        std::fs::write(InstancePaths::new(dir).meta(), "format = 9\n").unwrap();
        assert!(
            Meta::load(dir)
                .unwrap_err()
                .to_string()
                .contains("format 9")
        );
    }
}
