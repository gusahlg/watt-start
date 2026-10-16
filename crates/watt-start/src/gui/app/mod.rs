//! The application: state, routing and the top-level layout. [`App::ui`] is the single entry
//! point every screen is drawn from (used both by [`crate::gui::run`] and the snapshot tests).

mod dialogs;
mod instance_page;
mod new_instance;
mod settings_page;
mod sidebar;
mod toasts;
mod welcome;

use std::collections::HashMap;
use std::time::Instant;

use egui::{CentralPanel, Panel};
use watt_core::events::{Change, OpKind, RepoLog};
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
pub(crate) struct OpSlot {
    pub op: RunningOp,
    pub view: OpView,
    pub hidden: bool,
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
            toasted_load_errors: std::collections::HashSet::new(),
        };
        app.sync_updates_from_backend();
        if app.backend.settings().check_updates_on_start {
            let op = app
                .backend
                .start_op(OpRequest::CheckUpdates { instance: None });
            app.startup_check = Some((op, OpView::default()));
        }
        if let Some(first) = app.backend.instances().first() {
            app.main_view = Some(MainView::Instance(first.card.name.clone()));
        }
        app
    }

    // The rest of this block is test/screenshot-only: nothing in the real app calls these, so
    // they would otherwise warn as dead code in a plain `cargo build`/`cargo clippy -p
    // watt-start` (the `tests/gui_snapshots.rs` binary that does call them is a separate
    // compilation unit).
    #[allow(dead_code)]
    /// A demo-backed app with a placeholder forge, so rendering it never hits `forge.rs`'s
    /// `todo!()` implementation before the `w/forge` branch lands. Used by the snapshot tests.
    pub(crate) fn for_snapshot_tests() -> Self {
        let mut app = Self::with_backend(Box::new(DemoBackend::new()));
        app.forge = ForgeBridge::placeholder();
        app
    }

    #[allow(dead_code)]
    /// Like [`App::for_snapshot_tests`], with no instances (the welcome screen).
    pub(crate) fn for_snapshot_tests_empty() -> Self {
        let mut app = Self::with_backend(Box::new(DemoBackend::empty()));
        app.forge = ForgeBridge::placeholder();
        app
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
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        theme::apply(ui.ctx(), self.settings_draft.appearance.dark);
        ui.ctx()
            .set_zoom_factor(self.settings_draft.appearance.scale.max(0.5));

        if self.backend.tick() {
            self.sync_updates_from_backend();
        }
        self.poll_ops(ui.ctx());
        self.poll_startup_check();
        self.handle_shortcuts(ui.ctx());

        if self.backend.instances().is_empty() && self.new_instance.is_none() {
            self.welcome_screen(ui);
        } else {
            Panel::left("sidebar")
                .exact_size(280.0)
                .resizable(false)
                .show(ui, |ui| self.sidebar(ui));
            CentralPanel::default().show(ui, |ui| self.main_area(ui));
        }

        self.new_instance_dialog(ui.ctx());
        self.change_source_dialog(ui.ctx());
        self.confirm_dialog(ui.ctx());
        self.draw_toasts(ui.ctx());
    }

    fn main_area(&mut self, ui: &mut egui::Ui) {
        match self.main_view.clone() {
            Some(MainView::Settings) => self.settings_page(ui),
            Some(MainView::Instance(name))
                if self.backend.instances().iter().any(|e| e.card.name == name) =>
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
        let mut finished_names = Vec::new();
        for (name, slot) in &mut self.ops {
            if slot.op.poll(&mut slot.view) {
                ctx.request_repaint();
            }
            if !slot.view.running() {
                finished_names.push(name.clone());
            }
        }
        for name in finished_names {
            self.on_op_settled(&name);
        }
        if !self.ops.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// Called the first time a tracked op's view reports it is no longer running: refresh the
    /// catalog, record updates-available and toast when the instance is not the one on screen.
    fn on_op_settled(&mut self, name: &str) {
        let Some(slot) = self.ops.get(name) else {
            return;
        };
        if slot.hidden {
            // Already toasted when it was hidden; nothing more to do until the user closes it.
        }
        self.harvest_changes(name);
        self.backend.refresh();
        let looking_at = matches!(&self.main_view, Some(MainView::Instance(n)) if n == name);
        if let Some(slot) = self.ops.get(name)
            && !looking_at
        {
            let ok = matches!(
                slot.view.outcome,
                Some(watt_core::events::OpOutcome::Ok { .. })
            );
            let title = slot
                .view
                .instance
                .clone()
                .unwrap_or_else(|| name.to_owned());
            self.toast(if ok {
                format!("{title} is ready")
            } else {
                format!("{title}: the last operation did not finish cleanly")
            });
        }
    }

    fn harvest_changes(&mut self, name: &str) {
        if let Some(slot) = self.ops.get(name) {
            for change in &slot.view.changes {
                if let Change::UpdatesAvailable {
                    instance,
                    whats_new,
                } = change
                {
                    self.updates.insert(instance.clone(), whats_new.clone());
                }
            }
        }
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
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            && let Some(MainView::Instance(name)) = self.main_view.clone()
        {
            self.play(&name, false);
        }
    }

    pub(crate) fn toast(&mut self, message: impl Into<String>) {
        self.toasts.push(toasts::Toast {
            message: message.into(),
            created: Instant::now(),
        });
    }

    pub(crate) fn start_op(&mut self, key: impl Into<String>, request: OpRequest) {
        let key = key.into();
        let op = self.backend.start_op(request);
        self.ops.insert(
            key,
            OpSlot {
                op,
                view: OpView::default(),
                hidden: false,
            },
        );
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

    pub(crate) fn op_kind_running(&self, instance: &str) -> Option<OpKind> {
        self.ops.get(instance).and_then(|s| s.view.op)
    }
}
