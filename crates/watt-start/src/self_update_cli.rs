//! `watt-start self-update`: check the public manifest and, unless told not to, replace this
//! executable. The process is not restarted; the user starts the new build themselves.

use std::io::{IsTerminal, Write};

use anyhow::{Context as _, Result, bail};
use watt_core::paths::Paths;
use watt_core::selfupdate::{self, Phase};

/// Check for a launcher update and optionally install it.
///
/// `--check` stops after printing. `--yes` installs without a prompt. A non-interactive
/// stdin without `--yes` does not install. Package-managed and development installs print
/// instructions and succeed without replacing anything.
pub fn run(check_only: bool, yes: bool) -> Result<()> {
    if selfupdate::update_check_disabled() {
        println!("Launcher update checks are disabled (WATT_START_NO_UPDATE_CHECK).");
        return Ok(());
    }
    let paths = Paths::from_env()?;
    let status = selfupdate::check_and_record(&paths.update_state(), true);
    println!("Current version: {}", status.current_version);
    println!("Install: {}", status.install.label());
    match status.phase {
        Phase::NoInformation => {
            println!("No update information.");
            return Ok(());
        }
        Phase::Failed => {
            bail!(
                "{}",
                status
                    .error
                    .unwrap_or_else(|| "update check failed".to_owned())
            );
        }
        Phase::UpToDate => {
            println!(
                "Up to date ({}).",
                status
                    .latest_version
                    .as_deref()
                    .unwrap_or(&status.current_version)
            );
            return Ok(());
        }
        Phase::Available => {}
        Phase::Idle | Phase::Checking | Phase::Downloading => {
            bail!("update check did not finish");
        }
    }
    if let Some(version) = &status.latest_version {
        println!("Latest version: {version}");
    }
    if !status.notes.is_empty() {
        println!("{}", status.notes);
    }
    if !status.notes_url.is_empty() {
        println!("{}", status.notes_url);
    }
    if !status.install.can_replace() {
        println!("{}", status.install.instructions());
        return Ok(());
    }
    let Some(asset) = status.asset.clone() else {
        println!("No automatic update for this system. Download it from the release page.");
        return Ok(());
    };
    if check_only {
        return Ok(());
    }
    if !yes && !std::io::stdin().is_terminal() {
        println!("Re-run with --yes to install this update.");
        return Ok(());
    }
    if !yes && !confirm()? {
        println!("Not installed.");
        return Ok(());
    }
    let exe = status
        .install
        .exe()
        .context("this install has no executable path")?
        .to_path_buf();
    let version = status.latest_version.clone().unwrap_or_default();
    eprintln!("Downloading…");
    let mut last_pct = 0u8;
    let result = selfupdate::download_and_install(
        &paths.updates(),
        &asset,
        &version,
        &exe,
        &mut |done, total| {
            if let Some(total) = total.filter(|total| *total > 0) {
                let pct = ((done.saturating_mul(100)) / total).min(100) as u8;
                if pct != last_pct {
                    last_pct = pct;
                    eprint!("\rDownloading {pct}%");
                    let _ = std::io::stderr().flush();
                }
            }
        },
    );
    eprintln!();
    result?;
    println!("Installed {version}. Restart watt-start to use it.");
    Ok(())
}

/// Ask on stderr. Empty and anything other than yes declines.
fn confirm() -> Result<bool> {
    eprint!("Install this update? [y/N] ");
    std::io::stderr().flush().context("cannot prompt")?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("cannot read the answer")?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}
