//! The first-run welcome screen: shown instead of the sidebar + instance page when there are no
//! instances yet.

use crate::gui::backend::Loaded;
use crate::gui::theme;
use watt_core::doctor::Level;

impl super::App {
    pub(crate) fn welcome_screen(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(64.0);
                (self.forge.cube_icon)(ui, 64.0, theme::WATT, false);
                ui.add_space(16.0);
                ui.label(theme::title("Welcome to watt-start"));
                ui.add_space(6.0);
                ui.label("Keep Project Watt Cubed instances up to date and built, in one click.");
                ui.add_space(24.0);

                ui.set_max_width(520.0);
                ui.group(|ui| {
                    ui.label(theme::subheading("System checks"));
                    ui.add_space(6.0);
                    match self.backend.doctor() {
                        Loaded::Ready(checks) => {
                            for check in &checks {
                                ui.horizontal(|ui| {
                                    let color = match check.level {
                                        Level::Ok => theme::GOOD,
                                        Level::Info => theme::CYAN,
                                        Level::Warn => theme::WARN,
                                        Level::Error => theme::BAD,
                                    };
                                    theme::dot(ui, color);
                                    ui.label(&check.name);
                                    ui.label(theme::muted(check.detail.clone()));
                                });
                            }
                        }
                        Loaded::Loading | Loaded::Idle => {
                            ui.label(theme::muted("Checking your machine…"));
                        }
                        Loaded::Err(e) => {
                            ui.colored_label(theme::BAD, format!("Could not run checks: {e}"));
                        }
                    }
                });

                ui.add_space(24.0);
                if ui
                    .add(
                        egui::Button::new("Create your first instance")
                            .min_size(egui::vec2(260.0, 40.0)),
                    )
                    .clicked()
                {
                    self.open_new_instance_dialog();
                }
            });
        });
    }
}
