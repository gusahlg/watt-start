//! The instance page: header, action row, status banner and tabs.

use watt_core::meta::{Binding, short};
use watt_core::ops::OpRequest;
use watt_core::status::Readiness;

use super::{App, InstanceTab};
use crate::gui::backend::{InstanceEntry, Loaded, MetaEdit};
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
            self.ops.remove(name);
        }
        if response.play {
            self.ops.remove(name);
            self.play(name, false);
        }
        if let Some(recovery) = response.recovery {
            self.ops.remove(name);
            self.apply_recovery(name, recovery);
        }
    }

    fn apply_recovery(&mut self, name: &str, recovery: watt_core::events::Recovery) {
        use watt_core::events::Recovery;
        match recovery {
            Recovery::Retry => self.start_op(
                name,
                OpRequest::Build {
                    instance: name.to_owned(),
                    force: false,
                    profile: None,
                },
            ),
            Recovery::ResetLock => self.start_op(
                name,
                OpRequest::Update {
                    instance: name.to_owned(),
                    build: None,
                    reset_lock: true,
                },
            ),
            Recovery::Rollback => self.start_op(
                name,
                OpRequest::Rollback {
                    instance: name.to_owned(),
                    generation: None,
                },
            ),
            Recovery::PlayLastGood => self.play(name, true),
            Recovery::ForceRebuild => self.start_op(
                name,
                OpRequest::Build {
                    instance: name.to_owned(),
                    force: true,
                    profile: None,
                },
            ),
            Recovery::Doctor => self.main_view = Some(super::MainView::Settings),
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let accent = theme::accent_for_meta(&entry.card.name, entry.card.meta.color.as_deref());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(theme::heading(entry.card.meta.title(&entry.card.name)));
                ui.label(theme::muted(entry.card.dir.display().to_string()));
            });
        });
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2u8, accent);
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
                    let new_name = format!("{name}-copy");
                    self.start_op(
                        new_name.clone(),
                        OpRequest::Duplicate {
                            instance: name.clone(),
                            new_name,
                            display_name: None,
                            with_worlds: false,
                        },
                    );
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

    fn overview_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
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

    fn mods_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let staged = self.staged_mods.entry(name.clone()).or_default().clone();

        // Reserve the sticky bar's strip up front (rather than appending it after content of
        // unknown height) so the columns below it are capped to leave it clear, instead of a
        // card's bottom edge landing underneath it. This has to be a hard ceiling on the whole
        // two-column block's *own* `Ui`, not just a `ScrollArea::max_height`: each column's
        // search box/category chips eat a different amount of height before its scroll area
        // even starts, so a `max_height` sized for the outer budget alone still lets the
        // shallower column's `auto_shrink(false)` scroll area fill everything beneath it.
        const BAR_HEIGHT: f32 = 50.0;
        let content_height = if staged.is_empty() {
            ui.available_height()
        } else {
            (ui.available_height() - BAR_HEIGHT).max(100.0)
        };
        let content_width = ui.available_width();

        ui.allocate_ui(egui::vec2(content_width, content_height), |ui| {
            ui.columns(2, |cols| {
                cols[0].label(theme::subheading("Installed"));
                cols[0].add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt("installed")
                    .auto_shrink([false, false])
                    .show(&mut cols[0], |ui| {
                        for pkg in &entry.card.packages {
                            if pkg.kind == "bundle" {
                                egui::CollapsingHeader::new(format!(
                                    "{} ({})",
                                    pkg.name, pkg.version
                                ))
                                .show(ui, |ui| {
                                    for dep in &pkg.dependencies {
                                        ui.label(theme::muted(dep.clone()));
                                    }
                                });
                                continue;
                            }
                            ui.horizontal(|ui| {
                                let removing = staged.remove.contains(&pkg.id);
                                if pkg.direct {
                                    let mut keep = !removing;
                                    if ui.checkbox(&mut keep, "").changed() {
                                        self.toggle_remove(&name, &pkg.id, !keep);
                                    }
                                    ui.label(&pkg.name);
                                } else {
                                    ui.add_space(20.0);
                                    ui.label(theme::muted(&pkg.name));
                                    let via = pkg
                                        .in_bundles
                                        .first()
                                        .cloned()
                                        .unwrap_or_else(|| "a dependency".to_owned());
                                    ui.label(theme::muted(format!("(pulled in by {via})")));
                                }
                            });
                        }
                    });

                cols[1].label(theme::subheading("Browse"));
                cols[1].add_space(6.0);
                cols[1].horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mods_browse_search)
                            .hint_text("Search mods"),
                    );
                    ui.checkbox(&mut self.mods_show_libraries, "Show libraries");
                });
                let (available, _warnings) = match self.backend.available_mods(&name) {
                    Loaded::Ready(v) => v,
                    _ => (Vec::new(), Vec::new()),
                };
                let mut categories: Vec<String> = available
                    .iter()
                    .flat_map(|m| m.categories.clone())
                    .collect();
                categories.sort();
                categories.dedup();
                cols[1].horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(self.mods_browse_category.is_none(), "All")
                        .clicked()
                    {
                        self.mods_browse_category = None;
                    }
                    for cat in &categories {
                        if ui
                            .selectable_label(
                                self.mods_browse_category.as_deref() == Some(cat),
                                cat,
                            )
                            .clicked()
                        {
                            self.mods_browse_category = Some(cat.clone());
                        }
                    }
                });
                cols[1].add_space(6.0);
                let search = self.mods_browse_search.to_lowercase();
                let installed_ids: Vec<String> =
                    entry.card.packages.iter().map(|p| p.id.clone()).collect();
                let accent = theme::accent_for_meta(&name, entry.card.meta.color.as_deref());
                let mut undo_add = None;
                let mut undo_remove = None;
                let mut add = None;
                egui::ScrollArea::vertical()
                    .id_salt("browse")
                    .auto_shrink([false, false])
                    .show(&mut cols[1], |ui| {
                        for m in &available {
                            if !self.mods_show_libraries && m.kind == "library" {
                                continue;
                            }
                            if let Some(cat) = &self.mods_browse_category
                                && !m.categories.contains(cat)
                            {
                                continue;
                            }
                            if !search.is_empty()
                                && !m.name.to_lowercase().contains(&search)
                                && !m.id.to_lowercase().contains(&search)
                            {
                                continue;
                            }
                            let installed = installed_ids.contains(&m.id);
                            let staged_remove = installed && staged.remove.contains(&m.id);
                            let staged_add = staged
                                .add
                                .iter()
                                .any(|s| s == &m.id || s.starts_with(&format!("{}@", m.id)));
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(format!("{} {}", m.name, m.version));
                                        ui.label(theme::muted(m.description.clone()));
                                        ui.label(theme::muted(m.authors.join(", ")));
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if staged_add {
                                                if ui.small_button("Undo").clicked() {
                                                    undo_add = Some(m.id.clone());
                                                }
                                                ui.colored_label(accent, "Will be added");
                                            } else if staged_remove {
                                                if ui.small_button("Undo").clicked() {
                                                    undo_remove = Some(m.id.clone());
                                                }
                                                ui.colored_label(theme::BAD, "Will be removed");
                                            } else if installed {
                                                ui.label(theme::muted("Installed"));
                                            } else if ui.button("Add").clicked() {
                                                add = Some(m.id.clone());
                                            }
                                        },
                                    );
                                });
                            });
                        }
                    });
                if let Some(id) = add {
                    self.staged_mods
                        .entry(name.clone())
                        .or_default()
                        .add
                        .push(id);
                }
                if let Some(id) = undo_add {
                    self.staged_mods
                        .entry(name.clone())
                        .or_default()
                        .add
                        .retain(|x| x != &id);
                }
                if let Some(id) = undo_remove {
                    self.toggle_remove(&name, &id, false);
                }
            });
        });

        // The sticky bar is drawn last, in the height already reserved for it above, so it never
        // overlaps the (now correctly capped) columns.
        if !staged.is_empty() {
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!(
                    "+{} −{} changes",
                    staged.add.len(),
                    staged.remove.len()
                ));
                let apply_label = if self.settings_draft.rebuild_after_mod_change {
                    "Apply & rebuild"
                } else {
                    "Apply"
                };
                if ui.button(apply_label).clicked() {
                    self.start_op(
                        name.clone(),
                        OpRequest::Mods {
                            instance: name.clone(),
                            add: staged.add.clone(),
                            remove: staged.remove.clone(),
                            build: None,
                        },
                    );
                    self.staged_mods.remove(&name);
                }
                if ui.button("Discard").clicked() {
                    self.staged_mods.remove(&name);
                }
            });
        }
    }

    fn toggle_remove(&mut self, instance: &str, id: &str, remove: bool) {
        let staged = self.staged_mods.entry(instance.to_owned()).or_default();
        staged.remove.retain(|x| x != id);
        if remove {
            staged.remove.push(id.to_owned());
        }
    }

    fn builds_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
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

    fn log_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
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

    fn instance_settings_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        ui.label(theme::subheading("Display name"));
        let mut display_name = entry
            .card
            .meta
            .display_name
            .clone()
            .unwrap_or_else(|| name.clone());
        if ui.text_edit_singleline(&mut display_name).lost_focus() {
            let _ = self
                .backend
                .edit_meta(&name, MetaEdit::DisplayName(display_name));
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
        let mut group = entry.card.meta.group.clone().unwrap_or_default();
        if ui.text_edit_singleline(&mut group).lost_focus() {
            let _ = self.backend.edit_meta(&name, MetaEdit::Group(group));
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
fn short_env(env: &str) -> &str {
    short(env.strip_prefix("sha256:").unwrap_or(env))
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
