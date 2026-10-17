//! The application: state, routing and the top-level layout. [`App::ui`] is the single entry
//! point every screen is drawn from (used both by [`crate::gui::run`] and the snapshot tests).

mod dialogs;
mod instance_page;
mod instance_tabs;
mod mods_tab;
mod new_instance;
mod settings_page;
mod sidebar;
mod toasts;
mod welcome;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Panel};
use watt_core::client::OpWake;
use watt_core::events::{Change, OpKind, OpOutcome, RepoLog};
use watt_core::ops::{OpRequest, SourceSpec};
use watt_core::settings::Settings;
use watt_core::view::OpView;

use crate::gui::backend::{Backend, DemoBackend, RealBackend, RunningOp};
use crate::gui::forge_bridge::ForgeBridge;
use crate::gui::theme;

/// Which mod preset [`new_instance`] offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModPreset {
    Essentials,
    EssentialsDevToolkit,
    Vanilla,
}

impl ModPreset {
    fn specs(self) -> Vec<String> {
        match self {
            ModPreset::Essentials => vec!["pwc.essentials".to_owned()],
            ModPreset::EssentialsDevToolkit => {
                vec!["pwc.essentials".to_owned(), "pwc.dev-toolkit".to_owned()]
            }
            ModPreset::Vanilla => Vec::new(),
        }
    }
}

/// Which kind of source a dialog is configuring.
///
/// Managed and linked variants keep the binding fields the dialog does not edit
/// (`engine_ref`, `mods_ref`, `repositories`) so Apply can put them back unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceChoice {
    /// Follow a game branch. `engine_ref` and `mods_ref` are `auto` for a new instance.
    Managed {
        channel: String,
        engine_ref: String,
        mods_ref: String,
    },
    /// Build a checkout on this machine. `repositories` empty means pwc's list.
    Linked {
        path: String,
        repositories: Vec<PathBuf>,
    },
    /// Whatever checkout pwc itself is configured to use.
    PwcDefault,
}

impl SourceChoice {
    fn to_spec(&self) -> SourceSpec {
        match self {
            SourceChoice::Managed {
                channel,
                engine_ref,
                mods_ref,
            } => SourceSpec::Managed {
                channel: channel.clone(),
                engine_ref: engine_ref.clone(),
                mods_ref: mods_ref.clone(),
            },
            SourceChoice::Linked { path, repositories } => SourceSpec::Linked {
                pwc_source: PathBuf::from(path),
                repositories: repositories.clone(),
            },
            SourceChoice::PwcDefault => SourceSpec::PwcDefault,
        }
    }
}

/// The main area's current view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MainView {
    Instance(String),
    Settings,
}

/// Which tab of an instance page is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum InstanceTab {
    #[default]
    Overview,
    Mods,
    Builds,
    Log,
    InstanceSettings,
}

/// Mod changes staged in the Mods tab, not yet applied.
#[derive(Clone, Debug, Default)]
pub(crate) struct StagedMods {
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

impl StagedMods {
    pub(crate) fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

/// One op the GUI is tracking: either running or finished-but-not-yet-closed.
///
/// A slot is running until its view has an outcome (that includes the moments before
/// `op-started` arrives), then settled. Settlement (refresh, toast, harvesting changes) is
/// handled exactly once; the slot then stays until the user closes its result.
pub(crate) struct OpSlot {
    pub op: RunningOp,
    pub view: OpView,
    /// The instance page shows its tabs instead of the Forge; the header still shows progress.
    pub hidden: bool,
    /// What was asked: for Retry, and for which instances the op holds.
    pub request: OpRequest,
    /// When it started. A local clock: `view.t_ms` only moves when an event arrives.
    pub started: Instant,
    /// When the outcome arrived. Elapsed time stops here instead of keeping counting.
    pub settled_at: Option<Instant>,
    /// Settlement has been handled.
    pub settled: bool,
    /// What to call it before its instance is in the catalog (Create, Duplicate).
    pub title: String,
}

impl OpSlot {
    /// No outcome yet.
    pub fn running(&self) -> bool {
        self.view.running()
    }

    /// The requested op (known before `op-started` arrives, unlike `view.op`).
    pub fn kind(&self) -> OpKind {
        self.request.kind()
    }

    /// Finished successfully.
    pub fn succeeded(&self) -> bool {
        matches!(self.view.outcome, Some(OpOutcome::Ok { .. }))
    }

    /// Finished with an error (not cancelled).
    pub fn failed(&self) -> bool {
        matches!(self.view.outcome, Some(OpOutcome::Failed { .. }))
    }

    /// Whether this op works on `instance`: its key, the request's instance (the source of a
    /// duplicate) or the destination of a duplicate.
    fn holds(&self, key: &str, instance: &str) -> bool {
        key == instance
            || self.request.instance() == Some(instance)
            || matches!(&self.request, OpRequest::Duplicate { new_name, .. } if new_name == instance)
    }
}

/// A confirmation the user must type-to-confirm or just accept.
pub(crate) struct Confirm {
    pub title: String,
    pub body: String,
    pub type_to_confirm: Option<String>,
    pub typed: String,
    pub on_confirm: ConfirmAction,
}

#[derive(Clone, Debug)]
pub(crate) enum ConfirmAction {
    DeleteInstance(String),
    Maintenance(OpRequest),
}

/// The startup "check every instance" op, and how much of its report has been applied.
///
/// Changes are consumed once. An update that clears a badge while the check is still running
/// is recorded in `cleared` so a later frame of the same report cannot put the badge back.
pub(crate) struct StartupCheck {
    op: RunningOp,
    view: OpView,
    processed: usize,
    cleared: HashSet<String>,
}

/// The launcher application. A single [`App::ui`] call draws everything for the current state.
pub struct App {
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) forge: ForgeBridge,

    pub(crate) search: String,
    /// Set by Ctrl+F. The sidebar requests focus on its search field and clears this.
    pub(crate) focus_search: bool,
    pub(crate) main_view: Option<MainView>,
    pub(crate) instance_tab: InstanceTab,

    pub(crate) ops: HashMap<String, OpSlot>,
    pub(crate) staged_mods: HashMap<String, StagedMods>,
    pub(crate) updates: HashMap<String, Vec<RepoLog>>,
    pub(crate) startup_check: Option<StartupCheck>,
    /// First frame's clock. The startup update check waits for the catalog, or two seconds.
    first_frame: Option<Instant>,
    /// The startup update check has been started, or skipped because the setting is off.
    update_check_started: bool,
    /// When a dirty settings draft should be written. `None` while it matches what was saved.
    settings_save_at: Option<Instant>,
    /// One waker shared by every op, built on the first frame. `None` until then (tests that
    /// never draw poll ops themselves).
    op_waker: Option<watt_core::client::OpWaker>,
    /// Zoom this app last applied or adopted. `None` until the first frame, so the saved
    /// scale wins over egui's default of 1.0.
    zoom_applied: Option<f32>,
    /// When busy/running was last probed because a game or an external op was live.
    last_live_refresh: Option<Instant>,

    pub(crate) new_instance: Option<new_instance::NewInstanceState>,
    pub(crate) change_source: Option<new_instance::ChangeSourceState>,
    pub(crate) confirm: Option<Confirm>,
    /// Duplicate dialog, when open.
    pub(crate) duplicate: Option<dialogs::DuplicateState>,

    pub(crate) settings_draft: Settings,
    pub(crate) toasts: Vec<toasts::Toast>,

    /// Per-instance Mods tab: segment, filters, and which bundles are expanded.
    pub(crate) mods_ui: mods_tab::ModsTabState,

    pub(crate) notes_draft: HashMap<String, String>,
    /// In-progress display name, keyed by instance directory name.
    pub(crate) display_name_draft: HashMap<String, String>,
    /// In-progress sidebar group, keyed by instance directory name.
    pub(crate) group_draft: HashMap<String, String>,
    /// Log tails and which instance was last on screen (drafts commit when it changes).
    pub(crate) instance_ui: instance_tabs::InstanceUiState,
    pub(crate) toasted_load_errors: std::collections::HashSet<String>,
}

/// A settings edit inside the half-second save delay is written when the window closes.
impl Drop for App {
    fn drop(&mut self) {
        self.save_pending_settings();
    }
}

impl App {
    /// Build the app, picking the real or demo backend from `WATT_START_DEMO`.
    pub fn new() -> anyhow::Result<Self> {
        let backend: Box<dyn Backend> = if std::env::var("WATT_START_DEMO").as_deref() == Ok("1") {
            Box::new(DemoBackend::new())
        } else {
            Box::new(RealBackend::new()?)
        };
        Ok(Self::with_backend(backend))
    }

    /// Build the app around an explicit backend (tests use this with [`DemoBackend`]).
    pub(crate) fn with_backend(backend: Box<dyn Backend>) -> Self {
        let settings_draft = backend.settings().clone();
        let mut app = Self {
            backend,
            forge: ForgeBridge::real(),
            search: String::new(),
            focus_search: false,
            main_view: None,
            instance_tab: InstanceTab::Overview,
            ops: HashMap::new(),
            staged_mods: HashMap::new(),
            updates: HashMap::new(),
            startup_check: None,
            first_frame: None,
            update_check_started: false,
            settings_save_at: None,
            op_waker: None,
            zoom_applied: None,
            last_live_refresh: None,
            new_instance: None,
            change_source: None,
            confirm: None,
            duplicate: None,
            settings_draft,
            toasts: Vec::new(),
            mods_ui: mods_tab::ModsTabState::default(),
            notes_draft: HashMap::new(),
            display_name_draft: HashMap::new(),
            group_draft: HashMap::new(),
            instance_ui: instance_tabs::InstanceUiState::default(),
            toasted_load_errors: std::collections::HashSet::new(),
        };
        app.sync_updates_from_backend();
        // The update check waits until the first catalog lands (or two seconds), so it does not
        // compete with the opening scan. See [`App::maybe_start_update_check`].
        app.ensure_selection();
        app
    }

    // The rest of this block is test/screenshot-only: nothing in the real app calls these, so
    // they would otherwise warn as dead code in a plain `cargo build`/`cargo clippy -p
    // watt-start` (the `tests/gui_snapshots.rs` binary that does call them is a separate
    // compilation unit).
    #[allow(dead_code)]
    /// A demo-backed app (the real forge, like every other build). Used by the snapshot tests.
    pub(crate) fn for_snapshot_tests() -> Self {
        let mut app = Self::with_backend(Box::new(DemoBackend::new()));
        app.still_for_snapshots();
        app
    }

    #[allow(dead_code)]
    /// Like [`App::for_snapshot_tests`], with no instances (the welcome screen).
    pub(crate) fn for_snapshot_tests_empty() -> Self {
        let mut app = Self::with_backend(Box::new(DemoBackend::empty()));
        app.still_for_snapshots();
        app
    }

    /// Freeze motion so a screenshot is not taken mid-blend.
    ///
    /// Only the snapshot constructors call this. The demo backend stores the flag in memory;
    /// the forge view reads [`Self::settings_draft`].
    #[allow(dead_code)]
    fn still_for_snapshots(&mut self) {
        self.settings_draft.appearance.animations = false;
        let mut settings = self.backend.settings().clone();
        settings.appearance.animations = false;
        self.backend.save_settings(settings);
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show an instance's Overview tab.
    pub(crate) fn show_instance_overview(&mut self, name: &str) {
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = InstanceTab::Overview;
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show an instance's Mods tab.
    pub(crate) fn show_instance_mods(&mut self, name: &str) {
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = InstanceTab::Mods;
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show an instance's Builds tab.
    pub(crate) fn show_instance_builds(&mut self, name: &str) {
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = InstanceTab::Builds;
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: stage adding `id` to an instance's mods, without applying it.
    pub(crate) fn stage_mod_add_for_tests(&mut self, instance: &str, id: &str) {
        self.staged_mods
            .entry(instance.to_owned())
            .or_default()
            .add
            .push(id.to_owned());
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: stage removing `id` from an instance's mods, without applying it.
    pub(crate) fn stage_mod_remove_for_tests(&mut self, instance: &str, id: &str) {
        self.staged_mods
            .entry(instance.to_owned())
            .or_default()
            .remove
            .push(id.to_owned());
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: set the UI scale without going through the slider.
    pub(crate) fn set_scale_for_tests(&mut self, scale: f32) {
        self.settings_draft.appearance.scale = scale;
    }

    #[allow(dead_code)]
    /// Test helper: `"managed"`, `"linked"`, `"pwc"`, or `"closed"` for the New instance dialog.
    pub(crate) fn new_instance_source_kind_for_tests(&self) -> &'static str {
        match self.new_instance.as_ref().map(|state| &state.source) {
            Some(SourceChoice::Managed { .. }) => "managed",
            Some(SourceChoice::Linked { .. }) => "linked",
            Some(SourceChoice::PwcDefault) => "pwc",
            None => "closed",
        }
    }

    #[allow(dead_code)]
    /// Test helper: the New instance dialog is open.
    pub(crate) fn new_instance_open_for_tests(&self) -> bool {
        self.new_instance.is_some()
    }

    #[allow(dead_code)]
    /// Test helper: a Play op is tracked for `instance` (running or settled).
    pub(crate) fn has_play_op_for_tests(&self, instance: &str) -> bool {
        self.ops
            .get(instance)
            .is_some_and(|slot| slot.kind() == OpKind::Play)
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show the Settings page.
    pub(crate) fn show_settings(&mut self) {
        self.main_view = Some(MainView::Settings);
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: start an op on `instance` so the forge view is shown for it.
    pub(crate) fn start_op_for_tests(&mut self, instance: &str, request: OpRequest) {
        self.start_op(instance, request);
    }

    fn sync_updates_from_backend(&mut self) {
        for entry in self.backend.instances() {
            if let Some(whats_new) = self.backend.updates_available(&entry.card.name) {
                self.updates
                    .insert(entry.card.name.clone(), whats_new.to_vec());
            }
        }
        let mut new_toasts = Vec::new();
        for (name, error) in self.backend.load_errors() {
            let key = format!("{name}: {error}");
            if self.toasted_load_errors.insert(key.clone()) {
                new_toasts.push(format!("{name}: {error}"));
            }
        }
        for message in new_toasts {
            self.toast(message);
        }
    }

    /// Draw everything. Called every frame by [`crate::gui::run`] and by the snapshot tests.
    /// Never land on "Pick an instance" when there is one to show: prefer pwc's active instance,
    /// then a favourite, then whichever sorts first. Runs every frame because the real backend
    /// loads the catalog in the background, after the window opened.
    fn ensure_selection(&mut self) {
        if self.main_view.is_some() {
            return;
        }
        let instances = self.backend.instances();
        let initial = instances
            .iter()
            .find(|e| e.card.active)
            .or_else(|| instances.iter().find(|e| e.card.meta.favorite))
            .or_else(|| instances.first())
            .map(|e| e.card.name.clone());
        if let Some(name) = initial {
            self.main_view = Some(MainView::Instance(name));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.sync_zoom(ui.ctx());
        theme::apply(
            ui.ctx(),
            self.settings_draft.appearance.dark,
            self.settings_draft.appearance.animations,
        );
        self.ensure_wake(ui.ctx());

        if self.backend.tick() {
            self.ensure_selection();
            self.sync_updates_from_backend();
        }
        self.poll_ops(ui.ctx());
        self.poll_startup_check(ui.ctx());
        self.refresh_live_status(ui.ctx());
        // After tick, so a catalog that landed this frame counts. Before the panels, so the
        // brand spinner is on screen the same frame the check starts.
        self.maybe_start_update_check(ui.ctx());
        self.flush_settings();
        self.handle_shortcuts(ui.ctx());

        let no_catalog = self.backend.instances().is_empty() && !self.has_live_instance_op();
        let settings_open = matches!(self.main_view, Some(MainView::Settings));
        if no_catalog && self.new_instance.is_none() && !settings_open {
            if self.backend.loading() {
                self.catalog_loading(ui);
            } else {
                self.welcome_screen(ui);
            }
        } else {
            let palette = theme::palette(ui.ctx());
            let sidebar_w = (ui.ctx().content_rect().width() * 0.28).clamp(200.0, 260.0);
            Panel::left("sidebar")
                .exact_size(sidebar_w)
                .resizable(false)
                .frame(egui::Frame::new().fill(palette.bg_sidebar).inner_margin(0))
                .show_separator_line(false)
                .show(ui, |ui| self.sidebar(ui));
            CentralPanel::default()
                .frame(egui::Frame::new().fill(palette.bg_surface).inner_margin(0))
                .show(ui, |ui| self.main_area(ui));
        }

        self.new_instance_dialog(ui.ctx());
        self.change_source_dialog(ui.ctx());
        self.duplicate_dialog(ui.ctx());
        self.confirm_dialog(ui.ctx());
        self.draw_toasts(ui.ctx());
    }

    fn main_area(&mut self, ui: &mut egui::Ui) {
        // Before the panels draw, so Ctrl+, (handled above) still commits the instance we left.
        self.note_view_changed();
        match self.main_view.clone() {
            Some(MainView::Settings) => self.settings_page(ui),
            // An instance being created or duplicated has an op but no catalog entry yet.
            Some(MainView::Instance(name))
                if self.backend.instances().iter().any(|e| e.card.name == name)
                    || self.ops.contains_key(&name) =>
            {
                self.instance_page(ui, &name);
            }
            _ => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    let p = theme::palette(ui.ctx());
                    ui.label(
                        egui::RichText::new("Pick an instance from the sidebar")
                            .font(theme::semibold(15.0))
                            .color(p.text_muted),
                    );
                });
            }
        }
    }

    fn poll_ops(&mut self, ctx: &egui::Context) {
        let mut to_settle = Vec::new();
        let mut needs_clock = false;
        for (name, slot) in &mut self.ops {
            // A waker already asked for this frame. Requesting again would pin the delay at 0.
            let _applied = slot.op.poll(&mut slot.view);
            if slot.running() {
                if !slot.op.wakes_on_io() {
                    needs_clock = true;
                }
            } else if !slot.settled {
                slot.settled = true;
                slot.settled_at = Some(Instant::now());
                to_settle.push(name.clone());
            }
        }
        for name in to_settle {
            self.on_op_settled(ctx, &name);
        }
        if needs_clock {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Runs once per op, when its outcome arrives: harvest its changes, refresh the catalog,
    /// put failed mod changes back in the list, and toast when the op isn't on screen.
    fn on_op_settled(&mut self, ctx: &egui::Context, key: &str) {
        self.harvest_changes(key);
        self.backend.refresh();
        let Some(slot) = self.ops.get(key) else {
            return;
        };
        let succeeded = slot.succeeded();
        let failed = slot.failed();
        let hidden = slot.hidden;
        let title = slot.title.clone();
        let kind = slot.kind();
        let request = slot.request.clone();
        let launched = slot
            .view
            .changes
            .iter()
            .any(|c| matches!(c, Change::Launched { .. }));

        // After `refresh`, so a chained rescan finishes before the mod catalogue reloads.
        if matches!(
            kind,
            OpKind::Create | OpKind::Mods | OpKind::Update | OpKind::SetSource
        ) {
            self.backend.invalidate_mods();
        }

        // A failed mod change: give the user their staged list back (they can Discard it).
        if !succeeded
            && let OpRequest::Mods {
                instance,
                add,
                remove,
                ..
            } = &request
        {
            let staged = self.staged_mods.entry(instance.clone()).or_default();
            if staged.is_empty() {
                staged.add.clone_from(add);
                staged.remove.clone_from(remove);
            }
        }
        if succeeded && let OpRequest::Remove { instance } = &request {
            self.forget_instance(instance);
        }
        // "Hide the launcher while the game runs": minimise once the game is up. (The game is
        // detached, so the launcher can't bring itself back when it exits; minimising keeps
        // it one click away instead of hiding it for good.)
        if succeeded && launched && self.settings_draft.close_on_play {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        let looking_at = matches!(&self.main_view, Some(MainView::Instance(n)) if n == key);
        // A removed instance has nothing left to show. Drop the slot (the toast is the record)
        // so an empty catalog can return to the welcome screen.
        if succeeded && matches!(request, OpRequest::Remove { .. }) {
            self.toast_kind(
                toasts::ToastKind::Ok,
                format!("{title}: {} finished", op_noun(kind)),
            );
            self.ops.remove(key);
            if looking_at {
                self.main_view = None;
                self.ensure_selection();
            }
            return;
        }
        if looking_at && !hidden {
            return;
        }
        let what = op_noun(kind);
        if succeeded {
            self.toast_kind(toasts::ToastKind::Ok, format!("{title}: {what} finished"));
        } else if failed {
            self.toast_kind(toasts::ToastKind::Error, format!("{title}: {what} failed"));
        } else {
            self.toast(format!("{title}: {what} cancelled"));
        }
    }

    /// Updates-available bookkeeping from a settled op: new reports replace old ones; an
    /// update, source change or check that reported nothing for an instance clears its badge.
    fn harvest_changes(&mut self, key: &str) {
        let Some(slot) = self.ops.get(key) else {
            return;
        };
        let mut reported = Vec::new();
        for change in &slot.view.changes {
            if let Change::UpdatesAvailable {
                instance,
                whats_new,
            } = change
            {
                reported.push(instance.clone());
                self.updates.insert(instance.clone(), whats_new.clone());
            }
        }
        if !slot.succeeded() {
            return;
        }
        let cleared: Vec<String> = match &slot.request {
            OpRequest::Update { instance, .. } | OpRequest::SetSource { instance, .. } => {
                vec![instance.clone()]
            }
            OpRequest::CheckUpdates {
                instance: Some(instance),
            } => vec![instance.clone()],
            OpRequest::CheckUpdates { instance: None } => self.updates.keys().cloned().collect(),
            _ => Vec::new(),
        };
        for instance in cleared {
            if !reported.contains(&instance) {
                self.updates.remove(&instance);
                if let Some(check) = self.startup_check.as_mut()
                    && check.view.running()
                {
                    check.cleared.insert(instance);
                }
            }
        }
    }

    /// Drop everything kept per instance once it is gone.
    fn forget_instance(&mut self, instance: &str) {
        self.staged_mods.remove(instance);
        self.updates.remove(instance);
        self.notes_draft.remove(instance);
        self.display_name_draft.remove(instance);
        self.group_draft.remove(instance);
    }

    fn poll_startup_check(&mut self, ctx: &egui::Context) {
        let (fresh, done, needs_clock) = {
            let Some(check) = self.startup_check.as_mut() else {
                return;
            };
            let _applied = check.op.poll(&mut check.view);
            let start = check.processed.min(check.view.changes.len());
            let fresh: Vec<(String, Vec<RepoLog>)> = check.view.changes[start..]
                .iter()
                .filter_map(|change| match change {
                    Change::UpdatesAvailable {
                        instance,
                        whats_new,
                    } if !check.cleared.contains(instance) => {
                        Some((instance.clone(), whats_new.clone()))
                    }
                    _ => None,
                })
                .collect();
            check.processed = check.view.changes.len();
            let done = !check.view.running();
            let needs_clock = !done && !check.op.wakes_on_io();
            (fresh, done, needs_clock)
        };
        for (instance, whats_new) in fresh {
            self.updates.insert(instance, whats_new);
        }
        if needs_clock {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if done {
            self.startup_check = None;
        }
    }

    /// Adopt egui's own Ctrl+/- zoom, and apply the slider once a drag has been committed.
    ///
    /// `set_zoom_factor` does not change [`egui::Context::zoom_factor`] until the end of the
    /// pass, so `zoom_applied` is what distinguishes "we just set it" from a key the user pressed.
    fn sync_zoom(&mut self, ctx: &egui::Context) {
        let setting = self.settings_draft.appearance.scale;
        let zoom = ctx.zoom_factor();
        match self.zoom_applied {
            None => {
                ctx.set_zoom_factor(setting.max(0.5));
                self.zoom_applied = Some(setting);
            }
            Some(applied) => {
                let external = (zoom - applied).abs() > 0.001 && (zoom - setting).abs() > 0.001;
                if external {
                    let clamped = (zoom.clamp(0.75, 1.75) * 20.0).round() / 20.0;
                    self.settings_draft.appearance.scale = clamped;
                    self.zoom_applied = Some(clamped);
                    if (clamped - zoom).abs() > 0.001 {
                        ctx.set_zoom_factor(clamped);
                    }
                    self.backend.save_settings(self.settings_draft.clone());
                } else if (setting - zoom).abs() > 0.001 {
                    ctx.set_zoom_factor(setting.max(0.5));
                    self.zoom_applied = Some(setting);
                }
            }
        }
    }

    /// While a game is running, or pwc holds an instance, rescan every few seconds so Play
    /// leaves "Running" after the process exits. Idle repaint is scheduled; nothing spins.
    fn refresh_live_status(&mut self, ctx: &egui::Context) {
        let live = self
            .backend
            .instances()
            .iter()
            .any(|entry| entry.status.running.is_some() || entry.status.busy);
        if !live {
            self.last_live_refresh = None;
            return;
        }
        let due = self
            .last_live_refresh
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(3));
        if due {
            self.backend.refresh_liveness();
            self.last_live_refresh = Some(Instant::now());
        }
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    /// Install the egui context and one op waker on the first frame, then hand that waker to
    /// any op that does not have it yet.
    fn ensure_wake(&mut self, ctx: &egui::Context) {
        if self.op_waker.is_none() {
            self.backend.set_ui_context(ctx.clone());
            let ctx = ctx.clone();
            self.op_waker = Some(Arc::new(move |wake| match wake {
                OpWake::Immediate => ctx.request_repaint(),
                OpWake::Coalesce => ctx.request_repaint_after(Duration::from_millis(100)),
            }));
        }
        self.install_waker();
    }

    fn install_waker(&mut self) {
        let Some(waker) = self.op_waker.clone() else {
            return;
        };
        for slot in self.ops.values_mut() {
            if !slot.op.has_waker() {
                slot.op.set_waker(Arc::clone(&waker));
            }
        }
        if let Some(check) = self.startup_check.as_mut()
            && !check.op.has_waker()
        {
            check.op.set_waker(waker);
        }
    }

    /// Start the "check every instance" op once the catalog has landed, or two seconds after
    /// the first frame, whichever comes first. Honors `check_updates_on_start`.
    fn maybe_start_update_check(&mut self, ctx: &egui::Context) {
        if self.update_check_started {
            return;
        }
        if !self.settings_draft.check_updates_on_start {
            self.update_check_started = true;
            return;
        }
        let first = *self.first_frame.get_or_insert_with(Instant::now);
        let catalog_ready = !self.backend.refresh_pending();
        let waited = first.elapsed() >= Duration::from_secs(2);
        if catalog_ready || waited {
            self.update_check_started = true;
            self.begin_startup_check();
            return;
        }
        ctx.request_repaint_after(Duration::from_secs(2).saturating_sub(first.elapsed()));
    }

    fn begin_startup_check(&mut self) {
        let mut op = self
            .backend
            .start_op(OpRequest::CheckUpdates { instance: None });
        if let Some(waker) = &self.op_waker {
            op.set_waker(Arc::clone(waker));
        }
        self.startup_check = Some(StartupCheck {
            op,
            view: OpView::default(),
            processed: 0,
            cleared: HashSet::new(),
        });
    }

    /// Write a settings draft that has been dirty for half a second, or at once when the
    /// settings page is no longer showing (a dialog or op may read the saved settings next).
    fn flush_settings(&mut self) {
        let settings_open = matches!(self.main_view, Some(MainView::Settings));
        if self
            .settings_save_at
            .is_some_and(|at| !settings_open || Instant::now() >= at)
        {
            self.save_pending_settings();
        }
    }

    /// Write the settings draft now if a delayed save is pending.
    fn save_pending_settings(&mut self) {
        if self.settings_save_at.take().is_some() {
            self.backend.save_settings(self.settings_draft.clone());
        }
    }

    fn modal_open(&self) -> bool {
        self.new_instance.is_some()
            || self.change_source.is_some()
            || self.duplicate.is_some()
            || self.confirm.is_some()
    }

    /// A Create/Duplicate (or any other instance op) that has not settled yet. Settled results
    /// and launcher maintenance do not keep the welcome screen away.
    fn has_live_instance_op(&self) -> bool {
        self.ops.iter().any(|(key, slot)| {
            !slot.settled
                && key.as_str() != dialogs::MAINTENANCE_KEY
                && slot.kind() != OpKind::Maintenance
        })
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let modal_open = self.modal_open();
        if !modal_open && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.open_new_instance_dialog();
        }
        if !modal_open && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
            self.main_view = Some(MainView::Settings);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            self.backend.refresh();
        }
        let typing = ctx.memory(|mem| mem.focused().is_some());
        let popup = egui::Popup::is_any_open(ctx);
        // Look at the key before cloning an entry or asking why Play would refuse. Consume it
        // only when Play is actually allowed, so a blocked instance does not swallow Enter.
        let plain_enter =
            ctx.input(|input| input.key_pressed(Key::Enter) && input.modifiers == Modifiers::NONE);
        if !modal_open
            && !typing
            && !popup
            && plain_enter
            && let Some(MainView::Instance(name)) = self.main_view.clone()
        {
            let allowed = self
                .backend
                .instances()
                .iter()
                .find(|entry| entry.card.name == name)
                .cloned()
                .is_some_and(|entry| self.play_block_reason(entry.as_ref()).is_none());
            if allowed && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter)) {
                self.play(&name, false);
            }
        }
        if !modal_open && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.focus_search = true;
        }
    }

    /// An informational toast.
    pub(crate) fn toast(&mut self, message: impl Into<String>) {
        self.toast_kind(toasts::ToastKind::Info, message);
    }

    /// A toast of the given kind (its colour bar).
    pub(crate) fn toast_kind(&mut self, kind: toasts::ToastKind, message: impl Into<String>) {
        self.toasts.push(toasts::Toast {
            kind,
            message: message.into(),
            created: Instant::now(),
        });
    }

    /// Start `request`, tracked under `key` (the instance it is about; for Create and Duplicate
    /// the new instance's name). Refuses, with a toast, while another op holds any instance it
    /// touches, so a running op is never replaced and orphaned. Returns whether it started.
    pub(crate) fn start_op(&mut self, key: impl Into<String>, request: OpRequest) -> bool {
        let key = key.into();
        let mut touched = vec![key.clone()];
        touched.extend(request.instance().map(str::to_owned));
        if let OpRequest::Duplicate { new_name, .. } = &request {
            touched.push(new_name.clone());
        }
        if let Some((_, busy)) = self
            .ops
            .iter()
            .find(|(k, slot)| slot.running() && touched.iter().any(|t| slot.holds(k, t)))
        {
            let message = format!(
                "Wait for the {} of {} to finish first",
                op_noun(busy.kind()),
                busy.title
            );
            self.toast(message);
            return false;
        }
        let title = match &request {
            OpRequest::Create {
                display_name: Some(d),
                ..
            }
            | OpRequest::Duplicate {
                display_name: Some(d),
                ..
            } if !d.trim().is_empty() => d.clone(),
            OpRequest::Maintenance { .. } => "Maintenance".to_owned(),
            _ => self.instance_title(&key),
        };
        // The op runs in a child process that reads settings from disk.
        self.save_pending_settings();
        let mut op = self.backend.start_op(request.clone());
        if let Some(waker) = &self.op_waker {
            op.set_waker(Arc::clone(waker));
        }
        self.ops.insert(
            key,
            OpSlot {
                op,
                view: OpView::default(),
                hidden: false,
                request,
                started: Instant::now(),
                settled_at: None,
                settled: false,
                title,
            },
        );
        true
    }

    /// An instance's shown name: its display name, else its directory name.
    pub(crate) fn instance_title(&self, name: &str) -> String {
        self.backend
            .instances()
            .iter()
            .find(|e| e.card.name == name)
            .map(|e| e.card.meta.title(name).to_owned())
            .or_else(|| self.ops.get(name).map(|s| s.title.clone()))
            .unwrap_or_else(|| name.to_owned())
    }

    /// Whether a launcher op is running on `instance` right now (including the source and the
    /// destination of a duplicate). An external `pwc` op shows up as `status.busy` instead.
    pub(crate) fn busy(&self, instance: &str) -> bool {
        self.running_op(instance).is_some()
    }

    /// The running op that holds `instance`, if any.
    pub(crate) fn running_op(&self, instance: &str) -> Option<&OpSlot> {
        self.ops
            .iter()
            .find(|(k, slot)| slot.running() && slot.holds(k, instance))
            .map(|(_, slot)| slot)
    }

    /// Close a settled op's result (or hide a running one's Forge).
    pub(crate) fn dismiss_op(&mut self, key: &str) {
        match self.ops.get_mut(key) {
            Some(slot) if slot.running() => slot.hidden = true,
            Some(_) => {
                self.ops.remove(key);
            }
            None => {}
        }
    }

    /// Run a settled op's original request again.
    pub(crate) fn retry_op(&mut self, key: &str) {
        let Some(slot) = self.ops.get(key) else {
            return;
        };
        if slot.running() {
            let message = format!(
                "Wait for the {} of {} to finish first",
                op_noun(slot.kind()),
                slot.title
            );
            self.toast(message);
            return;
        }
        let request = slot.request.clone();
        // Leave the failed slot in place. `start_op` overwrites it on success and, if another
        // running op holds the instance, toasts and returns without inserting.
        self.start_op(key, request);
    }

    pub(crate) fn play(&mut self, instance: &str, last_good: bool) {
        self.start_op(
            instance,
            OpRequest::Play {
                instance: instance.to_owned(),
                last_good,
                args: Vec::new(),
            },
        );
    }

    /// Act on a recovery a failed op offered. Retry runs the failed op's own request again; the
    /// others replace the settled result with a new op.
    pub(crate) fn apply_recovery(&mut self, key: &str, recovery: watt_core::events::Recovery) {
        use watt_core::events::Recovery;
        // A duplicate's request names the source. Roll back / rebuild would hit that original.
        if self
            .ops
            .get(key)
            .is_some_and(|slot| slot.kind() == OpKind::Duplicate)
            && !matches!(recovery, Recovery::Retry)
        {
            return;
        }
        let instance = self
            .ops
            .get(key)
            .and_then(|s| s.request.instance().map(str::to_owned))
            .unwrap_or_else(|| key.to_owned());
        let request = match recovery {
            Recovery::Retry => {
                self.retry_op(key);
                return;
            }
            Recovery::Doctor => {
                self.main_view = Some(MainView::Settings);
                return;
            }
            Recovery::PlayLastGood => OpRequest::Play {
                instance: instance.clone(),
                last_good: true,
                args: Vec::new(),
            },
            Recovery::ResetLock => OpRequest::Update {
                instance: instance.clone(),
                build: None,
                reset_lock: true,
            },
            Recovery::Rollback => OpRequest::Rollback {
                instance: instance.clone(),
                generation: None,
            },
            Recovery::ForceRebuild => OpRequest::Build {
                instance: instance.clone(),
                force: true,
                profile: None,
            },
        };
        if self.ops.get(key).is_some_and(|s| !s.running()) {
            self.ops.remove(key);
        }
        self.start_op(instance, request);
    }

    /// The kind of the launcher op running on `instance`, if one is.
    pub(crate) fn op_kind_running(&self, instance: &str) -> Option<OpKind> {
        self.running_op(instance).map(OpSlot::kind)
    }
}

/// What an op is called in a sentence ("the update of Survival").
pub(crate) fn op_noun(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Create => "creation",
        OpKind::Mods => "mod change",
        OpKind::Update => "update",
        OpKind::Build => "build",
        OpKind::Play => "launch",
        OpKind::Rollback => "rollback",
        OpKind::Duplicate => "duplication",
        OpKind::Remove => "removal",
        OpKind::SetSource => "source change",
        OpKind::CheckUpdates => "update check",
        OpKind::Maintenance => "maintenance",
    }
}
