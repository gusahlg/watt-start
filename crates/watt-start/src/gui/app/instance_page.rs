//! The instance page: fixed header card, then either the Forge or the tab strip.
//!
//! A running or settled op that the user has not hidden replaces the tabs with the Forge. The
//! header stays, and its second row becomes the progress capsule while the op is running.

use watt_core::events::{OpKind, OpOutcome};
use watt_core::meta::{Binding, short};
use watt_core::ops::OpRequest;
use watt_core::status::Readiness;

use super::instance_tabs::{mod_count, op_result_word, paint_ellipsis};
use super::{App, InstanceTab, OpSlot};
use crate::gui::backend::InstanceEntry;
use crate::gui::forge::ForgeStyle;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Icon, Size, Variant};

/// Header card height, including its own padding.
const HEADER_H: f32 = 136.0;
const HEADER_RADIUS: f32 = 16.0;
/// Horizontal accent wash: alpha 28 at the left edge, gone by 60% of the width.
const WASH_ALPHA: f32 = 28.0;
const WASH_REACH: f32 = 0.6;

impl App {
    pub(crate) fn instance_page(&mut self, ui: &mut egui::Ui, name: &str) {
        let entry = self
            .backend
            .instances()
            .iter()
            .find(|e| e.card.name == name)
            .cloned();
        if entry.is_none() && !self.ops.contains_key(name) {
            return;
        }

        self.header(ui, name, entry.as_ref());

        let show_forge = self.ops.get(name).is_some_and(|slot| !slot.hidden);
        if show_forge {
            ui.add_space(12.0);
            self.forge_body(ui, name, entry.as_ref());
        } else if entry.is_none() {
            self.pending_hidden(ui);
        } else if let Some(entry) = entry.as_ref() {
            self.tabbed_body(ui, entry);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, name: &str, entry: Option<&InstanceEntry>) {
        ui.add_space(20.0);
        let outer_w = ui.available_width();
        let (outer, _) =
            ui.allocate_exact_size(egui::vec2(outer_w, HEADER_H), egui::Sense::hover());
        let rect = egui::Rect::from_min_max(
            egui::pos2(outer.left() + 24.0, outer.top()),
            egui::pos2(outer.right() - 24.0, outer.bottom()),
        );
        let p = theme::palette(ui.ctx());
        let accent = entry
            .map(|e| theme::accent_for_meta(&e.card.name, e.card.meta.color.as_deref()))
            .unwrap_or(p.volt);
        paint_header_card(ui.painter(), rect, accent, &p);

        let inner = rect.shrink(16.0);
        let row2 =
            egui::Rect::from_min_max(egui::pos2(inner.left(), inner.bottom() - 44.0), inner.max);
        let row1 = egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), row2.top() - 8.0));
        self.header_identity(ui, row1, name, entry, accent);
        self.header_actions(ui, row2, name, entry);
    }

    fn header_identity(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        name: &str,
        entry: Option<&InstanceEntry>,
        accent: egui::Color32,
    ) {
        let p = theme::palette(ui.ctx());
        let cube = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - 24.0),
            egui::Vec2::splat(48.0),
        );
        widgets::cube(ui.painter(), cube, accent);

        let (pill, pill_color, pulse) = self.header_pill(ui, name, entry);
        let pill_w = pill_width(ui, &pill);
        let text_left = cube.right() + 14.0;
        let text_w = (rect.right() - text_left).max(40.0);
        let title_font = theme::semibold(28.0);
        let meta_font = theme::regular(12.0);
        let title_h = ui
            .painter()
            .layout_no_wrap("Ag".to_owned(), title_font.clone(), p.text_bright)
            .size()
            .y;
        let meta = entry
            .map(|e| meta_line(e, mod_count(e, self.mods_show_libraries)))
            .unwrap_or_else(|| pending_meta(self.ops.get(name).map(OpSlot::kind)));
        let meta_h = ui
            .painter()
            .layout_no_wrap("Ag".to_owned(), meta_font.clone(), p.text)
            .size()
            .y;
        let block = title_h + 2.0 + meta_h;
        let y = rect.center().y - block * 0.5;
        let title_max = (text_w - pill_w - 10.0).max(24.0);
        let title = entry
            .map(|e| e.card.meta.title(&e.card.name).to_owned())
            .unwrap_or_else(|| {
                self.ops
                    .get(name)
                    .map(|slot| slot.title.clone())
                    .unwrap_or_else(|| name.to_owned())
            });
        let title_rect = paint_ellipsis(
            ui.painter(),
            egui::pos2(text_left, y),
            &title,
            title_font,
            p.text_bright,
            title_max,
        );
        let pill_rect = egui::Rect::from_min_size(
            egui::pos2(title_rect.right() + 10.0, y + title_h * 0.5 - 12.0),
            egui::vec2(pill_w, 24.0),
        );
        let mut pill_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(pill_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        widgets::status_pill(&mut pill_ui, &pill, pill_color, pulse);
        paint_ellipsis(
            ui.painter(),
            egui::pos2(text_left, y + title_h + 2.0),
            &meta,
            meta_font,
            p.text,
            text_w,
        );
    }

    fn header_actions(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        name: &str,
        entry: Option<&InstanceEntry>,
    ) {
        if let Some(key) = self.running_key(name) {
            self.progress_capsule(ui, rect, &key);
            return;
        }
        if let Some(slot) = self.ops.get(name)
            && (!slot.running())
            && (slot.hidden || entry.is_none())
        {
            let phrase = outcome_phrase(slot.kind(), slot.view.outcome.as_ref());
            let show = slot.hidden;
            self.outcome_line(ui, rect, name, &phrase, show);
            return;
        }
        if let Some(entry) = entry {
            self.action_row(ui, rect, entry);
        }
    }

    fn action_row(&mut self, ui: &mut egui::Ui, rect: egui::Rect, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        let commits = self
            .updates
            .get(&name)
            .map(|logs| logs.iter().map(|log| log.commits.len()).sum())
            .unwrap_or(0);
        let play_label = play_label(entry);
        let play_reason = self.play_block_reason(entry);
        let external = entry.status.busy;
        let game = entry.card.dir.join("game");
        let logs = entry.card.dir.join("watt-start").join("logs");

        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;

        let mut play = Button::new(play_label)
            .variant(Variant::Primary)
            .size(Size::Large)
            .min_width(160.0);
        if let Some(reason) = &play_reason {
            play = play.enabled(false).on_disabled_hover_text(reason);
        }
        if play.show(&mut row).clicked() {
            self.play(&name, false);
        }

        let mut update = Button::new("Update")
            .variant(Variant::Secondary)
            .size(Size::Medium)
            .min_width(110.0)
            .enabled(!external)
            .on_disabled_hover_text("Busy in pwc");
        if commits > 0 {
            update = update.badge(commits);
        }
        if update.show(&mut row).clicked() {
            self.start_op(
                name.clone(),
                OpRequest::Update {
                    instance: name.clone(),
                    build: None,
                    reset_lock: false,
                },
            );
        }

        if Button::new("Recompile")
            .variant(Variant::Secondary)
            .size(Size::Medium)
            .min_width(110.0)
            .enabled(!external)
            .on_disabled_hover_text("Busy in pwc")
            .show(&mut row)
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

        let rest = row.available_width();
        if rest > 36.0 {
            row.add_space(rest - 36.0);
        }
        let kebab = widgets::icon_button(&mut row, Icon::Kebab, Variant::Secondary, 36.0, "More");
        egui::Popup::menu(&kebab)
            .gap(4.0)
            .align(egui::RectAlign::BOTTOM_END)
            .show(|ui| {
                self.instance_menu(ui, &name, &game, &logs);
            });
    }

    fn instance_menu(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        game: &std::path::Path,
        logs: &std::path::Path,
    ) {
        let p = theme::palette(ui.ctx());
        if menu_item(ui, "Open game folder", p.text) {
            self.backend.open_path(game);
        }
        if menu_item(ui, "Open logs folder", p.text) {
            self.backend.open_path(logs);
        }
        if menu_item(ui, "Duplicate…", p.text) {
            self.commit_drafts(name);
            self.open_duplicate_dialog(name);
        }
        if menu_item(ui, "Change source…", p.text) {
            self.commit_drafts(name);
            self.open_change_source_dialog(name);
        }
        ui.separator();
        if menu_item(ui, "Delete…", p.bad) {
            self.commit_drafts(name);
            self.ask_delete_instance(name);
        }
    }

    fn progress_capsule(&mut self, ui: &mut egui::Ui, rect: egui::Rect, key: &str) {
        let Some((phase, fraction, hidden)) = self
            .ops
            .get(key)
            .map(|slot| (phase_title(slot), slot.view.fraction(), slot.hidden))
        else {
            return;
        };
        let p = theme::palette(ui.ctx());
        let pct = percent(fraction);
        let hide_label = if hidden { "Show forge" } else { "Hide forge" };

        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;

        let hide = Button::new(hide_label)
            .variant(Variant::Secondary)
            .size(Size::Medium)
            .show(&mut row);
        let cancel = Button::new("Cancel")
            .variant(Variant::Danger)
            .size(Size::Medium)
            .show(&mut row);

        let left_w = row.available_width();
        row.allocate_ui_with_layout(
            egui::vec2(left_w, rect.height()),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let pct_text = format!("{pct}%");
                let pct_w = ui
                    .painter()
                    .layout_no_wrap(pct_text.clone(), theme::semibold(14.0), p.text_bright)
                    .size()
                    .x;
                let title_w = (ui.available_width() - pct_w - 80.0).clamp(72.0, 240.0);
                let (title_rect, _) =
                    ui.allocate_exact_size(egui::vec2(title_w, 20.0), egui::Sense::hover());
                paint_ellipsis(
                    ui.painter(),
                    egui::pos2(title_rect.left(), title_rect.center().y - 8.0),
                    &phase,
                    theme::regular(14.0),
                    p.text,
                    title_w,
                );
                ui.label(
                    egui::RichText::new(pct_text)
                        .font(theme::semibold(14.0))
                        .color(p.text_bright),
                );
                widgets::progress_bar(ui, fraction, p.volt, 6.0);
            },
        );

        if cancel.clicked()
            && let Some(slot) = self.ops.get_mut(key)
        {
            slot.op.cancel();
        }
        if hide.clicked()
            && let Some(slot) = self.ops.get_mut(key)
        {
            slot.hidden = !slot.hidden;
        }
    }

    fn outcome_line(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        name: &str,
        phrase: &str,
        show: bool,
    ) {
        let color = outcome_color(ui.ctx(), phrase);
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        let text_w = (row.available_width() - if show { 120.0 } else { 40.0 }).max(24.0);
        let (text_rect, _) =
            row.allocate_exact_size(egui::vec2(text_w, 24.0), egui::Sense::hover());
        paint_ellipsis(
            row.painter(),
            egui::pos2(text_rect.left(), text_rect.center().y - 8.0),
            phrase,
            theme::semibold(14.0),
            color,
            text_w,
        );
        let reveal = show
            && Button::new("Show")
                .variant(Variant::Ghost)
                .size(Size::Small)
                .show(&mut row)
                .clicked();
        let dismiss =
            widgets::icon_button(&mut row, Icon::Cross, Variant::Ghost, 28.0, "Dismiss").clicked();
        if reveal && let Some(slot) = self.ops.get_mut(name) {
            slot.hidden = false;
        }
        if dismiss {
            self.dismiss_page_op(name);
        }
    }

    fn tabbed_body(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = entry.card.name.clone();
        ui.add_space(12.0);
        let strip_w = ui.available_width();
        let (strip_outer, _) =
            ui.allocate_exact_size(egui::vec2(strip_w, 40.0), egui::Sense::hover());
        let strip = strip_outer.shrink2(egui::vec2(24.0, 0.0));
        let count = mod_count(entry, self.mods_show_libraries);
        let staged = self
            .staged_mods
            .get(&name)
            .is_some_and(|mods| !mods.add.is_empty() || !mods.remove.is_empty());
        let mut tabs_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(strip)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let changed = widgets::tabs(
            &mut tabs_ui,
            &mut self.instance_tab,
            &[
                (InstanceTab::Overview, "Overview", None, false),
                (InstanceTab::Mods, "Mods", Some(count), staged),
                (InstanceTab::Builds, "Builds", None, false),
                (InstanceTab::Log, "Log", None, false),
                (InstanceTab::InstanceSettings, "Settings", None, false),
            ],
        );
        if changed {
            self.commit_drafts(&name);
        }
        ui.add_space(8.0);

        let rest = ui.available_rect_before_wrap();
        let content = egui::Rect::from_min_max(
            egui::pos2(rest.left() + 24.0, rest.top()),
            egui::pos2(rest.right() - 24.0, rest.bottom()),
        );
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body.set_clip_rect(content);
        match self.instance_tab {
            InstanceTab::Overview => self.overview_tab(&mut body, entry),
            InstanceTab::Mods => self.mods_tab(&mut body, entry),
            InstanceTab::Builds => self.builds_tab(&mut body, entry),
            InstanceTab::Log => self.log_tab(&mut body, entry),
            InstanceTab::InstanceSettings => self.instance_settings_tab(&mut body, entry),
        }
    }

    fn forge_body(&mut self, ui: &mut egui::Ui, name: &str, entry: Option<&InstanceEntry>) {
        let accent = entry
            .map(|e| theme::accent_for_meta(name, e.card.meta.color.as_deref()))
            .unwrap_or_else(|| theme::palette(ui.ctx()).volt);
        let Some(prepared) = self.prepare_forge(name, entry, accent) else {
            return;
        };
        let rest = ui.available_rect_before_wrap();
        let rect = egui::Rect::from_min_max(
            egui::pos2(rest.left() + 24.0, rest.top()),
            egui::pos2(rest.right() - 24.0, rest.bottom() - 12.0),
        );
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let response = (self.forge.forge)(&mut body, &prepared.view, &prepared.style);
        self.apply_forge_response(name, response);
    }

    fn prepare_forge(
        &self,
        name: &str,
        entry: Option<&InstanceEntry>,
        accent: egui::Color32,
    ) -> Option<PreparedForge> {
        let slot = self.ops.get(name)?;
        let title = entry
            .map(|e| e.card.meta.title(name).to_owned())
            .unwrap_or_else(|| slot.title.clone());
        let expected_ms = entry.and_then(|e| {
            e.card
                .meta
                .generations
                .iter()
                .find_map(|g| g.build.as_ref().map(|b| b.duration_ms))
        });
        Some(PreparedForge {
            view: slot.view.clone(),
            style: ForgeStyle {
                accent,
                animate: self.settings_draft.appearance.animations,
                expected_ms,
                title,
                elapsed_ms: Some(slot.started.elapsed().as_millis() as u64),
            },
        })
    }

    fn apply_forge_response(&mut self, name: &str, response: crate::gui::forge::ForgeResponse) {
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
            self.dismiss_page_op(name);
        }
        if response.play {
            self.dismiss_page_op(name);
            self.play(name, false);
        }
        if let Some(recovery) = response.recovery {
            self.apply_recovery(name, recovery);
        }
    }

    fn pending_hidden(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("The forge is hidden.")
                    .font(theme::regular(14.0))
                    .color(p.text_muted),
            );
        });
    }

    /// Close a settled result. A pending instance that never landed in the catalog returns to
    /// whichever instance [`App::ensure_selection`] would open.
    fn dismiss_page_op(&mut self, name: &str) {
        let pending = self
            .backend
            .instances()
            .iter()
            .all(|entry| entry.card.name != name);
        self.dismiss_op(name);
        if pending
            && !self.ops.contains_key(name)
            && matches!(&self.main_view, Some(super::MainView::Instance(current)) if current == name)
            && self
                .backend
                .instances()
                .iter()
                .all(|entry| entry.card.name != name)
        {
            self.main_view = None;
            self.ensure_selection();
        }
    }

    fn running_key(&self, instance: &str) -> Option<String> {
        self.ops.iter().find_map(|(key, slot)| {
            (slot.running() && slot.holds(key, instance)).then(|| key.clone())
        })
    }

    fn header_pill(
        &self,
        ui: &egui::Ui,
        name: &str,
        entry: Option<&InstanceEntry>,
    ) -> (String, egui::Color32, bool) {
        let p = theme::palette(ui.ctx());
        let Some(entry) = entry else {
            let slot = self.ops.get(name);
            let kind = slot.map(OpSlot::kind).unwrap_or(OpKind::Create);
            let running = slot.is_some_and(OpSlot::running);
            return pending_pill(
                &p,
                kind,
                running,
                slot.and_then(|s| s.view.outcome.as_ref()),
            );
        };
        if let Some(kind) = self.op_kind_running(name) {
            let fraction = self
                .running_op(name)
                .map(|slot| slot.view.fraction())
                .unwrap_or(0.0);
            let label = format!("{} {}%", kind.verb(), percent(fraction));
            return (label, p.volt, false);
        }
        if entry.status.running.is_some() {
            return ("Running".to_owned(), p.arc, true);
        }
        match &entry.status.readiness {
            Readiness::Ready { .. } => ("Ready".to_owned(), p.ok, false),
            Readiness::NeedsBuild { .. } => ("Needs build".to_owned(), p.volt, false),
            Readiness::Broken { .. } => ("Can't build".to_owned(), p.bad, false),
            Readiness::New => ("New".to_owned(), p.neutral, false),
        }
    }

    fn play_block_reason(&self, entry: &InstanceEntry) -> Option<String> {
        if let Some(reason) = self.action_block_reason(entry) {
            return Some(reason);
        }
        if let Readiness::Broken { reason } = &entry.status.readiness {
            return Some(reason.clone());
        }
        None
    }
}

struct PreparedForge {
    view: watt_core::view::OpView,
    style: ForgeStyle,
}

fn paint_header_card(
    painter: &egui::Painter,
    rect: egui::Rect,
    accent: egui::Color32,
    palette: &theme::Palette,
) {
    painter.rect_filled(rect, HEADER_RADIUS, palette.bg_card);
    paint_accent_wash(painter, rect, accent);
    painter.rect_stroke(
        rect,
        HEADER_RADIUS,
        egui::Stroke::new(1.0, palette.line_soft),
        egui::StrokeKind::Inside,
    );
}

/// Horizontal wash clipped to the rounded card. egui has no rounded clip, so each slice's
/// vertical span follows the corner radius.
fn paint_accent_wash(painter: &egui::Painter, rect: egui::Rect, accent: egui::Color32) {
    const SLICES: usize = 24;
    let wash_w = rect.width() * WASH_REACH;
    if wash_w < 1.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    for i in 0..SLICES {
        let t0 = i as f32 / SLICES as f32;
        let t1 = (i + 1) as f32 / SLICES as f32;
        let x0 = rect.left() + wash_w * t0;
        let x1 = rect.left() + wash_w * t1;
        // Vertex colours are premultiplied; an unmultiplied wash would bloom to full accent.
        let c0 = premul(accent, wash_alpha(t0));
        let c1 = premul(accent, wash_alpha(t1));
        let (y0t, y0b) = rounded_span(rect, HEADER_RADIUS, x0);
        let (y1t, y1b) = rounded_span(rect, HEADER_RADIUS, x1);
        if y0b <= y0t || y1b <= y1t {
            continue;
        }
        let i0 = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(x0, y0t), c0);
        mesh.colored_vertex(egui::pos2(x1, y1t), c1);
        mesh.colored_vertex(egui::pos2(x0, y0b), c0);
        mesh.colored_vertex(egui::pos2(x1, y1b), c1);
        mesh.add_triangle(i0, i0 + 1, i0 + 3);
        mesh.add_triangle(i0, i0 + 3, i0 + 2);
    }
    if mesh.is_empty() {
        return;
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn wash_alpha(t: f32) -> u8 {
    (WASH_ALPHA * (1.0 - t)).round().clamp(0.0, WASH_ALPHA) as u8
}

fn premul(color: egui::Color32, alpha: u8) -> egui::Color32 {
    let scale = f32::from(alpha) / 255.0;
    egui::Color32::from_rgba_premultiplied(
        (f32::from(color.r()) * scale).round() as u8,
        (f32::from(color.g()) * scale).round() as u8,
        (f32::from(color.b()) * scale).round() as u8,
        alpha,
    )
}

/// Top and bottom y inside a rounded rect at `x`, inset slightly so the wash stays off the
/// antialiased corner.
fn rounded_span(rect: egui::Rect, radius: f32, x: f32) -> (f32, f32) {
    let dx = if x < rect.left() + radius {
        rect.left() + radius - x
    } else if x > rect.right() - radius {
        x - (rect.right() - radius)
    } else {
        0.0
    };
    let dy = if dx <= 0.0 {
        0.0
    } else {
        let inside = (radius * radius - dx * dx).max(0.0).sqrt();
        radius - inside + 0.6
    };
    (rect.top() + dy, rect.bottom() - dy)
}

fn meta_line(entry: &InstanceEntry, mods: usize) -> String {
    let mut parts = Vec::new();
    parts.push(source_chip(entry));
    if let Some(version) = &entry.card.game.version {
        parts.push(format!("Game {version}"));
    }
    if let Some(engine) = &entry.card.game.engine_revision {
        parts.push(format!("Engine {}", short(engine)));
    }
    parts.push(if mods == 1 {
        "1 mod".to_owned()
    } else {
        format!("{mods} mods")
    });
    parts.push(entry.card.manifest.profile.as_str().to_owned());
    parts.join(" · ")
}

fn source_chip(entry: &InstanceEntry) -> String {
    match &entry.card.meta.source {
        Binding::Managed { channel, .. } => match &entry.card.game.revision {
            Some(rev) => format!("{channel} @ {}", short(rev)),
            None => channel.clone(),
        },
        Binding::Linked { .. } => {
            let branch = entry.card.game.branch.as_deref().unwrap_or("unknown");
            match &entry.card.game.revision {
                Some(rev) => format!("linked · {branch} @ {}", short(rev)),
                None => format!("linked · {branch}"),
            }
        }
        Binding::PwcDefault => match (&entry.card.game.branch, &entry.card.game.revision) {
            (Some(branch), Some(rev)) => format!("pwc · {branch} @ {}", short(rev)),
            (Some(branch), None) => format!("pwc · {branch}"),
            (None, Some(rev)) => format!("pwc · {}", short(rev)),
            (None, None) => "pwc".to_owned(),
        },
    }
}

fn pending_meta(kind: Option<OpKind>) -> String {
    match kind {
        Some(OpKind::Duplicate) => "Copying an instance".to_owned(),
        Some(OpKind::Create) | None => "Creating a new instance".to_owned(),
        Some(other) => other.verb().to_owned(),
    }
}

fn pending_pill(
    palette: &theme::Palette,
    kind: OpKind,
    running: bool,
    outcome: Option<&OpOutcome>,
) -> (String, egui::Color32, bool) {
    if running {
        let label = match kind {
            OpKind::Duplicate => "Copying…".to_owned(),
            OpKind::Create => "Creating…".to_owned(),
            other => format!("{}…", other.verb()),
        };
        return (label, palette.volt, false);
    }
    match outcome {
        Some(OpOutcome::Ok { .. }) => ("Done".to_owned(), palette.ok, false),
        Some(OpOutcome::Failed { .. }) => ("Failed".to_owned(), palette.bad, false),
        Some(OpOutcome::Cancelled) | None => ("Cancelled".to_owned(), palette.neutral, false),
    }
}

fn play_label(entry: &InstanceEntry) -> &'static str {
    if entry.status.running.is_some() {
        "Running"
    } else if matches!(
        entry.status.readiness,
        Readiness::New | Readiness::NeedsBuild { .. }
    ) {
        "Build & play"
    } else {
        "Play"
    }
}

fn phase_title(slot: &OpSlot) -> String {
    if let Some(phase) = slot.view.current {
        if let Some(view) = slot.view.phases.iter().find(|item| item.phase == phase) {
            return view.title.clone();
        }
        return phase.title().to_owned();
    }
    slot.kind().verb().to_owned()
}

fn outcome_phrase(kind: OpKind, outcome: Option<&OpOutcome>) -> String {
    let word = op_result_word(kind);
    match outcome {
        Some(OpOutcome::Ok { .. }) => format!("{word} finished"),
        Some(OpOutcome::Failed { .. }) => format!("{word} failed"),
        Some(OpOutcome::Cancelled) | None => format!("{word} cancelled"),
    }
}

/// Colour follows the phrase so the line and the pill agree without threading the outcome through.
fn outcome_color(ctx: &egui::Context, phrase: &str) -> egui::Color32 {
    let p = theme::palette(ctx);
    if phrase.ends_with("failed") {
        p.bad
    } else if phrase.ends_with("cancelled") {
        p.text_muted
    } else {
        p.text
    }
}

fn percent(fraction: f32) -> u32 {
    (fraction.clamp(0.0, 1.0) * 100.0).round() as u32
}

fn pill_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        theme::semibold(12.0),
        egui::Color32::PLACEHOLDER,
    );
    galley.size().x + 16.0 + 6.0 + 6.0
}

fn menu_item(ui: &mut egui::Ui, label: &str, color: egui::Color32) -> bool {
    let clicked = ui.button(egui::RichText::new(label).color(color)).clicked();
    if clicked {
        ui.close();
    }
    clicked
}
