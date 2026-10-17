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

use std::collections::HashMap;
use std::time::Instant;

use egui::{CentralPanel, Panel};
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

    fn label(self) -> &'static str {
        match self {
            ModPreset::Essentials => "Essentials",
            ModPreset::EssentialsDevToolkit => "Essentials + Developer Toolkit",
            ModPreset::Vanilla => "Vanilla (no mods)",
        }
    }
}

/// Which kind of source a dialog is configuring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceChoice {
    Managed { channel: String },
    Linked { path: String },
    PwcDefault,
}

impl SourceChoice {
    fn to_spec(&self) -> SourceSpec {
        match self {
            SourceChoice::Managed { channel } => SourceSpec::Managed {
                channel: channel.clone(),
                engine_ref: watt_core::meta::AUTO.to_owned(),
                mods_ref: watt_core::meta::AUTO.to_owned(),
            },
            SourceChoice::Linked { path } => SourceSpec::Linked {
                pwc_source: path.into(),
                repositories: Vec::new(),
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
    fn is_empty(&self) -> bool {
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
    #[allow(dead_code)] // transitional: read by the redesigned header and Forge
    pub started: Instant,
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
    pub(crate) startup_check: Option<(RunningOp, OpView)>,

    pub(crate) new_instance: Option<new_instance::NewInstanceState>,
    pub(crate) change_source: Option<new_instance::ChangeSourceState>,
    pub(crate) confirm: Option<Confirm>,

    pub(crate) settings_draft: Settings,
    pub(crate) toasts: Vec<toasts::Toast>,

    pub(crate) mods_browse_search: String,
    pub(crate) mods_browse_category: Option<String>,
    pub(crate) mods_show_libraries: bool,

    pub(crate) notes_draft: HashMap<String, String>,
    /// In-progress display name, keyed by instance directory name.
    pub(crate) display_name_draft: HashMap<String, String>,
    /// In-progress sidebar group, keyed by instance directory name.
    pub(crate) group_draft: HashMap<String, String>,
    pub(crate) toasted_load_errors: std::collections::HashSet<String>,
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
            new_instance: None,
            change_source: None,
            confirm: None,
            settings_draft,
            toasts: Vec::new(),
            mods_browse_search: String::new(),
            mods_browse_category: None,
            mods_show_libraries: false,
            notes_draft: HashMap::new(),
            display_name_draft: HashMap::new(),
            group_draft: HashMap::new(),
            toasted_load_errors: std::collections::HashSet::new(),
        };
        app.sync_updates_from_backend();
        if app.backend.settings().check_updates_on_start {
            let op = app
                .backend
                .start_op(OpRequest::CheckUpdates { instance: None });
            app.startup_check = Some((op, OpView::default()));
        }
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
        let scale = self.settings_draft.appearance.scale.max(0.5);
        if (ui.ctx().zoom_factor() - scale).abs() > 0.001 {
            ui.ctx().set_zoom_factor(scale);
        }
        theme::apply(
            ui.ctx(),
            self.settings_draft.appearance.dark,
            self.settings_draft.appearance.animations,
        );

        if self.backend.tick() {
            self.ensure_selection();
            self.sync_updates_from_backend();
        }
        self.poll_ops(ui.ctx());
        self.poll_startup_check();
        self.handle_shortcuts(ui.ctx());

        if self.backend.loading() {
            // Background jobs don't wake egui; poll them while they run, animations or not.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        if self.backend.instances().is_empty() && self.new_instance.is_none() && self.ops.is_empty()
        {
            self.welcome_screen(ui);
        } else {
            let palette = theme::palette(ui.ctx());
            Panel::left("sidebar")
                .exact_size(260.0)
                .resizable(false)
                .frame(egui::Frame::new().fill(palette.bg_sidebar).inner_margin(0))
                .show_separator_line(true)
                .show(ui, |ui| self.sidebar(ui));
            CentralPanel::default()
                .frame(egui::Frame::new().fill(palette.bg_surface).inner_margin(0))
                .show(ui, |ui| self.main_area(ui));
        }

        self.new_instance_dialog(ui.ctx());
        self.change_source_dialog(ui.ctx());
        self.confirm_dialog(ui.ctx());
        self.draw_toasts(ui.ctx());
    }

    fn main_area(&mut self, ui: &mut egui::Ui) {
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
                    ui.label(theme::subheading("Pick an instance from the sidebar"));
                });
            }
        }
    }

    fn poll_ops(&mut self, ctx: &egui::Context) {
        let mut to_settle = Vec::new();
        let mut any_running = false;
        for (name, slot) in &mut self.ops {
            if slot.op.poll(&mut slot.view) {
                ctx.request_repaint();
            }
            if slot.running() {
                any_running = true;
            } else if !slot.settled {
                slot.settled = true;
                to_settle.push(name.clone());
            }
        }
        for name in to_settle {
            self.on_op_settled(&name);
        }
        if any_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// Runs once per op, when its outcome arrives: harvest its changes, refresh the catalog,
    /// put failed mod changes back in the list, and toast when the op isn't on screen.
    fn on_op_settled(&mut self, key: &str) {
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

        let looking_at = matches!(&self.main_view, Some(MainView::Instance(n)) if n == key);
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

    fn poll_startup_check(&mut self) {
        if let Some((op, view)) = &mut self.startup_check {
            op.poll(view);
            for change in &view.changes {
                if let Change::UpdatesAvailable {
                    instance,
                    whats_new,
                } = change
                {
                    self.updates.insert(instance.clone(), whats_new.clone());
                }
            }
            if !view.running() {
                self.startup_check = None;
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.open_new_instance_dialog();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
            self.main_view = Some(MainView::Settings);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            self.backend.refresh();
        }
        let modal_open =
            self.new_instance.is_some() || self.change_source.is_some() || self.confirm.is_some();
        let typing = ctx.memory(|mem| mem.focused().is_some());
        if !modal_open
            && !typing
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            && let Some(MainView::Instance(name)) = self.main_view.clone()
        {
            self.play(&name, false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
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
            _ => self.instance_title(&key),
        };
        let op = self.backend.start_op(request.clone());
        self.ops.insert(
            key,
            OpSlot {
                op,
                view: OpView::default(),
                hidden: false,
                request,
                started: Instant::now(),
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
    #[allow(dead_code)] // transitional: used by the redesigned screens
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
            return;
        }
        let request = slot.request.clone();
        self.ops.remove(key);
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
