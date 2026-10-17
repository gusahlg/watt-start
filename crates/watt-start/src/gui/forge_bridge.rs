//! A thin indirection over [`crate::gui::forge`]'s drawing entry point, as a function pointer.
//! [`App`](crate::gui::app::App) calls `(self.forge.forge)(...)` rather than `forge::forge`
//! directly, so a different implementation can be swapped in later without touching the call
//! site.

use watt_core::view::OpView;

use super::forge::{ForgeResponse, ForgeStyle};

/// The forge entry point used outside `forge.rs` itself.
pub(crate) struct ForgeBridge {
    pub forge: fn(&mut egui::Ui, &OpView, &ForgeStyle) -> ForgeResponse,
}

impl ForgeBridge {
    /// The real `forge.rs` function: what every build of `watt-start` uses, screenshots
    /// included.
    pub fn real() -> Self {
        Self {
            forge: super::forge::forge,
        }
    }
}
