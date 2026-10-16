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

fn shot(name: &str, mut app: gui::App) {
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui(|ui| app.ui(ui));
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

#[test]
fn running_build() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival-classic");
    app.start_op_for_tests(
        "survival-classic",
        watt_core::ops::OpRequest::Build {
            instance: "survival-classic".to_owned(),
            force: false,
            profile: None,
        },
    );
    shot("running_build", app);
}
