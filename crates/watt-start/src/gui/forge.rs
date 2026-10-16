//! The Forge: the animated build/update display, and the small cube icons and spinners used
//! around the launcher.
//!
//! Everything is drawn with egui's painter (no textures, no assets). Animations follow
//! `ui.ctx()` time and request repaints only while something moves.

use watt_core::view::OpView;

/// Look of the forge.
#[derive(Clone, Debug)]
pub struct ForgeStyle {
    /// The instance's accent colour.
    pub accent: egui::Color32,
    /// Animations on (`settings.appearance.animations`); off = a static cube and plain bars.
    pub animate: bool,
    /// How long the previous comparable build took, for the estimate ("last build took 12 min").
    pub expected_ms: Option<u64>,
    /// The instance's shown name.
    pub title: String,
}

/// What the user pressed in the forge this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForgeResponse {
    /// "Cancel" (only offered while running).
    pub cancel: bool,
    /// "Hide" (go back to the instance page while the op keeps running).
    pub hide: bool,
    /// "Done"/"Close" after the op finished.
    pub close: bool,
    /// A recovery button after a failure.
    pub recovery: Option<watt_core::events::Recovery>,
    /// "Play" after a successful build/update.
    pub play: bool,
}

/// Draw the full forge for an op into the available space.
pub fn forge(ui: &mut egui::Ui, view: &OpView, style: &ForgeStyle) -> ForgeResponse {
    let _ = (ui, view, style);
    todo!("worker D")
}

/// A small isometric cube icon (instance avatars, the logo). `busy` makes it spin and crackle.
pub fn cube_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32, busy: bool) -> egui::Response {
    let _ = (ui, size, color, busy);
    todo!("worker D")
}

/// The window icon: a cube in the launcher colours, as RGBA pixels (`size` × `size`).
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let _ = size;
    todo!("worker D")
}
