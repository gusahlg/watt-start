//! Instance page tab bodies: overview, builds, log and instance settings.
//!
//! Notes, display name and group keep a per-instance draft. They save on focus loss and again
//! when the user leaves the instance or the tab, so a click on the sidebar cannot drop typed text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use watt_core::events::OpKind;
use watt_core::meta::{Binding, Generation, short};
use watt_core::ops::OpRequest;
use watt_core::status::Readiness;

use super::toasts::ToastKind;
use super::{App, InstanceTab, MainView};
use crate::gui::backend::{InstanceEntry, MetaEdit};
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Size, Variant};

/// How many trailing lines of an on-disk op log to keep in memory.
const LOG_TAIL_LINES: usize = 400;

/// Per-instance UI memory that has to survive frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstanceUiState {
    /// Last [`LOG_TAIL_LINES`] of a log file, keyed by path. An empty string is a cached miss
    /// (the file was absent), so a missing log is not re-read every frame.
    pub log_tails: HashMap<PathBuf, String>,
    /// The instance whose drafts were on screen. `None` until the first instance page, and when
    /// Settings or the welcome screen is showing. Used to commit drafts on navigation, including
    /// Ctrl+, which changes the view before the panels draw.
    pub shown_instance: Option<String>,
}

impl App {
    /// Save dirty notes, display name and group drafts for `instance`.
    ///
    /// A draft that still matches what is stored is dropped without writing. A failed
    /// `edit_meta` keeps the draft and toasts the error. Drafts for an instance that has left
    /// the catalog are dropped.
    pub(crate) fn commit_drafts(&mut self, instance: &str) {
        let Some(entry) = self
            .backend
            .instances()
            .iter()
            .find(|e| e.card.name == instance)
            .cloned()
        else {
            self.notes_draft.remove(instance);
            self.display_name_draft.remove(instance);
            self.group_draft.remove(instance);
            return;
        };
        self.commit_field(
            instance,
            Draft::Notes,
            &entry.card.meta.notes,
            MetaEdit::Notes,
        );
        self.commit_field(
            instance,
            Draft::DisplayName,
            &display_seed(&entry),
            MetaEdit::DisplayName,
        );
        self.commit_field(
            instance,
            Draft::Group,
            &entry.card.meta.group.clone().unwrap_or_default(),
            MetaEdit::Group,
        );
    }

    /// Commit the previous instance's drafts when the main view's instance changes.
    ///
    /// Called at the start of the main area, so a shortcut that switches to Settings still
    /// saves. The first frame only records what is showing.
    pub(super) fn note_view_changed(&mut self) {
        let current = match &self.main_view {
            Some(MainView::Instance(name)) => Some(name.clone()),
            _ => None,
        };
        if let Some(previous) = self.instance_ui.shown_instance.clone()
            && current.as_deref() != Some(previous.as_str())
        {
            self.commit_drafts(&previous);
        }
        self.instance_ui.shown_instance = current;
    }

    pub(super) fn overview_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let wide = ui.available_width() >= 880.0;
        let width = ui.available_width().min(960.0);
        egui::ScrollArea::vertical()
            .id_salt(("overview", entry.card.name.clone()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 16.0;
                ui.add_space(4.0);
                self.problem_card(ui, entry);
                self.updates_card(ui, entry);
                if wide {
                    self.details_and_activity(ui, entry);
                } else {
                    self.details_card(ui, entry);
                    self.activity_card(ui, entry);
                }
                self.notes_card(ui, entry);
                ui.add_space(20.0);
            });
    }

    pub(super) fn builds_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let blocked = self.action_block_reason(entry);
        egui::ScrollArea::vertical()
            .id_salt(("builds", name.clone()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.add_space(4.0);
                if entry.card.meta.generations.is_empty() {
                    let p = theme::palette(ui.ctx());
                    ui.add_space(24.0);
                    ui.label(
                        egui::RichText::new("No builds yet. Build & play creates the first one.")
                            .font(theme::regular(14.0))
                            .color(p.text_muted),
                    );
                } else {
                    let newest = entry.card.meta.generations.first().map(|g| g.id);
                    let last_good = entry.status.last_good.as_ref().map(|g| g.id);
                    for g in &entry.card.meta.generations {
                        self.build_row(ui, &name, g, newest, last_good, blocked.as_deref());
                    }
                }
                ui.add_space(20.0);
            });
    }

    pub(super) fn log_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let path = self.log_path(&name, entry);
        let text = self.log_body(&name, entry);
        let p = theme::palette(ui.ctx());

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let open = Button::new("Open log file")
                .variant(Variant::Secondary)
                .size(Size::Small);
            if let Some(path) = &path {
                if open.show(ui).clicked() {
                    self.backend.open_path(path);
                }
            } else {
                open.enabled(false)
                    .on_disabled_hover_text("No log file for this instance yet")
                    .show(ui);
            }
            if Button::new("Copy")
                .variant(Variant::Ghost)
                .size(Size::Small)
                .show(ui)
                .clicked()
            {
                if text.is_empty() {
                    self.toast_kind(ToastKind::Info, "No log to copy");
                } else {
                    ui.ctx().copy_text(text.clone());
                    self.toast_kind(ToastKind::Ok, "Copied the log");
                }
            }
        });
        ui.add_space(8.0);

        let height = (ui.available_height() - 20.0).max(80.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        ui.painter().rect(
            rect,
            12.0,
            p.bg_canvas,
            egui::Stroke::new(1.0, p.line_soft),
            egui::StrokeKind::Inside,
        );
        let inner = rect.shrink(12.0);
        let mut log_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        log_ui.set_clip_rect(inner);
        egui::ScrollArea::vertical()
            .id_salt(("log-box", name))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(&mut log_ui, |ui| {
                if text.is_empty() {
                    ui.label(
                        egui::RichText::new("No log for this session yet.")
                            .font(theme::regular(13.0))
                            .color(p.text_muted),
                    );
                } else {
                    for line in text.lines() {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line)
                                    .font(theme::mono(13.0))
                                    .color(p.text),
                            )
                            .selectable(true),
                        );
                    }
                }
            });
    }

    pub(super) fn instance_settings_tab(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let width = ui.available_width().min(720.0);
        egui::ScrollArea::vertical()
            .id_salt(("instance-settings", entry.card.name.clone()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 16.0;
                ui.add_space(4.0);
                self.identity_card(ui, entry);
                self.source_card(ui, entry);
                self.build_card(ui, entry);
                self.danger_card(ui, entry);
                ui.add_space(20.0);
            });
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show an instance's Settings tab.
    pub(crate) fn show_instance_settings(&mut self, name: &str) {
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = InstanceTab::InstanceSettings;
    }

    #[allow(dead_code)]
    /// Test/screenshot helper: show an instance's Log tab.
    pub(crate) fn show_instance_log(&mut self, name: &str) {
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = InstanceTab::Log;
    }

    fn commit_field(
        &mut self,
        instance: &str,
        draft: Draft,
        stored: &str,
        edit: fn(String) -> MetaEdit,
    ) {
        let Some(text) = self.draft_map(draft).get(instance).cloned() else {
            return;
        };
        if text == stored {
            self.draft_map(draft).remove(instance);
            return;
        }
        match self.backend.edit_meta(instance, edit(text)) {
            Ok(_) => {
                self.draft_map(draft).remove(instance);
            }
            Err(error) => self.toast_kind(ToastKind::Error, error),
        }
    }

    fn draft_map(&mut self, draft: Draft) -> &mut HashMap<String, String> {
        match draft {
            Draft::Notes => &mut self.notes_draft,
            Draft::DisplayName => &mut self.display_name_draft,
            Draft::Group => &mut self.group_draft,
        }
    }

    fn problem_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        if self.busy(&name) {
            return;
        }
        let failed = entry
            .card
            .meta
            .last_op
            .as_ref()
            .filter(|op| !op.ok)
            .cloned();
        let needs_build = matches!(entry.status.readiness, Readiness::NeedsBuild { .. });
        let broken = matches!(entry.status.readiness, Readiness::Broken { .. });
        if failed.is_none() && !needs_build && !broken {
            return;
        }
        let reason = match &entry.status.readiness {
            Readiness::NeedsBuild { reason } | Readiness::Broken { reason } => Some(reason.clone()),
            Readiness::Ready { .. } | Readiness::New => None,
        };
        let bar = if broken || failed.is_some() {
            theme::palette(ui.ctx()).bad
        } else {
            theme::palette(ui.ctx()).volt
        };
        let heading = if broken {
            "Can't build"
        } else if failed.is_some() {
            "Last build failed"
        } else {
            "Needs a build"
        };
        let show_failure_actions = broken || failed.is_some();
        let generations = entry.card.meta.generations.len();
        let last_good = entry.status.last_good.is_some();
        let log = failed.as_ref().and_then(|op| op.log.clone());
        let error = failed.and_then(|op| op.error);

        widgets::card_with_bar(ui, bar, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, heading);
            let p = theme::palette(ui.ctx());
            if let Some(reason) = reason {
                ui.add(wrapped_label(&reason, theme::regular(14.0), p.text));
            }
            if let Some(error) = error {
                ui.add(wrapped_label(&error, theme::mono(13.0), p.text));
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                if needs_build
                    && Button::new("Build now")
                        .variant(Variant::Primary)
                        .size(Size::Medium)
                        .show(ui)
                        .clicked()
                {
                    self.start_op(
                        name.clone(),
                        OpRequest::Build {
                            instance: name.clone(),
                            force: false,
                            profile: None,
                        },
                    );
                }
                if show_failure_actions && generations > 1 {
                    self.failure_button(ui, "Roll back", Variant::Secondary, &name, || {
                        OpRequest::Rollback {
                            instance: name.clone(),
                            generation: None,
                        }
                    });
                }
                if show_failure_actions
                    && last_good
                    && button(ui, "Play last build", Variant::Secondary)
                {
                    self.play(&name, true);
                }
                if show_failure_actions && button(ui, "Change source…", Variant::Secondary) {
                    self.commit_drafts(&name);
                    self.open_change_source_dialog(&name);
                }
                if let Some(log) = log
                    && button(ui, "Open log", Variant::Ghost)
                {
                    self.backend.open_path(&log);
                }
            });
        });
    }

    fn failure_button(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        variant: Variant,
        name: &str,
        request: impl FnOnce() -> OpRequest,
    ) {
        if button(ui, label, variant) {
            self.start_op(name, request());
        }
    }

    fn updates_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let Some(logs) = self.updates.get(&name).cloned().filter(|l| !l.is_empty()) else {
            return;
        };
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Updates available");
            let p = theme::palette(ui.ctx());
            for log in &logs {
                ui.label(
                    egui::RichText::new(log.repo.label())
                        .font(theme::semibold(14.0))
                        .color(p.text_bright),
                );
                for commit in &log.commits {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            egui::RichText::new(short(&commit.sha))
                                .font(theme::mono(12.0))
                                .color(p.text),
                        );
                        ui.add(wrapped_label(&commit.summary, theme::regular(13.0), p.text));
                    });
                }
                if log.truncated {
                    ui.label(
                        egui::RichText::new("…and more")
                            .font(theme::regular(12.0))
                            .color(p.text_muted),
                    );
                }
            }
            if Button::new("Update now")
                .variant(Variant::Primary)
                .size(Size::Medium)
                .show(ui)
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
        });
    }

    fn details_and_activity(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let gap = 16.0;
        let col = ((ui.available_width() - gap) * 0.5).max(0.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.vertical(|ui| {
                ui.set_width(col);
                self.details_card(ui, entry);
            });
            ui.vertical(|ui| {
                ui.set_width(col);
                self.activity_card(ui, entry);
            });
        });
    }

    fn details_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let dir = entry.card.dir.clone();
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Details");
            let p = theme::palette(ui.ctx());
            ui.add(wrapped_label(
                &binding_sentence(&entry.card.meta.source),
                theme::regular(14.0),
                p.text,
            ));
            kv(
                ui,
                "Game version",
                entry.card.game.version.as_deref().unwrap_or("—"),
            );
            kv(
                ui,
                "Game commit",
                entry
                    .card
                    .game
                    .revision
                    .as_deref()
                    .map(short)
                    .unwrap_or("—"),
            );
            kv(
                ui,
                "Engine commit",
                entry
                    .card
                    .game
                    .engine_revision
                    .as_deref()
                    .map(short)
                    .unwrap_or("—"),
            );
            kv(ui, "Mod API", entry.card.game.api.as_deref().unwrap_or("—"));
            kv(
                ui,
                "Environment",
                entry
                    .card
                    .game
                    .environment
                    .as_deref()
                    .map(short_env)
                    .unwrap_or("—"),
            );
            kv(ui, "Profile", entry.card.manifest.profile.as_str());
            self.directory_row(ui, &dir);
        });
    }

    fn directory_row(&mut self, ui: &mut egui::Ui, dir: &Path) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let (key_rect, _) =
                ui.allocate_exact_size(egui::vec2(120.0, 28.0), egui::Sense::hover());
            let galley = ui.painter().layout_no_wrap(
                "Directory".to_owned(),
                theme::regular(13.0),
                p.text_muted,
            );
            ui.painter().galley(
                egui::pos2(key_rect.left(), key_rect.center().y - galley.size().y * 0.5),
                galley,
                p.text_muted,
            );
            let button_w = 110.0;
            let text_w = (ui.available_width() - button_w - 8.0).max(24.0);
            let (text_rect, _) =
                ui.allocate_exact_size(egui::vec2(text_w, 28.0), egui::Sense::hover());
            paint_ellipsis(
                ui.painter(),
                egui::pos2(text_rect.left(), text_rect.center().y - 8.0),
                &dir.display().to_string(),
                theme::regular(13.0),
                p.text,
                text_w,
            );
            if Button::new("Open folder")
                .variant(Variant::Ghost)
                .size(Size::Small)
                .show(ui)
                .clicked()
            {
                self.backend.open_path(dir);
            }
        });
    }

    fn activity_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let last_build = entry
            .card
            .meta
            .generations
            .iter()
            .find_map(|g| g.build.as_ref().map(|b| b.finished));
        let kept = entry.card.meta.generations.len();
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Activity");
            kv(
                ui,
                "Last played",
                &humanize_opt(entry.card.meta.last_played),
            );
            kv(ui, "Last build", &humanize_opt(last_build));
            kv(
                ui,
                "Builds kept",
                &if kept == 1 {
                    "1 generation".to_owned()
                } else {
                    format!("{kept} generations")
                },
            );
        });
    }

    fn notes_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let stored = entry.card.meta.notes.clone();
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Notes");
            let id = egui::Id::unique(("instance-notes", &name));
            let p = theme::palette(ui.ctx());
            let focused = ui.memory(|mem| mem.has_focus(id));
            let stroke = if focused { p.arc } else { p.line };
            let save = egui::Frame::new()
                .fill(p.bg_canvas)
                .stroke(egui::Stroke::new(1.0, stroke))
                .corner_radius(8.0)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    let draft = self
                        .notes_draft
                        .entry(name.clone())
                        .or_insert_with(|| stored.clone());
                    ui.add(
                        egui::TextEdit::multiline(draft)
                            .id(id)
                            .hint_text(
                                egui::RichText::new("Notes")
                                    .font(theme::regular(14.0))
                                    .color(p.text_faint),
                            )
                            .font(theme::regular(14.0))
                            .text_color(p.text)
                            .desired_rows(5)
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE),
                    )
                    .lost_focus()
                })
                .inner;
            if save {
                self.commit_drafts(&name);
            }
        });
    }

    fn build_row(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        generation: &Generation,
        newest: Option<u64>,
        last_good: Option<u64>,
        blocked: Option<&str>,
    ) {
        let is_newest = Some(generation.id) == newest;
        let is_last_good = Some(generation.id) == last_good;
        let p = theme::palette(ui.ctx());
        let fill = if is_newest {
            p.bg_card.lerp_to_gamma(p.volt, 0.06)
        } else {
            p.bg_card
        };
        ui.push_id(generation.id, |ui| {
            let (rect, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
            ui.painter().rect(
                rect,
                12.0,
                fill,
                egui::Stroke::new(1.0, p.line_soft),
                egui::StrokeKind::Inside,
            );
            let inner = rect.shrink2(egui::vec2(16.0, 8.0));
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            row.spacing_mut().item_spacing.x = 8.0;

            let id_text = format!("#{}", generation.id);
            row.label(
                egui::RichText::new(id_text)
                    .font(theme::mono(13.0))
                    .color(p.text_muted),
            );
            if is_newest {
                widgets::pill(&mut row, "last built", p.ok, theme::with_alpha(p.ok, 38));
            }
            if is_last_good {
                widgets::pill(&mut row, "last good", p.arc, p.arc_tint);
            }

            let line = generation_line(generation);
            let when = humanize(generation.created);
            let meta = build_meta_line(generation);
            let text_w =
                (row.available_width() - build_actions_width(is_newest, is_last_good)).max(40.0);
            row.allocate_ui_with_layout(
                egui::vec2(text_w, inner.height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let when_w = ui
                            .painter()
                            .layout_no_wrap(when.clone(), theme::regular(12.0), p.text_muted)
                            .size()
                            .x;
                        let title_w = (ui.available_width() - when_w - 8.0).max(16.0);
                        let (title_rect, _) =
                            ui.allocate_exact_size(egui::vec2(title_w, 18.0), egui::Sense::hover());
                        paint_ellipsis(
                            ui.painter(),
                            title_rect.min,
                            &line,
                            theme::semibold(14.0),
                            p.text_bright,
                            title_w,
                        );
                        ui.label(
                            egui::RichText::new(when)
                                .font(theme::regular(12.0))
                                .color(p.text_muted),
                        );
                    });
                    ui.label(
                        egui::RichText::new(meta)
                            .font(theme::regular(12.0))
                            .color(p.text_muted),
                    );
                },
            );

            let enabled = blocked.is_none();
            let reason = blocked.unwrap_or("");
            if !is_newest {
                let response = Button::new("Roll back")
                    .variant(Variant::Secondary)
                    .size(Size::Small)
                    .enabled(enabled)
                    .on_disabled_hover_text(reason)
                    .show(&mut row);
                if response.clicked() {
                    self.start_op(
                        name,
                        OpRequest::Rollback {
                            instance: name.to_owned(),
                            generation: Some(generation.id),
                        },
                    );
                }
            }
            if is_last_good {
                let response = Button::new("Play this build")
                    .variant(Variant::Ghost)
                    .size(Size::Small)
                    .enabled(enabled)
                    .on_disabled_hover_text(reason)
                    .show(&mut row);
                if response.clicked() {
                    self.play(name, true);
                }
            }
        });
    }

    fn identity_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let current = theme::accent_for_meta(&name, entry.card.meta.color.as_deref());
        let mut favorite = entry.card.meta.favorite;
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Identity");
            field_caption(ui, "Display name");
            let display_seed = display_seed(entry);
            let save_display = self.draft_input(
                ui,
                &name,
                Draft::DisplayName,
                &display_seed,
                "Shown in the sidebar",
            );
            if save_display {
                self.commit_field(
                    &name,
                    Draft::DisplayName,
                    &display_seed,
                    MetaEdit::DisplayName,
                );
            }
            field_caption(ui, "Group");
            let group_seed = entry.card.meta.group.clone().unwrap_or_default();
            let save_group =
                self.draft_input(ui, &name, Draft::Group, &group_seed, "Sidebar heading");
            if save_group {
                self.commit_field(&name, Draft::Group, &group_seed, MetaEdit::Group);
            }
            field_caption(ui, "Colour");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                for color in theme::INSTANCE_PALETTE {
                    if widgets::swatch(ui, color, color == current).clicked() {
                        self.apply_edit(&name, MetaEdit::Color(theme::to_hex_color(color)));
                    }
                }
            });
            if widgets::switch_row(ui, &mut favorite, "Favourite", "").changed() {
                self.apply_edit(&name, MetaEdit::Favorite(favorite));
            }
        });
    }

    /// Single-line draft. Returns whether Enter or focus loss asked for a save.
    fn draft_input(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        draft: Draft,
        seed: &str,
        hint: &str,
    ) -> bool {
        let map = self.draft_map(draft);
        let text = map
            .entry(name.to_owned())
            .or_insert_with(|| seed.to_owned());
        let response = widgets::text_input(ui, text, hint, f32::INFINITY);
        response.lost_focus()
            || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
    }

    fn source_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let sentence = binding_sentence(&entry.card.meta.source);
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Source");
            let p = theme::palette(ui.ctx());
            ui.add(wrapped_label(&sentence, theme::regular(14.0), p.text));
            if button(ui, "Change source…", Variant::Secondary) {
                self.commit_drafts(&name);
                self.open_change_source_dialog(&name);
            }
        });
    }

    fn build_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let profile = entry.card.manifest.profile.as_str().to_owned();
        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "Build");
            field_caption(ui, "Profile");
            let p = theme::palette(ui.ctx());
            ui.label(
                egui::RichText::new(profile)
                    .font(theme::semibold(14.0))
                    .color(p.text_bright),
            );
            ui.label(
                egui::RichText::new("set in instance.toml")
                    .font(theme::regular(12.0))
                    .color(p.text_muted),
            );
        });
    }

    fn danger_card(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let p = theme::palette(ui.ctx());
        egui::Frame::new()
            .fill(p.bg_card)
            .stroke(egui::Stroke::new(1.0, theme::with_alpha(p.bad, 160)))
            .corner_radius(12.0)
            .inner_margin(16)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                widgets::section_heading(ui, "Danger zone");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if button(ui, "Duplicate…", Variant::Secondary) {
                        self.commit_drafts(&name);
                        self.open_duplicate_dialog(&name);
                    }
                    if button(ui, "Delete…", Variant::Danger) {
                        self.commit_drafts(&name);
                        self.ask_delete_instance(&name);
                    }
                });
            });
    }

    fn apply_edit(&mut self, instance: &str, edit: MetaEdit) {
        if let Err(error) = self.backend.edit_meta(instance, edit) {
            self.toast_kind(ToastKind::Error, error);
        }
    }

    /// Why Play / Roll back / Play this build should refuse, when they should.
    pub(super) fn action_block_reason(&self, entry: &InstanceEntry) -> Option<String> {
        if entry.status.running.is_some() {
            return Some("Running".to_owned());
        }
        if let Some(slot) = self.running_op(&entry.card.name) {
            return Some(format!(
                "Wait for the {} to finish",
                super::op_noun(slot.kind())
            ));
        }
        if entry.status.busy {
            return Some("Busy in pwc".to_owned());
        }
        None
    }

    fn log_path(&self, name: &str, entry: &InstanceEntry) -> Option<PathBuf> {
        self.ops
            .get(name)
            .and_then(|slot| slot.view.log_file.clone())
            .or_else(|| {
                entry
                    .card
                    .meta
                    .last_op
                    .as_ref()
                    .and_then(|op| op.log.clone())
            })
    }

    /// In-memory log while an op slot exists, otherwise the cached tail of `last_op.log`.
    fn log_body(&mut self, name: &str, entry: &InstanceEntry) -> String {
        if let Some(slot) = self.ops.get(name) {
            return slot.view.log.iter().cloned().collect::<Vec<_>>().join("\n");
        }
        let Some(path) = entry
            .card
            .meta
            .last_op
            .as_ref()
            .and_then(|op| op.log.clone())
        else {
            return String::new();
        };
        if let Some(cached) = self.instance_ui.log_tails.get(&path) {
            return cached.clone();
        }
        let text = read_log_tail(&path, LOG_TAIL_LINES);
        self.instance_ui.log_tails.insert(path, text.clone());
        text
    }
}

/// Which draft map [`App::commit_field`] writes.
#[derive(Clone, Copy)]
enum Draft {
    Notes,
    DisplayName,
    Group,
}

/// Direct requirements, counting libraries only while the Mods tab is showing them.
pub(super) fn mod_count(entry: &InstanceEntry, show_libraries: bool) -> usize {
    entry
        .card
        .packages
        .iter()
        .filter(|pkg| pkg.direct && (show_libraries || pkg.kind != "library"))
        .count()
}

/// One line of text, elided with `…` to `max_width`. Returns the painted rect.
pub(super) fn paint_ellipsis(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) -> egui::Rect {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    let rect = egui::Rect::from_min_size(pos, galley.size());
    painter.galley(pos, galley, color);
    rect
}

/// An environment hash's short form, without the leading `sha256:` ([`short`] alone would just
/// return that 7-character prefix).
pub(super) fn short_env(env: &str) -> &str {
    short(env.strip_prefix("sha256:").unwrap_or(env))
}

fn button(ui: &mut egui::Ui, label: &str, variant: Variant) -> bool {
    Button::new(label)
        .variant(variant)
        .size(Size::Medium)
        .show(ui)
        .clicked()
}

fn wrapped_label(text: &str, font: egui::FontId, color: egui::Color32) -> egui::Label {
    egui::Label::new(egui::RichText::new(text).font(font).color(color))
        .wrap_mode(egui::TextWrapMode::Wrap)
}

fn field_caption(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.label(
        egui::RichText::new(text)
            .font(theme::regular(12.0))
            .color(p.text_muted),
    );
}

fn kv(ui: &mut egui::Ui, key: &str, value: &str) {
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 20.0), egui::Sense::hover());
        let galley =
            ui.painter()
                .layout_no_wrap(key.to_owned(), theme::regular(13.0), p.text_muted);
        ui.painter().galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            p.text_muted,
        );
        ui.add(wrapped_label(value, theme::regular(13.0), p.text));
    });
}

fn display_seed(entry: &InstanceEntry) -> String {
    entry
        .card
        .meta
        .display_name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| entry.card.name.clone())
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

fn generation_line(generation: &Generation) -> String {
    let pin = match &generation.source {
        Binding::Managed { channel, pin, .. } => match pin {
            Some(pin) => format!("{channel} @ {}", short(&pin.game)),
            None => channel.clone(),
        },
        Binding::Linked { .. } => "linked".to_owned(),
        Binding::PwcDefault => "pwc".to_owned(),
    };
    format!("{} · {pin}", generation.game_version)
}

fn build_meta_line(generation: &Generation) -> String {
    let mut parts = Vec::new();
    if let Some(build) = &generation.build {
        parts.push(fmt_duration_ms(build.duration_ms));
    }
    let packages = generation.packages.len();
    parts.push(if packages == 1 {
        "1 package".to_owned()
    } else {
        format!("{packages} packages")
    });
    parts.push(format!("env {}", short_env(&generation.environment)));
    parts.join(" · ")
}

/// Width reserved so the version line does not run under the row's buttons.
fn build_actions_width(is_newest: bool, is_last_good: bool) -> f32 {
    let rollback = if is_newest { 0.0 } else { 100.0 };
    let play = if is_last_good { 140.0 } else { 0.0 };
    let gap = if rollback > 0.0 && play > 0.0 {
        8.0
    } else {
        0.0
    };
    rollback + play + gap + 8.0
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

fn read_log_tail(path: &Path, lines_kept: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(lines_kept);
    lines[start..].join("\n")
}

/// "Update", "Build", "Creation": the noun in "Update finished" / "Build failed".
pub(super) fn op_result_word(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Create => "Creation",
        OpKind::Mods => "Mod change",
        OpKind::Update => "Update",
        OpKind::Build => "Build",
        OpKind::Play => "Launch",
        OpKind::Rollback => "Rollback",
        OpKind::Duplicate => "Duplication",
        OpKind::Remove => "Removal",
        OpKind::SetSource => "Source change",
        OpKind::CheckUpdates => "Update check",
        OpKind::Maintenance => "Maintenance",
    }
}
