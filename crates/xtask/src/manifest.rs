//! Update manifest format 1.
//!
//! The launcher fetches `watt-start-update.json` from the latest GitHub release
//! and replaces itself only from assets with `"kind": "executable"`. Archives
//! (`tar.gz`, `zip`) stay out of the manifest. Unknown JSON keys are ignored by
//! the launcher, so this writer keeps the field set small and stable: the
//! minisign signature covers these exact bytes.
//!
//! Target keys are the ones the launcher computes from `cfg!`:
//! `x86_64-linux-gnu`, `aarch64-linux-gnu`, `x86_64-windows-msvc`,
//! `aarch64-macos`. A key is omitted when its file is not in the dist directory.
//! macOS is reserved; release CI does not build it yet. The filename, if a
//! build appears, is `watt-start-<version>-aarch64-macos` with no extension.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Fixed asset name inside `dist`.
pub const MANIFEST_NAME: &str = "watt-start-update.json";

const REPO: &str = "https://github.com/gusahlg/watt-start";

/// Launcher target keys, in the order they are written.
const TARGETS: &[Target] = &[
    Target {
        key: "x86_64-linux-gnu",
        windows: false,
    },
    Target {
        key: "aarch64-linux-gnu",
        windows: false,
    },
    Target {
        key: "x86_64-windows-msvc",
        windows: true,
    },
    Target {
        key: "aarch64-macos",
        windows: false,
    },
];

struct Target {
    key: &'static str,
    /// Windows assets are named `watt-start-<version>-<key>.exe`.
    windows: bool,
}

/// What [`render`] turns into `watt-start-update.json`.
pub struct Input<'a> {
    /// `0.2.0` or `v0.2.0`. The leading `v` is stripped to match `Cargo.toml`.
    pub version: &'a str,
    /// Directory of release files. Only the bare executables are hashed.
    pub dist: &'a Path,
    /// Release notes. Empty is allowed.
    pub notes: &'a str,
    /// Prefix for asset URLs, no trailing slash required.
    /// Default: `https://github.com/gusahlg/watt-start/releases/download/v<version>`.
    pub base_url: Option<&'a str>,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub published: &'a str,
}

/// Read notes and drop leading and trailing whitespace.
pub fn read_notes(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read notes {}", path.display()))?;
    Ok(text.trim().to_string())
}

/// `v0.2.0` and `0.2.0` both become `0.2.0`.
pub fn normalize_version(version: &str) -> Result<String> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
    {
        bail!("version {version:?} is not a release version");
    }
    Ok(version.to_string())
}

/// Bare executable name the updater downloads for `key`.
pub fn asset_filename(version: &str, key: &str) -> String {
    let windows = TARGETS
        .iter()
        .find(|target| target.key == key)
        .is_some_and(|target| target.windows);
    if windows {
        format!("watt-start-{version}-{key}.exe")
    } else {
        format!("watt-start-{version}-{key}")
    }
}

/// One `kind: executable` entry. Field order is the on-disk order.
#[derive(Serialize)]
struct Asset {
    url: String,
    sha256: String,
    size: u64,
    kind: &'static str,
}

/// Present targets only. Declaration order is the manifest order, which matches
/// the launcher's key list rather than alphabetical order (`serde_json`'s map
/// is a `BTreeMap` unless `preserve_order` is on, and that feature is not
/// enabled workspace-wide).
#[derive(Serialize)]
struct Assets {
    #[serde(rename = "x86_64-linux-gnu", skip_serializing_if = "Option::is_none")]
    x86_64_linux_gnu: Option<Asset>,
    #[serde(rename = "aarch64-linux-gnu", skip_serializing_if = "Option::is_none")]
    aarch64_linux_gnu: Option<Asset>,
    #[serde(
        rename = "x86_64-windows-msvc",
        skip_serializing_if = "Option::is_none"
    )]
    x86_64_windows_msvc: Option<Asset>,
    #[serde(rename = "aarch64-macos", skip_serializing_if = "Option::is_none")]
    aarch64_macos: Option<Asset>,
}

#[derive(Serialize)]
struct Manifest {
    format: u32,
    name: &'static str,
    version: String,
    published: String,
    notes: String,
    notes_url: String,
    assets: Assets,
}

/// Pretty-printed manifest, including the trailing newline.
pub fn render(input: &Input<'_>) -> Result<String> {
    let version = normalize_version(input.version)?;
    if !input.dist.is_dir() {
        bail!("dist directory {} does not exist", input.dist.display());
    }
    let published = input.published.trim();
    if published.is_empty() {
        bail!("published timestamp is empty");
    }

    let base = match input.base_url {
        Some(url) => url.trim().trim_end_matches('/').to_string(),
        None => format!("{REPO}/releases/download/v{version}"),
    };
    if base.is_empty() {
        bail!("base url is empty");
    }

    let mut found = 0u32;
    let mut assets = Assets {
        x86_64_linux_gnu: None,
        aarch64_linux_gnu: None,
        x86_64_windows_msvc: None,
        aarch64_macos: None,
    };
    for target in TARGETS {
        let file_name = asset_filename(&version, target.key);
        let path = input.dist.join(&file_name);
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let asset = Asset {
            url: format!("{base}/{file_name}"),
            sha256: sha256_hex(&bytes),
            size: u64::try_from(bytes.len()).context("file size does not fit in u64")?,
            kind: "executable",
        };
        match target.key {
            "x86_64-linux-gnu" => assets.x86_64_linux_gnu = Some(asset),
            "aarch64-linux-gnu" => assets.aarch64_linux_gnu = Some(asset),
            "x86_64-windows-msvc" => assets.x86_64_windows_msvc = Some(asset),
            "aarch64-macos" => assets.aarch64_macos = Some(asset),
            other => bail!("unknown target key {other}"),
        }
        found += 1;
    }
    if found == 0 {
        bail!(
            "no release executables in {} (looked for watt-start-{version}-<target>[.exe])",
            input.dist.display()
        );
    }

    let notes_url = format!("{REPO}/releases/tag/v{version}");
    let manifest = Manifest {
        format: 1,
        name: "watt-start",
        version,
        published: published.to_string(),
        notes: input.notes.trim().to_string(),
        notes_url,
        assets,
    };
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    Ok(text)
}

/// Write [`MANIFEST_NAME`] into `input.dist` and return its path.
pub fn write(input: &Input<'_>) -> Result<PathBuf> {
    let text = render(input)?;
    let path = input.dist.join(MANIFEST_NAME);
    fs::write(&path, &text).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn published_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_utc(secs)
}

/// Format a unix timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_unix_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = tod / 3_600;
    let min = (tod % 3_600) / 60;
    let sec = tod % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year_of_era = i64::from(yoe as u32);
    let y = year_of_era + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month as u32, day as u32)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(Sha256::digest(bytes).as_slice())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) {
        fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn unix_dates_match_the_civil_calendar() {
        let cases = [
            (0, "1970-01-01T00:00:00Z"),
            (946_684_799, "1999-12-31T23:59:59Z"),
            (946_684_800, "2000-01-01T00:00:00Z"),
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            (1_792_497_600, "2026-10-20T12:00:00Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"),
        ];
        for (secs, want) in cases {
            assert_eq!(format_unix_utc(secs), want, "secs {secs}");
        }
    }

    #[test]
    fn manifest_hashes_executables_and_skips_archives() {
        let dir = tempfile::tempdir().unwrap();
        let linux = b"linux-binary";
        let windows = b"windows-binary";
        let arm = b"arm-binary";
        write_file(dir.path(), "watt-start-0.2.0-x86_64-linux-gnu", linux);
        write_file(
            dir.path(),
            "watt-start-0.2.0-x86_64-linux-gnu.tar.gz",
            b"not-the-executable",
        );
        write_file(
            dir.path(),
            "watt-start-0.2.0-x86_64-windows-msvc.exe",
            windows,
        );
        write_file(
            dir.path(),
            "watt-start-0.2.0-x86_64-windows-msvc.zip",
            b"zip",
        );
        write_file(dir.path(), "watt-start-0.2.0-aarch64-linux-gnu", arm);
        write_file(dir.path(), "SHA256SUMS", b"ignore-me");
        write_file(dir.path(), "install.sh", b"#!/bin/sh\n");

        let text = render(&Input {
            version: "v0.2.0",
            dist: dir.path(),
            notes: "  One-click launcher updates.\n",
            base_url: None,
            published: "2026-10-20T12:00:00Z",
        })
        .unwrap();

        let expected = format!(
            r#"{{
  "format": 1,
  "name": "watt-start",
  "version": "0.2.0",
  "published": "2026-10-20T12:00:00Z",
  "notes": "One-click launcher updates.",
  "notes_url": "https://github.com/gusahlg/watt-start/releases/tag/v0.2.0",
  "assets": {{
    "x86_64-linux-gnu": {{
      "url": "https://github.com/gusahlg/watt-start/releases/download/v0.2.0/watt-start-0.2.0-x86_64-linux-gnu",
      "sha256": "{linux_sha}",
      "size": {linux_size},
      "kind": "executable"
    }},
    "aarch64-linux-gnu": {{
      "url": "https://github.com/gusahlg/watt-start/releases/download/v0.2.0/watt-start-0.2.0-aarch64-linux-gnu",
      "sha256": "{arm_sha}",
      "size": {arm_size},
      "kind": "executable"
    }},
    "x86_64-windows-msvc": {{
      "url": "https://github.com/gusahlg/watt-start/releases/download/v0.2.0/watt-start-0.2.0-x86_64-windows-msvc.exe",
      "sha256": "{windows_sha}",
      "size": {windows_size},
      "kind": "executable"
    }}
  }}
}}
"#,
            linux_sha = sha256_hex(linux),
            linux_size = linux.len(),
            arm_sha = sha256_hex(arm),
            arm_size = arm.len(),
            windows_sha = sha256_hex(windows),
            windows_size = windows.len(),
        );
        assert_eq!(text, expected);

        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["format"], 1);
        assert!(value["assets"].get("aarch64-macos").is_none());
        assert!(!text.contains("not-the-executable"));
        assert!(!text.contains("SHA256SUMS"));
    }

    #[test]
    fn base_url_and_macos_key() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "watt-start-0.2.0-aarch64-macos", b"mac");
        let text = render(&Input {
            version: "0.2.0",
            dist: dir.path(),
            notes: "",
            base_url: Some("https://example.test/builds/"),
            published: "2026-10-20T12:00:00Z",
        })
        .unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["notes"], "");
        assert_eq!(
            value["assets"]["aarch64-macos"]["url"],
            "https://example.test/builds/watt-start-0.2.0-aarch64-macos"
        );
        assert_eq!(value["assets"]["aarch64-macos"]["size"], 3);
        assert_eq!(value["assets"]["aarch64-macos"]["kind"], "executable");
        assert_eq!(
            value["assets"]["aarch64-macos"]["sha256"],
            sha256_hex(b"mac")
        );
    }

    #[test]
    fn write_places_the_manifest_in_dist() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "watt-start-1.0.0-x86_64-linux-gnu", b"x");
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "\nhello\n").unwrap();
        let path = write(&Input {
            version: "1.0.0",
            dist: dir.path(),
            notes: &read_notes(&notes).unwrap(),
            base_url: None,
            published: "2026-10-20T12:00:00Z",
        })
        .unwrap();
        assert_eq!(path, dir.path().join(MANIFEST_NAME));
        let text = fs::read_to_string(path).unwrap();
        assert!(text.ends_with('\n'));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["notes"], "hello");
        assert_eq!(value["version"], "1.0.0");
    }

    #[test]
    fn missing_directory_and_empty_dist_fail() {
        let missing = Path::new("/tmp/watt-start-xtask-does-not-exist");
        let err = render(&Input {
            version: "0.2.0",
            dist: missing,
            notes: "",
            base_url: None,
            published: "2026-10-20T12:00:00Z",
        })
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "README.md", b"nope");
        let err = render(&Input {
            version: "0.2.0",
            dist: dir.path(),
            notes: "",
            base_url: None,
            published: "2026-10-20T12:00:00Z",
        })
        .unwrap_err();
        assert!(err.to_string().contains("no release executables"), "{err}");
    }

    #[test]
    fn rejects_a_blank_or_pathlike_version() {
        assert!(normalize_version("  ").is_err());
        assert!(normalize_version("v").is_err());
        assert!(normalize_version("../0.2.0").is_err());
        assert_eq!(normalize_version(" v0.2.0 ").unwrap(), "0.2.0");
    }
}
