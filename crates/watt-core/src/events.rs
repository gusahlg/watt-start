//! The op event protocol, version 1.
//!
//! Every long operation ("op": create, update, build, play...) reports what it does as a stream of
//! [`Event`]s. A front end runs an op as a child process (`watt-start op <verb> --events json`)
//! and reads one JSON [`Envelope`] per line from its stdout. Everything else the op and its tools
//! print (git, cargo, rustc, nix) arrives as [`Event::Log`] lines, so stdout carries nothing but
//! the protocol. A stream that ends without [`Event::OpFinished`] means the op died.
//!
//! The protocol is a contract for scripts too: add fields with `#[serde(default)]`, never change
//! the meaning of an existing one, and bump [`PROTOCOL`] when an old reader would misread a stream.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// The protocol version written in every envelope.
pub const PROTOCOL: u32 = 1;

/// One line of the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// [`PROTOCOL`].
    pub protocol: u32,
    /// Identifies the op run (unique per run; also names its log file).
    pub op_id: String,
    /// 0, 1, 2... in emission order.
    pub seq: u64,
    /// Milliseconds since the op started.
    pub t_ms: u64,
    /// The event.
    #[serde(flatten)]
    pub event: Event,
}

/// What happened.
// Events are short-lived protocol values; boxing the big variants would only complicate serde.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    /// The first event of every op.
    OpStarted {
        /// Which op.
        op: OpKind,
        /// The instance it works on, if any.
        #[serde(default)]
        instance: Option<String>,
        /// The phases it plans to go through, in order (some may end up skipped).
        #[serde(default)]
        phases: Vec<Phase>,
        /// The op's log file.
        #[serde(default)]
        log: Option<PathBuf>,
    },
    /// A phase began.
    PhaseStarted {
        /// Which.
        phase: Phase,
        /// Human title ("Compiling the game").
        title: String,
        /// Whether [`Event::Progress`] will carry a total.
        #[serde(default)]
        determinate: bool,
    },
    /// Progress inside the current phase.
    Progress {
        /// The phase it belongs to.
        phase: Phase,
        /// Units done.
        done: u64,
        /// Units in total, when known.
        #[serde(default)]
        total: Option<u64>,
        /// What is being worked on right now ("voxel_engine, project_watt_cubed").
        #[serde(default)]
        detail: String,
    },
    /// One line of output from the op or a tool it runs.
    Log {
        /// Where it came from.
        stream: LogStream,
        /// The line, without its terminator and without ANSI escapes.
        line: String,
    },
    /// Something the user should know that does not stop the op.
    Warning {
        /// What.
        message: String,
    },
    /// State the op changed.
    Changed {
        /// What changed.
        change: Change,
    },
    /// A phase ended.
    PhaseFinished {
        /// Which.
        phase: Phase,
        /// How.
        outcome: PhaseOutcome,
        /// How long it took.
        duration_ms: u64,
    },
    /// The last event of every op.
    OpFinished {
        /// How it ended.
        outcome: OpOutcome,
        /// How long it took.
        duration_ms: u64,
    },
}

/// The ops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpKind {
    /// Create an instance.
    Create,
    /// Add/remove mods (then build, per settings).
    Mods,
    /// Update game, engine and mods (then build, per settings).
    Update,
    /// Build (or rebuild with `--force`).
    Build,
    /// Build if needed, then start the game.
    Play,
    /// Go back to an earlier generation.
    Rollback,
    /// Copy an instance.
    Duplicate,
    /// Delete an instance.
    Remove,
    /// Change where an instance's sources come from (channel, linked checkout).
    SetSource,
    /// Fetch the mirrors and report what updates are available. Changes no instance.
    CheckUpdates,
    /// Cache clean, store and snapshot garbage collection.
    Maintenance,
}

impl OpKind {
    /// "Updating", "Building"...: the verb for progress displays.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Create => "Creating",
            Self::Mods => "Applying mod changes",
            Self::Update => "Updating",
            Self::Build => "Building",
            Self::Play => "Starting",
            Self::Rollback => "Rolling back",
            Self::Duplicate => "Duplicating",
            Self::Remove => "Removing",
            Self::SetSource => "Switching source",
            Self::CheckUpdates => "Checking for updates",
            Self::Maintenance => "Cleaning up",
        }
    }
}

/// The phases ops are made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Reading state, taking locks, checking inputs.
    Prepare,
    /// Downloading game, engine and mods (git fetch).
    Fetch,
    /// Creating source snapshots (git clone + checkout).
    Checkout,
    /// Copying the instance's worlds aside before an update changes the game.
    Backup,
    /// Resolving mods and writing `pwc.lock`.
    Lock,
    /// Identifying the Rust toolchain (`rustc -vV`, possibly inside `nix develop`).
    Toolchain,
    /// Waiting for another build that holds the shared target directory.
    WaitForBuild,
    /// Cargo compiling crates.
    Compile,
    /// The final link of the game executable (fat LTO: long, no visible progress).
    Link,
    /// Copying the executables into the build cache, recording the generation.
    Install,
    /// Starting the game.
    Launch,
    /// Deleting caches, snapshots, store entries.
    Cleanup,
}

impl Phase {
    /// The default title.
    pub fn title(self) -> &'static str {
        match self {
            Self::Prepare => "Getting ready",
            Self::Fetch => "Downloading updates",
            Self::Checkout => "Unpacking sources",
            Self::Backup => "Backing up worlds",
            Self::Lock => "Resolving mods",
            Self::Toolchain => "Waking up the toolchain",
            Self::WaitForBuild => "Waiting for another build",
            Self::Compile => "Compiling",
            Self::Link => "Linking the game",
            Self::Install => "Installing the build",
            Self::Launch => "Launching",
            Self::Cleanup => "Cleaning up",
        }
    }
}

/// Where a log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogStream {
    /// The launcher itself.
    Launcher,
    /// A tool (git, cargo, rustc, nix).
    Tool,
}

/// How a phase ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PhaseOutcome {
    /// Did its work.
    Ok,
    /// Nothing to do: the result was already there (cached build, up-to-date mirror).
    Cached,
    /// Not needed for this run.
    Skipped,
    /// Failed (the op finishes with [`OpOutcome::Failed`]).
    Failed,
}

/// How an op ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum OpOutcome {
    /// Done.
    Ok {
        /// One line for the user ("Updated to main @ 1a2b3c4, 3 mods changed").
        summary: String,
    },
    /// Stopped on request.
    Cancelled,
    /// Failed.
    Failed {
        /// The phase it failed in.
        #[serde(default)]
        phase: Option<Phase>,
        /// The error with its causes, one per line.
        error: String,
        /// What the front end can offer to fix it.
        #[serde(default)]
        recovery: Vec<Recovery>,
    },
}

/// A follow-up the front end can offer after a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recovery {
    /// Run the op again.
    Retry,
    /// Drop the current lock and resolve from scratch (a mod changed without a version bump).
    ResetLock,
    /// Return to the previous working generation.
    Rollback,
    /// Play the last successful build while this is sorted out.
    PlayLastGood,
    /// Rebuild from scratch (`--force`).
    ForceRebuild,
    /// The toolchain or system libraries are missing: show the checks.
    Doctor,
}

/// State an op changed.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Change {
    /// `pwc.lock` was rewritten.
    Lock {
        /// Packages added, removed or changed.
        changes: Vec<PackageChange>,
        /// The new environment hash.
        environment: String,
    },
    /// A managed instance moved to new revisions.
    Pin {
        /// Before (None for a new instance).
        #[serde(default)]
        old: Option<crate::meta::Pin>,
        /// After.
        new: crate::meta::Pin,
        /// What changed in each repository, newest first.
        #[serde(default)]
        whats_new: Vec<RepoLog>,
    },
    /// A build finished or was found in the cache.
    Build {
        /// Build ID.
        build_id: String,
        /// The cache answered.
        cached: bool,
        /// The game executable.
        game: PathBuf,
    },
    /// A generation was recorded or activated.
    Generation {
        /// Its number.
        id: u64,
    },
    /// An instance was created, renamed or removed.
    Instance {
        /// Its name (directory name).
        name: String,
        /// Whether it exists now.
        exists: bool,
    },
    /// The game was started.
    Launched {
        /// Its process id.
        pid: u32,
        /// Its log file.
        log: PathBuf,
    },
    /// Updates are available (from `check-updates`).
    UpdatesAvailable {
        /// The instance.
        instance: String,
        /// Commits waiting per repository.
        whats_new: Vec<RepoLog>,
    },
}

/// One package's change in a lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    /// Package id.
    pub id: String,
    /// Old version (None = added).
    #[serde(default)]
    pub old: Option<String>,
    /// New version (None = removed).
    #[serde(default)]
    pub new: Option<String>,
}

/// Commits between two revisions of one repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoLog {
    /// Which repository.
    pub repo: crate::paths::Repo,
    /// Newest first.
    pub commits: Vec<Commit>,
    /// More commits exist than listed.
    #[serde(default)]
    pub truncated: bool,
}

/// One commit, for "what's new".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// Full hash.
    pub sha: String,
    /// First line of the message.
    pub summary: String,
    /// Author name.
    pub author: String,
    /// Commit time, Unix seconds.
    pub time: i64,
}

/// Where events go.
pub trait Sink: Send + Sync {
    /// Deliver one envelope. Must not panic; a broken pipe is ignored.
    fn emit(&self, envelope: &Envelope);
}

/// JSON lines on stdout: the child-process protocol.
pub struct JsonLinesSink;

impl Sink for JsonLinesSink {
    fn emit(&self, envelope: &Envelope) {
        use std::io::Write as _;
        if let Ok(line) = serde_json::to_string(envelope) {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// The readable form of an envelope, as `watt-start op` prints it and op log files hold it.
/// `None` for events that are not worth a line (progress ticks, phase ends).
pub fn human_line(envelope: &Envelope) -> Option<String> {
    let text = match &envelope.event {
        Event::OpStarted { op, instance, .. } => match instance {
            Some(name) => format!("==> {} `{name}`", op.verb()),
            None => format!("==> {}", op.verb()),
        },
        Event::PhaseStarted { title, .. } => format!("  -> {title}"),
        Event::Progress { .. } | Event::PhaseFinished { .. } => return None,
        Event::Log { line, .. } => format!("     {line}"),
        Event::Warning { message } => format!("warning: {message}"),
        Event::Changed { change } => match change {
            Change::Lock { changes, .. } if !changes.is_empty() => changes
                .iter()
                .map(|c| match (&c.old, &c.new) {
                    (None, Some(new)) => format!("  + {} {new}", c.id),
                    (Some(old), None) => format!("  - {} {old}", c.id),
                    (Some(old), Some(new)) => format!("  ~ {} {old} -> {new}", c.id),
                    (None, None) => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Change::Pin { old, new, .. } => match old {
                Some(old) if old != new => format!("  sources {} -> {}", old.label(), new.label()),
                Some(_) => return None,
                None => format!("  sources {}", new.label()),
            },
            Change::Build { game, cached, .. } => {
                format!(
                    "  {} {}",
                    if *cached { "cached" } else { "built" },
                    game.display()
                )
            }
            Change::Launched { pid, log } => {
                format!("  game started (pid {pid}, log {})", log.display())
            }
            Change::UpdatesAvailable {
                instance,
                whats_new,
            } => {
                let n: usize = whats_new.iter().map(|r| r.commits.len()).sum();
                format!("  `{instance}`: {n} new commits")
            }
            _ => return None,
        },
        Event::OpFinished {
            outcome,
            duration_ms,
        } => match outcome {
            OpOutcome::Ok { summary } => {
                format!("==> {summary} ({:.1} s)", *duration_ms as f64 / 1000.0)
            }
            OpOutcome::Cancelled => "==> cancelled".to_owned(),
            OpOutcome::Failed { error, .. } => format!("error: {error}"),
        },
    };
    (!text.is_empty()).then_some(text)
}

/// Readable lines on stderr, for people running `watt-start op ...` by hand.
pub struct HumanSink;

impl Sink for HumanSink {
    fn emit(&self, envelope: &Envelope) {
        use std::io::Write as _;
        if let Some(line) = human_line(envelope) {
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
    }
}

/// Readable lines appended to an op's log file.
pub struct FileSink(pub Mutex<std::fs::File>);

impl FileSink {
    /// Create (append to) the file, creating its directory.
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self(Mutex::new(file)))
    }
}

impl Sink for FileSink {
    fn emit(&self, envelope: &Envelope) {
        use std::io::Write as _;
        if let Some(line) = human_line(envelope) {
            let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(file, "[{:>8.1}s] {line}", envelope.t_ms as f64 / 1000.0);
        }
    }
}

/// Collects envelopes in memory (tests, in-process front ends).
#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<Envelope>>);

impl Sink for VecSink {
    fn emit(&self, envelope: &Envelope) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(envelope.clone());
    }
}

/// Fans out to several sinks (the protocol sink plus the op's log file).
pub struct TeeSink(pub Vec<Arc<dyn Sink>>);

impl Sink for TeeSink {
    fn emit(&self, envelope: &Envelope) {
        for sink in &self.0 {
            sink.emit(envelope);
        }
    }
}

/// A cooperative cancellation flag. Child-process ops are cancelled by killing their process
/// group instead; this flag is for in-process callers and is checked between steps.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Ask the op to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether stopping was asked for.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The error an op returns when it noticed [`CancelToken::is_cancelled`].
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

struct ReporterInner {
    sink: Arc<dyn Sink>,
    op_id: String,
    started: Instant,
    seq: AtomicU64,
    /// The running phase and when it started.
    phase: Mutex<Option<(Phase, Instant)>>,
    cancel: CancelToken,
}

/// How an op reports: numbered, timed envelopes, phase bookkeeping, cancellation. Cheap to clone
/// and shareable across threads (the stderr reader emits tool output through a clone).
#[derive(Clone)]
pub struct Reporter(Arc<ReporterInner>);

impl Reporter {
    /// A reporter for a new op run.
    pub fn new(sink: Arc<dyn Sink>, op_id: impl Into<String>, cancel: CancelToken) -> Self {
        Self(Arc::new(ReporterInner {
            sink,
            op_id: op_id.into(),
            started: Instant::now(),
            seq: AtomicU64::new(0),
            phase: Mutex::new(None),
            cancel,
        }))
    }

    /// The op id.
    pub fn op_id(&self) -> &str {
        &self.0.op_id
    }

    /// Emit an event.
    pub fn emit(&self, event: Event) {
        let envelope = Envelope {
            protocol: PROTOCOL,
            op_id: self.0.op_id.clone(),
            seq: self.0.seq.fetch_add(1, Ordering::SeqCst),
            t_ms: self.0.started.elapsed().as_millis() as u64,
            event,
        };
        self.0.sink.emit(&envelope);
    }

    /// Finish the running phase (if any) as `Ok`, then start `phase` with its default title.
    pub fn phase(&self, phase: Phase, determinate: bool) {
        self.phase_titled(phase, phase.title(), determinate);
    }

    /// [`Reporter::phase`] with a custom title.
    pub fn phase_titled(&self, phase: Phase, title: impl Into<String>, determinate: bool) {
        self.finish_phase(PhaseOutcome::Ok);
        *self.0.phase.lock().unwrap_or_else(|e| e.into_inner()) = Some((phase, Instant::now()));
        self.emit(Event::PhaseStarted {
            phase,
            title: title.into(),
            determinate,
        });
    }

    /// Finish the running phase, if any.
    pub fn finish_phase(&self, outcome: PhaseOutcome) {
        let running = self
            .0
            .phase
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some((phase, since)) = running {
            self.emit(Event::PhaseFinished {
                phase,
                outcome,
                duration_ms: since.elapsed().as_millis() as u64,
            });
        }
    }

    /// The running phase.
    pub fn current_phase(&self) -> Option<Phase> {
        self.0
            .phase
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|(p, _)| p)
    }

    /// Mark `phase` skipped (it never started).
    pub fn skip(&self, phase: Phase) {
        self.emit(Event::PhaseFinished {
            phase,
            outcome: PhaseOutcome::Skipped,
            duration_ms: 0,
        });
    }

    /// Progress in the running phase (dropped when no phase runs).
    pub fn progress(&self, done: u64, total: Option<u64>, detail: impl Into<String>) {
        if let Some(phase) = self.current_phase() {
            self.emit(Event::Progress {
                phase,
                done,
                total,
                detail: detail.into(),
            });
        }
    }

    /// A launcher log line.
    pub fn log(&self, line: impl Into<String>) {
        self.emit(Event::Log {
            stream: LogStream::Launcher,
            line: line.into(),
        });
    }

    /// A tool output line.
    pub fn tool_log(&self, line: impl Into<String>) {
        self.emit(Event::Log {
            stream: LogStream::Tool,
            line: line.into(),
        });
    }

    /// A warning.
    pub fn warn(&self, message: impl Into<String>) {
        self.emit(Event::Warning {
            message: message.into(),
        });
    }

    /// A state change.
    pub fn changed(&self, change: Change) {
        self.emit(Event::Changed { change });
    }

    /// Milliseconds since the op started.
    pub fn elapsed_ms(&self) -> u64 {
        self.0.started.elapsed().as_millis() as u64
    }

    /// The cancellation flag.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.0.cancel
    }

    /// `Err(Cancelled)` when cancellation was asked for. Call between steps.
    pub fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.0.cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        Ok(())
    }
}

/// A new op id: `<unix seconds>-<pid>-<counter>`, sortable and unique on one machine.
pub fn new_op_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{secs}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_round_trip_as_json_lines() {
        let envelope = Envelope {
            protocol: PROTOCOL,
            op_id: "1-2-3".into(),
            seq: 7,
            t_ms: 1234,
            event: Event::OpFinished {
                outcome: OpOutcome::Failed {
                    phase: Some(Phase::Lock),
                    error: "boom\ncause: x".into(),
                    recovery: vec![Recovery::ResetLock, Recovery::PlayLastGood],
                },
                duration_ms: 99,
            },
        };
        let line = serde_json::to_string(&envelope).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains(r#""type":"op-finished""#), "{line}");
        assert!(line.contains(r#""result":"failed""#), "{line}");
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back, envelope);
    }

    #[test]
    fn reporter_numbers_and_closes_phases() {
        let sink = Arc::new(VecSink::default());
        let r = Reporter::new(sink.clone(), "op", CancelToken::default());
        r.phase(Phase::Fetch, false);
        r.progress(1, Some(2), "x");
        r.phase(Phase::Lock, false);
        r.finish_phase(PhaseOutcome::Cached);
        let events = sink.0.lock().unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [0, 1, 2, 3, 4]);
        assert!(matches!(
            events[2].event,
            Event::PhaseFinished {
                phase: Phase::Fetch,
                outcome: PhaseOutcome::Ok,
                ..
            }
        ));
        assert!(matches!(
            events[4].event,
            Event::PhaseFinished {
                phase: Phase::Lock,
                outcome: PhaseOutcome::Cached,
                ..
            }
        ));
    }
}
