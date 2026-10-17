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
  The launcher additionally holds its own `build.lock` (launcher-wide, not per instance) from
  just before `pwc-builder::prepare` through `compile`'s completion metadata, so two launcher
  builds with identical inputs never race `pwc-builder`'s fixed temporary file names; the
  target-directory lock remains the mechanism that coordinates with the `pwc` CLI (it is released
  before `compile`, which takes it itself).
- `git.lock` serialises mirror and snapshot changes across launchers and ops. Snapshot garbage
  collection and unpinning run under one `git.lock` acquisition (`snapshots::gc_locked` +
  `git::unpin_except`), so nothing can publish a new snapshot or pin in the gap between them.
- `Maintenance` additionally takes every instance's `op.lock` (`try_acquire`, failing fast with
  "`<name>` is busy" if any is held) and holds them all for the whole op, so no concurrent
  launcher op can install a package or publish a lock/pin/generation underneath the keep-sets
  garbage collection computes. `clean_cache` also refuses while the target-directory lock is held.
- A build takes the exact `Lockfile` its caller just locked, never rereading `pwc.lock` from disk
  itself (another tool could have rewritten it in between); it still double-checks, right before
  compiling, that `pwc.lock` on disk still matches, failing ("pwc.lock changed while building")
  rather than recording a generation nobody can trust.
- `game.lock` (`watt-start/game.lock`, inside each instance) is held for exactly as long as a
  game launched by `Play` runs: acquired before launching, made inheritable
  (`FD_CLOEXEC` cleared) and then released by the launcher itself — the detached game's own
  inherited copy of the descriptor is what keeps the `flock` held, following it past the
  launcher's own exit or a crash. `status::compute`, `Play` and `Remove` (and the world-copying
  parts of `Update` and `Duplicate`) treat this lock, not `watt-start/game.toml` (which only
  records the pid/log/started for display), as the truth that a game is running.
- `Duplicate` builds its copy in a hidden `instances/.tmp-dup-<name>-<pid>` staging directory and
  publishes it with one atomic rename, refusing if the destination already exists; a kill
  partway through leaves only the staging directory, which the next `Duplicate` cleans up.
- Locks are `flock`s: a killed op releases them.
- Every file is written atomically, in an order that is consistent at every point: sources exist
  before anything points at them; `pwc.lock` is written before the binding it was made from (a lock
  newer than its binding is drift, repaired by the next op); a generation is recorded only after
  its build finished and `meta.toml` is committed before any file belonging to a generation it
  prunes is deleted; `SetSource` and `Rollback` commit the binding itself last, once every source,
  `instance.toml` and `pwc.lock` it depends on already exists — a nonexistent channel, or a kill
  partway through, leaves the previous (still working) binding in place.
- A generation's identity for rollback/recording purposes is `(environment, PWC source, binding
  pin)`, not the environment hash alone: two managed generations can have identical game and mod
  revisions (so equal environments) while differing only in the engine commit, which the
  environment hash does not cover. `status::compute` reports `NeedsBuild` ("the engine changed
  since the last build") in exactly that case, instead of incorrectly `Ready`.

## Interplay with the `pwc` CLI

- `pwc run/build --instance X` on a managed or linked instance relocks it against pwc's global
  checkout. The launcher sees the lock's `[pwc].source` differ from the binding and relocks back.
- `pwc store gc` keeps only packages referenced by current locks; a rollback whose packages were
  collected relocks from its snapshot. The launcher's own maintenance also keeps generation locks.
- `pwc cache clean` removes builds; generations whose executable is gone are not offered to play.
- `pwc instance remove` removes the launcher metadata with the instance.
- A game started with `pwc run` holds no `game.lock`: the launcher has no way to see it running,
  so `status::compute`, `Play` and `Remove` cannot detect or refuse against it. Only games started
  through the launcher's own `Play` are covered.
- `Maintenance`'s instance-lock and `git.lock` coordination (above) only serialises against the
  launcher's own ops; it has no way to hold off a concurrent `pwc store gc`, `pwc cache clean` or a
  CLI build. Racing one of those against launcher maintenance is not prevented — the existing
  defenses (atomic installs/renames, `pwc-builder`'s own target lock, relocking on drift) limit
  the damage, but do not eliminate the window.

## Open ends

- Per-instance game sources would ideally be a pwc feature (`pwc-source` in `instance.toml`), so
  the CLI respects them too.
- A cancellable, output-capturing build entry point in `pwc-builder` (`compile_with`) would allow
  in-process builds; the child process would remain useful for isolation.
- Mirrors are full clones. The public repositories are small today (tens of MiB together); a
  partial-clone mode would keep the first download small if the history grows.
- Windows: the op runner's stderr capture and process groups are Unix-only today.

## Self-update

watt-start can replace itself. The check is not an op: it runs on a background thread in the GUI
process (and inline in `watt-start self-update`). Game updates stay on the existing `CheckUpdates`
op.

The published manifest is `watt-start-update.json` at
`https://github.com/gusahlg/watt-start/releases/latest/download/`, or under `WATT_START_UPDATE_URL`.
Its signature is the same URL with `.minisig` appended and covers the raw manifest bytes. Assets
are keyed by target (`x86_64-linux-gnu`, `aarch64-linux-gnu`, `x86_64-windows-msvc`,
`aarch64-macos`). Only `kind: "executable"` is installed; other kinds are left for a manual
download. Unknown JSON keys are ignored. A release is offered only when its semver is newer than
`CARGO_PKG_VERSION`.

`packaging/update-key.pub` (and `update-key-next.pub`, for a rotation) is compiled in by
`crates/watt-core/build.rs` when the file exists (`cfg(watt_update_signed)`). With a key, a missing
or invalid signature is a hard failure. Without one, the updater trusts HTTPS plus the sha256 in
the manifest, and the About card says updates are not signed. The secret key is never in the repo.

A successful check is remembered in `<pwc data>/watt-start/update-check.json` (`last_checked`,
`latest_seen`). Failures and a 404 ("no update information") are not recorded, so the next launch
retries. The GUI checks at startup when `check-launcher-updates` is true (the default) and the last
success is older than 24 hours. "Check now" forces it. `WATT_START_NO_UPDATE_CHECK=1` skips the
network entirely, which is how tests stay offline. A startup failure is not a toast; it shows on
the About card.

`detect_install` classifies the running executable after resolving symlinks: Nix, Flatpak, Scoop,
a system package (`/usr` but not `/usr/local`), `cargo install`, a development build
(`debug_assertions` or a `target/` directory), a read-only directory, or a release binary. Package
managers win over the development rule. Everything except a writable release binary gets
instructions and no "Update and restart" button. The same rules are what `watt-start self-update`
prints. `--check` stops before installing; `--yes` installs without a prompt; a non-interactive
stdin without `--yes` does not install. The CLI does not restart the process.

A release install downloads into `<pwc data>/watt-start/updates/<version>/`, hashing while it
streams, and rejects a size or sha256 mismatch (size `0` is a real size). The file is then copied
beside the executable and swapped with rename-then-rename: the running file becomes `<stem>-prev`,
which is kept, and the new file takes its place. On Windows the existing `-prev` is removed with
short retries, falling back to `-prev-<pid>`. If the second rename fails, the previous file is
renamed back. Unix builds are marked `0o755`. Downloads are capped at 512 MiB.

After a successful replace the GUI sets `restart_pending`, spawns the new executable detached with
`WATT_START_UPDATED_FROM` set to the version just replaced (the path is captured before the
rename; on Linux `current_exe` afterwards follows `-prev`), and closes the window. The next launch
toasts `Updated to <version>`. `start_op` refuses while that restart is pending. "Later"
hides the sidebar row for the rest of the session only.

The HTTP client is ureq 3 with rustls (`User-Agent: watt-start/<version>`, gzip, 10 second header
timeouts). The GUI row, dialog, and About card live in `gui/app/launcher_update.rs` and use the
Filament widgets. `WATT_START_DEMO_UPDATE=1` (or `DemoBackend::show_update_available`) fakes an
available update without using the network.
