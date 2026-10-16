//! A thin indirection over [`crate::gui::forge`]'s two drawing entry points, as function
//! pointers. [`App`](crate::gui::app::App) calls `(self.forge.cube_icon)(...)` and
//! `(self.forge.forge)(...)` rather than `forge::cube_icon`/`forge::forge` directly, so a
//! different implementation can be swapped in later without touching every call site.

use watt_core::view::OpView;

use super::forge::{ForgeResponse, ForgeStyle};

/// The two forge entry points used outside `forge.rs` itself.
pub(crate) struct ForgeBridge {
    pub cube_icon: fn(&mut egui::Ui, f32, egui::Color32, bool) -> egui::Response,
    pub forge: fn(&mut egui::Ui, &OpView, &ForgeStyle) -> ForgeResponse,
}

impl ForgeBridge {
    /// The real `forge.rs` functions: what every build of `watt-start` uses, screenshots
    /// included.
    pub fn real() -> Self {
        Self {
            cube_icon: super::forge::cube_icon,
            forge: super::forge::forge,
        }
    }
}
