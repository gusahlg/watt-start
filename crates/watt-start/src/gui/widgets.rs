//! Shared Filament controls. Screens paint these instead of raw egui widgets.
//!
//! Every control returns an [`egui::Response`], blends hover and press with
//! [`crate::gui::theme::anim_secs`], and sets the pointing-hand cursor while it can be clicked.
//! Disabled colours are painted explicitly: egui's `add_enabled` fades the painter by
//! `disabled_alpha`, and these widgets undo that fade so the spec alphas stay exact.
//!
//! The kit is unused by the current screens (later phases adopt it). The snapshot gallery is the
//! only caller in this phase.

#![allow(dead_code)]

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, Layout, Pos2, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::gui::theme::{self, Palette};

/// Danger-primary fills: darker than [`theme::BAD`] so white text stays >= 4.5:1.
const DANGER_FILL: Color32 = Color32::from_rgb(0xC9, 0x34, 0x30);
const DANGER_FILL_HOVER: Color32 = Color32::from_rgb(0xD6, 0x40, 0x3B);
const DANGER_FILL_PRESSED: Color32 = Color32::from_rgb(0xB0, 0x2A, 0x26);

/// Visual weight of a [`button`] or [`icon_button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Volt fill, ink text. The one primary action on a surface.
    Primary,
    /// Raised fill, line stroke, body text.
    Secondary,
    /// No fill until hover.
    Ghost,
    /// Ghost with bad-coloured text. Confirms inside a dialog, never the only action.
    Danger,
    /// Bad fill, white text. A destructive primary such as Delete.
    DangerPrimary,
}

/// Button height. Small is 28, medium 36, large 44.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// 28px, radius 6. Toolbars and rows.
    Small,
    /// 36px, radius 8. Default.
    Medium,
    /// 44px, radius 8. The main action on a page.
    Large,
}

impl Size {
    fn height(self) -> f32 {
        match self {
            Self::Small => 28.0,
            Self::Medium => 36.0,
            Self::Large => 44.0,
        }
    }

    fn radius(self) -> u8 {
        match self {
            Self::Small => 6,
            Self::Medium | Self::Large => 8,
        }
    }

    fn pad_x(self) -> f32 {
        match self {
            Self::Small => 10.0,
            Self::Medium | Self::Large => 14.0,
        }
    }

    fn icon(self) -> f32 {
        match self {
            Self::Small => 14.0,
            Self::Medium | Self::Large => 16.0,
        }
    }
}

/// A text button with an optional leading icon and trailing count.
///
/// [`Button::show`] applies [`.enabled`](Button::enabled) and the disabled tooltip.
/// [`egui::Ui::add`] paints the button but ignores those two fields; prefer [`button`] or `show`.
#[must_use]
#[derive(Clone, Debug)]
pub struct Button<'a> {
    text: &'a str,
    variant: Variant,
    size: Size,
    min_width: f32,
    icon: Option<Icon>,
    badge: Option<usize>,
    enabled: Option<bool>,
    disabled_hover: Option<&'a str>,
    force_hover: bool,
    force_pressed: bool,
}

impl<'a> Button<'a> {
    /// Secondary, medium, shrink-wrapped to the label.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            variant: Variant::Secondary,
            size: Size::Medium,
            min_width: 0.0,
            icon: None,
            badge: None,
            enabled: None,
            disabled_hover: None,
            force_hover: false,
            force_pressed: false,
        }
    }

    /// [`Variant::Secondary`] by default.
    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    /// [`Size::Medium`] by default.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Floor on the outer width, including horizontal padding.
    pub fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = min_width;
        self
    }

    /// Painted icon before the label.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Arc count drawn after the label.
    pub fn badge(mut self, count: usize) -> Self {
        self.badge = Some(count);
        self
    }

    /// `false` disables the button even when the parent [`Ui`] is enabled.
    ///
    /// Applied by [`Button::show`], not by [`egui::Ui::add`].
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }

    /// Tooltip shown while the button is disabled and the pointer is over it.
    ///
    /// Callers can also use [`egui::Response::on_disabled_hover_text`] on the returned response.
    /// Applied by [`Button::show`].
    pub fn on_disabled_hover_text(mut self, text: &'a str) -> Self {
        self.disabled_hover = Some(text);
        self
    }

    /// Paint the hover face even when the pointer is elsewhere. Gallery only.
    pub fn force_hover(mut self, force: bool) -> Self {
        self.force_hover = force;
        self
    }

    /// Paint the pressed face. Gallery only.
    pub fn force_pressed(mut self, force: bool) -> Self {
        self.force_pressed = force;
        self
    }

    /// Allocate, paint, and apply [`.enabled`](Self::enabled) plus the disabled tooltip.
    pub fn show(self, ui: &mut Ui) -> Response {
        let tip = self.disabled_hover;
        let enabled = self.enabled.unwrap_or(true);
        let response = ui.add_enabled(enabled, self);
        if let Some(tip) = tip {
            response.on_disabled_hover_text(tip)
        } else {
            response
        }
    }

    fn paint(self, ui: &mut Ui) -> Response {
        undo_disabled_fade(ui);
        let font = theme::semibold(14.0);
        let galley = if self.text.is_empty() {
            None
        } else {
            Some(
                ui.painter()
                    .layout_no_wrap(self.text.to_owned(), font, Color32::PLACEHOLDER),
            )
        };
        let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
        let icon_px = self.size.icon();
        let has_icon = self.icon.is_some();
        let badge_w = self
            .badge
            .map(|count| badge_width(ui, count))
            .unwrap_or(0.0);
        let gap = 8.0;
        let mut content_w = text_w;
        if has_icon {
            content_w += icon_px;
            if text_w > 0.0 || badge_w > 0.0 {
                content_w += gap;
            }
        }
        if badge_w > 0.0 && (text_w > 0.0 || has_icon) {
            content_w += gap + badge_w;
        } else {
            content_w += badge_w;
        }
        let width = (content_w + self.size.pad_x() * 2.0).max(self.min_width);
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, self.size.height()), Sense::click());
        let fg = paint_chrome(
            ui,
            rect,
            &response,
            self.variant,
            self.size.radius(),
            self.force_hover,
            self.force_pressed,
        );
        let painter = ui.painter();
        let mut x = rect.center().x - content_w * 0.5;
        let cy = rect.center().y;
        if let Some(icon) = self.icon {
            let icon_rect =
                Rect::from_center_size(pos2(x + icon_px * 0.5, cy), Vec2::splat(icon_px));
            paint_icon(painter, icon_rect, icon, fg, 0.0);
            x += icon_px;
            if text_w > 0.0 || badge_w > 0.0 {
                x += gap;
            }
        }
        if let Some(galley) = galley {
            let y = cy - galley.size().y * 0.5;
            painter.galley(pos2(x, y), galley, fg);
            x += text_w;
            if badge_w > 0.0 {
                x += gap;
            }
        }
        if let Some(count) = self.badge {
            paint_count_badge(
                painter,
                pos2(x, cy - 8.0),
                count,
                theme::palette(ui.ctx()).arc,
                theme::palette(ui.ctx()).on_accent,
            );
        }
        hand(response)
    }
}

impl egui::Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.paint(ui)
    }
}

/// Label button. Equivalent to [`Button::new`] with the variant and size, then [`Button::show`].
pub fn button(ui: &mut Ui, text: &str, variant: Variant, size: Size) -> Response {
    Button::new(text).variant(variant).size(size).show(ui)
}

/// Square button that paints a single [`Icon`]. `size` is the outer side in points.
///
/// An empty `tooltip` shows nothing. The same text is used for the enabled and disabled hover.
pub fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    variant: Variant,
    size: f32,
    tooltip: &str,
) -> Response {
    undo_disabled_fade(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let radius = if size < 32.0 { 6 } else { 8 };
    let fg = paint_chrome(ui, rect, &response, variant, radius, false, false);
    let icon_px = (size * 0.55).clamp(12.0, 22.0);
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_px));
    paint_icon(ui.painter(), icon_rect, icon, fg, 0.0);
    let response = hand(response);
    if tooltip.is_empty() {
        response
    } else {
        response
            .on_hover_text(tooltip)
            .on_disabled_hover_text(tooltip)
    }
}

/// Full-width list row.
///
/// The rect is allocated and sensed first, then `add_contents` runs in a child [`Ui`] clipped to
/// that rect with selectable labels off, so a label does not steal the row click. A nested control
/// (a [`icon_button`] at the right, for example) is registered later and takes the click instead.
///
/// The child is given the full row rect. Callers inset their own content by about 12px. A sidebar
/// accent bar is the caller's to paint; this row does not add one.
///
/// `selected` uses the raised fill. Hover and press blend from there, or from transparent when the
/// row is not selected.
pub fn row<R>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    height: f32,
    selected: bool,
    add_contents: impl FnOnce(&mut Ui, Rect, &Response) -> R,
) -> Response {
    undo_disabled_fade(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let p = theme::palette(ui.ctx());
    let enabled = response.enabled();
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();
    let hover_t = anim_flag(ui, response.id.with("hover"), hovered);
    let press_t = anim_flag(ui, response.id.with("press"), pressed);
    let (rest, hot, down) = if selected {
        (p.bg_raised, p.bg_overlay, p.pressed_fill)
    } else {
        (Color32::TRANSPARENT, p.wash_hover, p.wash_pressed)
    };
    let fill = lerp_color(lerp_color(rest, hot, hover_t), down, press_t);
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, 8.0, fill);
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center))
            .id_salt(id_salt),
    );
    child.style_mut().interaction.selectable_labels = false;
    child.shrink_clip_rect(rect);
    add_contents(&mut child, rect, &response);
    if enabled && response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            8.0,
            Stroke::new(1.5, p.arc),
            StrokeKind::Inside,
        );
    }
    hand(response)
}

/// 40×22 switch. `on` flips when the control is clicked.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    undo_disabled_fade(ui);
    if !ui.is_enabled() {
        ui.set_opacity(ui.painter().opacity() * 0.4);
    }
    let (rect, mut response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let state = *on;
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, true, state, ""));
    let p = theme::palette(ui.ctx());
    let enabled = response.enabled();
    let hovered = enabled && response.hovered();
    let hover_t = anim_flag(ui, response.id.with("hover"), hovered);
    let off_hot = p.line_strong.lerp_to_gamma(Color32::WHITE, 0.10);
    let rest = if *on { p.volt } else { p.line_strong };
    let hot = if *on { p.volt_hover } else { off_hot };
    let track = lerp_color(rest, hot, hover_t);
    let painter = ui.painter();
    painter.rect_filled(rect, 11.0, track);
    let start = rect.left() + 3.0;
    let end = rect.right() - 3.0 - 16.0;
    let target = if *on { end } else { start };
    let x = ui.ctx().animate_value_with_time(
        response.id.with("knob"),
        target,
        theme::anim_secs(ui.ctx(), 0.12),
    );
    let travel = (end - start).max(1.0);
    let along = ((x - start) / travel).clamp(0.0, 1.0);
    let knob = Rect::from_min_size(pos2(x, rect.center().y - 8.0), vec2(16.0, 16.0));
    painter.circle_filled(knob.center(), 8.0, lerp_color(p.text, p.on_accent, along));
    if enabled && response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            13.0,
            Stroke::new(2.0, p.arc),
            StrokeKind::Inside,
        );
    }
    hand(response)
}

/// Settings row: bright label and muted description on the left, [`switch`] on the right.
///
/// Clicking the label flips `on` as well. The returned response covers both hit targets.
/// Two rows with the same label and description in one parent share an id.
pub fn switch_row(ui: &mut Ui, on: &mut bool, label: &str, description: &str) -> Response {
    undo_disabled_fade(ui);
    let p = theme::palette(ui.ctx());
    let width = ui.available_width();
    let height = if description.is_empty() { 36.0 } else { 52.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let label_rect = Rect::from_min_max(rect.min, pos2(rect.right() - 48.0, rect.bottom()));
    let label_id = ui.scope_id().with((label, description));
    let mut label_response = ui.interact(label_rect, label_id, Sense::click());
    if label_response.clicked() {
        *on = !*on;
        label_response.mark_changed();
    }
    let label_galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), theme::semibold(14.0), p.text_bright);
    let desc_galley = if description.is_empty() {
        None
    } else {
        Some(ui.painter().layout_no_wrap(
            description.to_owned(),
            theme::regular(12.0),
            p.text_muted,
        ))
    };
    let desc_h = desc_galley
        .as_ref()
        .map(|g| 2.0 + g.size().y)
        .unwrap_or(0.0);
    let block_h = label_galley.size().y + desc_h;
    let mut y = rect.center().y - block_h * 0.5;
    ui.painter()
        .galley(pos2(rect.left(), y), label_galley, p.text_bright);
    if let Some(galley) = desc_galley {
        y += block_h - galley.size().y;
        ui.painter()
            .galley(pos2(rect.left(), y), galley, p.text_muted);
    }
    let switch_rect = Rect::from_min_size(
        pos2(rect.right() - 40.0, rect.center().y - 11.0),
        vec2(40.0, 22.0),
    );
    let mut switch_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(switch_rect)
            .layout(Layout::left_to_right(Align::Center))
            .id_salt((label, "switch")),
    );
    let switch_response = switch(&mut switch_ui, on);
    let response = label_response.union(switch_response);
    hand(response)
}

/// Capsule of 12px text. `bg` is the fill and `fg` the text. Not a control.
pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::regular(12.0), fg);
    let width = galley.size().x + 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 255.0, bg);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter().galley(pos2(rect.left() + 8.0, y), galley, fg);
    response
}

/// Numeric capsule, raised fill, body text. Used beside tab labels and section titles.
pub fn count_badge(ui: &mut Ui, count: usize) -> Response {
    let p = theme::palette(ui.ctx());
    let text = count.to_string();
    let galley = ui
        .painter()
        .layout_no_wrap(text, theme::semibold(11.0), p.text);
    let width = (galley.size().x + 10.0).max(18.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    ui.painter().rect_filled(rect, 9.0, p.bg_raised);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter().galley(
        pos2(rect.center().x - galley.size().x * 0.5, y),
        galley,
        p.text,
    );
    response
}

/// Status capsule: tinted fill, a 6px dot, and 12px semibold text in `color`.
///
/// When `pulsing` is set and animations are on, the dot breathes over 1.6s and the widget
/// requests repaints only for that duration. Animations off paints a steady dot.
pub fn status_pill(ui: &mut Ui, text: &str, color: Color32, pulsing: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::semibold(12.0), color);
    let width = galley.size().x + 16.0 + 6.0 + 6.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 12.0, theme::with_alpha(color, 38));
    let mut dot = color;
    if pulsing && theme::anim_secs(ui.ctx(), 1.0) > 0.0 {
        let t = ui.input(|i| i.time) as f32;
        let phase = (t * std::f32::consts::TAU / 1.6).sin();
        let wobble = 0.55 + 0.45 * (0.5 + 0.5 * phase);
        dot = color.gamma_multiply(wobble);
        ui.ctx().request_repaint();
    }
    ui.painter()
        .circle_filled(pos2(rect.left() + 12.0, rect.center().y), 3.0, dot);
    let y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(pos2(rect.left() + 22.0, y), galley, color);
    response
}

/// Card: [`Palette::bg_card`], soft line, radius 12, padding 16.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let p = theme::palette(ui.ctx());
    egui::Frame::new()
        .fill(p.bg_card)
        .stroke(Stroke::new(1.0, p.line_soft))
        .corner_radius(12.0)
        .inner_margin(16)
        .show(ui, add_contents)
}

/// [`card`] plus a 3px bar in `color` along the left edge, inset so it stays inside the radius.
pub fn card_with_bar<R>(
    ui: &mut Ui,
    color: Color32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let inner = card(ui, add_contents);
    let rect = inner.response.rect;
    let bar = Rect::from_min_max(
        pos2(rect.left() + 1.0, rect.top() + 12.0),
        pos2(rect.left() + 4.0, rect.bottom() - 12.0),
    );
    ui.painter().rect_filled(
        bar,
        CornerRadius {
            nw: 2,
            ne: 0,
            sw: 2,
            se: 0,
        },
        color,
    );
    inner
}

/// 15px semibold bright heading above a group of controls.
pub fn section_heading(ui: &mut Ui, text: &str) -> Response {
    let p = theme::palette(ui.ctx());
    ui.label(
        egui::RichText::new(text)
            .font(theme::semibold(15.0))
            .color(p.text_bright),
    )
}

/// Segmented control. Each item is a label plus an optional count.
///
/// The highlight slides over 150ms. Returns whether `selected` changed.
/// `id` must be unique among segmented controls in the same parent.
pub fn segmented(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: &mut usize,
    items: &[(&str, Option<usize>)],
) -> bool {
    undo_disabled_fade(ui);
    if items.is_empty() {
        return false;
    }
    let p = theme::palette(ui.ctx());
    let font = theme::semibold(13.0);
    let mut widths = Vec::with_capacity(items.len());
    for (label, count) in items {
        let galley = ui
            .painter()
            .layout_no_wrap((*label).to_owned(), font.clone(), p.text);
        let mut w = galley.size().x + 24.0;
        if count.is_some() {
            w += 22.0;
        }
        widths.push(w);
    }
    let total = widths.iter().sum::<f32>() + 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 32.0), Sense::hover());
    ui.painter().rect(
        rect,
        8.0,
        p.bg_card,
        Stroke::new(1.0, p.line),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 3.0;
    let mut target_x = x;
    let mut target_w = widths.first().copied().unwrap_or(0.0);
    for (i, w) in widths.iter().enumerate() {
        if i == *selected {
            target_x = x;
            target_w = *w;
        }
        x += *w;
    }
    let anim_id = ui.scope_id().with(id);
    let ax = ui.ctx().animate_value_with_time(
        anim_id.with("x"),
        target_x,
        theme::anim_secs(ui.ctx(), 0.15),
    );
    let aw = ui.ctx().animate_value_with_time(
        anim_id.with("w"),
        target_w,
        theme::anim_secs(ui.ctx(), 0.15),
    );
    let highlight = Rect::from_min_size(pos2(ax, rect.top() + 3.0), vec2(aw.max(0.0), 26.0));
    ui.painter().rect_filled(highlight, 6.0, p.bg_overlay);

    let mut changed = false;
    let mut cursor = rect.left() + 3.0;
    for (i, ((label, count), w)) in items.iter().zip(widths).enumerate() {
        let seg = Rect::from_min_size(pos2(cursor, rect.top() + 3.0), vec2(w, 26.0));
        let response = ui.interact(seg, anim_id.with(i), Sense::click());
        if response.clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
        if response.enabled() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let active = i == *selected;
        let fg = if active {
            p.text_bright
        } else if response.hovered() && response.enabled() {
            p.text
        } else {
            p.text_muted
        };
        let galley = ui
            .painter()
            .layout_no_wrap((*label).to_owned(), font.clone(), fg);
        let text_w = galley.size().x;
        let extra = if count.is_some() { 26.0 } else { 0.0 };
        let mut pen = seg.center().x - (text_w + extra) * 0.5;
        let text_y = seg.center().y - galley.size().y * 0.5;
        ui.painter().galley(pos2(pen, text_y), galley, fg);
        pen += text_w;
        if let Some(count) = count {
            pen += 6.0;
            let badge = count.to_string();
            let badge_galley = ui
                .painter()
                .layout_no_wrap(badge, theme::semibold(11.0), fg);
            ui.painter().galley(
                pos2(pen, seg.center().y - badge_galley.size().y * 0.5),
                badge_galley,
                fg,
            );
        }
        cursor += w;
    }
    changed
}

/// Selectable card with a painted radio, title, description, and extra content when selected.
///
/// The card's click is registered behind its children, so a nested input inside `extra` keeps
/// the pointer. `extra` runs only when `selected` is true. The caller updates `selected`.
pub fn choice_card(
    ui: &mut Ui,
    selected: bool,
    title: &str,
    description: &str,
    extra: impl FnOnce(&mut Ui),
) -> Response {
    undo_disabled_fade(ui);
    let p = theme::palette(ui.ctx());
    let inner = ui.scope_builder(
        UiBuilder::new()
            .sense(Sense::click())
            .id_salt((title, "choice")),
        |ui| {
            let bg = ui.painter().add(Shape::Noop);
            let width = ui.available_width();
            ui.set_min_width(width);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let (radio, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                paint_radio(ui.painter(), radio, selected, &p);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .font(theme::semibold(14.0))
                            .color(p.text_bright),
                    );
                    ui.label(
                        egui::RichText::new(description)
                            .font(theme::regular(12.0))
                            .color(p.text_muted),
                    );
                });
                ui.add_space(12.0);
            });
            if selected {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    extra(ui);
                    ui.add_space(12.0);
                });
            }
            ui.add_space(10.0);
            let rect = ui.min_rect();
            let fill = if selected {
                theme::with_alpha(p.volt, 15)
            } else {
                p.bg_card
            };
            let stroke = if selected {
                Stroke::new(1.5, p.volt)
            } else {
                Stroke::new(1.0, p.line)
            };
            ui.painter().set(
                bg,
                Shape::Rect(egui::epaint::RectShape::new(
                    rect,
                    8.0,
                    fill,
                    stroke,
                    StrokeKind::Inside,
                )),
            );
        },
    );
    hand(inner.response)
}

/// 24×24 colour chip. Hover draws a muted ring; `selected` draws a bright ring 2px outside.
pub fn swatch(ui: &mut Ui, color: Color32, selected: bool) -> Response {
    undo_disabled_fade(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let p = theme::palette(ui.ctx());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, color);
    if response.enabled() && response.hovered() {
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, p.text_muted),
            StrokeKind::Inside,
        );
    }
    if selected {
        painter.rect_stroke(
            rect.expand(2.0),
            8.0,
            Stroke::new(2.0, p.text_bright),
            StrokeKind::Inside,
        );
    }
    hand(response)
}

/// Single-line field. Height 32, canvas fill, line stroke, arc stroke while focused.
///
/// `width` is the outer width. A non-finite width uses the available width.
/// The focus stroke stays 1px (the spec's 1.5px would shift the text when the stroke grows).
pub fn text_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    input_field(ui, text, hint, width, false)
}

/// [`text_input`] with a painted magnifier and extra left padding.
pub fn search_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    input_field(ui, text, hint, width, true)
}

/// Underline tabs.
///
/// `T` identifies a tab. Each entry is `(value, label, optional count, show a volt dot)`.
/// Every label is measured in semibold 14 so the strip does not jump when the selection moves.
/// The underline slides. Returns whether `selected` changed.
pub fn tabs<T: Copy + PartialEq>(
    ui: &mut Ui,
    selected: &mut T,
    items: &[(T, &str, Option<usize>, bool)],
) -> bool {
    undo_disabled_fade(ui);
    if items.is_empty() {
        return false;
    }
    let p = theme::palette(ui.ctx());
    let font = theme::semibold(14.0);
    let salt: String = items
        .iter()
        .map(|(_, label, _, _)| *label)
        .collect::<Vec<_>>()
        .join("|");
    let anim_id = ui.scope_id().with(salt);
    let mut widths = Vec::with_capacity(items.len());
    for (_, label, count, dot) in items {
        let galley = ui
            .painter()
            .layout_no_wrap((*label).to_owned(), font.clone(), p.text);
        let mut w = galley.size().x + 28.0;
        if count.is_some() {
            w += 26.0;
        }
        if *dot {
            w += 12.0;
        }
        widths.push(w);
    }
    let total = widths.iter().sum::<f32>();
    let (rect, _) = ui.allocate_exact_size(vec2(total.max(1.0), 40.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.line),
    );

    let mut cursor = rect.left();
    let mut target = (cursor, widths[0]);
    for (i, w) in widths.iter().enumerate() {
        if items[i].0 == *selected {
            target = (cursor, *w);
        }
        cursor += *w;
    }
    let ux = ui.ctx().animate_value_with_time(
        anim_id.with("x"),
        target.0,
        theme::anim_secs(ui.ctx(), 0.15),
    );
    let uw = ui.ctx().animate_value_with_time(
        anim_id.with("w"),
        target.1,
        theme::anim_secs(ui.ctx(), 0.15),
    );
    let underline = Rect::from_min_size(
        pos2(ux + 8.0, rect.bottom() - 2.0),
        vec2((uw - 16.0).max(8.0), 2.0),
    );
    ui.painter().rect_filled(underline, 1.0, p.volt);

    let mut changed = false;
    cursor = rect.left();
    for (i, ((value, label, count, dot), w)) in items.iter().zip(widths).enumerate() {
        let tab = Rect::from_min_size(pos2(cursor, rect.top()), vec2(w, 40.0));
        let response = ui.interact(tab, anim_id.with(i), Sense::click());
        if response.clicked() && *selected != *value {
            *selected = *value;
            changed = true;
        }
        if response.enabled() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let active = *selected == *value;
        if !active && response.hovered() && response.enabled() {
            let wash = tab.shrink2(vec2(4.0, 6.0));
            ui.painter().rect_filled(wash, 6.0, p.wash_hover);
        }
        let fg = if active {
            p.text_bright
        } else if response.hovered() && response.enabled() {
            p.text
        } else {
            p.text_muted
        };
        let galley = ui
            .painter()
            .layout_no_wrap((*label).to_owned(), font.clone(), fg);
        let text_w = galley.size().x;
        let mut pen = tab.left() + 14.0;
        let y = tab.center().y - galley.size().y * 0.5;
        ui.painter().galley(pos2(pen, y), galley, fg);
        pen += text_w;
        if let Some(count) = count {
            pen += 8.0;
            let badge = count.to_string();
            let badge_galley = ui
                .painter()
                .layout_no_wrap(badge, theme::semibold(11.0), p.text);
            let badge_w = (badge_galley.size().x + 8.0).max(16.0);
            let badge_rect =
                Rect::from_min_size(pos2(pen, tab.center().y - 8.0), vec2(badge_w, 16.0));
            ui.painter().rect_filled(badge_rect, 8.0, p.bg_raised);
            ui.painter().galley(
                pos2(
                    badge_rect.center().x - badge_galley.size().x * 0.5,
                    badge_rect.center().y - badge_galley.size().y * 0.5,
                ),
                badge_galley,
                p.text,
            );
            pen += badge_w;
        }
        if *dot {
            pen += 8.0;
            ui.painter()
                .circle_filled(pos2(pen + 3.0, tab.center().y), 3.0, p.volt);
        }
        cursor += w;
    }
    changed
}

/// Painted icon. No font glyphs. `time` is seconds and only affects [`Icon::Spinner`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Filled isometric cube, the instance mark.
    Cube,
    /// Wireframe of the same cube.
    HollowCube,
    /// Three small cubes, back to front.
    StackedCubes,
    /// Check mark.
    Check,
    /// Close.
    Cross,
    /// Chevron pointing right.
    ChevronRight,
    /// Chevron pointing down.
    ChevronDown,
    /// Plus.
    Plus,
    /// Three vertical dots.
    Kebab,
    /// Magnifier.
    Search,
    /// Folder.
    Folder,
    /// Circular arrow.
    Refresh,
    /// Play triangle.
    Play,
    /// Open arc. Pass time in seconds to [`paint_icon`] to spin it.
    Spinner,
}

impl Icon {
    /// Every icon, in declaration order. The gallery paints this set.
    pub const ALL: [Self; 14] = [
        Self::Cube,
        Self::HollowCube,
        Self::StackedCubes,
        Self::Check,
        Self::Cross,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::Plus,
        Self::Kebab,
        Self::Search,
        Self::Folder,
        Self::Refresh,
        Self::Play,
        Self::Spinner,
    ];
}

/// Paint `icon` inside `rect` (the icon pads itself). `time` spins [`Icon::Spinner`] once a second.
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, time: f32) {
    match icon {
        Icon::Cube => cube(painter, rect, color),
        Icon::HollowCube => paint_hollow_cube(painter, rect, color),
        Icon::StackedCubes => paint_stacked(painter, rect, color),
        Icon::Check => paint_check(painter, rect, color),
        Icon::Cross => paint_cross(painter, rect, color),
        Icon::ChevronRight => paint_chevron(painter, rect, color, false),
        Icon::ChevronDown => paint_chevron(painter, rect, color, true),
        Icon::Plus => paint_plus(painter, rect, color),
        Icon::Kebab => paint_kebab(painter, rect, color),
        Icon::Search => paint_search(painter, rect, color),
        Icon::Folder => paint_folder(painter, rect, color),
        Icon::Refresh => paint_refresh(painter, rect, color),
        Icon::Play => paint_play(painter, rect, color),
        Icon::Spinner => paint_spinner_arc(painter, rect, color, time),
    }
}

/// Spinning arc, one revolution per second. Static when animations are off.
///
/// Requests a repaint only while it is actually spinning.
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let spinning = theme::anim_secs(ui.ctx(), 1.0) > 0.0;
    let time = if spinning {
        ui.ctx().request_repaint();
        ui.input(|i| i.time) as f32
    } else {
        0.15
    };
    paint_icon(ui.painter(), rect, Icon::Spinner, color, time);
    response
}

/// Static isometric cube with three shaded faces. Readable from 16px through 72px.
///
/// Faces are shaded with [`Color32::lerp_to_gamma`] so a translucent `color` keeps its alpha.
pub fn cube(painter: &egui::Painter, rect: Rect, color: Color32) {
    let [t, tl, tr, mid, bl, br, bot] = cube_points(rect);
    let s = rect.width().min(rect.height());
    let edge = if s >= 20.0 {
        Stroke::new((s / 48.0).clamp(0.6, 1.1), Color32::from_black_alpha(45))
    } else {
        Stroke::NONE
    };
    let top = color.lerp_to_gamma(Color32::WHITE, 0.42);
    let left = color.lerp_to_gamma(Color32::BLACK, 0.35);
    painter.add(Shape::convex_polygon(vec![t, tr, mid, tl], top, edge));
    painter.add(Shape::convex_polygon(vec![tr, br, bot, mid], color, edge));
    painter.add(Shape::convex_polygon(vec![tl, mid, bot, bl], left, edge));
}

/// Bar that eases toward `fraction` over 300ms. The first value snaps.
///
/// The track is the overlay colour. The fill uses `color`. Radius is half the height, capped at 4.
pub fn progress_bar(ui: &mut Ui, fraction: f32, color: Color32, height: f32) -> Response {
    let fraction = fraction.clamp(0.0, 1.0);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width.max(1.0), height), Sense::hover());
    let shown = ui.ctx().animate_value_with_time(
        response.id.with("fill"),
        fraction,
        theme::anim_secs(ui.ctx(), 0.3),
    );
    let p = theme::palette(ui.ctx());
    let radius = (height * 0.5).min(4.0);
    ui.painter().rect_filled(rect, radius, p.bg_overlay);
    let fill_w = (rect.width() * shown.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.5 {
        let fill = Rect::from_min_size(rect.min, vec2(fill_w, rect.height()));
        ui.painter().rect_filled(fill, radius, color);
    }
    response
}

/// Every control, in the states a still frame can force. Drawn by the `widget_gallery` snapshot.
#[allow(dead_code)]
pub(crate) fn widget_gallery(ui: &mut Ui) {
    egui::Frame::new().inner_margin(20).show(ui, |ui| {
        section_heading(ui, "Icons");
        icon_strip(ui, 16.0);
        icon_strip(ui, 24.0);
        icon_strip(ui, 48.0);
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(400.0);
                gallery_buttons(ui);
            });
            ui.vertical(|ui| {
                ui.set_width(400.0);
                gallery_pills(ui);
            });
            ui.vertical(|ui| {
                ui.set_min_width(380.0);
                gallery_cards(ui);
            });
        });
    });
}

fn gallery_buttons(ui: &mut Ui) {
    section_heading(ui, "Buttons");
    ui.horizontal_wrapped(|ui| {
        button(ui, "Primary", Variant::Primary, Size::Medium);
        button(ui, "Secondary", Variant::Secondary, Size::Medium);
        button(ui, "Ghost", Variant::Ghost, Size::Medium);
        button(ui, "Danger", Variant::Danger, Size::Medium);
        button(ui, "Delete", Variant::DangerPrimary, Size::Medium);
        Button::new("Hovered")
            .variant(Variant::Secondary)
            .force_hover(true)
            .show(ui);
        Button::new("Pressed")
            .variant(Variant::Primary)
            .force_pressed(true)
            .show(ui);
        Button::new("Disabled")
            .variant(Variant::Primary)
            .enabled(false)
            .on_disabled_hover_text("Build the instance first")
            .show(ui);
    });
    ui.horizontal(|ui| {
        button(ui, "Small", Variant::Secondary, Size::Small);
        button(ui, "Medium", Variant::Secondary, Size::Medium);
        button(ui, "Large", Variant::Secondary, Size::Large);
    });
    Button::new("Play")
        .variant(Variant::Primary)
        .size(Size::Large)
        .icon(Icon::Play)
        .badge(3)
        .min_width(168.0)
        .show(ui);
    ui.horizontal(|ui| {
        icon_button(ui, Icon::Cross, Variant::Ghost, 32.0, "Close");
        icon_button(ui, Icon::Kebab, Variant::Secondary, 32.0, "More");
        icon_button(ui, Icon::Refresh, Variant::Ghost, 32.0, "Refresh");
        icon_button(ui, Icon::Plus, Variant::Primary, 32.0, "Add");
        spinner(ui, 20.0, theme::palette(ui.ctx()).arc);
    });

    section_heading(ui, "Inputs");
    let mut named = String::from("survival");
    let mut empty = String::new();
    text_input(ui, &mut named, "Display name", 280.0);
    search_input(ui, &mut empty, "Search instances", 280.0);
    ui.add_enabled_ui(false, |ui| {
        let mut locked = String::from("locked");
        text_input(ui, &mut locked, "Disabled", 280.0);
    });

    section_heading(ui, "Switch");
    ui.horizontal(|ui| {
        let mut on = true;
        let mut off = false;
        switch(ui, &mut on);
        switch(ui, &mut off);
        ui.add_enabled_ui(false, |ui| {
            let mut stuck = true;
            switch(ui, &mut stuck);
        });
    });
    let mut row_on = true;
    switch_row(
        ui,
        &mut row_on,
        "Animations",
        "Motion on buttons, tabs, and the forge",
    );

    section_heading(ui, "Progress");
    progress_bar(ui, 0.35, theme::VOLT, 6.0);
    progress_bar(ui, 0.72, theme::ARC, 8.0);
}

fn gallery_pills(ui: &mut Ui) {
    let p = theme::palette(ui.ctx());
    section_heading(ui, "Pills");
    ui.horizontal_wrapped(|ui| {
        pill(ui, "Ready", p.ok, theme::with_alpha(p.ok, 38));
        pill(ui, "Needs build", p.volt, p.volt_tint);
        pill(ui, "Broken", p.bad, theme::with_alpha(p.bad, 38));
        count_badge(ui, 4);
        count_badge(ui, 128);
        status_pill(ui, "Running", p.arc, true);
        status_pill(ui, "Failed", p.bad, false);
    });

    section_heading(ui, "Segmented");
    let mut segment = 1usize;
    segmented(
        ui,
        "gallery-segments",
        &mut segment,
        &[("Overview", None), ("Mods", Some(12)), ("Builds", Some(3))],
    );

    section_heading(ui, "Tabs");
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DemoTab {
        Overview,
        Mods,
        Log,
    }
    let mut tab = DemoTab::Mods;
    tabs(
        ui,
        &mut tab,
        &[
            (DemoTab::Overview, "Overview", None, false),
            (DemoTab::Mods, "Mods", Some(12), true),
            (DemoTab::Log, "Log", None, false),
        ],
    );

    section_heading(ui, "Swatches");
    ui.horizontal_wrapped(|ui| {
        for (i, color) in theme::INSTANCE_PALETTE.iter().enumerate() {
            swatch(ui, *color, i == 2);
        }
    });
}

fn gallery_cards(ui: &mut Ui) {
    let p = theme::palette(ui.ctx());
    section_heading(ui, "Cards");
    card(ui, |ui| {
        ui.label(
            egui::RichText::new("Survival")
                .font(theme::semibold(14.0))
                .color(p.text_bright),
        );
        ui.label(theme::muted("1.21.11 · 12 mods · release"));
    });
    card_with_bar(ui, p.volt, |ui| {
        ui.label(
            egui::RichText::new("Needs a build")
                .font(theme::semibold(14.0))
                .color(p.text_bright),
        );
        ui.label(theme::muted("The manifest changed since the last build."));
    });

    section_heading(ui, "Choices");
    choice_card(
        ui,
        true,
        "Essentials",
        "The usual mods for a survival world.",
        |ui| {
            ui.label(theme::muted("Profile: release"));
        },
    );
    choice_card(
        ui,
        false,
        "Vanilla",
        "No mods. The game, as shipped.",
        |_| {},
    );

    section_heading(ui, "Rows");
    egui::Frame::new()
        .fill(p.bg_sidebar)
        .corner_radius(10.0)
        .inner_margin(8)
        .show(ui, |ui| {
            row(ui, "gallery-row-on", 48.0, true, |ui, rect, _| {
                let bar = Rect::from_center_size(
                    pos2(rect.left() + 6.0, rect.center().y),
                    vec2(3.0, 28.0),
                );
                ui.painter().rect_filled(bar, 2.0, p.volt);
                ui.add_space(16.0);
                let (icon, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                cube(ui.painter(), icon, p.volt);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Survival")
                            .font(theme::semibold(14.0))
                            .color(p.text_bright),
                    );
                    ui.label(theme::muted("Ready"));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    icon_button(ui, Icon::Cross, Variant::Ghost, 28.0, "Hide");
                });
            });
            row(ui, "gallery-row-off", 48.0, false, |ui, _, _| {
                ui.add_space(16.0);
                let (icon, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                cube(ui.painter(), icon, p.arc);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Frontier")
                            .font(theme::semibold(14.0))
                            .color(p.text),
                    );
                    ui.label(theme::muted("Needs build"));
                });
            });
        });
}

fn icon_strip(ui: &mut Ui, px: f32) {
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(theme::muted(format!("{px:.0}")));
        for icon in Icon::ALL {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(px), Sense::hover());
            paint_icon(ui.painter(), rect, icon, p.text_bright, 0.18);
        }
    });
}

fn input_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, search: bool) -> Response {
    undo_disabled_fade(ui);
    let p = theme::palette(ui.ctx());
    let width = if width.is_finite() {
        width
    } else {
        ui.available_width()
    };
    let response = ui
        .scope(|ui| {
            ui.visuals_mut().weak_text_color = Some(p.text_faint);
            let id = ui.next_auto_id();
            ui.skip_ahead_auto_ids(1);
            let focused = ui.memory(|mem| mem.has_focus(id));
            let predicted = Rect::from_min_size(ui.cursor().min, vec2(width, 32.0));
            let hovered = ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pos| predicted.contains(pos));
            let enabled = ui.is_enabled();
            let stroke = if !enabled {
                p.line_soft
            } else if focused {
                p.arc
            } else if hovered {
                p.line_strong
            } else {
                p.line
            };
            let fill = if enabled { p.bg_canvas } else { p.bg_card };
            let fg = if enabled { p.text } else { p.text_faint };
            let left: i8 = if search { 28 } else { 10 };
            let frame = egui::Frame::new()
                .fill(fill)
                .stroke(Stroke::new(1.0, stroke))
                .corner_radius(6.0)
                .inner_margin(egui::Margin {
                    left,
                    right: 10,
                    top: 0,
                    bottom: 0,
                });
            ui.add(
                egui::TextEdit::singleline(text)
                    .id(id)
                    .hint_text(
                        egui::RichText::new(hint)
                            .font(theme::regular(14.0))
                            .color(p.text_faint),
                    )
                    .font(theme::regular(14.0))
                    .text_color(fg)
                    .frame(frame)
                    .min_size(vec2(width, 32.0))
                    .desired_width(width)
                    .align(Align2::LEFT_CENTER),
            )
        })
        .inner;
    if search {
        let icon = Rect::from_center_size(
            pos2(response.rect.left() + 16.0, response.rect.center().y),
            Vec2::splat(14.0),
        );
        let color = if response.enabled() {
            p.text_muted
        } else {
            p.text_faint
        };
        paint_icon(ui.painter(), icon, Icon::Search, color, 0.0);
    }
    response
}

#[derive(Clone, Copy)]
enum Face {
    Rest,
    Hover,
    Pressed,
    Disabled,
}

fn button_face(variant: Variant, face: Face, p: &Palette) -> (Color32, Color32, Color32) {
    let clear = Color32::TRANSPARENT;
    match (variant, face) {
        (Variant::Primary, Face::Rest) => (p.volt, clear, p.on_accent),
        (Variant::Primary, Face::Hover) => (p.volt_hover, clear, p.on_accent),
        (Variant::Primary, Face::Pressed) => (p.volt_pressed, clear, p.on_accent),
        (Variant::Primary, Face::Disabled) => (
            theme::with_alpha(p.volt, 89),
            clear,
            theme::with_alpha(p.on_accent, 153),
        ),
        (Variant::Secondary, Face::Rest) => (p.bg_raised, p.line, p.text),
        (Variant::Secondary, Face::Hover) => (p.bg_overlay, p.line_strong, p.text_bright),
        (Variant::Secondary, Face::Pressed) => (p.pressed_fill, p.line_strong, p.text_bright),
        (Variant::Secondary, Face::Disabled) => (p.bg_card, p.line_soft, p.text_faint),
        (Variant::Ghost, Face::Rest) => (clear, clear, p.text_muted),
        (Variant::Ghost, Face::Hover) => (p.ghost_hover, clear, p.text),
        (Variant::Ghost, Face::Pressed) => (p.ghost_pressed, clear, p.text),
        (Variant::Ghost, Face::Disabled) => (clear, clear, p.text_faint),
        (Variant::Danger, Face::Rest) => (clear, clear, p.bad),
        (Variant::Danger, Face::Hover) => (
            theme::with_alpha(p.bad, 38),
            theme::with_alpha(p.bad, 128),
            p.bad,
        ),
        (Variant::Danger, Face::Pressed) => (
            theme::with_alpha(p.bad, 64),
            theme::with_alpha(p.bad, 160),
            p.bad,
        ),
        (Variant::Danger, Face::Disabled) => (clear, clear, theme::with_alpha(p.bad, 102)),
        (Variant::DangerPrimary, Face::Rest) => (DANGER_FILL, clear, Color32::WHITE),
        (Variant::DangerPrimary, Face::Hover) => (DANGER_FILL_HOVER, clear, Color32::WHITE),
        (Variant::DangerPrimary, Face::Pressed) => (DANGER_FILL_PRESSED, clear, Color32::WHITE),
        (Variant::DangerPrimary, Face::Disabled) => (
            theme::with_alpha(DANGER_FILL, 89),
            clear,
            theme::with_alpha(Color32::WHITE, 153),
        ),
    }
}

fn paint_chrome(
    ui: &Ui,
    rect: Rect,
    response: &Response,
    variant: Variant,
    radius: u8,
    force_hover: bool,
    force_pressed: bool,
) -> Color32 {
    let p = theme::palette(ui.ctx());
    let enabled = response.enabled();
    let hovered = enabled && (force_hover || response.hovered());
    let pressed = enabled && (force_pressed || response.is_pointer_button_down_on());
    let hover_t = anim_flag(ui, response.id.with("hover"), hovered && !pressed);
    let press_t = anim_flag(ui, response.id.with("press"), pressed);
    let (fill, stroke, fg) = if !enabled {
        button_face(variant, Face::Disabled, &p)
    } else {
        let rest = button_face(variant, Face::Rest, &p);
        let hot = button_face(variant, Face::Hover, &p);
        let down = button_face(variant, Face::Pressed, &p);
        let mid = lerp_face(rest, hot, hover_t);
        lerp_face(mid, down, press_t)
    };
    let stroke = if stroke.a() == 0 {
        Stroke::NONE
    } else {
        Stroke::new(1.0, stroke)
    };
    if fill.a() > 0 || stroke.width > 0.0 {
        ui.painter()
            .rect(rect, radius, fill, stroke, StrokeKind::Inside);
    }
    if enabled && response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            radius.saturating_add(2),
            Stroke::new(2.0, p.arc),
            StrokeKind::Inside,
        );
    }
    fg
}

fn lerp_face(
    a: (Color32, Color32, Color32),
    b: (Color32, Color32, Color32),
    t: f32,
) -> (Color32, Color32, Color32) {
    (
        lerp_color(a.0, b.0, t),
        lerp_color(a.1, b.1, t),
        lerp_color(a.2, b.2, t),
    )
}

fn paint_radio(painter: &egui::Painter, rect: Rect, selected: bool, p: &Palette) {
    let center = rect.center();
    let radius = rect.width() * 0.5 - 1.0;
    if selected {
        painter.circle_stroke(center, radius, Stroke::new(1.5, p.volt));
        painter.circle_filled(center, radius * 0.48, p.volt);
    } else {
        painter.circle_stroke(center, radius, Stroke::new(1.5, p.line_strong));
    }
}

fn badge_width(ui: &Ui, count: usize) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        count.to_string(),
        theme::semibold(11.0),
        Color32::PLACEHOLDER,
    );
    (galley.size().x + 8.0).max(16.0)
}

fn paint_count_badge(painter: &egui::Painter, min: Pos2, count: usize, bg: Color32, fg: Color32) {
    let galley = painter.layout_no_wrap(count.to_string(), theme::semibold(11.0), fg);
    let width = (galley.size().x + 8.0).max(16.0);
    let rect = Rect::from_min_size(min, vec2(width, 16.0));
    painter.rect_filled(rect, 8.0, bg);
    painter.galley(
        pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
}

fn cube_points(rect: Rect) -> [Pos2; 7] {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let hw = s * 0.46;
    let hh = hw * 0.5;
    let face = hw * 0.92;
    let half = face * 0.5;
    [
        c + vec2(0.0, -(hh + half)),
        c + vec2(-hw, -half),
        c + vec2(hw, -half),
        c + vec2(0.0, hh - half),
        c + vec2(-hw, half),
        c + vec2(hw, half),
        c + vec2(0.0, hh + half),
    ]
}

fn paint_hollow_cube(painter: &egui::Painter, rect: Rect, color: Color32) {
    let [t, tl, tr, mid, bl, br, bot] = cube_points(rect);
    let s = rect.width().min(rect.height());
    let stroke = Stroke::new(icon_stroke(s), color);
    for edge in [
        [t, tl],
        [t, tr],
        [tr, mid],
        [mid, tl],
        [tr, br],
        [br, bot],
        [bot, mid],
        [tl, bl],
        [bl, bot],
    ] {
        painter.line_segment(edge, stroke);
    }
}

fn paint_stacked(painter: &egui::Painter, rect: Rect, color: Color32) {
    let s = rect.width().min(rect.height());
    let cube_s = s * 0.58;
    let step = s * 0.15;
    let offsets = [
        vec2(-step, -step * 0.75),
        vec2(0.0, 0.0),
        vec2(step * 0.7, step * 0.6),
    ];
    for offset in offsets {
        let r = Rect::from_center_size(rect.center() + offset, Vec2::splat(cube_s));
        cube(painter, r, color);
    }
}

fn paint_check(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let stroke = Stroke::new(icon_stroke(b.width()) * 1.15, color);
    painter.line(
        vec![at(b, 0.16, 0.52), at(b, 0.40, 0.78), at(b, 0.86, 0.22)],
        stroke,
    );
}

fn paint_cross(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let stroke = Stroke::new(icon_stroke(b.width()) * 1.1, color);
    painter.line_segment([at(b, 0.24, 0.24), at(b, 0.76, 0.76)], stroke);
    painter.line_segment([at(b, 0.76, 0.24), at(b, 0.24, 0.76)], stroke);
}

fn paint_chevron(painter: &egui::Painter, rect: Rect, color: Color32, down: bool) {
    let b = icon_box(rect);
    let stroke = Stroke::new(icon_stroke(b.width()) * 1.1, color);
    let points = if down {
        vec![at(b, 0.18, 0.36), at(b, 0.50, 0.68), at(b, 0.82, 0.36)]
    } else {
        vec![at(b, 0.36, 0.18), at(b, 0.68, 0.50), at(b, 0.36, 0.82)]
    };
    painter.line(points, stroke);
}

fn paint_plus(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let stroke = Stroke::new(icon_stroke(b.width()) * 1.15, color);
    painter.line_segment([at(b, 0.50, 0.18), at(b, 0.50, 0.82)], stroke);
    painter.line_segment([at(b, 0.18, 0.50), at(b, 0.82, 0.50)], stroke);
}

fn paint_kebab(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let r = (b.width() * 0.09).max(1.25);
    for y in [0.22, 0.50, 0.78] {
        painter.circle_filled(at(b, 0.50, y), r, color);
    }
}

fn paint_search(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let stroke = Stroke::new(icon_stroke(b.width()), color);
    let c = at(b, 0.40, 0.40);
    let radius = b.width() * 0.28;
    painter.circle_stroke(c, radius, stroke);
    let start = c + Vec2::angled(std::f32::consts::TAU * 0.125) * radius;
    painter.line_segment([start, at(b, 0.86, 0.86)], stroke);
}

fn paint_folder(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let tab = color.lerp_to_gamma(Color32::WHITE, 0.22);
    painter.add(Shape::convex_polygon(
        vec![
            at(b, 0.10, 0.40),
            at(b, 0.10, 0.22),
            at(b, 0.46, 0.22),
            at(b, 0.56, 0.40),
        ],
        tab,
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        vec![
            at(b, 0.08, 0.38),
            at(b, 0.92, 0.38),
            at(b, 0.92, 0.84),
            at(b, 0.08, 0.84),
        ],
        color,
        Stroke::NONE,
    ));
}

fn paint_refresh(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    let stroke_w = icon_stroke(b.width());
    let center = b.center();
    let radius = b.width() * 0.36;
    let start = 0.55;
    let sweep = std::f32::consts::TAU * 0.72;
    paint_arc(painter, center, radius, stroke_w, start, sweep, color);
    let end = start + sweep;
    let tip = center + Vec2::angled(end) * radius;
    let dir = Vec2::angled(end + std::f32::consts::TAU * 0.25);
    let radial = Vec2::angled(end);
    let wing = radius * 0.55;
    painter.add(Shape::convex_polygon(
        vec![
            tip + dir * (wing * 0.15),
            tip - dir * wing + radial * wing * 0.55,
            tip - dir * wing - radial * wing * 0.55,
        ],
        color,
        Stroke::NONE,
    ));
}

fn paint_play(painter: &egui::Painter, rect: Rect, color: Color32) {
    let b = icon_box(rect);
    painter.add(Shape::convex_polygon(
        vec![at(b, 0.30, 0.16), at(b, 0.30, 0.84), at(b, 0.84, 0.50)],
        color,
        Stroke::NONE,
    ));
}

fn paint_spinner_arc(painter: &egui::Painter, rect: Rect, color: Color32, time: f32) {
    let b = icon_box(rect);
    let stroke_w = icon_stroke(b.width());
    let start = time * std::f32::consts::TAU;
    paint_arc(
        painter,
        b.center(),
        b.width() * 0.36,
        stroke_w,
        start,
        std::f32::consts::TAU * 0.72,
        color,
    );
}

fn paint_arc(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    width: f32,
    start: f32,
    sweep: f32,
    color: Color32,
) {
    let steps = 28;
    let mut points = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        points.push(center + Vec2::angled(start + sweep * t) * radius);
    }
    painter.add(Shape::line(points, Stroke::new(width, color)));
    let cap = width * 0.5;
    painter.circle_filled(center + Vec2::angled(start) * radius, cap, color);
    painter.circle_filled(center + Vec2::angled(start + sweep) * radius, cap, color);
}

fn icon_box(rect: Rect) -> Rect {
    let s = rect.width().min(rect.height());
    let pad = (s * 0.12).min(8.0);
    Rect::from_center_size(rect.center(), Vec2::splat((s - pad * 2.0).max(1.0)))
}

fn at(rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(
        rect.min.x + x * rect.width(),
        rect.min.y + y * rect.height(),
    )
}

fn icon_stroke(size: f32) -> f32 {
    (size * 0.1).clamp(1.25, 2.6)
}

fn undo_disabled_fade(ui: &mut Ui) {
    if ui.is_enabled() {
        return;
    }
    let alpha = ui.visuals().disabled_alpha();
    if alpha > 0.001 {
        let full = (ui.painter().opacity() / alpha).min(1.0);
        ui.set_opacity(full);
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    if t <= 0.0 {
        a
    } else if t >= 1.0 {
        b
    } else {
        a.lerp_to_gamma(b, t)
    }
}

fn anim_flag(ui: &Ui, id: egui::Id, target: bool) -> f32 {
    ui.ctx()
        .animate_bool_with_time(id, target, theme::anim_secs(ui.ctx(), 0.12))
}

fn hand(response: Response) -> Response {
    if response.enabled() && response.hovered() {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}
