//! Instance page tab bodies: overview, builds, log and instance settings.
//!
//! Moved out of `instance_page` so a later phase can restyle these screens on their own.
//! Behaviour matches the previous file, except the display name and group keep a per-instance
//! draft so typing is not rebuilt from metadata every frame.

use watt_core::meta::{Binding, short};
use watt_core::ops::OpRequest;

use super::App;
use crate::gui::backend::{InstanceEntry, MetaEdit};
use crate::gui::theme;

impl App {
    pub(super) fn overview_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        ui.label(binding_sentence(&entry.card.meta.source));
        ui.add_space(10.0);

        if let Some(whats_new) = self.updates.get(&name).cloned()
            && !whats_new.is_empty()
        {
            ui.label(theme::subheading("Updates available"));
            for log in &whats_new {
                ui.label(egui::RichText::new(log.repo.label()).strong());
                for commit in &log.commits {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(theme::muted(short(&commit.sha)));
                        ui.label(&commit.summary);
                    });
                }
                if log.truncated {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(theme::muted("…and more"));
                    });
                }
            }
            ui.add_space(10.0);
        }

        ui.label(theme::subheading("History"));
        ui.horizontal(|ui| {
            ui.label("Last played:");
            ui.label(humanize_opt(entry.card.meta.last_played));
        });
        let last_build = entry
            .card
            .meta
            .generations
            .iter()
            .find_map(|g| g.build.as_ref().map(|b| b.finished));
        ui.horizontal(|ui| {
            ui.label("Last build:");
            ui.label(humanize_opt(last_build));
        });

        ui.add_space(10.0);
        ui.label(theme::subheading("Notes"));
        let draft = self
            .notes_draft
            .entry(name.clone())
            .or_insert_with(|| entry.card.meta.notes.clone());
        let response = ui.add(
            egui::TextEdit::multiline(draft)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        if response.lost_focus() {
            let text = draft.clone();
            let _ = self.backend.edit_meta(&name, MetaEdit::Notes(text));
        }
    }

    pub(super) fn builds_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let last_good_id = entry.status.last_good.as_ref().map(|g| g.id);
        let current_id = entry.card.meta.generations.first().map(|g| g.id);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for g in &entry.card.meta.generations {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("#{}", g.id));
                            if Some(g.id) == current_id {
                                ui.colored_label(theme::GOOD, "current");
                            }
                            ui.label(humanize(g.created));
                            ui.label(&g.game_version);
                            ui.label(theme::muted(format!("env {}", short_env(&g.environment))));
                            if let Some(build) = &g.build {
                                ui.label(theme::muted(fmt_duration_ms(build.duration_ms)));
                            }
                            ui.label(theme::muted(format!("{} packages", g.packages.len())));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if Some(g.id) == last_good_id
                                        && ui.button("Play this build").clicked()
                                    {
                                        self.play(&name, true);
                                    }
                                    if Some(g.id) != current_id && ui.button("Roll back").clicked()
                                    {
                                        self.start_op(
                                            name.clone(),
                                            OpRequest::Rollback {
                                                instance: name.clone(),
                                                generation: Some(g.id),
                                            },
                                        );
                                    }
                                },
                            );
                        });
                    });
                }
            });
    }

    pub(super) fn log_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let log_file = self
            .ops
            .get(&name)
            .and_then(|s| s.view.log_file.clone())
            .or_else(|| entry.card.meta.last_op.as_ref().and_then(|o| o.log.clone()));
        if let Some(path) = &log_file
            && ui.button("Open log file").clicked()
        {
            self.backend.open_path(path);
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if let Some(slot) = self.ops.get(&name) {
                    for line in &slot.view.log {
                        ui.label(egui::RichText::new(line).monospace());
                    }
                } else {
                    ui.label(theme::muted("No log for this session yet."));
                }
            });
    }

    pub(super) fn instance_settings_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        ui.label(theme::subheading("Display name"));
        let save_display = {
            let draft = self
                .display_name_draft
                .entry(name.clone())
                .or_insert_with(|| {
                    entry
                        .card
                        .meta
                        .display_name
                        .clone()
                        .unwrap_or_else(|| name.clone())
                });
            let response = ui.text_edit_singleline(draft);
            response.lost_focus()
                || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
        };
        if save_display
            && let Some(text) = self.display_name_draft.remove(&name)
            && self
                .backend
                .edit_meta(&name, MetaEdit::DisplayName(text.clone()))
                .is_err()
        {
            self.display_name_draft.insert(name.clone(), text);
        }

        ui.add_space(10.0);
        ui.label(theme::subheading("Colour"));
        ui.horizontal(|ui| {
            let current = theme::accent_for_meta(&name, entry.card.meta.color.as_deref());
            for c in theme::INSTANCE_PALETTE {
                let size = egui::vec2(22.0, 22.0);
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                ui.painter().rect_filled(rect, 6u8, c);
                if c == current {
                    ui.painter().rect_stroke(
                        rect,
                        6u8,
                        egui::Stroke::new(2.0, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
                if response.clicked() {
                    let _ = self
                        .backend
                        .edit_meta(&name, MetaEdit::Color(theme::to_hex_color(c)));
                }
            }
        });

        ui.add_space(10.0);
        ui.label(theme::subheading("Group"));
        let save_group = {
            let draft = self
                .group_draft
                .entry(name.clone())
                .or_insert_with(|| entry.card.meta.group.clone().unwrap_or_default());
            let response = ui.text_edit_singleline(draft);
            response.lost_focus()
                || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
        };
        if save_group
            && let Some(text) = self.group_draft.remove(&name)
            && self
                .backend
                .edit_meta(&name, MetaEdit::Group(text.clone()))
                .is_err()
        {
            self.group_draft.insert(name.clone(), text);
        }

        ui.add_space(10.0);
        let mut favorite = entry.card.meta.favorite;
        if ui.checkbox(&mut favorite, "Favourite").changed() {
            let _ = self.backend.edit_meta(&name, MetaEdit::Favorite(favorite));
        }

        ui.add_space(10.0);
        ui.label(theme::subheading("Build profile"));
        ui.label(theme::muted(format!(
            "{} (set in instance.toml)",
            entry.card.manifest.profile.as_str()
        )));

        ui.add_space(14.0);
        ui.label(theme::subheading("Source"));
        ui.label(binding_sentence(&entry.card.meta.source));
        if ui.button("Change source…").clicked() {
            self.open_change_source_dialog(&name);
        }
    }
}

/// An environment hash's short form, without the leading `sha256:` ([`short`] alone would just
/// return that 7-character prefix).
pub(super) fn short_env(env: &str) -> &str {
    short(env.strip_prefix("sha256:").unwrap_or(env))
}

fn binding_sentence(binding: &Binding) -> String {
    match binding {
        Binding::Managed { channel, .. } => {
            format!("Follows the \"{channel}\" branch of the game.")
        }
        Binding::Linked { pwc_source, .. } => {
            format!("Built from your checkout at {}.", pwc_source.display())
        }
        Binding::PwcDefault => "Uses pwc's configured game checkout.".to_owned(),
    }
}

fn humanize(secs: u64) -> String {
    let now = watt_core::paths::now_secs();
    let diff = now.saturating_sub(secs);
    if diff < 60 {
        "just now".to_owned()
    } else if diff < 3600 {
        format!("{} min ago", diff / 60)
    } else if diff < 86_400 {
        format!("{} h ago", diff / 3600)
    } else {
        format!("{} d ago", diff / 86_400)
    }
}

fn humanize_opt(secs: Option<u64>) -> String {
    secs.map(humanize).unwrap_or_else(|| "never".to_owned())
}

fn fmt_duration_ms(ms: u64) -> String {
    if ms == 0 {
        return "cached".to_owned();
    }
    let total_secs = ms / 1000;
    if total_secs < 60 {
        format!("{total_secs} s")
    } else {
        format!("{} min {} s", total_secs / 60, total_secs % 60)
    }
}
