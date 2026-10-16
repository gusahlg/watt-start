//! Indirection over [`crate::gui::forge`]'s three entry points.
//!
//! `forge.rs` is implemented on a sibling branch (`w/forge`) in parallel with this one and is
//! `todo!()` here, so [`App`](crate::gui::app::App) never calls it directly: it goes through a
//! [`ForgeBridge`], which defaults to the real functions ([`ForgeBridge::real`]) but can be
//! swapped for [`ForgeBridge::placeholder`] (used by the snapshot tests) so a render never hits
//! an unimplemented panic before `forge.rs` lands.

use watt_core::view::OpView;

use super::forge::{ForgeResponse, ForgeStyle};

/// The three forge entry points, as function pointers so a test can swap them out.
pub(crate) struct ForgeBridge {
    pub cube_icon: fn(&mut egui::Ui, f32, egui::Color32, bool) -> egui::Response,
    pub forge: fn(&mut egui::Ui, &OpView, &ForgeStyle) -> ForgeResponse,
}

impl ForgeBridge {
    /// The real `forge.rs` functions (what `watt-start` ships with once `w/forge` lands).
    pub fn real() -> Self {
        Self {
            cube_icon: super::forge::cube_icon,
            forge: super::forge::forge,
        }
    }

    /// A plain stand-in that never calls the `todo!()` implementation: a flat rounded square for
    /// the cube, and a simple phase list with a progress bar for the forge view. Used by the
    /// snapshot tests until `forge.rs` is implemented.
    pub fn placeholder() -> Self {
        Self {
            cube_icon: placeholder_cube_icon,
            forge: placeholder_forge,
        }
    }
}

fn placeholder_cube_icon(
    ui: &mut egui::Ui,
    size: f32,
    color: egui::Color32,
    busy: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, (size * 0.22) as u8, color.linear_multiply(0.85));
    if busy {
        painter.rect_stroke(
            rect.shrink(1.5),
            (size * 0.22) as u8,
            egui::Stroke::new(1.5, egui::Color32::WHITE.linear_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
    response
}

fn placeholder_forge(ui: &mut egui::Ui, view: &OpView, style: &ForgeStyle) -> ForgeResponse {
    let mut response = ForgeResponse::default();
    ui.vertical(|ui| {
        ui.heading(&style.title);
        if let Some(op) = view.op {
            ui.label(format!("{} …", op.verb()));
        }
        for phase in &view.phases {
            let state = match phase.state {
                watt_core::view::PhaseState::Pending => "pending".to_owned(),
                watt_core::view::PhaseState::Running => "running".to_owned(),
                watt_core::view::PhaseState::Done(outcome) => format!("{outcome:?}").to_lowercase(),
            };
            ui.label(format!("{}: {state}", phase.title));
        }
        ui.add(egui::ProgressBar::new(view.fraction()).show_percentage());
        ui.horizontal(|ui| {
            if view.running() {
                if ui.button("Cancel").clicked() {
                    response.cancel = true;
                }
                if ui.button("Hide").clicked() {
                    response.hide = true;
                }
            } else {
                if ui.button("Close").clicked() {
                    response.close = true;
                }
                if matches!(view.outcome, Some(watt_core::events::OpOutcome::Ok { .. }))
                    && ui.button("Play").clicked()
                {
                    response.play = true;
                }
            }
        });
    });
    response
}
