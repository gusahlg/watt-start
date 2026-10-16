//! The "New instance" and "Change source" modal dialogs.

use watt_core::meta::Binding;
use watt_core::ops::OpRequest;

use super::{App, MainView, ModPreset, SourceChoice};
use crate::gui::backend::Loaded;
use crate::gui::theme;

/// State of the "New instance" dialog.
pub(crate) struct NewInstanceState {
    pub name: String,
    pub name_edited: bool,
    pub display_name: String,
    pub color: egui::Color32,
    pub source: SourceChoice,
    pub mod_preset: ModPreset,
    pub build_now: bool,
}

/// State of the "Change source" dialog, for an existing instance.
pub(crate) struct ChangeSourceState {
    pub instance: String,
    pub source: SourceChoice,
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(64).collect()
}

fn valid_name(s: &str, taken: &[String]) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 64 {
        return Err("1-64 characters");
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("must start with a lowercase letter or digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("only lowercase letters, digits and `-`");
    }
    if taken.iter().any(|t| t == s) {
        return Err("an instance with this name already exists");
    }
    Ok(())
}

impl App {
    pub(crate) fn open_new_instance_dialog(&mut self) {
        self.new_instance = Some(NewInstanceState {
            name: String::new(),
            name_edited: false,
            display_name: String::new(),
            color: theme::WATT,
            source: SourceChoice::Managed {
                channel: self.backend.settings().default_channel.clone(),
            },
            mod_preset: ModPreset::Essentials,
            build_now: true,
        });
    }

    pub(crate) fn open_change_source_dialog(&mut self, instance: &str) {
        let source = self
            .backend
            .instances()
            .iter()
            .find(|e| e.card.name == instance)
            .map(|e| match &e.card.meta.source {
                Binding::Managed { channel, .. } => SourceChoice::Managed {
                    channel: channel.clone(),
                },
                Binding::Linked { pwc_source, .. } => SourceChoice::Linked {
                    path: pwc_source.display().to_string(),
                },
                Binding::PwcDefault => SourceChoice::PwcDefault,
            })
            .unwrap_or(SourceChoice::PwcDefault);
        self.change_source = Some(ChangeSourceState {
            instance: instance.to_owned(),
            source,
        });
    }

    pub(crate) fn new_instance_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.new_instance else {
            return;
        };
        if !state.name_edited {
            state.name = slugify(&state.display_name.clone());
        }
        let taken: Vec<String> = self
            .backend
            .instances()
            .iter()
            .map(|e| e.card.name.clone())
            .collect();
        let name_err = valid_name(&state.name, &taken).err();
        let branches = self.backend.branches();

        let mut close = false;
        let mut create = false;
        egui::Modal::new(egui::Id::unique("new_instance")).show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.heading("New instance");
            ui.add_space(8.0);

            ui.label("Display name");
            ui.add(egui::TextEdit::singleline(&mut state.display_name).hint_text("Survival"));
            ui.add_space(6.0);
            ui.label("Directory name");
            if ui
                .add(egui::TextEdit::singleline(&mut state.name).hint_text("survival"))
                .changed()
            {
                state.name_edited = true;
            }
            if let Some(err) = &name_err {
                ui.colored_label(theme::BAD, *err);
            }
            ui.add_space(6.0);
            ui.label("Colour");
            ui.horizontal(|ui| {
                for c in theme::INSTANCE_PALETTE {
                    let swatch = ui.add(swatch_button(c, state.color == c));
                    if swatch.clicked() {
                        state.color = c;
                    }
                }
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(theme::subheading("Source"));
            let mut kind = source_kind(&state.source);
            ui.radio_value(&mut kind, 0, "Latest game (recommended)");
            if kind == 0 {
                let SourceChoice::Managed { channel } = &mut state.source else {
                    unreachable!()
                };
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    channel_field(ui, "new_channel", channel, &branches);
                });
            }
            ui.radio_value(&mut kind, 1, "My game checkout");
            if kind == 1 {
                if !matches!(state.source, SourceChoice::Linked { .. }) {
                    state.source = SourceChoice::Linked {
                        path: String::new(),
                    };
                }
                let SourceChoice::Linked { path } = &mut state.source else {
                    unreachable!()
                };
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label("Path");
                    ui.text_edit_singleline(path);
                });
            }
            ui.radio_value(&mut kind, 2, "pwc's configured checkout");
            if kind == 2 {
                state.source = SourceChoice::PwcDefault;
            }
            if kind == 0 && !matches!(state.source, SourceChoice::Managed { .. }) {
                state.source = SourceChoice::Managed {
                    channel: self.backend.settings().default_channel.clone(),
                };
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(theme::subheading("Mods"));
            for preset in [
                ModPreset::Essentials,
                ModPreset::EssentialsDevToolkit,
                ModPreset::Vanilla,
            ] {
                ui.radio_value(&mut state.mod_preset, preset, preset.label());
            }

            ui.add_space(10.0);
            ui.checkbox(&mut state.build_now, "Build right away");
            if state.build_now {
                ui.label(theme::muted(
                    "A first build takes 15-40 minutes depending on your machine.",
                ));
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(name_err.is_none(), egui::Button::new("Create"))
                        .clicked()
                    {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });

        if create {
            let state = self.new_instance.take().unwrap();
            let request = OpRequest::Create {
                name: state.name.clone(),
                display_name: (!state.display_name.trim().is_empty())
                    .then_some(state.display_name.clone()),
                color: Some(theme::to_hex_color(state.color)),
                group: None,
                source: state.source.to_spec(),
                mods: state.mod_preset.specs(),
                build: state.build_now,
            };
            self.start_op(state.name.clone(), request);
            self.main_view = Some(MainView::Instance(state.name));
        } else if close {
            self.new_instance = None;
        }
    }

    pub(crate) fn change_source_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.change_source else {
            return;
        };
        let branches = self.backend.branches();
        let mut close = false;
        let mut apply = false;
        egui::Modal::new(egui::Id::unique("change_source")).show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.heading(format!("Change source: {}", state.instance));
            ui.add_space(8.0);
            let mut kind = source_kind(&state.source);
            ui.radio_value(&mut kind, 0, "Latest game (a channel)");
            if kind == 0 {
                if !matches!(state.source, SourceChoice::Managed { .. }) {
                    state.source = SourceChoice::Managed {
                        channel: "main".to_owned(),
                    };
                }
                let SourceChoice::Managed { channel } = &mut state.source else {
                    unreachable!()
                };
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    channel_field(ui, "change_channel", channel, &branches);
                });
            }
            ui.radio_value(&mut kind, 1, "My game checkout");
            if kind == 1 {
                if !matches!(state.source, SourceChoice::Linked { .. }) {
                    state.source = SourceChoice::Linked {
                        path: String::new(),
                    };
                }
                let SourceChoice::Linked { path } = &mut state.source else {
                    unreachable!()
                };
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label("Path");
                    ui.text_edit_singleline(path);
                });
            }
            ui.radio_value(&mut kind, 2, "pwc's configured checkout");
            if kind == 2 {
                state.source = SourceChoice::PwcDefault;
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });

        if apply {
            let state = self.change_source.take().unwrap();
            self.start_op(
                state.instance.clone(),
                OpRequest::SetSource {
                    instance: state.instance,
                    source: state.source.to_spec(),
                    build: None,
                },
            );
        } else if close {
            self.change_source = None;
        }
    }
}

/// "Channel" label plus either a combo box of known branches (populated in the background from
/// the game mirror) or a plain text field while they are not known yet.
fn channel_field(
    ui: &mut egui::Ui,
    id_salt: &str,
    channel: &mut String,
    branches: &Loaded<Vec<watt_core::git::BranchInfo>>,
) {
    ui.label("Channel");
    match branches {
        Loaded::Ready(list) if !list.is_empty() => {
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(channel.clone())
                .show_ui(ui, |ui| {
                    for b in list {
                        if ui
                            .selectable_label(channel.as_str() == b.name, &b.name)
                            .clicked()
                        {
                            *channel = b.name.clone();
                        }
                    }
                });
        }
        _ => {
            ui.add(egui::TextEdit::singleline(channel).desired_width(140.0));
        }
    }
}

fn source_kind(s: &SourceChoice) -> u8 {
    match s {
        SourceChoice::Managed { .. } => 0,
        SourceChoice::Linked { .. } => 1,
        SourceChoice::PwcDefault => 2,
    }
}

fn swatch_button(color: egui::Color32, selected: bool) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let size = egui::vec2(22.0, 22.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 6u8, color);
        if selected {
            painter.rect_stroke(
                rect,
                6u8,
                egui::Stroke::new(2.0, egui::Color32::WHITE),
                egui::StrokeKind::Outside,
            );
        }
        response
    }
}
