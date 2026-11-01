//! The left sidebar: brand, search, grouped instance list, and the pinned actions.
//!
//! Rows use [`widgets::row`]. Exactly one status sits on the right: a running game, a launcher
//! op, an external `pwc` lock, waiting commits, or a readiness dot.

use watt_core::events::{OpKind, OpOutcome};
use watt_core::meta::Binding;
use watt_core::status::Readiness;

use super::instance_tabs::paint_ellipsis;
use super::{App, MainView};
use crate::gui::backend::InstanceEntry;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Icon, Size, Variant};

const ROW_H: f32 = 54.0;
const ROW_INSET: f32 = 8.0;

impl App {
    pub(crate) fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        self.brand_row(ui);
        self.search_row(ui);
        ui.add_space(8.0);

        let bottom_h = super::launcher_update::sidebar_bottom_height(self);
        let list_h = (ui.available_height() - bottom_h).max(0.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(list_h)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(4.0);
                self.instance_list(ui);
                ui.add_space(8.0);
            });

        let p = theme::palette(ui.ctx());
        let y = ui.cursor().top();
        ui.painter().hline(
            ui.max_rect().x_range(),
            y,
            egui::Stroke::new(1.0, p.line_soft),
        );
        egui::Frame::new()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                let width = ui.available_width();
                super::launcher_update::update_row(ui, self);
                if Button::new("New instance")
                    .variant(Variant::Secondary)
                    .size(Size::Medium)
                    .icon(Icon::Plus)
                    .min_width(width)
                    .show(ui)
                    .clicked()
                {
                    self.open_new_instance_dialog();
                }
                let on_settings = matches!(self.main_view, Some(MainView::Settings));
                if Button::new("Settings")
                    .variant(if on_settings {
                        Variant::Secondary
                    } else {
                        Variant::Ghost
                    })
                    .size(Size::Small)
                    .min_width(width)
                    .show(ui)
                    .clicked()
                {
                    self.open_settings();
                }
            });

        let rect = ui.max_rect();
        ui.painter().vline(
            rect.right() - 0.5,
            rect.y_range(),
            egui::Stroke::new(1.0, p.line),
        );
    }

    fn brand_row(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), egui::Sense::hover());
        let cube = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.center().y - 11.0),
            egui::Vec2::splat(22.0),
        );
        widgets::cube(ui.painter(), cube, p.volt);
        let galley = ui.painter().layout_no_wrap(
            "watt-start".to_owned(),
            theme::semibold(16.0),
            p.text_bright,
        );
        ui.painter().galley(
            egui::pos2(cube.right() + 10.0, rect.center().y - galley.size().y * 0.5),
            galley,
            p.text_bright,
        );
        if self.startup_check.is_some() {
            let spinner = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 18.0, rect.center().y),
                egui::Vec2::splat(12.0),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(spinner)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            widgets::spinner(&mut child, 12.0, p.arc).on_hover_text("Checking for updates");
        }
    }

    fn search_row(&mut self, ui: &mut egui::Ui) {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
        let field = rect.shrink2(egui::vec2(12.0, 0.0));
        let mut search_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(field)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let response = widgets::search_input_with_id(
            &mut search_ui,
            "sidebar-search",
            &mut self.search,
            "Search instances",
            field.width(),
        );
        if self.focus_search {
            self.focus_search = false;
            response.request_focus();
        }
    }

    fn instance_list(&mut self, ui: &mut egui::Ui) {
        // Plan under `&self` so a row can be built without cloning the catalog entry, then paint.
        for item in self.sidebar_plan() {
            match item {
                SideItem::Heading(title) => group_heading(ui, &title),
                SideItem::Row(row) => self.side_row(ui, &row),
            }
        }
    }

    fn sidebar_plan(&self) -> Vec<SideItem> {
        let needle = self.search.as_str();
        let entries: Vec<&InstanceEntry> = self
            .backend
            .instances()
            .iter()
            .filter(|entry| {
                needle.is_empty()
                    || contains_ignore_case(entry.card.meta.title(&entry.card.name), needle)
                    || contains_ignore_case(&entry.card.name, needle)
            })
            .map(|entry| entry.as_ref())
            .collect();
        let pending = self.pending_rows(needle);
        let mut items = Vec::new();

        let favorites: Vec<&InstanceEntry> = entries
            .iter()
            .copied()
            .filter(|entry| entry.card.meta.favorite)
            .collect();
        if !favorites.is_empty() {
            items.push(SideItem::Heading("Favourites".to_owned()));
            for entry in &favorites {
                items.push(SideItem::Row(self.row_for_entry(entry)));
            }
        }

        let mut groups: Vec<&str> = entries
            .iter()
            .filter(|entry| !entry.card.meta.favorite)
            .filter_map(|entry| entry.card.meta.group.as_deref())
            .collect();
        groups.sort_unstable();
        groups.dedup();
        let grouped = !favorites.is_empty() || !groups.is_empty();
        for group in groups {
            items.push(SideItem::Heading(group.to_owned()));
            for entry in entries.iter().copied().filter(|entry| {
                !entry.card.meta.favorite && entry.card.meta.group.as_deref() == Some(group)
            }) {
                items.push(SideItem::Row(self.row_for_entry(entry)));
            }
        }

        let rest: Vec<&InstanceEntry> = entries
            .iter()
            .copied()
            .filter(|entry| !entry.card.meta.favorite && entry.card.meta.group.is_none())
            .collect();
        let mut showed_other = false;
        if !rest.is_empty() {
            if grouped || !pending.is_empty() {
                items.push(SideItem::Heading("Other".to_owned()));
                showed_other = true;
            }
            for entry in rest {
                items.push(SideItem::Row(self.row_for_entry(entry)));
            }
        }
        if !pending.is_empty() {
            if !showed_other {
                items.push(SideItem::Heading("Other".to_owned()));
            }
            items.extend(pending.into_iter().map(SideItem::Row));
        }
        items
    }

    /// Ops whose key is not a catalog entry yet (Create, Duplicate). Sorted by title, then key,
    /// so a `HashMap` walk does not shuffle the list between frames.
    fn pending_rows(&self, needle: &str) -> Vec<SideRow> {
        let mut pending: Vec<_> = self
            .ops
            .iter()
            .filter(|(_, slot)| matches!(slot.kind(), OpKind::Create | OpKind::Duplicate))
            .filter(|(key, _)| {
                self.backend
                    .instances()
                    .iter()
                    .all(|entry| &entry.card.name != *key)
            })
            .filter(|(key, slot)| {
                needle.is_empty()
                    || contains_ignore_case(&slot.title, needle)
                    || contains_ignore_case(key, needle)
            })
            .map(|(key, slot)| {
                (
                    key.clone(),
                    slot.title.clone(),
                    slot.kind(),
                    slot.running(),
                    slot.view.fraction(),
                    slot.view.outcome.clone(),
                )
            })
            .collect();
        pending.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        pending
            .into_iter()
            .map(|(key, title, kind, running, fraction, outcome)| {
                let (subtitle, status, progress) = if running {
                    (
                        pending_subtitle(kind).to_owned(),
                        SideStatus::Spinner,
                        Some(fraction),
                    )
                } else {
                    let (subtitle, color) = match outcome {
                        Some(OpOutcome::Ok { .. }) => ("Finished", theme::OK),
                        Some(OpOutcome::Failed { .. }) => ("Failed", theme::BAD),
                        Some(OpOutcome::Cancelled) | None => ("Cancelled", theme::NEUTRAL),
                    };
                    (subtitle.to_owned(), SideStatus::Dot(color), None)
                };
                SideRow {
                    selected: matches!(&self.main_view, Some(MainView::Instance(current)) if current == &key),
                    key,
                    title,
                    subtitle,
                    accent: theme::VOLT,
                    status,
                    progress,
                }
            })
            .collect()
    }

    fn row_for_entry(&self, entry: &InstanceEntry) -> SideRow {
        let name = entry.card.name.clone();
        SideRow {
            key: name.clone(),
            title: entry.card.meta.title(&name).to_owned(),
            subtitle: subtitle(entry),
            accent: theme::accent_for_meta(&name, entry.card.meta.color.as_deref()),
            selected: matches!(&self.main_view, Some(MainView::Instance(current)) if current == &name),
            status: self.row_status(entry),
            progress: self.row_progress(entry),
        }
    }

    fn row_status(&self, entry: &InstanceEntry) -> SideStatus {
        if entry.status.running.is_some() {
            return SideStatus::Pulse;
        }
        if self.op_kind_running(&entry.card.name).is_some() || entry.status.busy {
            return SideStatus::Spinner;
        }
        let commits: usize = self
            .updates
            .get(&entry.card.name)
            .map(|logs| logs.iter().map(|log| log.commits.len()).sum())
            .unwrap_or(0);
        if commits > 0 {
            return SideStatus::Count(commits);
        }
        SideStatus::Dot(readiness_color(&entry.status.readiness))
    }

    fn row_progress(&self, entry: &InstanceEntry) -> Option<f32> {
        if entry.status.running.is_some() {
            return None;
        }
        self.running_op(&entry.card.name)
            .map(|slot| slot.view.fraction())
    }

    fn side_row(&mut self, ui: &mut egui::Ui, row: &SideRow) {
        let key = row.key.clone();
        // `widgets::row` keys hover by its response id, and that id is positional: the salt only
        // reaches the inner child. Wrapping the row keeps one instance's hover off the next.
        ui.push_id(&row.key, |ui| {
            let full = ui.available_width();
            let (outer, _) = ui.allocate_exact_size(egui::vec2(full, ROW_H), egui::Sense::hover());
            let rect = outer.shrink2(egui::vec2(ROW_INSET, 0.0));
            let mut host = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let response = widgets::row(
                &mut host,
                "instance",
                ROW_H,
                row.selected,
                |ui, row_rect, _| {
                    paint_side_row(ui, row_rect, row);
                },
            );
            if response.clicked() {
                self.select_instance(&key);
            }
        });
    }

    fn select_instance(&mut self, name: &str) {
        if let Some(MainView::Instance(current)) = &self.main_view {
            let current = current.clone();
            self.commit_drafts(&current);
        }
        self.main_view = Some(MainView::Instance(name.to_owned()));
        self.instance_tab = super::InstanceTab::Overview;
    }

    fn open_settings(&mut self) {
        if let Some(MainView::Instance(name)) = &self.main_view {
            let name = name.clone();
            self.commit_drafts(&name);
        }
        self.main_view = Some(MainView::Settings);
    }
}

enum SideItem {
    Heading(String),
    Row(SideRow),
}

struct SideRow {
    key: String,
    title: String,
    subtitle: String,
    accent: egui::Color32,
    selected: bool,
    status: SideStatus,
    progress: Option<f32>,
}

enum SideStatus {
    Pulse,
    Spinner,
    Count(usize),
    Dot(egui::Color32),
}

/// Case-insensitive `contains`. ASCII text (the common case) is compared in place; anything
/// else falls back to lowercased copies so "Ö" still matches "ö".
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if !haystack.is_ascii() || !needle.is_ascii() {
        return haystack.to_lowercase().contains(&needle.to_lowercase());
    }
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn paint_side_row(ui: &mut egui::Ui, rect: egui::Rect, row: &SideRow) {
    if row.selected {
        let bar = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 1.5, rect.center().y),
            egui::vec2(3.0, 28.0),
        );
        ui.painter().rect_filled(bar, 1.5, row.accent);
    }
    let cube = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.center().y - 14.0),
        egui::Vec2::splat(28.0),
    );
    widgets::cube(ui.painter(), cube, row.accent);

    let status_w = status_width(ui, &row.status);
    let status_h = match row.status {
        SideStatus::Count(_) => 18.0,
        SideStatus::Spinner => 14.0,
        SideStatus::Pulse | SideStatus::Dot(_) => 8.0,
    };
    let status_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.right() - 10.0 - status_w,
            rect.center().y - status_h * 0.5,
        ),
        egui::vec2(status_w, status_h),
    );
    let text_left = cube.right() + 10.0;
    let text_w = (status_rect.left() - 8.0 - text_left).max(8.0);
    let p = theme::palette(ui.ctx());
    let title_font = theme::semibold(14.0);
    let sub_font = theme::regular(12.0);
    let title_h = ui
        .painter()
        .layout_no_wrap("Ag".to_owned(), title_font.clone(), p.text_bright)
        .size()
        .y;
    let sub_h = ui
        .painter()
        .layout_no_wrap("Ag".to_owned(), sub_font.clone(), p.text_muted)
        .size()
        .y;
    let y = rect.center().y - (title_h + 2.0 + sub_h) * 0.5;
    paint_ellipsis(
        ui.painter(),
        egui::pos2(text_left, y),
        &row.title,
        title_font,
        p.text_bright,
        text_w,
    );
    paint_ellipsis(
        ui.painter(),
        egui::pos2(text_left, y + title_h + 2.0),
        &row.subtitle,
        sub_font,
        p.text_muted,
        text_w,
    );

    let mut status_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(status_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    paint_status(&mut status_ui, &row.status);

    if let Some(fraction) = row.progress {
        let line_w = (rect.width() - 16.0) * fraction.clamp(0.0, 1.0);
        if line_w > 0.5 {
            let line = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, rect.bottom() - 2.0),
                egui::vec2(line_w, 2.0),
            );
            ui.painter().rect_filled(line, 1.0, p.volt);
        }
    }
}

fn paint_status(ui: &mut egui::Ui, status: &SideStatus) {
    let p = theme::palette(ui.ctx());
    match status {
        SideStatus::Pulse => live_dot(ui, p.arc, 8.0),
        SideStatus::Spinner => {
            widgets::spinner(ui, 14.0, p.arc);
        }
        SideStatus::Count(count) => count_pill(ui, *count),
        SideStatus::Dot(color) => {
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(8.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, *color);
        }
    }
}

fn status_width(ui: &egui::Ui, status: &SideStatus) -> f32 {
    match status {
        SideStatus::Pulse | SideStatus::Dot(_) => 8.0,
        SideStatus::Spinner => 14.0,
        SideStatus::Count(count) => count_pill_width(ui, *count),
    }
}

fn count_pill(ui: &mut egui::Ui, count: usize) {
    let p = theme::palette(ui.ctx());
    let width = count_pill_width(ui, count);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 9.0, p.arc_tint);
    let galley = ui
        .painter()
        .layout_no_wrap(count.to_string(), theme::semibold(11.0), p.arc);
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        p.arc,
    );
}

fn count_pill_width(ui: &egui::Ui, count: usize) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        count.to_string(),
        theme::semibold(11.0),
        theme::palette(ui.ctx()).arc,
    );
    (galley.size().x + 12.0).max(18.0)
}

fn live_dot(ui: &mut egui::Ui, color: egui::Color32, diameter: f32) {
    // A running game must not request a repaint. The dot stays a steady arc mark;
    // the catalog refresh in `App::ui` is what clears "Running" after the game exits.
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(diameter), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter * 0.5, color);
}

fn group_heading(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
    let mut job = egui::text::LayoutJob::single_section(
        text.to_uppercase(),
        egui::text::TextFormat {
            font_id: theme::semibold(11.0),
            extra_letter_spacing: 0.8,
            color: p.text_muted,
            ..Default::default()
        },
    );
    job.wrap.max_width = (rect.width() - 24.0).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(rect.left() + 12.0, rect.top() + 8.0),
        galley,
        p.text_muted,
    );
}

fn subtitle(entry: &InstanceEntry) -> String {
    match &entry.card.meta.source {
        Binding::Managed { channel, .. } => format!(
            "{} · {channel}",
            entry.card.game.version.as_deref().unwrap_or("—")
        ),
        Binding::Linked { .. } => format!(
            "linked · {}",
            entry.card.game.branch.as_deref().unwrap_or("unknown")
        ),
        Binding::PwcDefault => "pwc".to_owned(),
    }
}

fn pending_subtitle(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Duplicate => "Copying…",
        OpKind::Create => "Creating…",
        OpKind::Update => "Updating…",
        OpKind::Build => "Building…",
        OpKind::Play => "Starting…",
        OpKind::Rollback => "Rolling back…",
        OpKind::Remove => "Removing…",
        OpKind::Modules => "Applying modules…",
        OpKind::SetSource => "Switching source…",
        OpKind::CheckUpdates => "Checking…",
        OpKind::Maintenance => "Cleaning up…",
    }
}

fn readiness_color(readiness: &Readiness) -> egui::Color32 {
    match readiness {
        Readiness::Ready { .. } => theme::OK,
        Readiness::NeedsBuild { .. } => theme::VOLT,
        Readiness::Broken { .. } => theme::BAD,
        Readiness::New => theme::NEUTRAL,
    }
}
