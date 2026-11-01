# Distribution

How watt-start gets onto a machine, and what we are not doing yet. This is for the people
building it. The install commands users see are in the README.

## The barrier

The hard part of installing Project Watt Cubed is not the launcher's package format. Modules
are Rust crates compiled into the game, so the first launch of an instance is a 15–40 minute
build and it needs a working toolchain: git, rustc, a C++ linker, a Vulkan driver. An
installer that only unpacks `watt-start` leaves that wall where it is. The work below is
ordered so the launcher gets easier to obtain first, and the toolchain and the game build
get easier after that.

## v0.2 (this release)

GitHub Releases, from a `v*` tag, with no extra host:

- Linux: a tarball and `install.sh`, installed with
  `curl -fsSL https://github.com/gusahlg/watt-start/releases/latest/download/install.sh | sh`
  into `~/.local` (or `/usr/local` with `--system`).
- Windows: a zip and `install.ps1`, installed with
  `irm https://github.com/gusahlg/watt-start/releases/latest/download/install.ps1 | iex`
  into `%LOCALAPPDATA%\Programs\watt-start`, plus a Start-menu shortcut. No UAC prompt.
  The executable is marked `asInvoker` for the same reason.
- Bare executables named `watt-start-<version>-<target>[.exe]` for the updater.
- `watt-start-update.json` (format 1) and, once a key exists, its minisign signature, at
  stable `releases/latest/download/` URLs.
- `SHA256SUMS`.
- The Nix flake stays. On NixOS it is the install path that works: the raw binary cannot
  find libwayland or the Vulkan loader there.

Targets shipped now: `x86_64-linux-gnu`, `aarch64-linux-gnu`, `x86_64-windows-msvc`.
The manifest already has a slot for `aarch64-macos`; we do not build it.

Self-update downloads the bare executable, checks the sha256 from the manifest, and checks
the minisign signature when a public key was compiled in from `packaging/update-key.pub`.
No key is in the tree yet, so current builds say updates are not signed and rely on HTTPS
plus that hash. Package-managed installs (Nix store, `cargo install`, a read-only directory)
are told how to update and are never overwritten.

The real accessibility work is still ahead. v0.2 only removes "clone two repositories and
compile the launcher".

## v0.2.x — a launcher-managed toolchain

The next slice is a private toolchain under `<pwc data>/watt-start/toolchain/`, used only
by the launcher's own builds:

- `rustup-init` with a pinned toolchain, into a private `RUSTUP_HOME` and `CARGO_HOME`.
  `PATH` for that toolchain is set on the op processes, not on the user's shell.
- MinGit, portable, on Windows, so "install Git for Windows" is not a prerequisite.
- Visual Studio Build Tools detected with `vswhere`. When they are missing, offer
  `winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools"`.
  That install does need an administrator. The launcher itself still must not.
- On Linux, doctor shows the distro's package command for the linker and the Vulkan loader
  instead of a generic "install a C toolchain".
- doctor and the welcome screen are where this shows up. A machine that cannot build should
  say what is missing and the exact next command.

## v0.3 — prebuilt games, then nicer packages

Compiling the game on the player's machine is the remaining 15–40 minutes. v0.3 moves the
common cases off that path.

Prebuilt game executables live in a separate repository (the working name is
`gusahlg/pwc-builds`), one set per game tag and a nightly, for vanilla, essentials, and
essentials plus the dev toolkit, per target. CI publishes a signed index keyed by
`(lock environment hash, game revision, engine revision, target, profile)`. The environment
hash is machine-independent. Build IDs are not, so they are not the key.

The launcher hooks in before `build::build`'s prepare step. On a hit it downloads the
executable and records a generation that points at it. Assets still come from the snapshot;
only the linked binary is prebuilt.

AGPL: the index entry's revisions, the instance's `pwc.lock` and the public repositories are
the corresponding source. The index should say that in the note next to the URL, not only
in this document.

Packaging that can wait until the binary is worth distributing more widely:

- Inno Setup, per user, no UAC, so self-update can still replace the exe.
- An AppImage for Linux desktops that do not want the shell one-liner.
- winget, and an AUR package.

Flatpak is a no. The sandbox fights `cargo`, `git` and the system Vulkan driver, which is
the whole of what a build does.

## Later

Windows code signing (Azure Trusted Signing). SmartScreen is the biggest Windows barrier
once the zip exists: an unsigned downloaded exe is what the dialog warns about, and
`Unblock-File` in `install.ps1` only clears the Mark of the Web, it does not make the
signature exist.

macOS: an `.app`, MoltenVK, notarization. The manifest key `aarch64-macos` is the reserved
name; the archive format is not chosen.

## Open questions

These need a decision before the work above them is started.

1. **Where the minisign secret lives.** The release workflow expects it in the
   `MINISIGN_SECRET_KEY` Actions secret (and its password in `MINISIGN_PASSWORD`). The
   alternative is to sign the manifest on a machine that holds the key offline and attach
   `watt-start-update.json.minisig` by hand. Offline is safer against a compromised CI token
   or workflow, and slower, and it is easy to forget. CI is what the workflow implements
   today. The key has not been generated.

2. **A separate `pwc-builds` repository** for prebuilt game artifacts, versus attaching
   those artifacts to `project_watt_cubed` releases or to this repository. A separate repo
   keeps game tags and launcher tags from sharing a release page, and it keeps multi-hundred-megabyte
   binaries out of `watt-start`. It is another repository to permission and to sign.

3. **Budget for code signing.** Azure Trusted Signing (or another Authenticode certificate)
   has a yearly cost and an identity check. Until that is paid, Windows installs will hit
   SmartScreen. It is the main reason a careful Windows user will not run the zip.
