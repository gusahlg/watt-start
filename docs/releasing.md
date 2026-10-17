# Releasing watt-start

A release is an annotated git tag `vX.Y.Z` whose `X.Y.Z` is `workspace.package.version` in
the root `Cargo.toml`. Pushing the tag runs [`.github/workflows/release.yml`](../.github/workflows/release.yml).
The workflow refuses the tag when the two versions differ.

```sh
# Cargo.toml already says version = "0.2.0"
git tag -a v0.2.0 -m "watt-start 0.2.0" -m "One-click launcher updates. Windows build."
git push origin v0.2.0
```

The tag message body becomes the release notes and the manifest's `notes` field. If the tag
is lightweight, or the body is empty, the workflow looks for a `CHANGELOG.md` section
headed `## 0.2.0` or `## [0.2.0]` and uses the lines under it until the next `##`. If neither
exists, the notes are `watt-start 0.2.0`.

## What CI publishes

The package manager stays a path dependency (`../pwc-package-manager`). Release CI checks
out `gusahlg/pwc-package-manager` as a sibling of `watt-start` at `PWC_REF` in the workflow
(default `main`). Change that line to pin a revision. The built launcher does not need the
package-manager source at runtime.

Matrix:

| Runner | Target key | Bare executable | Archive |
| --- | --- | --- | --- |
| `ubuntu-22.04` | `x86_64-linux-gnu` | `watt-start-<version>-x86_64-linux-gnu` | `.tar.gz` |
| `ubuntu-22.04-arm` | `aarch64-linux-gnu` | `watt-start-<version>-aarch64-linux-gnu` | `.tar.gz` |
| `windows-latest` | `x86_64-windows-msvc` | `watt-start-<version>-x86_64-windows-msvc.exe` | `.zip` |

`aarch64-macos` is a reserved manifest key. Nothing builds it yet. If a file named
`watt-start-<version>-aarch64-macos` is placed in the dist directory, `xtask manifest`
will pick it up.

Linux tarball layout (one top-level directory):

```text
watt-start/
  watt-start
  watt-start.desktop
  watt-start.png
  install.sh
```

Run as `./install.sh` from the unpacked tarball, the script installs the files next to it and
needs no network. Piped from `curl`, it downloads the latest release tarball for the machine's
CPU. The Windows zip contains `watt-start.exe` at its root.

The publish job then adds:

- `watt-start-update.json` (manifest format 1)
- `watt-start-update.json.minisig` when `MINISIGN_SECRET_KEY` is set
- `SHA256SUMS` (`sha256sum` format, every file except the sums file itself)
- `install.sh` and `install.ps1` under those stable names, which is what the README one-liners download

Archives are not entries in the manifest. The updater only follows `"kind": "executable"`.

## The manifest

`cargo run -p xtask -- manifest --version <v> --dist <dir> [--notes-file <f>] [--base-url <url>]`
writes `<dir>/watt-start-update.json`. `<v>` may be `0.2.0` or `v0.2.0`. Each present bare
executable becomes an asset keyed by its target, with `url`, lowercase hex `sha256`, `size`
and `"kind": "executable"`.

Default asset URL:

```text
https://github.com/gusahlg/watt-start/releases/download/v<version>/<file>
```

`--base-url` replaces that prefix. `notes_url` stays the GitHub tag page. `published` is the
UTC time the command runs, `YYYY-MM-DDTHH:MM:SSZ`. The file is pretty-printed and ends with
a newline. The signature is over those exact bytes, so do not reformat the file after signing.

The launcher fetches:

```text
https://github.com/gusahlg/watt-start/releases/latest/download/watt-start-update.json
https://github.com/gusahlg/watt-start/releases/latest/download/watt-start-update.json.minisig
```

## Signing

minisign, not a new service. The public key is compiled into the launcher from
`packaging/update-key.pub` when that file exists. It is not in the repository yet.
[packaging/update-key.pub.README](../packaging/update-key.pub.README) is the short version
of creating one. Do not commit the secret.

```sh
minisign -G -p packaging/update-key.pub -s watt-start-update.key
```

Store the whole secret-key file, comment lines included, as the Actions secret
`MINISIGN_SECRET_KEY`, and the key's password as `MINISIGN_PASSWORD`. minisign reads the
password from stdin when stdin is not a terminal, so the workflow pipes the password in. It is
never on a command line or in the log. A key generated with `-W` has no password; then leave
`MINISIGN_PASSWORD` unset. The password protects the key file if it leaks from a backup or a
laptop. Inside Actions both secrets are available to the same job, so it adds nothing there.

The publish job runs on `ubuntu-24.04` because 22.04 has no `minisign` package. It writes the
key to a mode-0600 temp file and signs with:

```sh
printf '%s\n' "$MINISIGN_PASSWORD" | minisign -S -s "$keyfile" \
    -m dist/watt-start-update.json \
    -x dist/watt-start-update.json.minisig \
    -t "watt-start <version>"
```

A wrong or missing password fails the job before anything is published. If
`MINISIGN_SECRET_KEY` is unset, the release is still published, the run shows a warning and
the manifest is unsigned. Builds without `packaging/update-key.pub` accept that (HTTPS and the
sha256). Builds with the public key compiled in require a valid signature and refuse the
manifest otherwise. Once a key is committed, keep the secret set.

To check a published signature by hand:

```sh
minisign -V -p packaging/update-key.pub -m watt-start-update.json
```

### Rotation

The launcher keeps room for a second public key. To rotate:

1. Generate a new key pair with `minisign -G`. Keep the old secret until the overlap release is out.
2. Compile **both** public keys into the next release (the second slot is the new key) and
   sign the manifest with the **old** secret, which those already-shipped binaries trust.
3. Ship that release and wait until you are willing to abandon older binaries.
4. Sign the following release with the **new** secret (`MINISIGN_SECRET_KEY` replaced), and
   drop the old public key from the build.

A binary that only has the old key cannot verify a manifest signed only by the new key.
That is the overlap release.

## Rollback

The updater keeps the previous executable next to the new one: `<stem>-prev<ext>`
(`watt-start-prev` on Linux, `watt-start-prev.exe` on Windows), overwritten on the next
successful update. There is no second-previous copy. To go back, quit the launcher and
rename `-prev` over the current executable. A dev or package-managed install never replaces
itself; use that package's own rollback.

## Windows executable resources

`crates/watt-start/build.rs` embeds an icon, version info and an application manifest when
**both** the host and the target are Windows. That is the `windows-latest` release job, which
has `rc.exe` from the Windows SDK. The manifest sets `longPathAware`, per-monitor DPI
awareness and Common Controls 6, and requests `asInvoker` so the process does not trigger UAC.

A Linux `cargo check --target x86_64-pc-windows-msvc` does not run `rc.exe` or MinGW. Linux
builds of a Linux binary do not look for Windows tools. The icon embedded on Windows is
generated from `packaging/watt-start.png` during the build.

## The icon

`packaging/watt-start.png` is `forge::app_icon_rgba(256)` from the `watt-start` binary.
`xtask` cannot link that crate (there is no library target, and the GUI would pull wgpu and
the package manager into the release tool). `cargo run -p xtask -- icon --out <path>` writes
the captured PNG.

To recapture after the cube icon changes, add this as `crates/watt-start/examples/dump_icon.rs`
(it is the same `#[path]` include the snapshot tests use), run it, delete the example:

```rust
#![allow(dead_code)]

#[path = "../src/gui/mod.rs"]
mod gui;

fn main() {
    let out = std::env::args().nth(1).expect("rgba path");
    let size = 256u32;
    let rgba = gui::forge::app_icon_rgba(size);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&rgba);
    std::fs::write(out, bytes).unwrap();
}
```

```sh
cargo run -p watt-start --example dump_icon -- /tmp/watt-icon.rgba
```

Then wrap the raw `width, height, RGBA` (little-endian u32s) as a PNG again. The committed
file is 256×256, 8-bit RGBA.

## CI other than releases

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on branch pushes and pull
requests (not on `v*` tags, which release.yml already builds):

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --examples --locked -- -D warnings`
- `cargo test -p watt-core --locked` and `cargo test -p xtask --locked`

on `ubuntu-22.04` and `windows-latest`. The GUI snapshot tests need a GPU. A separate Linux
job runs `cargo test -p watt-start --test gui_snapshots` under lavapipe
(`mesa-vulkan-drivers`) with `continue-on-error: true`. A failure there does not fail the
workflow.

The release binary links only glibc: Wayland, X11, libxkbcommon and the Vulkan loader are all
dlopen'd at runtime (check with `readelf -d`). Linux compile jobs still install `pkg-config`
and `libwayland-dev`, which `wayland-sys` would need if a dependency ever turned its `dlopen`
feature off; they cost seconds. The runtime libraries and lavapipe are installed only on the
snapshot job.
