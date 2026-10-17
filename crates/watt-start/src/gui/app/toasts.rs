//! Small transient notifications, stacked from the bottom-right corner.
//!
//! Motion state lives in egui temp data (one [`App`] field is already the duplicate dialog).
//! A toast slides 16px and fades in over 180ms, stays for 6s unless the pointer is over it,
//! then fades out over 250ms. Animations off skips the slide and the fade. Repaints are
//! requested only while something is moving, plus one wake-up when a visible toast expires.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Id, Order, Rect, pos2, vec2};

use super::App;
use crate::gui::theme::{self, Palette};
use crate::gui::widgets::{self, Icon, Variant};

/// How long a toast stays up, not counting time the pointer spends over it.
const LIFETIME: Duration = Duration::from_secs(6);
/// Slide distance, in points, from below the resting position.
const SLIDE: f32 = 16.0;
/// Fade and slide in.
const INTRO: f32 = 0.18;
/// Fade and slide out.
const OUTRO: f32 = 0.25;
/// Oldest toasts past this are dropped.
const MAX_VISIBLE: usize = 4;
const WIDTH: f32 = 320.0;
const MARGIN: f32 = 20.0;
const GAP: f32 = 8.0;

/// What a toast reports: its colour bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral information.
    Info,
    /// Something finished.
    Ok,
    /// Something failed.
    Error,
}

/// One toast.
pub struct Toast {
    pub kind: ToastKind,
    pub message: String,
    pub created: Instant,
}

/// Per-toast clock. Keyed by [`Toast::created`] in egui temp data.
#[derive(Clone, Default)]
struct ToastTrack {
    /// Time the pointer has already spent over the toast.
    paused: Duration,
    /// When the current hover started.
    hover_since: Option<Instant>,
    /// When the toast started leaving. None while it is still up.
    leaving_since: Option<Instant>,
}

fn track_id() -> Id {
    Id::unique("watt-toast-tracks")
}

fn bar_color(kind: ToastKind, palette: &Palette) -> Color32 {
    match kind {
        ToastKind::Ok => palette.ok,
        ToastKind::Error => palette.bad,
        ToastKind::Info => palette.arc,
    }
}

fn shown_amount(now: Instant, created: Instant, leaving: Option<Instant>, animate: bool) -> f32 {
    if !animate {
        return if leaving.is_some() { 0.0 } else { 1.0 };
    }
    let intro = (now.saturating_duration_since(created).as_secs_f32() / INTRO).clamp(0.0, 1.0);
    let outro = leaving
        .map(|since| (now.saturating_duration_since(since).as_secs_f32() / OUTRO).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    intro * (1.0 - outro)
}

impl App {
    /// Snapshot helper: one toast of each kind, newest (error) closest to the corner.
    #[allow(dead_code)]
    pub(crate) fn show_toasts_for_tests(&mut self) {
        self.toast_kind(ToastKind::Info, "Checking for updates");
        self.toast_kind(ToastKind::Ok, "Survival: build finished");
        self.toast_kind(ToastKind::Error, "Frontier: update failed");
    }

    pub(crate) fn draw_toasts(&mut self, ctx: &egui::Context) {
        if self.toasts.len() > MAX_VISIBLE {
            let drop_n = self.toasts.len() - MAX_VISIBLE;
            self.toasts.drain(0..drop_n);
        }
        if self.toasts.is_empty() {
            ctx.data_mut(|data| data.remove::<HashMap<Instant, ToastTrack>>(track_id()));
            return;
        }

        let animate = theme::anim_secs(ctx, 1.0) > 0.0;
        let now = Instant::now();
        let mut tracks: HashMap<Instant, ToastTrack> =
            ctx.data(|data| data.get_temp(track_id()).unwrap_or_default());
        tracks.retain(|created, _| self.toasts.iter().any(|t| t.created == *created));

        let width = WIDTH.min((ctx.content_rect().width() - MARGIN * 2.0).max(160.0));
        let mut y = MARGIN;
        let mut remove = Vec::new();
        let mut repaint_now = false;
        let mut repaint_after: Option<Duration> = None;

        for toast in self.toasts.iter().rev() {
            let mut track = tracks.remove(&toast.created).unwrap_or_default();
            let hover_extra = track
                .hover_since
                .map(|since| now.saturating_duration_since(since))
                .unwrap_or_default();
            let age = now
                .saturating_duration_since(toast.created)
                .saturating_sub(track.paused)
                .saturating_sub(hover_extra);
            if track.leaving_since.is_none() && age >= LIFETIME {
                track.leaving_since = Some(now);
            }

            let shown = shown_amount(now, toast.created, track.leaving_since, animate);
            if !animate && track.leaving_since.is_some() {
                remove.push(toast.created);
                continue;
            }
            if animate && shown <= 0.0 && track.leaving_since.is_some() {
                remove.push(toast.created);
                continue;
            }

            let slide = (1.0 - shown) * SLIDE;
            let offset = vec2(-MARGIN, -(y - slide));
            let mut dismissed = false;
            let response = egui::Area::new(Id::unique(("watt-toast", toast.created)))
                .anchor(Align2::RIGHT_BOTTOM, offset)
                .order(Order::Foreground)
                .interactable(true)
                .movable(false)
                .fade_in(false)
                .show(ctx, |ui| {
                    ui.set_opacity(shown.clamp(0.0, 1.0));
                    dismissed = paint_toast(ui, toast, width);
                });

            let hovered = ctx
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pos| response.response.rect.contains(pos));
            if hovered {
                track.hover_since.get_or_insert(now);
            } else if let Some(since) = track.hover_since.take() {
                track.paused += now.saturating_duration_since(since);
            }
            if dismissed && track.leaving_since.is_none() {
                track.leaving_since = Some(now);
                if !animate {
                    remove.push(toast.created);
                    y += response.response.rect.height() + GAP;
                    continue;
                }
            }

            let intro = if animate {
                (now.saturating_duration_since(toast.created).as_secs_f32() / INTRO).min(1.0)
            } else {
                1.0
            };
            let leaving = track.leaving_since.is_some();
            if animate && (intro < 1.0 || leaving) {
                repaint_now = true;
            } else if !leaving && track.hover_since.is_none() {
                let remain = LIFETIME.saturating_sub(age);
                repaint_after = Some(match repaint_after {
                    Some(sooner) => sooner.min(remain),
                    None => remain,
                });
            }

            y += response.response.rect.height() + GAP;
            tracks.insert(toast.created, track);
        }

        ctx.data_mut(|data| data.insert_temp(track_id(), tracks));
        self.toasts.retain(|t| !remove.contains(&t.created));
        if repaint_now {
            ctx.request_repaint();
        } else if let Some(after) = repaint_after {
            if !after.is_zero() {
                ctx.request_repaint_after(after);
            } else {
                ctx.request_repaint();
            }
        }
    }
}

/// Paint one toast. Returns whether the dismiss button was clicked.
fn paint_toast(ui: &mut egui::Ui, toast: &Toast, width: f32) -> bool {
    let p = theme::palette(ui.ctx());
    let color = bar_color(toast.kind, &p);
    let inner = egui::Frame::new()
        .fill(p.bg_raised)
        .stroke(egui::Stroke::new(1.0, p.line))
        .corner_radius(12.0)
        .inner_margin(egui::Margin {
            left: 16,
            right: 8,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_min_width(width - 24.0);
            ui.set_max_width(width - 24.0);
            let text_w = (width - 24.0 - 8.0 - 28.0).max(40.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.vertical(|ui| {
                    ui.set_width(text_w);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&toast.message)
                                .font(theme::regular(13.0))
                                .color(p.text),
                        )
                        .wrap(),
                    );
                });
                widgets::icon_button(ui, Icon::Cross, Variant::Ghost, 28.0, "Dismiss").clicked()
            })
            .inner
        });
    let rect = inner.response.rect;
    let bar = Rect::from_min_max(
        pos2(rect.left() + 1.0, rect.top() + 10.0),
        pos2(rect.left() + 4.0, rect.bottom() - 10.0),
    );
    ui.painter().rect_filled(bar, 1.5, color);
    inner.inner
}
