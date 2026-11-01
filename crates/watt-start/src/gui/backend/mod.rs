//! [`Backend`]: all data access and actions the GUI needs, so screens never touch `watt-core`
//! or the filesystem directly. [`RealBackend`] wraps `watt-core`; [`DemoBackend`] fakes
//! realistic data and simulated ops, selected by `WATT_START_DEMO=1` (see [`crate::gui::run`]).

mod demo;
mod real;
mod update;

use std::io::{Read as _, Seek as _};
use std::path::Path;
use std::sync::Arc;

use watt_core::catalog::{AvailableMod, InstanceCard};
use watt_core::doctor::Check;
use watt_core::events::RepoLog;
use watt_core::git::BranchInfo;
use watt_core::meta::Meta;
use watt_core::ops::OpRequest;
use watt_core::selfupdate::UpdateStatus;
use watt_core::settings::Settings;
use watt_core::status::InstanceStatus;
use watt_core::view::OpView;

pub use demo::DemoBackend;
pub use real::RealBackend;

/// One instance as the catalog and status views together describe it.
#[derive(Clone, Debug)]
pub struct InstanceEntry {
    /// `catalog::InstanceCard`.
    pub card: InstanceCard,
    /// `status::compute` for this card.
    pub status: InstanceStatus,
}

/// The result of a background load: absent until requested, then loading, then settled.
#[derive(Clone, Debug, Default)]
pub enum Loaded<T> {
    /// Never requested.
    #[default]
    Idle,
    /// Requested, not back yet.
    Loading,
    /// Back.
    Ready(T),
    /// Failed.
    Err(String),
}

/// A presentation edit applied in-process through `meta::update` (everything else is an op).
#[derive(Clone, Debug)]
pub enum MetaEdit {
    /// Shown name (empty clears it, falling back to the directory name).
    DisplayName(String),
    /// `#rrggbb`.
    Color(String),
    /// Sidebar group (empty clears it).
    Group(String),
    /// Pinned to the top of the sidebar.
    Favorite(bool),
    /// Free text, saved on focus loss.
    Notes(String),
}

/// A running (or finished) op, abstracting over a real child process and a simulated one.
pub enum RunningOp {
    /// `watt-core::client::OpHandle`.
    Real(watt_core::client::OpHandle),
    /// A timed sequence of envelopes, for the demo backend.
    Demo(demo::DemoOp),
}

impl RunningOp {
    /// Apply new envelopes to `view`; see `OpHandle::poll`.
    pub fn poll(&mut self, view: &mut OpView) -> bool {
        match self {
            RunningOp::Real(h) => h.poll(view),
            RunningOp::Demo(d) => d.poll(view),
        }
    }

    /// Ask the op to stop.
    pub fn cancel(&mut self) {
        match self {
            RunningOp::Real(h) => h.cancel(),
            RunningOp::Demo(d) => d.cancel(),
        }
    }

    /// Hand the UI waker to the op. Real reader threads use it; demo ops call it when an
    /// event falls due.
    pub fn set_waker(&mut self, waker: watt_core::client::OpWaker) {
        match self {
            RunningOp::Real(h) => h.set_waker(waker),
            RunningOp::Demo(d) => d.set_waker(waker),
        }
    }

    /// A real op with a waker wakes the UI from its reader. Demo ops are clock-driven and
    /// need the UI to poll until the next event is due.
    pub fn wakes_on_io(&self) -> bool {
        match self {
            RunningOp::Real(h) => h.has_waker(),
            RunningOp::Demo(_) => false,
        }
    }

    /// Whether [`RunningOp::set_waker`] has already been called.
    pub fn has_waker(&self) -> bool {
        match self {
            RunningOp::Real(h) => h.has_waker(),
            RunningOp::Demo(d) => d.has_waker(),
        }
    }
}

/// Last 400 lines of an op log, reading at most the trailing 128 KiB off disk.
///
/// A read that starts mid-file drops the first (partial) line. A missing file is an empty tail.
pub(crate) fn read_log_tail(path: &Path) -> Arc<Vec<String>> {
    const TAIL_BYTES: u64 = 128 * 1024;
    const LINES: usize = 400;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Arc::new(Vec::new());
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Arc::new(Vec::new());
    };
    let start = len.saturating_sub(TAIL_BYTES);
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).is_err() {
        return Arc::new(Vec::new());
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Arc::new(Vec::new());
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > LINES {
        let drop_n = lines.len() - LINES;
        lines.drain(0..drop_n);
    }
    Arc::new(lines)
}

/// Everything the GUI needs from the launcher's engine (or a fake of it).
pub trait Backend {
    /// The current settings.
    fn settings(&self) -> &Settings;

    /// Replace the settings and persist them.
    fn save_settings(&mut self, settings: Settings);

    /// Drain any background results; call once per frame. Returns whether anything changed
    /// (a repaint is worth it).
    fn tick(&mut self) -> bool;

    /// The instances, as of the last [`Backend::refresh`].
    ///
    /// `Arc` so a frame can keep one entry (the sidebar, a shortcut) without cloning the card.
    fn instances(&self) -> &[Arc<InstanceEntry>];

    /// Instance directories that failed to load, with the reason.
    fn load_errors(&self) -> &[(String, String)];

    /// Updates known per instance, from the background `CheckUpdates` op (or a plain refresh).
    fn updates_available(&self, instance: &str) -> Option<&[RepoLog]>;

    /// Kick a background catalog + status rescan; results land through [`Backend::tick`].
    fn refresh(&mut self);

    /// Probe busy/running for every instance. A full [`Backend::refresh`] runs only when one
    /// of those flips. The live tick calls this; F5 still calls [`Backend::refresh`].
    fn refresh_liveness(&mut self) {}

    /// Whether the initial (or a later) catalog scan is still in flight.
    fn refresh_pending(&self) -> bool {
        false
    }

    /// Store the egui context so background jobs can wake the UI when they finish.
    /// The default does nothing (the demo backend has no jobs).
    fn set_ui_context(&mut self, _ctx: egui::Context) {}

    /// Apply a presentation edit now (blocking: this is a small, local file write).
    fn edit_meta(&mut self, instance: &str, edit: MetaEdit) -> Result<Meta, String>;

    /// The modules an instance could add (background-loaded).
    ///
    /// The `Arc` is cheap to clone. The modules tab should keep it rather than cloning the vecs.
    fn available_modules(
        &mut self,
        instance: &str,
    ) -> Loaded<Arc<(Vec<AvailableMod>, Vec<String>)>>;

    /// Drop the cached module catalogue after a modules/source/update/create op or a settings change.
    /// A live refresh must not call this. The default does nothing.
    fn invalidate_modules(&mut self) {}

    /// Trailing lines of an on-disk op log. The real backend loads them off the UI thread and
    /// keeps the previous tail visible while a reload runs.
    fn log_tail(&mut self, path: &Path) -> Loaded<Arc<Vec<String>>> {
        Loaded::Ready(read_log_tail(path))
    }

    /// Branches of the game mirror, for the channel picker (background-loaded).
    fn branches(&mut self) -> Loaded<Vec<BranchInfo>>;

    /// System checks (background-loaded; call [`Backend::run_doctor`] to (re)start one).
    fn doctor(&mut self) -> Loaded<Vec<Check>>;

    /// (Re)start the system checks in the background.
    fn run_doctor(&mut self);

    /// Start an op as a child process (or simulate one).
    fn start_op(&mut self, request: OpRequest) -> RunningOp;

    /// Open a path in the system file manager, best effort.
    fn open_path(&self, path: &Path);

    /// Whether background jobs are still out (the GUI keeps polling [`Backend::tick`] while
    /// they are, since their threads cannot wake it).
    fn loading(&self) -> bool {
        false
    }

    /// The launcher's own update check, as of the last [`Backend::tick`].
    fn launcher_update(&self) -> &UpdateStatus;

    /// Start a check. `force` ignores the 24 hour interval (the Settings "Check now" button).
    fn check_launcher_update(&mut self, force: bool);

    /// Download the executable for this target and replace this process's file.
    fn apply_launcher_update(&mut self);
}
