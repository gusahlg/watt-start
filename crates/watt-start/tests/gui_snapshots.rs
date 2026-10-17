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
use egui_kittest::kittest::Queryable;

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

// ---- shell ----

fn shot_at(name: &str, size: egui::Vec2, mut app: gui::App) {
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
fn instance_settings_tab() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_settings("survival");
    shot("instance_settings_tab", app);
}

#[test]
fn instance_log_tab() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_log("survival");
    shot("instance_log_tab", app);
}

#[test]
fn sidebar_pending_create() {
    let mut app = gui::App::for_snapshot_tests();
    app.start_op_for_tests(
        "brand-new",
        watt_core::ops::OpRequest::Create {
            name: "brand-new".into(),
            display_name: Some("Brand New".into()),
            color: None,
            group: None,
            source: watt_core::ops::SourceSpec::PwcDefault,
            mods: Vec::new(),
            build: false,
        },
    );
    app.show_instance_overview("brand-new");
    shot("sidebar_pending_create", app);
}

#[test]
fn instance_overview_small() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival");
    shot_at("instance_overview_small", egui::vec2(900.0, 600.0), app);
}

// ---- mods ----

#[test]
fn mods_installed() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    shot("mods_installed", app);
}

#[test]
fn mods_browse() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.mods_show_browse_for_tests("survival");
    shot("mods_browse", app);
}

#[test]
fn mods_many() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("modded");
    app.stage_mod_remove_for_tests("modded", "pwc.ambient-audio");
    app.stage_mod_add_for_tests("modded", "pwc.recipe-book");
    shot("mods_many", app);
}

#[test]
fn mods_many_browse_filtered() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("modded");
    app.mods_show_browse_for_tests("modded");
    app.mods_set_search_for_tests("modded", "ore");
    shot("mods_many_browse_filtered", app);
}

#[test]
fn mods_empty() {
    let mut app = gui::App::for_snapshot_tests();
    let Some(name) = app.mods_empty_instance_for_tests() else {
        return;
    };
    app.show_instance_mods(&name);
    shot("mods_empty", app);
}

#[test]
fn mods_narrow() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    shot_at("mods_narrow", egui::vec2(900.0, 600.0), app);
}

// ---- dialogs ----

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
    shot_at("settings_page_small", egui::vec2(900.0, 600.0), app);
}

// ---- fixes ----

fn app_harness(app: gui::App) -> Harness<'static, gui::App> {
    Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .wgpu()
        .build_ui_state(
            |ui, app: &mut gui::App| full_bleed(ui, |ui| app.ui(ui)),
            app,
        )
}

fn click_at(harness: &Harness<'_, gui::App>, pos: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(pos));
    harness.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
}

/// Backdrop press used to bury the dialog card under the dim layer. A control inside still works.
#[test]
fn new_instance_backdrop_then_choice() {
    let mut app = gui::App::for_snapshot_tests();
    app.open_new_instance_dialog();
    let mut harness = app_harness(app);
    harness.run_steps(3);
    click_at(&harness, egui::pos2(16.0, 16.0));
    harness.run();
    assert!(
        harness.state().new_instance_open_for_tests(),
        "backdrop click closed the new instance dialog"
    );
    harness.get_by_label("My game checkout").click();
    harness.run();
    assert_eq!(
        harness.state().new_instance_source_kind_for_tests(),
        "linked"
    );
}

/// The title is a label on top of the card. Clicking it selects the card.
#[test]
fn choice_card_title_selects() {
    let mut app = gui::App::for_snapshot_tests();
    app.open_new_instance_dialog();
    let mut harness = app_harness(app);
    harness.run_steps(3);
    harness.get_by_label("My game checkout").click();
    harness.run();
    assert_eq!(
        harness.state().new_instance_source_kind_for_tests(),
        "linked"
    );
}

/// Enter follows the same check as the Play button. A broken instance stays put.
#[test]
fn enter_does_not_play_broken() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("flaky-build");
    let mut harness = app_harness(app);
    harness.run_steps(3);
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(!harness.state().has_play_op_for_tests("flaky-build"));
}

/// The same key does launch a ready instance, so the broken case is not a dead shortcut.
#[test]
fn enter_plays_ready() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival");
    let mut harness = app_harness(app);
    harness.run_steps(3);
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(harness.state().has_play_op_for_tests("survival"));
}

#[test]
fn overview_900_scale_150() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_overview("survival");
    app.set_scale_for_tests(1.5);
    shot_at("overview_900_scale_150", egui::vec2(900.0, 600.0), app);
}

#[test]
fn mods_pending_900_scale_150() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.stage_mod_add_for_tests("survival", "pwc.better-caves");
    app.set_scale_for_tests(1.5);
    shot_at("mods_pending_900_scale_150", egui::vec2(900.0, 600.0), app);
}

// ---- mods v2 ----

#[test]
fn mods_impact() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("modded");
    app.stage_mod_add_for_tests("modded", "pwc.rivers");
    app.stage_mod_remove_for_tests("modded", "pwc.soundscape");
    app.stage_mod_remove_for_tests("modded", "pwc.ambient-audio");
    app.mods_open_impact_for_tests("modded");
    shot("mods_impact", app);
}

#[test]
fn mods_browse_compat() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("modded");
    app.mods_show_browse_for_tests("modded");
    app.mods_set_search_for_tests("modded", "rail");
    shot("mods_browse_compat", app);
}

#[test]
fn mods_details() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.mods_select_for_tests("survival", "pwc.essentials", false);
    shot("mods_details", app);
}

#[test]
fn mods_details_narrow() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.mods_select_for_tests("survival", "pwc.essentials", true);
    shot_at("mods_details_narrow", egui::vec2(900.0, 600.0), app);
}

#[test]
fn mods_updates() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("modded");
    shot("mods_updates", app);
}

#[test]
fn mods_apply_split() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.stage_mod_add_for_tests("survival", "pwc.better-caves");
    shot("mods_apply_split", app);
}

/// ↓ selects the first installed row, then Delete stages that mod for removal.
#[test]
fn mods_key_down_delete_stages_remove() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    let mut harness = app_harness(app);
    harness.run_steps(3);
    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::Delete);
    harness.run();
    assert_eq!(
        harness.state().mods_staged_remove_for_tests("survival"),
        vec!["pwc.essentials".to_owned()]
    );
}

/// `/` moves focus to the mods search field.
#[test]
fn mods_slash_focuses_search() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    let mut harness = app_harness(app);
    harness.run_steps(3);
    harness.key_press(egui::Key::Slash);
    harness.run();
    assert!(
        harness.get_by_label("Search mods").is_focused(),
        "slash did not focus the mods search"
    );
}

/// Escape clears the selected mod.
#[test]
fn mods_escape_clears_selection() {
    let mut app = gui::App::for_snapshot_tests();
    app.show_instance_mods("survival");
    app.mods_select_for_tests("survival", "pwc.essentials", false);
    let mut harness = app_harness(app);
    harness.run_steps(3);
    assert_eq!(
        harness
            .state()
            .mods_selected_for_tests("survival")
            .as_deref(),
        Some("pwc.essentials")
    );
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert_eq!(harness.state().mods_selected_for_tests("survival"), None);
}
