//! The Forge: the animated build/update display, and the small cube icons and spinners used
//! around the launcher.
//!
//! Everything is drawn with egui's painter (no textures, no assets). Animations follow
//! `ui.ctx()` time and request repaints only while something moves.

use std::f32::consts::TAU;

use egui::{Color32, Id, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, vec2};
use watt_core::events::{Phase, Recovery};
use watt_core::view::{OpView, PhaseState};

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

/// How many voxels make up the cube (3x3x3).
const N_VOXELS: usize = 27;
/// How long a voxel's drop-in-and-bounce animation lasts.
const PLACE_ANIM_SECS: f64 = 0.4;
/// How long the finish-spark burst lasts.
const SPARK_SECS: f64 = 1.0;
/// Rotation period, in seconds, for a full turn of the cube.
const ROTATE_SECS: f32 = 14.0;

/// Per-op animation memory, kept in egui's temporary data so placements and finish animations
/// stay smooth across frames and restart cleanly for a new op.
#[derive(Clone, Debug)]
struct ForgeState {
    /// The op this state belongs to; a mismatch means "start fresh".
    op_id: String,
    /// `ctx` time each voxel (in spiral placement order) was placed, if placed yet.
    voxel_time: [Option<f64>; N_VOXELS],
    /// `ctx` time the op's outcome was first observed (drives the finish animation).
    finish_time: Option<f64>,
    /// A small counter mixed into the arc/spark RNG seed so bolts look different over time.
    tick: u64,
}

impl ForgeState {
    fn fresh(op_id: String) -> Self {
        Self {
            op_id,
            voxel_time: [None; N_VOXELS],
            finish_time: None,
            tick: 0,
        }
    }
}

impl Default for ForgeState {
    fn default() -> Self {
        Self::fresh(String::new())
    }
}

/// Draw the full forge for an op into the available space.
pub fn forge(ui: &mut Ui, view: &OpView, style: &ForgeStyle) -> ForgeResponse {
    let now = ui.ctx().input(|i| i.time);
    let state_id = Id::unique("watt-start-forge").with(&view.op_id);
    let mut state = ui
        .ctx()
        .data_mut(|d| d.get_temp_mut_or_default::<ForgeState>(state_id).clone());
    if state.op_id != view.op_id {
        state = ForgeState::fresh(view.op_id.clone());
    }
    state.tick = state.tick.wrapping_add(1);

    let fraction = view.fraction();
    update_voxel_times(&mut state, fraction, now, style.animate);
    let just_finished = view.outcome.is_some() && state.finish_time.is_none();
    if just_finished {
        state.finish_time = Some(now);
    }
    if view.outcome.is_none() {
        state.finish_time = None;
    }

    let finish_age = state.finish_time.map(|t| now - t);
    let spark_burst = style.animate && finish_age.is_some_and(|age| age < SPARK_SECS);
    let animating = style.animate && (view.running() || spark_burst);

    let mut response = ForgeResponse::default();

    ui.vertical(|ui| {
        let avail = ui.available_rect_before_wrap();
        let cube_h = (avail.height() * 0.5).clamp(160.0, 480.0);
        // The rest of the panel (status text, chips, buttons, the collapsed log header) takes
        // roughly this much height; centre the whole block vertically by padding above it,
        // rather than leaving it stranded at the top with empty space below. Below the minimum
        // supported size the content no longer fits at all; a scroll area (rather than silent
        // clipping) keeps the buttons reachable.
        const REST_H: f32 = 250.0;
        let top_pad = ((avail.height() - cube_h - REST_H) / 2.0).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("forge-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if top_pad > 1.0 {
                    ui.add_space(top_pad);
                }
                let (cube_rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), cube_h), Sense::hover());
                draw_cube(ui, cube_rect, view, style, &state, now, finish_age);

                ui.add_space(6.0);
                draw_status_block(ui, view, style, now);
                ui.add_space(4.0);
                draw_phase_checklist(ui, view, style);
                ui.add_space(6.0);
                response = draw_buttons(ui, view);
                ui.add_space(4.0);
                draw_log_section(ui, view);
            });
    });

    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
    if animating {
        ui.ctx().request_repaint();
    }
    response
}

/// Record the moment (in `ctx` time) each voxel first becomes visible, as `fraction` grows.
/// Voxels never un-place even if `fraction` were to shrink (it doesn't, in practice).
fn update_voxel_times(state: &mut ForgeState, fraction: f32, now: f64, animate: bool) {
    for k in 0..N_VOXELS {
        let threshold = (k + 1) as f32 / N_VOXELS as f32;
        if fraction >= threshold && state.voxel_time[k].is_none() {
            // With animation off, back-date the placement so no bounce/flash is seen.
            state.voxel_time[k] = Some(if animate {
                now
            } else {
                now - PLACE_ANIM_SECS * 2.0
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Phase colours and energy
// ---------------------------------------------------------------------------------------------

/// The phase's base colour, before blending with the instance accent.
fn phase_base_color(phase: Option<Phase>) -> Color32 {
    match phase {
        Some(Phase::Fetch | Phase::Checkout) => Color32::from_rgb(70, 210, 230),
        Some(Phase::Lock) => Color32::from_rgb(160, 110, 240),
        Some(Phase::Toolchain | Phase::WaitForBuild) => Color32::from_rgb(130, 140, 160),
        Some(Phase::Compile) => Color32::from_rgb(255, 170, 40),
        Some(Phase::Link) => Color32::from_rgb(255, 250, 235),
        Some(Phase::Install | Phase::Launch) => Color32::from_rgb(70, 230, 120),
        Some(Phase::Prepare | Phase::Backup | Phase::Cleanup) | None => {
            Color32::from_rgb(150, 160, 180)
        }
    }
}

/// Blend a phase's base colour with the instance's accent, and add any slow breathing pulse.
fn phase_color(phase: Option<Phase>, accent: Color32, now: f64, animate: bool) -> Color32 {
    let base = phase_base_color(phase).lerp_to_gamma(accent, 0.3);
    match phase {
        Some(Phase::Toolchain | Phase::WaitForBuild) if animate => {
            let breathe = 0.78 + 0.22 * (now * TAU as f64 / 3.2).sin() as f32;
            base.gamma_multiply(breathe)
        }
        Some(Phase::Link) if animate => {
            let pulse = 0.88 + 0.12 * (now * TAU as f64 / 0.9).sin() as f32;
            base.gamma_multiply(pulse)
        }
        _ => base,
    }
}

/// Whether arcs/electrons crackle around the cube for this phase.
fn phase_is_energetic(phase: Option<Phase>) -> bool {
    matches!(phase, Some(Phase::Compile | Phase::Link))
}

const LINK_LINES: &[&str] = &[
    "Fat LTO: rustc is looking at the whole game at once...",
    "Stitching the voxel engine to the game...",
    "The linker is laying every wire at once.",
    "No percentage here: this part just takes a while.",
    "Final optimisation pass across every crate...",
];

// ---------------------------------------------------------------------------------------------
// Tiny deterministic PRNG (no external crate: xorshift32)
// ---------------------------------------------------------------------------------------------

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

    /// `0.0..1.0`.
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

/// Spiral order of the 9 cells in one 3x3 layer (clockwise from the top-left corner, ending in
/// the centre), as `(gx, gz)` with each in `0..3`.
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

/// The 27 voxel grid coordinates, in placement order (bottom layer first, each layer a spiral).
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

/// Ease-out "bounce": overshoots past 1.0 briefly, then settles, for a satisfying drop-in.
fn ease_out_bounce(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 1.0 {
        // A damped sine riding an ease-out curve: a quick drop that overshoots once.
        let decay = (-6.0 * t).exp();
        1.0 - decay * ((1.0 - t) * 9.0).cos()
    } else {
        1.0
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cube(
    ui: &mut Ui,
    rect: Rect,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
    finish_age: Option<f64>,
) {
    let painter = ui.painter_at(rect);
    let failed = matches!(
        view.outcome,
        Some(watt_core::events::OpOutcome::Failed { .. })
    );
    let cancelled = matches!(view.outcome, Some(watt_core::events::OpOutcome::Cancelled));
    let dimmed = failed || cancelled;

    // A per-op phase offset so every run starts at a different (non axis-aligned) angle, rather
    // than always opening with the same pose.
    let yaw_offset = (seed_from(&view.op_id, 0xA11CE) as f32 / u32::MAX as f32) * TAU;
    let yaw = if style.animate {
        yaw_offset + (now as f32 / ROTATE_SECS) * TAU
    } else {
        yaw_offset + 0.4
    };
    let tilt: f32 = -0.27;

    let short_side = rect.width().min(rect.height() * 1.6);
    let scale = (short_side * 0.14).clamp(14.0, 60.0);
    let cam = Camera {
        centre: Pos2::new(rect.center().x, rect.center().y - rect.height() * 0.06),
        scale,
        focal: 4.6,
    };

    let phase = view.current;
    let base_color = if failed {
        Color32::from_rgb(190, 70, 70)
    } else if cancelled {
        Color32::from_gray(120)
    } else {
        phase_color(phase, style.accent, now, style.animate)
    };
    let hot = matches!(phase, Some(Phase::Link));
    let light_dir = {
        let v = Vec3::new(0.35, 0.85, 0.38);
        v.normalized()
    };

    let order = voxel_order();
    // Spark burst position memory: pick a stable set of directions from the seed.
    let spark_seed = seed_from(&view.op_id, 0xF00D);

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
                let drop = if style.animate && age < PLACE_ANIM_SECS as f32 {
                    let t = age / PLACE_ANIM_SECS as f32;
                    (1.0 - ease_out_bounce(t)) * 2.4
                } else {
                    0.0
                };
                let settle = if dimmed {
                    // A few voxels slump when things go wrong, farther out ones more.
                    let slump_seed = seed_from(&view.op_id, k as u64);
                    let slumps = slump_seed.is_multiple_of(5);
                    if slumps { -0.18 } else { 0.0 }
                } else {
                    0.0
                };
                P3::new(gx, gy + drop + settle, gz)
            }
            None => P3::new(gx, gy, gz),
        };

        if placed_at.is_none() {
            // Ghost wireframe: draw the 12 edges of the unit cube, faint.
            for edge in CUBE_EDGES {
                let a = cube_vertex(center, edge.0).rotated(yaw, tilt);
                let b = cube_vertex(center, edge.1).rotated(yaw, tilt);
                ghost_lines.push([cam.project(a), cam.project(b)]);
            }
            continue;
        }

        let age = placed_at.map(|t0| (now - t0) as f32).unwrap_or(10.0);
        let flash = if style.animate {
            (1.0 - age / 0.35).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Indeterminate-phase brightness wave travelling through placed voxels.
        let wave = if style.animate && view.phase_fraction().is_none() && view.running() {
            let phase_pos = (gx + gy + gz) * 0.5;
            0.5 + 0.5 * (now as f32 * 2.2 - phase_pos * 1.6).sin()
        } else {
            1.0
        };

        for &(normal, face) in &CUBE_FACES {
            let n_rot = normal.rotated(yaw, tilt);
            if n_rot.z >= -0.02 {
                continue; // back-face cull: not visible to the camera.
            }
            let verts: [P3; 4] = face.map(|v| cube_vertex(center, v).rotated(yaw, tilt));
            let depth = (verts[0].z + verts[1].z + verts[2].z + verts[3].z) / 4.0;
            let nv = Vec3::new(n_rot.x, n_rot.y, n_rot.z);
            let diffuse = 0.35 + 0.65 * nv.dot(light_dir).max(0.0);
            let mut color = base_color.gamma_multiply(diffuse * wave);
            if flash > 0.0 {
                color = color.lerp_to_gamma(Color32::WHITE, flash);
            }
            if hot && style.animate {
                let glow = 1.0 + 0.1 * (now as f32 * 5.0 + depth).sin().max(0.0);
                color = color.gamma_multiply(glow);
            }
            let pts = verts.map(|v| cam.project(v));
            // At some rotations a side face is seen at an extreme grazing angle and projects to
            // a near-zero-area sliver (correct geometry, but reads as a stray bright hairline);
            // drop those rather than draw them.
            if !is_visibly_flat(&pts) {
                quads.push((depth, pts, color));
            }
        }
    }

    // Ghosts first (wireframe, order doesn't matter for lines). A neutral tint (rather than the
    // phase colour) keeps them readable as "not yet built" even during a near-white phase like
    // Link.
    let ghost_color = Color32::from_rgba_unmultiplied(170, 180, 195, if dimmed { 16 } else { 36 });
    for [a, b] in &ghost_lines {
        painter.add(egui::Shape::line_segment(
            [*a, *b],
            Stroke::new(1.0, ghost_color),
        ));
    }

    // Painter's algorithm: farthest (largest camera-space z) first.
    quads.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let edge_color = Color32::from_rgba_unmultiplied(10, 10, 14, 90);
    for (_, pts, color) in &quads {
        painter.add(egui::Shape::convex_polygon(
            pts.to_vec(),
            *color,
            Stroke::new(1.0, edge_color),
        ));
    }

    if style.animate && phase_is_energetic(phase) && !dimmed {
        draw_arcs(
            &painter,
            &cam,
            yaw,
            tilt,
            &view.op_id,
            state.tick,
            base_color,
        );
        draw_electrons(&painter, &cam, yaw, tilt, now, base_color);
    }

    if let Some(age) = finish_age
        && style.animate
        && age < SPARK_SECS
        && !dimmed
    {
        draw_sparks(
            &painter, &cam, yaw, tilt, age as f32, spark_seed, base_color,
        );
    }
}

/// A 3D vector, for lighting dot products (kept separate from [`P3`], a model-space point, so the
/// two aren't accidentally mixed up).
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

/// Half-extent of a voxel (full size `2 * VOXEL_H`), with a gap of `1.0 - 2*VOXEL_H` to its
/// neighbours in the 1-unit grid.
const VOXEL_H: f32 = 0.42;

/// The 8 corners of a unit cube, indexed 0..8 (bit 0 = +x, bit 1 = +y, bit 2 = +z).
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

/// The 12 edges of a unit cube, as pairs of corner indices (for ghost wireframes).
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

/// The 6 faces of a unit cube: outward normal (model space, unrotated) and its 4 corners in
/// winding order.
const CUBE_FACES: [(P3Lit, [u8; 4]); 6] = [
    (P3Lit(1.0, 0.0, 0.0), [1, 3, 7, 5]),
    (P3Lit(-1.0, 0.0, 0.0), [0, 4, 6, 2]),
    (P3Lit(0.0, 1.0, 0.0), [2, 3, 7, 6]),
    (P3Lit(0.0, -1.0, 0.0), [0, 1, 5, 4]),
    (P3Lit(0.0, 0.0, 1.0), [4, 5, 7, 6]),
    (P3Lit(0.0, 0.0, -1.0), [0, 2, 3, 1]),
];

/// A `const`-friendly stand-in for [`P3`] (which isn't itself usable in a `const` array because
/// of its rotation methods living in a non-const impl); converted with [`P3Lit::rotated`].
#[derive(Clone, Copy)]
struct P3Lit(f32, f32, f32);

impl P3Lit {
    fn rotated(self, yaw: f32, tilt: f32) -> P3 {
        P3::new(self.0, self.1, self.2).rotated(yaw, tilt)
    }
}

/// Jagged lightning arcs between random cube corners, regenerated every 70-120ms.
fn draw_arcs(
    painter: &egui::Painter,
    cam: &Camera,
    yaw: f32,
    tilt: f32,
    op_id: &str,
    tick: u64,
    color: Color32,
) {
    // Bucket time into ~90ms windows so the bolts hold still briefly rather than vibrating.
    let window = (tick / 6).max(1);
    let mut rng = Rng::new(seed_from(op_id, window));
    let n_arcs = 2 + (rng.next_u32() % 2) as usize;
    let extent = VOXEL_H * 3.0 + VOXEL_H; // reach just past the outer voxel faces.
    for _ in 0..n_arcs {
        let a_corner = rng.range(0.0, TAU);
        let a_h = rng.range(-1.0, 1.0);
        let b_corner = a_corner + rng.range(1.2, TAU - 1.2);
        let b_h = rng.range(-1.0, 1.0);
        let a = P3::new(
            extent * a_corner.cos(),
            extent * a_h,
            extent * a_corner.sin(),
        )
        .rotated(yaw, tilt);
        let b = P3::new(
            extent * b_corner.cos(),
            extent * b_h,
            extent * b_corner.sin(),
        )
        .rotated(yaw, tilt);
        let pa = cam.project(a);
        let pb = cam.project(b);
        let segments = 6;
        let mut points = Vec::with_capacity(segments + 1);
        points.push(pa);
        let dir = pb - pa;
        let perp = Vec2::new(-dir.y, dir.x).normalized();
        for s in 1..segments {
            let t = s as f32 / segments as f32;
            let jitter = rng.range(-10.0, 10.0) * (1.0 - (t - 0.5).abs() * 2.0).max(0.2);
            points.push(pa + dir * t + perp * jitter);
        }
        points.push(pb);
        let glow = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60);
        painter.add(egui::Shape::line(points.clone(), Stroke::new(5.0, glow)));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.6, Color32::WHITE.lerp_to_gamma(color, 0.2)),
        ));
    }
}

/// Small glowing dots orbiting the cube on tilted elliptical paths, with short fading trails.
fn draw_electrons(
    painter: &egui::Painter,
    cam: &Camera,
    yaw: f32,
    tilt: f32,
    now: f64,
    color: Color32,
) {
    const ORBITS: [(f32, f32, f32, f32); 3] = [
        // (radius, vertical squash, tilt offset, speed)
        (2.1, 0.5, 0.3, 1.3),
        (2.4, 0.35, -0.5, -0.9),
        (1.9, 0.6, 1.1, 1.7),
    ];
    for (i, &(r, squash, orbit_tilt, speed)) in ORBITS.iter().enumerate() {
        let phase = now as f32 * speed + i as f32 * 2.1;
        for trail in 0..5 {
            let t = phase - trail as f32 * 0.07;
            let p = P3::new(
                r * t.cos(),
                r * squash * t.sin() * orbit_tilt.cos(),
                r * t.sin() * orbit_tilt.sin(),
            )
            .rotated(yaw, tilt);
            let pos = cam.project(p);
            let alpha = ((5 - trail) as f32 / 5.0 * 200.0) as u8;
            let c = Color32::from_rgba_unmultiplied(
                color.r().saturating_add(40),
                color.g().saturating_add(40),
                color.b().saturating_add(40),
                alpha,
            );
            let radius = if trail == 0 { 3.2 } else { 1.6 };
            painter.add(egui::Shape::circle_filled(pos, radius, c));
        }
    }
}

/// A once-only burst of sparks radiating from the cube on a successful finish.
fn draw_sparks(
    painter: &egui::Painter,
    cam: &Camera,
    yaw: f32,
    tilt: f32,
    age: f32,
    seed: u32,
    color: Color32,
) {
    let mut rng = Rng::new(seed);
    let n = 22;
    let t = (age / SPARK_SECS as f32).clamp(0.0, 1.0);
    let fade = (1.0 - t).powf(1.5);
    for _ in 0..n {
        let dir_yaw = rng.range(0.0, TAU);
        let dir_h = rng.range(-0.7, 0.7);
        let speed = rng.range(2.0, 4.5);
        let dist = speed * t;
        let p =
            P3::new(dist * dir_yaw.cos(), dist * dir_h, dist * dir_yaw.sin()).rotated(yaw, tilt);
        let pos = cam.project(p);
        let c = Color32::from_rgba_unmultiplied(
            255,
            color.g().saturating_add(60),
            color.b(),
            (fade * 230.0) as u8,
        );
        painter.add(egui::Shape::circle_filled(pos, 2.2 * fade.max(0.2), c));
    }
}

// ---------------------------------------------------------------------------------------------
// Status block: headline, phase, percentage, bar, ticker, elapsed/estimate
// ---------------------------------------------------------------------------------------------

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn format_minutes(ms: u64) -> String {
    let mins = (ms + 30_000) / 60_000;
    if mins <= 1 {
        "about a minute".to_owned()
    } else {
        format!("about {mins} min")
    }
}

fn ellipsise(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push_str("...");
        out
    }
}

fn draw_status_block(ui: &mut Ui, view: &OpView, style: &ForgeStyle, now: f64) {
    let phase = view.current;
    let phase_col = phase_color(phase, style.accent, now, style.animate);

    match &view.outcome {
        Some(watt_core::events::OpOutcome::Ok { summary }) => {
            ui.vertical_centered(|ui| {
                ui.heading("Ready");
                ui.label(summary);
            });
            return;
        }
        Some(watt_core::events::OpOutcome::Cancelled) => {
            ui.vertical_centered(|ui| {
                ui.heading("Cancelled");
                ui.label("Stopped on request.");
            });
            return;
        }
        Some(watt_core::events::OpOutcome::Failed { phase, error, .. }) => {
            ui.vertical_centered(|ui| {
                let where_ = phase.map(|p| p.title()).unwrap_or("the run");
                ui.heading(format!("Failed in {where_}"));
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("forge-error")
                .max_height(90.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(error.as_str()).monospace())
                            .selectable(true)
                            .wrap(),
                    );
                });
            return;
        }
        None => {}
    }

    ui.vertical_centered(|ui| {
        let verb = view
            .op
            .map(watt_core::events::OpKind::verb)
            .unwrap_or("Working on");
        ui.heading(format!("{verb} {}", style.title));
        ui.colored_label(
            phase_col,
            phase
                .map(|p| p.title().to_owned())
                .or_else(|| view.phases.last().map(|p| p.title.clone()))
                .unwrap_or_default(),
        );

        let fraction = view.fraction();
        ui.label(
            egui::RichText::new(format!("{:.0}%", fraction * 100.0))
                .size(28.0)
                .strong(),
        );

        let bar_rect = ui
            .allocate_exact_size(vec2(ui.available_width().min(420.0), 10.0), Sense::hover())
            .0;
        draw_glow_bar(ui, bar_rect, fraction, phase_col, style.animate, now);

        if phase == Some(Phase::Compile)
            && let Some((done, Some(total))) = view.progress
        {
            ui.label(format!("{done} / {total} crates"));
        }

        if !view.detail.is_empty() {
            ui.label(ellipsise(&view.detail, 70));
        }

        if phase == Some(Phase::Link) {
            let idx = ((now / 6.0) as usize) % LINK_LINES.len();
            ui.label(egui::RichText::new(LINK_LINES[idx]).italics());
        }

        ui.horizontal(|ui| {
            ui.label(format!("Elapsed {}", format_duration(view.t_ms)));
            if let Some(expected) = style.expected_ms {
                if fraction < 0.001 {
                    ui.label(format!("last build took {}", format_minutes(expected)));
                } else if fraction >= 0.05 {
                    let estimate_total = view.t_ms as f32 / fraction;
                    let remaining = (estimate_total - view.t_ms as f32).max(0.0) as u64;
                    ui.label(format!("{} left", format_minutes(remaining)));
                }
            }
        });
    });
}

fn draw_glow_bar(ui: &mut Ui, rect: Rect, fraction: f32, color: Color32, animate: bool, now: f64) {
    let painter = ui.painter_at(rect);
    let bg = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    painter.add(egui::Shape::rect_filled(rect, rect.height() / 2.0, bg));
    let fill_w = rect.width() * fraction.clamp(0.0, 1.0);
    if fill_w > 1.0 {
        let fill_rect = Rect::from_min_size(rect.min, vec2(fill_w, rect.height()));
        let glow_alpha = if animate {
            (160.0 + 60.0 * (now * 4.0).sin() as f32) as u8
        } else {
            200
        };
        let glow = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), glow_alpha);
        painter.add(egui::Shape::rect_filled(
            fill_rect,
            rect.height() / 2.0,
            glow,
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// Phase checklist
// ---------------------------------------------------------------------------------------------

fn draw_phase_checklist(ui: &mut Ui, view: &OpView, style: &ForgeStyle) {
    if view.phases.is_empty() {
        return;
    }
    // Below a certain width, chips in a row no longer fit; fall back to a compact vertical list
    // rather than relying on a horizontal scrollbar the user may not notice.
    if ui.available_width() < 650.0 {
        egui::ScrollArea::vertical()
            .id_salt("forge-phase-list")
            .max_height(110.0)
            .show(ui, |ui| {
                for phase_view in &view.phases {
                    draw_phase_chip(ui, phase_view, style);
                }
            });
        return;
    }
    egui::ScrollArea::horizontal()
        .id_salt("forge-phase-chips")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for phase_view in &view.phases {
                    draw_phase_chip(ui, phase_view, style);
                    ui.add_space(4.0);
                }
            });
        });
}

fn draw_phase_chip(ui: &mut Ui, phase_view: &watt_core::view::PhaseView, style: &ForgeStyle) {
    let now = ui.ctx().input(|i| i.time);
    let (label, color, strong) = match phase_view.state {
        PhaseState::Pending => (phase_view.title.clone(), Color32::from_gray(110), false),
        PhaseState::Running => {
            let marker = if style.animate {
                spinner_glyph(now)
            } else {
                '*'
            };
            (
                format!("{marker} {}", phase_view.title),
                phase_color(Some(phase_view.phase), style.accent, now, style.animate),
                true,
            )
        }
        PhaseState::Done(outcome) => {
            let secs = phase_view.duration_ms as f32 / 1000.0;
            let tag = match outcome {
                watt_core::events::PhaseOutcome::Ok => format!("[done] {:.1}s", secs),
                watt_core::events::PhaseOutcome::Cached => "cached".to_owned(),
                watt_core::events::PhaseOutcome::Skipped => "skipped".to_owned(),
                watt_core::events::PhaseOutcome::Failed => "[failed]".to_owned(),
            };
            let col = if outcome == watt_core::events::PhaseOutcome::Failed {
                Color32::from_rgb(220, 90, 90)
            } else {
                Color32::from_gray(150)
            };
            (format!("{} {tag}", phase_view.title), col, false)
        }
    };
    let text = if strong {
        egui::RichText::new(label).color(color).strong()
    } else {
        egui::RichText::new(label).color(color)
    };
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| ui.label(text));
}

fn spinner_glyph(now: f64) -> char {
    const GLYPHS: [char; 4] = ['-', '\\', '|', '/'];
    GLYPHS[((now * 6.0) as usize) % GLYPHS.len()]
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

fn draw_buttons(ui: &mut Ui, view: &OpView) -> ForgeResponse {
    let mut response = ForgeResponse::default();
    ui.horizontal(|ui| match &view.outcome {
        None => {
            if ui.button("Cancel").clicked() {
                response.cancel = true;
            }
            if ui.button("Hide").clicked() {
                response.hide = true;
            }
        }
        Some(watt_core::events::OpOutcome::Ok { .. }) => {
            if matches!(
                view.op,
                Some(
                    watt_core::events::OpKind::Build
                        | watt_core::events::OpKind::Update
                        | watt_core::events::OpKind::Mods
                        | watt_core::events::OpKind::Create
                )
            ) && ui.button("Play").clicked()
            {
                response.play = true;
            }
            if ui.button("Close").clicked() {
                response.close = true;
            }
        }
        Some(watt_core::events::OpOutcome::Cancelled) => {
            if ui.button("Close").clicked() {
                response.close = true;
            }
        }
        Some(watt_core::events::OpOutcome::Failed { recovery, .. }) => {
            for r in recovery {
                if ui.button(recovery_label(*r)).clicked() {
                    response.recovery = Some(*r);
                }
            }
            if ui.button("Close").clicked() {
                response.close = true;
            }
        }
    });
    response
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
// Log section
// ---------------------------------------------------------------------------------------------

fn draw_log_section(ui: &mut Ui, view: &OpView) {
    egui::CollapsingHeader::new("Log")
        .id_salt("forge-log")
        .default_open(false)
        .show(ui, |ui| {
            let start = view.log.len().saturating_sub(200);
            egui::ScrollArea::vertical()
                .id_salt("forge-log-scroll")
                .max_height(160.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in view.log.iter().skip(start) {
                        ui.monospace(line);
                    }
                });
        });
}

/// A small isometric cube icon (instance avatars, the logo). `busy` makes it spin and crackle.
pub fn cube_icon(ui: &mut Ui, size: f32, color: Color32, busy: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter_at(rect);
    let now = ui.ctx().input(|i| i.time);
    let yaw = if busy {
        (now as f32) * (TAU / 2.5)
    } else {
        0.6
    };
    let tilt: f32 = -0.3;
    let cam = Camera {
        centre: rect.center(),
        scale: size * 0.30,
        focal: 4.0,
    };
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
    if busy {
        let mut rng = Rng::new(((now * 10.0) as u32).wrapping_mul(2_654_435_761) | 1);
        if rng.next_f32() < 0.6 {
            let extent = VOXEL_H * 2.2;
            let a_corner = rng.range(0.0, TAU);
            let b_corner = a_corner + rng.range(1.0, TAU - 1.0);
            let a = cam.project(
                P3::new(
                    extent * a_corner.cos(),
                    extent * rng.range(-1.0, 1.0),
                    extent * a_corner.sin(),
                )
                .rotated(yaw, tilt),
            );
            let b = cam.project(
                P3::new(
                    extent * b_corner.cos(),
                    extent * rng.range(-1.0, 1.0),
                    extent * b_corner.sin(),
                )
                .rotated(yaw, tilt),
            );
            painter.add(egui::Shape::line_segment(
                [a, b],
                Stroke::new(1.4, Color32::WHITE.lerp_to_gamma(color, 0.2)),
            ));
        }
        ui.ctx().request_repaint();
    }
    response
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
        scale: size as f32 * 0.30,
        focal: 4.0,
    };
    let yaw = 0.6_f32;
    let tilt = -0.3_f32;
    let center = P3::new(0.0, 0.0, 0.0);
    let light_dir = Vec3::new(0.35, 0.85, 0.38).normalized();

    // Build the three visible faces as polygons with flat colour, then rasterise them with a
    // simple point-in-triangle scan (no tessellation crate available here).
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

    // A simple lightning-bolt glyph cut through the centre: a thin zigzag, drawn in a bright
    // colour (not cut as a hole, to stay legible at small sizes).
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

/// Whether a projected quad is so thin (an extreme grazing-angle sliver) that it should be
/// skipped rather than drawn, comparing its shoelace area against its longest diagonal squared.
fn is_visibly_flat(pts: &[Pos2; 4]) -> bool {
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

/// A thick line drawn directly into an RGBA buffer (for the window icon, where there's no
/// painter to use).
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
