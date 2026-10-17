//! The launcher window (egui on wgpu).

mod app;
pub(crate) mod backend;
pub mod forge;
pub(crate) mod forge_bridge;
pub(crate) mod theme;
pub(crate) mod widgets;

pub use app::App;

/// Open the window and run until it closes.
pub fn run() -> anyhow::Result<()> {
    let app = App::new()?;
    let icon = forge::app_icon_rgba(64);
    let mut options = eframe::NativeOptions {
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
    configure_wgpu(&mut options);
    eframe::run_native(
        "watt-start",
        options,
        Box::new(|_cc| Ok(Box::new(EframeApp { app }))),
    )
    .map_err(|e| anyhow::anyhow!("cannot start the launcher window: {e}"))
}

/// Vulkan on Linux. DirectX 12, then Vulkan, on Windows: the launcher window does not need the
/// Vulkan driver the game does (the doctor checks that). `WGPU_BACKEND` is not consulted, so the
/// choice stays the same on every machine. wgpu lists Vulkan before Dx12, so the selector asks
/// for a Dx12 adapter first.
fn configure_wgpu(options: &mut eframe::NativeOptions) {
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
    else {
        return;
    };
    #[cfg(windows)]
    {
        setup.instance_descriptor.backends =
            eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::VULKAN;
        setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters, _surface| {
            adapters
                .iter()
                .find(|adapter| adapter.get_info().backend == eframe::wgpu::Backend::Dx12)
                .or_else(|| adapters.first())
                .cloned()
                .ok_or_else(|| "no graphics adapter".to_owned())
        }));
    }
    #[cfg(target_os = "linux")]
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::VULKAN;
    }
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
