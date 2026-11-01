//! A standalone window that drives the forge through a scripted Build (success), a
//! Failed run (with recovery options) and a Cancelled run, looping forever. Useful for eyeballing
//! the animation without running a real build.
//!
//! Run with `cargo run -p watt-start --example forge_demo`.

// The demo only drives the forge; the rest of the included GUI tree is unused here.
#![allow(dead_code)]

#[path = "../src/gui/mod.rs"]
mod gui;

use gui::forge;

use egui::Color32;
use watt_core::events::{
    Envelope, Event, LogStream, OpKind, OpOutcome, PROTOCOL, Phase, PhaseOutcome, Recovery,
};
use watt_core::view::OpView;

/// Crate names the scripted compile phase claims to be working on.
const CRATES: &[&str] = &[
    "voxel_engine",
    "project_watt_cubed",
    "serde",
    "tokio",
    "wgpu",
    "egui",
    "anyhow",
    "pwc_manifest",
    "material",
    "field",
];

fn push(out: &mut Vec<Envelope>, seq: &mut u64, op_id: &str, t_ms: u64, event: Event) {
    out.push(Envelope {
        protocol: PROTOCOL,
        op_id: op_id.to_owned(),
        seq: *seq,
        t_ms,
        event,
    });
    *seq += 1;
}

/// A Build that cycles Toolchain -> Compile (0..437 crates) -> Link -> Install -> Ok.
fn script_success(op_id: &str) -> Vec<Envelope> {
    let mut out = Vec::new();
    let mut seq = 0u64;
    let phases = vec![
        Phase::Toolchain,
        Phase::Compile,
        Phase::Link,
        Phase::Install,
    ];
    push(
        &mut out,
        &mut seq,
        op_id,
        0,
        Event::OpStarted {
            op: OpKind::Build,
            instance: Some("survival".into()),
            phases,
            log: None,
        },
    );

    let mut t = 0u64;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Toolchain,
            title: Phase::Toolchain.title().to_owned(),
            determinate: false,
        },
    );
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::Log {
            stream: LogStream::Launcher,
            line: "rustc 1.90.0 (via nix develop)".into(),
        },
    );
    t += 1_800;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Toolchain,
            outcome: PhaseOutcome::Ok,
            duration_ms: 1_800,
        },
    );

    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Compile,
            title: Phase::Compile.title().to_owned(),
            determinate: true,
        },
    );
    let total: u64 = 437;
    let compile_duration: u64 = 40_000;
    let ticks = 90;
    let compile_start = t;
    for i in 1..=ticks {
        let tick_t = compile_start + compile_duration * i / ticks;
        let done = (total * i / ticks).min(total);
        let a = CRATES[(i as usize) % CRATES.len()];
        let b = CRATES[(i as usize + 3) % CRATES.len()];
        push(
            &mut out,
            &mut seq,
            op_id,
            tick_t,
            Event::Progress {
                phase: Phase::Compile,
                done,
                total: Some(total),
                detail: format!("{a}, {b}"),
            },
        );
        if i % 11 == 0 {
            push(
                &mut out,
                &mut seq,
                op_id,
                tick_t,
                Event::Log {
                    stream: LogStream::Tool,
                    line: format!("   Compiling {a} v0.{i}.0"),
                },
            );
        }
    }
    t = compile_start + compile_duration;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Compile,
            outcome: PhaseOutcome::Ok,
            duration_ms: compile_duration,
        },
    );

    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Link,
            title: Phase::Link.title().to_owned(),
            determinate: false,
        },
    );
    let link_duration = 10_000;
    t += link_duration;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Link,
            outcome: PhaseOutcome::Ok,
            duration_ms: link_duration,
        },
    );

    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Install,
            title: Phase::Install.title().to_owned(),
            determinate: false,
        },
    );
    let install_duration = 1_200;
    t += install_duration;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Install,
            outcome: PhaseOutcome::Ok,
            duration_ms: install_duration,
        },
    );

    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::OpFinished {
            outcome: OpOutcome::Ok {
                summary: "Built 1a2b3c4, 3 modules changed".into(),
            },
            duration_ms: t,
        },
    );
    out
}

/// A Build that fails midway through Compile, offering recovery options.
fn script_failed(op_id: &str) -> Vec<Envelope> {
    let mut out = Vec::new();
    let mut seq = 0u64;
    let phases = vec![Phase::Toolchain, Phase::Compile, Phase::Link];
    push(
        &mut out,
        &mut seq,
        op_id,
        0,
        Event::OpStarted {
            op: OpKind::Update,
            instance: Some("survival".into()),
            phases,
            log: None,
        },
    );
    let mut t = 0u64;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Toolchain,
            title: Phase::Toolchain.title().to_owned(),
            determinate: false,
        },
    );
    t += 1_200;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Toolchain,
            outcome: PhaseOutcome::Ok,
            duration_ms: 1_200,
        },
    );
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Compile,
            title: Phase::Compile.title().to_owned(),
            determinate: true,
        },
    );
    let total = 437;
    let ticks = 30;
    let fail_duration = 7_000;
    let compile_start = t;
    for i in 1..=ticks {
        let tick_t = compile_start + fail_duration * i / ticks;
        let done = total * i / (ticks * 3);
        push(
            &mut out,
            &mut seq,
            op_id,
            tick_t,
            Event::Progress {
                phase: Phase::Compile,
                done,
                total: Some(total),
                detail: format!("{}, {}", CRATES[i as usize % CRATES.len()], CRATES[0]),
            },
        );
    }
    t = compile_start + fail_duration;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::Log {
            stream: LogStream::Tool,
            line: "error[E0308]: mismatched types".into(),
        },
    );
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::OpFinished {
            outcome: OpOutcome::Failed {
                phase: Some(Phase::Compile),
                error: "error[E0308]: mismatched types\n  --> src/world/edits.rs:42:9\ncause: a module's pin changed without a lock bump".into(),
                recovery: vec![
                    Recovery::ResetLock,
                    Recovery::Retry,
                    Recovery::PlayLastGood,
                    Recovery::ForceRebuild,
                ],
            },
            duration_ms: t,
        },
    );
    out
}

/// A Build cancelled partway through Compile.
fn script_cancelled(op_id: &str) -> Vec<Envelope> {
    let mut out = Vec::new();
    let mut seq = 0u64;
    let phases = vec![Phase::Toolchain, Phase::Compile, Phase::Link];
    push(
        &mut out,
        &mut seq,
        op_id,
        0,
        Event::OpStarted {
            op: OpKind::Build,
            instance: Some("survival".into()),
            phases,
            log: None,
        },
    );
    let mut t = 0u64;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Toolchain,
            title: Phase::Toolchain.title().to_owned(),
            determinate: false,
        },
    );
    t += 900;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseFinished {
            phase: Phase::Toolchain,
            outcome: PhaseOutcome::Ok,
            duration_ms: 900,
        },
    );
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::PhaseStarted {
            phase: Phase::Compile,
            title: Phase::Compile.title().to_owned(),
            determinate: true,
        },
    );
    let total = 437;
    let ticks = 20;
    let cancel_duration = 5_000;
    let compile_start = t;
    for i in 1..=ticks {
        let tick_t = compile_start + cancel_duration * i / ticks;
        let done = total * i / (ticks * 4);
        push(
            &mut out,
            &mut seq,
            op_id,
            tick_t,
            Event::Progress {
                phase: Phase::Compile,
                done,
                total: Some(total),
                detail: format!("{}, {}", CRATES[i as usize % CRATES.len()], CRATES[1]),
            },
        );
    }
    t = compile_start + cancel_duration;
    push(
        &mut out,
        &mut seq,
        op_id,
        t,
        Event::OpFinished {
            outcome: OpOutcome::Cancelled,
            duration_ms: t,
        },
    );
    out
}

/// How long to linger on a finished scenario before starting the next one.
const LINGER_SECS: f64 = 4.0;

struct Demo {
    scripts: Vec<fn(&str) -> Vec<Envelope>>,
    scenario: usize,
    script: Vec<Envelope>,
    applied: usize,
    view: OpView,
    op_start: f64,
    finished_at: Option<f64>,
    style: forge::ForgeStyle,
}

impl Demo {
    fn new(now: f64) -> Self {
        let scripts: Vec<fn(&str) -> Vec<Envelope>> =
            vec![script_success, script_failed, script_cancelled];
        let mut demo = Self {
            scripts,
            scenario: 0,
            script: Vec::new(),
            applied: 0,
            view: OpView::default(),
            op_start: now,
            finished_at: None,
            style: forge::ForgeStyle {
                accent: Color32::from_rgb(90, 180, 255),
                animate: true,
                expected_ms: Some(11 * 60_000),
                title: "Survival".to_owned(),
                elapsed_ms: None,
                ready: true,
                retry_only: false,
            },
        };
        demo.start_scenario(now);
        demo
    }

    fn start_scenario(&mut self, now: f64) {
        let op_id = format!("demo-{}-{}", self.scenario, now as u64);
        self.script = self.scripts[self.scenario](&op_id);
        self.applied = 0;
        self.view = OpView::default();
        self.op_start = now;
        self.finished_at = None;
    }

    fn tick(&mut self, now: f64) {
        let elapsed_ms = ((now - self.op_start) * 1000.0).max(0.0) as u64;
        while self.applied < self.script.len() && self.script[self.applied].t_ms <= elapsed_ms {
            self.view.apply(&self.script[self.applied]);
            self.applied += 1;
        }
        self.style.elapsed_ms = Some(elapsed_ms);
        if !self.view.running() && self.finished_at.is_none() {
            self.finished_at = Some(now);
        }
        if let Some(finished_at) = self.finished_at
            && now - finished_at > LINGER_SECS
        {
            self.scenario = (self.scenario + 1) % self.scripts.len();
            self.start_scenario(now);
            self.style.elapsed_ms = Some(0);
        }
    }
}

fn main() -> eframe::Result {
    let icon = forge::app_icon_rgba(64);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(egui::vec2(980.0, 700.0))
            .with_icon(egui::IconData {
                rgba: icon,
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };

    let mut demo: Option<Demo> = None;
    eframe::run_ui_native("Forge demo", options, move |ui, _frame| {
        let now = ui.ctx().input(|i| i.time);
        let demo = demo.get_or_insert_with(|| Demo::new(now));
        demo.tick(now);
        gui::theme::apply(ui.ctx(), true, demo.style.animate);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                forge::cube_icon(ui, 22.0, demo.style.accent, demo.view.running());
                ui.label(
                    egui::RichText::new("Forge demo: cycling success / failed / cancelled runs")
                        .font(gui::theme::regular(14.0))
                        .color(gui::theme::TEXT),
                );
            });
            ui.add_space(8.0);
            let _ = forge::forge(ui, &demo.view, &demo.style);
        });
        // The script keeps playing after the forge itself has settled and stopped asking.
        ui.ctx().request_repaint();
    })
}
