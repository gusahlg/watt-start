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

You need Linux, git, a Vulkan driver, and Rust 1.90 or newer (or Nix). The game, its engine and
the package manager are downloaded by the launcher itself.

```sh
git clone https://github.com/gusahlg/pwc-package-manager
git clone https://github.com/gusahlg/watt-start
cd watt-start
cargo install --path crates/watt-start   # or: nix profile add .
watt-start
```

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
nix develop          # or any shell with Rust, git and the Wayland/Vulkan libraries
cargo test --workspace
WATT_START_DEMO=1 cargo run   # the window with demo data, no builds
```

The launcher expects the package manager checked out next to it (`../pwc-package-manager`).
See [ARCHITECTURE.md](ARCHITECTURE.md) for how it works.

## Licence

`AGPL-3.0-or-later`, like the package manager it is built on. See [LICENSE](LICENSE).
