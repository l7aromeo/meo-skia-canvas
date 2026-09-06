#![allow(unused_mut)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(non_snake_case)]
#![allow(dead_code)]
use neon::prelude::*;
use skia_safe::{
    Matrix, Path, PathBuilder, PathDirection, PathEffect, PathFillType, PathOp,
    Point, RRect, Rect, StrokeRec,
    path::{self, AddPathMode, Verb},
    trim_path_effect,
};
use std::{cell::RefCell, f32::consts::PI};

use crate::{
    node::verbs::verbs,
    path::{FillRule, Path2D as CratePath, PathSegment},
    utils::*,
};

/// The scale [`round_degrees`] rounds at: four decimal places.
const DEGREE_PRECISION: f32 = 10_000.0;

/// `degrees` rounded to [`DEGREE_PRECISION`].
fn round_degrees(degrees: f32) -> f32 {
    (degrees * DEGREE_PRECISION).round() / DEGREE_PRECISION
}

pub type BoxedPath2D = JsBox<RefCell<Path2D>>;
impl Finalize for Path2D {}

/// A path being drawn, and the path it has drawn so far.
///
/// Both halves are optional and at least one is always present, which is
/// what lets each be absent when it would only be built to be thrown away.
/// A path assembled segment by segment has a builder and takes its snapshot
/// when something reads it; a path that arrived whole -- from an effect,
/// from SVG, from another path -- has the snapshot and never makes a builder
/// unless something appends to it, which is the rarer half: an effect's
/// result usually goes straight into a fill.
pub struct Path2D {
    /// The builder, made only when something appends.
    ///
    /// Private, so nothing can append without going through
    /// [`Path2D::builder_mut`] and dropping the snapshot with it.
    builder: Option<PathBuilder>,
    /// The path, taken from the builder or handed over whole.
    ///
    /// [`Path2D::path`] is reached by every read of a path and by every fill
    /// or stroke that names one, and `PathBuilder::snapshot` walks the whole
    /// builder to answer. So the cost of using a path grew with the path:
    /// filling a 2000-segment one took 4.1 microseconds and did the same
    /// work again on the next frame, against 0.29 for a path of ten.
    ///
    /// A `RefCell` because `path` takes `&self` -- it is reached through a
    /// `JsBox`'s `borrow`, and every caller of it holds a shared reference.
    cache: RefCell<Option<Path>>,
}

impl Default for Path2D {
    fn default() -> Self {
        Self {
            builder: Some(PathBuilder::new()),
            cache: RefCell::new(None),
        }
    }
}

impl From<PathBuilder> for Path2D {
    fn from(builder: PathBuilder) -> Self {
        Self {
            builder: Some(builder),
            cache: RefCell::new(None),
        }
    }
}

/// A path that is already drawn.
///
/// No builder: Skia hands a filtered path back as a `PathBuilder`, and the
/// effects here take the path out of it, so rebuilding one to hold it walked
/// the result a second time to arrive where it started.
impl From<Path> for Path2D {
    fn from(path: Path) -> Self {
        Self {
            builder: None,
            cache: RefCell::new(Some(path)),
        }
    }
}

/// Append a conic, degenerating to a line for a non-positive weight.
///
/// `SkPath::conicTo` opened with `if (!(w > 0)) { this->lineTo(x2, y2); }`, so
/// a zero or negative weight drew a straight line to the end point.
/// `SkPathBuilder::conicTo` dropped that guard and stores the weight as given
/// -- and a negative weight makes the rational denominator cross zero, which is
/// undefined rather than merely different. Non-finite weights never reach here;
/// the argument coercion rejects them first.
pub fn conic_or_line(
    builder: &mut PathBuilder,
    ctrl: impl Into<Point>,
    end: impl Into<Point>,
    weight: f32,
) {
    let end = end.into();
    if weight > 0.0 {
        builder.conic_to(ctrl, end, weight);
    } else {
        builder.line_to(end);
    }
}

impl Path2D {
    /// Gets an immutable `Path` snapshot for rendering.
    /// The path this has built, snapshotted once per change.
    pub fn path(&self) -> Path {
        if let Some(path) = self.cache.borrow().as_ref() {
            return path.clone();
        }
        // Cheap to hand back: an `SkPath` is copy-on-write, so the clone
        // above and this one are a reference count rather than the geometry.
        // The empty path is unreachable -- one of the two halves is always
        // present, and the cache is the one that is not -- and is an answer
        // rather than a panic because an empty path is what an empty
        // `Path2D` would have given anyway.
        let path = self
            .builder
            .as_ref()
            .map(PathBuilder::snapshot)
            .unwrap_or_default();
        *self.cache.borrow_mut() = Some(path.clone());
        path
    }

    /// The builder, for appending to it.
    ///
    /// Taking this drops the snapshot, which is the only reason the builder
    /// is not a public field: a caller that appended straight to it would
    /// leave a stale path behind and nothing would say so. A path that
    /// arrived whole grows a builder here, which is the one place the walk
    /// this arrangement avoids is actually paid.
    pub fn builder_mut(&mut self) -> &mut PathBuilder {
        let built = self.cache.get_mut().take();
        self.builder.get_or_insert_with(|| match built {
            Some(path) => PathBuilder::new_path(&path),
            None => PathBuilder::new(),
        })
    }

    pub fn scoot(&mut self, x: f32, y: f32) {
        // Asked of whichever half is present, and of neither in a way that
        // walks it. This runs before every segment append, and `snapshot()`
        // copies the whole path, which makes construction quadratic: 16k
        // lineTo calls take 134 ms that way against 3.9 ms here. `verbs()`
        // on a builder and `is_empty()` on a path are both O(1).
        let empty = match (&self.builder, self.cache.borrow().as_ref()) {
            (Some(builder), _) => builder.verbs().is_empty(),
            (None, Some(path)) => path.is_empty(),
            // Unreachable: one of the two is always present.
            (None, None) => true,
        };
        if empty {
            self.builder_mut().move_to((x, y));
        }
    }

    pub fn add_ellipse(
        &mut self,
        origin: impl Into<Point>,
        radii: impl Into<Point>,
        rotation: f32,
        start_angle: f32,
        end_angle: f32,
        ccw: bool,
    ) {
        let Point { x, y } = origin.into();
        let Point {
            x: x_radius,
            y: y_radius,
        } = radii.into();

        // based off of CanonicalizeAngle in Chrome
        let tau = 2.0 * PI;
        let mut new_start_angle = start_angle % tau;
        if new_start_angle < 0.0 {
            new_start_angle += tau;
        }
        let delta = new_start_angle - start_angle;
        let start_angle = new_start_angle;
        let mut end_angle = end_angle + delta;

        // Originally based off of AdjustEndAngle in Chrome, but does not limit
        // to 360 degree sweep.
        if !ccw && start_angle > end_angle {
            end_angle = start_angle + (tau - (start_angle - end_angle) % tau);
        } else if ccw && start_angle < end_angle {
            end_angle = start_angle - (tau - (end_angle - start_angle) % tau);
        }

        let oval =
            Rect::new(x - x_radius, y - y_radius, x + x_radius, y + y_radius);

        let mut rotated = Matrix::new_identity();
        rotated
            .pre_translate((x, y))
            .pre_rotate(rotation.to_degrees(), None)
            .pre_translate((-x, -y));

        // Based off of Chrome's implementation in
        // https://cs.chromium.org/chromium/src/third_party/blink/renderer/platform/graphics/path.cc
        // of note, can't use addArc or addOval because they close the arc,
        // which the spec says not to do (unless the user
        // explicitly calls closePath). This throws off points
        // being in/out of the arc.

        // Rounded before the comparisons below, which ask whether a
        // sweep has reached a whole turn. Converting radians to degrees
        // in `f32` leaves a full circle a hair either side of 360, so
        // an unrounded comparison decides the same arc differently
        // depending on how the angle was arrived at.
        //
        // Four decimals: far finer than any angle a caller can mean --
        // a ten-thousandth of a degree is a third of an arcsecond --
        // and coarse enough to swallow the conversion error, which is
        // around 1e-5 degrees at the magnitudes a canvas uses.
        let sweep_deg = round_degrees((end_angle - start_angle).to_degrees());
        let start_deg = round_degrees(start_angle.to_degrees());

        // draw 360° ellipses in two 180° segments; trying to draw the full
        // ellipse at once draws nothing.
        let sweep = |arc: &mut PathBuilder| {
            if sweep_deg >= 360.0 - f32::EPSILON {
                arc.arc_to(oval, start_deg, 180.0, false);
                arc.arc_to(oval, start_deg + 180.0, 180.0, false);
            } else if sweep_deg <= -360.0 + f32::EPSILON {
                arc.arc_to(oval, start_deg, -180.0, false);
                arc.arc_to(oval, start_deg - 180.0, -180.0, false);
            } else {
                // Draw incomplete (< 360°) ellipses in a single arc.
                arc.arc_to(oval, start_deg, sweep_deg, false);
            }
        };

        // Unrotated, the arc goes straight into the path. `arc_to` with
        // `force_move_to` false already continues the current contour with a
        // connecting line, which is the whole of what `AddPathMode::Extend`
        // was providing below -- so the detour through a second builder, a
        // `detach` and a transformed copy of every verb bought nothing.
        //
        // That is not a rare case. `arc()` has no rotation to pass and hands
        // in a literal zero, and an `ellipse()` is usually axis-aligned too.
        if rotation == 0.0 {
            sweep(self.builder_mut());
            return;
        }

        let mut arc = PathBuilder::new();
        sweep(&mut arc);

        // The arc is built on its own and added transformed, rather than the
        // path being rotated into the arc's frame and back around it.
        //
        // Rotating the whole path twice per call is what this used to do, and
        // it made building one quadratic: an ellipse cost 12 microseconds on a
        // 250-segment path and 76 on a 2000-segment one, where a path of
        // straight lines stays flat at about a quarter of a microsecond.
        //
        // Extend, so the arc continues the current contour with a connecting
        // line, which is what rotating the path around an `arc_to` did.
        self.builder_mut().add_path_with_transform(
            &arc.detach(),
            &rotated,
            AddPathMode::Extend,
        );
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

//
// -- Drawing verbs
// --------------------------------------------------------------------------
//

verbs! {
    PathVerb for BoxedPath2D => Path2D;

    // A subpath opens where it is told to, so this one does not `scoot`.
    moveTo as MoveTo (x, y) => |path| {
        path.builder_mut().move_to((x, y));
    },

    // `scoot` first, here and below: a segment added to an empty path opens
    // the subpath at its own first point, as the Canvas API says.
    lineTo as LineTo (x, y) => |path| {
        path.scoot(x, y);
        path.builder_mut().line_to((x, y));
    },

    quadraticCurveTo as QuadraticCurveTo (cpx, cpy, x, y) => |path| {
        path.scoot(cpx, cpy);
        path.builder_mut().quad_to((cpx, cpy), (x, y));
    },

    bezierCurveTo as BezierCurveTo (cp1x, cp1y, cp2x, cp2y, x, y) => |path| {
        path.scoot(cp1x, cp1y);
        path.builder_mut().cubic_to((cp1x, cp1y), (cp2x, cp2y), (x, y));
    },

    conicCurveTo as ConicCurveTo (cpx, cpy, x, y, weight) => |path| {
        path.scoot(cpx, cpy);
        conic_or_line(path.builder_mut(), (cpx, cpy), (x, y), weight);
    },

    // Always clockwise, over a rect left inverted by a negative dimension:
    // traversing an inverted rect is what reverses the winding. Choosing the
    // direction from the signs as well reversed it a second time and cancelled
    // the effect, so a rect drawn with one negative dimension inside another
    // filled solid where a browser -- and `ctx.rect`, which passes the default
    // -- punches a hole.
    //
    // `roundRect` is the opposite case and keeps the sign rule: an `RRect`
    // normalises the rect it is built from, so nothing else is left to carry
    // the reversal.
    rect as Rect (x, y, width, height) => |path| {
        let rect = Rect::from_xywh(x, y, width, height);
        path.builder_mut().add_rect(rect, PathDirection::CW, 0);
    },

    // The context's `arcTo` has always rejected a negative radius; the path's
    // had no guard at all until the constraint moved into the declaration.
    arcTo as ArcTo (x1, y1, x2, y2, radius @ non_negative) => |path| {
        path.scoot(x1, y1);
        path.builder_mut().arc_to_tangent((x1, y1), (x2, y2), radius);
    },

    // A negative radius is refused rather than ignored -- the one place these
    // verbs differ from every other coordinate they take, and what a browser
    // does for both.
    arc as Arc (x, y, radius @ non_negative, startAngle, endAngle); ccw => |path| {
        path.add_ellipse((x, y), (radius, radius), 0.0, startAngle, endAngle, ccw);
    },

    ellipse as Ellipse (
        x, y, xRadius @ non_negative, yRadius @ non_negative,
        rotation, startAngle, endAngle
    ); ccw => |path| {
        path.add_ellipse(
            (x, y),
            (xRadius, yRadius),
            rotation,
            startAngle,
            endAngle,
            ccw,
        );
    },

    closePath as ClosePath () => |path| {
        path.builder_mut().close();
    },

    // The form that takes no matrix, which is the one a loop building a
    // composite path uses. The matrix form stays hand-written below: a
    // `DOMMatrix` is an object, and a record holds numbers, strings and
    // handles.
    appendPath as AppendPath (other @ handle) => |path| {
        path.builder_mut().add_path_with_transform(
            &other,
            &Matrix::new_identity(),
            AddPathMode::Append,
        );
    },

    // One radius for all four corners. The other form takes eight numbers
    // that JavaScript worked out from a CSS value, and only the uniform one
    // survives that parse as a single number.
    //
    // The start index is pinned to 0 here and left to Skia's 6/7 in the
    // context's verb of the same shape. That asymmetry is deliberate and
    // load-bearing -- it decides where `AddPathMode::Extend` attaches, where
    // the current point lands, and where a dash phase begins -- so this
    // mirrors the hand-written `roundRect` below rather than the context's.
    roundRectUniform as RoundRectUniform (
        x, y, width, height, radius @ non_negative
    ) => |path| {
        let rect = Rect::from_xywh(x, y, width, height);
        let radii = [Point::new(radius, radius); 4];
        let rrect = RRect::new_rect_radii(rect, &radii);
        let direction = if width.signum() == height.signum() {
            PathDirection::CW
        } else {
            PathDirection::CCW
        };
        path.builder_mut().add_rrect(rrect, direction, 0);
    },
}

/// A `Path2D` holding what `path` holds.
///
/// The binding's own type is a builder, and every operation below now goes
/// through `crate::path::Path2D` rather than reaching into `skia_safe` -- so
/// an operation the crate does not expose is one the binding cannot reach
/// either, which is what stopped these accreting on one surface only.
fn from_crate(path: &CratePath) -> Path2D {
    // The effect's own result, held as it is. Rebuilding a `PathBuilder`
    // around it walked the whole thing to arrive back where it started, and
    // a path an effect produced usually goes straight into a draw without
    // anything ever appending to it.
    Path2D::from(path.to_skia())
}

/// The crate's fill rule for one of Skia's.
fn from_skia_rule(rule: PathFillType) -> FillRule {
    match rule {
        PathFillType::EvenOdd | PathFillType::InverseEvenOdd => {
            FillRule::EvenOdd
        }
        _ => FillRule::NonZero,
    }
}

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    Ok(cx.boxed(RefCell::new(Path2D::default())))
}

pub fn from_path(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let other_path = path2d_arg(&mut cx, 1)?;
    let path = other_path.borrow().path();
    Ok(cx.boxed(RefCell::new(Path2D::from(path))))
}

pub fn from_svg(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let svg_string = string_arg(&mut cx, 1, "svgPath")?;
    // Empty rather than an error, because that is what the constructor is
    // defined to do: the Canvas specification says a `new Path2D(d)` whose
    // data does not parse yields an empty path, and the browsers and upstream
    // agree. Skia's parser is all-or-nothing, so a string with a valid prefix
    // loses that too -- `"M 0 0 L nonsense"` gives `""`, not `"M 0 0"`. The
    // default is the empty path, so `unwrap_or_default` is the mapping; the
    // shape is clippy's, which refuses the `match` that spells it out.
    //
    // `set_d` below throws on the same string, and the difference is
    // deliberate: `d` is this fork's own accessor rather than a Canvas API
    // member, so nothing defines it as forgiving, and an assignment that
    // silently emptied a path would be a worse answer than an error.
    let path = Path::from_svg(svg_string).unwrap_or_default();
    Ok(cx.boxed(RefCell::new(Path2D::from(path))))
}

// Adds a path to the current path.
pub fn addPath(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let other = path2d_arg(&mut cx, 1)?;
    let matrix =
        opt_matrix_arg(&mut cx, 2).unwrap_or_else(Matrix::new_identity);

    // Always a copy: path() snapshots, so the borrow is released before
    // borrow_mut() below and adding a path to itself cannot panic. A ref
    // would avoid the copy in the non-self case; the copy costs a little
    // and removes the special case.
    let src = other.borrow().path();
    this.borrow_mut().builder_mut().add_path_with_transform(
        &src,
        &matrix,
        AddPathMode::Append,
    );

    Ok(cx.undefined())
}

// Adds an arc to the path which is centered at (x, y) position with radius r
// starting at startAngle and ending at endAngle going in the given direction by
// anticlockwise (defaulting to clockwise).

// Adds a circular arc to the path with the given control points and radius,
// connected to the previous point by a straight line.

// Adds an elliptical arc to the path which is centered at (x, y) position with
// the radii radiusX and radiusY starting at startAngle and ending at endAngle
// going in the given direction by anticlockwise (defaulting to clockwise).

// Creates a path for a rounded rectangle at position (x, y) with a size (w, h)
// and whose radii are specified in x/y pairs for top_left, top_right,
// bottom_right, and bottom_left
pub fn roundRect(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let mut this = this.borrow_mut();

    // A non-finite coordinate makes the call a no-op, as it does in the
    // other eight path methods and in a browser. It has to be detected
    // before the reader runs: `float_args` refuses a non-finite number and a
    // `Symbol` with the same message, because `_as_double` maps both to
    // `None`, so switching this method to the strict-only reader would have
    // made `roundRect` ignore a `Symbol` too -- which
    // `tests/suite/arguments.test.js` pins as a throw, and which a browser
    // throws for.
    if converts_to_non_finite(&mut cx, 1..13) {
        return Ok(cx.undefined());
    }

    let nums = float_args(
        &mut cx,
        &[
            "x", "y", "width", "height", "r1x", "r1y", "r2x", "r2y", "r3x",
            "r3y", "r4x", "r4y",
        ],
    )?;
    if let [x, y, w, h] = &nums[..4] {
        let rect = Rect::from_xywh(*x, *y, *w, *h);
        let radii: Vec<Point> = nums[4..]
            .chunks(2)
            .map(|xy| Point::new(xy[0], xy[1]))
            .collect();
        let rrect = RRect::new_rect_radii(
            rect,
            &[radii[0], radii[1], radii[2], radii[3]],
        );
        let direction = if w.signum() == h.signum() {
            PathDirection::CW
        } else {
            PathDirection::CCW
        };
        // Start index pinned to 0. Skia m86 changed the default from 0 to
        // 6/7 depending on direction, which reorders the contour's points --
        // visible through Path2D.d, dash phase, and where
        // AddPathMode::Extend joins.
        this.builder_mut().add_rrect(rrect, direction, 0);
    }

    Ok(cx.undefined())
}

// Applies a boolean operator to this and a second path, returning a new Path2D
// with their combination
pub fn op(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let other_path = path2d_arg(&mut cx, 1)?;
    let op_name = string_arg(&mut cx, 2, "pathOp")?;

    if let Some(path_op) = to_path_op(&op_name) {
        let this = this.borrow();
        let other = other_path.borrow();
        match CratePath::from_inner(this.path())
            .combine(&CratePath::from_inner(other.path()), path_op)
        {
            Some(path) => Ok(cx.boxed(RefCell::new(from_crate(&path)))),
            None => cx.throw_error("path operation failed"),
        }
    } else {
        cx.throw_error(
            "pathOp must be Difference, Intersect, Union, XOR, or Complement",
        )
    }
}

pub fn interpolate(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let other = path2d_arg(&mut cx, 1)?;
    let weight = float_arg(&mut cx, 2, "weight")?;

    let this = this.borrow();
    let other = other.borrow();
    match CratePath::from_inner(this.path())
        .interpolate(&CratePath::from_inner(other.path()), weight)
    {
        Some(path) => Ok(cx.boxed(RefCell::new(from_crate(&path)))),
        None => cx.throw_error(
            "the two paths have different verbs, so they cannot be \
             interpolated between",
        ),
    }
}

// Returns a path with only non-overlapping contours that describe the same area
// as the original path
pub fn simplify(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let rule = fill_rule_arg_or(&mut cx, 1, "nonzero")?;
    let this = this.borrow();
    let simpler =
        CratePath::from_inner(this.path()).simplify(from_skia_rule(rule));
    Ok(cx.boxed(RefCell::new(from_crate(&simpler))))
}

// Returns a path that can be drawn with a nonzero fill but looks like the
// original drawn with evenodd
pub fn unwind(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let this = this.borrow();
    let rewound = CratePath::from_inner(this.path()).unwind();
    Ok(cx.boxed(RefCell::new(from_crate(&rewound))))
}

// Returns a copy whose points have been shifted by (dx, dy)
pub fn offset(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let dx = float_arg(&mut cx, 1, "dx")?;
    let dy = float_arg(&mut cx, 2, "dy")?;

    let this = this.borrow();
    let moved = CratePath::from_inner(this.path()).offset(dx, dy);
    Ok(cx.boxed(RefCell::new(from_crate(&moved))))
}

// Returns a copy whose points have been transformed by a given matrix
pub fn transform(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let matrix = matrix_arg(&mut cx, 1)?;

    let this = this.borrow();
    let path = this.path().make_transform(&matrix);
    Ok(cx.boxed(RefCell::new(Path2D::from(path))))
}

// Returns a copy where every sharp junction to an arcTo-style rounded corner
pub fn round(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let radius = float_arg(&mut cx, 1, "radius")?;

    let this = this.borrow();
    let rounded = CratePath::from_inner(this.path()).round(radius);
    Ok(cx.boxed(RefCell::new(from_crate(&rounded))))
}

// Clips a proportional segment out of the middle of the path (or the edges if
// invert=true)
pub fn trim(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let begin = float_arg_or_bail(&mut cx, 1, "begin")?;
    let end = float_arg_or_bail(&mut cx, 2, "end")?;
    let invert = bool_arg_or(&mut cx, 3, false);

    let this = this.borrow();
    let trimmed = CratePath::from_inner(this.path()).trim(begin, end, invert);
    Ok(cx.boxed(RefCell::new(from_crate(&trimmed))))
}

// Discretizes the path at a fixed segment length then randomly offsets the
// points
pub fn jitter(mut cx: FunctionContext) -> JsResult<BoxedPath2D> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let seg_len = float_arg_or_bail(&mut cx, 1, "segmentLength")?;
    let std_dev = float_arg_or_bail(&mut cx, 2, "variance")?;
    let seed = float_arg_or(&mut cx, 3, 0.0) as u32;

    let this = this.borrow();
    let jittered =
        CratePath::from_inner(this.path()).jitter(seg_len, std_dev, seed);
    Ok(cx.boxed(RefCell::new(from_crate(&jittered))))
}

// Returns the computed `tight` bounds that contain all the points, control
// points, and connecting contours
pub fn bounds(mut cx: FunctionContext) -> JsResult<JsObject> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let this = this.borrow();

    let b = this.path().compute_tight_bounds();

    let js_object: Handle<JsObject> = cx.empty_object();
    let left = cx.number(b.left);
    let top = cx.number(b.top);
    let right = cx.number(b.right);
    let bottom = cx.number(b.bottom);
    let width = cx.number(b.width());
    let height = cx.number(b.height());

    js_object.set(&mut cx, "left", left)?;
    js_object.set(&mut cx, "top", top)?;
    js_object.set(&mut cx, "right", right)?;
    js_object.set(&mut cx, "bottom", bottom)?;
    js_object.set(&mut cx, "width", width)?;
    js_object.set(&mut cx, "height", height)?;
    Ok(js_object)
}

pub fn contains(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let x = float_arg(&mut cx, 1, "x")?;
    let y = float_arg(&mut cx, 2, "y")?;
    let this = this.borrow();
    Ok(cx.boolean(CratePath::from_inner(this.path()).contains(x, y)))
}

/// One `PathSegment` as the `[verb, ...numbers]` array JavaScript expects.
fn to_js_edge<'a>(
    cx: &mut FunctionContext<'a>,
    segment: PathSegment,
) -> JsResult<'a, JsArray> {
    // The verb name, then its points in order, then a conic's weight.
    let (name, numbers): (_, Vec<f32>) = match segment {
        PathSegment::MoveTo { x, y } => ("moveTo", vec![x, y]),
        PathSegment::LineTo { x, y } => ("lineTo", vec![x, y]),
        PathSegment::QuadraticCurveTo { cx: qx, cy, x, y } => {
            ("quadraticCurveTo", vec![qx, cy, x, y])
        }
        PathSegment::BezierCurveTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => ("bezierCurveTo", vec![c1x, c1y, c2x, c2y, x, y]),
        PathSegment::ConicCurveTo {
            cx: qx,
            cy,
            x,
            y,
            weight,
        } => ("conicCurveTo", vec![qx, cy, x, y, weight]),
        PathSegment::ClosePath => ("closePath", vec![]),
    };

    let array = JsArray::new(cx, 1 + numbers.len());
    let verb = cx.string(name);
    array.set(cx, 0, verb)?;
    for (i, value) in numbers.into_iter().enumerate() {
        let number = cx.number(value);
        array.set(cx, 1 + i as u32, number)?;
    }
    Ok(array)
}

pub fn edges(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let segments = {
        let this = this.borrow();
        CratePath::from_inner(this.path()).edges()
    };

    let verbs = JsArray::new(&mut cx, segments.len());
    for (i, segment) in segments.into_iter().enumerate() {
        let edge = to_js_edge(&mut cx, segment)?;
        verbs.set(&mut cx, i as u32, edge)?;
    }
    Ok(verbs)
}

pub fn get_d(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let this = this.borrow();
    Ok(cx.string(CratePath::from_inner(this.path()).to_svg()))
}

pub fn set_d(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedPath2D>(0)?;
    let svg_string = string_arg(&mut cx, 1, "svgPath")?;
    let mut this = this.borrow_mut();

    if let Some(path) = Path::from_svg(svg_string) {
        let builder = this.builder_mut();
        builder.reset();
        builder.add_path(&path, None);
        Ok(cx.undefined())
    } else {
        cx.throw_type_error("Expected a valid SVG path string")
    }
}
