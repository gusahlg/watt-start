# watt-start

The launcher for [Project Watt Cubed](https://github.com/gusahlg/project_watt_cubed). It keeps
several game **instances** side by side, each with its own game version and mods, updates them with
one click and recompiles them when needed, so you never have to think about `cargo` or `pwc`.

PWC mods are Rust code compiled into the game, so every instance is its own build. watt-start
is a graphical front end over the [PWC package manager](https://github.com/gusahlg/pwc-package-manager)'s
libraries and shares its state: instances made with `pwc` show up here, and the other way round.

## What it does

- **Instances**: name them, colour them, group them, mark favourites. Each shows its game version,
  branch, engine revision, mods and whether it is ready to play.
- **Update**: one button fetches the newest game, voxel engine and mods together, shows what
  changed, backs up your worlds when the game itself changes, and recompiles.
- **Mods**: browse the available mods, stage additions and removals, then apply. The instance is
  recompiled automatically (you can turn that off in Settings).
- **Builds**: recompile on demand, roll back to an earlier build that worked, or play the last
  working build while a new one is broken.
- **The Forge**: while an update or a build runs, an animated voxel cube assembles itself with
  the progress, with the current step, the crates being compiled and an estimate. Cancel at any
  time.
- **Sources**: follow a branch of the game (`main` by default, or any other), build your own
  checkout of the game (for developers), or use what `pwc` is configured with.

## Install

You need git, a Vulkan-capable GPU driver, and Rust 1.95 or newer (or Nix, on Linux). The game,
its engine and the package manager are downloaded by the launcher itself. Linux is the tested
platform. Windows 10/11 builds, but has not been run on a Windows machine yet (see
[ARCHITECTURE.md](ARCHITECTURE.md)).

```sh
git clone https://github.com/gusahlg/pwc-package-manager
git clone https://github.com/gusahlg/watt-start
cd watt-start
cargo install --path crates/watt-start   # or: nix profile add .
watt-start
```

On Windows, install [Git for Windows](https://git-scm.com/download/win) (`winget install Git.Git`)
and rustup (`winget install Rustlang.Rustup`) with the MSVC toolchain, plus the Visual Studio
Build Tools workload "Desktop development with C++" (the linker and Windows SDK `rustc` uses).
The GNU toolchain (`stable-x86_64-pc-windows-gnu`) also type-checks. The game needs a GPU driver
that provides `vulkan-1.dll`; the launcher window itself uses DirectX 12 when it is available.
Build with `cargo build --release` from a terminal. There is no installer yet.

The first build of an instance compiles the whole game and takes 15–40 minutes. Later builds
reuse everything that did not change, and instances on the same game version share their builds.

## Command line

The window is the main interface, but every operation also runs from a terminal:

```sh
watt-start list                       # instances and their state
watt-start create survival --mod pwc.essentials
watt-start update survival
watt-start build survival --force
watt-start play survival
watt-start doctor                     # can this machine build and run the game?
```

## Development

```sh
nix develop                   # Rust, git and the Wayland/Vulkan libraries the window needs
cargo test --workspace
WATT_START_DEMO=1 cargo run   # the window with demo data, no builds
cargo run                     # the real launcher (shares pwc's instances)
```

A plain `cargo build` outside `nix develop` compiles, but on NixOS the binary cannot find the
Wayland and Vulkan libraries when it starts. Binaries built inside the dev shell carry those
library paths, so they also start from elsewhere until the next `nix-collect-garbage`; for a
lasting install use `nix profile add .`.

The launcher expects the package manager checked out next to it (`../pwc-package-manager`).
See [ARCHITECTURE.md](ARCHITECTURE.md) for how it works.

## Licence

`AGPL-3.0-or-later`, like the package manager it is built on. See [LICENSE](LICENSE).
