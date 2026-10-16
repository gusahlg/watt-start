//! Parsing tool output into progress: Cargo's status lines and git's `--progress` lines.
//!
//! Tools that think they print to a terminal redraw one line with `\r`; Cargo is forced to print
//! its progress bar into a pipe with `CARGO_TERM_PROGRESS_WHEN=always`. [`LineSplitter`] turns the
//! raw byte stream into lines (splitting on `\n` and `\r`, removing ANSI escapes), and the parsers
//! classify each line. Progress output is presentation, not a protocol: everything here must
//! tolerate garbage and unknown lines (they are [`CargoLine::Other`] / `None`).

use crate::git::GitProgress;

/// A classified Cargo stderr line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CargoLine {
    /// `    Building [=====>      ] 120/437: a, b(build.rs), c`
    Progress {
        /// Units finished.
        done: u64,
        /// Units in total.
        total: u64,
        /// The crates in flight, as listed (may end truncated, without a closing name).
        active: Vec<String>,
    },
    /// `   Compiling foo v1.2.3` or `   Compiling foo v1.2.3 (/path)`.
    Compiling {
        /// Crate name.
        name: String,
        /// Version without the `v`.
        version: String,
    },
    /// `    Finished `release` profile [optimized] target(s) in 3m 01s`.
    Finished,
    /// `error: ...` or `error[E0425]: ...`.
    Error(String),
    /// `warning: ...`.
    Warning(String),
    /// Anything else.
    Other,
}

/// Classify one Cargo stderr line (already split and stripped of ANSI escapes).
pub fn parse_cargo_line(line: &str) -> CargoLine {
    let _ = line;
    todo!("qwen")
}

/// Parse one git progress line, e.g. `Receiving objects:  45% (123/456), 1.20 MiB | 2.00 MiB/s`,
/// `Resolving deltas: 100% (10/10), done.`, `remote: Counting objects:  12% (5/40)`,
/// `Updating files:  33% (100/300)`. `None` for anything else.
pub fn parse_git_progress(line: &str) -> Option<GitProgress> {
    let _ = line;
    todo!("qwen")
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final-byte`, OSC `ESC ] ... BEL|ESC \`, and lone
/// two-byte `ESC x` sequences).
pub fn strip_ansi(text: &str) -> String {
    let _ = text;
    todo!("qwen")
}

/// Splits a byte stream into lines on `\n`, `\r\n` and lone `\r`; strips ANSI escapes and trailing
/// whitespace; drops empty lines; decodes invalid UTF-8 lossily. Holds an unfinished line until
/// more bytes or [`LineSplitter::finish`] arrive. A line longer than 64 KiB is emitted in pieces.
#[derive(Default)]
pub struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    /// A new splitter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the lines they completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let _ = (bytes, &mut self.pending);
        todo!("qwen")
    }

    /// The last unfinished line, if any.
    pub fn finish(&mut self) -> Option<String> {
        todo!("qwen")
    }
}

/// Whether a Cargo progress line's in-flight crates mean the build is in its final link: only
/// the generated instance crate (`pwc-game-instance`, shown as `pwc-game-instance(bin)` or
/// `pwc-game-instance(bin "pwc-game")`) or the game library (`project_watt_cubed`) is still
/// building, and at most one unit remains.
pub fn is_final_link(done: u64, total: u64, active: &[String]) -> bool {
    let _ = (done, total, active);
    todo!("qwen")
}
