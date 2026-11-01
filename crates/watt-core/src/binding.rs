//! Turning an instance's [`Binding`] into what pwc's library functions take: a game checkout and
//! a `pwc_instance::Config` with the right repositories and wrapper. pwc's `lock_instance` and the
//! builder take these as parameters, which is how each instance gets its own sources without the
//! launcher ever touching pwc's global `config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use pwc_instance::{Config, RepositoryConfig, Wrapper};

use crate::meta::Binding;
use crate::paths::{Paths, Repo};
use crate::settings::{Settings, WrapperSetting};
use crate::snapshots;

/// Everything locking, building and running need to know about an instance's sources.
#[derive(Clone, Debug)]
pub struct ResolvedSource {
    /// The game checkout (`[pwc].source` of the lock).
    pub pwc_dir: PathBuf,
    /// pwc's configuration with `pwc-source`, the repositories and the wrapper for this instance.
    pub config: Config,
    /// The command prefix Cargo and the game run behind (empty = none).
    pub wrapper: Vec<String>,
}

impl ResolvedSource {
    /// `<game>/assets` (`WATT_ASSET_DIR`).
    pub fn asset_dir(&self) -> PathBuf {
        self.pwc_dir.join("assets")
    }
}

/// pwc's global configuration (`config.toml`; defaults when missing).
pub fn pwc_config(paths: &Paths) -> Result<Config> {
    let path = paths.pwc.config_file();
    Config::load(&path).with_context(|| format!("cannot read {}", path.display()))
}

/// Resolve a binding.
///
/// - `PwcDefault`: pwc's configuration as it is (error if no `pwc-source` is configured).
/// - `Linked`: that checkout; its repositories, or pwc's when the list is empty.
/// - `Managed`: the snapshots of its pin (which must exist: create them with
///   `snapshots::ensure_*` first); repositories = the modules snapshot's `modules/` (as `first-party`)
///   followed by the settings' extra repositories. An unpinned managed binding is an error.
///
/// The wrapper follows pwc's `[build] wrapper` (and `PWC_NO_WRAPPER`) unless the settings say
/// [`WrapperSetting::None`].
pub fn resolve(paths: &Paths, settings: &Settings, binding: &Binding) -> Result<ResolvedSource> {
    let global = pwc_config(paths)?;
    let mut config = global.clone();
    let pwc_dir = match binding {
        Binding::PwcDefault => global.pwc_source.clone().ok_or_else(|| {
            anyhow!("pwc has no game checkout configured (run `pwc setup`, or give this instance its own source)")
        })?,
        Binding::Linked {
            pwc_source,
            repositories,
        } => {
            if !repositories.is_empty() {
                config.repositories = named_repositories(repositories.iter().cloned());
            }
            pwc_source.clone()
        }
        Binding::Managed { pin, channel, .. } => {
            let Some(pin) = pin else {
                bail!("the `{channel}` sources have not been downloaded yet (run Update)");
            };
            let game = snapshots::game_snapshot(paths, &pin.game, &pin.engine);
            let modules = snapshots::modules_snapshot(paths, &pin.modules);
            if !snapshots::is_complete(&game.dir) || !snapshots::is_complete(&modules) {
                bail!(
                    "the sources for {} are missing (run Update or Rebuild to restore them)",
                    pin.label()
                );
            }
            let mut repos = vec![RepositoryConfig {
                name: "first-party".to_owned(),
                path: package_repository_dir(&modules.join(Repo::Modules.dir_name())),
            }];
            repos.extend(named_repositories(settings.extra_repositories.iter().cloned()));
            dedupe_names(&mut repos);
            config.repositories = repos;
            game.game
        }
    };
    // Canonical, and on Windows without a `\\?\` prefix, so the path handed to pwc (and from
    // there into `pwc.lock`) is the same shape Linux already stores. A missing checkout keeps
    // an absolute non-verbatim path instead of depending on the working directory.
    let pwc_dir = crate::paths::persisted_path(&pwc_dir);
    config.pwc_source = Some(pwc_dir.clone());
    if settings.wrapper == WrapperSetting::None {
        config.wrapper = Wrapper::Command(Vec::new());
    }
    let wrapper = config.wrapper_command(&pwc_dir);
    Ok(ResolvedSource {
        pwc_dir,
        config,
        wrapper,
    })
}

/// The first-party package directory of a package-manager checkout: `modules/`, or `mods/` in
/// snapshots of revisions from before the rename.
fn package_repository_dir(checkout: &Path) -> PathBuf {
    let modules = checkout.join("modules");
    if modules.is_dir() {
        return modules;
    }
    let legacy = checkout.join("mods");
    if legacy.is_dir() { legacy } else { modules }
}

/// Repositories named after their directories ("modules", "modules-2", ...).
fn named_repositories(dirs: impl Iterator<Item = PathBuf>) -> Vec<RepositoryConfig> {
    let mut repos: Vec<RepositoryConfig> = dirs
        .map(|path| RepositoryConfig {
            name: path.file_name().map_or_else(
                || "repository".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            path,
        })
        .collect();
    dedupe_names(&mut repos);
    repos
}

fn dedupe_names(repos: &mut [RepositoryConfig]) {
    let mut seen = std::collections::BTreeSet::new();
    for repo in repos.iter_mut() {
        let base = repo.name.clone();
        let mut n = 1;
        while !seen.insert(repo.name.clone()) {
            n += 1;
            repo.name = format!("{base}-{n}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `modules/` wins; an older package-manager snapshot with only `mods/` still resolves.
    #[test]
    fn the_package_directory_is_modules_or_the_older_mods() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        std::fs::create_dir_all(old.join("mods")).unwrap();
        assert_eq!(package_repository_dir(&old), old.join("mods"));
        let new = tmp.path().join("new");
        std::fs::create_dir_all(new.join("modules")).unwrap();
        std::fs::create_dir_all(new.join("mods")).unwrap();
        assert_eq!(package_repository_dir(&new), new.join("modules"));
        assert_eq!(
            package_repository_dir(&tmp.path().join("none")),
            tmp.path().join("none/modules")
        );
    }

    #[test]
    fn repository_names_are_unique() {
        let repos = named_repositories(
            ["/a/modules", "/b/modules", "/c/extra"]
                .into_iter()
                .map(PathBuf::from),
        );
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["modules", "modules-2", "extra"]);
    }
}
