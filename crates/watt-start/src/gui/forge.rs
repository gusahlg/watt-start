//! The Forge: build and update progress, drawn in the instance page's content area.
//!
//! The voxel cube stays (it is the launcher's mark) but the old lightning, orbiting dots,
//! brightness wave and bounce are gone. Motion is a slow turn, a short drop as voxels appear,
//! a white-hot glow while linking, and a spark burst on success. With animations off the cube
//! holds a fixed angle and every placement is already settled.
//!
//! Repaints are requested only while something is moving. A running op with animations off asks
//! for one repaint a second so the elapsed clock keeps ticking.

use std::f32::consts::TAU;
use std::time::Duration;

use egui::{Color32, Id, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, vec2};
use watt_core::events::{OpKind, OpOutcome, Phase, PhaseOutcome, Recovery};
use watt_core::view::{OpView, PhaseState, PhaseView};

use crate::gui::theme::{self, Palette};
use crate::gui::widgets::{self, Button, Icon, Size, Variant};

/// Look of the forge.
#[derive(Clone, Debug)]
pub struct ForgeStyle {
    /// The instance's accent colour.
    pub accent: egui::Color32,
    /// Animations on (`settings.appearance.animations`); off = a static cube and plain bars.
    pub animate: bool,
    /// How long the previous comparable build took, for the estimate ("about 11 min left").
    pub expected_ms: Option<u64>,
    /// The instance's shown name.
    pub title: String,
    /// Milliseconds since the op started, from the GUI's own clock (`view.t_ms` only moves when
    /// an event arrives, so it stalls during a quiet link). `None`: use `view.t_ms`.
    pub elapsed_ms: Option<u64>,
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
    pub recovery: Option<Recovery>,
    /// "Play" after a successful build/update.
    pub play: bool,
}

/// Max width of the status column under the cube.
const CONTENT_MAX_WIDTH: f32 = 640.0;
/// Side inset so nothing sits on the content edge.
const SIDE_INSET: f32 = 16.0;
/// How many voxels make up the cube (3x3x3).
const N_VOXELS: usize = 27;
/// Drop-in duration. Ease-out cubic, no bounce.
const DROP_SECS: f32 = 0.25;
/// How far a voxel falls, in grid units.
const DROP_HEIGHT: f32 = 1.2;
/// White flash after a voxel lands.
const FLASH_SECS: f32 = 0.30;
/// Success spark burst.
const SPARK_SECS: f64 = 0.8;
/// One full turn of the cube.
const ROTATE_SECS: f32 = 14.0;
/// Resting yaw: three faces, not aligned to an axis.
const BASE_YAW: f32 = 0.62;
/// Fixed camera tilt.
const TILT: f32 = -0.34;
/// Progress bar width.
const BAR_WIDTH: f32 = 420.0;
/// Stepper dot diameter.
const DOT: f32 = 14.0;

/// Per-op animation memory. Kept in egui temp data so a new op starts clean.
#[derive(Clone, Debug)]
struct ForgeState {
    /// The op this state belongs to; a mismatch means "start fresh".
    op_id: String,
    /// `ctx` time each voxel (spiral order) was placed, if placed yet.
    voxel_time: [Option<f64>; N_VOXELS],
    /// `ctx` time the outcome was first seen. Freezes the cube's yaw.
    settled_at: Option<f64>,
    /// Set when a failure is first shown, so the log opens once.
    log_opened_for_failure: bool,
}

impl ForgeState {
    fn fresh(op_id: String) -> Self {
        Self {
            op_id,
            voxel_time: [None; N_VOXELS],
            settled_at: None,
            log_opened_for_failure: false,
        }
    }
}

impl Default for ForgeState {
    fn default() -> Self {
        Self::fresh(String::new())
    }
}

/// Draw the forge into the available rect (the instance page's content area).
///
/// The column scrolls only when the rect is too short for the cube plus the status block.
pub fn forge(ui: &mut Ui, view: &OpView, style: &ForgeStyle) -> ForgeResponse {
    let now = ui.ctx().input(|i| i.time);
    let state_id = Id::unique("watt-start-forge").with(&view.op_id);
    let mut state = ui
        .ctx()
        .data_mut(|d| d.get_temp_mut_or_default::<ForgeState>(state_id).clone());
    if state.op_id != view.op_id {
        state = ForgeState::fresh(view.op_id.clone());
    }

    update_voxel_times(&mut state, view.fraction(), now, style.animate);
    if view.outcome.is_some() {
        if state.settled_at.is_none() {
            state.settled_at = Some(now);
        }
    } else {
        state.settled_at = None;
    }

    let failed = matches!(view.outcome, Some(OpOutcome::Failed { .. }));
    if failed && !state.log_opened_for_failure {
        state.log_opened_for_failure = true;
        ui.ctx()
            .data_mut(|d| d.insert_temp(log_open_id(view), true));
    }

    let avail = ui.available_size();
    let layout = forge_layout(avail.y);
    let rest = estimate_rest_height(view);
    let top_pad = ((avail.y - layout.cube_h - rest) * 0.5).max(0.0);

    let mut response = ForgeResponse::default();
    egui::ScrollArea::vertical()
        .id_salt(("forge-scroll", &view.op_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().interaction.selectable_labels = false;
            ui.spacing_mut().item_spacing.y = 0.0;
            if top_pad > 1.0 {
                ui.add_space(top_pad);
            }
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(SIDE_INSET as i8, layout.frame_v))
                .show(ui, |ui| {
                    let (cube_rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), layout.cube_h),
                        Sense::hover(),
                    );
                    draw_cube(ui, cube_rect, view, style, &state, now);

                    ui.add_space(layout.after_cube);
                    let column = ui.available_width().min(CONTENT_MAX_WIDTH);
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(column);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        draw_status_block(ui, view, style, layout.tight_status);
                        if !view.phases.is_empty() {
                            ui.add_space(layout.before_stepper);
                            draw_stepper(ui, view, style, now);
                        }
                        ui.add_space(layout.before_buttons);
                        response = draw_buttons(ui, view);
                        ui.add_space(layout.before_log);
                        draw_log(ui, view, failed);
                    });
                    ui.add_space(layout.tail);
                });
        });

    request_motion_repaint(ui, view, style, &state, now);
    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
    response
}

/// Record when each voxel first becomes visible. Placements stick.
///
/// With animation off, the time is back-dated so the drop and the flash are already over.
fn update_voxel_times(state: &mut ForgeState, fraction: f32, now: f64, animate: bool) {
    for k in 0..N_VOXELS {
        let threshold = (k + 1) as f32 / N_VOXELS as f32;
        if fraction >= threshold && state.voxel_time[k].is_none() {
            state.voxel_time[k] = Some(if animate {
                now
            } else {
                now - f64::from(DROP_SECS + FLASH_SECS) - 0.05
            });
        }
    }
}

/// Cube band and the gaps under it.
///
/// The spec band is `clamp(0.34 × height, 160, 300)`. That band plus the status column is
/// taller than the 590×400 content area, which pushed Cancel and Hide under the fold. When
/// the height is short the band shrinks (never below [`CUBE_MIN`]) and the gaps tighten, so
/// the running column stays on screen. A failed op with its log open may still scroll.
struct ForgeLayout {
    cube_h: f32,
    after_cube: f32,
    before_stepper: f32,
    before_buttons: f32,
    before_log: f32,
    tail: f32,
    frame_v: i8,
    tight_status: bool,
}

/// Shortest cube band that still holds the tilted cube at the minimum camera scale.
const CUBE_MIN: f32 = 112.0;

fn forge_layout(avail_y: f32) -> ForgeLayout {
    let ideal = (avail_y * 0.34).clamp(160.0, 300.0);
    // Status + stepper + buttons + collapsed log, including the frame margins.
    // Measured from the spacious 980×600 render and padded so a couple of pixels
    // of rounding cannot grow a scrollbar.
    const STACK: f32 = 320.0;
    if avail_y >= ideal + STACK {
        return ForgeLayout {
            cube_h: ideal,
            after_cube: 12.0,
            before_stepper: 16.0,
            before_buttons: 16.0,
            before_log: 12.0,
            tail: 8.0,
            frame_v: 4,
            tight_status: false,
        };
    }
    // Tight stack is the spacious one minus the gap cuts below (~48px).
    const TIGHT_STACK: f32 = 276.0;
    ForgeLayout {
        cube_h: (avail_y - TIGHT_STACK).clamp(CUBE_MIN, ideal),
        after_cube: 8.0,
        before_stepper: 8.0,
        before_buttons: 8.0,
        before_log: 8.0,
        tail: 4.0,
        frame_v: 0,
        tight_status: true,
    }
}

/// Generous height of everything under the cube, used only to centre the block.
///
/// Over-estimating drops the top padding (the block sits a little high) rather than pushing
/// the buttons into the scroll.
fn estimate_rest_height(view: &OpView) -> f32 {
    let mut h = 28.0;
    match &view.outcome {
        Some(OpOutcome::Ok { summary }) => {
            if !summary.is_empty() {
                h += 24.0;
            }
            h += 22.0;
        }
        Some(OpOutcome::Cancelled) => h += 46.0,
        Some(OpOutcome::Failed { error, .. }) => {
            let lines = error.lines().count().clamp(2, 8);
            h += 36.0 + lines as f32 * 16.0;
            h += 24.0;
        }
        None => {
            h += 28.0 + 48.0 + 24.0 + 22.0;
            if view.current == Some(Phase::Link) {
                h += 20.0;
            }
            if !view.detail.is_empty() || view.progress.is_some() {
                h += 20.0;
            }
        }
    }
    if !view.phases.is_empty() {
        h += 16.0 + 40.0;
    }
    h += 16.0 + 40.0;
    if matches!(view.outcome, Some(OpOutcome::Failed { ref recovery, .. }) if recovery.len() >= 3) {
        h += 44.0;
    }
    h += 12.0 + 32.0;
    if matches!(view.outcome, Some(OpOutcome::Failed { .. })) {
        let lines = view.log.len().clamp(1, 6);
        h += 16.0 + 28.0 + lines as f32 * 16.0;
    }
    h + 24.0
}

fn request_motion_repaint(
    ui: &Ui,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
) {
    let dropping = style.animate
        && state
            .voxel_time
            .iter()
            .flatten()
            .any(|t0| now - t0 < f64::from(DROP_SECS + FLASH_SECS));
    let success = matches!(view.outcome, Some(OpOutcome::Ok { .. }));
    let sparking =
        style.animate && success && state.settled_at.is_some_and(|t| now - t < SPARK_SECS);
    if style.animate && (view.running() || dropping || sparking) {
        ui.ctx().request_repaint();
    } else if view.running() {
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
}

// ---------------------------------------------------------------------------------------------
// Phase colour
// ---------------------------------------------------------------------------------------------

/// Work phases are Volt, waiting is neutral, linking is white-hot.
///
/// The instance accent tints Volt only, and only a little: Link, failure and cancel stay
/// readable as themselves.
fn phase_color(phase: Option<Phase>, accent: Color32, p: &Palette) -> Color32 {
    let base = match phase {
        Some(Phase::Link) => p.hot,
        Some(Phase::WaitForBuild) | None => p.neutral,
        Some(
            Phase::Prepare
            | Phase::Fetch
            | Phase::Checkout
            | Phase::Backup
            | Phase::Lock
            | Phase::Toolchain
            | Phase::Compile
            | Phase::Install
            | Phase::Launch
            | Phase::Cleanup,
        ) => p.volt,
    };
    if base == p.volt {
        base.lerp_to_gamma(accent, 0.12)
    } else {
        base
    }
}

fn cube_color(view: &OpView, accent: Color32, p: &Palette) -> Color32 {
    match view.outcome {
        Some(OpOutcome::Ok { .. }) => p.volt.lerp_to_gamma(accent, 0.12),
        Some(OpOutcome::Failed { .. }) => p.bad,
        Some(OpOutcome::Cancelled) => p.neutral,
        None => phase_color(view.current, accent, p),
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

// ---------------------------------------------------------------------------------------------
// The 3D cube
// ---------------------------------------------------------------------------------------------

/// A point in model space (voxel-grid units, cube centred at the origin).
#[derive(Clone, Copy)]
struct P3 {
    x: f32,
    y: f32,
    z: f32,
}

impl P3 {
    fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Rotate about the vertical (Y) axis, then tilt about X.
    fn rotated(self, yaw: f32, tilt: f32) -> Self {
        let (sy, cy) = yaw.sin_cos();
        let x1 = self.x * cy + self.z * sy;
        let z1 = -self.x * sy + self.z * cy;
        let (st, ct) = tilt.sin_cos();
        let y2 = self.y * ct - z1 * st;
        let z2 = self.y * st + z1 * ct;
        Self::new(x1, y2, z2)
    }
}

/// A simple perspective camera: fixed focal distance, origin-centred.
struct Camera {
    centre: Pos2,
    scale: f32,
    focal: f32,
}

impl Camera {
    fn project(&self, p: P3) -> Pos2 {
        let d = (self.focal / (self.focal + p.z)).max(0.1);
        Pos2::new(
            self.centre.x + p.x * self.scale * d,
            self.centre.y - p.y * self.scale * d,
        )
    }
}

/// Spiral order of the 9 cells in one 3x3 layer (clockwise from the top-left, ending centre).
const LAYER_SPIRAL: [(i32, i32); 9] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (2, 1),
    (2, 2),
    (1, 2),
    (0, 2),
    (0, 1),
    (1, 1),
];

/// The 27 voxel grid coordinates, bottom layer first, each layer a spiral.
fn voxel_order() -> [(i32, i32, i32); N_VOXELS] {
    let mut out = [(0, 0, 0); N_VOXELS];
    let mut k = 0;
    for gy in 0..3 {
        for (gx, gz) in LAYER_SPIRAL {
            out[k] = (gx, gy, gz);
            k += 1;
        }
    }
    out
}

fn draw_cube(
    ui: &mut Ui,
    rect: Rect,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
) {
    let p = theme::palette(ui.ctx());
    let painter = ui.painter_at(rect);
    let yaw = cube_yaw(style, view, state, now);
    let short = rect.width().min(rect.height());
    // Spec: scale = (short side × 0.14), clamped. The short side is the cube rect's.
    // A band under 160px makes that a speck, so grow it until the silhouette keeps
    // about 18px of air. 4.15 is the settled cube's screen height per unit of scale.
    let spec_scale = (short * 0.14).clamp(14.0, 48.0);
    let scale = if rect.height() < 160.0 {
        let fitted = ((rect.height() - 36.0) / 4.15).clamp(14.0, 48.0);
        spec_scale.max(fitted)
    } else {
        spec_scale
    };
    let cam = Camera {
        centre: rect.center(),
        scale,
        focal: 4.6,
    };

    let color = cube_color(view, style.accent, &p);
    let light_dir = Vec3::new(0.35, 0.85, 0.38).normalized();
    let linking = view.running() && view.current == Some(Phase::Link);

    if linking {
        draw_link_glow(&painter, rect, now, style.animate, &p);
    }

    let order = voxel_order();
    let mut quads: Vec<(f32, [Pos2; 4], Color32)> = Vec::with_capacity(27 * 3);
    let mut ghost_lines: Vec<[Pos2; 2]> = Vec::new();

    for (k, &(gx, gy, gz)) in order.iter().enumerate() {
        let gx = gx as f32 - 1.0;
        let gy = gy as f32 - 1.0;
        let gz = gz as f32 - 1.0;
        let placed_at = state.voxel_time[k];

        let center = match placed_at {
            Some(t0) => {
                let age = (now - t0) as f32;
                let drop = if style.animate && age < DROP_SECS {
                    (1.0 - ease_out_cubic(age / DROP_SECS)) * DROP_HEIGHT
                } else {
                    0.0
                };
                P3::new(gx, gy + drop, gz)
            }
            None => P3::new(gx, gy, gz),
        };

        let Some(placed_at) = placed_at else {
            for edge in CUBE_EDGES {
                let a = cube_vertex(center, edge.0).rotated(yaw, TILT);
                let b = cube_vertex(center, edge.1).rotated(yaw, TILT);
                ghost_lines.push([cam.project(a), cam.project(b)]);
            }
            continue;
        };

        let age = (now - placed_at) as f32;
        let flash = if style.animate {
            (1.0 - age / FLASH_SECS).clamp(0.0, 1.0)
        } else {
            0.0
        };

        for &(normal, face) in &CUBE_FACES {
            let n_rot = normal.rotated(yaw, TILT);
            if n_rot.z >= -0.02 {
                continue;
            }
            let verts: [P3; 4] = face.map(|v| cube_vertex(center, v).rotated(yaw, TILT));
            let depth = (verts[0].z + verts[1].z + verts[2].z + verts[3].z) / 4.0;
            let nv = Vec3::new(n_rot.x, n_rot.y, n_rot.z);
            let diffuse = 0.38 + 0.62 * nv.dot(light_dir).max(0.0);
            let mut face_color = color.gamma_multiply(diffuse);
            if flash > 0.0 {
                face_color = face_color.lerp_to_gamma(Color32::WHITE, flash);
            }
            let pts = verts.map(|v| cam.project(v));
            if !is_grazing_sliver(&pts) {
                quads.push((depth, pts, face_color));
            }
        }
    }

    let ghost = Color32::from_rgba_unmultiplied(170, 180, 195, 36);
    for [a, b] in &ghost_lines {
        painter.add(egui::Shape::line_segment([*a, *b], Stroke::new(1.0, ghost)));
    }

    quads.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let edge_color = Color32::from_rgba_unmultiplied(10, 10, 14, 90);
    for (_, pts, face_color) in &quads {
        painter.add(egui::Shape::convex_polygon(
            pts.to_vec(),
            *face_color,
            Stroke::new(1.0, edge_color),
        ));
    }

    if style.animate
        && matches!(view.outcome, Some(OpOutcome::Ok { .. }))
        && let Some(t0) = state.settled_at
    {
        let age = (now - t0) as f32;
        if age < SPARK_SECS as f32 {
            draw_sparks(
                &painter,
                rect,
                cam.project(P3::new(0.0, 0.0, 0.0)),
                age,
                &view.op_id,
                &p,
            );
        }
    }
}

/// Yaw while running; frozen at the moment the op settled. Static when animations are off.
fn cube_yaw(style: &ForgeStyle, view: &OpView, state: &ForgeState, now: f64) -> f32 {
    if !style.animate {
        return BASE_YAW;
    }
    let t = if view.running() {
        now
    } else {
        state.settled_at.unwrap_or(now)
    };
    BASE_YAW + (t as f32 / ROTATE_SECS) * TAU
}

/// A 3D vector for lighting. Separate from [`P3`] so a point and a direction aren't mixed.
#[derive(Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn normalized(self) -> Self {
        let len = (self.x * self.x + self.y * self.y + self.z * self.z)
            .sqrt()
            .max(1e-6);
        Self::new(self.x / len, self.y / len, self.z / len)
    }

    fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
}

/// Half-extent of a voxel. The gap to the next cell is `1.0 - 2*VOXEL_H`.
const VOXEL_H: f32 = 0.42;

/// The 8 corners of a unit cube (bit 0 = +x, bit 1 = +y, bit 2 = +z).
fn cube_vertex(center: P3, corner: u8) -> P3 {
    let sx = if corner & 1 != 0 { 1.0 } else { -1.0 };
    let sy = if corner & 2 != 0 { 1.0 } else { -1.0 };
    let sz = if corner & 4 != 0 { 1.0 } else { -1.0 };
    P3::new(
        center.x + sx * VOXEL_H,
        center.y + sy * VOXEL_H,
        center.z + sz * VOXEL_H,
    )
}

/// The 12 edges of a unit cube, as corner-index pairs (ghost wireframes).
const CUBE_EDGES: [(u8, u8); 12] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (1, 3),
    (1, 5),
    (2, 3),
    (2, 6),
    (3, 7),
    (4, 5),
    (4, 6),
    (5, 7),
    (6, 7),
];

/// The 6 faces: outward normal (unrotated) and 4 corners in winding order.
const CUBE_FACES: [(P3Lit, [u8; 4]); 6] = [
    (P3Lit(1.0, 0.0, 0.0), [1, 3, 7, 5]),
    (P3Lit(-1.0, 0.0, 0.0), [0, 4, 6, 2]),
    (P3Lit(0.0, 1.0, 0.0), [2, 3, 7, 6]),
    (P3Lit(0.0, -1.0, 0.0), [0, 1, 5, 4]),
    (P3Lit(0.0, 0.0, 1.0), [4, 5, 7, 6]),
    (P3Lit(0.0, 0.0, -1.0), [0, 2, 3, 1]),
];

/// A `const`-friendly stand-in for [`P3`].
#[derive(Clone, Copy)]
struct P3Lit(f32, f32, f32);

impl P3Lit {
    fn rotated(self, yaw: f32, tilt: f32) -> P3 {
        P3::new(self.0, self.1, self.2).rotated(yaw, tilt)
    }
}

/// Radial glow for the Link phase. The mesh is a disc, so it never clips to a square.
///
/// Centre is hot at alpha 70; the ring is transparent. Radius is `min(w, h) * 0.45`,
/// pulsing ±10% over 0.9s, and capped so the disc stays inside `rect`.
fn draw_link_glow(painter: &egui::Painter, rect: Rect, now: f64, animate: bool, p: &Palette) {
    let center = rect.center();
    let base = rect.width().min(rect.height()) * 0.45;
    let pulse = if animate {
        1.0 + 0.10 * (now * f64::from(TAU) / 0.9).sin() as f32
    } else {
        1.0
    };
    let cap = rect.width().min(rect.height()) * 0.5 - 1.0;
    let radius = (base * pulse).clamp(1.0, cap.max(1.0));

    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, theme::with_alpha(p.hot, 70));
    const RING: u32 = 32;
    for i in 0..RING {
        let a = TAU * (i as f32) / RING as f32;
        let pos = center + Vec2::new(a.cos(), a.sin()) * radius;
        mesh.colored_vertex(pos, theme::with_alpha(p.hot, 0));
    }
    for i in 0..RING {
        let a = 1 + i;
        let b = 1 + (i + 1) % RING;
        mesh.add_triangle(0, a, b);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Twenty-four sparks, travelling no further than the cube rect.
fn draw_sparks(
    painter: &egui::Painter,
    rect: Rect,
    origin: Pos2,
    age: f32,
    op_id: &str,
    p: &Palette,
) {
    let mut rng = Rng::new(seed_from(op_id, 0xF00D));
    let t = (age / SPARK_SECS as f32).clamp(0.0, 1.0);
    let fade = (1.0 - t).powf(1.2);
    // Part-way out immediately, then easing to the rim, so the burst is a ring and not a blob
    // sitting on the cube.
    let travel = 0.5 + 0.5 * ease_out_cubic(t);
    let max_r = inset_radius(origin, rect).max(8.0);
    for _ in 0..24 {
        let ang = rng.range(0.0, TAU);
        let reach = rng.range(0.58, 0.90) * max_r;
        let hot = rng.range(0.3, 0.95);
        let pos = origin + Vec2::new(ang.cos(), ang.sin()) * (reach * travel);
        if !rect.contains(pos) {
            continue;
        }
        let color = p.volt.lerp_to_gamma(Color32::WHITE, hot);
        let alpha = (fade * 230.0) as u8;
        let room = inset_radius(pos, rect).max(1.0);
        let radius = (1.3 + 2.8 * fade * (0.8 + 0.45 * hot)).min(room);
        let halo = (radius * 1.9).min(room);
        if halo > radius + 0.5 {
            painter.circle_filled(
                pos,
                halo,
                theme::with_alpha(color, (f32::from(alpha) * 0.16) as u8),
            );
        }
        painter.circle_filled(pos, radius, theme::with_alpha(color, alpha));
    }
}

/// Distance from `origin` to the nearest edge of `rect`, minus a hair of padding.
fn inset_radius(origin: Pos2, rect: Rect) -> f32 {
    (origin.x - rect.left())
        .min(rect.right() - origin.x)
        .min(origin.y - rect.top())
        .min(rect.bottom() - origin.y)
        - 3.0
}

/// True when a projected face is a near-zero-area sliver (a grazing angle's hairline).
fn is_grazing_sliver(pts: &[Pos2; 4]) -> bool {
    let mut area = 0.0_f32;
    for i in 0..4 {
        let p0 = pts[i];
        let p1 = pts[(i + 1) % 4];
        area += p0.x * p1.y - p1.x * p0.y;
    }
    area = area.abs() / 2.0;
    let diag = (pts[0] - pts[2]).length().max((pts[1] - pts[3]).length());
    diag > 10.0 && area < diag * diag * 0.08
}

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }
}

fn seed_from(op_id: &str, salt: u64) -> u32 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15 ^ salt;
    for b in op_id.bytes() {
        h = h.wrapping_mul(1_099_511_628_211).wrapping_add(u64::from(b));
    }
    (h ^ (h >> 32)) as u32
}

// ---------------------------------------------------------------------------------------------
// Status block
// ---------------------------------------------------------------------------------------------

fn draw_status_block(ui: &mut Ui, view: &OpView, style: &ForgeStyle, tight: bool) {
    let p = theme::palette(ui.ctx());
    let elapsed = style.elapsed_ms.unwrap_or(view.t_ms);

    match &view.outcome {
        Some(OpOutcome::Ok { summary }) => {
            let play = offers_play(view.op);
            centered_line(
                ui,
                &ready_headline(&style.title, play),
                theme::semibold(20.0),
                p.text_bright,
            );
            if !summary.is_empty() {
                ui.add_space(6.0);
                centered_line(ui, summary, theme::regular(14.0), p.text);
            }
            ui.add_space(6.0);
            centered_line(
                ui,
                &meta_line(view, style, elapsed),
                theme::regular(12.0),
                p.text_muted,
            );
        }
        Some(OpOutcome::Cancelled) => {
            centered_line(ui, "Cancelled", theme::semibold(20.0), p.neutral);
            ui.add_space(6.0);
            centered_line(
                ui,
                "Stopped on request.",
                theme::regular(14.0),
                p.text_muted,
            );
            ui.add_space(6.0);
            centered_line(
                ui,
                &meta_line(view, style, elapsed),
                theme::regular(12.0),
                p.text_muted,
            );
        }
        Some(OpOutcome::Failed { phase, error, .. }) => {
            centered_line(ui, &failed_while(*phase), theme::semibold(20.0), p.bad);
            ui.add_space(10.0);
            draw_error_box(ui, error, &p);
            ui.add_space(8.0);
            centered_line(
                ui,
                &meta_line(view, style, elapsed),
                theme::regular(12.0),
                p.text_muted,
            );
        }
        None => draw_running_status(ui, view, style, elapsed, &p, tight),
    }
}

fn draw_running_status(
    ui: &mut Ui,
    view: &OpView,
    style: &ForgeStyle,
    elapsed: u64,
    p: &Palette,
    tight: bool,
) {
    let color = phase_color(view.current, style.accent, p);
    let gap = if tight { 4.0 } else { 8.0 };
    centered_line(
        ui,
        action_headline(view.op),
        theme::semibold(20.0),
        p.text_bright,
    );
    let phase_title = current_phase_title(view);
    if !phase_title.is_empty() {
        ui.add_space(4.0);
        centered_line(ui, &phase_title, theme::regular(14.0), color);
    }
    ui.add_space(gap);
    let pct = format!("{:.0}%", view.fraction() * 100.0);
    centered_line(ui, &pct, theme::semibold(32.0), p.text_bright);
    ui.add_space(gap);
    draw_progress(ui, view.fraction(), color, p);
    if let Some(detail) = progress_detail(view) {
        ui.add_space(gap);
        centered_line(ui, &detail, theme::mono(12.0), p.text_muted);
    }
    if view.current == Some(Phase::Link) {
        ui.add_space(if tight { 2.0 } else { 4.0 });
        centered_line(
            ui,
            "Fat LTO: rustc is looking at the whole game at once",
            theme::regular(12.0),
            p.text_muted,
        );
    }
    ui.add_space(if tight { 4.0 } else { 6.0 });
    centered_line(
        ui,
        &meta_line(view, style, elapsed),
        theme::regular(12.0),
        p.text_muted,
    );
}

fn centered_line(ui: &mut Ui, text: &str, font: egui::FontId, color: Color32) {
    ui.label(egui::RichText::new(text).font(font).color(color));
}

fn draw_progress(ui: &mut Ui, fraction: f32, color: Color32, p: &Palette) {
    let fraction = fraction.clamp(0.0, 1.0);
    let width = ui.available_width().min(BAR_WIDTH);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    let shown = ui.ctx().animate_value_with_time(
        ui.scope_id().with("forge-bar"),
        fraction,
        theme::anim_secs(ui.ctx(), 0.3),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, p.bg_raised);
    let fill_w = rect.width() * shown.clamp(0.0, 1.0);
    if fill_w > 0.5 {
        let fill = Rect::from_min_size(rect.min, vec2(fill_w, rect.height()));
        let glow = Rect::from_min_size(
            pos_offset(fill.min, -1.0, -2.0),
            vec2(fill_w + 2.0, rect.height() + 4.0),
        );
        painter.rect_filled(glow, 6.0, theme::with_alpha(color, 48));
        painter.rect_filled(fill, 4.0, color);
    }
}

fn pos_offset(p: Pos2, dx: f32, dy: f32) -> Pos2 {
    Pos2::new(p.x + dx, p.y + dy)
}

/// "196 / 437 crates · voxel_engine", or just the detail line when it isn't a crate count.
fn progress_detail(view: &OpView) -> Option<String> {
    let detail = ellipsise(view.detail.trim(), 56);
    match view.progress {
        Some((done, Some(total))) if view.current == Some(Phase::Compile) && total > 0 => {
            let mut line = format!("{done} / {total} crates");
            if !detail.is_empty() {
                line.push_str(" · ");
                line.push_str(&detail);
            }
            Some(line)
        }
        Some((done, Some(total))) if total > 0 && view.current != Some(Phase::Compile) => {
            let mut line = format!("{done} / {total}");
            if !detail.is_empty() {
                line.push_str(" · ");
                line.push_str(&detail);
            }
            Some(line)
        }
        _ if !detail.is_empty() => Some(detail),
        _ => None,
    }
}

fn meta_line(view: &OpView, style: &ForgeStyle, elapsed_ms: u64) -> String {
    let mut line = format!("Elapsed {}", format_clock(elapsed_ms));
    if let Some(rest) = remaining_estimate(view, style, elapsed_ms) {
        line.push_str(" · ");
        line.push_str(&rest);
    }
    line
}

/// Rate-based remainder once the op has moved, otherwise the previous build's duration.
/// Hidden when there is nothing meaningful to say.
fn remaining_estimate(view: &OpView, style: &ForgeStyle, elapsed_ms: u64) -> Option<String> {
    if !view.running() {
        return None;
    }
    let fraction = view.fraction();
    if fraction >= 0.05 && elapsed_ms > 0 {
        let total = elapsed_ms as f32 / fraction;
        let remaining = (total - elapsed_ms as f32).max(0.0) as u64;
        if remaining < 45_000 {
            return None;
        }
        return Some(format!("{} left", about_minutes(remaining)));
    }
    style
        .expected_ms
        .filter(|ms| *ms >= 45_000)
        .map(|ms| format!("{} left", about_minutes(ms)))
}

fn about_minutes(ms: u64) -> String {
    let mins = (ms + 30_000) / 60_000;
    if mins <= 1 {
        "about a minute".to_owned()
    } else {
        format!("about {mins} min")
    }
}

fn format_clock(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn ellipsise(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn action_headline(op: Option<OpKind>) -> &'static str {
    match op {
        Some(OpKind::Build) => "Building",
        Some(OpKind::Update) => "Updating",
        Some(OpKind::Mods) => "Applying mod changes",
        Some(OpKind::Create) => "Creating",
        Some(OpKind::Duplicate) => "Copying",
        Some(OpKind::Rollback) => "Rolling back",
        Some(OpKind::Play) => "Launching",
        Some(OpKind::SetSource) => "Changing source",
        Some(OpKind::Remove) => "Removing",
        Some(OpKind::CheckUpdates) => "Checking for updates",
        Some(OpKind::Maintenance) => "Cleaning up",
        None => "Working",
    }
}

fn ready_headline(title: &str, play: bool) -> String {
    if !play {
        return "Done".to_owned();
    }
    let title = title.trim();
    if title.is_empty() {
        "Ready to play".to_owned()
    } else {
        format!("{title} is ready")
    }
}

fn failed_while(phase: Option<Phase>) -> String {
    match phase {
        Some(phase) => format!("Failed while {}", uncapitalise(phase.title())),
        None => "Failed".to_owned(),
    }
}

fn uncapitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn current_phase_title(view: &OpView) -> String {
    if let Some(phase) = view.current
        && let Some(found) = view.phases.iter().find(|p| p.phase == phase)
    {
        return found.title.clone();
    }
    view.current
        .map(|p| p.title().to_owned())
        .unwrap_or_default()
}

/// Play is offered after an op that leaves an instance ready to launch.
fn offers_play(op: Option<OpKind>) -> bool {
    matches!(
        op,
        Some(
            OpKind::Create
                | OpKind::Update
                | OpKind::Mods
                | OpKind::Build
                | OpKind::Rollback
                | OpKind::SetSource
                | OpKind::Duplicate
        )
    )
}

fn draw_error_box(ui: &mut Ui, error: &str, p: &Palette) {
    egui::Frame::new()
        .fill(theme::with_alpha(p.bad, 20))
        .stroke(Stroke::new(1.0, theme::with_alpha(p.bad, 128)))
        .corner_radius(12.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("forge-error")
                .max_height(150.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.style_mut().interaction.selectable_labels = true;
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error)
                                .font(theme::mono(12.0))
                                .color(p.text),
                        )
                        .selectable(true)
                        .wrap(),
                    );
                });
        });
}

// ---------------------------------------------------------------------------------------------
// Stepper
// ---------------------------------------------------------------------------------------------

/// One row of dots. Labels sit under every dot when they fit; otherwise only the active phase
/// is labelled, so nothing overlaps.
fn draw_stepper(ui: &mut Ui, view: &OpView, style: &ForgeStyle, now: f64) {
    let phases = &view.phases;
    let n = phases.len();
    if n == 0 {
        return;
    }
    let p = theme::palette(ui.ctx());
    let avail = ui.available_width();
    let font = theme::regular(11.0);
    let measure = |text: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER)
            .size()
            .x
    };

    let full: Vec<String> = phases.iter().map(|phase| phase.title.clone()).collect();
    let short: Vec<String> = phases
        .iter()
        .map(|phase| short_phase(phase.phase).to_owned())
        .collect();
    let full_w: Vec<f32> = full.iter().map(|s| measure(s)).collect();
    let short_w: Vec<f32> = short.iter().map(|s| measure(s)).collect();

    let (labels, pitch) = if let Some(pitch) = row_pitch(&full_w, avail) {
        (StepLabels::Each(full), pitch)
    } else if let Some(pitch) = row_pitch(&short_w, avail) {
        (StepLabels::Each(short), pitch)
    } else {
        let index = focus_index(phases);
        let text = if measure(&full[index]) <= avail {
            full[index].clone()
        } else {
            short[index].clone()
        };
        let pitch = if n == 1 {
            0.0
        } else {
            ((avail - DOT) / (n - 1) as f32).clamp(DOT + 4.0, 64.0)
        };
        (StepLabels::OnlyCurrent { index, text }, pitch)
    };

    let label_h = 15.0;
    let row_h = DOT + 4.0 + label_h;
    let (rect, _) = ui.allocate_exact_size(vec2(avail, row_h), Sense::hover());
    let row_w = if n == 1 {
        DOT
    } else {
        (n - 1) as f32 * pitch + DOT
    };
    let x0 = rect.center().x - row_w * 0.5 + DOT * 0.5;
    let dot_y = rect.top() + DOT * 0.5;
    let painter = ui.painter();

    for i in 0..n.saturating_sub(1) {
        let a = Pos2::new(x0 + pitch * i as f32, dot_y);
        let b = Pos2::new(x0 + pitch * (i + 1) as f32, dot_y);
        painter.line_segment([a, b], Stroke::new(2.0, p.line));
    }

    let spin_time = if style.animate { now as f32 } else { 0.2 };
    for (i, phase) in phases.iter().enumerate() {
        let center = Pos2::new(x0 + pitch * i as f32, dot_y);
        let hit = Rect::from_center_size(center, Vec2::splat(DOT + 8.0));
        let response = ui.interact(
            hit,
            ui.scope_id().with("forge-step").with(phase.phase),
            Sense::hover(),
        );
        if response.hovered() {
            painter.circle_filled(center, DOT * 0.5 + 3.0, p.wash_hover);
        }
        paint_step_dot(painter, center, phase, &p, spin_time);
        response.on_hover_text(phase_tooltip(phase));
    }

    match labels {
        StepLabels::Each(texts) => {
            for (i, text) in texts.iter().enumerate() {
                let center_x = x0 + pitch * i as f32;
                paint_step_label(
                    ui,
                    text,
                    center_x,
                    rect,
                    label_color(&phases[i].state, &p),
                    &font,
                );
            }
        }
        StepLabels::OnlyCurrent { index, text } => {
            let center_x = x0 + pitch * index as f32;
            paint_step_label(
                ui,
                &text,
                center_x,
                rect,
                label_color(&phases[index].state, &p),
                &font,
            );
        }
    }
}

enum StepLabels {
    Each(Vec<String>),
    OnlyCurrent { index: usize, text: String },
}

/// Pitch that keeps adjacent labels 8px apart, or `None` when the row would exceed `avail`.
fn row_pitch(widths: &[f32], avail: f32) -> Option<f32> {
    let n = widths.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        return if widths[0] <= avail { Some(0.0) } else { None };
    }
    let mut pitch = DOT + 10.0;
    for pair in widths.windows(2) {
        pitch = pitch.max(pair[0] * 0.5 + pair[1] * 0.5 + 8.0);
    }
    let width = (n - 1) as f32 * pitch + widths[0] * 0.5 + widths[n - 1] * 0.5;
    if width <= avail { Some(pitch) } else { None }
}

fn focus_index(phases: &[PhaseView]) -> usize {
    if let Some(i) = phases.iter().position(|p| p.state == PhaseState::Running) {
        return i;
    }
    if let Some(i) = phases
        .iter()
        .position(|p| matches!(p.state, PhaseState::Done(PhaseOutcome::Failed)))
    {
        return i;
    }
    phases
        .iter()
        .rposition(|p| matches!(p.state, PhaseState::Done(_)))
        .unwrap_or(0)
}

fn short_phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Prepare => "Prepare",
        Phase::Fetch => "Fetch",
        Phase::Checkout => "Unpack",
        Phase::Backup => "Backup",
        Phase::Lock => "Resolve",
        Phase::Toolchain => "Tools",
        Phase::WaitForBuild => "Waiting",
        Phase::Compile => "Compile",
        Phase::Link => "Link",
        Phase::Install => "Install",
        Phase::Launch => "Launch",
        Phase::Cleanup => "Cleanup",
    }
}

fn label_color(state: &PhaseState, p: &Palette) -> Color32 {
    match state {
        PhaseState::Running => p.text_bright,
        PhaseState::Done(PhaseOutcome::Failed) => p.bad,
        PhaseState::Done(PhaseOutcome::Cached | PhaseOutcome::Skipped) => p.neutral,
        PhaseState::Pending | PhaseState::Done(PhaseOutcome::Ok) => p.text_muted,
    }
}

fn paint_step_dot(
    painter: &egui::Painter,
    center: Pos2,
    phase: &PhaseView,
    p: &Palette,
    spin_time: f32,
) {
    match phase.state {
        PhaseState::Pending => {
            painter.circle_stroke(center, 6.0, Stroke::new(1.5, p.line_strong));
        }
        PhaseState::Running => {
            painter.circle_stroke(center, 6.0, Stroke::new(1.5, p.arc));
            let spin = Rect::from_center_size(center, Vec2::splat(12.0));
            widgets::paint_icon(painter, spin, Icon::Spinner, p.arc, spin_time);
        }
        PhaseState::Done(PhaseOutcome::Ok) => {
            painter.circle_filled(center, 7.0, p.volt);
            let icon = Rect::from_center_size(center, Vec2::splat(12.0));
            widgets::paint_icon(painter, icon, Icon::Check, p.on_accent, 0.0);
        }
        PhaseState::Done(PhaseOutcome::Failed) => {
            painter.circle_filled(center, 7.0, p.bad);
            let icon = Rect::from_center_size(center, Vec2::splat(11.0));
            widgets::paint_icon(painter, icon, Icon::Cross, p.text_bright, 0.0);
        }
        PhaseState::Done(PhaseOutcome::Cached | PhaseOutcome::Skipped) => {
            painter.circle_filled(center, 4.5, p.neutral);
        }
    }
}

fn paint_step_label(
    ui: &Ui,
    text: &str,
    center_x: f32,
    row: Rect,
    color: Color32,
    font: &egui::FontId,
) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), color);
    let mut x = center_x - galley.size().x * 0.5;
    x = x.clamp(row.left(), (row.right() - galley.size().x).max(row.left()));
    let y = row.top() + DOT + 4.0;
    ui.painter().galley(Pos2::new(x, y), galley, color);
}

fn phase_tooltip(phase: &PhaseView) -> String {
    match phase.state {
        PhaseState::Pending => format!("{} · waiting", phase.title),
        PhaseState::Running => format!("{} · in progress", phase.title),
        PhaseState::Done(PhaseOutcome::Ok) if phase.duration_ms >= 1000 => {
            format!("{} · {}", phase.title, format_span(phase.duration_ms))
        }
        PhaseState::Done(PhaseOutcome::Ok) => phase.title.clone(),
        PhaseState::Done(PhaseOutcome::Cached) => format!("{} · cached", phase.title),
        PhaseState::Done(PhaseOutcome::Skipped) => format!("{} · skipped", phase.title),
        PhaseState::Done(PhaseOutcome::Failed) => format!("{} · failed", phase.title),
    }
}

fn format_span(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

struct ForgeButton {
    label: &'static str,
    variant: Variant,
    kind: ButtonKind,
}

enum ButtonKind {
    Cancel,
    Hide,
    Play,
    Close,
    Recovery(Recovery),
}

fn draw_buttons(ui: &mut Ui, view: &OpView) -> ForgeResponse {
    let buttons = button_row(view);
    let gap = 8.0;
    let widths: Vec<f32> = buttons.iter().map(|b| button_width(ui, b.label)).collect();
    let avail = ui.available_width();
    let rows = pack_rows(&widths, avail, gap);

    let mut response = ForgeResponse::default();
    for (row_i, indices) in rows.iter().enumerate() {
        if row_i > 0 {
            ui.add_space(8.0);
        }
        let total: f32 = indices.iter().map(|&i| widths[i]).sum::<f32>()
            + gap * indices.len().saturating_sub(1) as f32;
        let pad = ((avail - total) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            // `item_spacing` is inserted before the next widget, so the pad is short by `gap`.
            if pad > gap {
                ui.add_space(pad - gap);
            }
            for &i in indices {
                let button = &buttons[i];
                let clicked = Button::new(button.label)
                    .variant(button.variant)
                    .size(Size::Medium)
                    .min_width(96.0)
                    .show(ui)
                    .clicked();
                if clicked {
                    apply_button(&mut response, &button.kind);
                }
            }
        });
    }
    response
}

fn button_row(view: &OpView) -> Vec<ForgeButton> {
    match &view.outcome {
        None => vec![
            ForgeButton {
                label: "Cancel",
                variant: Variant::Danger,
                kind: ButtonKind::Cancel,
            },
            ForgeButton {
                label: "Hide",
                variant: Variant::Secondary,
                kind: ButtonKind::Hide,
            },
        ],
        Some(OpOutcome::Ok { .. }) if offers_play(view.op) => vec![
            ForgeButton {
                label: "Play",
                variant: Variant::Primary,
                kind: ButtonKind::Play,
            },
            ForgeButton {
                label: "Close",
                variant: Variant::Secondary,
                kind: ButtonKind::Close,
            },
        ],
        Some(OpOutcome::Ok { .. }) => vec![ForgeButton {
            label: "Done",
            variant: Variant::Primary,
            kind: ButtonKind::Close,
        }],
        Some(OpOutcome::Cancelled) => vec![ForgeButton {
            label: "Close",
            variant: Variant::Secondary,
            kind: ButtonKind::Close,
        }],
        Some(OpOutcome::Failed { recovery, .. }) => {
            let mut row = Vec::with_capacity(recovery.len() + 1);
            for (i, recovery) in recovery.iter().enumerate() {
                row.push(ForgeButton {
                    label: recovery_label(*recovery),
                    variant: if i == 0 {
                        Variant::Primary
                    } else {
                        Variant::Secondary
                    },
                    kind: ButtonKind::Recovery(*recovery),
                });
            }
            row.push(ForgeButton {
                label: "Close",
                variant: Variant::Secondary,
                kind: ButtonKind::Close,
            });
            row
        }
    }
}

fn apply_button(response: &mut ForgeResponse, kind: &ButtonKind) {
    match kind {
        ButtonKind::Cancel => response.cancel = true,
        ButtonKind::Hide => response.hide = true,
        ButtonKind::Play => response.play = true,
        ButtonKind::Close => response.close = true,
        ButtonKind::Recovery(recovery) => response.recovery = Some(*recovery),
    }
}

fn button_width(ui: &Ui, label: &str) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            theme::semibold(14.0),
            Color32::PLACEHOLDER,
        )
        .size()
        .x;
    (text + 14.0 * 2.0).max(96.0)
}

fn pack_rows(widths: &[f32], avail: f32, gap: f32) -> Vec<Vec<usize>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut row_w = 0.0_f32;
    for (i, width) in widths.iter().enumerate() {
        let add = if row.is_empty() { *width } else { gap + *width };
        if !row.is_empty() && row_w + add > avail + 0.5 {
            rows.push(std::mem::take(&mut row));
            row_w = 0.0;
        }
        let add = if row.is_empty() { *width } else { gap + *width };
        row_w += add;
        row.push(i);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn recovery_label(r: Recovery) -> &'static str {
    match r {
        Recovery::Retry => "Retry",
        Recovery::ResetLock => "Reset lock",
        Recovery::Rollback => "Roll back",
        Recovery::PlayLastGood => "Play last build",
        Recovery::ForceRebuild => "Rebuild from scratch",
        Recovery::Doctor => "Check system",
    }
}

// ---------------------------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------------------------

fn log_open_id(view: &OpView) -> Id {
    Id::unique("watt-forge-log").with(&view.op_id)
}

fn log_button(ui: &mut Ui, open: bool) -> bool {
    let icon = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    Button::new("Log")
        .variant(Variant::Ghost)
        .size(Size::Small)
        .icon(icon)
        .show(ui)
        .clicked()
}

fn draw_log(ui: &mut Ui, view: &OpView, failed: bool) {
    let p = theme::palette(ui.ctx());
    let id = log_open_id(view);
    let open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(failed);
    // Collapsed, the disclosure sits under the centred buttons. Open, the box is the
    // full column width with the header at its top-left.
    if !open {
        if log_button(ui, false) {
            ui.ctx().data_mut(|d| d.insert_temp(id, true));
        }
        return;
    }
    ui.vertical(|ui| {
        ui.set_min_width(ui.available_width());
        if log_button(ui, true) {
            ui.ctx().data_mut(|d| d.insert_temp(id, false));
        }
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(p.bg_canvas)
            .corner_radius(12.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt(("forge-log-scroll", &view.op_id))
                    .max_height(200.0)
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if view.log.is_empty() {
                            ui.label(
                                egui::RichText::new("No log yet.")
                                    .font(theme::mono(12.0))
                                    .color(p.text_muted),
                            );
                        } else {
                            for line in &view.log {
                                ui.label(
                                    egui::RichText::new(line)
                                        .font(theme::mono(12.0))
                                        .color(p.text),
                                );
                            }
                        }
                    });
            });
    });
}

// ---------------------------------------------------------------------------------------------
// Small cube icon and the window icon
// ---------------------------------------------------------------------------------------------

/// Camera scale (relative to `size`) for a single-voxel icon, tuned so the silhouette fills
/// about 90% of its box.
const ICON_CUBE_SCALE: f32 = 0.82;

/// A small isometric cube (legacy call site). Always static.
///
/// `busy` used to spin the cube and draw a crackle. That slash is gone; the sidebar paints
/// [`widgets::cube`] instead. The flag stays so existing callers keep compiling.
pub fn cube_icon(ui: &mut Ui, size: f32, color: Color32, busy: bool) -> Response {
    let _ = busy;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter_at(rect);
    let yaw = 0.6_f32;
    let tilt = -0.3_f32;
    let cam = Camera {
        centre: rect.center(),
        scale: size * ICON_CUBE_SCALE,
        focal: 4.0,
    };
    paint_icon_cube(&painter, &cam, yaw, tilt, color);
    response
}

fn paint_icon_cube(painter: &egui::Painter, cam: &Camera, yaw: f32, tilt: f32, color: Color32) {
    let center = P3::new(0.0, 0.0, 0.0);
    let light_dir = Vec3::new(0.35, 0.85, 0.38).normalized();
    let mut quads: Vec<(f32, [Pos2; 4], Color32)> = Vec::with_capacity(3);
    for &(normal, face) in &CUBE_FACES {
        let n_rot = normal.rotated(yaw, tilt);
        if n_rot.z >= -0.02 {
            continue;
        }
        let verts: [P3; 4] = face.map(|v| cube_vertex(center, v).rotated(yaw, tilt));
        let depth = (verts[0].z + verts[1].z + verts[2].z + verts[3].z) / 4.0;
        let nv = Vec3::new(n_rot.x, n_rot.y, n_rot.z);
        let diffuse = 0.4 + 0.6 * nv.dot(light_dir).max(0.0);
        let pts = verts.map(|v| cam.project(v));
        quads.push((depth, pts, color.gamma_multiply(diffuse)));
    }
    quads.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let edge_color = Color32::from_rgba_unmultiplied(10, 10, 14, 110);
    for (_, pts, c) in &quads {
        painter.add(egui::Shape::convex_polygon(
            pts.to_vec(),
            *c,
            Stroke::new(1.0, edge_color),
        ));
    }
}

/// The window icon: a cube in the launcher colours, as RGBA pixels (`size` x `size`).
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let size = size.max(4);
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let amber = Color32::from_rgb(255, 160, 40);
    let amber_dark = Color32::from_rgb(200, 110, 20);
    let bolt = Color32::from_rgb(255, 240, 200);

    let cam = Camera {
        centre: Pos2::new(size as f32 / 2.0, size as f32 / 2.0),
        scale: size as f32 * ICON_CUBE_SCALE,
        focal: 4.0,
    };
    let yaw = 0.6_f32;
    let tilt = -0.3_f32;
    let center = P3::new(0.0, 0.0, 0.0);
    let light_dir = Vec3::new(0.35, 0.85, 0.38).normalized();

    let mut polys: Vec<(f32, [Pos2; 4], Color32)> = Vec::new();
    for (i, &(normal, face)) in CUBE_FACES.iter().enumerate() {
        let n_rot = normal.rotated(yaw, tilt);
        if n_rot.z >= -0.02 {
            continue;
        }
        let verts: [P3; 4] = face.map(|v| cube_vertex(center, v).rotated(yaw, tilt));
        let depth = (verts[0].z + verts[1].z + verts[2].z + verts[3].z) / 4.0;
        let nv = Vec3::new(n_rot.x, n_rot.y, n_rot.z);
        let diffuse = 0.4 + 0.6 * nv.dot(light_dir).max(0.0);
        let base = if i % 2 == 0 { amber } else { amber_dark };
        let pts = verts.map(|v| cam.project(v));
        polys.push((depth, pts, base.gamma_multiply(diffuse)));
    }
    polys.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    for y in 0..size {
        for x in 0..size {
            let p = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            let mut pixel = Color32::TRANSPARENT;
            for (_, quad, color) in &polys {
                if point_in_convex_quad(p, quad) {
                    pixel = *color;
                }
            }
            let idx = ((y * size + x) * 4) as usize;
            pixels[idx] = pixel.r();
            pixels[idx + 1] = pixel.g();
            pixels[idx + 2] = pixel.b();
            pixels[idx + 3] = pixel.a();
        }
    }

    // A lightning-bolt glyph through the centre, bright so it stays legible at small sizes.
    let bolt_pts: [(f32, f32); 5] = [
        (0.52, 0.14),
        (0.40, 0.48),
        (0.50, 0.48),
        (0.42, 0.86),
        (0.66, 0.42),
    ];
    let w = size as f32;
    for i in 0..bolt_pts.len() - 1 {
        let (x0, y0) = bolt_pts[i];
        let (x1, y1) = bolt_pts[i + 1];
        draw_line_px(
            &mut pixels,
            size,
            Pos2::new(x0 * w, y0 * w),
            Pos2::new(x1 * w, y1 * w),
            bolt,
            (size as f32 * 0.05).max(1.0),
        );
    }

    pixels
}

fn point_in_convex_quad(p: Pos2, quad: &[Pos2; 4]) -> bool {
    let mut sign = 0.0_f32;
    for i in 0..4 {
        let a = quad[i];
        let b = quad[(i + 1) % 4];
        let edge = b - a;
        let to_p = p - a;
        let cross = edge.x * to_p.y - edge.y * to_p.x;
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != 0.0 && cross.signum() != sign {
            return false;
        }
    }
    true
}

/// A thick line drawn into an RGBA buffer (the window icon has no painter).
fn draw_line_px(pixels: &mut [u8], size: u32, a: Pos2, b: Pos2, color: Color32, width: f32) {
    let steps = ((b - a).length().ceil() as i32).max(1) * 2;
    let half = width / 2.0;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let p = a + (b - a) * t;
        let r = half.ceil() as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if (dx * dx + dy * dy) as f32 > half * half {
                    continue;
                }
                let x = p.x as i32 + dx;
                let y = p.y as i32 + dy;
                if x < 0 || y < 0 || x as u32 >= size || y as u32 >= size {
                    continue;
                }
                let idx = ((y as u32 * size + x as u32) * 4) as usize;
                pixels[idx] = color.r();
                pixels[idx + 1] = color.g();
                pixels[idx + 2] = color.b();
                pixels[idx + 3] = color.a();
            }
        }
    }
}
