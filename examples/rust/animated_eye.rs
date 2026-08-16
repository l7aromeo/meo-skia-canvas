//! An animated eye: a wink driven by springs rather than keyframes.
//!
//! Run with:
//!
//!     cargo run --release --example animated_eye -- [outdir]
//!
//! The Rust half of `examples/node/animated-eye.js`.
//!
//! Nothing here is a keyframe. The lid, the pupil, the gaze, the brow and
//! every one of the 200 lashes is a spring-damper integrated at a fixed
//! 240 Hz with an accumulator, so the motion is a consequence of the forces
//! rather than a curve someone drew:
//!
//!   - the lid spring is asymmetric -- stiff closing, soft opening -- so the
//!     wink snaps shut and drifts back open, overshooting as it settles
//!   - each lash lags the lid through its own spring, and its root angle is
//!     blended from the open fan to a swept-down rest pose as the lid falls, so
//!     the whole fan rotates outward and never sweeps through the eye
//!   - lid velocity draws a second ghost copy of each lash: motion blur that
//!     costs nothing and appears only during the snap
//!   - the eyeball rolls up as the lid closes (Bell's phenomenon), so the iris
//!     is seen climbing out of view mid-wink
//!   - the catchlights sit on the cornea rather than in the iris plane, so they
//!     track the gaze at roughly half speed -- the parallax is what makes the
//!     eye read as a dome instead of a disc
//!
//! The drawing leans on four things this library has that a browser canvas
//! does not: [`Path2D::jitter`] for hand-drawn edges on every hair and fibre,
//! [`MaskFilter`] for the soft occlusion in the socket and under the lashes,
//! a Display P3 canvas for the iris blues, and writing the animation straight
//! out of the canvas's pages.
//!
//! On the numbers: this file is full of them and almost none are named. They
//! are not values from a specification -- they are where a shadow sits and
//! how wide a lash is, chosen by looking. Naming `0.74` would say nothing
//! `sin(u * PI).powf(0.74)` does not already say, and the comments carry the
//! reasoning that matters. The few that govern the simulation rather than the
//! picture are named at the top.

use std::{
    env,
    error::Error,
    f32::consts::{PI, TAU},
    fs,
    path::PathBuf,
};

use meo_skia_canvas::prelude::*;

const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 500.0;
const FRAMES: usize = 150;
const FPS: f32 = 60.0;
/// Physics timestep. Fixed at 240 Hz and decoupled from the frame rate by an
/// accumulator, so the springs settle the same way whatever FPS is set to.
const DT: f32 = 1.0 / 240.0;

const CX: f32 = WIDTH / 2.0;
const CY: f32 = HEIGHT / 2.0 + 26.0;
/// Iris radius.
const R: f32 = 88.0;

/// Inner and outer corners of the eye opening.
const IN_X: f32 = CX - 160.0;
const OUT_X: f32 = CX + 160.0;

/// Lash lag springs. One per lash across all three upper rows plus the lower
/// row, indexed modulo this.
const LASH_COUNT: usize = 200;

fn hex(value: &str) -> RgbaLinear {
    RgbaLinear::from_hex(value).expect("a literal written in this file")
}

/// A colour with an alpha, the way the JavaScript mirror writes
/// `rgba(r g b / a)`.
fn rgba(value: &str, alpha: f32) -> RgbaLinear {
    hex(value).with_opacity(alpha)
}

// ── palette ────────────────────────────────────────────────────────────────

const SKIN: &str = "#f2d3c6";
const SKIN_DEEP: &str = "#e2ad9d";
const CREASE: &str = "#c08a7c";
const LID_TINT: &str = "#dba396";
const BROW: &str = "#5f4230";
const BROW_LIT: &str = "#96704e";
const BROW_DK: &str = "#453022";
const WATER: &str = "#c94f52";
const SCLERA: &str = "#f5efec";
const SCLERA_SHADE: &str = "#c9bcb9";
const DEEP: &str = "#12405c";
const MID: &str = "#2f83ad";
const PALE: &str = "#a8dcec";
const GOLD: &str = "#c8963a";
const LASH: &str = "#100b0b";

// ── deterministic noise ────────────────────────────────────────────────────

/// The one-liner hash every shader toy uses, and the reason the stipple and
/// the vasculature do not crawl between frames: the same index gives the same
/// number on every run and on every machine.
fn r1(i: f32) -> f32 {
    // In `f64`, because the JavaScript mirror is: `43758.5453` needs more
    // mantissa than `f32` has, so computing this in single precision quietly
    // rounds the constant to `43758.547` and every hair, pore and vessel
    // lands somewhere slightly else. Same arithmetic, same picture.
    let s = (f64::from(i) * 12.9898).sin() * 43758.5453;
    (s - s.floor()) as f32
}

// ── physics state ──────────────────────────────────────────────────────────

struct Spring {
    value: f32,
    velocity: f32,
}

struct Eye {
    pupil: Spring,
    gaze_x: Spring,
    gaze_y: Spring,
    gaze_target: (f32, f32),
    lid: Spring,
    lid_target: f32,
    brow: Spring,
    /// Per-lash angular lag. Each one is its own spring driven by the lid's
    /// velocity, which is what makes the fan whip on the snap and flutter on
    /// the reopen.
    lash_lag: Vec<Spring>,
    /// Orbicularis squeeze, recomputed per frame rather than integrated.
    squeeze: f32,
}

impl Eye {
    fn new() -> Self {
        Self {
            pupil: Spring {
                value: 26.0,
                velocity: 0.0,
            },
            gaze_x: Spring {
                value: 0.0,
                velocity: 0.0,
            },
            gaze_y: Spring {
                value: 0.0,
                velocity: 0.0,
            },
            gaze_target: (0.0, 0.0),
            lid: Spring {
                value: 1.0,
                velocity: 0.0,
            },
            lid_target: 1.0,
            brow: Spring {
                value: 0.0,
                velocity: 0.0,
            },
            lash_lag: (0..LASH_COUNT)
                .map(|_| Spring {
                    value: 0.0,
                    velocity: 0.0,
                })
                .collect(),
            squeeze: 0.0,
        }
    }

    fn step(&mut self, t: f32, wink_depth: f32) {
        let light = 0.5
            + 0.3 * (t * TAU).sin()
            + 0.3 * (-40.0 * (t % 1.0 - 0.5).powi(2)).exp();
        let want = 34.0 - light * 16.0;
        self.pupil.velocity += (-90.0 * (self.pupil.value - want)
            - 14.0 * self.pupil.velocity)
            * DT;
        self.pupil.value =
            (self.pupil.value + self.pupil.velocity * DT).clamp(12.0, 40.0);

        for (axis, target) in [
            (&mut self.gaze_x, self.gaze_target.0),
            (&mut self.gaze_y, self.gaze_target.1),
        ] {
            axis.velocity +=
                (-150.0 * (axis.value - target) - 17.0 * axis.velocity) * DT;
            axis.value += axis.velocity * DT;
        }

        // Asymmetric lid spring: the close is a snap, the reopen is soft and
        // underdamped, so it overshoots -- which is what reads as dramatic.
        let closing = self.lid_target < self.lid.value;
        let (k, c) = match closing {
            true => (460.0, 27.0),
            false => (190.0, 17.0),
        };
        self.lid.velocity += (-k * (self.lid.value - self.lid_target)
            - c * self.lid.velocity)
            * DT;
        self.lid.value =
            (self.lid.value + self.lid.velocity * DT).clamp(-0.02, 1.1);

        self.brow.velocity += (-120.0 * (self.brow.value - wink_depth * 13.0)
            - 14.0 * self.brow.velocity)
            * DT;
        self.brow.value += self.brow.velocity * DT;

        // Lashes lag the lid through their own springs; the gain is high
        // enough that the snap-close whips them and the reopen flutters.
        for lash in &mut self.lash_lag {
            lash.velocity += (-200.0 * lash.value - 12.0 * lash.velocity
                + self.lid.velocity * 7.5)
                * DT;
            lash.value = (lash.value + lash.velocity * DT).clamp(-0.65, 0.65);
        }
    }
}

// ── geometry ───────────────────────────────────────────────────────────────

fn at_x(u: f32) -> f32 {
    IN_X + (OUT_X - IN_X) * u
}

/// `sin(u * PI)` raised to a power, with the sine floored at zero.
///
/// The floor is load-bearing. `f32::consts::PI` rounds *up* past pi, so
/// `(1.0 * PI).sin()` is -8.74e-8 rather than zero, and a negative base under
/// a fractional exponent is NaN. Every profile below samples `u` at exactly
/// 1.0, so without this the last vertex of each curve is NaN -- which Skia
/// turns into an empty path rather than into an error.
///
/// That is what left this drawing with no sclera and no iris: `opening_path`
/// came back with bounds of (0, 0)-(0, 0), the clip built from it was empty,
/// and the eyeball drawn inside that clip went nowhere. The lid, the lashes
/// and the brow were unaffected, so the frame looked like a shut eye rather
/// than like a fault.
///
/// The JavaScript twin computes the same expression in double precision,
/// where `Math.sin(Math.PI)` is a small *positive* number, and so never had
/// this to solve.
fn arc(u: f32, exponent: f32) -> f32 {
    (u * PI).sin().max(0.0).powf(exponent)
}

/// The squeeze lifts the lower lid -- a wink engages it, a blink barely.
fn lower_y(u: f32, squeeze: f32) -> f32 {
    CY + arc(u, 0.68) * 79.0 - squeeze * 26.0 * (u * PI).sin()
}

/// Exponent below one fills the arc out: a full dome, not a pointed wedge.
fn upper_y(u: f32, open: f32, squeeze: f32) -> f32 {
    let dome = arc(u, 0.74);
    let wide = CY - dome * 97.0;
    let shut = lower_y(u, squeeze) - 1.5;
    shut + (wide - shut) * open
}

fn crease_y(u: f32, open: f32, squeeze: f32) -> f32 {
    let rest = upper_y(u, 1.0, squeeze) - 30.0 - (u * PI).sin() * 12.0;
    // As the lid closes the fold chases it down.
    rest + (upper_y(u, open, squeeze) - rest) * (1.0 - open) * 0.5
}

/// The eye opening: the upper lid out, the lower lid back.
fn opening_path(open: f32, squeeze: f32) -> Path2D {
    const STEPS: usize = 48;
    let mut path = PathBuilder::new();
    path.move_to(IN_X, lower_y(0.0, squeeze) - 1.0);
    for i in 1..=STEPS {
        let u = i as f32 / STEPS as f32;
        path.line_to(at_x(u), upper_y(u, open, squeeze));
    }
    for i in (0..STEPS).rev() {
        let u = i as f32 / STEPS as f32;
        path.line_to(at_x(u), lower_y(u, squeeze));
    }
    path.close_path();
    path.build(FillRule::NonZero)
}

/// A lash: a closed tapered sliver -- thick at the root, a point at the tip.
fn lash_path(
    x: f32,
    y: f32,
    angle: f32,
    length: f32,
    curl: f32,
    wide: f32,
) -> Path2D {
    let (mx, my) = (
        x + angle.cos() * length * 0.55,
        y + angle.sin() * length * 0.55,
    );
    let (tx, ty) = (
        mx + (angle + curl).cos() * length * 0.6,
        my + (angle + curl).sin() * length * 0.6,
    );
    let (nx, ny) = (-angle.sin() * wide, angle.cos() * wide);
    let mut path = PathBuilder::new();
    path.move_to(x - nx, y - ny);
    path.quadratic_curve_to(mx - nx * 0.4, my - ny * 0.4, tx, ty);
    path.quadratic_curve_to(mx + nx * 0.4, my + ny * 0.4, x + nx, y + ny);
    path.close_path();
    path.build(FillRule::NonZero)
}

/// Where an upper lash points, and how long it looks, as the lid falls.
///
/// The fan is mirror-symmetric about the middle of the lid: the further left
/// a lash sits the further left it leans, and the same to the right. That has
/// to hold at every stage of the wink, not just at the ends, and it is the
/// reason this interpolates a direction *vector* rather than an angle. Angles
/// cannot do it. Turning every lash the same way collapses the fan to
/// near-horizontal halfway down -- a 0.22 rad spread, all of it leaning one
/// way. Letting each half turn its own way is symmetric but meets in a seam
/// at the centre, where the two directions are 180 apart and the shorter way
/// round flips sign.
///
/// A vector lerp has neither problem: each side crosses through its own side
/// as it falls, so the fan spreads open halfway and closes again, and nothing
/// is discontinuous anywhere.
fn upper_lash_aim(u: f32, open: f32, lag: f32) -> (f32, f32) {
    let open_angle = -PI / 2.0 - 0.62 + u * 1.24;
    // The same fan, mirrored downward.
    let shut_angle = -open_angle;
    let k = 1.0 - open.clamp(0.0, 1.0).powf(0.75);

    let x = open_angle.cos() * (1.0 - k) + shut_angle.cos() * k;
    let mut y = open_angle.sin() * (1.0 - k) + shut_angle.sin() * k;

    // The centre lash points at the viewer halfway down, which in a flat
    // drawing is a vector of length zero. A little downward bias, peaking at
    // the halfway point, gives it somewhere to be -- and the shortening that
    // survives is the foreshortening a lash pointing outward really has.
    y += (PI * k).sin() * 0.55;

    (y.atan2(x) + lag, x.hypot(y))
}

// ── drawing helpers ────────────────────────────────────────────────────────

fn blur(sigma: f32) -> MaskFilter {
    MaskFilter::blur(BlurStyle::Normal, sigma, false)
        .expect("a finite positive sigma")
}

fn stops(entries: &[(f32, RgbaLinear)]) -> Vec<GradientStop> {
    entries
        .iter()
        .map(|(position, color)| GradientStop {
            position: *position,
            color: *color,
        })
        .collect()
}

/// The Canvas API's `createRadialGradient`, which is Skia's two-point conical
/// with both circles concentric or not as the caller likes.
fn radial(
    from: Point,
    from_radius: f32,
    to: Point,
    to_radius: f32,
    entries: &[(f32, RgbaLinear)],
) -> Result<Shader, Box<dyn Error>> {
    Shader::two_point_conical_gradient(
        from,
        from_radius,
        to,
        to_radius,
        &stops(entries),
        GradientColorSpace::Srgb,
    )
    .map_err(Into::into)
}

/// Builds a polyline along `u` from `from` to `to` inclusive.
fn along(
    from: usize,
    to: usize,
    divisor: f32,
    mut y: impl FnMut(f32) -> f32,
) -> Path2D {
    let mut path = PathBuilder::new();
    for i in from..=to {
        let u = i as f32 / divisor;
        match i == from {
            true => path.move_to(at_x(u), y(u)),
            false => path.line_to(at_x(u), y(u)),
        };
    }
    path.build(FillRule::NonZero)
}

// ── one frame ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn draw_frame(
    ctx: &mut Context2D,
    eye: &Eye,
    frame: usize,
    open: f32,
    lid_velocity: f32,
    wink_depth: f32,
) -> Result<(), Box<dyn Error>> {
    let squeeze = eye.squeeze;
    let pupil_r = eye.pupil.value;

    // The eye is never still: a slow two-frequency drift keeps it alive.
    let micro = (frame as f32 * 0.63).sin() * 0.7
        + (frame as f32 * 1.71 + 2.0).sin() * 0.5;
    // Bell's phenomenon: the eyeball rolls up as the lid closes, so what you
    // glimpse mid-wink is the iris climbing out of view.
    let roll = (1.0 - open.clamp(0.0, 1.0)) * 20.0;
    let gx = CX + eye.gaze_x.value + micro;
    let gy = CY - 12.0 + eye.gaze_y.value - roll;
    // A reflection sits on the cornea, not in the iris plane, so it tracks
    // the gaze at roughly half speed -- that parallax is what sells the dome.
    let clx = CX + eye.gaze_x.value * 0.45 + micro * 0.5;
    let cly = CY - 12.0 + eye.gaze_y.value * 0.45 - roll * 0.35;

    // — paper —
    ctx.set_fill_style(hex("#f6f2ee"));
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);

    // — skin mass —
    ctx.save();
    ctx.set_mask_filter(Some(&blur(28.0)));
    ctx.set_fill_shader(&radial(
        Point::new(CX, CY - 14.0),
        40.0,
        Point::new(CX, CY - 14.0),
        265.0,
        &[
            (0.0, hex(SKIN_DEEP)),
            (0.5, hex(SKIN)),
            (1.0, rgba(SKIN, 0.0)),
        ],
    )?);
    ctx.begin_path();
    ctx.ellipse(CX, CY - 18.0, 235.0, 168.0, 0.0, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();

    // — under-eye subsurface tint, and the highlight along the brow bone —
    ctx.save();
    ctx.set_mask_filter(Some(&blur(18.0)));
    ctx.set_global_alpha(0.22);
    ctx.set_fill_style(hex("#c99aa4"));
    ctx.begin_path();
    ctx.ellipse(
        CX,
        lower_y(0.5, squeeze) + 44.0,
        165.0,
        30.0,
        0.0,
        0.0,
        TAU,
        false,
    )?;
    ctx.fill(FillRule::NonZero);
    ctx.set_global_alpha(0.35);
    ctx.set_fill_style(hex("#fdeee2"));
    ctx.begin_path();
    ctx.ellipse(
        CX + 20.0,
        CY - 158.0 + eye.brow.value * 0.5,
        150.0,
        22.0,
        -0.04,
        0.0,
        TAU,
        false,
    )?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // Pore stipple, fixed seed so it does not crawl frame to frame.
    ctx.save();
    for i in 0..420 {
        let i = i as f32;
        let rx = r1(i * 3.1) * 2.0 - 1.0;
        let ry = r1(i * 5.7 + 1.0) * 2.0 - 1.0;
        let x = CX + rx * 250.0;
        let y = CY - 30.0 + ry * 190.0;
        // Not on the eye.
        if ((x - CX) / 195.0).powi(2) + ((y - CY) / 105.0).powi(2) < 1.0 {
            continue;
        }
        ctx.set_global_alpha(0.03 + r1(i * 7.7) * 0.05);
        ctx.set_fill_style(match r1(i * 2.3) > 0.5 {
            true => hex("#b98a7c"),
            false => hex("#fde8dc"),
        });
        ctx.begin_path();
        ctx.arc(x, y, 0.7 + r1(i * 9.1) * 1.1, 0.0, TAU, false)?;
        ctx.fill(FillRule::NonZero);
    }
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // — nose-bridge shadow on the inner side, temple shading on the outer —
    ctx.save();
    ctx.set_mask_filter(Some(&blur(26.0)));
    ctx.set_fill_style(hex("#dda595"));
    ctx.set_global_alpha(0.3);
    ctx.begin_path();
    ctx.ellipse(IN_X - 66.0, CY + 4.0, 36.0, 82.0, 0.1, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.set_fill_style(hex("#e0ac9b"));
    ctx.set_global_alpha(0.18);
    ctx.begin_path();
    ctx.ellipse(OUT_X + 72.0, CY - 6.0, 30.0, 78.0, -0.12, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();

    // Crow's feet at the outer corner: faint at rest, cut deep by the squeeze.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(1.8)));
    ctx.set_stroke_style(hex("#b97f70"));
    ctx.set_line_cap(StrokeCap::Round);
    for i in 0..4 {
        let rnd = r1(i as f32 * 21.0 + 4.0);
        let a = -0.32 + i as f32 * 0.24 + (rnd - 0.5) * 0.08;
        let x0 = OUT_X + 4.0;
        let y0 = CY - 6.0 + (i as f32 - 1.5) * 10.0;
        let len = (26.0 + rnd * 22.0) * (1.0 + wink_depth * 0.6);
        ctx.set_global_alpha(0.12 + wink_depth * 0.3);
        ctx.set_line_width(1.6 + wink_depth * 1.2);
        let mut foot = PathBuilder::new();
        foot.move_to(x0, y0);
        foot.quadratic_curve_to(
            x0 + a.cos() * len * 0.6,
            y0 + a.sin() * len * 0.55,
            x0 + a.cos() * len,
            y0 + a.sin() * len,
        );
        ctx.stroke_path(&foot.build(FillRule::NonZero).jitter(
            5.0,
            1.4,
            i as u32 * 43,
        ));
    }
    ctx.restore();

    // The fine under-eye crease where lid meets cheek.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(2.2)));
    ctx.set_stroke_style(hex("#c08a7c"));
    ctx.set_global_alpha(0.22 + wink_depth * 0.2);
    ctx.set_line_width(2.4);
    let under = along(4, 40, 44.0, |u| {
        lower_y(u, squeeze) + 20.0 + (u * PI).sin() * 6.0 - wink_depth * 8.0
    });
    ctx.stroke_path(&under.jitter(8.0, 1.6, frame as u32 * 3 + 9));
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // — cheek pushed up by the wink —
    if wink_depth > 0.02 {
        ctx.save();
        ctx.set_mask_filter(Some(&blur(22.0)));
        ctx.set_global_alpha(wink_depth * 0.5);
        ctx.set_fill_style(hex("#e8a795"));
        ctx.begin_path();
        ctx.ellipse(
            CX + 10.0,
            lower_y(0.5, squeeze) + 46.0 - wink_depth * 12.0,
            150.0,
            40.0,
            0.0,
            0.0,
            TAU,
            false,
        )?;
        ctx.fill(FillRule::NonZero);
        ctx.restore();
    }

    // — brow: shadow understory, then hairs in three zones —
    let brow_base = |u: f32| {
        let rise = match u < 0.62 {
            true => ((u / 0.62) * PI / 2.0).sin(),
            false => ((((u - 0.62) / 0.38) * PI / 2.0) * 0.9).cos(),
        };
        CY - 208.0 - rise * 34.0
            + u * 20.0
            + eye.brow.value
            + wink_depth * 6.0 * (1.0 - u)
    };

    // The socket is a cavity. Both shadow bands are drawn as short segments
    // whose alpha rises and falls along the brow, because a stroke with a
    // blurred square end reads as a smudge floating past the hairs.
    for (dy, width, sigma, color, peak) in [
        (44.0, 42.0, 22.0, "#d6a08f", 0.42), // broad orbital shading
        (16.0, 13.0, 8.0, "#bd8676", 0.5),   // the brow's own cast line
    ] {
        ctx.save();
        ctx.set_mask_filter(Some(&blur(sigma)));
        ctx.set_stroke_style(hex(color));
        ctx.set_line_width(width);
        ctx.set_line_cap(StrokeCap::Round);
        for i in 0..22 {
            let u0 = 0.05 + (i as f32 / 22.0) * 0.9;
            let u1 = 0.05 + ((i as f32 + 1.4) / 22.0) * 0.9;
            let envelope = arc(((u0 + u1) / 2.0 - 0.05) / 0.9, 0.55);
            ctx.set_global_alpha(peak * envelope);
            ctx.begin_path();
            ctx.move_to(
                IN_X - 20.0 + (OUT_X - IN_X + 62.0) * u0,
                brow_base(u0) + dy,
            );
            ctx.line_to(
                IN_X - 20.0 + (OUT_X - IN_X + 62.0) * u1,
                brow_base(u1) + dy,
            );
            ctx.stroke();
        }
        ctx.restore();
    }

    ctx.save();
    ctx.set_line_cap(StrokeCap::Round);
    for i in 0..300 {
        let rnd = r1(i as f32 * 1.7 + 3.0);
        let u = i as f32 / 300.0;
        let bx = IN_X - 28.0 + (OUT_X - IN_X + 78.0) * u + (rnd - 0.5) * 8.0;
        let by = brow_base(u) + (rnd - 0.5) * (20.0 - u * 8.0);
        // Head hairs stand nearly upright, the body angles over, the tail
        // lies flat.
        let zone = match u < 0.16 {
            true => -1.25,
            false => -0.92 + (u - 0.16) * 1.35,
        };
        let angle = zone + (rnd - 0.5) * 0.3;
        let len = (22.0 + rnd * 30.0)
            * match u < 0.16 {
                true => 1.15,
                false => 1.0 - u * 0.42,
            };
        ctx.set_stroke_style(hex(match rnd {
            r if r > 0.75 => BROW_LIT,
            r if r > 0.25 => BROW,
            _ => BROW_DK,
        }));
        ctx.set_global_alpha(
            (0.3 + rnd * 0.5)
                * match u > 0.82 {
                    true => 1.0 - (u - 0.82) * 3.5,
                    false => 1.0,
                },
        );
        ctx.set_line_width(0.9 + rnd * 1.9);
        let mut hair = PathBuilder::new();
        hair.move_to(bx, by);
        hair.quadratic_curve_to(
            bx + angle.cos() * len * 0.55,
            by + angle.sin() * len * 0.5,
            bx + (angle + 0.4).cos() * len,
            by + (angle + 0.4).sin() * len,
        );
        ctx.stroke_path(&hair.build(FillRule::NonZero).jitter(
            6.0,
            1.3,
            i as u32 * 11,
        ));
    }
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // — crease, deepening as the lid folds —
    ctx.save();
    ctx.set_mask_filter(Some(&blur(6.0)));
    ctx.set_stroke_style(hex(CREASE));
    ctx.set_line_width(8.0 + wink_depth * 4.0);
    ctx.set_global_alpha(0.75 + wink_depth * 0.25);
    let crease = along(0, 36, 36.0, |u| crease_y(u, open, squeeze));
    ctx.stroke_path(&crease.jitter(9.0, 1.5, frame as u32 * 13 + 5));

    // Lid plate tint between crease and lash line.
    ctx.set_mask_filter(Some(&blur(11.0)));
    ctx.set_stroke_style(hex(LID_TINT));
    ctx.set_global_alpha(0.35);
    ctx.set_line_width(24.0);
    let plate = along(0, 36, 36.0, |u| {
        (crease_y(u, open, squeeze) + upper_y(u, open, squeeze)) / 2.0
    });
    ctx.stroke_path(&plate);
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // Extra bunched folds while the wink squeezes.
    if wink_depth > 0.05 {
        ctx.save();
        ctx.set_global_alpha(0.16 * wink_depth);
        ctx.set_stroke_style(hex(CREASE));
        ctx.set_line_width(2.0);
        for i in 0..8 {
            let rnd = r1(i as f32 * 7.0 + 2.0);
            let u0 = 0.15 + rnd * 0.6;
            let mut fold = PathBuilder::new();
            fold.move_to(
                at_x(u0),
                crease_y(u0, open, squeeze) - 8.0 - rnd * 14.0,
            );
            fold.quadratic_curve_to(
                at_x(u0 + 0.12),
                crease_y(u0 + 0.1, open, squeeze) - 20.0 - rnd * 12.0,
                at_x(u0 + 0.22),
                crease_y(u0 + 0.2, open, squeeze) - 10.0 - rnd * 10.0,
            );
            ctx.stroke_path(&fold.build(FillRule::NonZero).jitter(
                6.0,
                2.0,
                i as u32 * 31,
            ));
        }
        ctx.restore();
    }

    // — waterline —
    let opening = opening_path(open, squeeze);
    ctx.save();
    ctx.set_mask_filter(Some(&blur(3.5)));
    ctx.set_stroke_style(hex(WATER));
    ctx.set_line_width(7.0);
    ctx.stroke_path(&opening);
    ctx.restore();

    // — eyeball —
    ctx.save();
    ctx.clip_path(&opening, FillRule::NonZero);

    ctx.set_fill_shader(&radial(
        Point::new(gx - 20.0, gy - 16.0),
        24.0,
        Point::new(CX, CY),
        225.0,
        &[
            (0.0, RgbaLinear::from_srgb8(255, 255, 255, 1.0)),
            (0.42, hex(SCLERA)),
            (1.0, hex(SCLERA_SHADE)),
        ],
    )?);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);

    // Pink corner shading.
    for corner in [IN_X + 18.0, OUT_X - 18.0] {
        ctx.set_fill_shader(&radial(
            Point::new(corner, CY),
            4.0,
            Point::new(corner, CY),
            76.0,
            &[(0.0, rgba("#e08e86", 0.5)), (1.0, rgba("#e08e86", 0.0))],
        )?);
        ctx.fill_rect(corner - 80.0, CY - 80.0, 160.0, 160.0);
    }

    // Vasculature, forking, from both corners.
    ctx.set_stroke_style(hex("#b4483f"));
    for i in 0..24 {
        let rnd = r1(i as f32 * 91.7);
        let side = match i % 2 {
            0 => -1.0,
            _ => 1.0,
        };
        let x0 = match side < 0.0 {
            true => IN_X + 6.0,
            false => OUT_X - 6.0,
        };
        let y0 = CY - 30.0 + rnd * 60.0;
        let alpha = 0.08 + rnd * 0.2;
        ctx.set_global_alpha(alpha);
        ctx.set_line_width(0.7 + rnd * 1.4);
        let mut vessel = PathBuilder::new();
        vessel.move_to(x0, y0);
        vessel.quadratic_curve_to(
            x0 + side * (40.0 + rnd * 42.0),
            y0 - 24.0 + rnd * 46.0,
            x0 + side * (88.0 + rnd * 60.0),
            y0 - 6.0 + rnd * 28.0,
        );
        ctx.stroke_path(&vessel.build(FillRule::NonZero).jitter(
            5.0,
            2.6,
            i as u32 * 13 + 3,
        ));
        if rnd > 0.55 {
            let mut branch = PathBuilder::new();
            branch.move_to(x0 + side * 44.0, y0 - 6.0);
            branch.quadratic_curve_to(
                x0 + side * 74.0,
                y0 + 16.0,
                x0 + side * 106.0,
                y0 + 8.0,
            );
            ctx.set_global_alpha(alpha * 0.7);
            ctx.stroke_path(&branch.build(FillRule::NonZero).jitter(
                5.0,
                2.4,
                i as u32 * 29,
            ));
        }
    }
    ctx.set_global_alpha(1.0);

    // — iris —
    let mut iris = PathBuilder::new();
    iris.arc(gx, gy, R, 0.0, TAU, false)?;
    ctx.save();
    ctx.clip_path(&iris.build(FillRule::NonZero), FillRule::NonZero);

    ctx.set_fill_shader(&radial(
        Point::new(gx, gy),
        pupil_r * 0.8,
        Point::new(gx, gy),
        R,
        &[
            (0.0, hex("#1d5c7d")),
            (0.3, hex(MID)),
            (0.72, hex("#5aa8c8")),
            (1.0, hex(DEEP)),
        ],
    )?);
    ctx.fill_rect(gx - R, gy - R, R * 2.0, R * 2.0);

    ctx.set_line_cap(StrokeCap::Round);
    // Three passes of stromal fibre: dark base, mid strands, bright flecks.
    for (count, low, high, width, alpha, seed) in [
        (260, "#0d3550", "#1b5c7f", 3.4, 0.5, 3.0),
        (340, "#3f95bd", PALE, 2.0, 0.55, 11.0),
        (140, "#cdeef8", "#ffffff", 1.2, 0.4, 29.0),
    ] {
        for i in 0..count {
            let rnd = r1(i as f32 + seed);
            let a = (i as f32 / count as f32) * TAU + rnd * 0.03;
            let inner = pupil_r + 2.0 + rnd * 9.0;
            let outer = R * (0.64 + rnd * 0.38);
            ctx.set_stroke_style(hex(match rnd > 0.5 {
                true => high,
                false => low,
            }));
            ctx.set_global_alpha(alpha * (0.45 + rnd * 0.7));
            ctx.set_line_width(width * (0.4 + rnd));
            let mut fibre = PathBuilder::new();
            fibre.move_to(gx + a.cos() * inner, gy + a.sin() * inner);
            fibre.line_to(gx + a.cos() * outer, gy + a.sin() * outer);
            ctx.stroke_path(&fibre.build(FillRule::NonZero).jitter(
                5.0,
                2.1,
                i as u32 * 31 + seed as u32 * 7,
            ));
        }
    }

    // Crypts: dark radial pits in the stroma.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(2.0)));
    ctx.set_fill_style(hex("#0e3247"));
    for i in 0..16 {
        let rnd = r1(i as f32 * 4.4 + 9.0);
        let a = (i as f32 / 16.0) * TAU + rnd;
        let rr = pupil_r + 16.0 + rnd * (R * 0.5);
        ctx.set_global_alpha(0.22 + rnd * 0.2);
        ctx.save();
        ctx.translate(gx + a.cos() * rr, gy + a.sin() * rr);
        ctx.rotate(a);
        ctx.begin_path();
        ctx.ellipse(
            0.0,
            0.0,
            5.0 + rnd * 9.0,
            2.5 + rnd * 3.0,
            0.0,
            0.0,
            TAU,
            false,
        )?;
        ctx.fill(FillRule::NonZero);
        ctx.restore();
    }
    ctx.restore();

    // Contraction furrows: concentric arcs.
    ctx.set_stroke_style(hex("#123a52"));
    for (index, rr) in [pupil_r + 24.0, pupil_r + 38.0, R * 0.82]
        .into_iter()
        .enumerate()
    {
        ctx.set_global_alpha(0.2);
        ctx.set_line_width(1.6);
        let mut furrow = PathBuilder::new();
        furrow.arc(gx, gy, rr.min(R - 6.0), 0.0, TAU, false)?;
        ctx.stroke_path(&furrow.build(FillRule::NonZero).jitter(
            6.0,
            2.4,
            60 + index as u32 * 17,
        ));
    }

    // Collarette: gold sunburst spokes.
    for i in 0..72 {
        let rnd = r1(i as f32 * 55.3);
        let a = (i as f32 / 72.0) * TAU;
        ctx.set_stroke_style(hex(match rnd > 0.5 {
            true => "#d9a441",
            false => GOLD,
        }));
        ctx.set_global_alpha(0.55 + rnd * 0.45);
        ctx.set_line_width(2.0 + rnd * 3.4);
        let mut spoke = PathBuilder::new();
        spoke.move_to(
            gx + a.cos() * (pupil_r + 1.0),
            gy + a.sin() * (pupil_r + 1.0),
        );
        spoke.line_to(
            gx + a.cos() * (pupil_r + 14.0 + rnd * 15.0),
            gy + a.sin() * (pupil_r + 14.0 + rnd * 15.0),
        );
        ctx.stroke_path(&spoke.build(FillRule::NonZero).jitter(
            4.0,
            1.8,
            i as u32 * 23,
        ));
    }
    ctx.set_global_alpha(1.0);

    // Caustic: light dumped through the lens onto the far side of the iris.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(9.0)));
    ctx.set_fill_style(rgba("#beeefc", 0.4));
    ctx.begin_path();
    ctx.ellipse(
        gx + R * 0.34,
        gy + R * 0.44,
        R * 0.5,
        R * 0.3,
        0.5,
        0.0,
        TAU,
        false,
    )?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();

    // Limbal ring: blurred, because a real limbus is a gradient not a stroke.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(5.0)));
    ctx.set_stroke_style(hex("#0b1d2b"));
    ctx.set_line_width(15.0);
    ctx.set_global_alpha(0.9);
    ctx.begin_path();
    ctx.arc(gx, gy, R - 3.0, 0.0, TAU, false)?;
    ctx.stroke();
    ctx.restore();

    // Corneal sheen: a faint film of light over the upper iris.
    ctx.set_fill_shader(&Shader::linear_gradient(
        Point::new(0.0, gy - R),
        Point::new(0.0, gy + R * 0.35),
        &stops(&[
            (0.0, RgbaLinear::from_srgb8(255, 255, 255, 0.15)),
            (1.0, RgbaLinear::from_srgb8(255, 255, 255, 0.0)),
        ]),
        GradientColorSpace::Srgb,
    )?);
    ctx.fill_rect(gx - R, gy - R, R * 2.0, R * 1.4);
    ctx.set_global_alpha(1.0);
    ctx.restore(); // end iris clip

    // Pupil.
    ctx.set_fill_style(hex("#060406"));
    ctx.begin_path();
    ctx.arc(gx, gy, pupil_r, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.save();
    ctx.set_mask_filter(Some(&blur(3.0)));
    ctx.set_stroke_style(rgba("#060406", 0.7));
    ctx.set_line_width(4.0);
    ctx.begin_path();
    ctx.arc(gx, gy, pupil_r + 1.5, 0.0, TAU, false)?;
    ctx.stroke();
    ctx.restore();

    // The upper lid's shadow falling on the ball, tracking the lid itself.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(15.0)));
    ctx.set_fill_style(rgba("#40221c", 0.4));
    let mut band = PathBuilder::new();
    for i in 0..=36 {
        let u = i as f32 / 36.0;
        let y = upper_y(u, open, squeeze) + 12.0;
        match i {
            0 => band.move_to(at_x(u), y),
            _ => band.line_to(at_x(u), y),
        };
    }
    for i in (0..=36).rev() {
        let u = i as f32 / 36.0;
        band.line_to(at_x(u), upper_y(u, open, squeeze) - 26.0);
    }
    band.close_path();
    ctx.fill_path(&band.build(FillRule::NonZero), FillRule::NonZero);
    ctx.restore();

    // Wet meniscus along the lower lid, plus one sparkle.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(1.6)));
    ctx.set_stroke_style(RgbaLinear::from_srgb8(255, 255, 255, 0.5));
    ctx.set_line_width(2.4);
    ctx.stroke_path(&along(4, 44, 48.0, |u| lower_y(u, squeeze) - 3.0));
    ctx.restore();
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 0.85));
    ctx.begin_path();
    ctx.arc(
        at_x(0.72),
        lower_y(0.72, squeeze) - 5.0,
        2.2,
        0.0,
        TAU,
        false,
    )?;
    ctx.fill(FillRule::NonZero);

    // Catchlights: window shapes, drawn over the pupil edge.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(7.0)));
    ctx.set_fill_style(rgba("#bee1ff", 0.6));
    ctx.begin_path();
    ctx.ellipse(clx - 34.0, cly - 36.0, 19.0, 15.0, -0.5, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 0.97));
    ctx.begin_path();
    ctx.round_rect(clx - 44.0, cly - 46.0, 19.0, 17.0, [2.5; 4])?;
    ctx.fill(FillRule::NonZero);
    ctx.begin_path();
    ctx.round_rect(clx - 22.0, cly - 26.0, 10.0, 9.0, [2.0; 4])?;
    ctx.fill(FillRule::NonZero);
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 0.45));
    ctx.begin_path();
    ctx.ellipse(clx + 30.0, cly + 34.0, 8.0, 5.0, 0.4, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);

    ctx.restore(); // end opening clip

    // — tear duct: the lids close over it, so it fades with the opening —
    let duct_alpha = ((open - 0.18) / 0.45).clamp(0.0, 1.0);
    if duct_alpha > 0.02 {
        ctx.save();
        ctx.set_global_alpha(duct_alpha);
        ctx.set_mask_filter(Some(&blur(3.0)));
        let mut duct = PathBuilder::new();
        duct.move_to(IN_X + 2.0, CY - 14.0);
        duct.quadratic_curve_to(IN_X + 34.0, CY - 6.0, IN_X + 30.0, CY + 10.0);
        duct.quadratic_curve_to(IN_X + 16.0, CY + 20.0, IN_X + 2.0, CY - 14.0);
        duct.close_path();
        ctx.set_fill_style(hex("#d4726f"));
        ctx.fill_path(&duct.build(FillRule::NonZero), FillRule::NonZero);
        ctx.set_fill_style(hex("#f2b0a8"));
        ctx.begin_path();
        ctx.ellipse(IN_X + 16.0, CY - 1.0, 8.0, 6.0, -0.3, 0.0, TAU, false)?;
        ctx.fill(FillRule::NonZero);
        ctx.restore();
        ctx.set_global_alpha(1.0);
    }

    // — lid margins —
    ctx.set_stroke_style(hex("#8e3f3d"));
    ctx.set_line_width(3.5);
    ctx.stroke_path(&opening_path(open, squeeze));

    // Dark lash-line shelf on the upper lid.
    ctx.save();
    ctx.set_mask_filter(Some(&blur(2.5)));
    ctx.set_stroke_style(rgba("#281212", 0.8));
    ctx.set_line_width(6.0);
    let shelf = along(0, 40, 40.0, |u| upper_y(u, open, squeeze) - 3.0);
    ctx.stroke_path(&shelf.jitter(7.0, 1.2, frame as u32 * 7 + 41));

    // Light platform under the lower lashes.
    ctx.set_mask_filter(Some(&blur(4.0)));
    ctx.set_stroke_style(rgba("#f8ded2", 0.55));
    ctx.set_line_width(7.0);
    ctx.stroke_path(&along(2, 38, 40.0, |u| lower_y(u, squeeze) + 8.0));
    ctx.restore();

    // — upper lashes: fill row, main row, hero row; all rotate with the lid —
    ctx.set_fill_style(hex(LASH));
    let ghost = (lid_velocity.abs() * 0.05).min(0.35);
    for (count, seed_offset, len_mul, wide_mul, alpha, lag_gain) in [
        (64usize, 5usize, 0.6, 0.7, 0.55, 0.8), // short fill behind
        (78, 17, 1.0, 1.0, 0.95, 1.0),          // main row
        (24, 45, 1.5, 1.25, 1.0, 1.3),          // long hero lashes
    ] {
        for i in 0..count {
            let rnd = r1(i as f32 * 3.3 + seed_offset as f32);
            let u = 0.04 + (i as f32 / (count - 1) as f32) * 0.93;
            let x = at_x(u);
            let y = upper_y(u, open, squeeze);
            let grow = arc(u, 0.3);
            let len = (44.0 + rnd * 34.0) * grow * (0.5 + u * 0.85) * len_mul;
            let index = (i * 7 + seed_offset) % LASH_COUNT;
            // Real lashes gather into clumps of three or four whose tips
            // converge; a shared per-clump bias does that without modelling
            // adhesion.
            let clump =
                (r1((i >> 2) as f32 * 13.0 + seed_offset as f32) - 0.5) * 0.17;
            let (angle, squash) = upper_lash_aim(
                u,
                open,
                eye.lash_lag[index].value * lag_gain
                    + clump
                    + (rnd - 0.5) * 0.06,
            );
            // Foreshortened as it swings through the viewer: a lash pointing
            // at the camera is short on the page, which is most of why the
            // fan reads as sitting on a curved lid rather than a flat one.
            let shown = len * squash;
            let curl =
                (0.6 + rnd * 0.32) * (0.35 + 0.65 * open.clamp(0.0, 1.0));
            let wide = (1.4 + rnd * 1.8) * grow * wide_mul;
            ctx.set_global_alpha(alpha * (0.75 + rnd * 0.25));
            ctx.fill_path(
                &lash_path(x, y + 1.0, angle, shown, curl, wide),
                FillRule::NonZero,
            );
            if ghost > 0.04 {
                // Motion blur on the snap.
                ctx.set_global_alpha(ghost * alpha);
                ctx.fill_path(
                    &lash_path(
                        x,
                        y + 1.0,
                        angle - lid_velocity * 0.016,
                        shown,
                        curl,
                        wide,
                    ),
                    FillRule::NonZero,
                );
            }
        }
    }

    // — lower-lash shadows cast on the skin, a few px below their owners —
    if open > 0.5 {
        ctx.save();
        ctx.set_mask_filter(Some(&blur(2.5)));
        ctx.set_fill_style(rgba("#5e302a", 0.16));
        for i in (0..34).step_by(2) {
            let rnd = r1(i as f32 * 78.2);
            let u = 0.12 + (i as f32 / 33.0) * 0.8;
            let grow = arc(u, 0.5);
            let len = (17.0 + rnd * 15.0) * grow;
            let angle = PI / 2.0 - 0.42 + u * 0.9;
            ctx.fill_path(
                &lash_path(
                    at_x(u),
                    lower_y(u, squeeze) + 7.0,
                    angle,
                    len,
                    0.24,
                    1.1 * grow,
                ),
                FillRule::NonZero,
            );
        }
        ctx.restore();
    }

    // — lower lashes: shorter, sparser, pressed down by the squeeze —
    for i in 0..34 {
        let rnd = r1(i as f32 * 78.2);
        let u = 0.12 + (i as f32 / 33.0) * 0.8;
        let grow = arc(u, 0.5);
        let len = (17.0 + rnd * 15.0) * grow * (1.0 - wink_depth * 0.25);
        let angle = PI / 2.0 - 0.5
            + u * 0.95
            + eye.lash_lag[(i * 5 + 90) % LASH_COUNT].value * 0.5
            + wink_depth * 0.3;
        ctx.set_global_alpha(0.6 + rnd * 0.3);
        ctx.fill_path(
            &lash_path(
                at_x(u),
                lower_y(u, squeeze) - 1.0,
                angle,
                len,
                0.3,
                (0.9 + rnd * 0.8) * grow,
            ),
            FillRule::NonZero,
        );
    }
    ctx.set_global_alpha(1.0);

    // — film grain, reseeded each frame: the shimmer of a drawn frame —
    ctx.save();
    for i in 0..260 {
        let gxr = r1(i as f32 * 1.9 + frame as f32 * 37.7) * WIDTH;
        let gyr = r1(i as f32 * 4.3 + frame as f32 * 91.1) * HEIGHT;
        ctx.set_global_alpha(0.028);
        ctx.set_fill_style(match r1(i as f32 + frame as f32) > 0.5 {
            true => hex("#3a2c28"),
            false => RgbaLinear::from_srgb8(255, 255, 255, 1.0),
        });
        ctx.fill_rect(gxr, gyr, 1.4, 1.4);
    }
    ctx.restore();
    ctx.set_global_alpha(1.0);

    // — vignette into the paper —
    ctx.set_fill_shader(&radial(
        Point::new(CX, CY - 20.0),
        210.0,
        Point::new(CX, CY - 20.0),
        400.0,
        &[(0.0, rgba("#f6f2ee", 0.0)), (1.0, rgba("#f6f2ee", 0.9))],
    )?);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    Ok(())
}

// ── the timeline ───────────────────────────────────────────────────────────

/// Smoothstep, which is what every eased segment of the timeline uses.
fn smoothstep(q: f32) -> f32 {
    q * q * (3.0 - 2.0 * q)
}

/// Where the lid is asked to be at `t`, and where the gaze is asked to look.
///
/// Saccades, one quick natural blink, then the dramatic wink: widen, snap
/// shut, hold, and a slow reopen.
fn timeline(t: f32, eye: &mut Eye) -> f32 {
    for (from, to, target) in [
        (0.0, 0.005, (0.0, 0.0)),
        (0.1, 0.11, (-26.0, -8.0)),
        (0.26, 0.27, (20.0, 6.0)),
        (0.44, 0.45, (0.0, 0.0)),
    ] {
        if t >= from && t < to {
            eye.gaze_target = target;
        }
    }

    // A quick natural blink, centred on t = 0.34.
    let natural = (t - 0.34).abs();
    let mut target = match natural < 0.045 {
        true => smoothstep(natural / 0.045),
        false => 1.0,
    };

    if (0.55..0.62).contains(&t) {
        // Anticipation: widen.
        target = 1.07;
    } else if (0.62..0.655).contains(&t) {
        // Snap shut.
        target = 1.07 * (1.0 - smoothstep((t - 0.62) / 0.035));
    } else if (0.655..0.76).contains(&t) {
        // Hold.
        target = 0.0;
    } else if (0.76..0.88).contains(&t) {
        // Slow reopen.
        target = smoothstep((t - 0.76) / 0.12);
    }
    eye.lid_target = target;

    match t > 0.56 && t < 0.97 {
        true => (1.0 - eye.lid.value.max(0.0)).max(0.0),
        false => 0.0,
    }
}

const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

fn main() -> Result<(), Box<dyn Error>> {
    let out =
        PathBuf::from(env::args().nth(1).unwrap_or_else(|| "out".to_string()));
    fs::create_dir_all(&out)?;

    // GPU by default, which is what you want for anything this stroke-heavy.
    // Set `MEO_EYE_CPU=1` to force the raster path: the two renderers
    // antialias differently -- measurably, 19% of bytes on the same drawing
    // -- so pinning the CPU is what makes a committed asset byte-identical
    // between machines. That matters for a regenerated file in a repo and
    // for nothing else.
    let mut canvas = Canvas::with_options(
        WIDTH,
        HEIGHT,
        CanvasOptions {
            color_space: PixelColorSpace::DisplayP3,
            gpu: env::var_os("MEO_EYE_CPU").is_none(),
            ..CanvasOptions::default()
        },
    )?;

    let mut eye = Eye::new();
    let mut accumulator = 0.0f32;

    for frame in 0..FRAMES {
        let t = frame as f32 / FRAMES as f32;
        let wink_depth = timeline(t, &mut eye);

        // Fixed-step integration with an accumulator, so the springs settle
        // identically whatever FPS is set to.
        accumulator += 1.0 / FPS;
        while accumulator >= DT {
            eye.step(t, wink_depth);
            accumulator -= DT;
        }
        eye.squeeze = wink_depth * 0.9;

        let open = eye.lid.value;
        let lid_velocity = eye.lid.velocity;
        let ctx = match frame {
            0 => canvas.context(),
            _ => canvas.new_page(),
        };
        draw_frame(ctx, &eye, frame, open, lid_velocity, wink_depth)?;
    }

    // APNG rather than GIF, for two reasons that are both arithmetic.
    //
    // GIF stores a frame delay in hundredths of a second, so a 60fps frame --
    // 16.67ms -- is not a whole number of them. The delays are handed out as
    // differences between running totals rather than rounded one at a time,
    // so the average comes out right, but the individual frames alternate
    // between 10 and 20ms and the format cannot do better than that. APNG
    // stores a fraction and lands on 60fps exactly.
    //
    // The other reason is colour. GIF has a 256-entry palette per frame, and
    // this drawing is mostly smooth gradient -- skin, sclera, iris -- which
    // is exactly what banding shows up in worst. APNG is full RGBA.
    //
    // The GIF is written too, for anywhere that will not take an APNG.
    for (name, format) in
        [("apng", ImageFormat::Apng), ("gif", ImageFormat::Gif)]
    {
        let file = out.join(format!("animated-eye.{name}"));
        canvas.to_file(
            &file,
            &EncodeOptions {
                fps: Some(FPS),
                loops: Some(0),
                ..EncodeOptions::default()
            },
        )?;
        // The GIF is written at the rate it was asked for and the file says
        // so. Browsers will not play it: anything at or under 10ms is
        // rendered at 100ms, and 60fps needs frames of 16.67. Named here
        // rather than left for someone to discover from a GIF that limps --
        // and named as the browsers' behaviour, because nothing in this
        // library caps anything and a native viewer will play it as written.
        let caveat = match format {
            ImageFormat::Gif => " (browsers play <=10ms frames at 100ms)",
            _ => "",
        };
        println!(
            "{:<20} {}x{} {FRAMES} frames @ {FPS}fps{caveat} {:.1} MB",
            format!("animated-eye.{name}"),
            canvas.width(),
            canvas.height(),
            fs::metadata(&file)?.len() as f64 / BYTES_PER_MIB
        );
    }
    Ok(())
}
