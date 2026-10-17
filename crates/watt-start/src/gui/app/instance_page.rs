//! The instance page: header, action row, status banner, tab strip and forge placement.
//!
//! Tab bodies live in [`super::instance_tabs`] and [`super::mods_tab`].

use watt_core::meta::{Binding, short};
use watt_core::ops::OpRequest;
use watt_core::status::Readiness;

use super::instance_tabs::short_env;
use super::{App, InstanceTab};
use crate::gui::backend::InstanceEntry;
use crate::gui::forge::ForgeStyle;
use crate::gui::theme;

impl App {
    pub(crate) fn instance_page(&mut self, ui: &mut egui::Ui, name: &str) {
        let Some(entry) = self
            .backend
            .instances()
            .iter()
            .find(|e| e.card.name == name)
            .cloned()
        else {
            return;
        };

        if let Some(slot) = self.ops.get(name)
            && !slot.hidden
            && (slot.view.running() || slot.view.outcome.is_some())
        {
            self.forge_view(ui, name, &entry);
            return;
        }

        self.header(ui, &entry);
        self.action_row(ui, &entry);
        self.status_banner(ui, &entry);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (InstanceTab::Overview, "Overview"),
                (InstanceTab::Mods, "Mods"),
                (InstanceTab::Builds, "Builds"),
                (InstanceTab::Log, "Log"),
                (InstanceTab::InstanceSettings, "Instance settings"),
            ] {
                if ui
                    .selectable_label(self.instance_tab == tab, label)
                    .clicked()
                {
                    self.instance_tab = tab;
                }
            }
        });
        ui.separator();
        ui.add_space(8.0);

        match self.instance_tab {
            InstanceTab::Overview => self.overview_tab(ui, &entry),
            InstanceTab::Mods => self.mods_tab(ui, &entry),
            InstanceTab::Builds => self.builds_tab(ui, &entry),
            InstanceTab::Log => self.log_tab(ui, &entry),
            InstanceTab::InstanceSettings => self.instance_settings_tab(ui, &entry),
        }
    }

    fn forge_view(&mut self, ui: &mut egui::Ui, name: &str, entry: &InstanceEntry) {
        let accent = theme::accent_for_meta(name, entry.card.meta.color.as_deref());
        let expected_ms = entry
            .card
            .meta
            .generations
            .iter()
            .find_map(|g| g.build.as_ref())
            .map(|b| b.duration_ms);
        let style = ForgeStyle {
            accent,
            animate: self.settings_draft.appearance.animations,
            expected_ms,
            title: entry.card.meta.title(name).to_owned(),
            elapsed_ms: self
                .ops
                .get(name)
                .map(|s| s.started.elapsed().as_millis() as u64),
        };
        let view = self
            .ops
            .get(name)
            .map(|s| s.view.clone())
            .unwrap_or_default();
        let response = (self.forge.forge)(ui, &view, &style);

        if response.cancel
            && let Some(slot) = self.ops.get_mut(name)
        {
            slot.op.cancel();
        }
        if response.hide
            && let Some(slot) = self.ops.get_mut(name)
        {
            slot.hidden = true;
        }
        if response.close {
            self.dismiss_op(name);
        }
        if response.play {
            self.dismiss_op(name);
            self.play(name, false);
        }
        if let Some(recovery) = response.recovery {
            self.apply_recovery(name, recovery);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let accent = theme::accent_for_meta(&entry.card.name, entry.card.meta.color.as_deref());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::heading(entry.card.meta.title(&entry.card.name)));
                // A short underline under the title (not a full-width bar, which reads too much
                // like a divider or progress line) is enough of an accent cue.
                let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 2.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 1u8, accent);
                ui.label(theme::muted(entry.card.dir.display().to_string()));
            });
        });
        ui.add_space(8.0);

        ui.horizontal_wrapped(|ui| {
            chip(
                ui,
                &format!("Game {}", entry.card.game.version.as_deref().unwrap_or("-")),
            );
            let branch_rev = match &entry.card.meta.source {
                Binding::Managed { channel, .. } => entry
                    .card
                    .game
                    .revision
                    .as_deref()
                    .map(|r| format!("{channel} @ {}", short(r)))
                    .unwrap_or_else(|| channel.clone()),
                Binding::Linked { .. } | Binding::PwcDefault => entry
                    .card
                    .game
                    .branch
                    .clone()
                    .unwrap_or_else(|| "?".to_owned()),
            };
            chip(ui, &branch_rev);
            if let Some(engine) = &entry.card.game.engine_revision {
                chip(ui, &format!("Engine {}", short(engine)));
            }
            if let Some(api) = &entry.card.game.api {
                chip(ui, &format!("Mod API {api}"));
            }
            if let Some(env) = &entry.card.game.environment {
                chip(ui, &format!("env {}", short_env(env)));
            }
            chip(ui, &format!("{} mods", entry.card.packages.len()));
            chip(ui, entry.card.manifest.profile.as_str());
        });
        ui.add_space(8.0);
    }

    fn action_row(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let running = self.op_kind_running(&name);
        let accent = theme::accent_for_meta(&name, entry.card.meta.color.as_deref());
        ui.horizontal(|ui| {
            // The one unmistakable primary action: big, filled with the instance's accent (or
            // amber when it has none of its own), dark bold text so it reads on any accent.
            let play_label = if running == Some(watt_core::events::OpKind::Play) {
                "Playing…"
            } else if matches!(
                entry.status.readiness,
                Readiness::New | Readiness::NeedsBuild { .. }
            ) {
                "Build & play"
            } else {
                "Play"
            };
            let play_text = egui::RichText::new(play_label)
                .size(16.0)
                .color(egui::Color32::from_rgb(0x14, 0x16, 0x1e))
                .strong();
            let play_button = egui::Button::new(play_text)
                .fill(accent)
                .corner_radius(10u8)
                .min_size(egui::vec2(160.0, 44.0));
            if ui.add_enabled(running.is_none(), play_button).clicked() {
                self.play(&name, false);
            }

            if ui
                .add_enabled(running.is_none(), egui::Button::new("Update"))
                .clicked()
            {
                self.start_op(
                    name.clone(),
                    OpRequest::Update {
                        instance: name.clone(),
                        build: None,
                        reset_lock: false,
                    },
                );
            }
            let update_count = self.updates.get(&name).map(Vec::len).unwrap_or(0);
            if update_count > 0 {
                egui::Frame::new()
                    .fill(theme::CYAN)
                    .corner_radius(255u8)
                    .inner_margin(egui::Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(0x08, 0x1a, 0x1a),
                            update_count.to_string(),
                        );
                    });
            }
            if ui
                .add_enabled(running.is_none(), egui::Button::new("Recompile"))
                .clicked()
            {
                self.start_op(
                    name.clone(),
                    OpRequest::Build {
                        instance: name.clone(),
                        force: true,
                        profile: None,
                    },
                );
            }

            ui.menu_button("...", |ui| {
                if ui.button("Duplicate…").clicked() {
                    self.open_duplicate_dialog(&name);
                    ui.close();
                }
                if ui.button("Change source…").clicked() {
                    self.open_change_source_dialog(&name);
                    ui.close();
                }
                if ui.button("Open game folder").clicked() {
                    self.backend.open_path(&entry.card.dir.join("game"));
                    ui.close();
                }
                if ui.button("Open log folder").clicked() {
                    self.backend
                        .open_path(&entry.card.dir.join("watt-start").join("logs"));
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete…").clicked() {
                    self.ask_delete_instance(&name);
                    ui.close();
                }
            });
        });
    }

    fn status_banner(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        match &entry.status.readiness {
            Readiness::NeedsBuild { reason } => {
                ui.add_space(6.0);
                banner(ui, theme::WARN, reason, || {});
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    if ui.button("Build now").clicked() {
                        self.start_op(
                            name.clone(),
                            OpRequest::Build {
                                instance: name.clone(),
                                force: false,
                                profile: None,
                            },
                        );
                    }
                });
            }
            Readiness::Broken { reason } => {
                ui.add_space(6.0);
                banner(ui, theme::BAD, reason, || {});
            }
            Readiness::New | Readiness::Ready { .. } => {}
        }
        if let Some(last_op) = &entry.card.meta.last_op
            && !last_op.ok
            && self.op_kind_running(&name).is_none()
        {
            ui.add_space(6.0);
            let message = last_op
                .error
                .clone()
                .unwrap_or_else(|| "The last operation failed.".to_owned());
            banner(
                ui,
                theme::BAD,
                &format!("Last {:?} failed: {message}", last_op.op),
                || {},
            );
        }
    }
}

fn chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::BG_2)
        .corner_radius(255u8)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(text);
        });
}

fn banner(ui: &mut egui::Ui, color: egui::Color32, text: &str, _extra: impl FnOnce()) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.18))
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(8u8)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.colored_label(color, text);
        });
}
