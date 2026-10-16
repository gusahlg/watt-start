//! Small transient notifications, bottom-right.

use std::time::{Duration, Instant};

use super::App;

/// How long a toast stays up.
const LIFETIME: Duration = Duration::from_secs(5);

/// One toast.
pub struct Toast {
    pub message: String,
    pub created: Instant,
}

impl App {
    pub(crate) fn draw_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.created.elapsed() < LIFETIME);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for toast in self.toasts.iter().rev() {
                        egui::Frame::popup(ui.style())
                            .fill(crate::gui::theme::BG_3)
                            .corner_radius(10u8)
                            .show(ui, |ui| {
                                ui.set_max_width(320.0);
                                ui.label(&toast.message);
                            });
                    }
                });
            });
    }
}
