//! Headless screenshot tests for the forge. These render real frames (via `egui_kittest`'s wgpu
//! backend) and save PNGs under `target/forge-shots/` for visual review; they don't assert pixel
//! content (no golden images checked in), just that each scene builds and renders without
//! panicking.

#[path = "../src/gui/forge.rs"]
mod forge;

use egui::Color32;
use watt_core::events::{
    Envelope, Event, LogStream, OpKind, OpOutcome, PROTOCOL, Phase, PhaseOutcome, Recovery,
};
use watt_core::view::OpView;

fn env(op_id: &str, seq: u64, t_ms: u64, event: Event) -> Envelope {
    Envelope {
        protocol: PROTOCOL,
        op_id: op_id.to_owned(),
        seq,
        t_ms,
        event,
    }
}

/// A view sitting at `fraction` through a Compile phase with `total` crates.
fn compiling_view(op_id: &str, done: u64, total: u64) -> OpView {
    let mut view = OpView::default();
    let mut seq = 0;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::OpStarted {
            op: OpKind::Build,
            instance: Some("survival".into()),
            phases: vec![
                Phase::Toolchain,
                Phase::Compile,
                Phase::Link,
                Phase::Install,
            ],
            log: None,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::PhaseStarted {
            phase: Phase::Toolchain,
            title: Phase::Toolchain.title().to_owned(),
            determinate: false,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        1_000,
        Event::PhaseFinished {
            phase: Phase::Toolchain,
            outcome: PhaseOutcome::Ok,
            duration_ms: 1_000,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        1_000,
        Event::PhaseStarted {
            phase: Phase::Compile,
            title: Phase::Compile.title().to_owned(),
            determinate: true,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        18_000,
        Event::Progress {
            phase: Phase::Compile,
            done,
            total: Some(total),
            detail: "voxel_engine, project_watt_cubed".to_owned(),
        },
    ));
    seq += 1;
    for i in 0..40 {
        view.apply(&env(
            op_id,
            seq,
            18_000,
            Event::Log {
                stream: LogStream::Tool,
                line: format!("   Compiling crate_{i} v0.1.0"),
            },
        ));
        seq += 1;
    }
    view
}

/// A view in the middle of the Link phase (indeterminate).
fn linking_view(op_id: &str) -> OpView {
    let mut view = OpView::default();
    let mut seq = 0;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::OpStarted {
            op: OpKind::Build,
            instance: Some("survival".into()),
            phases: vec![
                Phase::Toolchain,
                Phase::Compile,
                Phase::Link,
                Phase::Install,
            ],
            log: None,
        },
    ));
    seq += 1;
    for (phase, dur) in [(Phase::Toolchain, 1_000), (Phase::Compile, 40_000)] {
        view.apply(&env(
            op_id,
            seq,
            0,
            Event::PhaseStarted {
                phase,
                title: phase.title().to_owned(),
                determinate: false,
            },
        ));
        seq += 1;
        view.apply(&env(
            op_id,
            seq,
            dur,
            Event::PhaseFinished {
                phase,
                outcome: PhaseOutcome::Ok,
                duration_ms: dur,
            },
        ));
        seq += 1;
    }
    view.apply(&env(
        op_id,
        seq,
        41_000,
        Event::PhaseStarted {
            phase: Phase::Link,
            title: Phase::Link.title().to_owned(),
            determinate: false,
        },
    ));
    view
}

fn finished_ok_view(op_id: &str) -> OpView {
    let mut view = OpView::default();
    let mut seq = 0;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::OpStarted {
            op: OpKind::Build,
            instance: Some("survival".into()),
            phases: vec![
                Phase::Toolchain,
                Phase::Compile,
                Phase::Link,
                Phase::Install,
            ],
            log: None,
        },
    ));
    seq += 1;
    for (phase, dur) in [
        (Phase::Toolchain, 1_000),
        (Phase::Compile, 40_000),
        (Phase::Link, 8_000),
        (Phase::Install, 1_200),
    ] {
        view.apply(&env(
            op_id,
            seq,
            0,
            Event::PhaseStarted {
                phase,
                title: phase.title().to_owned(),
                determinate: false,
            },
        ));
        seq += 1;
        view.apply(&env(
            op_id,
            seq,
            dur,
            Event::PhaseFinished {
                phase,
                outcome: PhaseOutcome::Ok,
                duration_ms: dur,
            },
        ));
        seq += 1;
    }
    view.apply(&env(
        op_id,
        seq,
        50_200,
        Event::OpFinished {
            outcome: OpOutcome::Ok {
                summary: "Built 1a2b3c4, 3 mods changed".into(),
            },
            duration_ms: 50_200,
        },
    ));
    view
}

fn failed_view(op_id: &str) -> OpView {
    let mut view = OpView::default();
    let mut seq = 0;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::OpStarted {
            op: OpKind::Update,
            instance: Some("survival".into()),
            phases: vec![Phase::Toolchain, Phase::Compile],
            log: None,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::PhaseStarted {
            phase: Phase::Toolchain,
            title: Phase::Toolchain.title().to_owned(),
            determinate: false,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        900,
        Event::PhaseFinished {
            phase: Phase::Toolchain,
            outcome: PhaseOutcome::Ok,
            duration_ms: 900,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        900,
        Event::PhaseStarted {
            phase: Phase::Compile,
            title: Phase::Compile.title().to_owned(),
            determinate: true,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        5_000,
        Event::Progress {
            phase: Phase::Compile,
            done: 80,
            total: Some(437),
            detail: "voxel_engine".to_owned(),
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        5_200,
        Event::OpFinished {
            outcome: OpOutcome::Failed {
                phase: Some(Phase::Compile),
                error: "error[E0308]: mismatched types\n  --> src/world/edits.rs:42:9\ncause: a mod's pin changed without a lock bump".into(),
                recovery: vec![
                    Recovery::ResetLock,
                    Recovery::Retry,
                    Recovery::PlayLastGood,
                    Recovery::ForceRebuild,
                ],
            },
            duration_ms: 5_200,
        },
    ));
    view
}

/// A view in Fetch, which has no total (indeterminate), with a bit of running time.
fn fetching_view(op_id: &str) -> OpView {
    let mut view = OpView::default();
    let mut seq = 0;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::OpStarted {
            op: OpKind::Update,
            instance: Some("survival".into()),
            phases: vec![Phase::Fetch, Phase::Lock, Phase::Compile],
            log: None,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        0,
        Event::PhaseStarted {
            phase: Phase::Fetch,
            title: Phase::Fetch.title().to_owned(),
            determinate: false,
        },
    ));
    seq += 1;
    view.apply(&env(
        op_id,
        seq,
        2_000,
        Event::Progress {
            phase: Phase::Fetch,
            done: 1,
            total: None,
            detail: "voxel_engine, project_watt_cubed".to_owned(),
        },
    ));
    view
}

fn style(animate: bool) -> forge::ForgeStyle {
    forge::ForgeStyle {
        accent: Color32::from_rgb(90, 180, 255),
        animate,
        expected_ms: Some(11 * 60_000),
        title: "Survival".to_owned(),
    }
}

/// Build a harness for `view`/`style`, step it a few times (so placement/flash/arc animations
/// settle into a representative frame), render it and save the PNG under
/// `target/forge-shots/<name>.png`. Panics (failing the test) if anything in the pipeline fails.
/// `times` are `ctx` seconds set explicitly before each step (via `RawInput::time`), so the
/// cube's rotation and the finish-burst age are fully deterministic rather than depending on how
/// long each step happens to take to render in real wall-clock time.
fn snapshot(name: &str, view: OpView, style: forge::ForgeStyle, size: egui::Vec2, times: &[f64]) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/forge-shots");
    std::fs::create_dir_all(dir).expect("create target/forge-shots");

    let mut harness = egui_kittest::Harness::builder()
        .with_size(size)
        .wgpu()
        .build_ui(move |ui| {
            forge::forge(ui, &view, &style);
        });
    for &t in times {
        harness.input_mut().time = Some(t);
        harness.step();
    }
    let image = harness.render().expect("render");
    let path = format!("{dir}/{name}.png");
    image
        .save(&path)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

/// A handful of steps spanning ~1.5 simulated seconds: enough rotation for the cube to read
/// clearly as 3D without landing on an axis-aligned angle.
const STEPS: &[f64] = &[0.0, 0.3, 0.6, 0.9, 1.2, 1.5];

#[test]
fn compile_about_45_percent() {
    let view = compiling_view("snap-compile", 196, 437);
    snapshot(
        "compile_45pct",
        view,
        style(true),
        egui::vec2(1100.0, 760.0),
        STEPS,
    );
}

#[test]
fn link_phase() {
    let view = linking_view("snap-link");
    snapshot("link", view, style(true), egui::vec2(1100.0, 760.0), STEPS);
}

#[test]
fn finished_ok() {
    // The finish burst starts on the first frame the Ok outcome is seen (t=0.0 here); render a
    // second frame 250ms later so the spark burst and shockwave ring are caught mid-flight.
    let view = finished_ok_view("snap-ok");
    snapshot(
        "finished_ok",
        view,
        style(true),
        egui::vec2(1100.0, 760.0),
        &[0.0, 0.25],
    );
}

#[test]
fn failed_with_recovery() {
    let view = failed_view("snap-failed");
    snapshot(
        "failed_recovery",
        view,
        style(true),
        egui::vec2(1100.0, 760.0),
        STEPS,
    );
}

#[test]
fn fetch_indeterminate() {
    let view = fetching_view("snap-fetch");
    snapshot(
        "fetch_indeterminate",
        view,
        style(true),
        egui::vec2(1100.0, 760.0),
        STEPS,
    );
}

#[test]
fn animation_off_is_static() {
    let view = compiling_view("snap-static", 196, 437);
    snapshot(
        "animate_off",
        view,
        style(false),
        egui::vec2(1100.0, 760.0),
        &[0.0, 0.3],
    );
}

#[test]
fn small_size() {
    let view = compiling_view("snap-small", 196, 437);
    snapshot(
        "small_560x420",
        view,
        style(true),
        egui::vec2(560.0, 420.0),
        STEPS,
    );
}

/// `cube_icon` at the sizes it's actually called at around the app (sidebar row icons at 20,
/// the sidebar header at 26, the welcome screen at 64) plus a small 16, on one strip, so a
/// regression in how big the cube renders relative to its box is visible at a glance.
#[test]
fn cube_icon_sizes() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/forge-shots");
    std::fs::create_dir_all(dir).expect("create target/forge-shots");

    let sizes: [f32; 5] = [16.0, 20.0, 26.0, 32.0, 64.0];
    let accent = Color32::from_rgb(230, 170, 70);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(420.0, 100.0))
        .wgpu()
        .build_ui(move |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(16.0);
                for &size in &sizes {
                    forge::cube_icon(ui, size, accent, false);
                    ui.add_space(16.0);
                }
            });
        });
    harness.input_mut().time = Some(0.0);
    harness.step();
    let image = harness.render().expect("render");
    let path = format!("{dir}/cube_icon_sizes.png");
    image
        .save(&path)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}
