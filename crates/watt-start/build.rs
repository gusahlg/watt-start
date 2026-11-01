//! Windows executable resources: icon, version info, and an application manifest.
//!
//! `winresource` runs `rc.exe`. That exists on the `windows-latest` release
//! runner, and not on a Linux host. This script therefore embeds resources only
//! when the host and the target are both Windows. A Linux
//! `cargo check --target x86_64-pc-windows-msvc` still builds the `.ico` and
//! the manifest below, then skips the resource compiler, so the type-check does
//! not need MinGW. A Linux build of a Linux binary returns before any of that
//! and never looks for a Windows tool.
//!
//! The manifest asks for long paths, per-monitor DPI and Common Controls 6, and
//! for `asInvoker` so starting the launcher does not raise a UAC prompt.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon_png = manifest_dir.join("../../packaging/watt-start.png");
    println!("cargo:rerun-if-changed={}", icon_png.display());

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let prepared = prepare(&icon_png).unwrap_or_else(|err| panic!("windows resources: {err}"));

    #[cfg(windows)]
    compile(&prepared).unwrap_or_else(|err| panic!("embed Windows resources: {err}"));

    #[cfg(not(windows))]
    skip_embed(&prepared);
}

struct Prepared {
    ico: Vec<u8>,
    manifest: String,
}

fn prepare(icon_png: &Path) -> Result<Prepared, String> {
    let png = fs::read(icon_png).map_err(|err| format!("read {}: {err}", icon_png.display()))?;
    let (width, height) = png_dimensions(&png)?;
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    Ok(Prepared {
        ico: png_to_ico(&png, width, height)?,
        manifest: app_manifest(&four_part(&version), processor_architecture(&arch)),
    })
}

#[cfg(not(windows))]
fn skip_embed(prepared: &Prepared) {
    eprintln!(
        "watt-start: host is not Windows, so rc.exe is not run ({} byte icon, {} byte manifest). The windows-latest release build embeds them.",
        prepared.ico.len(),
        prepared.manifest.len()
    );
}

#[cfg(windows)]
fn compile(prepared: &Prepared) -> Result<(), String> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|err| err.to_string())?);
    let ico_path = out_dir.join("watt-start.ico");
    fs::write(&ico_path, &prepared.ico)
        .map_err(|err| format!("write {}: {err}", ico_path.display()))?;
    let ico = ico_path
        .to_str()
        .ok_or_else(|| format!("icon path is not utf-8: {}", ico_path.display()))?;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico)
        .set_language(0x0409)
        .set_manifest(&prepared.manifest)
        .set("FileDescription", "Project Watt Cubed launcher")
        .set("ProductName", "watt-start")
        .set("OriginalFilename", "watt-start.exe")
        .set("InternalName", "watt-start")
        .set("CompanyName", "Project Watt Cubed contributors")
        .set("LegalCopyright", "AGPL-3.0-or-later")
        .set(
            "Comments",
            "Instances, modules, one-click updates and builds for Project Watt Cubed",
        );
    res.compile().map_err(|err| format!("winresource: {err}"))
}

fn png_dimensions(png: &[u8]) -> Result<(u32, u32), String> {
    if png.len() < 26 || png[..8] != *b"\x89PNG\r\n\x1a\n" {
        return Err("packaging/watt-start.png is not a PNG".to_string());
    }
    if png[12..16] != *b"IHDR" {
        return Err("packaging/watt-start.png has no IHDR".to_string());
    }
    let mut size = [0u8; 4];
    size.copy_from_slice(&png[16..20]);
    let width = u32::from_be_bytes(size);
    size.copy_from_slice(&png[20..24]);
    let height = u32::from_be_bytes(size);
    if width == 0 || height == 0 || width > 256 || height > 256 {
        return Err(format!(
            "icon is {width}x{height}; an .ico directory entry holds at most 256"
        ));
    }
    if png[24] != 8 || png[25] != 6 {
        return Err("icon PNG must be 8-bit RGBA".to_string());
    }
    Ok((width, height))
}

fn png_to_ico(png: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let png_len =
        u32::try_from(png.len()).map_err(|_| "icon PNG does not fit in a u32".to_string())?;
    let mut out = Vec::with_capacity(22 + png.len());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(ico_dim(width));
    out.push(ico_dim(height));
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&png_len.to_le_bytes());
    // ICONDIR (6) + one ICONDIRENTRY (16).
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(png);
    Ok(out)
}

/// ICO dimensions are a byte. 0 means 256.
fn ico_dim(pixels: u32) -> u8 {
    u8::try_from(pixels).unwrap_or(0)
}

/// `0.2.0` and `0.2.0-rc.1` become `0.2.0.0`, which is what the Windows
/// manifest and version resource want.
fn four_part(version: &str) -> String {
    let numeric = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts: Vec<&str> = numeric.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    parts.truncate(4);
    parts.join(".")
}

fn processor_architecture(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "*",
    }
}

fn app_manifest(version4: &str, arch: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="gusahlg.watt-start" version="{version4}" processorArchitecture="{arch}"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
</assembly>
"#
    )
}
