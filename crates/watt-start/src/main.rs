//! `watt-start`: the Project Watt Cubed launcher.
//!
//! Without arguments it opens the GUI. `watt-start op ...` runs one op headless: the GUI runs
//! every long operation this way, as a child process whose JSON event lines it reads
//! (`--events json --request <json>`); people and scripts use the friendlier subcommands.

mod gui;
mod self_update_cli;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use watt_core::events::{
    CancelToken, FileSink, HumanSink, JsonLinesSink, OpOutcome, Reporter, Sink, TeeSink, new_op_id,
};
use watt_core::ops::{OpRequest, SourceSpec};
use watt_core::paths::Paths;
use watt_core::settings::Settings;

#[derive(Parser)]
#[command(
    name = "watt-start",
    version,
    about = "The Project Watt Cubed launcher"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the launcher window (the default).
    Gui,
    /// Run one op from its JSON request (the launcher's worker protocol).
    Op {
        /// The request, as JSON (see `watt_core::ops::OpRequest`).
        #[arg(long)]
        request: String,
        /// How to report.
        #[arg(long, value_enum, default_value_t = Events::Human)]
        events: Events,
    },
    /// List instances.
    List,
    /// Check that this machine can build and run the game.
    Doctor,
    /// Create an instance that follows a game branch.
    Create {
        /// Directory name.
        name: String,
        /// Game branch or tag to follow.
        #[arg(long, default_value = "main")]
        channel: String,
        /// Build against this game checkout instead (developers).
        #[arg(long, conflicts_with = "channel")]
        linked: Option<std::path::PathBuf>,
        /// Mods to add (`pwc mod add` specs).
        #[arg(long = "mod")]
        mods: Vec<String>,
        /// Do not build right away.
        #[arg(long)]
        no_build: bool,
    },
    /// Update game, engine and mods, then build (per the settings).
    Update {
        /// Instance.
        instance: String,
        /// Resolve mods from scratch.
        #[arg(long)]
        reset_lock: bool,
    },
    /// Build an instance.
    Build {
        /// Instance.
        instance: String,
        /// Rebuild even when cached.
        #[arg(long)]
        force: bool,
    },
    /// Build if needed and play.
    Play {
        /// Instance.
        instance: String,
        /// Run the last working build.
        #[arg(long)]
        last_good: bool,
    },
    /// Fetch and report available updates.
    CheckUpdates {
        /// Instance (default: all managed instances).
        instance: Option<String>,
    },
    /// Check for a new watt-start release, and install it when this copy can replace itself.
    SelfUpdate {
        /// Only report; do not install.
        #[arg(long)]
        check: bool,
        /// Install without asking.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Events {
    /// Readable lines on stderr.
    Human,
    /// Protocol envelopes on stdout, one JSON object per line.
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None | Some(Command::Gui) => gui::run().map(|()| ExitCode::SUCCESS),
        Some(Command::Op { request, events }) => serde_json::from_str::<OpRequest>(&request)
            .context("invalid --request")
            .and_then(|request| run_op(&request, events)),
        Some(Command::List) => list().map(|()| ExitCode::SUCCESS),
        Some(Command::Doctor) => doctor().map(|()| ExitCode::SUCCESS),
        Some(Command::Create {
            name,
            channel,
            linked,
            mods,
            no_build,
        }) => {
            let source = match linked {
                Some(pwc_source) => SourceSpec::Linked {
                    pwc_source,
                    repositories: Vec::new(),
                },
                None => SourceSpec::Managed {
                    channel,
                    engine_ref: watt_core::meta::AUTO.to_owned(),
                    mods_ref: watt_core::meta::AUTO.to_owned(),
                },
            };
            run_op(
                &OpRequest::Create {
                    name,
                    display_name: None,
                    color: None,
                    group: None,
                    source,
                    mods,
                    build: !no_build,
                },
                Events::Human,
            )
        }
        Some(Command::Update {
            instance,
            reset_lock,
        }) => run_op(
            &OpRequest::Update {
                instance,
                build: None,
                reset_lock,
            },
            Events::Human,
        ),
        Some(Command::Build { instance, force }) => run_op(
            &OpRequest::Build {
                instance,
                force,
                profile: None,
            },
            Events::Human,
        ),
        Some(Command::Play {
            instance,
            last_good,
        }) => run_op(
            &OpRequest::Play {
                instance,
                last_good,
                args: Vec::new(),
            },
            Events::Human,
        ),
        Some(Command::CheckUpdates { instance }) => {
            run_op(&OpRequest::CheckUpdates { instance }, Events::Human)
        }
        Some(Command::SelfUpdate { check, yes }) => {
            self_update_cli::run(check, yes).map(|()| ExitCode::SUCCESS)
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Run one op with the chosen reporting, teeing a readable copy into the op's log file.
fn run_op(request: &OpRequest, events: Events) -> Result<ExitCode> {
    let paths = Paths::from_env()?;
    let settings = Settings::load(&paths.settings_file)?;
    let op_id = new_op_id();
    let mut sinks: Vec<Arc<dyn Sink>> = vec![match events {
        Events::Human => Arc::new(HumanSink),
        Events::Json => Arc::new(JsonLinesSink),
    }];
    if let Ok(file) = FileSink::open(&paths.op_log(&op_id)) {
        sinks.push(Arc::new(file));
    }
    let reporter = Reporter::new(Arc::new(TeeSink(sinks)), op_id, CancelToken::default());

    let mut capture = None;
    if matches!(events, Events::Json) {
        // Cargo draws its progress bar only for terminals unless told otherwise; the stderr
        // capture parses it. Set before any thread starts (the capture's reader is the first).
        // SAFETY: single-threaded at this point; nothing reads the environment concurrently.
        unsafe {
            std::env::set_var("CARGO_TERM_PROGRESS_WHEN", "always");
            std::env::set_var("CARGO_TERM_PROGRESS_WIDTH", "200");
            std::env::set_var("CARGO_TERM_COLOR", "never");
        }
        capture = Some(watt_core::process::capture_stderr(reporter.clone(), None)?);
    }
    if matches!(
        request,
        OpRequest::Build { .. }
            | OpRequest::Update { .. }
            | OpRequest::Mods { .. }
            | OpRequest::Create { .. }
            | OpRequest::Rollback { .. }
            | OpRequest::SetSource { .. }
    ) {
        watt_core::process::set_nice(settings.build_nice);
    }

    let outcome = watt_core::ops::run(&paths, &settings, request, &reporter);
    drop(capture);
    Ok(match outcome {
        OpOutcome::Ok { .. } => ExitCode::SUCCESS,
        OpOutcome::Cancelled => ExitCode::from(130),
        OpOutcome::Failed { .. } => ExitCode::FAILURE,
    })
}

fn list() -> Result<()> {
    let paths = Paths::from_env()?;
    for card in watt_core::catalog::list_instances(&paths)? {
        let status = watt_core::status::compute(&paths, &card)?;
        println!(
            "{:<20} {:<28} {:<12} {:>3} mods  {:?}",
            card.name,
            card.meta.title(&card.name),
            card.game.version.as_deref().unwrap_or("-"),
            card.packages.len(),
            status.readiness
        );
    }
    for (name, error) in watt_core::catalog::list_errors(&paths) {
        println!("{name:<20} unreadable: {error}");
    }
    Ok(())
}

fn doctor() -> Result<()> {
    let paths = Paths::from_env()?;
    for check in watt_core::doctor::run(&paths) {
        println!(
            "{:<6} {:<16} {}",
            format!("{:?}", check.level),
            check.name,
            check.detail
        );
        if !check.hint.is_empty() {
            println!("       {:<16} {}", "", check.hint);
        }
    }
    Ok(())
}
