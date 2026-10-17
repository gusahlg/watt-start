//! Headless screenshots of the GUI, rendered with the demo backend (`egui_kittest` + `wgpu`).
//!
//! `watt-start` is a binary crate, so this test includes the `gui` module tree directly rather
//! than linking against a library; the module stays self-contained (only `watt_core::` and
//! `crate::gui::` paths), so including it this way just works.

// This test compiles the whole `gui` module tree but drives only the demo backend, so the
// real backend and the window entry point are unused here (they are used by the binary).
#![allow(dead_code)]

#[path = "../src/gui/mod.rs"]
mod gui;

use egui_kittest::Harness;

const SIZE: egui::Vec2 = egui::vec2(1280.0, 800.0);

/// The harness wraps the UI in a frame with an 8px outer margin; eframe's root `Ui` has none.
/// Draw over the whole window, like the real app.
fn full_bleed(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) {
    let full = ui.ctx().content_rect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(full));
    child.set_clip_rect(full);
    f(&mut child);
}

fn shot(name: &str, mut app: gui::App) {
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui(|ui| full_bleed(ui, |ui| app.ui(ui)));
    harness.run_steps(3);
    let image = harness.render().expect("render");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("gui-shots");
    std::fs::create_dir_all(&dir).expect("create target/gui-shots");
    image
        .save(dir.join(format!("{name}.png")))
        .expect("save png");
}

#[test]
fn instance_overview() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival");
    shot("instance_overview", app);
}

#[test]
fn instance_overview_updates_available() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("frontier");
    shot("instance_overview_updates", app);
}

#[test]
fn mods_tab_with_staged_changes() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.stage_mod_add_for_tests("survival", "pwc.better-caves");
    app.stage_mod_remove_for_tests("survival", "pwc.waypoints");
    shot("mods_staged", app);
}

#[test]
fn builds_tab() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_builds("survival");
    shot("builds", app);
}

#[test]
fn broken_instance() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("flaky-build");
    shot("broken_instance", app);
}

#[test]
fn settings_page() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_settings();
    shot("settings", app);
}

#[test]
fn new_instance_dialog() {
    let mut app = gui::App::for_snapshot_tests();
    app.open_new_instance_dialog();
    shot("new_instance_dialog", app);
}

#[test]
fn welcome_screen() {
    let app = gui::App::for_snapshot_tests_empty();
    shot("welcome", app);
}

/// The demo build timeline (see `gui::backend::demo::build_timeline`) is driven by real elapsed
/// time, so these sleep past the point they want to land on before taking the shot.
fn build_request(instance: &str) -> watt_core::ops::OpRequest {
    watt_core::ops::OpRequest::Build {
        instance: instance.to_owned(),
        force: false,
        profile: None,
    }
}

#[test]
fn running_build_compile() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival-classic");
    app.start_op_for_tests("survival-classic", build_request("survival-classic"));
    // Just past the "wgpu" progress tick (t=4900ms): overall fraction ~43%, the closest the
    // timeline's discrete ticks land to "~45%" mid-Compile.
    std::thread::sleep(std::time::Duration::from_millis(5100));
    shot("running_build_compile", app);
}

#[test]
fn running_build_link() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival-classic");
    app.start_op_for_tests("survival-classic", build_request("survival-classic"));
    // Compile ends at t=11200ms; comfortably into Link before Install starts at t=15700ms.
    std::thread::sleep(std::time::Duration::from_millis(11500));
    shot("running_build_link", app);
}

#[test]
fn running_build_failed() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("flaky-build");
    // `flaky-build` is the demo backend's dedicated failure instance (see `DemoBackend::start_op`).
    app.start_op_for_tests("flaky-build", build_request("flaky-build"));
    std::thread::sleep(std::time::Duration::from_millis(2300));
    shot("running_build_failed", app);
}

/// A real-backend screenshot against an actual pwc home: set `WATT_START_SHOT_HOME` to a pwc
/// data/config root (`PWC_HOME`) with at least one instance to see it rendered for real; unset,
/// this is a no-op (skipped, not failed, so it is safe in every other environment).
#[test]
fn real_backend() {
    let Ok(home) = std::env::var("WATT_START_SHOT_HOME") else {
        return;
    };
    // SAFETY: single-threaded at this point (first thing the test does, no other thread of this
    // process reads/writes the environment concurrently).
    unsafe {
        std::env::set_var("PWC_HOME", &home);
    }
    let app = gui::App::new().expect("RealBackend::new against WATT_START_SHOT_HOME");

    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui_state(
            |ui, app: &mut gui::App| full_bleed(ui, |ui| app.ui(ui)),
            app,
        );

    // The catalog/status scan (and the doctor/startup-check ops) run on background threads;
    // poll with a timeout rather than a fixed sleep, so this finishes as soon as the catalog
    // shows at least one instance and still bails out if the real backend never responds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        harness.step();
        if !harness.state().backend.instances().is_empty() || std::time::Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    harness.run_steps(3);

    let image = harness.render().expect("render");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("gui-shots");
    std::fs::create_dir_all(&dir).expect("create target/gui-shots");
    image.save(dir.join("real_backend.png")).expect("save png");
}

#[test]
fn widget_gallery() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_accessibility_check(false)
        .wgpu()
        .build_ui(|ui| {
            gui::theme::apply(ui.ctx(), true, false);
            ui.painter()
                .rect_filled(ui.clip_rect(), 0.0, gui::theme::BG_SURFACE);
            gui::widgets::widget_gallery(ui);
        });
    harness.run_steps(3);
    let image = harness.render().expect("render");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("gui-shots");
    std::fs::create_dir_all(&dir).expect("create target/gui-shots");
    image
        .save(dir.join("widget_gallery.png"))
        .expect("save png");
}

// ---- dialogs ----

fn shot_sized(name: &str, mut app: gui::App, size: egui::Vec2) {
    let mut harness = Harness::builder()
        .with_size(size)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui(|ui| full_bleed(ui, |ui| app.ui(ui)));
    harness.run_steps(3);
    let image = harness.render().expect("render");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("gui-shots");
    std::fs::create_dir_all(&dir).expect("create target/gui-shots");
    image
        .save(dir.join(format!("{name}.png")))
        .expect("save png");
}

#[test]
fn new_instance_dialog_linked() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_new_instance_linked_for_tests("/home/player/repos/project_watt_cubed");
    shot("new_instance_dialog_linked", app);
}

#[test]
fn change_source_dialog() {
    let mut app = gui::App::for_snapshot_tests();
    app.open_change_source_dialog("survival");
    shot("change_source_dialog", app);
}

#[test]
fn duplicate_dialog() {
    let mut app = gui::App::for_snapshot_tests();
    app.open_duplicate_dialog("survival");
    shot("duplicate_dialog", app);
}

#[test]
fn delete_confirm_dialog() {
    let mut app = gui::App::for_snapshot_tests();
    app.ask_delete_instance("survival");
    shot("delete_confirm_dialog", app);
}

#[test]
fn toasts() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_toasts_for_tests();
    shot("toasts", app);
}

#[test]
fn settings_page_small() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_settings();
    shot_sized("settings_page_small", app, egui::vec2(900.0, 600.0));
}
