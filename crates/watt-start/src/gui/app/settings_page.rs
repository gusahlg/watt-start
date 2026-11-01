//! The settings page: preferences, advanced source configuration, maintenance and system checks.
//!
//! A control that reports [`egui::Response::changed`] marks the draft dirty. [`super::App::ui`]
//! writes it about half a second later, so a drag does not save every frame. Theme, scale and
//! animations apply on the next frame, when [`super::App::ui`] calls [`crate::gui::theme::apply`].

use std::path::PathBuf;

use watt_core::doctor::Level;
use watt_core::events::OpOutcome;
use watt_core::ops::OpRequest;
use watt_core::paths::Repo;
use watt_core::settings::WrapperSetting;
use watt_core::view::OpView;

use super::App;
use super::dialogs::MAINTENANCE_KEY;
use crate::gui::backend::Loaded;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Icon, Size, Variant};

/// What the maintenance slot is showing, copied out so the card can mutably use [`App`].
enum MaintenanceSnap {
    Running { phase: String, fraction: f32 },
    Settled { summary: String, error: bool },
}

impl App {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        let maintenance = maintenance_snap(self);
        let doctor = self.backend.doctor();
        super::dialogs::visible_scrollbar(ui, 12.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let full = ui.available_width();
                ui.set_min_width(full);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(24, 24))
                    .show(ui, |ui| {
                        let column = ui.available_width().min(720.0);
                        ui.set_min_width(column);
                        ui.set_max_width(column);
                        ui.spacing_mut().item_spacing.y = 16.0;
                        page_title(ui);
                        let changed = preferences(ui, &mut self.settings_draft);
                        if changed {
                            self.settings_save_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(500),
                            );
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(500));
                        }
                        maintenance_card(ui, &maintenance, self);
                        doctor_card(ui, &doctor, self);
                        super::launcher_update::about_card(ui, self);
                    });
            });
    }
}

fn page_title(ui: &mut egui::Ui) {
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new("Settings")
                .font(theme::semibold(20.0))
                .color(p.text_bright),
        )
        .truncate(),
    );
}

fn preferences(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    changed |= updates_card(ui, s);
    changed |= appearance_card(ui, s);
    changed |= build_card(ui, s);
    changed |= advanced_card(ui, s);
    changed
}

fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    widgets::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        if !title.is_empty() {
            widgets::section_heading(ui, title);
        }
        body(ui);
    });
}

fn updates_card(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    card(ui, "Updates & builds", |ui| {
        changed |= widgets::switch_row(
            ui,
            &mut s.rebuild_after_update,
            "Recompile after updates",
            "Rebuild when an update changes the game.",
        )
        .changed();
        changed |= widgets::switch_row(
            ui,
            &mut s.rebuild_after_module_change,
            "Recompile after module changes",
            "Rebuild when staged modules are applied.",
        )
        .changed();
        changed |= widgets::switch_row(
            ui,
            &mut s.check_updates_on_start,
            "Check for updates on start",
            "Look for new commits when watt-start opens.",
        )
        .changed();
        changed |= widgets::switch_row(
            ui,
            &mut s.backup_before_update,
            "Back up worlds",
            "Copy worlds aside before the game changes.",
        )
        .changed();
        if s.backup_before_update {
            changed |= setting_row(ui, "Backups to keep", "Per instance.", 88.0, |ui| {
                ui.add(egui::DragValue::new(&mut s.backups_to_keep).range(1..=20))
                    .changed()
            });
        }
        changed |= widgets::switch_row(
            ui,
            &mut s.close_on_play,
            "Hide while playing",
            "Hide this window while the game runs.",
        )
        .changed();
        changed |= setting_row(
            ui,
            "Generations kept",
            "Rollback points per instance.",
            88.0,
            |ui| {
                ui.add(egui::DragValue::new(&mut s.generations_to_keep).range(1..=50))
                    .changed()
            },
        );
    });
    changed
}

fn appearance_card(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    card(ui, "Appearance", |ui| {
        changed |= setting_row(ui, "Theme", "", 160.0, |ui| {
            let mut idx = usize::from(!s.appearance.dark);
            let edited = widgets::segmented(
                ui,
                "settings-theme",
                &mut idx,
                &[("Dark", None), ("Light", None)],
            );
            if edited {
                s.appearance.dark = idx == 0;
            }
            edited
        });
        changed |= widgets::switch_row(
            ui,
            &mut s.appearance.animations,
            "Animations",
            "Also stills the Forge.",
        )
        .changed();
        changed |= setting_row(ui, "Scale", "", 210.0, |ui| {
            ui.spacing_mut().slider_width = 150.0;
            // Applying zoom on every drag step moves the pointer in points and the slider
            // oscillates. Hold the value until the drag ends (or a keyboard nudge lands).
            let id = ui.scope_id().with("pending-scale");
            let mut pending = ui
                .ctx()
                .data(|d| d.get_temp::<f32>(id))
                .unwrap_or(s.appearance.scale);
            let response = ui.add(
                egui::Slider::new(&mut pending, 0.75..=1.75)
                    .step_by(0.05)
                    .fixed_decimals(2),
            );
            let commit = response.drag_stopped() || (response.changed() && !response.dragged());
            if commit {
                ui.ctx().data_mut(|d| d.remove::<f32>(id));
                s.appearance.scale = pending;
                true
            } else if response.dragged() {
                ui.ctx().data_mut(|d| d.insert_temp(id, pending));
                false
            } else {
                false
            }
        });
    });
    changed
}

fn build_card(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    card(ui, "Build performance", |ui| {
        changed |= setting_row(ui, "Parallel jobs", "0 means automatic.", 230.0, |ui| {
            let jobs = ui.add(egui::DragValue::new(&mut s.build_jobs).range(0..=256));
            let p = theme::palette(ui.ctx());
            ui.label(
                egui::RichText::new(format!("effective: {}", s.effective_jobs()))
                    .font(theme::regular(12.0))
                    .color(p.text_muted),
            );
            jobs.changed()
        });
        changed |= setting_row(
            ui,
            "Build priority",
            "Lower = faster builds, higher = a smoother desktop",
            210.0,
            |ui| {
                ui.spacing_mut().slider_width = 150.0;
                ui.add(egui::Slider::new(&mut s.build_nice, 0..=19))
                    .changed()
            },
        );
    });
    changed
}

fn advanced_card(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    card(ui, "", |ui| {
        let open = advanced_header(ui);
        if !open {
            return;
        }
        changed |= setting_row(
            ui,
            "Default channel",
            "New instances follow this branch.",
            200.0,
            |ui| {
                widgets::text_input_with_id(
                    ui,
                    "default-channel",
                    &mut s.default_channel,
                    "main",
                    ui.available_width(),
                )
                .changed()
            },
        );
        let p = theme::palette(ui.ctx());
        ui.add(
            egui::Label::new(
                egui::RichText::new("Remotes")
                    .font(theme::semibold(13.0))
                    .color(p.text_bright),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new("Empty uses the public GitHub repository.")
                    .font(theme::regular(12.0))
                    .color(p.text_muted),
            )
            .wrap(),
        );
        for repo in Repo::ALL {
            changed |= remote_row(ui, s, repo);
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new("Extra repositories")
                    .font(theme::semibold(13.0))
                    .color(p.text_bright),
            )
            .truncate(),
        );
        changed |= extra_repositories(ui, s);
        changed |= setting_row(ui, "Wrapper", "", 220.0, |ui| {
            let mut idx = match s.wrapper {
                WrapperSetting::FollowPwc => 0,
                WrapperSetting::None => 1,
            };
            let edited = widgets::segmented(
                ui,
                "settings-wrapper",
                &mut idx,
                &[("Follow pwc", None), ("None", None)],
            );
            if edited {
                s.wrapper = if idx == 0 {
                    WrapperSetting::FollowPwc
                } else {
                    WrapperSetting::None
                };
            }
            edited
        });
    });
    changed
}

fn remote_row(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings, repo: Repo) -> bool {
    let mut value = s.remotes.get(&repo).cloned().unwrap_or_default();
    let edited = setting_row(ui, repo.label(), "", 240.0, |ui| {
        widgets::text_input_with_id(
            ui,
            ("remote", repo.label()),
            &mut value,
            "Public GitHub",
            ui.available_width(),
        )
        .changed()
    });
    if !edited {
        return false;
    }
    if value.trim().is_empty() {
        s.remotes.remove(&repo);
    } else {
        s.remotes.insert(repo, value);
    }
    true
}

fn extra_repositories(ui: &mut egui::Ui, s: &mut watt_core::settings::Settings) -> bool {
    let mut changed = false;
    let mut remove_at = None;
    for (i, path) in s.extra_repositories.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let row_w = ui.available_width();
            let mut text = path.display().to_string();
            let input_w = (row_w - 36.0).max(80.0);
            if widgets::text_input_with_id(
                ui,
                ("extra-repo", i),
                &mut text,
                "Repository path",
                input_w,
            )
            .changed()
            {
                *path = PathBuf::from(text);
                changed = true;
            }
            if widgets::icon_button(ui, Icon::Cross, Variant::Ghost, 28.0, "Remove").clicked() {
                remove_at = Some(i);
            }
        });
    }
    if let Some(i) = remove_at {
        s.extra_repositories.remove(i);
        changed = true;
    }
    if Button::new("Add repository")
        .variant(Variant::Secondary)
        .size(Size::Small)
        .icon(Icon::Plus)
        .show(ui)
        .clicked()
    {
        s.extra_repositories.push(PathBuf::new());
        changed = true;
    }
    changed
}

/// Disclosure row. Open state is kept in egui temp data and starts closed.
fn advanced_header(ui: &mut egui::Ui) -> bool {
    let p = theme::palette(ui.ctx());
    let id = egui::Id::unique("settings-advanced");
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
    if response.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Advanced"));
    let fill = if response.is_pointer_button_down_on() {
        p.ghost_pressed
    } else if response.hovered() {
        p.ghost_hover
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, 8.0, fill);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            8.0,
            egui::Stroke::new(1.5, p.arc),
            egui::StrokeKind::Inside,
        );
    }
    let icon = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Vec2::splat(16.0),
    );
    let color = if open || response.hovered() {
        p.text_bright
    } else {
        p.text_muted
    };
    widgets::paint_icon(ui.painter(), icon_rect, icon, color, 0.0);
    let galley =
        ui.painter()
            .layout_no_wrap("Advanced".to_owned(), theme::semibold(15.0), p.text_bright);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(egui::pos2(rect.left() + 28.0, y), galley, p.text_bright);
    open
}

/// Label and description on the left, `control` right-aligned. At least 44px tall.
fn setting_row(
    ui: &mut egui::Ui,
    label: &str,
    description: &str,
    control_w: f32,
    control: impl FnOnce(&mut egui::Ui) -> bool,
) -> bool {
    let p = theme::palette(ui.ctx());
    let width = ui.available_width();
    let text_w = (width - control_w - 12.0).max(40.0);
    let label_galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), theme::semibold(14.0), p.text_bright);
    let desc_galley = if description.is_empty() {
        None
    } else {
        Some(ui.painter().layout(
            description.to_owned(),
            theme::regular(12.0),
            p.text_muted,
            text_w,
        ))
    };
    let desc_h = desc_galley
        .as_ref()
        .map(|g| g.size().y + 2.0)
        .unwrap_or(0.0);
    let block = label_galley.size().y + desc_h;
    let row_h = (block + 16.0).max(44.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
    let mut y = rect.center().y - block * 0.5;
    ui.painter()
        .galley(egui::pos2(rect.left(), y), label_galley, p.text_bright);
    if let Some(galley) = desc_galley {
        y += block - galley.size().y;
        ui.painter()
            .galley(egui::pos2(rect.left(), y), galley, p.text_muted);
    }
    let control_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - control_w, rect.top()),
        rect.right_bottom(),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(control_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    control(&mut child)
}

fn maintenance_snap(app: &App) -> Option<MaintenanceSnap> {
    let slot = app.ops.get(MAINTENANCE_KEY)?;
    if slot.running() {
        return Some(MaintenanceSnap::Running {
            phase: phase_title(&slot.view),
            fraction: slot.view.fraction(),
        });
    }
    let (summary, error) = match &slot.view.outcome {
        Some(OpOutcome::Ok { summary }) => (summary.clone(), false),
        Some(OpOutcome::Failed { error, .. }) => (error.clone(), true),
        Some(OpOutcome::Cancelled) => ("Cancelled".to_owned(), false),
        None => ("Done".to_owned(), false),
    };
    Some(MaintenanceSnap::Settled { summary, error })
}

fn phase_title(view: &OpView) -> String {
    if let Some(current) = view.current
        && let Some(phase) = view.phases.iter().find(|phase| phase.phase == current)
    {
        return phase.title.clone();
    }
    if let Some(current) = view.current {
        return current.title().to_owned();
    }
    view.phases
        .last()
        .map(|phase| phase.title.clone())
        .unwrap_or_else(|| "Working".to_owned())
}

fn maintenance_card(ui: &mut egui::Ui, snap: &Option<MaintenanceSnap>, app: &mut App) {
    card(ui, "Maintenance", |ui| match snap {
        Some(MaintenanceSnap::Running { phase, fraction }) => {
            if maintenance_running(ui, phase, *fraction)
                && let Some(slot) = app.ops.get_mut(MAINTENANCE_KEY)
            {
                slot.op.cancel();
            }
        }
        Some(MaintenanceSnap::Settled { summary, error }) => {
            if maintenance_settled(ui, summary, *error) {
                app.dismiss_op(MAINTENANCE_KEY);
            }
        }
        None => {
            if maint_button(
                ui,
                "Clean build cache",
                "The next build of every instance starts from scratch.",
            ) {
                app.ask_maintenance(
                    "Clean the build cache?",
                    "The next build of every instance starts from scratch. This can take a long time.",
                    OpRequest::Maintenance {
                        clean_cache: true,
                        gc_snapshots: false,
                        gc_store: false,
                        dry_run: false,
                    },
                );
            }
            if maint_button(
                ui,
                "Remove unused sources",
                "Deletes source snapshots nothing still uses.",
            ) {
                app.ask_maintenance(
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
            if maint_button(
                ui,
                "Remove unused packages",
                "Deletes store packages no lock still uses.",
            ) {
                app.ask_maintenance(
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
        }
    });
}

fn maint_button(ui: &mut egui::Ui, label: &str, description: &str) -> bool {
    let clicked = Button::new(label)
        .variant(Variant::Secondary)
        .show(ui)
        .clicked();
    let p = theme::palette(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(description)
                .font(theme::regular(12.0))
                .color(p.text_muted),
        )
        .wrap(),
    );
    clicked
}

fn maintenance_running(ui: &mut egui::Ui, phase: &str, fraction: f32) -> bool {
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.set_min_height(36.0);
        let phase_w = 180.0_f32.min(ui.available_width() * 0.4);
        ui.allocate_ui_with_layout(
            egui::vec2(phase_w, 36.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(phase)
                            .font(theme::semibold(14.0))
                            .color(p.text_bright),
                    )
                    .truncate(),
                );
            },
        );
        let bar_w = (ui.available_width() - 88.0).max(48.0);
        ui.allocate_ui_with_layout(
            egui::vec2(bar_w, 16.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                widgets::progress_bar(ui, fraction, p.volt, 6.0);
            },
        );
        Button::new("Cancel")
            .variant(Variant::Danger)
            .size(Size::Small)
            .show(ui)
            .clicked()
    })
    .inner
}

fn maintenance_settled(ui: &mut egui::Ui, summary: &str, error: bool) -> bool {
    let p = theme::palette(ui.ctx());
    let color = if error { p.bad } else { p.text };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let text_w = (ui.available_width() - 96.0).max(40.0);
        ui.vertical(|ui| {
            ui.set_width(text_w);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(summary)
                        .font(theme::regular(14.0))
                        .color(color),
                )
                .wrap(),
            );
        });
        Button::new("Dismiss")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(ui)
            .clicked()
    })
    .inner
}

fn doctor_card(ui: &mut egui::Ui, doctor: &Loaded<Vec<watt_core::doctor::Check>>, app: &mut App) {
    card(ui, "", |ui| {
        let run = checks_header(ui);
        match doctor {
            Loaded::Ready(checks) => {
                for check in checks {
                    doctor_row(ui, check);
                }
            }
            Loaded::Loading | Loaded::Idle => {
                let p = theme::palette(ui.ctx());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Running checks…")
                            .font(theme::regular(14.0))
                            .color(p.text_muted),
                    )
                    .truncate(),
                );
            }
            Loaded::Err(error) => {
                let p = theme::palette(ui.ctx());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("Could not run checks: {error}"))
                            .font(theme::regular(14.0))
                            .color(p.bad),
                    )
                    .wrap(),
                );
            }
        }
        if run {
            app.backend.run_doctor();
        }
    });
}

fn checks_header(ui: &mut egui::Ui) -> bool {
    let p = theme::palette(ui.ctx());
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::hover());
    let galley = ui.painter().layout_no_wrap(
        "System checks".to_owned(),
        theme::semibold(15.0),
        p.text_bright,
    );
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(egui::pos2(rect.left(), y), galley, p.text_bright);
    let button_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 120.0, rect.top()),
        rect.right_bottom(),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(button_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    Button::new("Run again")
        .variant(Variant::Ghost)
        .size(Size::Small)
        .show(&mut child)
        .clicked()
}

fn doctor_row(ui: &mut egui::Ui, check: &watt_core::doctor::Check) {
    let p = theme::palette(ui.ctx());
    let color = match check.level {
        Level::Ok => p.ok,
        Level::Info => p.arc,
        Level::Warn => p.warn,
        Level::Error => p.bad,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        theme::dot(ui, color);
        ui.add(
            egui::Label::new(
                egui::RichText::new(&check.name)
                    .font(theme::semibold(14.0))
                    .color(p.text_bright),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(&check.detail)
                    .font(theme::regular(12.0))
                    .color(p.text_muted),
            )
            .truncate(),
        );
    });
    if !check.hint.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&check.hint)
                        .font(theme::regular(12.0))
                        .color(p.text_muted),
                )
                .wrap(),
            );
        });
    }
}
