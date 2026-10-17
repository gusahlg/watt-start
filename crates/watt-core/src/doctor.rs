//! First-run and troubleshooting checks: can this machine build and run PWC?

use std::path::Path;
use std::process::Output;

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
/// - Git: `git --version` (Error when missing or when it exits non-zero: required).
/// - Cargo and rustc: `cargo --version`, `rustc --version` on PATH, both must exit 0; the detail
///   includes the rustc host triple from `rustc -vV`. Error when missing unless `nix` is on PATH
///   (then Info: builds use the game's `nix develop` shell); Warn when older than 1.90.
/// - Linker (Windows only): for an MSVC host, Ok when `vswhere.exe` finds Visual Studio C++
///   build tools, Warn when not; for a GNU host, Info when `gcc` is not on PATH.
/// - Nix: Info when `nix` is on PATH ("builds run inside the game's nix develop shell").
/// - Vulkan driver: on Linux, an ICD JSON in `/usr/share/vulkan/icd.d`, `/etc/vulkan/icd.d`,
///   `/run/opengl-driver/share/vulkan/icd.d` or `$VK_ICD_FILENAMES`/`$VK_DRIVER_FILES`; on
///   Windows, whether `vulkan-1.dll` loads. Warn when none is found.
/// - Disk space: free bytes where the pwc cache lives; Warn under 10 GiB, Error under 2 GiB
///   (`paths` gives the cache directory; nearest existing ancestor is measured).
/// - Memory: total physical memory; Warn under 8 GiB ("builds use fewer jobs").
pub fn run(paths: &crate::paths::Paths) -> Vec<Check> {
    let mut checks = Vec::new();

    // 1. Git
    checks.push(check_git());

    // 2. Rust (and, on Windows, the linker that host needs)
    checks.push(check_rust());
    if let Some(linker) = check_linker() {
        checks.push(linker);
    }

    // 3. Nix (only if on PATH)
    if on_path("nix") {
        checks.push(check_nix());
    }

    // 4. Vulkan driver
    checks.push(check_vulkan());

    // 5. Disk space
    checks.push(check_disk_space(&paths.pwc.cache));

    // 6. Memory
    checks.push(check_memory());

    checks
}

/// Check if an executable is on PATH. On Windows a name without `.exe` also matches `name.exe`.
fn on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if is_executable(&dir.join(name)) {
            return true;
        }
        #[cfg(windows)]
        if !name.to_ascii_lowercase().ends_with(".exe")
            && is_executable(&dir.join(format!("{name}.exe")))
        {
            return true;
        }
    }
    false
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn probe(program: &str, args: &[&str]) -> std::io::Result<Output> {
    crate::process::command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
}

/// Parse the minor version from "rustc 1.95.0 (...)" format.
fn parse_rustc_version(s: &str) -> Option<u32> {
    // Format: "rustc 1.95.0 (...)"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let version_str = parts[1];
    let version_parts: Vec<&str> = version_str.split('.').collect();
    if version_parts.len() < 2 {
        return None;
    }

    version_parts[1].parse().ok()
}

fn rustc_host(verbose: &str) -> Option<&str> {
    verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
}

/// Format bytes as GiB with one decimal place.
fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{gib:.1} GiB")
}

fn git_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Install Git for Windows: `winget install Git.Git`."
    }
    #[cfg(not(windows))]
    {
        "Install git from your distribution's packages."
    }
}

fn rust_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Install Rust: `winget install Rustlang.Rustup`, then the MSVC toolchain and the Visual \
         Studio Build Tools workload \"Desktop development with C++\" (or the GNU toolchain)."
    }
    #[cfg(not(windows))]
    {
        "Install Rust from https://rustup.rs"
    }
}

/// Check Git availability.
fn check_git() -> Check {
    match probe("git", &["--version"]) {
        Ok(output) if output.status.success() => {
            let detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check {
                name: "Git".to_string(),
                level: Level::Ok,
                detail,
                hint: String::new(),
            }
        }
        Ok(output) => Check {
            name: "Git".to_string(),
            level: Level::Error,
            detail: format!("git --version failed ({})", output.status),
            hint: git_hint().to_string(),
        },
        Err(_) => Check {
            name: "Git".to_string(),
            level: Level::Error,
            detail: "not found".to_string(),
            hint: git_hint().to_string(),
        },
    }
}

/// Check Rust (rustc and cargo) availability and version.
fn check_rust() -> Check {
    let output = match probe("rustc", &["--version"]) {
        Ok(output) => output,
        Err(_) if on_path("nix") => {
            return Check {
                name: "Rust".to_string(),
                level: Level::Info,
                detail: "not installed; builds use the game's nix develop shell".to_string(),
                hint: String::new(),
            };
        }
        Err(_) => {
            return Check {
                name: "Rust".to_string(),
                level: Level::Error,
                detail: "not found".to_string(),
                hint: rust_hint().to_string(),
            };
        }
    };
    if !output.status.success() {
        return Check {
            name: "Rust".to_string(),
            level: Level::Error,
            detail: format!("rustc --version failed ({})", output.status),
            hint: rust_hint().to_string(),
        };
    }

    let mut detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if let Some(host) = probe("rustc", &["-vV"])
        .ok()
        .filter(|verbose| verbose.status.success())
        .and_then(|verbose| {
            let text = String::from_utf8_lossy(&verbose.stdout).into_owned();
            rustc_host(&text).map(str::to_owned)
        })
    {
        detail = format!("{detail} (host: {host})");
    }

    match probe("cargo", &["--version"]) {
        Ok(cargo) if cargo.status.success() => {}
        Ok(cargo) => {
            return Check {
                name: "Rust".to_string(),
                level: Level::Error,
                detail: format!("cargo --version failed ({})", cargo.status),
                hint: rust_hint().to_string(),
            };
        }
        Err(_) => {
            return Check {
                name: "Rust".to_string(),
                level: Level::Error,
                detail: "cargo not found".to_string(),
                hint: rust_hint().to_string(),
            };
        }
    }

    let (level, hint) = if let Some(minor) = parse_rustc_version(&detail) {
        if minor >= 90 {
            (Level::Ok, String::new())
        } else {
            (
                Level::Warn,
                "Rust 1.90 or newer is needed; run `rustup update`.".to_string(),
            )
        }
    } else {
        (Level::Ok, String::new())
    };

    Check {
        name: "Rust".to_string(),
        level,
        detail,
        hint,
    }
}

/// Linker check for a Windows rustc host. `None` off Windows, and when rustc does not say.
///
/// MSVC: rustc finds `link.exe` through the Visual Studio installation, not PATH (outside a
/// Developer prompt it is never on PATH), so the check asks `vswhere.exe` for an installation
/// with the x64 C++ build tools, the same component rustc's linker lookup needs. GNU: rustup's
/// GNU toolchain ships its own self-contained linker, so a missing `gcc` is only Info (crates
/// that compile C code still need MinGW).
fn check_linker() -> Option<Check> {
    #[cfg(windows)]
    {
        let output = probe("rustc", &["-vV"]).ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let host = rustc_host(&text)?;
        if host.contains("windows-msvc") {
            Some(match msvc_build_tools() {
                Some(dir) => Check {
                    name: "Linker".to_string(),
                    level: Level::Ok,
                    detail: format!("MSVC build tools in {dir}"),
                    hint: String::new(),
                },
                None => Check {
                    name: "Linker".to_string(),
                    level: Level::Warn,
                    detail: "no Visual Studio C++ build tools found".to_string(),
                    hint: "Install the Visual Studio Build Tools workload \"Desktop development \
                           with C++\" (MSVC linker and Windows SDK), or use the GNU toolchain."
                        .to_string(),
                },
            })
        } else if host.contains("windows-gnu") && !on_path("gcc") {
            Some(Check {
                name: "Linker".to_string(),
                level: Level::Info,
                detail: "gcc is not on PATH; rustup's self-contained MinGW linker is used"
                    .to_string(),
                hint: "Crates that compile C code need a MinGW-w64 gcc on PATH.".to_string(),
            })
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The installation path of a Visual Studio (or Build Tools) instance with the x64 C++ tools,
/// from `vswhere.exe` (installed with every Visual Studio 2017+ product). `None` when there is
/// none, or `link.exe` on PATH with `VCINSTALLDIR` set (a Developer prompt) when vswhere is absent.
#[cfg(windows)]
fn msvc_build_tools() -> Option<String> {
    if let Some(dir) = std::env::var_os("VCINSTALLDIR").filter(|_| on_path("link")) {
        return Some(dir.to_string_lossy().trim_end_matches('\\').to_owned());
    }
    let program_files =
        std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles"))?;
    let vswhere = Path::new(&program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    let vswhere = vswhere.to_str()?;
    let output = probe(
        vswhere,
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
    )
    .ok()
    .filter(|output| output.status.success())?;
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!dir.is_empty()).then_some(dir)
}

/// Check Nix availability. A non-zero exit is still reported: the check only cares that `nix`
/// could be started (builds invoke it themselves).
fn check_nix() -> Check {
    match probe("nix", &["--version"]) {
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check {
                name: "Nix".to_string(),
                level: Level::Info,
                detail,
                hint: "Builds run inside the game's `nix develop` shell.".to_string(),
            }
        }
        Err(_) => Check {
            name: "Nix".to_string(),
            level: Level::Error,
            detail: "not found".to_string(),
            hint: String::new(),
        },
    }
}

/// Check Vulkan driver availability.
fn check_vulkan() -> Check {
    #[cfg(windows)]
    {
        if crate::process::vulkan_loader_present() {
            Check {
                name: "Vulkan driver".to_string(),
                level: Level::Ok,
                detail: "vulkan-1.dll".to_string(),
                hint: String::new(),
            }
        } else {
            Check {
                name: "Vulkan driver".to_string(),
                level: Level::Warn,
                detail: "vulkan-1.dll not found".to_string(),
                hint: "Install a GPU driver that provides Vulkan (vulkan-1.dll).".to_string(),
            }
        }
    }
    #[cfg(not(windows))]
    {
        check_vulkan_icd()
    }
}

#[cfg(not(windows))]
fn check_vulkan_icd() -> Check {
    // Check environment variables first
    if let Ok(val) = std::env::var("VK_ICD_FILENAMES")
        && !val.is_empty()
    {
        return Check {
            name: "Vulkan driver".to_string(),
            level: Level::Ok,
            detail: val,
            hint: String::new(),
        };
    }

    if let Ok(val) = std::env::var("VK_DRIVER_FILES")
        && !val.is_empty()
    {
        return Check {
            name: "Vulkan driver".to_string(),
            level: Level::Ok,
            detail: val,
            hint: String::new(),
        };
    }

    // Check directories
    let dirs = [
        "/usr/share/vulkan/icd.d",
        "/etc/vulkan/icd.d",
        "/run/opengl-driver/share/vulkan/icd.d",
    ];

    // Name every driver found ("nvidia, radeon"), not just the first file: machines often carry
    // several Mesa drivers next to the one their GPU uses.
    let mut drivers: Vec<String> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "json").then(|| {
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                Some(stem.split('_').next().unwrap_or(&stem).to_owned())
            })?
        })
        .collect();
    drivers.sort();
    drivers.dedup();
    if !drivers.is_empty() {
        return Check {
            name: "Vulkan driver".to_string(),
            level: Level::Ok,
            detail: drivers.join(", "),
            hint: String::new(),
        };
    }

    Check {
        name: "Vulkan driver".to_string(),
        level: Level::Warn,
        detail: "no Vulkan driver found".to_string(),
        hint:
            "Install your GPU's Vulkan driver (Mesa for AMD/Intel, the NVIDIA driver for NVIDIA)."
                .to_string(),
    }
}

/// Check disk space at the cache directory.
fn check_disk_space(cache_path: &Path) -> Check {
    // Find the nearest existing ancestor directory
    let mut check_path = cache_path.to_path_buf();
    loop {
        if check_path.exists() {
            break;
        }
        if !check_path.pop() {
            return unknown_disk();
        }
    }

    let Some(free_bytes) = crate::process::disk_free_bytes(&check_path) else {
        return unknown_disk();
    };
    let detail = format!(
        "{} free in {}",
        format_gib(free_bytes),
        check_path.display()
    );

    let (level, hint) = if free_bytes < 2 * 1024 * 1024 * 1024 {
        (Level::Error, "A build needs about 10 GiB free.".to_string())
    } else if free_bytes < 10 * 1024 * 1024 * 1024 {
        (Level::Warn, "A build needs about 10 GiB free.".to_string())
    } else {
        (Level::Ok, String::new())
    };

    Check {
        name: "Disk space".to_string(),
        level,
        detail,
        hint,
    }
}

fn unknown_disk() -> Check {
    Check {
        name: "Disk space".to_string(),
        level: Level::Warn,
        detail: "unknown".to_string(),
        hint: String::new(),
    }
}

/// Check physical memory. Linux reads `/proc/meminfo`; Windows calls `GlobalMemoryStatusEx`.
fn check_memory() -> Check {
    match crate::process::total_memory_bytes() {
        Some(bytes) => {
            let detail = format_gib(bytes);
            let (level, hint) = if bytes < 8 * 1024 * 1024 * 1024 {
                (
                    Level::Warn,
                    "Builds will use fewer parallel jobs.".to_string(),
                )
            } else {
                (Level::Ok, String::new())
            };
            Check {
                name: "Memory".to_string(),
                level,
                detail,
                hint,
            }
        }
        None => Check {
            name: "Memory".to_string(),
            level: Level::Info,
            detail: "unknown".to_string(),
            hint: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwc_instance::Dirs;

    #[test]
    fn test_run_returns_checks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = Dirs::under(tmp.path());
        let paths = crate::paths::Paths::new(dirs).unwrap();
        let checks = run(&paths);

        assert!(checks.len() >= 4);
        assert_eq!(checks[0].name, "Git");
    }

    #[test]
    fn test_parse_rustc_version() {
        assert_eq!(
            parse_rustc_version("rustc 1.95.0 (abc 2026-01-01)"),
            Some(95)
        );
        assert_eq!(parse_rustc_version("garbage"), None);
        assert_eq!(parse_rustc_version(""), None);
        assert_eq!(parse_rustc_version("rustc 1.90.0"), Some(90));
    }

    #[test]
    fn test_format_gib() {
        let bytes = 1610612736; // 1.5 GiB
        assert_eq!(format_gib(bytes), "1.5 GiB");
    }
}
