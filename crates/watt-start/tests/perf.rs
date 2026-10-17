//! Frame cost of the launcher shell, on the CPU kittest harness (no wgpu).
//!
//! `watt-start` is a binary crate, so this test includes the `gui` module tree directly.
//! Timing assertions are `#[ignore]` and print µs/frame and allocations/frame:
//!
//! ```text
//! cargo test --release -p watt-start --test perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The repaint-policy tests run in the normal suite.

#![allow(dead_code)]

#[path = "../src/gui/mod.rs"]
mod gui;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use egui_kittest::Harness;

const SIZE: egui::Vec2 = egui::vec2(1280.0, 800.0);
const WARMUP: usize = 10;
const FRAMES: usize = 200;

/// Counts `alloc` and `realloc`. `dealloc` is free.
struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The harness wraps the UI in a frame with an 8px outer margin; eframe's root `Ui` has none.
fn full_bleed(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) {
    let full = ui.ctx().content_rect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(full));
    child.set_clip_rect(full);
    f(&mut child);
}

fn stress_app() -> gui::App {
    gui::App::with_backend(Box::new(gui::backend::DemoBackend::stress(40, 400, 2000)))
}

fn harness(app: gui::App) -> Harness<'static, gui::App> {
    Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, app: &mut gui::App| full_bleed(ui, |ui| app.ui(ui)),
            app,
        )
}

fn build_request(instance: &str) -> watt_core::ops::OpRequest {
    watt_core::ops::OpRequest::Build {
        instance: instance.to_owned(),
        force: false,
        profile: None,
    }
}

/// Warm up, then time `FRAMES` steps. Ceilings are loose; the printed line is the measurement.
fn measure(label: &str, harness: &mut Harness<'static, gui::App>) {
    harness.run_steps(WARMUP);
    ALLOCS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    harness.run_steps(FRAMES);
    let elapsed = start.elapsed();
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let us = elapsed.as_secs_f64() * 1_000_000.0 / FRAMES as f64;
    let per = allocs as f64 / FRAMES as f64;
    println!("{label}: {us:.1} µs/frame, {per:.1} allocs/frame");
    assert!(
        us < 500_000.0,
        "{label} took {us:.1} µs/frame (ceiling 500 ms)"
    );
    assert!(
        per < 5_000_000.0,
        "{label} allocated {per:.1} times/frame (ceiling 5e6)"
    );
}

fn repaint_delay(harness: &Harness<gui::App>) -> Duration {
    harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.repaint_delay)
        .unwrap_or(Duration::MAX)
}

/// Open the Forge log for the running op. The disclosure lives in egui temp data, and the
/// custom button does not expose a reliable accesskit label.
fn open_forge_log(harness: &mut Harness<'static, gui::App>) {
    let op_id = harness
        .state()
        .ops
        .get("s00")
        .map(|slot| slot.view.op_id.clone())
        .expect("stress op id");
    let id = egui::Id::unique("watt-forge-log").with(&op_id);
    harness.ctx.data_mut(|data| data.insert_temp(id, true));
}

#[test]
#[ignore = "release frame-cost measurement; run with --ignored --nocapture"]
fn frame_cost() {
    // Overview paints the 40-row sidebar as well. The sidebar scenario is the same window,
    // measured on its own so the two numbers can be compared.
    {
        let mut app = stress_app();
        app.show_instance_overview("s00");
        let mut harness = harness(app);
        measure("overview", &mut harness);
    }
    {
        let mut app = stress_app();
        app.show_instance_mods("s00");
        app.mods_show_browse_for_tests("s00");
        let mut harness = harness(app);
        measure("mods-browse", &mut harness);
    }
    {
        let mut app = stress_app();
        app.show_instance_mods("s00");
        app.mods_show_browse_for_tests("s00");
        app.mods_set_search_for_tests("s00", "stress");
        let mut harness = harness(app);
        measure("mods-search", &mut harness);
    }
    {
        let mut app = stress_app();
        app.start_op_for_tests("s00", build_request("s00"));
        // A running op shows the Forge until it is hidden. Hide it, then open the Log tab.
        // The first harness frame polls the t=0 log lines into the slot before painting.
        app.dismiss_op("s00");
        app.show_instance_log("s00");
        let mut harness = harness(app);
        measure("log-tab", &mut harness);
    }
    {
        let mut app = stress_app();
        app.show_instance_overview("s00");
        app.start_op_for_tests("s00", build_request("s00"));
        let mut harness = harness(app);
        open_forge_log(&mut harness);
        measure("forge-log", &mut harness);
    }
    {
        let mut app = stress_app();
        app.show_instance_overview("s00");
        let mut harness = harness(app);
        measure("sidebar-40", &mut harness);
    }
}

#[test]
fn repaint_idle_overview_settles() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival");
    // The startup-check spinner asks for another frame. This test is the idle overview.
    app.startup_check = None;
    let mut harness = harness(app);
    harness.try_run().expect("idle overview settles");
}

#[test]
fn repaint_running_op_is_coalesced() {
    // Animations and the startup check are off (see `DemoBackend::stress`), so the only
    // repaint request left is the running op. Every event is already due at t=0; after the
    // first frame, polls apply nothing and must not ask for an immediate repaint.
    let mut app = gui::App::with_backend(Box::new(gui::backend::DemoBackend::stress(1, 0, 1)));
    app.show_instance_overview("s00");
    app.start_op_for_tests("s00", build_request("s00"));
    let mut harness = harness(app);
    harness.run_steps(3);
    let delay = repaint_delay(&harness);
    assert!(
        delay >= Duration::from_millis(100),
        "running op repaint delay {delay:?} is under 100 ms"
    );
}
