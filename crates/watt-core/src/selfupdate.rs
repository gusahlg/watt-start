//! Launcher self-update: manifest, install-kind detection, download, and replace.
//!
//! The check runs in the GUI process (not as an op). A release binary is replaced by
//! rename-then-rename, keeping `<exe>-prev` for a manual rollback. Package-managed and
//! development installs are reported with instructions and are never replaced.
//!
//! The minisign public key is compiled in from `packaging/update-key.pub` (and
//! `update-key-next.pub` for rotation) when those files exist. With a key, a missing or
//! invalid signature is a hard failure. Without one, the updater trusts HTTPS plus the
//! sha256 in the manifest.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use pwc_manifest::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest as _;

use crate::http::{Fetch, Fetched};
use crate::paths::{now_secs, write_atomic};

/// How often a successful check may be repeated unless the user asks again.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Where the published manifest lives, unless `WATT_START_UPDATE_URL` overrides it.
pub const DEFAULT_UPDATE_BASE: &str =
    "https://github.com/gusahlg/watt-start/releases/latest/download";

const MANIFEST_FILE: &str = "watt-start-update.json";
const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_SIGNATURE: u64 = 64 * 1024;
const MAX_BINARY: u64 = 512 * 1024 * 1024;

/// Manifest format this updater understands.
pub const MANIFEST_FORMAT: u32 = 1;

/// The asset key for the binary this process was built as.
///
/// `std::env::consts` is fixed from the target `cfg` at compile time.
pub fn target_key() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-linux-gnu",
        ("aarch64", "linux") => "aarch64-linux-gnu",
        ("x86_64", "windows") => "x86_64-windows-msvc",
        ("aarch64", "macos") => "aarch64-macos",
        _ => "unknown",
    }
}

/// This build's version (`CARGO_PKG_VERSION`, which is the workspace version).
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is valid semver")
}

/// The manifest URL for `base` (no trailing slash required).
pub fn manifest_url(base: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), MANIFEST_FILE)
}

/// `WATT_START_UPDATE_URL`, or [`DEFAULT_UPDATE_BASE`].
pub fn base_url_from_env() -> String {
    match std::env::var("WATT_START_UPDATE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => DEFAULT_UPDATE_BASE.to_owned(),
    }
}

/// `true` when `WATT_START_NO_UPDATE_CHECK=1`.
pub fn update_check_disabled() -> bool {
    std::env::var("WATT_START_NO_UPDATE_CHECK").ok().as_deref() == Some("1")
}

/// Public keys compiled in from `packaging/update-key.pub` and `update-key-next.pub`.
///
/// Empty when those files were absent at build time. Each entry is the file contents
/// (comment line plus base64, as `minisign` writes it).
pub fn compiled_public_keys() -> Vec<String> {
    #[cfg(watt_update_signed)]
    {
        include_str!(concat!(env!("OUT_DIR"), "/update_keys.txt"))
            .split("\n---\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
    #[cfg(not(watt_update_signed))]
    Vec::new()
}

/// Whether this build verifies the manifest signature.
pub fn updates_are_signed() -> bool {
    cfg!(watt_update_signed)
}

/// `updates/<version>/<file>` under the launcher's updates directory.
pub fn download_path(updates_dir: &Path, version: &str, asset: &Asset) -> PathBuf {
    updates_dir.join(version).join(asset_file_name(&asset.url))
}

fn asset_file_name(url: &str) -> String {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    let name = bare.rsplit(['/', '\\']).next().unwrap_or("watt-start");
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        "watt-start".to_owned()
    } else {
        cleaned
    }
}

/// A published launcher release. Unknown JSON keys are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: u32,
    /// Always `watt-start` today.
    pub name: String,
    /// Release version. The updater only acts when this is newer than the running build.
    pub version: Version,
    /// ISO-8601 publish time. Display only.
    #[serde(default)]
    pub published: String,
    /// Release notes, shown in the update dialog.
    #[serde(default)]
    pub notes: String,
    /// Release page. Opened by "What's new".
    #[serde(default)]
    pub notes_url: String,
    /// Assets keyed by [`target_key`].
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
}

/// One downloadable file in a [`Manifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// HTTPS URL of the file.
    pub url: String,
    /// Lower or upper case hex sha256 of the file bytes.
    pub sha256: String,
    /// Exact size in bytes. A mismatch rejects the download.
    pub size: u64,
    /// What the file is. Only [`AssetKind::Executable`] is installed automatically.
    pub kind: AssetKind,
}

/// What an [`Asset`] is. Unknown kinds are kept so the UI can offer a manual download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetKind {
    /// A bare executable the updater can rename into place.
    Executable,
    /// Anything else (`tar.gz`, `zip`, `appimage`, or a future kind).
    Other(String),
}

impl Serialize for AssetKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Executable => serializer.serialize_str("executable"),
            Self::Other(kind) => serializer.serialize_str(kind),
        }
    }
}

impl<'de> Deserialize<'de> for AssetKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        if kind == "executable" {
            Ok(Self::Executable)
        } else {
            Ok(Self::Other(kind))
        }
    }
}

/// Which asset applies to a target key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetChoice<'a> {
    /// An executable the updater can install.
    Executable(&'a Asset),
    /// Present, but not an executable. Offer the release page instead.
    Manual(&'a Asset),
    /// This target is not in the manifest.
    Missing,
}

/// Pick the asset for `target`.
pub fn choose_asset<'a>(manifest: &'a Manifest, target: &str) -> AssetChoice<'a> {
    match manifest.assets.get(target) {
        Some(asset) if asset.kind == AssetKind::Executable => AssetChoice::Executable(asset),
        Some(asset) => AssetChoice::Manual(asset),
        None => AssetChoice::Missing,
    }
}

/// Parse a manifest. Unknown keys are ignored. The format number is not checked here.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    serde_json::from_slice(bytes).context("invalid update manifest")
}

/// How this process was installed. Everything except [`InstallKind::ReleaseBinary`] is shown
/// instructions and is not replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// A writable directory we may rename a new executable into.
    ReleaseBinary {
        /// The resolved executable path.
        exe: PathBuf,
    },
    /// `/nix/store/...`.
    Nix,
    /// `$FLATPAK_ID` is set, or the binary lives under `/app`.
    Flatpak,
    /// `cargo install` (`$CARGO_HOME/bin` or `~/.cargo/bin`).
    CargoInstall,
    /// A Scoop app (`\scoop\apps\`).
    Scoop,
    /// `/usr` but not `/usr/local`.
    SystemPackage,
    /// The directory is not writable, and no package manager claimed the path.
    ReadOnly {
        /// The resolved executable path.
        exe: PathBuf,
    },
    /// `debug_assertions`, or the executable sits under a `target/` directory.
    Development,
}

impl InstallKind {
    /// Short label for the About card.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ReleaseBinary { .. } => "Release build",
            Self::Nix => "Nix",
            Self::Flatpak => "Flatpak",
            Self::CargoInstall => "Cargo install",
            Self::Scoop => "Scoop",
            Self::SystemPackage => "System package",
            Self::ReadOnly { .. } => "Read-only directory",
            Self::Development => "Development build",
        }
    }

    /// Whether "Update and restart" may replace the file.
    pub fn can_replace(&self) -> bool {
        matches!(self, Self::ReleaseBinary { .. })
    }

    /// The executable a release install would replace.
    pub fn exe(&self) -> Option<&Path> {
        match self {
            Self::ReleaseBinary { exe } | Self::ReadOnly { exe } => Some(exe),
            _ => None,
        }
    }

    /// What to tell the user when this install cannot replace itself. Empty for a release binary.
    pub fn instructions(&self) -> String {
        match self {
            Self::ReleaseBinary { .. } => String::new(),
            Self::Nix => "This copy comes from Nix. Update it with your Nix profile or flake \
                 (for example: nix profile upgrade)"
                .to_owned(),
            Self::Flatpak => "This copy is a Flatpak. Update it with:\nflatpak update".to_owned(),
            Self::CargoInstall => "Rebuild from your checkout:\n\
                 cargo install --path crates/watt-start"
                .to_owned(),
            Self::Scoop => "This copy was installed with Scoop. Update it with:\n\
                 scoop update watt-start"
                .to_owned(),
            Self::SystemPackage => "This copy comes from your system package manager. \
                 Update watt-start there."
                .to_owned(),
            Self::ReadOnly { .. } => "watt-start cannot replace itself here because the directory \
                 is not writable. Download the new version and replace the file yourself."
                .to_owned(),
            Self::Development => "This is a development build. A newer release exists; \
                 rebuild it from your checkout."
                .to_owned(),
        }
    }
}

/// Inputs for [`detect_install`]. Tests pass these instead of reading the process environment.
#[derive(Clone, Debug, Default)]
pub struct InstallEnv {
    /// `$FLATPAK_ID`, when set and non-empty.
    pub flatpak_id: Option<String>,
    /// `$CARGO_HOME`, when set.
    pub cargo_home: Option<PathBuf>,
    /// `$HOME` or `%USERPROFILE%`.
    pub home: Option<PathBuf>,
    /// `debug_assertions` for the running binary. The `target/` path rule is separate.
    pub development: bool,
    /// Whether the executable's directory accepts a new file.
    pub dir_writable: bool,
}

impl InstallEnv {
    /// The real process environment, for [`detect_current`].
    pub fn from_process(exe: &Path) -> Self {
        Self {
            flatpak_id: std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty()),
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            development: cfg!(debug_assertions),
            dir_writable: exe.parent().is_some_and(dir_is_writable),
        }
    }
}

/// Classify `exe`. `exe` should already be symlink-resolved; this function does not touch
/// the filesystem except through the flags in `env`.
pub fn detect_install(exe: &Path, env: &InstallEnv) -> InstallKind {
    if is_prefixed(exe, "/nix/store") {
        return InstallKind::Nix;
    }
    if env.flatpak_id.is_some() || is_prefixed(exe, "/app") {
        return InstallKind::Flatpak;
    }
    if is_scoop(exe) {
        return InstallKind::Scoop;
    }
    if is_system_package(exe) {
        return InstallKind::SystemPackage;
    }
    if is_cargo_install(exe, env) {
        return InstallKind::CargoInstall;
    }
    if env.development || has_component(exe, "target") {
        return InstallKind::Development;
    }
    if !env.dir_writable {
        return InstallKind::ReadOnly {
            exe: exe.to_path_buf(),
        };
    }
    InstallKind::ReleaseBinary {
        exe: exe.to_path_buf(),
    }
}

/// Resolve `current_exe` (following symlinks) and classify it.
pub fn detect_current() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Development;
    };
    let exe = std::fs::canonicalize(&exe).map_or(exe, strip_verbatim);
    let env = InstallEnv::from_process(&exe);
    detect_install(&exe, &env)
}

/// `canonicalize` on Windows returns `\\?\C:\...`. Drop the prefix for a plain drive path so
/// the path compares against `%CARGO_HOME%` and reads normally in messages. UNC paths stay.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let stripped = path
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .map(PathBuf::from);
    stripped.unwrap_or(path)
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_prefixed(path: &Path, prefix: &str) -> bool {
    let text = slash(path);
    text == prefix || text.starts_with(&format!("{prefix}/"))
}

fn is_system_package(path: &Path) -> bool {
    let text = slash(path);
    let Some(rest) = text.strip_prefix("/usr/") else {
        return false;
    };
    !rest.starts_with("local/") && rest != "local"
}

fn is_scoop(path: &Path) -> bool {
    slash(path).to_ascii_lowercase().contains("/scoop/apps/")
}

fn is_cargo_install(exe: &Path, env: &InstallEnv) -> bool {
    if let Some(cargo_home) = &env.cargo_home
        && is_under(exe, &cargo_home.join("bin"))
    {
        return true;
    }
    if let Some(home) = &env.home
        && is_under(exe, &home.join(".cargo").join("bin"))
    {
        return true;
    }
    false
}

fn is_under(path: &Path, dir: &Path) -> bool {
    let mut path = slash(path);
    let mut dir = slash(dir);
    // Windows paths are case-insensitive. `$CARGO_HOME` and the executable can differ in case.
    if cfg!(windows) {
        path.make_ascii_lowercase();
        dir.make_ascii_lowercase();
    }
    if !dir.ends_with('/') {
        dir.push('/');
    }
    path.starts_with(&dir) || path.trim_end_matches('/') == dir.trim_end_matches('/')
}

fn has_component(path: &Path, name: &str) -> bool {
    slash(path).split('/').any(|part| part == name)
}

fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".watt-start-write-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// `<pwc data>/watt-start/update-check.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckState {
    /// Unix seconds of the last successful check. `0` means never.
    #[serde(default)]
    pub last_checked: u64,
    /// The version string last seen. Informational.
    #[serde(default)]
    pub latest_seen: String,
}

impl CheckState {
    /// Load, or the defaults when the file is missing.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("cannot read {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Write atomically. Callers do this only after a successful check.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_vec_pretty(self).context("cannot serialise the update check")?;
        write_atomic(path, &text)
    }
}

/// Whether a check should hit the network.
pub fn check_is_due(state: &CheckState, now: u64, force: bool) -> bool {
    force
        || state.last_checked == 0
        || now.saturating_sub(state.last_checked) >= CHECK_INTERVAL_SECS
}

/// Arguments for [`check`]. The fetcher is borrowed so tests can pass a fake.
pub struct CheckInput<'a> {
    /// HTTP client.
    pub fetcher: &'a dyn Fetch,
    /// Unix seconds "now".
    pub now: u64,
    /// Previous successful check. Not written by [`check`].
    pub state: &'a CheckState,
    /// Ignore [`CHECK_INTERVAL_SECS`].
    pub force: bool,
    /// The running version.
    pub current: &'a Version,
    /// Asset key, usually [`target_key`].
    pub target: &'a str,
    /// Full manifest URL.
    pub manifest_url: &'a str,
    /// Compiled public keys. Empty means the signature is not required.
    pub keys: &'a [String],
}

/// What [`check`] decided. State is written by the caller, and only for the success variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The last success is still inside the interval.
    Skipped,
    /// The manifest URL returned 404. Silent at startup.
    NoInformation,
    /// The manifest is not newer than the running build.
    UpToDate {
        /// The version the manifest advertised.
        latest: Version,
    },
    /// A newer release. `asset` is set only for an executable this target can install.
    Available {
        /// The parsed manifest.
        manifest: Box<Manifest>,
        /// The executable for this target, when there is one.
        asset: Option<Asset>,
    },
    /// A transport, signature, or parse failure. Startup stays quiet; Settings shows it.
    Failed {
        /// What to show the user.
        message: String,
    },
}

impl CheckOutcome {
    /// The version a successful check observed.
    pub fn observed_version(&self) -> Option<&Version> {
        match self {
            Self::UpToDate { latest } => Some(latest),
            Self::Available { manifest, .. } => Some(&manifest.version),
            _ => None,
        }
    }
}

/// Fetch and interpret the manifest. Does not write the state file and does not download
/// the binary. Only a strictly newer version is [`CheckOutcome::Available`].
pub fn check(input: &CheckInput<'_>) -> CheckOutcome {
    if !check_is_due(input.state, input.now, input.force) {
        return CheckOutcome::Skipped;
    }
    let bytes = match read_url(input.fetcher, input.manifest_url, MAX_MANIFEST) {
        Ok(bytes) => bytes,
        Err(BodyError::NotFound) => return CheckOutcome::NoInformation,
        Err(BodyError::Other(message)) => return CheckOutcome::Failed { message },
    };
    if !input.keys.is_empty() {
        let sig_url = format!("{}.minisig", input.manifest_url);
        let sig = match read_url(input.fetcher, &sig_url, MAX_SIGNATURE) {
            Ok(sig) => sig,
            Err(BodyError::NotFound) => {
                return CheckOutcome::Failed {
                    message: "the update manifest is not signed".to_owned(),
                };
            }
            Err(BodyError::Other(message)) => return CheckOutcome::Failed { message },
        };
        let sig_text = String::from_utf8_lossy(&sig);
        if let Err(e) = verify_manifest(&bytes, Some(&sig_text), input.keys) {
            return CheckOutcome::Failed {
                message: format!("{e:#}"),
            };
        }
    }
    let manifest = match parse_manifest(&bytes) {
        Ok(manifest) => manifest,
        Err(e) => {
            return CheckOutcome::Failed {
                message: format!("cannot read the update manifest: {e:#}"),
            };
        }
    };
    if manifest.format != MANIFEST_FORMAT {
        return CheckOutcome::Failed {
            message: format!("unsupported update manifest format {}", manifest.format),
        };
    }
    if manifest.version > *input.current {
        let asset = match choose_asset(&manifest, input.target) {
            AssetChoice::Executable(asset) => Some(asset.clone()),
            AssetChoice::Manual(_) | AssetChoice::Missing => None,
        };
        CheckOutcome::Available {
            manifest: Box::new(manifest),
            asset,
        }
    } else {
        CheckOutcome::UpToDate {
            latest: manifest.version,
        }
    }
}

/// Remember a successful check. Failures and 404s are not recorded, so the next launch retries.
pub fn record_success(path: &Path, now: u64, latest: &Version) -> Result<()> {
    CheckState {
        last_checked: now,
        latest_seen: latest.to_string(),
    }
    .save(path)
}

enum BodyError {
    NotFound,
    Other(String),
}

fn read_url(fetcher: &dyn Fetch, url: &str, limit: u64) -> Result<Vec<u8>, BodyError> {
    let fetched = fetcher
        .get(url)
        .map_err(|e| BodyError::Other(format!("{e:#}")))?;
    if fetched.status == 404 {
        return Err(BodyError::NotFound);
    }
    if !(200..300).contains(&fetched.status) {
        return Err(BodyError::Other(format!(
            "update server returned HTTP {}",
            fetched.status
        )));
    }
    read_limited(fetched, limit).map_err(|e| BodyError::Other(format!("{e:#}")))
}

fn read_limited(mut fetched: Fetched, limit: u64) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = fetched
            .body
            .read(&mut chunk)
            .context("cannot read the response")?;
        if n == 0 {
            break;
        }
        if buf.len() as u64 + n as u64 > limit {
            bail!("response is larger than {limit} bytes");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

/// Verify `bytes` against a minisign signature.
///
/// An empty `keys` slice succeeds without looking at `signature` (this build has no key).
/// Otherwise the signature must verify against one of the keys.
pub fn verify_manifest(bytes: &[u8], signature: Option<&str>, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let Some(signature) = signature.map(str::trim).filter(|s| !s.is_empty()) else {
        bail!("the update manifest is not signed");
    };
    let signature = minisign_verify::Signature::decode(signature)
        .context("the update manifest signature is unreadable")?;
    let mut saw_key = false;
    for key in keys {
        let Ok(public_key) = parse_public_key(key) else {
            continue;
        };
        saw_key = true;
        if public_key.verify(bytes, &signature, false).is_ok() {
            return Ok(());
        }
    }
    if !saw_key {
        bail!("the compiled update key is unreadable");
    }
    bail!("the update manifest signature is not valid");
}

fn parse_public_key(text: &str) -> Result<minisign_verify::PublicKey, minisign_verify::Error> {
    let trimmed = text.trim();
    if trimmed.contains('\n') {
        minisign_verify::PublicKey::decode(trimmed)
    } else {
        minisign_verify::PublicKey::from_base64(trimmed)
    }
}

/// Download `asset` to `dest`, checking the size and sha256 before the file is published.
///
/// `progress` is called with `(bytes_written, Some(asset.size))` as the body arrives.
/// A mismatch deletes the partial file and returns an error. `dest`'s parent is created.
pub fn download_asset(
    fetcher: &dyn Fetch,
    asset: &Asset,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    if asset.kind != AssetKind::Executable {
        bail!("this asset is not an executable watt-start can install");
    }
    if asset.size > MAX_BINARY {
        bail!("update is larger than {MAX_BINARY} bytes");
    }
    let expected = parse_sha256(&asset.sha256)?;
    let mut fetched = fetcher
        .get(&asset.url)
        .with_context(|| format!("cannot download {}", asset.url))?;
    if fetched.status == 404 {
        bail!("update file was not found");
    }
    if !(200..300).contains(&fetched.status) {
        bail!("update server returned HTTP {}", fetched.status);
    }
    let parent = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let partial = parent.join(format!(
        ".{}.{}.partial",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let download = (|| -> Result<()> {
        let mut file = std::fs::File::create(&partial)
            .with_context(|| format!("cannot write {}", partial.display()))?;
        let mut hasher = sha2::Sha256::new();
        let mut downloaded = 0u64;
        let mut chunk = [0u8; 64 * 1024];
        let total = Some(asset.size);
        loop {
            let n = fetched
                .body
                .read(&mut chunk)
                .context("cannot read the update")?;
            if n == 0 {
                break;
            }
            downloaded += n as u64;
            if downloaded > asset.size || downloaded > MAX_BINARY {
                bail!("update size does not match the manifest");
            }
            hasher.update(&chunk[..n]);
            file.write_all(&chunk[..n])
                .with_context(|| format!("cannot write {}", partial.display()))?;
            progress(downloaded, total);
        }
        file.flush().context("cannot finish the update download")?;
        drop(file);
        if downloaded != asset.size {
            bail!(
                "update size does not match the manifest (expected {} bytes, got {downloaded})",
                asset.size
            );
        }
        let digest = hasher.finalize();
        if digest.as_slice() != expected {
            bail!("update sha256 does not match the manifest");
        }
        if dest.exists() {
            remove_file_retry(dest)
                .with_context(|| format!("cannot replace {}", dest.display()))?;
        }
        std::fs::rename(&partial, dest)
            .with_context(|| format!("cannot write {}", dest.display()))?;
        Ok(())
    })();
    if download.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    download
}

fn parse_sha256(text: &str) -> Result<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("manifest sha256 is not 64 hex characters");
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .context("manifest sha256 is not 64 hex characters")?;
    }
    Ok(out)
}

/// Copy `new_file` next to `exe` and replace `exe`, keeping the previous binary as `<stem>-prev`.
///
/// The copy lands in the executable's directory first so the rename stays on one filesystem.
/// On Windows a locked `-prev` is retried, then written as `-prev-<pid>` instead. If the final
/// rename fails, the previous binary is moved back.
pub fn install_executable(new_file: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .context("the executable path has no directory")?;
    let tmp = dir.join(format!(".watt-start-update-{}", std::process::id()));
    if tmp.exists() {
        remove_file_retry(&tmp).with_context(|| format!("cannot replace {}", tmp.display()))?;
    }
    std::fs::copy(new_file, &tmp).with_context(|| format!("cannot stage {}", tmp.display()))?;
    make_executable(&tmp)?;
    if let Err(e) = replace_staged(&tmp, exe) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn replace_staged(tmp: &Path, exe: &Path) -> Result<()> {
    let mut prev = sibling_tag(exe, "prev");
    if prev.exists() && remove_file_retry(&prev).is_err() {
        prev = sibling_tag(exe, &format!("prev-{}", std::process::id()));
        if prev.exists() {
            remove_file_retry(&prev)
                .with_context(|| format!("cannot replace {}", prev.display()))?;
        }
    }
    if exe.exists() {
        std::fs::rename(exe, &prev)
            .with_context(|| format!("cannot move {} aside", exe.display()))?;
    }
    if let Err(e) = std::fs::rename(tmp, exe) {
        if prev.exists()
            && !exe.exists()
            && let Err(rollback) = std::fs::rename(&prev, exe)
        {
            bail!(
                "cannot install the update ({e:#}), and restoring the previous binary failed ({rollback})"
            );
        }
        return Err(e).context("cannot install the updated executable");
    }
    Ok(())
}

fn sibling_tag(exe: &Path, tag: &str) -> PathBuf {
    let stem = exe.file_stem().unwrap_or_default().to_string_lossy();
    let name = match exe.extension() {
        Some(ext) => format!("{stem}-{tag}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{tag}"),
    };
    exe.with_file_name(name)
}

fn remove_file_retry(path: &Path) -> std::io::Result<()> {
    let attempts = if cfg!(windows) { 8 } else { 1 };
    let mut last = None;
    for attempt in 0..attempts {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt + 1 < attempts {
                    std::thread::sleep(Duration::from_millis(50 * (attempt as u64 + 1)));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing")))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("cannot mark {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Start `exe` detached, with no arguments and `WATT_START_UPDATED_FROM=<from_version>`.
///
/// Capture `exe` before [`install_executable`]: on Linux `current_exe` afterwards follows the
/// renamed `-prev` file. The caller then closes this process.
pub fn restart(exe: &Path, from_version: &str) -> Result<()> {
    let mut command = std::process::Command::new(exe);
    command
        .env("WATT_START_UPDATED_FROM", from_version)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .with_context(|| format!("cannot restart {}", exe.display()))?;
    Ok(())
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid() is async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// `YYYY-MM-DD HH:MM UTC` for a unix timestamp. Stable, so the About card does not need chrono.
pub fn format_unix_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// Howard Hinnant's `civil_from_days`. `days` is days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

/// Where a launcher self-update check or install has got to. The GUI reads this every frame.
#[derive(Clone, Debug)]
pub struct UpdateStatus {
    /// Current step.
    pub phase: Phase,
    /// Running version (`CARGO_PKG_VERSION`).
    pub current_version: String,
    /// How this process was installed.
    pub install: InstallKind,
    /// Newest version seen, when a check has succeeded.
    pub latest_version: Option<String>,
    /// Release notes of `latest_version`.
    pub notes: String,
    /// Release page.
    pub notes_url: String,
    /// Executable asset for this target, when the manifest had one.
    pub asset: Option<Asset>,
    /// `asset` is an executable and [`InstallKind::can_replace`] is true.
    pub can_apply: bool,
    /// Download fraction, `0.0..=1.0`.
    pub progress: f32,
    /// Bytes written for the current download.
    pub downloaded: u64,
    /// Expected size, when known.
    pub total: Option<u64>,
    /// Last check or install error. Startup does not toast this.
    pub error: Option<String>,
    /// The error came from installing, so Retry downloads again instead of rechecking.
    pub apply_failed: bool,
    /// Unix seconds of the last successful check.
    pub last_checked: Option<u64>,
    /// A minisign public key was compiled into this build.
    pub signed_build: bool,
    /// Set after a successful replace. The GUI spawns [`restart`] once and closes.
    pub restart: Option<RestartPlan>,
}

/// One step of [`UpdateStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No check yet this session (or a check was skipped as fresh).
    Idle,
    /// A check is in flight.
    Checking,
    /// The running build is current.
    UpToDate,
    /// A newer release was found.
    Available,
    /// The executable is downloading.
    Downloading,
    /// The manifest URL returned 404.
    NoInformation,
    /// The last check or install failed. See [`UpdateStatus::error`].
    Failed,
}

/// What the GUI needs to spawn the updated process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartPlan {
    /// Path captured before the replace.
    pub exe: PathBuf,
    /// The version that was just replaced. Passed as `WATT_START_UPDATED_FROM`.
    pub from_version: String,
}

impl UpdateStatus {
    /// A status for `install`, before any check. Does not read the process environment.
    pub fn with_install(install: InstallKind) -> Self {
        Self {
            phase: Phase::Idle,
            current_version: current_version().to_string(),
            install,
            latest_version: None,
            notes: String::new(),
            notes_url: String::new(),
            asset: None,
            can_apply: false,
            progress: 0.0,
            downloaded: 0,
            total: None,
            error: None,
            apply_failed: false,
            last_checked: None,
            signed_build: updates_are_signed(),
            restart: None,
        }
    }

    /// The running install, before any check.
    pub fn current() -> Self {
        Self::with_install(detect_current())
    }

    /// Fill this status from a check outcome. `install` is left as it was.
    pub fn apply_outcome(&mut self, outcome: &CheckOutcome, now: u64) {
        self.progress = 0.0;
        self.downloaded = 0;
        self.total = None;
        self.apply_failed = false;
        self.restart = None;
        self.error = None;
        match outcome {
            CheckOutcome::Skipped => {}
            CheckOutcome::NoInformation => {
                self.phase = Phase::NoInformation;
            }
            CheckOutcome::Failed { message } => {
                self.phase = Phase::Failed;
                self.error = Some(message.clone());
            }
            CheckOutcome::UpToDate { latest } => {
                self.phase = Phase::UpToDate;
                self.latest_version = Some(latest.to_string());
                self.notes.clear();
                self.notes_url.clear();
                self.asset = None;
                self.can_apply = false;
                self.last_checked = Some(now);
            }
            CheckOutcome::Available { manifest, asset } => {
                self.phase = Phase::Available;
                self.latest_version = Some(manifest.version.to_string());
                self.notes.clone_from(&manifest.notes);
                self.notes_url.clone_from(&manifest.notes_url);
                self.asset.clone_from(asset);
                self.can_apply = asset.is_some() && self.install.can_replace();
                self.last_checked = Some(now);
            }
        }
    }
}

/// Run a check against the public manifest and, on success, write `state_path`.
pub fn check_and_record(paths_state: &Path, force: bool) -> UpdateStatus {
    let mut status = UpdateStatus::current();
    if update_check_disabled() {
        return status;
    }
    let state = CheckState::load(paths_state).unwrap_or_default();
    if state.last_checked > 0 {
        status.last_checked = Some(state.last_checked);
    }
    let fetcher = crate::http::UreqFetcher::new();
    let current = current_version();
    let url = manifest_url(&base_url_from_env());
    let keys = compiled_public_keys();
    let now = now_secs();
    let outcome = check(&CheckInput {
        fetcher: &fetcher,
        now,
        state: &state,
        force,
        current: &current,
        target: target_key(),
        manifest_url: &url,
        keys: &keys,
    });
    if let Some(latest) = outcome.observed_version() {
        let _ = record_success(paths_state, now, latest);
    }
    status.apply_outcome(&outcome, now);
    status
}

/// Download `asset` and replace `exe`. `exe` must be the path captured before this call.
pub fn download_and_install(
    updates_dir: &Path,
    asset: &Asset,
    version: &str,
    exe: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let dest = download_path(updates_dir, version, asset);
    let fetcher = crate::http::UreqFetcher::new();
    download_asset(&fetcher, asset, &dest, progress)?;
    install_executable(&dest, exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::Mutex;

    struct MapFetch {
        files: HashMap<String, (u16, Vec<u8>)>,
        hits: Mutex<Vec<String>>,
    }

    impl MapFetch {
        fn new(files: HashMap<String, (u16, Vec<u8>)>) -> Self {
            Self {
                files,
                hits: Mutex::new(Vec::new()),
            }
        }

        fn hits(&self) -> Vec<String> {
            self.hits.lock().expect("hits").clone()
        }
    }

    impl Fetch for MapFetch {
        fn get(&self, url: &str) -> Result<Fetched> {
            self.hits.lock().expect("hits").push(url.to_owned());
            let (status, bytes) = self.files.get(url).cloned().unwrap_or((404, Vec::new()));
            Ok(Fetched {
                status,
                content_length: Some(bytes.len() as u64),
                body: Box::new(Cursor::new(bytes)),
            })
        }
    }

    fn ver(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn asset(url: &str, bytes: &[u8], kind: &str) -> serde_json::Value {
        serde_json::json!({
            "url": url,
            "sha256": sha_hex(bytes),
            "size": bytes.len(),
            "kind": kind,
            "future_asset": true,
        })
    }

    fn manifest_bytes(version: &str, assets: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "name": "watt-start",
            "version": version,
            "published": "2026-10-20T12:00:00Z",
            "notes": "One-click launcher updates.",
            "notes_url": "https://example.com/notes",
            "future": {"ignored": true},
            "assets": assets,
        }))
        .unwrap()
    }

    fn sign_bytes(data: &[u8]) -> (String, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature =
            minisign::sign(Some(&kp.pk), &kp.sk, Cursor::new(data), None, None).unwrap();
        let public = kp.pk.to_box().unwrap().into_string();
        (public, signature.to_string())
    }

    fn input<'a>(
        fetcher: &'a dyn Fetch,
        state: &'a CheckState,
        current: &'a Version,
        url: &'a str,
        keys: &'a [String],
        force: bool,
        now: u64,
    ) -> CheckInput<'a> {
        CheckInput {
            fetcher,
            now,
            state,
            force,
            current,
            target: "test-target",
            manifest_url: url,
            keys,
        }
    }

    #[test]
    fn manifest_ignores_unknown_keys_and_unknown_kinds() {
        let body = b"bin";
        let bytes = manifest_bytes(
            "0.2.0",
            serde_json::json!({
                "test-target": asset("https://example.com/watt", body, "executable"),
                "other": asset("https://example.com/app.AppImage", b"img", "appimage"),
            }),
        );
        let manifest = parse_manifest(&bytes).unwrap();
        assert_eq!(manifest.version, ver("0.2.0"));
        assert_eq!(manifest.notes, "One-click launcher updates.");
        assert!(matches!(
            choose_asset(&manifest, "test-target"),
            AssetChoice::Executable(_)
        ));
        assert!(matches!(
            choose_asset(&manifest, "other"),
            AssetChoice::Manual(_)
        ));
        assert!(matches!(
            choose_asset(&manifest, "missing"),
            AssetChoice::Missing
        ));
        let executable = manifest.assets.get("test-target").unwrap();
        assert_eq!(executable.size, 3);
        assert_eq!(executable.kind, AssetKind::Executable);
    }

    #[test]
    fn only_a_newer_version_is_an_update() {
        let url = "https://example.com/watt-start-update.json";
        let body = b"new";
        let cases = [("0.2.0", true), ("0.1.0", false), ("0.0.9", false)];
        for (version, newer) in cases {
            let bytes = manifest_bytes(
                version,
                serde_json::json!({
                    "test-target": asset("https://example.com/watt", body, "executable"),
                }),
            );
            let fetch = MapFetch::new(HashMap::from([(url.to_owned(), (200, bytes))]));
            let state = CheckState::default();
            let current = ver("0.1.0");
            let outcome = check(&input(&fetch, &state, &current, url, &[], true, 100));
            if newer {
                match outcome {
                    CheckOutcome::Available { manifest, asset } => {
                        assert_eq!(manifest.version, ver(version));
                        assert!(asset.is_some());
                    }
                    other => panic!("expected available, got {other:?}"),
                }
            } else {
                assert!(
                    matches!(outcome, CheckOutcome::UpToDate { .. }),
                    "{version} -> {outcome:?}"
                );
            }
        }
    }

    #[test]
    fn fresh_check_does_not_touch_the_network_unless_forced() {
        let url = "https://example.com/watt-start-update.json";
        let fetch = MapFetch::new(HashMap::new());
        let state = CheckState {
            last_checked: 1_000,
            latest_seen: "0.1.0".into(),
        };
        let current = ver("0.1.0");
        let skipped = check(&input(
            &fetch,
            &state,
            &current,
            url,
            &[],
            false,
            1_000 + 60,
        ));
        assert_eq!(skipped, CheckOutcome::Skipped);
        assert!(fetch.hits().is_empty());
        let due = check(&input(
            &fetch,
            &state,
            &current,
            url,
            &[],
            false,
            1_000 + CHECK_INTERVAL_SECS,
        ));
        assert!(matches!(due, CheckOutcome::NoInformation));
        let forced = check(&input(&fetch, &state, &current, url, &[], true, 1_001));
        assert!(matches!(forced, CheckOutcome::NoInformation));
        assert_eq!(fetch.hits().len(), 2);
    }

    #[test]
    fn missing_manifest_is_no_information_and_other_http_errors_fail() {
        let url = "https://example.com/watt-start-update.json";
        let current = ver("0.1.0");
        let state = CheckState::default();
        let missing = MapFetch::new(HashMap::new());
        assert_eq!(
            check(&input(&missing, &state, &current, url, &[], true, 1)),
            CheckOutcome::NoInformation
        );
        let broken = MapFetch::new(HashMap::from([(url.to_owned(), (500, b"nope".to_vec()))]));
        match check(&input(&broken, &state, &current, url, &[], true, 1)) {
            CheckOutcome::Failed { message } => assert!(message.contains("500"), "{message}"),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn signature_round_trip_and_rejection() {
        let bytes = manifest_bytes("0.2.0", serde_json::json!({}));
        let (public, signature) = sign_bytes(&bytes);
        let keys = vec![public.clone()];
        verify_manifest(&bytes, Some(&signature), &keys).unwrap();
        verify_manifest(&bytes, Some(&signature), &[]).unwrap();

        let mut tampered = bytes.clone();
        tampered[0] ^= 0xff;
        let err = verify_manifest(&tampered, Some(&signature), &keys).unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");

        let (other, _) = sign_bytes(b"unrelated");
        let err =
            verify_manifest(&bytes, Some(&signature), std::slice::from_ref(&other)).unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");

        // Rotation: an old key still accepts a signature from the previous key.
        verify_manifest(&bytes, Some(&signature), &[other, public]).unwrap();

        let err = verify_manifest(&bytes, None, &keys).unwrap_err();
        assert!(err.to_string().contains("not signed"), "{err}");
    }

    #[test]
    fn check_requires_a_signature_when_a_key_is_configured() {
        let url = "https://example.com/watt-start-update.json";
        let bytes = manifest_bytes(
            "0.2.0",
            serde_json::json!({
                "test-target": asset("https://example.com/watt", b"new", "executable"),
            }),
        );
        let (public, signature) = sign_bytes(&bytes);
        let keys = vec![public];
        let current = ver("0.1.0");
        let state = CheckState::default();
        let unsigned = MapFetch::new(HashMap::from([(url.to_owned(), (200, bytes.clone()))]));
        match check(&input(&unsigned, &state, &current, url, &keys, true, 1)) {
            CheckOutcome::Failed { message } => {
                assert!(message.contains("not signed"), "{message}")
            }
            other => panic!("expected a signature failure, got {other:?}"),
        }
        let signed = MapFetch::new(HashMap::from([
            (url.to_owned(), (200, bytes)),
            (format!("{url}.minisig"), (200, signature.into_bytes())),
        ]));
        assert!(matches!(
            check(&input(&signed, &state, &current, url, &keys, true, 1)),
            CheckOutcome::Available { .. }
        ));
    }

    #[test]
    fn unknown_asset_kind_is_available_but_not_installable() {
        let url = "https://example.com/watt-start-update.json";
        let bytes = manifest_bytes(
            "0.3.0",
            serde_json::json!({
                "test-target": asset("https://example.com/w.tar.gz", b"tar", "tar.gz"),
            }),
        );
        let fetch = MapFetch::new(HashMap::from([(url.to_owned(), (200, bytes))]));
        let current = ver("0.1.0");
        let state = CheckState::default();
        match check(&input(&fetch, &state, &current, url, &[], true, 1)) {
            CheckOutcome::Available { asset, .. } => assert!(asset.is_none()),
            other => panic!("expected a manual update, got {other:?}"),
        }
    }

    #[test]
    fn target_key_matches_this_platform() {
        let key = target_key();
        assert!(!key.is_empty());
        match (std::env::consts::ARCH, std::env::consts::OS) {
            ("x86_64", "linux") => assert_eq!(key, "x86_64-linux-gnu"),
            ("aarch64", "linux") => assert_eq!(key, "aarch64-linux-gnu"),
            ("x86_64", "windows") => assert_eq!(key, "x86_64-windows-msvc"),
            ("aarch64", "macos") => assert_eq!(key, "aarch64-macos"),
            _ => assert_eq!(key, "unknown"),
        }
    }

    fn env(development: bool, writable: bool) -> InstallEnv {
        InstallEnv {
            flatpak_id: None,
            cargo_home: None,
            home: Some(PathBuf::from("/home/user")),
            development,
            dir_writable: writable,
        }
    }

    #[test]
    fn install_kind_follows_path_and_env() {
        let release = Path::new("/home/user/bin/watt-start");
        assert_eq!(
            detect_install(release, &env(false, true)),
            InstallKind::ReleaseBinary {
                exe: release.to_path_buf(),
            }
        );
        assert!(matches!(
            detect_install(release, &env(false, false)),
            InstallKind::ReadOnly { .. }
        ));
        assert_eq!(
            detect_install(release, &env(true, true)),
            InstallKind::Development
        );
        assert_eq!(
            detect_install(
                Path::new("/home/user/proj/target/release/watt-start"),
                &env(false, true)
            ),
            InstallKind::Development
        );
        assert_eq!(
            detect_install(Path::new("/nix/store/abc/bin/watt-start"), &env(true, true)),
            InstallKind::Nix
        );
        assert_eq!(
            detect_install(Path::new("/app/bin/watt-start"), &env(false, true)),
            InstallKind::Flatpak
        );
        let mut flatpak = env(false, true);
        flatpak.flatpak_id = Some("io.example.Watt".into());
        assert_eq!(detect_install(release, &flatpak), InstallKind::Flatpak);
        assert_eq!(
            detect_install(
                Path::new("/home/user/.cargo/bin/watt-start"),
                &env(false, true)
            ),
            InstallKind::CargoInstall
        );
        let mut cargo_home = env(false, true);
        cargo_home.cargo_home = Some(PathBuf::from("/opt/cargo"));
        cargo_home.home = None;
        assert_eq!(
            detect_install(Path::new("/opt/cargo/bin/watt-start"), &cargo_home),
            InstallKind::CargoInstall
        );
        assert_eq!(
            detect_install(Path::new("/usr/bin/watt-start"), &env(false, true)),
            InstallKind::SystemPackage
        );
        assert_eq!(
            detect_install(Path::new("/usr/local/bin/watt-start"), &env(false, true)),
            InstallKind::ReleaseBinary {
                exe: PathBuf::from("/usr/local/bin/watt-start"),
            }
        );
        let scoop = Path::new(r"C:\Users\a\scoop\apps\watt-start\current\watt-start.exe");
        assert_eq!(detect_install(scoop, &env(false, true)), InstallKind::Scoop);
        let cargo = detect_install(
            Path::new("/home/user/.cargo/bin/watt-start"),
            &env(false, true),
        );
        assert!(
            cargo
                .instructions()
                .contains("cargo install --path crates/watt-start"),
            "{}",
            cargo.instructions()
        );
        assert!(!InstallKind::Nix.can_replace());
        assert!(detect_install(release, &env(false, true)).can_replace());
    }

    #[test]
    fn download_rejects_a_bad_hash_and_a_bad_size() {
        let tmp = tempfile::tempdir().unwrap();
        let good = b"hello watt";
        let bad_hash = Asset {
            url: "https://example.com/watt".into(),
            sha256: sha_hex(b"other"),
            size: good.len() as u64,
            kind: AssetKind::Executable,
        };
        let fetch = MapFetch::new(HashMap::from([(
            bad_hash.url.clone(),
            (200, good.to_vec()),
        )]));
        let dest = tmp.path().join("watt-start");
        let err = download_asset(&fetch, &bad_hash, &dest, &mut |_, _| ()).unwrap_err();
        assert!(err.to_string().contains("sha256"), "{err}");
        assert!(!dest.exists());

        let bad_size = Asset {
            sha256: sha_hex(good),
            size: 4,
            ..bad_hash.clone()
        };
        let err = download_asset(&fetch, &bad_size, &dest, &mut |_, _| ()).unwrap_err();
        assert!(err.to_string().contains("size"), "{err}");
        assert!(!dest.exists());

        let mut seen = 0u64;
        download_asset(
            &fetch,
            &Asset {
                size: good.len() as u64,
                ..bad_size
            },
            &dest,
            &mut |n, total| {
                seen = n;
                assert_eq!(total, Some(good.len() as u64));
            },
        )
        .unwrap();
        assert_eq!(seen, good.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), good);
    }

    #[test]
    fn replace_keeps_prev_and_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("watt-start");
        let new_file = tmp.path().join("new");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new_file, b"new").unwrap();
        install_executable(&new_file, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(
            std::fs::read(tmp.path().join("watt-start-prev")).unwrap(),
            b"old"
        );
        assert!(
            !tmp.path()
                .join(format!(".watt-start-update-{}", std::process::id()))
                .exists()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }

        // A missing staged file fails the second rename; the original bytes come back.
        std::fs::write(&exe, b"current").unwrap();
        let missing = tmp.path().join("missing-stage");
        let err = replace_staged(&missing, &exe).unwrap_err();
        assert!(err.to_string().contains("cannot install"), "{err}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"current");
    }

    #[test]
    fn prev_falls_back_when_it_cannot_be_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("watt-start");
        let new_file = tmp.path().join("new");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new_file, b"new").unwrap();
        // A directory cannot be remove_file'd, so the updater picks -prev-<pid>.
        std::fs::create_dir(tmp.path().join("watt-start-prev")).unwrap();
        install_executable(&new_file, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let fallback = tmp
            .path()
            .join(format!("watt-start-prev-{}", std::process::id()));
        assert_eq!(std::fs::read(fallback).unwrap(), b"old");
        assert!(tmp.path().join("watt-start-prev").is_dir());
    }

    #[test]
    fn check_state_round_trip_and_calendar() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("update-check.json");
        assert_eq!(CheckState::load(&path).unwrap(), CheckState::default());
        record_success(&path, 1_700_000_000, &ver("0.2.0")).unwrap();
        let loaded = CheckState::load(&path).unwrap();
        assert_eq!(loaded.last_checked, 1_700_000_000);
        assert_eq!(loaded.latest_seen, "0.2.0");
        std::fs::write(
            &path,
            r#"{"last_checked": 5, "latest_seen": "0.1.0", "extra": 1}"#,
        )
        .unwrap();
        let loaded = CheckState::load(&path).unwrap();
        assert_eq!(loaded.latest_seen, "0.1.0");
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_unix_utc(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn verbatim_drive_prefix_is_dropped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\a\watt-start.exe")),
            PathBuf::from(r"C:\Users\a\watt-start.exe")
        );
        let unc = PathBuf::from(r"\\?\UNC\server\share\watt-start.exe");
        assert_eq!(strip_verbatim(unc.clone()), unc);
        let plain = PathBuf::from("/home/user/bin/watt-start");
        assert_eq!(strip_verbatim(plain.clone()), plain);
    }

    #[test]
    fn manifest_url_trims_a_trailing_slash() {
        assert_eq!(
            manifest_url("https://example.com/updates/"),
            "https://example.com/updates/watt-start-update.json"
        );
    }
}
