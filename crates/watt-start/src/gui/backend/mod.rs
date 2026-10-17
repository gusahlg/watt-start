//! [`Backend`]: all data access and actions the GUI needs, so screens never touch `watt-core`
//! or the filesystem directly. [`RealBackend`] wraps `watt-core`; [`DemoBackend`] fakes
//! realistic data and simulated ops, selected by `WATT_START_DEMO=1` (see [`crate::gui::run`]).

mod demo;
mod real;
mod update;

use std::path::Path;

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
    fn instances(&self) -> &[InstanceEntry];

    /// Instance directories that failed to load, with the reason.
    fn load_errors(&self) -> &[(String, String)];

    /// Updates known per instance, from the background `CheckUpdates` op (or a plain refresh).
    fn updates_available(&self, instance: &str) -> Option<&[RepoLog]>;

    /// Kick a background catalog + status rescan; results land through [`Backend::tick`].
    fn refresh(&mut self);

    /// Apply a presentation edit now (blocking: this is a small, local file write).
    fn edit_meta(&mut self, instance: &str, edit: MetaEdit) -> Result<Meta, String>;

    /// The mods an instance could add (background-loaded; `None` while idle/loading).
    fn available_mods(&mut self, instance: &str) -> Loaded<(Vec<AvailableMod>, Vec<String>)>;

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
