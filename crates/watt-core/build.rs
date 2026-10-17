//! Compile the minisign public key into watt-core when `packaging/update-key.pub` exists.
//!
//! A second key at `packaging/update-key-next.pub` is accepted for rotation. Neither file is
//! required: without them the updater relies on HTTPS and the manifest's sha256.

use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(watt_update_signed)");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let packaging = manifest_dir.join("../../packaging");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // Watch the directory, not the key files: a missing file would rerun this script (and
    // rebuild watt-core) on every build. A directory is rescanned when anything in it changes.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", packaging.display());
    let mut combined = String::new();
    let mut any = false;
    for name in ["update-key.pub", "update-key-next.pub"] {
        let path = packaging.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if any {
            combined.push_str("\n---\n");
        }
        combined.push_str(text.trim());
        combined.push('\n');
        any = true;
    }
    let dest = out_dir.join("update_keys.txt");
    std::fs::write(&dest, combined).expect("cannot write update_keys.txt");
    if any {
        println!("cargo:rustc-cfg=watt_update_signed");
    }
}
