//! The settings page: preferences, advanced source configuration, maintenance and system checks.

use watt_core::doctor::Level;
use watt_core::ops::OpRequest;
use watt_core::paths::Repo;
use watt_core::settings::WrapperSetting;

use super::App;
use crate::gui::backend::Loaded;
use crate::gui::theme;

impl App {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(680.0);
                ui.add_space(8.0);
                ui.label(theme::title("Settings"));
                ui.add_space(16.0);

                let mut changed = false;
                let s = &mut self.settings_draft;

                ui.label(theme::subheading("Updates and builds"));
                changed |= ui
                    .checkbox(
                        &mut s.rebuild_after_update,
                        "Recompile automatically after updates",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut s.rebuild_after_mod_change,
                        "Recompile automatically after mod changes",
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut s.check_updates_on_start, "Check for updates on start")
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut s.backup_before_update,
                        "Back up worlds before an update changes the game",
                    )
                    .changed();
                if s.backup_before_update {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label("Backups to keep per instance");
                        changed |= ui
                            .add(egui::DragValue::new(&mut s.backups_to_keep).range(1..=20))
                            .changed();
                    });
                }
                changed |= ui
                    .checkbox(
                        &mut s.close_on_play,
                        "Hide the launcher while the game runs",
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Generations kept per instance");
                    changed |= ui
                        .add(egui::DragValue::new(&mut s.generations_to_keep).range(1..=50))
                        .changed();
                });

                ui.add_space(14.0);
                ui.label(theme::subheading("Appearance"));
                changed |= ui.checkbox(&mut s.appearance.dark, "Dark theme").changed();
                changed |= ui
                    .checkbox(&mut s.appearance.animations, "Animations")
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Scale");
                    changed |= ui
                        .add(egui::Slider::new(&mut s.appearance.scale, 0.75..=1.75).step_by(0.05))
                        .changed();
                });

                ui.add_space(14.0);
                ui.label(theme::subheading("Build"));
                ui.horizontal(|ui| {
                    ui.label("Parallel jobs (0 = automatic)");
                    changed |= ui
                        .add(egui::DragValue::new(&mut s.build_jobs).range(0..=256))
                        .changed();
                    ui.label(theme::muted(format!("effective: {}", s.effective_jobs())));
                });
                ui.horizontal(|ui| {
                    ui.label("Build priority (nice)");
                    changed |= ui
                        .add(egui::Slider::new(&mut s.build_nice, 0..=19))
                        .changed();
                });
                ui.label(theme::muted(
                    "Lower = faster builds, higher = a smoother desktop while one runs.",
                ));

                ui.add_space(14.0);
                egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Default channel");
                        changed |= ui.text_edit_singleline(&mut s.default_channel).changed();
                    });
                    ui.add_space(6.0);
                    ui.label("Remotes (empty = the public GitHub repository)");
                    for repo in Repo::ALL {
                        let mut value = s.remotes.get(&repo).cloned().unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(repo.label());
                            if ui.text_edit_singleline(&mut value).changed() {
                                if value.is_empty() {
                                    s.remotes.remove(&repo);
                                } else {
                                    s.remotes.insert(repo, value);
                                }
                                changed = true;
                            }
                        });
                    }
                    ui.add_space(6.0);
                    ui.label("Extra package repositories");
                    let mut remove_at = None;
                    for (i, path) in s.extra_repositories.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let mut text = path.display().to_string();
                            if ui.text_edit_singleline(&mut text).changed() {
                                *path = text.into();
                                changed = true;
                            }
                            if ui.button("Remove").clicked() {
                                remove_at = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove_at {
                        s.extra_repositories.remove(i);
                        changed = true;
                    }
                    if ui.button("+ Add repository").clicked() {
                        s.extra_repositories.push(Default::default());
                        changed = true;
                    }
                    ui.add_space(6.0);
                    ui.label("Wrapper");
                    let mut follow_pwc = matches!(s.wrapper, WrapperSetting::FollowPwc);
                    ui.horizontal(|ui| {
                        if ui.radio(follow_pwc, "Follow pwc").clicked() && !follow_pwc {
                            follow_pwc = true;
                            s.wrapper = WrapperSetting::FollowPwc;
                            changed = true;
                        }
                        if ui.radio(!follow_pwc, "None").clicked() && follow_pwc {
                            s.wrapper = WrapperSetting::None;
                            changed = true;
                        }
                    });
                });

                if changed {
                    self.backend.save_settings(self.settings_draft.clone());
                }

                ui.add_space(20.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(theme::subheading("Maintenance"));
                ui.horizontal(|ui| {
                    if ui.button("Clean build cache").clicked() {
                        self.ask_maintenance(
                            "Clean the build cache?",
                            "The next build of every instance starts from scratch. This can \
                             take a long time.",
                            OpRequest::Maintenance {
                                clean_cache: true,
                                gc_snapshots: false,
                                gc_store: false,
                                dry_run: false,
                            },
                        );
                    }
                    if ui.button("Remove unused sources").clicked() {
                        self.ask_maintenance(
                            "Remove unused sources?",
                            "Deletes source snapshots no instance or generation uses.",
                            OpRequest::Maintenance {
                                clean_cache: false,
                                gc_snapshots: true,
                                gc_store: false,
                                dry_run: false,
                            },
                        );
                    }
                    if ui.button("Remove unused packages").clicked() {
                        self.ask_maintenance(
                            "Remove unused packages?",
                            "Deletes store packages no lock or generation uses.",
                            OpRequest::Maintenance {
                                clean_cache: false,
                                gc_snapshots: false,
                                gc_store: true,
                                dry_run: false,
                            },
                        );
                    }
                });

                ui.add_space(20.0);
                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(theme::subheading("System checks"));
                    if ui.button("Run again").clicked() {
                        self.backend.run_doctor();
                    }
                });
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
                            if !check.hint.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(20.0);
                                    ui.label(theme::muted(check.hint.clone()));
                                });
                            }
                        }
                    }
                    Loaded::Loading | Loaded::Idle => {
                        ui.label(theme::muted("Running checks…"));
                    }
                    Loaded::Err(e) => {
                        ui.colored_label(theme::BAD, format!("Could not run checks: {e}"));
                    }
                }
                ui.add_space(20.0);
            });
    }
}
