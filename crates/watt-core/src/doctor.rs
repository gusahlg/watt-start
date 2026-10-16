//! First-run and troubleshooting checks: can this machine build and run PWC?

use std::os::unix::ffi::OsStrExt;

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
    let mut checks = Vec::new();

    // 1. Git
    checks.push(check_git());

    // 2. Rust
    checks.push(check_rust());

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

/// Check if an executable is on PATH.
fn on_path(name: &str) -> bool {
    let path_var = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };

    for dir in std::env::split_paths(&path_var) {
        let exe_path = dir.join(name);
        match std::fs::metadata(&exe_path) {
            Ok(meta) => {
                if meta.is_file() {
                    let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
                    if mode & 0o111 != 0 {
                        return true;
                    }
                }
            }
            Err(_) => continue,
        }
    }

    false
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

/// Format bytes as GiB with one decimal place.
fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{:.1} GiB", gib)
}

/// Check Git availability.
fn check_git() -> Check {
    match std::process::Command::new("git")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check {
                name: "Git".to_string(),
                level: Level::Ok,
                detail,
                hint: String::new(),
            }
        }
        Err(_) => Check {
            name: "Git".to_string(),
            level: Level::Error,
            detail: "not found".to_string(),
            hint: "Install git from your distribution's packages.".to_string(),
        },
    }
}

/// Check Rust (rustc and cargo) availability and version.
fn check_rust() -> Check {
    match std::process::Command::new("rustc")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => {
            let rustc_output = String::from_utf8_lossy(&output.stdout);
            let detail = rustc_output.trim().to_string();

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
        Err(_) => {
            if on_path("nix") {
                Check {
                    name: "Rust".to_string(),
                    level: Level::Info,
                    detail: "not installed; builds use the game's nix develop shell".to_string(),
                    hint: String::new(),
                }
            } else {
                Check {
                    name: "Rust".to_string(),
                    level: Level::Error,
                    detail: "not found".to_string(),
                    hint: "Install Rust from https://rustup.rs".to_string(),
                }
            }
        }
    }
}

/// Check Nix availability.
fn check_nix() -> Check {
    match std::process::Command::new("nix")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
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
fn check_disk_space(cache_path: &std::path::Path) -> Check {
    // Find the nearest existing ancestor directory
    let mut check_path = cache_path.to_path_buf();
    loop {
        if check_path.exists() {
            break;
        }
        if !check_path.pop() {
            return Check {
                name: "Disk space".to_string(),
                level: Level::Warn,
                detail: "unknown".to_string(),
                hint: String::new(),
            };
        }
    }

    // Use statvfs to get free space
    let c_path = match std::ffi::CString::new(check_path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => {
            return Check {
                name: "Disk space".to_string(),
                level: Level::Warn,
                detail: "unknown".to_string(),
                hint: String::new(),
            };
        }
    };

    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut st) };

    if result != 0 {
        return Check {
            name: "Disk space".to_string(),
            level: Level::Warn,
            detail: "unknown".to_string(),
            hint: String::new(),
        };
    }

    let free_bytes = st.f_bavail as u64 * st.f_frsize as u64;
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

/// Check available memory from /proc/meminfo.
fn check_memory() -> Check {
    match std::fs::read_to_string("/proc/meminfo") {
        Ok(content) => {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 && parts[1].parse::<u64>().is_ok() {
                        let kb = parts[1].parse::<u64>().unwrap_or(0);
                        let bytes = kb * 1024;
                        let detail = format_gib(bytes);
                        let (level, hint) = if bytes < 8 * 1024 * 1024 * 1024 {
                            (
                                Level::Warn,
                                "Builds will use fewer parallel jobs.".to_string(),
                            )
                        } else {
                            (Level::Ok, String::new())
                        };

                        return Check {
                            name: "Memory".to_string(),
                            level,
                            detail,
                            hint,
                        };
                    }
                }
            }

            Check {
                name: "Memory".to_string(),
                level: Level::Info,
                detail: "unknown".to_string(),
                hint: String::new(),
            }
        }
        Err(_) => Check {
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
