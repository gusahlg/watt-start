//! Release helpers invoked as `cargo run -p xtask -- <command>`.
//!
//! Not a default workspace member: `cargo build` and `cargo run` at the
//! workspace root still build the launcher.

mod icon;
mod manifest;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", version, about = "Release tooling for watt-start")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Hash release executables and write `watt-start-update.json`.
    Manifest {
        /// Release version (`0.2.0` or `v0.2.0`). Must match the workspace version.
        #[arg(long)]
        version: String,
        /// Directory that holds the bare `watt-start-<version>-<target>` files.
        #[arg(long)]
        dist: PathBuf,
        /// Release notes. Leading and trailing whitespace is dropped.
        #[arg(long)]
        notes_file: Option<PathBuf>,
        /// Asset URL prefix. Default: the GitHub release download URL for this version.
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Write the application icon PNG (256×256, from `forge::app_icon_rgba`).
    Icon {
        /// Destination path.
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Manifest {
            version,
            dist,
            notes_file,
            base_url,
        } => {
            let normalized = manifest::normalize_version(&version)?;
            if normalized != env!("CARGO_PKG_VERSION") {
                anyhow::bail!(
                    "version {normalized} does not match the workspace version {}",
                    env!("CARGO_PKG_VERSION")
                );
            }
            let notes = match notes_file {
                Some(path) => manifest::read_notes(&path)?,
                None => String::new(),
            };
            let path = manifest::write(&manifest::Input {
                version: &version,
                dist: &dist,
                notes: &notes,
                base_url: base_url.as_deref(),
                published: &manifest::published_now(),
            })?;
            println!("wrote {}", path.display());
        }
        Command::Icon { out } => {
            icon::write_icon(&out).context("write icon")?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
