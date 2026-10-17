//! First-run welcome, and the "still loading the catalog" state.
//!
//! Both paint over the whole window. Settings stays reachable from either, so an empty catalog
//! cannot trap the user. Leaving the last instance still commits drafts via [`App::note_view_changed`].

use watt_core::doctor::{Check, Level};

use super::MainView;
use crate::gui::backend::Loaded;
use crate::gui::theme;
use crate::gui::widgets::{self, Button, Size, Variant};

const COLUMN_W: f32 = 560.0;

impl super::App {
    pub(crate) fn welcome_screen(&mut self, ui: &mut egui::Ui) {
        self.note_view_changed();
        let p = theme::palette(ui.ctx());
        ui.painter().rect_filled(ui.max_rect(), 0.0, p.bg_surface);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.add_space(56.0);
                let avail = ui.available_width();
                let column_w = COLUMN_W.min((avail - 32.0).max(240.0));
                let side = ((avail - column_w) * 0.5).max(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(column_w);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        self.welcome_column(ui);
                    });
                });
                ui.add_space(32.0);
            });
    }

    /// Catalog still loading and nothing to list yet. Distinct from "no instances".
    pub(crate) fn catalog_loading(&mut self, ui: &mut egui::Ui) {
        self.note_view_changed();
        let p = theme::palette(ui.ctx());
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, p.bg_surface);
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(full)
                .layout(egui::Layout::top_down(egui::Align::Center)),
        );
        body.add_space((full.height() * 0.36).max(48.0));
        let (spinner, _) = body.allocate_exact_size(egui::Vec2::splat(28.0), egui::Sense::hover());
        let mut spin = body.new_child(
            egui::UiBuilder::new()
                .max_rect(spinner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        widgets::spinner(&mut spin, 28.0, p.arc);
        body.add_space(14.0);
        body.label(
            egui::RichText::new("Looking for instances…")
                .font(theme::semibold(16.0))
                .color(p.text),
        );
        body.add_space(20.0);
        if Button::new("Settings")
            .variant(Variant::Ghost)
            .size(Size::Small)
            .show(&mut body)
            .clicked()
        {
            self.main_view = Some(MainView::Settings);
        }
    }

    fn welcome_column(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            let (cube, _) = ui.allocate_exact_size(egui::Vec2::splat(72.0), egui::Sense::hover());
            widgets::cube(ui.painter(), cube, p.volt);
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Welcome to watt-start")
                    .font(theme::semibold(28.0))
                    .color(p.text_bright),
            );
        });
        ui.add_space(8.0);
        centered_text(
            ui,
            "Keep Project Watt Cubed instances up to date and built, in one click.",
            theme::regular(14.0),
            p.text_muted,
        );
        ui.add_space(24.0);

        widgets::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            widgets::section_heading(ui, "System checks");
            match self.backend.doctor() {
                Loaded::Ready(checks) => {
                    for check in &checks {
                        doctor_row(ui, check);
                    }
                }
                Loaded::Loading | Loaded::Idle => {
                    let p = theme::palette(ui.ctx());
                    ui.label(
                        egui::RichText::new("Checking your machine…")
                            .font(theme::regular(14.0))
                            .color(p.text_muted),
                    );
                }
                Loaded::Err(error) => {
                    let p = theme::palette(ui.ctx());
                    ui.label(
                        egui::RichText::new(error)
                            .font(theme::regular(14.0))
                            .color(p.bad),
                    );
                }
            }
        });

        ui.add_space(24.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            if Button::new("Create your first instance")
                .variant(Variant::Primary)
                .size(Size::Large)
                .min_width(260.0)
                .show(ui)
                .clicked()
            {
                self.open_new_instance_dialog();
            }
            ui.add_space(10.0);
            let p = theme::palette(ui.ctx());
            ui.label(
                egui::RichText::new("Instances made with pwc show up here too.")
                    .font(theme::regular(12.0))
                    .color(p.text_muted),
            );
            ui.add_space(8.0);
            if Button::new("Settings")
                .variant(Variant::Ghost)
                .size(Size::Small)
                .show(ui)
                .clicked()
            {
                self.main_view = Some(MainView::Settings);
            }
        });
    }
}

fn doctor_row(ui: &mut egui::Ui, check: &Check) {
    let p = theme::palette(ui.ctx());
    let color = match check.level {
        Level::Ok => p.ok,
        Level::Info => p.arc,
        Level::Warn => p.warn,
        Level::Error => p.bad,
    };
    let name_galley =
        ui.painter()
            .layout_no_wrap(check.name.clone(), theme::semibold(13.0), p.text_bright);
    let name_w = name_galley.size().x;
    let row_h = 22.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::hover(),
    );
    ui.painter()
        .circle_filled(egui::pos2(rect.left() + 4.0, rect.center().y), 4.0, color);
    let name_x = rect.left() + 16.0;
    ui.painter().galley(
        egui::pos2(name_x, rect.center().y - name_galley.size().y * 0.5),
        name_galley,
        p.text_bright,
    );
    let detail_x = name_x + name_w + 10.0;
    let detail_w = (rect.right() - detail_x).max(24.0);
    let mut job = egui::text::LayoutJob::single_section(
        check.detail.clone(),
        egui::text::TextFormat {
            font_id: theme::regular(13.0),
            color: p.text_muted,
            ..Default::default()
        },
    );
    job.wrap.max_width = detail_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let detail = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(detail_x, rect.center().y - detail.size().y * 0.5),
        detail,
        p.text_muted,
    );
    let full_w = ui
        .painter()
        .layout_no_wrap(check.detail.clone(), theme::regular(13.0), p.text_muted)
        .size()
        .x;
    if full_w > detail_w {
        let _ = response.on_hover_text(check.detail.clone());
    }
    if !check.hint.is_empty() {
        ui.add_space(2.0);
        let (hint_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
        let mut hint = egui::text::LayoutJob::single_section(
            check.hint.clone(),
            egui::text::TextFormat {
                font_id: theme::regular(12.0),
                color: p.text_muted,
                ..Default::default()
            },
        );
        hint.wrap.max_width = (hint_rect.width() - 16.0).max(24.0);
        hint.wrap.max_rows = 1;
        hint.wrap.break_anywhere = true;
        hint.wrap.overflow_character = Some('…');
        let galley = ui.painter().layout_job(hint);
        ui.painter().galley(
            egui::pos2(
                hint_rect.left() + 16.0,
                hint_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            p.text_muted,
        );
    }
}

fn centered_text(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: egui::Color32) {
    let width = ui.available_width();
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = width;
    job.halign = egui::Align::Center;
    ui.label(job);
}
