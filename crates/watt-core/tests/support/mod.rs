//! Test fixtures: a fake PWC source tree, a fake package repository and a wrapper script that
//! fakes `rustc` and `cargo` (ported from pwc-builder's own tests), so `cargo test -p
//! watt-core` never runs a real compiler.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use pwc_instance::{Config, Dirs, RepositoryConfig, Wrapper};

use watt_core::events::{CancelToken, OpOutcome, Reporter, VecSink};
use watt_core::ops::OpRequest;
use watt_core::paths::{Paths, Repo};
use watt_core::settings::Settings;

/// The local "remote" git checkouts a managed instance's mirrors are fetched from.
pub struct ManagedRemotes {
    pub game: PathBuf,
    pub engine: PathBuf,
    pub mods: PathBuf,
}

/// These tests exercise real OS primitives (advisory file locks, detached subprocesses) that
/// each test treats as exclusively its own; run them one at a time regardless of the harness's
/// thread count (`World::new` holds this for the whole test).
static SERIAL: Mutex<()> = Mutex::new(());

/// A whole fake installation: a PWC source, a mod repository, a fake toolchain, and the
/// launcher's own `Paths`.
pub struct World {
    _serial: MutexGuard<'static, ()>,
    pub root: tempfile::TempDir,
    pub pwc_dir: PathBuf,
    pub repo_dir: PathBuf,
    pub paths: Paths,
    pub settings: Settings,
}

impl World {
    /// Set up a fresh fake installation with no packages in the repository yet.
    pub fn new() -> Self {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().expect("tempdir");
        let pwc_dir = fake_pwc_source(root.path());
        let repo_dir = root.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let wrapper = write_fake_toolchain(root.path());

        let dirs = Dirs::under(&root.path().join("pwchome"));
        let paths = Paths::new(dirs).expect("Paths::new");

        let config = Config {
            pwc_source: Some(pwc_dir.clone()),
            active_instance: None,
            repositories: vec![RepositoryConfig {
                name: "first-party".to_owned(),
                path: repo_dir.clone(),
            }],
            wrapper: Wrapper::Command(vec![wrapper.to_string_lossy().into_owned()]),
            jobs: 0,
        };
        config
            .save(&paths.pwc.config_file())
            .expect("cannot write config.toml");

        let settings = Settings {
            build_jobs: 1,
            check_updates_on_start: false,
            ..Settings::default()
        };

        Self {
            _serial,
            root,
            pwc_dir,
            repo_dir,
            paths,
            settings,
        }
    }

    /// Write a package (`mod.toml`, `README.md`, `LICENSE`, `src/lib.rs`) into the repository at
    /// `<repo>/<id>-<version>/`.
    pub fn add_package(&self, id: &str, version: &str, deps: &[(&str, &str)]) -> PathBuf {
        let dir = self.repo_dir.join(format!("{id}-{version}"));
        write_package(&dir, id, version, deps);
        dir
    }

    /// Make the fake `cargo build` fail (prints an error and exits non-zero) until cleared.
    pub fn set_cargo_should_fail(&self, fail: bool) {
        let flag = self.root.path().join("cargo-should-fail");
        if fail {
            std::fs::write(flag, "").unwrap();
        } else {
            let _ = std::fs::remove_file(flag);
        }
    }

    /// Every `cargo build ...` invocation line logged by the fake wrapper, in order.
    pub fn cargo_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("cargo.log"))
            .map(|s| s.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Run one op in-process and return its outcome plus every event it emitted.
    pub fn run(&self, request: &OpRequest) -> (OpOutcome, Arc<VecSink>) {
        let sink = Arc::new(VecSink::default());
        let reporter = Reporter::new(sink.clone(), "t", CancelToken::default());
        let outcome = watt_core::ops::run(&self.paths, &self.settings, request, &reporter);
        (outcome, sink)
    }

    /// Create local, plain (non-bare) git checkouts standing in for the game, engine and mods
    /// repositories, each with a `main` branch, and point `settings.remotes` at them so
    /// `Managed` instances fetch from the filesystem instead of the network.
    pub fn add_managed_remotes(&mut self) -> ManagedRemotes {
        let game = init_git_repo(&self.root.path().join("remote-game"));
        write_pwc_tree(&game, "2.0.0");
        commit_all(&game, "game: initial");

        let engine = init_git_repo(&self.root.path().join("remote-engine"));
        std::fs::create_dir_all(engine.join("src")).unwrap();
        std::fs::write(
            engine.join("Cargo.toml"),
            "[package]\nname = \"voxel_engine\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(engine.join("src/lib.rs"), "").unwrap();
        commit_all(&engine, "engine: initial");

        let mods = init_git_repo(&self.root.path().join("remote-mods"));
        std::fs::create_dir_all(mods.join("mods")).unwrap();
        std::fs::write(mods.join("mods/.gitkeep"), "").unwrap();
        commit_all(&mods, "mods: initial");

        self.settings
            .remotes
            .insert(Repo::Game, game.to_string_lossy().into_owned());
        self.settings
            .remotes
            .insert(Repo::Engine, engine.to_string_lossy().into_owned());
        self.settings
            .remotes
            .insert(Repo::Mods, mods.to_string_lossy().into_owned());

        ManagedRemotes { game, engine, mods }
    }

    /// Add a package to a `mods` remote checkout (from [`World::add_managed_remotes`]) and commit
    /// it, so the next `Update`/`Create` fetch sees it.
    pub fn add_remote_package(
        &self,
        mods_repo: &Path,
        id: &str,
        version: &str,
        deps: &[(&str, &str)],
    ) {
        let dir = mods_repo.join("mods").join(format!("{id}-{version}"));
        write_package(&dir, id, version, deps);
        commit_all(mods_repo, &format!("mods: add {id} {version}"));
    }
}

fn init_git_repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    dir.to_path_buf()
}

/// `git add -A && git commit` (allowing an empty commit), returning the new `HEAD`.
pub fn commit_all(dir: &Path, message: &str) -> String {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    run_git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("cannot run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The fake PWC source tree's files, written at `dir` (used both for the local, non-git PWC
/// source and for the managed "game" remote checkout).
fn write_pwc_tree(dir: &Path, version: &str) {
    std::fs::create_dir_all(dir.join("crates/pwc-mod-api/src")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"project_watt_cubed\"\nversion = \"{version}\"\nedition = \"2024\"\n\n\
             [workspace]\nresolver = \"2\"\nmembers = [\"crates/pwc-mod-api\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Cargo.lock"), "# seed lock\nversion = 4\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        dir.join("crates/pwc-mod-api/Cargo.toml"),
        "[package]\nname = \"pwc-mod-api\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("crates/pwc-mod-api/src/lib.rs"), "").unwrap();
}

fn fake_pwc_source(root: &Path) -> PathBuf {
    let dir = root.join("project_watt_cubed");
    write_pwc_tree(&dir, "2.0.0");
    dir
}

fn write_package(dir: &Path, id: &str, version: &str, deps: &[(&str, &str)]) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let mut text = format!(
        "format = 1\n\n[package]\nid = \"{id}\"\nname = \"Test {id}\"\nversion = \"{version}\"\n\
         description = \"A test package for watt-core's tests.\"\nauthors = [\"Tester\"]\n\
         license = \"AGPL-3.0-or-later\"\nlicense-files = [\"LICENSE\"]\nkind = \"mod\"\n\
         pwc-api = \"^1.0\"\n"
    );
    if !deps.is_empty() {
        text.push_str("\n[dependencies]\n");
        for (dep_id, req) in deps {
            text.push_str(&format!("\"{dep_id}\" = \"{req}\"\n"));
        }
    }
    std::fs::write(dir.join("mod.toml"), text).unwrap();
    std::fs::write(dir.join("README.md"), format!("# {id}\n")).unwrap();
    std::fs::write(
        dir.join("LICENSE"),
        "GNU AFFERO GENERAL PUBLIC LICENSE\nVersion 3\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn register() {}\n").unwrap();
}

/// Write the wrapper script that fakes `rustc -vV` and `cargo build`, logging every `cargo`
/// invocation to `cargo.log` and writing fake `pwc-game`/`pwc-golden` executables that dump their
/// received environment to `<exe>.env` and then sleep briefly (so "is it running" checks have time
/// to observe them) when actually launched.
#[cfg(unix)]
fn write_fake_toolchain(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let script = root.join("fake-toolchain.sh");
    let log = root.join("cargo.log");
    let fail_flag = root.join("cargo-should-fail");
    let body = format!(
        r#"#!/bin/sh
set -e
tool="$1"; shift
case "$tool" in
  rustc)
    printf 'rustc 1.95.0 (fake)\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\nrelease: 1.95.0\n'
    ;;
  cargo)
    manifest=""
    profile=debug
    while [ $# -gt 0 ]; do
      case "$1" in
        --manifest-path) manifest="$2"; shift ;;
        --release) profile=release ;;
      esac
      shift
    done
    echo "cargo $* | cwd=$(pwd)" >> "{log}"
    if [ -f "{fail_flag}" ]; then
      echo "error[E9999]: the fake build was told to fail" 1>&2
      exit 101
    fi
    test -f "$manifest"
    test -f "$(dirname "$manifest")/bundle/src/lib.rs"
    mkdir -p "$CARGO_TARGET_DIR/$profile"
    for bin in pwc-game pwc-golden; do
      out="$CARGO_TARGET_DIR/$profile/$bin"
      printf '#!/bin/sh\nenv > "$0.env"\nsleep 5\n' > "$out"
      chmod +x "$out"
    done
    ;;
  *) exec "$tool" "$@" ;;
esac
"#,
        log = log.display(),
        fail_flag = fail_flag.display(),
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Windows stand-in for the shell wrapper. pwc looks for `pwc-game.exe` (`exe_name` adds
/// `.exe`) and `CreateProcess` will not run a `.cmd`, so this compiles a tiny Rust helper with
/// the host `rustc`. Copied into the Cargo target dir it is the fake game: when its own file
/// name is `pwc-game` or `pwc-golden` it dumps the environment to `<exe>.env` (so
/// `pwc-game.exe.env`, the name the tests derive from the executable path) and sleeps; otherwise
/// the first argument is `rustc` or `cargo`, matching the Unix script.
#[cfg(windows)]
fn write_fake_toolchain(root: &Path) -> PathBuf {
    let source_path = root.join("fake-toolchain.rs");
    let helper = root.join("fake-toolchain.exe");
    let log = root.join("cargo.log");
    let fail_flag = root.join("cargo-should-fail");
    let host = if cfg!(target_env = "msvc") {
        format!("{}-pc-windows-msvc", std::env::consts::ARCH)
    } else {
        format!("{}-pc-windows-gnu", std::env::consts::ARCH)
    };
    let source = format!(
        r#"
fn main() {{
    let exe = std::env::current_exe().unwrap_or_default();
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if stem == "pwc-game" || stem == "pwc-golden" {{
        let mut text = String::new();
        for (key, value) in std::env::vars() {{
            text.push_str(&key);
            text.push('=');
            text.push_str(&value);
            text.push('\n');
        }}
        let _ = std::fs::write(format!("{{}}.env", exe.display()), text);
        std::thread::sleep(std::time::Duration::from_secs(5));
        return;
    }}
    let mut args = std::env::args().skip(1);
    let tool = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    match tool.as_str() {{
        "rustc" => {{
            println!("rustc 1.95.0 (fake)");
            println!("binary: rustc");
            println!("host: {host}");
            println!("release: 1.95.0");
        }}
        "cargo" => {{
            let mut manifest = String::new();
            let mut profile = "debug".to_owned();
            let mut i = 0;
            while i < rest.len() {{
                match rest[i].as_str() {{
                    "--manifest-path" => {{
                        if let Some(path) = rest.get(i + 1) {{
                            manifest = path.clone();
                            i += 1;
                        }}
                    }}
                    "--release" => profile = "release".to_owned(),
                    _ => {{}}
                }}
                i += 1;
            }}
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open({log:?})
                .unwrap();
            use std::io::Write as _;
            let cwd = std::env::current_dir().unwrap_or_default();
            writeln!(file, "cargo {{}} | cwd={{}}", rest.join(" "), cwd.display()).unwrap();
            if std::path::Path::new({fail_flag:?}).is_file() {{
                eprintln!("error[E9999]: the fake build was told to fail");
                std::process::exit(101);
            }}
            let manifest_path = std::path::Path::new(&manifest);
            assert!(manifest_path.is_file(), "missing manifest");
            let bundle = manifest_path.parent().unwrap().join("bundle/src/lib.rs");
            assert!(bundle.is_file(), "missing bundle");
            let target = std::env::var("CARGO_TARGET_DIR").expect("CARGO_TARGET_DIR");
            let dir = std::path::PathBuf::from(target).join(profile);
            std::fs::create_dir_all(&dir).unwrap();
            let me = std::env::current_exe().unwrap();
            for bin in ["pwc-game.exe", "pwc-golden.exe"] {{
                std::fs::copy(&me, dir.join(bin)).unwrap();
            }}
        }}
        other => {{
            let status = std::process::Command::new(other).args(&rest).status().unwrap();
            std::process::exit(status.code().unwrap_or(1));
        }}
    }}
}}
"#
    );
    std::fs::write(&source_path, source).unwrap();
    let status = std::process::Command::new("rustc")
        .arg(&source_path)
        .arg("-o")
        .arg(&helper)
        .status()
        .expect("rustc for the fake toolchain");
    assert!(status.success(), "fake toolchain failed to compile");
    helper
}
