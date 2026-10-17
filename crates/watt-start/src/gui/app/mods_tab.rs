//! The mods tab: installed list, browse column and the sticky apply bar.
//!
//! Moved out of `instance_page` without a redesign.

use watt_core::ops::OpRequest;

use super::App;
use crate::gui::backend::{InstanceEntry, Loaded};
use crate::gui::theme;

impl App {
    pub(crate) fn mods_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
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
                if ui.button(apply_label).clicked()
                    && self.start_op(
                        name.clone(),
                        OpRequest::Mods {
                            instance: name.clone(),
                            add: staged.add.clone(),
                            remove: staged.remove.clone(),
                            build: None,
                        },
                    )
                {
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
}
