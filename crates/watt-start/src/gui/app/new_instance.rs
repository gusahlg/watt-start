//! The "New instance" and "Change source" dialogs.
//!
//! Switching source kind converts [`SourceChoice`] before any field is read. The old radios
//! destructured the previous variant and panicked (`unreachable!`) on the way back to Latest game.

use watt_core::meta::Binding;
use watt_core::ops::OpRequest;

use super::dialogs::{self, FormAction};
use super::{App, MainView, ModPreset, SourceChoice};
use crate::gui::backend::Loaded;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Variant};

/// State of the "New instance" dialog.
pub(crate) struct NewInstanceState {
    pub name: String,
    pub name_edited: bool,
    pub display_name: String,
    pub color: egui::Color32,
    pub source: SourceChoice,
    pub mod_preset: ModPreset,
    pub build_now: bool,
    /// The directory field is revealed.
    pub edit_dir: bool,
    /// The user has typed, so a validation error may show.
    pub typed: bool,
    /// Request focus on the display field once.
    pub focus_display: bool,
    /// Request focus on the directory field once.
    pub focus_dir: bool,
}

/// State of the "Change source" dialog, for an existing instance.
pub(crate) struct ChangeSourceState {
    pub instance: String,
    pub source: SourceChoice,
    /// The binding as opened, including refs the dialog does not edit.
    pub original: SourceChoice,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Managed,
    Linked,
    PwcDefault,
}

fn kind_of(source: &SourceChoice) -> SourceKind {
    match source {
        SourceChoice::Managed { .. } => SourceKind::Managed,
        SourceChoice::Linked { .. } => SourceKind::Linked,
        SourceChoice::PwcDefault => SourceKind::PwcDefault,
    }
}

/// Point `source` at `kind` before anyone matches on it.
///
/// Staying on the same kind keeps in-progress edits. Switching back to the kind `original` had
/// restores that value, hidden refs included. Anything else starts empty.
fn set_source_kind(
    source: &mut SourceChoice,
    kind: SourceKind,
    original: Option<&SourceChoice>,
    default_channel: &str,
) {
    if kind_of(source) == kind {
        return;
    }
    if let Some(original) = original.filter(|original| kind_of(original) == kind) {
        *source = original.clone();
        return;
    }
    *source = match kind {
        SourceKind::Managed => dialogs::fresh_managed(default_channel),
        SourceKind::Linked => dialogs::fresh_linked(),
        SourceKind::PwcDefault => SourceChoice::PwcDefault,
    };
}

fn branch_names(app: &mut App) -> Vec<String> {
    match app.backend.branches() {
        Loaded::Ready(list) => list.into_iter().map(|b| b.name).collect(),
        _ => Vec::new(),
    }
}

impl App {
    pub(crate) fn open_new_instance_dialog(&mut self) {
        let channel = self.backend.settings().default_channel.clone();
        self.new_instance = Some(NewInstanceState {
            name: String::new(),
            name_edited: false,
            display_name: String::new(),
            color: theme::VOLT,
            source: dialogs::fresh_managed(&channel),
            mod_preset: ModPreset::Essentials,
            build_now: true,
            edit_dir: false,
            typed: false,
            focus_display: true,
            focus_dir: false,
        });
    }

    pub(crate) fn open_change_source_dialog(&mut self, instance: &str) {
        let source = self
            .backend
            .instances()
            .iter()
            .find(|e| e.card.name == instance)
            .map(|e| match &e.card.meta.source {
                Binding::Managed {
                    channel,
                    engine_ref,
                    mods_ref,
                    ..
                } => SourceChoice::Managed {
                    channel: channel.clone(),
                    engine_ref: engine_ref.clone(),
                    mods_ref: mods_ref.clone(),
                },
                Binding::Linked {
                    pwc_source,
                    repositories,
                } => SourceChoice::Linked {
                    path: pwc_source.display().to_string(),
                    repositories: repositories.clone(),
                },
                Binding::PwcDefault => SourceChoice::PwcDefault,
            })
            .unwrap_or(SourceChoice::PwcDefault);
        self.change_source = Some(ChangeSourceState {
            instance: instance.to_owned(),
            original: source.clone(),
            source,
        });
    }

    /// Snapshot helper: New instance, already on a linked checkout with `path` typed.
    #[allow(dead_code)]
    pub(crate) fn show_new_instance_linked_for_tests(&mut self, path: &str) {
        self.open_new_instance_dialog();
        let state = self.new_instance.as_mut().expect("dialog just opened");
        state.display_name = "Linked world".to_owned();
        state.name = "linked-world".to_owned();
        state.name_edited = true;
        state.typed = true;
        state.focus_display = false;
        state.source = SourceChoice::Linked {
            path: path.to_owned(),
            repositories: Vec::new(),
        };
    }

    pub(crate) fn new_instance_dialog(&mut self, ctx: &egui::Context) {
        if self.new_instance.is_none() {
            return;
        }
        let branches = branch_names(self);
        let taken = dialogs::taken_dirs(self);
        let default_channel = self.backend.settings().default_channel.clone();
        let (dialog, action) = dialogs::show_dialog(
            ctx,
            egui::Id::unique("new_instance"),
            "New instance",
            None,
            520.0,
            |dialog| {
                let state = self.new_instance.as_mut().unwrap();
                let mut action = FormAction::fresh();
                let valid = dialog.scroll(|ui| {
                    new_instance_body(ui, state, &branches, &taken, &default_channel, &mut action)
                });
                action.valid = valid;
                let tip = create_tip(state, &taken);
                dialog.footer(|ui| {
                    if Button::new("Create")
                        .variant(Variant::Primary)
                        .min_width(96.0)
                        .enabled(action.valid)
                        .on_disabled_hover_text(tip)
                        .show(ui)
                        .clicked()
                    {
                        action.submit = true;
                    }
                    if Button::new("Cancel")
                        .variant(Variant::Ghost)
                        .min_width(88.0)
                        .show(ui)
                        .clicked()
                    {
                        action.cancel = true;
                    }
                });
                action
            },
        );
        if dialogs::form_closed(&dialog, &action, false) {
            self.new_instance = None;
        } else if dialogs::form_submitted(&dialog, &action) {
            self.confirm_new_instance();
        }
    }

    fn confirm_new_instance(&mut self) {
        let Some(state) = self.new_instance.take() else {
            return;
        };
        let name = state.name.clone();
        let trimmed = state.display_name.trim();
        let request = OpRequest::Create {
            name: name.clone(),
            display_name: (!trimmed.is_empty()).then(|| trimmed.to_owned()),
            color: Some(theme::to_hex_color(state.color)),
            group: None,
            source: state.source.to_spec(),
            mods: state.mod_preset.specs(),
            build: state.build_now,
        };
        if self.start_op(name.clone(), request) {
            self.main_view = Some(MainView::Instance(name));
        } else {
            self.new_instance = Some(state);
        }
    }

    pub(crate) fn change_source_dialog(&mut self, ctx: &egui::Context) {
        let Some(instance) = self.change_source.as_ref().map(|s| s.instance.clone()) else {
            return;
        };
        let branches = branch_names(self);
        let default_channel = self.backend.settings().default_channel.clone();
        let rebuild = self.backend.settings().rebuild_after_update;
        let subtitle = self.instance_title(&instance);
        let busy_tip = busy_tip(self, &instance);
        let (dialog, action) = dialogs::show_dialog(
            ctx,
            egui::Id::unique("change_source"),
            "Change source",
            Some(subtitle.as_str()),
            460.0,
            |dialog| {
                let state = self.change_source.as_mut().unwrap();
                let mut action = FormAction::fresh();
                let valid = dialog.scroll(|ui| {
                    change_source_body(
                        ui,
                        state,
                        &branches,
                        &default_channel,
                        rebuild,
                        busy_tip.is_empty(),
                        &mut action,
                    )
                });
                action.valid = valid;
                let tip = apply_tip(state, &busy_tip);
                dialog.footer(|ui| {
                    if Button::new("Apply")
                        .variant(Variant::Primary)
                        .min_width(88.0)
                        .enabled(action.valid)
                        .on_disabled_hover_text(&tip)
                        .show(ui)
                        .clicked()
                    {
                        action.submit = true;
                    }
                    if Button::new("Cancel")
                        .variant(Variant::Ghost)
                        .min_width(88.0)
                        .show(ui)
                        .clicked()
                    {
                        action.cancel = true;
                    }
                });
                action
            },
        );
        if dialogs::form_closed(&dialog, &action, false) {
            self.change_source = None;
        } else if dialogs::form_submitted(&dialog, &action) {
            self.confirm_change_source();
        }
    }

    fn confirm_change_source(&mut self) {
        let Some(state) = self.change_source.take() else {
            return;
        };
        let instance = state.instance.clone();
        let request = OpRequest::SetSource {
            instance: instance.clone(),
            source: state.source.to_spec(),
            build: None,
        };
        if !self.start_op(instance, request) {
            self.change_source = Some(state);
        }
    }
}

fn busy_tip(app: &App, instance: &str) -> String {
    if let Some(op) = app.running_op(instance) {
        return format!(
            "Wait for the {} of {} to finish",
            super::op_noun(op.kind()),
            op.title
        );
    }
    let external = app
        .backend
        .instances()
        .iter()
        .any(|e| e.card.name == instance && e.status.busy);
    if external {
        "This instance is busy".to_owned()
    } else {
        String::new()
    }
}

fn new_instance_body(
    ui: &mut egui::Ui,
    state: &mut NewInstanceState,
    branches: &[String],
    taken: &[String],
    default_channel: &str,
    action: &mut FormAction,
) -> bool {
    // 6px between a label and its control; `add_space(6)` plus that gap is a 12px section break.
    ui.spacing_mut().item_spacing.y = 6.0;
    let display_changed = dialogs::display_field(
        ui,
        "new-display",
        &mut state.display_name,
        &mut state.focus_display,
    );
    if !state.name_edited {
        state.name = dialogs::slugify(&state.display_name);
    }
    let (dir_changed, edit_clicked) = dialogs::dir_line(
        ui,
        "new-dir",
        &mut state.name,
        &mut state.edit_dir,
        &mut state.focus_dir,
    );
    if edit_clicked {
        action.interacted = true;
    }
    if display_changed || dir_changed {
        state.typed = true;
    }
    if dir_changed {
        state.name_edited = true;
    }
    let name_err = dialogs::valid_name(&state.name, taken).err();
    if state.typed
        && let Some(err) = name_err
    {
        dialogs::error_line(ui, err);
    }

    ui.add_space(6.0);
    dialogs::field_label(ui, "Colour");
    if swatches(ui, &mut state.color) {
        action.interacted = true;
    }

    ui.add_space(6.0);
    dialogs::field_label(ui, "Source");
    if source_cards(
        ui,
        &mut state.source,
        None,
        default_channel,
        branches,
        "new-channel",
    ) {
        action.interacted = true;
    }

    ui.add_space(6.0);
    dialogs::field_label(ui, "Mods");
    if mod_preset_row(ui, &mut state.mod_preset) {
        action.interacted = true;
    }

    ui.add_space(6.0);
    widgets::switch_row(
        ui,
        &mut state.build_now,
        "Build right away",
        "A first build takes 15–40 minutes.",
    );
    name_err.is_none() && dialogs::source_problem(&state.source).is_none()
}

fn create_tip(state: &NewInstanceState, taken: &[String]) -> &'static str {
    if state.typed
        && let Err(err) = dialogs::valid_name(&state.name, taken)
    {
        return err;
    }
    if dialogs::valid_name(&state.name, taken).is_err() {
        return "Enter a name";
    }
    dialogs::source_problem(&state.source).unwrap_or("Enter a name")
}

fn change_source_body(
    ui: &mut egui::Ui,
    state: &mut ChangeSourceState,
    branches: &[String],
    default_channel: &str,
    rebuild: bool,
    idle: bool,
    action: &mut FormAction,
) -> bool {
    if source_cards(
        ui,
        &mut state.source,
        Some(&state.original),
        default_channel,
        branches,
        "change-channel",
    ) {
        action.interacted = true;
    }
    let p = theme::palette(ui.ctx());
    let mut note = "Applying re-resolves the instance now".to_owned();
    if rebuild {
        note.push_str(" and rebuilds it");
    }
    note.push('.');
    ui.add(
        egui::Label::new(
            egui::RichText::new(note)
                .font(theme::regular(12.0))
                .color(p.warn),
        )
        .wrap(),
    );
    let changed = state.source != state.original;
    idle && changed && dialogs::source_problem(&state.source).is_none()
}

fn apply_tip(state: &ChangeSourceState, busy_tip: &str) -> String {
    if !busy_tip.is_empty() {
        return busy_tip.to_owned();
    }
    if let Some(err) = dialogs::source_problem(&state.source) {
        return err.to_owned();
    }
    if state.source == state.original {
        return "Nothing to apply".to_owned();
    }
    String::new()
}

fn swatches(ui: &mut egui::Ui, color: &mut egui::Color32) -> bool {
    let mut picked = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        ui.add_space(2.0);
        for swatch in theme::INSTANCE_PALETTE {
            if widgets::swatch(ui, swatch, *color == swatch).clicked() {
                *color = swatch;
                picked = true;
            }
        }
    });
    picked
}

/// The three source cards. Returns whether one was clicked this frame.
fn source_cards(
    ui: &mut egui::Ui,
    source: &mut SourceChoice,
    original: Option<&SourceChoice>,
    default_channel: &str,
    branches: &[String],
    channel_id: &str,
) -> bool {
    let current = kind_of(source);
    let mut pick = None;
    if widgets::choice_card(
        ui,
        current == SourceKind::Managed,
        "Latest game",
        "Follows a branch.",
        |ui| {
            if let SourceChoice::Managed { channel, .. } = source {
                channel_field(ui, channel_id, channel, branches);
            }
        },
    )
    .clicked()
    {
        pick = Some(SourceKind::Managed);
    }
    if widgets::choice_card(
        ui,
        current == SourceKind::Linked,
        "My game checkout",
        "A checkout on this machine.",
        |ui| {
            if let SourceChoice::Linked { path, .. } = source {
                widgets::text_input_with_id(
                    ui,
                    (channel_id, "path"),
                    path,
                    "Checkout path",
                    ui.available_width(),
                );
            }
        },
    )
    .clicked()
    {
        pick = Some(SourceKind::Linked);
    }
    if widgets::choice_card_plain(
        ui,
        current == SourceKind::PwcDefault,
        "pwc's configured checkout",
        "The checkout pwc is already using.",
    )
    .clicked()
    {
        pick = Some(SourceKind::PwcDefault);
    }
    let Some(kind) = pick else {
        return false;
    };
    set_source_kind(source, kind, original, default_channel);
    true
}

/// Compact preset picker: one segmented control and the selected preset's description.
fn mod_preset_row(ui: &mut egui::Ui, preset: &mut ModPreset) -> bool {
    let mut idx = match *preset {
        ModPreset::Essentials => 0,
        ModPreset::EssentialsDevToolkit => 1,
        ModPreset::Vanilla => 2,
    };
    let changed = widgets::segmented(
        ui,
        "new-instance-mods",
        &mut idx,
        &[
            ("Essentials", None),
            ("+ Developer Toolkit", None),
            ("Vanilla", None),
        ],
    );
    if changed {
        *preset = match idx {
            1 => ModPreset::EssentialsDevToolkit,
            2 => ModPreset::Vanilla,
            _ => ModPreset::Essentials,
        };
    }
    let description = match *preset {
        ModPreset::Essentials => "The mods most worlds want.",
        ModPreset::EssentialsDevToolkit => "Essentials, plus overlays and debug tools.",
        ModPreset::Vanilla => "The game as shipped, with no mods.",
    };
    let p = theme::palette(ui.ctx());
    ui.label(
        egui::RichText::new(description)
            .font(theme::regular(12.0))
            .color(p.text_muted),
    );
    changed
}

fn channel_field(ui: &mut egui::Ui, id_salt: &str, channel: &mut String, branches: &[String]) {
    if branches.is_empty() {
        widgets::text_input_with_id(
            ui,
            (id_salt, "text"),
            channel,
            "Channel",
            ui.available_width(),
        );
        return;
    }
    let shown = if channel.is_empty() {
        "Channel".to_owned()
    } else {
        channel.clone()
    };
    let width = ui.available_width().min(420.0);
    egui::ComboBox::from_id_salt(id_salt)
        .width(width)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for name in branches {
                ui.selectable_value(channel, name.clone(), name);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_to_latest_does_not_panic() {
        let mut source = SourceChoice::Linked {
            path: "/tmp/game".to_owned(),
            repositories: vec!["/tmp/mods".into()],
        };
        set_source_kind(&mut source, SourceKind::Managed, None, "main");
        match source {
            SourceChoice::Managed {
                channel,
                engine_ref,
                mods_ref,
            } => {
                assert_eq!(channel, "main");
                assert_eq!(engine_ref, watt_core::meta::AUTO);
                assert_eq!(mods_ref, watt_core::meta::AUTO);
            }
            other => panic!("expected a managed source, got {other:?}"),
        }
    }

    #[test]
    fn switching_back_restores_hidden_fields() {
        let original = SourceChoice::Managed {
            channel: "experimental".to_owned(),
            engine_ref: "abc".to_owned(),
            mods_ref: "def".to_owned(),
        };
        let mut source = original.clone();
        set_source_kind(&mut source, SourceKind::Managed, Some(&original), "main");
        if let SourceChoice::Managed { channel, .. } = &mut source {
            *channel = "mp-fix".to_owned();
        }
        set_source_kind(&mut source, SourceKind::Linked, Some(&original), "main");
        match &source {
            SourceChoice::Linked { path, repositories } => {
                assert!(path.is_empty());
                assert!(repositories.is_empty());
            }
            other => panic!("expected a linked source, got {other:?}"),
        }
        set_source_kind(&mut source, SourceKind::Managed, Some(&original), "main");
        assert_eq!(source, original);
    }

    #[test]
    fn pending_op_directory_is_rejected() {
        let mut app = App::for_snapshot_tests();
        app.start_op_for_tests(
            "incoming",
            OpRequest::Create {
                name: "incoming".to_owned(),
                display_name: None,
                color: None,
                group: None,
                source: watt_core::ops::SourceSpec::PwcDefault,
                mods: Vec::new(),
                build: false,
            },
        );
        let taken = dialogs::taken_dirs(&app);
        assert!(dialogs::valid_name("incoming", &taken).is_err());
        assert!(dialogs::valid_name("survival", &taken).is_err());
        assert!(dialogs::valid_name("brand-new", &taken).is_ok());
    }
}
