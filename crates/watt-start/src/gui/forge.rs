//! The Forge: build and update progress, drawn in the instance page's content area.
//!
//! The voxel cube is the launcher's mark. It turns once every 14 s, voxels drop in as the op
//! advances, and a heat model (white-hot on landing, a heartbeat on the newest voxel, molten
//! during link) replaces the old flash. Success eases the cube onto a quarter-turn and sweeps
//! a shine across it. With animations off nothing moves and a running op ticks once a second.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, TAU};
use std::time::Duration;

use egui::{Color32, Id, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, vec2};
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
    /// The catalog says this instance can launch. False while it still needs a build, and for
    /// an instance that is not in the catalog yet.
    pub ready: bool,
    /// A failed Duplicate only offers Retry. Roll back and rebuild would hit the source instance.
    pub retry_only: bool,
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
    /// "Play" after a successful build/update that left the instance ready.
    pub play: bool,
    /// "Build now" after a success that still needs a build.
    pub build: bool,
}

/// Max width of the status column under the cube.
const CONTENT_MAX_WIDTH: f32 = 640.0;
/// Side inset so nothing sits on the content edge.
const SIDE_INSET: f32 = 16.0;
/// How many voxels make up the cube (3x3x3).
pub(crate) const N_VOXELS: usize = 27;
/// Drop-in duration. Ease-out cubic, no bounce.
const DROP_SECS: f32 = 0.25;
/// How far a voxel falls, in grid units.
const DROP_HEIGHT: f32 = 1.2;
/// Heat fades from white-hot to the floor over this long after landing.
const HEAT_COOL_SECS: f32 = 0.9;
/// Gap between voxels that cross the line in the same frame. Capped at [`CASCADE_MAX`].
const CASCADE_GAP: f64 = 0.030;
/// Longest cascade delay, so a jump to finished still resolves within a second.
const CASCADE_MAX: f64 = 0.8;
/// Success spark burst.
const SPARK_SECS: f64 = 0.8;
/// Shine sweep length, starting `SHINE_DELAY` after the last voxel lands.
const SHINE_SECS: f64 = 0.6;
/// Pause after the last landing before the shine and the sparks.
const SHINE_DELAY: f64 = 0.1;
const SPARK_DELAY: f64 = 0.2;
/// One full turn of the cube.
pub(crate) const ROTATE_SECS: f32 = 14.0;
/// Resting yaw: three faces, not aligned to an axis.
pub(crate) const BASE_YAW: f32 = 0.62;
/// Fixed camera tilt, about 25°. Negative puts the eye above the cube.
pub(crate) const TILT: f32 = -0.44;
/// Pinhole distance. Far enough that the outer voxels diverge by only a few degrees.
pub(crate) const FOCAL: f32 = 10.0;
/// Half-extent of a voxel. The gap to the next cell is `1 - 2 * VOXEL_H`.
pub(crate) const VOXEL_H: f32 = 0.42;
/// Yaw ease onto the nearest quarter-turn after the op ends.
const SETTLE_SECS: f64 = 0.6;
/// Continuous repaint window after the outcome, covering the cascade and the sweep.
const AFTER_SETTLE_SECS: f64 = 1.6;
/// Plain rotation while a build runs. 30 fps is smooth for a 14 s turn.
const SPIN_FRAME: Duration = Duration::from_millis(33);
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
    /// A time in the future is a cascade slot that has not started falling.
    voxel_time: [Option<f64>; N_VOXELS],
    /// `ctx` time the outcome was first seen.
    settled_at: Option<f64>,
    /// Set when a failure is first shown, so the log opens once.
    log_opened_for_failure: bool,
    /// `ctx` time Link was first seen. Ember phase is `now - link_since`.
    link_since: Option<f64>,
    /// Yaw captured when the outcome appeared. Eased toward the nearest quarter-turn.
    settle_yaw_from: f32,
    /// `now - spin_origin` is the spin clock while the window is visible.
    spin_origin: f64,
    /// Spin clock frozen while the window is hidden.
    spin_frozen: Option<f64>,
    /// `ctx` time that maps to settle-ease 0. Shifted across hidden gaps.
    settle_origin: f64,
    /// Settle ease frozen in 0..=1 while the window is hidden.
    settle_frozen_u: Option<f32>,
}

impl ForgeState {
    fn fresh(op_id: String) -> Self {
        Self {
            op_id,
            voxel_time: [None; N_VOXELS],
            settled_at: None,
            log_opened_for_failure: false,
            link_since: None,
            settle_yaw_from: BASE_YAW,
            spin_origin: 0.0,
            spin_frozen: None,
            settle_origin: 0.0,
            settle_frozen_u: None,
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
    let hidden = viewport_hidden(ui.ctx());
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
            state.settle_yaw_from = if style.animate {
                spin_yaw(&mut state, now, hidden)
            } else {
                BASE_YAW
            };
            state.settled_at = Some(now);
            state.settle_origin = now;
            state.settle_frozen_u = None;
        }
    } else {
        state.settled_at = None;
        state.settle_frozen_u = None;
    }
    if view.running() && view.current == Some(Phase::Link) && state.link_since.is_none() {
        state.link_since = Some(now);
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
                    draw_cube(ui, cube_rect, view, style, &mut state, now, hidden);

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
                        response = draw_buttons(ui, view, style);
                        ui.add_space(layout.before_log);
                        draw_log(ui, view, failed);
                    });
                    ui.add_space(layout.tail);
                });
        });

    request_motion_repaint(ui, view, style, &state, now, hidden);
    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
    response
}

/// Record when each voxel first becomes visible. Placements stick.
///
/// Several thresholds crossed in one frame cascade: voxel `i` starts `i * 30 ms` later, and
/// never more than [`CASCADE_MAX`] out. With animation off the time is back-dated so the drop
/// is already over.
fn update_voxel_times(state: &mut ForgeState, fraction: f32, now: f64, animate: bool) {
    let mut wave = 0u32;
    for k in 0..N_VOXELS {
        let threshold = (k + 1) as f32 / N_VOXELS as f32;
        if fraction >= threshold && state.voxel_time[k].is_none() {
            state.voxel_time[k] = Some(if animate {
                let delay = (f64::from(wave) * CASCADE_GAP).min(CASCADE_MAX);
                now + delay
            } else {
                now - f64::from(DROP_SECS) - 1.0
            });
            wave += 1;
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

fn viewport_hidden(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        let viewport = i.viewport();
        viewport.focused == Some(false)
            || viewport.minimized == Some(true)
            || viewport.occluded == Some(true)
    })
}

fn request_motion_repaint(
    ui: &Ui,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
    hidden: bool,
) {
    let fast = style.animate && motion_is_fast(view, state, now);
    if hidden {
        // The yaw stays put. A 1 s tick keeps the elapsed clock honest while a build runs
        // and lets a finish animation complete without a display-rate wake.
        if view.running() || fast {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        return;
    }
    if !style.animate {
        if view.running() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        return;
    }
    if fast {
        ui.ctx().request_repaint();
    } else if view.running() {
        ui.ctx().request_repaint_after(SPIN_FRAME);
    }
}

/// Drops, cooling metal, embers, the post-settle choreography, and the yaw ease.
/// A plain turn (and the slow heartbeat) is not fast: that runs at [`SPIN_FRAME`].
fn motion_is_fast(view: &OpView, state: &ForgeState, now: f64) -> bool {
    let dropping = state.voxel_time.iter().flatten().any(|t0| {
        let age = now - *t0;
        (0.0..f64::from(DROP_SECS)).contains(&age)
    });
    let cooling = state.voxel_time.iter().flatten().any(|t0| {
        let since_land = now - (*t0 + f64::from(DROP_SECS));
        if since_land < 0.0 {
            return false;
        }
        let u = (since_land / f64::from(HEAT_COOL_SECS)) as f32;
        1.0 - u * u > 0.01
    });
    let embers = view.running() && view.current == Some(Phase::Link);
    let since_settle = state.settled_at.map(|t0| now - t0);
    let after_settle = since_settle.is_some_and(|age| (0.0..AFTER_SETTLE_SECS).contains(&age));
    let yaw_settling = since_settle.is_some_and(|age| (0.0..SETTLE_SECS).contains(&age));
    // A full 27-voxel cascade lands late enough that the shine and sparks outlast the 1.6 s window.
    let finale = matches!(view.outcome, Some(OpOutcome::Ok { .. }))
        && last_land(state).is_some_and(|land| {
            let shine_end = land + SHINE_DELAY + SHINE_SECS;
            let spark_end = land + SPARK_DELAY + SPARK_SECS;
            now < shine_end.max(spark_end)
        });
    dropping || cooling || embers || after_settle || yaw_settling || finale
}

/// `ctx` time the last scheduled voxel reaches its slot, including ones still in the future.
fn last_land(state: &ForgeState) -> Option<f64> {
    state
        .voxel_time
        .iter()
        .flatten()
        .copied()
        .max_by(f64::total_cmp)
        .map(|t0| t0 + f64::from(DROP_SECS))
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

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let u = -2.0 * t + 2.0;
        1.0 - u * u * u / 2.0
    }
}

/// Darken RGB and leave alpha alone, so a shaded face stays solid metal.
fn shade(color: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    Color32::from_rgba_unmultiplied(
        (f32::from(r) * factor + 0.5) as u8,
        (f32::from(g) * factor + 0.5) as u8,
        (f32::from(b) * factor + 0.5) as u8,
        a,
    )
}

// ---------------------------------------------------------------------------------------------
// Cube geometry. Pure functions: the yaw-sweep test calls these without an egui context.
//
// Visibility is the sign of (eye − face plane), not a view-space normal test. A face's
// projected width hits zero at the same yaw it stops being drawn, so nothing can pop.
// ---------------------------------------------------------------------------------------------

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

const fn voxel_grid() -> [(i32, i32, i32); N_VOXELS] {
    let mut out = [(0, 0, 0); N_VOXELS];
    let mut k = 0;
    let mut gy = 0;
    while gy < 3 {
        let mut s = 0;
        while s < 9 {
            let (gx, gz) = LAYER_SPIRAL[s];
            out[k] = (gx, gy, gz);
            k += 1;
            s += 1;
        }
        gy += 1;
    }
    out
}

/// Grid coordinates, bottom layer first, each layer a spiral. Index `k` is the placement order.
const GRID: [(i32, i32, i32); N_VOXELS] = voxel_grid();

const fn lattice_table() -> [[[usize; 3]; 3]; 3] {
    let mut idx = [[[0usize; 3]; 3]; 3];
    let mut k = 0;
    while k < N_VOXELS {
        let (gx, gy, gz) = GRID[k];
        idx[gy as usize][gz as usize][gx as usize] = k;
        k += 1;
    }
    idx
}

/// `LATTICE[y][z][x]` is the voxel index.
const LATTICE: [[[usize; 3]; 3]; 3] = lattice_table();

/// Four corners of each face. Bit 0 = +x, bit 1 = +y, bit 2 = +z.
///
/// Order is the outward winding. Screen y points down, so a front-facing quad has a
/// positive shoelace area (clockwise). Anything else is reversed before it is stroked.
const FACE_CORNERS: [[u8; 4]; 6] = [
    [1, 3, 7, 5], // +x
    [0, 4, 6, 2], // -x
    [2, 3, 7, 6], // +y
    [0, 1, 5, 4], // -y
    [4, 5, 7, 6], // +z
    [0, 2, 3, 1], // -z
];

const FACE_NORMAL: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
];

/// Placed-face stroke. Dark enough to read as a crisp edge on Volt and on BAD.
const PLACED_STROKE: Color32 = Color32::from_rgba_unmultiplied_const(10, 10, 14, 70);
/// Glass hull of a slot that has not been placed yet.
const GHOST_FILL: Color32 = Color32::from_rgba_unmultiplied_const(170, 180, 195, 14);
const GHOST_STROKE: Color32 = Color32::from_rgba_unmultiplied_const(170, 180, 195, 40);
/// Below this projected altitude a face is filled as a mesh. egui's feathered
/// polygon miters without a limit, and a sliver throws that miter off the cube.
const THIN_FACE_PX: f32 = 3.0;

/// Resting centres of the 27 voxels, in placement order.
pub(crate) fn rest_centers() -> [[f32; 3]; N_VOXELS] {
    std::array::from_fn(|k| grid_center(k, 0.0))
}

fn grid_center(k: usize, y_off: f32) -> [f32; 3] {
    let (gx, gy, gz) = GRID[k];
    [gx as f32 - 1.0, gy as f32 - 1.0 + y_off, gz as f32 - 1.0]
}

/// Eye in model space: the inverse of yaw-then-tilt applied to view-space `(0, 0, -focal)`.
///
/// With `tilt < 0`, `eye[1]` is positive (the camera looks down on the cube).
pub(crate) fn eye_model(focal: f32, yaw: f32, tilt: f32) -> [f32; 3] {
    let (sy, cy) = yaw.sin_cos();
    let (st, ct) = tilt.sin_cos();
    [focal * ct * sy, -focal * st, -focal * ct * cy]
}

/// Visible faces of one voxel. Bits 0,2,4 are +x,+y,+z; bits 1,3,5 are the negatives.
///
/// A face is drawn only while the eye is strictly outside its plane, so the projected
/// width is zero at the frame the face disappears.
pub(crate) fn visible_face_mask(eye: [f32; 3], center: [f32; 3]) -> u8 {
    let mut mask = 0u8;
    for axis in 0..3 {
        let e = eye[axis];
        let c = center[axis];
        if e > c + VOXEL_H {
            mask |= 1 << (axis * 2);
        } else if e < c - VOXEL_H {
            mask |= 1 << (axis * 2 + 1);
        }
    }
    mask
}

/// Far-to-near voxel order.
///
/// Key is the L1 distance from the eye to the voxel centre in model space, descending,
/// ties broken by index. A ray from the eye is monotone on each axis, so if it hits A
/// before B then B's cell is at least as far on every axis and strictly farther on one.
/// Equal keys do not overlap on screen. The same argument holds while a voxel is dropping:
/// one column is split by a y-plane, and another column's x/z term dominates a drop of 1.2.
pub(crate) fn painter_order(eye: [f32; 3], centers: &[[f32; 3]; N_VOXELS]) -> [usize; N_VOXELS] {
    let mut keys = [0.0f32; N_VOXELS];
    for (i, center) in centers.iter().enumerate() {
        keys[i] =
            (center[0] - eye[0]).abs() + (center[1] - eye[1]).abs() + (center[2] - eye[2]).abs();
    }
    let mut order = [0usize; N_VOXELS];
    for (i, slot) in order.iter_mut().enumerate() {
        *slot = i;
    }
    order.sort_by(|&a, &b| keys[b].total_cmp(&keys[a]).then(a.cmp(&b)));
    order
}

/// Absolute projected area of one face at scale 1. Zero exactly when the eye lies in the plane.
// The snapshot crate calls this; the binary does not.
#[allow(dead_code)]
pub(crate) fn face_projected_area(yaw: f32, center: [f32; 3], face: usize) -> f32 {
    let pts = project_face(unit_proj(yaw), center, face);
    shoelace(&pts).abs() * 0.5
}

/// `true` when this face looks out of the 3×3×3 (no cell, placed or ghost, on that side).
pub(crate) fn is_hull_face(voxel: usize, face: usize) -> bool {
    neighbor_index(voxel, face).is_none()
}

/// Vertical extent, in pixels at scale 1, of the outer ±1.42 box at yaw π/4.
pub(crate) fn vertical_extent() -> f32 {
    let edge = 1.0 + VOXEL_H;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let cam = unit_proj(FRAC_PI_4);
    for sx in [-1.0_f32, 1.0] {
        for sy in [-1.0_f32, 1.0] {
            for sz in [-1.0_f32, 1.0] {
                let y = project_point(cam, [sx * edge, sy * edge, sz * edge])[1];
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    (max_y - min_y).max(0.001)
}

fn nearest_rest_yaw(yaw: f32) -> f32 {
    let k = ((yaw - BASE_YAW) / FRAC_PI_2).round();
    BASE_YAW + k * FRAC_PI_2
}

/// Pinhole used by [`project_point`]. Bundled so the projectors stay under the arg limit.
#[derive(Clone, Copy)]
struct Proj {
    focal: f32,
    yaw: f32,
    tilt: f32,
    scale: f32,
    cx: f32,
    cy: f32,
}

/// Forge camera at scale 1, origin at (0, 0). Used by extent and the area test.
fn unit_proj(yaw: f32) -> Proj {
    Proj {
        focal: FOCAL,
        yaw,
        tilt: TILT,
        scale: 1.0,
        cx: 0.0,
        cy: 0.0,
    }
}

fn rotate_point(p: [f32; 3], yaw: f32, tilt: f32) -> [f32; 3] {
    let (sy, cy) = yaw.sin_cos();
    let x1 = p[0] * cy + p[2] * sy;
    let z1 = -p[0] * sy + p[2] * cy;
    let (st, ct) = tilt.sin_cos();
    [x1, p[1] * ct - z1 * st, p[1] * st + z1 * ct]
}

fn project_point(cam: Proj, p: [f32; 3]) -> [f32; 2] {
    let rotated = rotate_point(p, cam.yaw, cam.tilt);
    let depth = (cam.focal / (cam.focal + rotated[2])).max(0.1);
    [
        cam.cx + rotated[0] * cam.scale * depth,
        cam.cy - rotated[1] * cam.scale * depth,
    ]
}

fn project_face(cam: Proj, center: [f32; 3], face: usize) -> [[f32; 2]; 4] {
    FACE_CORNERS[face].map(|bits| project_point(cam, voxel_corner(center, bits)))
}

fn voxel_corner(center: [f32; 3], bits: u8) -> [f32; 3] {
    [
        center[0] + if bits & 1 != 0 { VOXEL_H } else { -VOXEL_H },
        center[1] + if bits & 2 != 0 { VOXEL_H } else { -VOXEL_H },
        center[2] + if bits & 4 != 0 { VOXEL_H } else { -VOXEL_H },
    ]
}

fn shoelace(pts: &[[f32; 2]; 4]) -> f32 {
    let mut area = 0.0f32;
    for i in 0..4 {
        let p = pts[i];
        let q = pts[(i + 1) % 4];
        area += p[0] * q[1] - q[0] * p[1];
    }
    area
}

/// Positive shoelace in screen space is clockwise, which is the winding egui feathers cleanly.
fn orient_clockwise(pts: &mut [[f32; 2]; 4]) {
    if shoelace(pts) < 0.0 {
        pts.reverse();
    }
}

fn neighbor_index(voxel: usize, face: usize) -> Option<usize> {
    let (mut gx, mut gy, mut gz) = GRID[voxel];
    let delta = if face.is_multiple_of(2) { 1 } else { -1 };
    match face / 2 {
        0 => gx += delta,
        1 => gy += delta,
        _ => gz += delta,
    }
    if (0..3).contains(&gx) && (0..3).contains(&gy) && (0..3).contains(&gz) {
        Some(LATTICE[gy as usize][gz as usize][gx as usize])
    } else {
        None
    }
}

fn face_view_depth(yaw: f32, tilt: f32, center: [f32; 3], face: usize) -> f32 {
    let mut z = 0.0f32;
    for bits in FACE_CORNERS[face] {
        z += rotate_point(voxel_corner(center, bits), yaw, tilt)[2];
    }
    z / 4.0
}

fn diffuse_for(face: usize, yaw: f32, tilt: f32) -> f32 {
    let normal = rotate_point(FACE_NORMAL[face], yaw, tilt);
    // (0.35, 0.85, 0.38) is already unit length to well under a percent.
    let len = (0.35f32 * 0.35 + 0.85 * 0.85 + 0.38 * 0.38).sqrt();
    let dot = (normal[0] * 0.35 + normal[1] * 0.85 + normal[2] * 0.38) / len;
    0.38 + 0.62 * dot.max(0.0)
}

/// Scale that keeps the silhouette inside the band, and never above the spec size.
fn cube_scale(rect: Rect) -> f32 {
    let short = rect.width().min(rect.height());
    let spec = (short * 0.14).clamp(14.0, 48.0);
    let fitted = (rect.height() - 36.0) / vertical_extent();
    spec.min(fitted.max(1.0))
}

fn draw_cube(
    ui: &mut Ui,
    rect: Rect,
    view: &OpView,
    style: &ForgeStyle,
    state: &mut ForgeState,
    now: f64,
    hidden: bool,
) {
    let p = theme::palette(ui.ctx());
    let painter = ui.painter_at(rect);
    let yaw = cube_yaw(style, view, state, now, hidden);
    let cam = FrameCam {
        yaw,
        scale: cube_scale(rect),
        cx: rect.center().x,
        cy: rect.center().y,
    };
    let linking = view.running() && view.current == Some(Phase::Link);
    if linking {
        draw_link_glow(&painter, rect, now, style.animate, &p);
    }

    let mut scratch = take_scratch(ui, &view.op_id);
    if scratch.faces.capacity() < 81 {
        scratch.faces.reserve(81);
    }
    scratch.faces.clear();
    build_cube_faces(ui, view, style, state, now, cam, &mut scratch.faces);
    for face in &scratch.faces {
        paint_voxel_face(&painter, face);
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(scratch_id(&view.op_id), scratch));

    if linking
        && style.animate
        && let Some(since) = state.link_since
    {
        draw_embers(&painter, rect, cam, now - since, &view.op_id, &p);
    }
    draw_success_sparks(&painter, rect, view, style, state, now, cam);
}

/// Spin clock in seconds. Absolute `ctx` time when the window has never been hidden
/// (`spin_origin` starts at 0), so a snapshot at time T matches `BASE_YAW + T / ROTATE_SECS * TAU`.
fn spin_secs(state: &mut ForgeState, now: f64, hidden: bool) -> f64 {
    if hidden {
        if state.spin_frozen.is_none() {
            state.spin_frozen = Some(now - state.spin_origin);
        }
        return state.spin_frozen.unwrap_or(0.0);
    }
    if let Some(frozen) = state.spin_frozen.take() {
        state.spin_origin = now - frozen;
    }
    now - state.spin_origin
}

fn spin_yaw(state: &mut ForgeState, now: f64, hidden: bool) -> f32 {
    let t = spin_secs(state, now, hidden) as f32;
    BASE_YAW + (t / ROTATE_SECS) * TAU
}

fn settle_u(state: &mut ForgeState, now: f64, hidden: bool) -> f32 {
    let raw = ((now - state.settle_origin) / SETTLE_SECS) as f32;
    let raw = raw.clamp(0.0, 1.0);
    if hidden {
        if state.settle_frozen_u.is_none() {
            state.settle_frozen_u = Some(raw);
        }
        return state.settle_frozen_u.unwrap_or(raw);
    }
    if let Some(frozen) = state.settle_frozen_u.take() {
        state.settle_origin = now - f64::from(frozen) * SETTLE_SECS;
        return frozen;
    }
    raw
}

/// Yaw of the turn, or the ease onto the nearest quarter-turn after the op ends.
/// Static at [`BASE_YAW`] when animations are off.
fn cube_yaw(
    style: &ForgeStyle,
    view: &OpView,
    state: &mut ForgeState,
    now: f64,
    hidden: bool,
) -> f32 {
    if !style.animate {
        return BASE_YAW;
    }
    if view.running() || state.settled_at.is_none() {
        return spin_yaw(state, now, hidden);
    }
    let from = state.settle_yaw_from;
    let to = nearest_rest_yaw(from);
    from + (to - from) * ease_out_cubic(settle_u(state, now, hidden))
}

#[derive(Clone, Copy)]
struct FrameCam {
    yaw: f32,
    scale: f32,
    cx: f32,
    cy: f32,
}

impl FrameCam {
    fn proj(self) -> Proj {
        Proj {
            focal: FOCAL,
            yaw: self.yaw,
            tilt: TILT,
            scale: self.scale,
            cx: self.cx,
            cy: self.cy,
        }
    }

    fn project(self, p: [f32; 3]) -> [f32; 2] {
        project_point(self.proj(), p)
    }

    fn face(self, center: [f32; 3], face: usize) -> [[f32; 2]; 4] {
        project_face(self.proj(), center, face)
    }
}

#[derive(Clone, Copy)]
struct PaintFace {
    pts: [Pos2; 4],
    fill: Color32,
    stroke: Color32,
}

#[derive(Clone, Default)]
struct CubeScratch {
    faces: Vec<PaintFace>,
}

fn scratch_id(op_id: &str) -> Id {
    Id::unique("watt-forge-cube-scratch").with(op_id)
}

fn take_scratch(ui: &Ui, op_id: &str) -> CubeScratch {
    let id = scratch_id(op_id);
    ui.ctx().data_mut(|d| d.remove_temp(id).unwrap_or_default())
}

fn push_face(out: &mut Vec<PaintFace>, mut pts: [[f32; 2]; 4], fill: Color32, stroke: Color32) {
    orient_clockwise(&mut pts);
    out.push(PaintFace {
        pts: pts.map(|p| Pos2::new(p[0], p[1])),
        fill,
        stroke,
    });
}

/// Altitude onto the longest edge. A square face seen edge-on collapses toward zero.
fn face_altitude(pts: &[Pos2; 4]) -> f32 {
    let mut twice_area = 0.0f32;
    let mut longest = 0.0f32;
    for i in 0..4 {
        let p = pts[i];
        let q = pts[(i + 1) % 4];
        twice_area += p.x * q.y - q.x * p.y;
        longest = longest.max((q - p).length());
    }
    if longest < 1.0 {
        return 0.0;
    }
    twice_area.abs() / longest
}

fn paint_voxel_face(painter: &egui::Painter, face: &PaintFace) {
    if face_altitude(&face.pts) < THIN_FACE_PX {
        for shape in thin_face_shapes(face) {
            painter.add(shape);
        }
    } else {
        painter.add(Shape::convex_polygon(
            face.pts.to_vec(),
            face.fill,
            Stroke::new(1.0, face.stroke),
        ));
    }
}

/// Fill that stays inside the quad, plus one stroke per edge (no miter join).
fn thin_face_shapes(face: &PaintFace) -> Vec<Shape> {
    let mut mesh = egui::Mesh::default();
    for p in face.pts {
        mesh.colored_vertex(p, face.fill);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    let mut shapes = Vec::with_capacity(5);
    shapes.push(Shape::mesh(std::sync::Arc::new(mesh)));
    let stroke = Stroke::new(1.0, face.stroke);
    for i in 0..4 {
        let a = face.pts[i];
        let b = face.pts[(i + 1) % 4];
        if (b - a).length_sq() >= 0.25 {
            shapes.push(Shape::line_segment([a, b], stroke));
        }
    }
    shapes
}

fn voxel_centers(state: &ForgeState, now: f64, animate: bool) -> [[f32; 3]; N_VOXELS] {
    let mut centers = rest_centers();
    if !animate {
        return centers;
    }
    for (k, center) in centers.iter_mut().enumerate() {
        let Some(t0) = state.voxel_time[k] else {
            continue;
        };
        let age = (now - t0) as f32;
        if (0.0..DROP_SECS).contains(&age) {
            center[1] += (1.0 - ease_out_cubic(age / DROP_SECS)) * DROP_HEIGHT;
        }
    }
    centers
}

fn newest_placed(state: &ForgeState, now: f64) -> Option<usize> {
    state
        .voxel_time
        .iter()
        .enumerate()
        .rev()
        .find_map(|(k, t)| t.is_some_and(|t0| now >= t0).then_some(k))
}

fn heat_target(k: usize, newest: Option<usize>, linking: bool, running: bool) -> f32 {
    if linking {
        0.55
    } else if running && newest == Some(k) {
        0.20
    } else {
        0.0
    }
}

fn heat_floor(ui: &Ui, op_id: &str, k: usize, target: f32) -> f32 {
    let id = Id::unique("watt-forge-heat").with(op_id).with(k);
    ui.ctx()
        .animate_value_with_time(id, target, theme::anim_secs(ui.ctx(), 0.6))
}

/// White-hot while falling, then `1 - t²` down to the floor. `t` is not clamped, so a voxel
/// that has cooled past 0.9 s sits on the floor (heartbeat, molten link, or zero).
fn voxel_temp(t0: f64, now: f64, floor: f32, animate: bool) -> f32 {
    if !animate {
        return 0.0;
    }
    let age = (now - t0) as f32;
    if age < DROP_SECS {
        return 1.0;
    }
    let u = (age - DROP_SECS) / HEAT_COOL_SECS;
    (1.0 - u * u).max(floor)
}

fn heat_pulse(now: f64, base: f32, beating: bool) -> f32 {
    if !beating {
        return 0.0;
    }
    let scale = (base / 0.20).clamp(0.0, 1.0);
    scale * 0.12 * (TAU * now as f32 / 1.6).sin()
}

struct ShineBand {
    min_u: f32,
    span: f32,
    dir: [f32; 2],
    centre: f32,
}

impl ShineBand {
    fn coverage(&self, pts: &[[f32; 2]; 4]) -> f32 {
        let (mut x, mut y) = (0.0, 0.0);
        for p in pts {
            x += p[0];
            y += p[1];
        }
        let u = (x * 0.25 * self.dir[0] + y * 0.25 * self.dir[1] - self.min_u) / self.span;
        (1.0 - (u - self.centre).abs() / 0.18).max(0.0)
    }
}

fn shine_band(
    state: &ForgeState,
    view: &OpView,
    style: &ForgeStyle,
    now: f64,
    cam: FrameCam,
    centers: &[[f32; 3]; N_VOXELS],
) -> Option<ShineBand> {
    if !style.animate || !matches!(view.outcome, Some(OpOutcome::Ok { .. })) {
        return None;
    }
    let land = last_land(state)?;
    let age = now - (land + SHINE_DELAY);
    if !(0.0..SHINE_SECS).contains(&age) {
        return None;
    }
    let centre = -0.15 + 1.30 * ease_in_out_cubic((age / SHINE_SECS) as f32);
    let dir = Vec2::new(1.0, 0.5).normalized();
    let mut min_u = f32::INFINITY;
    let mut max_u = f32::NEG_INFINITY;
    for center in centers {
        for bits in 0..8u8 {
            let p = cam.project(voxel_corner(*center, bits));
            let u = p[0] * dir.x + p[1] * dir.y;
            min_u = min_u.min(u);
            max_u = max_u.max(u);
        }
    }
    let span = max_u - min_u;
    if span < 1.0 {
        return None;
    }
    Some(ShineBand {
        min_u,
        span,
        dir: [dir.x, dir.y],
        centre,
    })
}

fn build_cube_faces(
    ui: &Ui,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
    cam: FrameCam,
    out: &mut Vec<PaintFace>,
) {
    let p = theme::palette(ui.ctx());
    let centers = voxel_centers(state, now, style.animate);
    let eye = eye_model(FOCAL, cam.yaw, TILT);
    let order = painter_order(eye, &centers);
    let base = cube_color(view, style.accent, &p);
    let linking = view.running() && view.current == Some(Phase::Link);
    let newest = newest_placed(state, now);
    let shine = shine_band(state, view, style, now, cam, &centers);
    let mut floors = [0.0f32; N_VOXELS];
    if style.animate {
        for (k, floor) in floors.iter_mut().enumerate() {
            if state.voxel_time[k].is_none() {
                continue;
            }
            let target = heat_target(k, newest, linking, view.running());
            *floor = heat_floor(ui, &view.op_id, k, target);
        }
    }

    for &k in &order {
        let mask = visible_face_mask(eye, centers[k]);
        let Some(t0) = state.voxel_time[k].filter(|t0| now >= *t0) else {
            for face in 0..6 {
                if mask & (1 << face) == 0 || !is_hull_face(k, face) {
                    continue;
                }
                push_face(out, cam.face(centers[k], face), GHOST_FILL, GHOST_STROKE);
            }
            continue;
        };
        let beating = view.running() && !linking && newest == Some(k);
        let floor = (floors[k] + heat_pulse(now, floors[k], beating)).clamp(0.0, 1.0);
        let temp = voxel_temp(t0, now, floor, style.animate);
        let metal = base.lerp_to_gamma(p.hot, temp);
        for face in 0..6 {
            if mask & (1 << face) == 0 {
                continue;
            }
            let pts = cam.face(centers[k], face);
            let mut color = shade(metal, diffuse_for(face, cam.yaw, TILT));
            if neighbor_index(k, face).is_some_and(|n| state.voxel_time[n].is_some()) {
                color = shade(color, 0.55);
            }
            if let Some(band) = &shine {
                let coverage = band.coverage(&pts);
                color = color.lerp_to_gamma(Color32::WHITE, 0.35 * coverage);
            }
            push_face(out, pts, color, PLACED_STROKE);
        }
    }
}

fn draw_success_sparks(
    painter: &egui::Painter,
    rect: Rect,
    view: &OpView,
    style: &ForgeStyle,
    state: &ForgeState,
    now: f64,
    cam: FrameCam,
) {
    if !style.animate || !matches!(view.outcome, Some(OpOutcome::Ok { .. })) {
        return;
    }
    let Some(land) = last_land(state) else {
        return;
    };
    let age = now - (land + SPARK_DELAY);
    if !(0.0..SPARK_SECS).contains(&age) {
        return;
    }
    let origin = cam.project([0.0, 0.0, 0.0]);
    draw_sparks(
        painter,
        rect,
        Pos2::new(origin[0], origin[1]),
        age as f32,
        &view.op_id,
        &theme::palette(painter.ctx()),
    );
}

/// Eight embers, each a pure function of `(now - link_since, seed(op_id, i))`.
///
/// They rise off the top of the cube and fade over the last 60% of a 2.0–2.8 s life.
fn draw_embers(
    painter: &egui::Painter,
    rect: Rect,
    cam: FrameCam,
    age: f64,
    op_id: &str,
    p: &Palette,
) {
    for i in 0..8u32 {
        let mut rng = Rng::new(seed_from(op_id, 0xE3BE ^ u64::from(i)));
        let period = rng.range(2.0, 2.8);
        let x = rng.range(-1.2, 1.2);
        let z = rng.range(-1.2, 1.2);
        let drift = rng.range(-6.0, 6.0);
        let offset = i as f32 * period / 8.0;
        let life = (age as f32 + offset).rem_euclid(period) / period;
        let radius = 2.2 + (1.0 - 2.2) * life;
        let spawn = cam.project([x, 1.0 + VOXEL_H, z]);
        // The cube band is shorter than a 0.45×height rise, so cap the travel at the sky
        // inside the band. Otherwise the bright part of the life is sliced off at the top.
        let room = (spawn[1] - rect.top() - radius - 2.0).max(12.0);
        let travel = (0.45 * rect.height()).min(room);
        let rise = ease_out_cubic(life) * travel;
        let pos = Pos2::new(spawn[0] + drift * (TAU * 0.5 * life).sin(), spawn[1] - rise);
        let alpha = if life < 0.4 {
            180.0
        } else {
            180.0 * (1.0 - (life - 0.4) / 0.6)
        };
        let color = theme::with_alpha(p.hot.lerp_to_gamma(p.volt, life), alpha as u8);
        painter.circle_filled(pos, radius, color);
    }
}

/// Placed-cube polygons for the timing test. Same face path as a finished cube, no egui context.
#[doc(hidden)]
#[allow(dead_code)] // tests/forge_snapshots.rs
pub(crate) fn cube_shapes_for_bench(yaw: f32, scale: f32, origin: Pos2) -> Vec<Shape> {
    let cam = FrameCam {
        yaw,
        scale,
        cx: origin.x,
        cy: origin.y,
    };
    let eye = eye_model(FOCAL, yaw, TILT);
    let centers = rest_centers();
    let order = painter_order(eye, &centers);
    let mut faces = Vec::new();
    let color = Color32::from_rgb(255, 194, 26);
    for &k in &order {
        let mask = visible_face_mask(eye, centers[k]);
        for face in 0..6 {
            if mask & (1 << face) == 0 {
                continue;
            }
            let mut fill = shade(color, diffuse_for(face, yaw, TILT));
            if !is_hull_face(k, face) {
                fill = shade(fill, 0.55);
            }
            push_face(&mut faces, cam.face(centers[k], face), fill, PLACED_STROKE);
        }
    }
    let mut shapes = Vec::with_capacity(faces.len());
    for face in &faces {
        if face_altitude(&face.pts) < THIN_FACE_PX {
            shapes.extend(thin_face_shapes(face));
        } else {
            shapes.push(Shape::convex_polygon(
                face.pts.to_vec(),
                face.fill,
                Stroke::new(1.0, face.stroke),
            ));
        }
    }
    shapes
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
            centered_line(
                ui,
                &success_headline(view, style),
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

fn success_headline(view: &OpView, style: &ForgeStyle) -> String {
    let play = offers_play(view.op);
    if play && style.ready {
        return ready_headline(&style.title, true);
    }
    if play && !style.ready && matches!(view.op, Some(OpKind::Mods)) {
        return "Mods applied — build needed".to_owned();
    }
    "Done".to_owned()
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
    // The measurer borrows `ui`. Drop it before the row is allocated.
    let (labels, pitch, current_full) = {
        let measure = |text: &str| -> f32 {
            ui.painter()
                .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        };
        let full_w: Vec<f32> = phases.iter().map(|phase| measure(&phase.title)).collect();
        let short_w: Vec<f32> = phases
            .iter()
            .map(|phase| measure(short_phase(phase.phase)))
            .collect();
        if let Some(pitch) = row_pitch(&full_w, avail) {
            (StepLabels::Full, pitch, false)
        } else if let Some(pitch) = row_pitch(&short_w, avail) {
            (StepLabels::Short, pitch, false)
        } else {
            let index = focus_index(phases);
            let pitch = if n == 1 {
                0.0
            } else {
                ((avail - DOT) / (n - 1) as f32).clamp(DOT + 4.0, 64.0)
            };
            let full = measure(&phases[index].title) <= avail;
            (StepLabels::Current, pitch, full)
        }
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
        StepLabels::Full | StepLabels::Short => {
            for (i, phase) in phases.iter().enumerate() {
                let text: &str = if labels == StepLabels::Full {
                    &phase.title
                } else {
                    short_phase(phase.phase)
                };
                paint_step_label(
                    ui,
                    text,
                    x0 + pitch * i as f32,
                    rect,
                    label_color(&phase.state, &p),
                    &font,
                );
            }
        }
        StepLabels::Current => {
            let index = focus_index(phases);
            let text: &str = if current_full {
                &phases[index].title
            } else {
                short_phase(phases[index].phase)
            };
            paint_step_label(
                ui,
                text,
                x0 + pitch * index as f32,
                rect,
                label_color(&phases[index].state, &p),
                &font,
            );
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepLabels {
    Full,
    Short,
    Current,
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
    Build,
    Recovery(Recovery),
}

fn draw_buttons(ui: &mut Ui, view: &OpView, style: &ForgeStyle) -> ForgeResponse {
    let buttons = button_row(view, style);
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
                // A fresh id per kind. Positional ids survive the outcome change, so a Cancel
                // pressed as the op finishes can release onto Play.
                ui.push_id(button_salt(&button.kind), |ui| {
                    let clicked = Button::new(button.label)
                        .variant(button.variant)
                        .size(Size::Medium)
                        .min_width(96.0)
                        .show(ui)
                        .clicked();
                    if clicked {
                        apply_button(&mut response, &button.kind);
                    }
                });
            }
        });
    }
    response
}

fn button_salt(kind: &ButtonKind) -> String {
    match kind {
        ButtonKind::Cancel => "cancel".to_owned(),
        ButtonKind::Hide => "hide".to_owned(),
        ButtonKind::Play => "play".to_owned(),
        ButtonKind::Close => "close".to_owned(),
        ButtonKind::Build => "build".to_owned(),
        ButtonKind::Recovery(recovery) => format!("recovery-{}", recovery_label(*recovery)),
    }
}

fn button_row(view: &OpView, style: &ForgeStyle) -> Vec<ForgeButton> {
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
        Some(OpOutcome::Ok { .. }) if offers_play(view.op) && style.ready => vec![
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
        Some(OpOutcome::Ok { .. }) if offers_play(view.op) => vec![
            ForgeButton {
                label: "Build now",
                variant: Variant::Primary,
                kind: ButtonKind::Build,
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
            let offered: Vec<Recovery> = recovery
                .iter()
                .copied()
                .filter(|item| !style.retry_only || matches!(item, Recovery::Retry))
                .collect();
            let mut row = Vec::with_capacity(offered.len() + 1);
            for (i, recovery) in offered.iter().enumerate() {
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
        ButtonKind::Build => response.build = true,
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
                let scroll = || {
                    egui::ScrollArea::vertical()
                        .id_salt(("forge-log-scroll", &view.op_id))
                        .max_height(200.0)
                        .auto_shrink([false, true])
                        .stick_to_bottom(true)
                };
                if view.log.is_empty() {
                    scroll().show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("No log yet.")
                                .font(theme::mono(12.0))
                                .color(p.text_muted),
                        );
                    });
                } else {
                    let font = theme::mono(12.0);
                    let row_h = ui.ctx().fonts_mut(|fonts| fonts.row_height(&font));
                    let len = view.log.len();
                    scroll().show_rows(ui, row_h, len, |ui, range| {
                        for row in range {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&view.log[row])
                                        .font(theme::mono(12.0))
                                        .color(p.text),
                                )
                                .wrap_mode(egui::TextWrapMode::Truncate),
                            );
                        }
                    });
                }
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
#[allow(dead_code)] // the app paints widgets::cube; the forge demo and snapshot test call this
pub fn cube_icon(ui: &mut Ui, size: f32, color: Color32, busy: bool) -> Response {
    let _ = busy;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter_at(rect);
    paint_icon_cube(&painter, rect.center(), size * ICON_CUBE_SCALE, color);
    response
}

/// Icon camera. Not the forge camera: a closer focal, so the mark fills a small box.
const ICON_FOCAL: f32 = 4.0;
const ICON_YAW: f32 = 0.6;
const ICON_TILT: f32 = -0.3;

fn paint_icon_cube(painter: &egui::Painter, centre: Pos2, scale: f32, color: Color32) {
    let edge = Color32::from_rgba_unmultiplied(10, 10, 14, 110);
    for (_, pts, fill) in icon_faces(centre, scale, |_, diffuse| shade(color, diffuse)) {
        painter.add(Shape::convex_polygon(
            pts.to_vec(),
            fill,
            Stroke::new(1.0, edge),
        ));
    }
}

/// Visible faces of the single-voxel icon, far to near (larger view-z first).
fn icon_faces(
    centre: Pos2,
    scale: f32,
    color_of: impl Fn(usize, f32) -> Color32,
) -> Vec<(f32, [Pos2; 4], Color32)> {
    let eye = eye_model(ICON_FOCAL, ICON_YAW, ICON_TILT);
    let center = [0.0; 3];
    let mask = visible_face_mask(eye, center);
    let cam = Proj {
        focal: ICON_FOCAL,
        yaw: ICON_YAW,
        tilt: ICON_TILT,
        scale,
        cx: centre.x,
        cy: centre.y,
    };
    let mut faces = Vec::with_capacity(3);
    for face in 0..6 {
        if mask & (1 << face) == 0 {
            continue;
        }
        let depth = face_view_depth(ICON_YAW, ICON_TILT, center, face);
        let diffuse = diffuse_for(face, ICON_YAW, ICON_TILT);
        let mut pts = project_face(cam, center, face);
        orient_clockwise(&mut pts);
        faces.push((
            depth,
            pts.map(|p| Pos2::new(p[0], p[1])),
            color_of(face, diffuse),
        ));
    }
    faces.sort_by(|a, b| b.0.total_cmp(&a.0));
    faces
}

/// The window icon: a cube in the launcher colours, as RGBA pixels (`size` x `size`).
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let size = size.max(4);
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let amber = Color32::from_rgb(255, 160, 40);
    let amber_dark = Color32::from_rgb(200, 110, 20);
    let bolt = Color32::from_rgb(255, 240, 200);

    let centre = Pos2::new(size as f32 / 2.0, size as f32 / 2.0);
    let polys = icon_faces(centre, size as f32 * ICON_CUBE_SCALE, |face, diffuse| {
        let base = if face.is_multiple_of(2) {
            amber
        } else {
            amber_dark
        };
        shade(base, diffuse)
    });

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
