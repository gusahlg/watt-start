# Architecture

watt-start is a graphical front end over the PWC package manager's library crates
(`pwc-instance`, `pwc-builder`, `pwc-manifest`, `pwc-store`), the "launcher" phase of the package
manager's roadmap. It adds what is too high-level for a `pwc` command: instances with their own
game versions and branches, one-click updates of game + engine + mods, automatic rebuilds,
rollback to earlier working builds, and a progress display you cannot miss.

```text
crates/watt-core     the engine: no GUI, fully testable
crates/watt-start    the binary: the egui window, plus `watt-start op ...` (headless ops)
```

## Ideas

- **Instance**: a pwc instance (`instance.toml` = wanted mods, `pwc.lock` = resolved set, `game/` =
  worlds and settings) plus launcher metadata (`watt-start/meta.toml`).
- **Binding**: where an instance's game, engine and mods come from. It is the truth; the lock is
  derived from it and relocked whenever it drifts.
  - `managed`: follows a channel (a game branch or tag). The engine and the package manager follow
    the same-named branch when they have one, else their default branch (overridable). Pinned to
    exact commits; Update moves the pin.
  - `linked`: a game checkout on disk (developers). Built, never modified.
  - `pwc-default`: exactly what `pwc` does (global `config.toml`). Every instance made with the
    CLI is this.
- **Snapshot**: an immutable, content-addressed checkout of exact commits, created from local
  mirrors. `sources/v1/pwc-<game12>-<engine12>/{project_watt_cubed,voxel-engine}` and
  `sources/v1/mods-<mods12>/pwc-package-manager`. Instances pinned alike share a snapshot, and
  therefore their Cargo artifacts and Build IDs; updating one instance never touches another.
- **Generation**: a state that built (binding, lock, executable). Rollback re-activates one; "play
  the last working build" runs one when the current state does not build.
- **Op**: a long operation (create, mods, update, build, play, rollback, duplicate, remove,
  set-source, check-updates, maintenance), reported as a stream of events.

## State

The launcher shares pwc's directories (`pwc_instance::Dirs`: XDG, or `PWC_HOME`), so `pwc` and the
launcher see the same instances, and every build shares one package store, one build cache and one
Cargo target directory (the reason a second instance builds in minutes, not 40). Its own state
lives inside pwc's roots (see `crates/watt-core/src/paths.rs`):

```text
<pwc data>/watt-start/git/<repo>.git            bare mirrors (refs/pins/* keep pinned commits alive)
<pwc data>/watt-start/sources/v1/...            snapshots (.complete marks a finished one)
<pwc data>/watt-start/logs/<op id>.log          one log per op
<pwc config>/watt-start.toml                    settings
<pwc data>/instances/<name>/watt-start/         meta.toml, generations/<n>.lock, op.lock,
                                                game.toml (while running), logs/, backups/
```

Snapshot paths are baked into locks, Build IDs and Cargo fingerprints, so the data root is
resolved to its canonical path once and must not move. The layout is versioned (`sources/v1`).

## Process model

```text
 GUI (egui)  ──spawn──▶  watt-start op --events json --request '<json>'   (own process group)
     ▲                         │  stdout: one JSON envelope per line (events.rs, protocol 1)
     │  OpView (view.rs)       │  fd 2 captured: git/nix/cargo/rustc output → Log/Progress events
     └──────── client.rs ◀─────┘
```

Every op runs in a child process. pwc's builder runs Cargo synchronously with its output on
stderr and without a cancellation hook; a child process gives streaming (the child replaces its own
fd 2 with a pipe and turns every line into events, Cargo's progress bar into `Progress`, the final
fat-LTO link into a `Link` phase), cancellation (SIGTERM, then SIGKILL, to the whole process group
including nix, cargo and rustc) and crash isolation, without changing pwc. The same binary serves
both sides, so the protocol cannot drift; `watt-start op ...` also works by hand and in scripts.

Read-only views (catalog, status, available mods, doctor) run in the GUI process on background
threads. Small presentation edits (name, colour, group, notes) are written in-process through
`meta::update`, which re-reads `meta.toml` under a lock so they never overwrite an op's changes.

## Concurrency and crash safety

- One op per instance (`op.lock`); different instances can run ops at once. Builds serialise on
  pwc's target-directory lock; an op waiting for it shows a "Waiting for another build" phase.
- `git.lock` serialises mirror and snapshot changes across launchers and ops.
- Locks are `flock`s: a killed op releases them.
- Every file is written atomically, in an order that is consistent at every point: sources exist
  before anything points at them; `pwc.lock` is written before the binding it was made from (a lock
  newer than its binding is drift, repaired by the next op); a generation is recorded only after
  its build finished.

## Interplay with the `pwc` CLI

- `pwc run/build --instance X` on a managed or linked instance relocks it against pwc's global
  checkout. The launcher sees the lock's `[pwc].source` differ from the binding and relocks back.
- `pwc store gc` keeps only packages referenced by current locks; a rollback whose packages were
  collected relocks from its snapshot. The launcher's own maintenance also keeps generation locks.
- `pwc cache clean` removes builds; generations whose executable is gone are not offered to play.
- `pwc instance remove` removes the launcher metadata with the instance.

## Open ends

- Per-instance game sources would ideally be a pwc feature (`pwc-source` in `instance.toml`), so
  the CLI respects them too.
- A cancellable, output-capturing build entry point in `pwc-builder` (`compile_with`) would allow
  in-process builds; the child process would remain useful for isolation.
- Mirrors are full clones. The public repositories are small today (tens of MiB together); a
  partial-clone mode would keep the first download small if the history grows.
- Windows: the op runner's stderr capture and process groups are Unix-only today.
