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
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("Building [") {
        return parse_building(rest).unwrap_or(CargoLine::Other);
    }
    if let Some(rest) = line.strip_prefix("Compiling ") {
        let mut words = rest.split_whitespace();
        if let (Some(name), Some(version)) = (words.next(), words.next())
            && let Some(version) = version.strip_prefix('v')
        {
            return CargoLine::Compiling {
                name: name.to_owned(),
                version: version.to_owned(),
            };
        }
        return CargoLine::Other;
    }
    if line.starts_with("Finished ") {
        return CargoLine::Finished;
    }
    if line.starts_with("error:") || line.starts_with("error[") {
        return CargoLine::Error(line.to_owned());
    }
    if line.starts_with("warning:") {
        return CargoLine::Warning(line.to_owned());
    }
    CargoLine::Other
}

/// The part of a `Building [` line after the bracket: `===>   ] 120/437: a, b`.
fn parse_building(rest: &str) -> Option<CargoLine> {
    let (_bar, after) = rest.split_once(']')?;
    let after = after.trim_start();
    let (counts, names) = match after.split_once(':') {
        Some((counts, names)) => (counts, names.trim()),
        None => (after, ""),
    };
    let (done, total) = counts.trim().split_once('/')?;
    let active = if names.is_empty() {
        Vec::new()
    } else {
        names
            .split(", ")
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect()
    };
    Some(CargoLine::Progress {
        done: done.trim().parse().ok()?,
        total: total.trim().parse().ok()?,
        active,
    })
}

/// Parse one git progress line, e.g. `Receiving objects:  45% (123/456), 1.20 MiB | 2.00 MiB/s`,
/// `Resolving deltas: 100% (10/10), done.`, `remote: Counting objects:  12% (5/40)`,
/// `Updating files:  33% (100/300)`. `None` for anything else.
pub fn parse_git_progress(line: &str) -> Option<GitProgress> {
    let line = line.trim();
    let line = line.strip_prefix("remote:").map_or(line, str::trim_start);
    let (stage, rest) = line.split_once(':')?;
    let stage = stage.trim();
    if stage.is_empty() {
        return None;
    }
    let rest = rest.trim_start();
    let (percent, after) = rest.split_once('%')?;
    let percent: u8 = percent.trim().parse().ok().filter(|p| *p <= 100)?;
    let counts = after
        .trim_start()
        .strip_prefix('(')
        .and_then(|s| s.split_once(')'))
        .and_then(|(inside, _)| inside.split_once('/'))
        .and_then(|(d, t)| Some((d.trim().parse().ok()?, t.trim().parse().ok()?)));
    Some(GitProgress {
        stage: stage.to_owned(),
        percent,
        done: counts.map(|(d, _)| d),
        total: counts.map(|(_, t)| t),
    })
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final-byte`, OSC `ESC ] ... BEL|ESC \`, and lone
/// two-byte `ESC x` sequences).
pub fn strip_ansi(text: &str) -> String {
    if !text.contains('\x1b') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        // `ESC \` ends the sequence; consume the backslash.
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The longest line [`LineSplitter`] holds before emitting it in pieces.
const MAX_LINE: usize = 64 * 1024;

/// Splits a byte stream into lines on `\n`, `\r\n` and lone `\r`; strips ANSI escapes and trailing
/// whitespace; drops empty lines; decodes invalid UTF-8 lossily. Holds an unfinished line until
/// more bytes or [`LineSplitter::finish`] arrive. A line longer than 64 KiB is emitted in pieces.
#[derive(Default)]
pub struct LineSplitter {
    pending: Vec<u8>,
    /// The previous chunk ended in `\r`: a `\n` starting the next chunk belongs to it.
    skip_lf: bool,
}

impl LineSplitter {
    /// A new splitter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the lines they completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut bytes = bytes;
        if self.skip_lf {
            self.skip_lf = false;
            if let Some(rest) = bytes.strip_prefix(b"\n") {
                bytes = rest;
            }
        }
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => self.emit(&mut lines),
                b'\r' => {
                    self.emit(&mut lines);
                    match bytes.get(i + 1) {
                        Some(b'\n') => i += 1,
                        Some(_) => {}
                        None => self.skip_lf = true,
                    }
                }
                byte => {
                    self.pending.push(byte);
                    if self.pending.len() >= MAX_LINE {
                        self.emit(&mut lines);
                    }
                }
            }
            i += 1;
        }
        lines
    }

    /// The last unfinished line, if any.
    pub fn finish(&mut self) -> Option<String> {
        self.skip_lf = false;
        let mut lines = Vec::new();
        self.emit(&mut lines);
        lines.pop()
    }

    /// Turn the pending bytes into a cleaned line (if not empty after cleaning) and clear them.
    fn emit(&mut self, lines: &mut Vec<String>) {
        if self.pending.is_empty() {
            return;
        }
        let line = strip_ansi(&String::from_utf8_lossy(&self.pending));
        self.pending.clear();
        let line = line.trim_end();
        if !line.is_empty() {
            lines.push(line.to_owned());
        }
    }
}

/// Whether a Cargo progress line's in-flight crates mean the build is in its final link: only
/// the generated instance crate (`pwc-game-instance`, shown as `pwc-game-instance(bin)` or
/// `pwc-game-instance(bin "pwc-game")`) or the game library (`project_watt_cubed`) is still
/// building, and at most one unit remains.
pub fn is_final_link(done: u64, total: u64, active: &[String]) -> bool {
    total > 0
        && total.saturating_sub(done) <= 1
        && !active.is_empty()
        && active.iter().all(|unit| {
            let name = unit.split('(').next().unwrap_or(unit).trim();
            name == "pwc-game-instance" || name == "project_watt_cubed"
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(line: &str) -> (u64, u64, Vec<String>) {
        match parse_cargo_line(line) {
            CargoLine::Progress {
                done,
                total,
                active,
            } => (done, total, active),
            other => panic!("{line:?}: {other:?}"),
        }
    }

    #[test]
    fn cargo_progress_lines() {
        assert_eq!(
            progress("    Building [=====>      ] 120/437: a, b(build.rs), c"),
            (120, 437, vec!["a".into(), "b(build.rs)".into(), "c".into()])
        );
        assert_eq!(
            progress("    Building [                            ] 0/24: proc-macro2(build.rs)"),
            (0, 24, vec!["proc-macro2(build.rs)".into()])
        );
        // Cut off at the terminal width.
        assert_eq!(
            progress("    Building [==>  ] 2/24: memchr, proc-macro2(build.rs), serde_core(bu"),
            (
                2,
                24,
                vec![
                    "memchr".into(),
                    "proc-macro2(build.rs)".into(),
                    "serde_core(bu".into()
                ]
            )
        );
        assert_eq!(progress("Building [=>] 5/9"), (5, 9, vec![]));
        assert_eq!(parse_cargo_line("Building [=> 5/9"), CargoLine::Other);
    }

    #[test]
    fn cargo_other_lines() {
        assert_eq!(
            parse_cargo_line("   Compiling serde v1.0.229"),
            CargoLine::Compiling {
                name: "serde".into(),
                version: "1.0.229".into()
            }
        );
        assert_eq!(
            parse_cargo_line("   Compiling voxel_engine v0.3.0 (/path/to/voxel-engine)"),
            CargoLine::Compiling {
                name: "voxel_engine".into(),
                version: "0.3.0".into()
            }
        );
        assert_eq!(
            parse_cargo_line("    Finished `release` profile [optimized] target(s) in 3m 01s"),
            CargoLine::Finished
        );
        assert_eq!(
            parse_cargo_line("error[E0425]: cannot find value"),
            CargoLine::Error("error[E0425]: cannot find value".into())
        );
        assert_eq!(
            parse_cargo_line("error: could not compile `x`"),
            CargoLine::Error("error: could not compile `x`".into())
        );
        assert_eq!(
            parse_cargo_line("warning: unused variable"),
            CargoLine::Warning("warning: unused variable".into())
        );
        for garbage in ["", "Compiling", "Building", "ü€ [ 1/", &"x".repeat(100_000)] {
            assert_eq!(parse_cargo_line(garbage), CargoLine::Other);
        }
    }

    #[test]
    fn git_progress_lines() {
        assert_eq!(
            parse_git_progress("Receiving objects:  45% (123/456), 1.20 MiB | 2.00 MiB/s"),
            Some(GitProgress {
                stage: "Receiving objects".into(),
                percent: 45,
                done: Some(123),
                total: Some(456)
            })
        );
        let p = parse_git_progress("remote: Counting objects:  12% (5/40)").unwrap();
        assert_eq!((p.stage.as_str(), p.percent), ("Counting objects", 12));
        assert_eq!(
            parse_git_progress("Resolving deltas: 100% (10/10), done.").map(|p| p.percent),
            Some(100)
        );
        let p = parse_git_progress("Updating files:  33%").unwrap();
        assert_eq!((p.percent, p.done, p.total), (33, None, None));
        assert_eq!(parse_git_progress("Counting objects: 7, done."), None);
        assert_eq!(parse_git_progress("Receiving objects: 150% (1/2)"), None);
        assert_eq!(parse_git_progress("fatal: not a git repository"), None);
    }

    #[test]
    fn ansi_is_removed() {
        assert_eq!(
            strip_ansi("\x1b[1m\x1b[32m   Compiling\x1b[0m foo v1.0.0"),
            "   Compiling foo v1.0.0"
        );
        assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
        assert_eq!(strip_ansi("a\x1b]8;;http://x\x1b\\b"), "ab");
        assert_eq!(strip_ansi("a\x1b=b ünï"), "ab ünï");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn lines_split_on_every_terminator() {
        let mut s = LineSplitter::new();
        assert_eq!(s.feed(b"one\ntwo\r\nthree\rfour"), ["one", "two", "three"]);
        assert_eq!(s.feed(b" more\r"), ["four more"]);
        // The `\n` of a `\r\n` split across chunks is not an extra line.
        assert_eq!(s.feed(b"\nfive  \n\n\x1b[32m\x1b[0m\n"), ["five"]);
        assert_eq!(s.feed(b"six"), Vec::<String>::new());
        assert_eq!(s.finish().as_deref(), Some("six"));
        assert_eq!(s.finish(), None);
        assert_eq!(s.feed(b"\xffok\n"), ["\u{fffd}ok"]);
    }

    #[test]
    fn long_lines_come_in_pieces() {
        let mut s = LineSplitter::new();
        let lines = s.feed(&vec![b'x'; MAX_LINE + 10]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE);
        assert_eq!(s.finish().map(|l| l.len()), Some(10));
    }

    #[test]
    fn final_link_detection() {
        let units = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert!(is_final_link(
            436,
            437,
            &units(&["pwc-game-instance(bin \"pwc-game\")"])
        ));
        assert!(is_final_link(436, 437, &units(&["pwc-game-instance(bin)"])));
        assert!(is_final_link(437, 437, &units(&["project_watt_cubed"])));
        assert!(!is_final_link(436, 437, &units(&["voxel_engine"])));
        assert!(!is_final_link(
            430,
            437,
            &units(&["pwc-game-instance(bin)"])
        ));
        assert!(!is_final_link(436, 437, &[]));
        assert!(!is_final_link(0, 0, &units(&["project_watt_cubed"])));
        assert!(!is_final_link(500, 437, &units(&["x"])));
    }
}
