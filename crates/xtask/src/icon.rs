//! The window icon, captured from `forge::app_icon_rgba`.
//!
//! `watt-start` is a binary crate and this package must not link the GUI, wgpu
//! or the package manager just to hash a release. `packaging/watt-start.png`
//! is that function at 256×256. Regenerating it is described in
//! `docs/releasing.md`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// RGBA PNG, 256×256, produced by `watt_start`'s `forge::app_icon_rgba(256)`.
pub const PNG: &[u8] = include_bytes!("../../../packaging/watt-start.png");

/// Write the captured icon to `out`, creating parent directories.
pub fn write_icon(out: &Path) -> Result<()> {
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(out, PNG).with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_icon_is_a_256_rgba_png() {
        assert!(PNG.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&PNG[12..16], b"IHDR");
        let width = u32::from_be_bytes(PNG[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(PNG[20..24].try_into().unwrap());
        assert_eq!((width, height), (256, 256));
        assert_eq!(PNG[24], 8, "bit depth");
        assert_eq!(PNG[25], 6, "RGBA");
    }
}
