//! The launcher window (egui on wgpu).

mod app;
pub(crate) mod backend;
pub mod forge;
pub(crate) mod forge_bridge;
mod theme;

pub use app::App;

/// Open the window and run until it closes.
pub fn run() -> anyhow::Result<()> {
    let app = App::new()?;
    let icon = forge::app_icon_rgba(64);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("watt-start")
            .with_inner_size(egui::vec2(1280.0, 800.0))
            .with_min_inner_size(egui::vec2(900.0, 600.0))
            .with_icon(egui::IconData {
                rgba: icon,
                width: 64,
                height: 64,
            }),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "watt-start",
        options,
        Box::new(|_cc| Ok(Box::new(EframeApp { app }))),
    )
    .map_err(|e| anyhow::anyhow!("cannot start the launcher window: {e}"))
}

/// Adapts [`App`] (one `ui(&mut self, ui)` method, so it can be driven from tests too) to
/// [`eframe::App`].
struct EframeApp {
    app: App,
}

impl eframe::App for EframeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }
}
