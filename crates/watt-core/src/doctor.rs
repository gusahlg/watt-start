//! First-run and troubleshooting checks: can this machine build and run PWC?

/// One check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// What was checked ("Git", "Rust", "Vulkan driver").
    pub name: String,
    /// The outcome.
    pub level: Level,
    /// What was found ("git version 2.51.0", "rustc 1.95.0 via nix develop").
    pub detail: String,
    /// What to do about a problem (empty when fine).
    pub hint: String,
}

/// How a check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Fine.
    Ok,
    /// Works, but worth knowing (Nix present: builds run inside `nix develop`).
    Info,
    /// Might cause trouble.
    Warn,
    /// Blocks building or playing.
    Error,
}

/// Run every check. Quick (well under a second; never runs `nix develop` or builds anything):
/// - Git: `git --version` (Error when missing: required).
/// - Cargo and rustc: `cargo --version`, `rustc --version` on PATH; Error when missing unless
///   `nix` is on PATH (then Info: builds use the game's `nix develop` shell); Warn when older than
///   1.90.
/// - Nix: Info when `nix` is on PATH ("builds run inside the game's nix develop shell").
/// - Vulkan driver: an ICD JSON in `/usr/share/vulkan/icd.d`, `/etc/vulkan/icd.d`,
///   `/run/opengl-driver/share/vulkan/icd.d` or `$VK_ICD_FILENAMES`/`$VK_DRIVER_FILES`; Warn when
///   none is found.
/// - Disk space: free bytes where the pwc cache lives (`statvfs`); Warn under 10 GiB, Error under
///   2 GiB (`paths` gives the cache directory; nearest existing ancestor is measured).
/// - Memory: `/proc/meminfo` MemTotal; Warn under 8 GiB ("builds use fewer jobs").
pub fn run(paths: &crate::paths::Paths) -> Vec<Check> {
    let _ = paths;
    todo!("qwen")
}
