use skia_safe::{
    Path as SkPath, PathBuilder as SkPathBuilder, PathDirection, PathEffect,
    PathFillType, PathOp as SkPathOp, Point as SkPoint, RRect, Rect as SkRect,
    StrokeRec,
    path::{self, AddPathMode, Verb},
    trim_path_effect,
    utils::parse_path,
};

use crate::{
    context2d::{affine_to_matrix, check_radii},
    error::Error,
    geometry::{Affine, Rect},
    node::path::{Path2D as NodePath2D, conic_or_line},
};

/// Path2D winding rule.
///
/// Matches SVG / Canvas semantics: - `NonZero` (Skia's `Winding`) fills any
/// region whose net winding is non-zero. - `EvenOdd` fills any region with an
/// odd winding count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FillRule {
    /// Fills a region when its net winding count is non-zero.
    #[default]
    NonZero,
    /// Fills a region when its winding count is odd.
    EvenOdd,
}

impl FillRule {
    pub(crate) fn to_skia(self) -> PathFillType {
        match self {
            Self::NonZero => PathFillType::Winding,
            Self::EvenOdd => PathFillType::EvenOdd,
        }
    }
}

/// Vector path.
///
/// Immutable once built, and cheap to clone, so one path can be filled,
/// stroked and clipped with in turn without being rebuilt. Build one from SVG
/// path data with [`Path2D::from_svg`], or segment by segment with
/// [`PathBuilder`].
pub struct Path2D {
    pub(crate) inner: SkPath,
}

impl Path2D {
    /// Wraps a Skia path built elsewhere in the crate.
    pub(crate) fn from_inner(inner: SkPath) -> Self {
        Self { inner }
    }

    /// The Skia path this wraps, for the binding to rebuild its own type
    /// from.
    pub(crate) fn to_skia(&self) -> SkPath {
        self.inner.clone()
    }

    /// The tight bounding box of the path's geometry.
    ///
    /// Tight means the curve's true extent, so a cubic's control points can
    /// sit well outside the box: what is measured is where the curve goes,
    /// not where its handles are. Stroke width is excluded too -- this
    /// measures the geometry, not what painting it would cover.
    pub fn bounds(&self) -> Rect {
        let bounds = self.inner.compute_tight_bounds();
        Rect {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        }
    }

    /// Parses SVG path data into a [`Path2D`].
    ///
    /// `data` is the same syntax an SVG `d=""` attribute takes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSvgPath`] when the data cannot be parsed.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let triangle = Path2D::from_svg("M0 0 L10 0 L5 8 Z", FillRule::NonZero)?;
    /// assert_eq!(triangle.fill_rule(), FillRule::NonZero);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn from_svg(data: &str, fill_rule: FillRule) -> Result<Self, Error> {
        let mut path = parse_path::from_svg(data).ok_or_else(|| {
            Error::InvalidSvgPath {
                reason: format!("could not parse SVG path data: {data:?}"),
            }
        })?;
        path.set_fill_type(fill_rule.to_skia());
        Ok(Self { inner: path })
    }

    /// Returns the winding rule this path fills with.
    pub fn fill_rule(&self) -> FillRule {
        match self.inner.fill_type() {
            PathFillType::EvenOdd | PathFillType::InverseEvenOdd => {
                FillRule::EvenOdd
            }
            _ => FillRule::NonZero,
        }
    }

    /// Sets the winding rule this path fills with.
    pub fn set_fill_rule(&mut self, fill_rule: FillRule) {
        self.inner.set_fill_type(fill_rule.to_skia());
    }
}

/// A boolean operation between two paths.
///
/// The set operations a vector editor calls pathfinder or booleans. Each
/// treats a path as the region it fills, which is why the winding rule
/// matters: `Difference` on two overlapping circles depends on whether the
/// overlap counts as inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathOp {
    /// What the first path covers and the second does not.
    #[default]
    Difference,
    /// What both cover.
    Intersect,
    /// What either covers.
    Union,
    /// What exactly one covers.
    Xor,
    /// What the second covers and the first does not -- `Difference` with
    /// the operands swapped.
    Complement,
}

impl PathOp {
    fn to_skia(self) -> SkPathOp {
        match self {
            Self::Difference => SkPathOp::Difference,
            Self::Intersect => SkPathOp::Intersect,
            Self::Union => SkPathOp::Union,
            Self::Xor => SkPathOp::XOR,
            Self::Complement => SkPathOp::ReverseDifference,
        }
    }
}

/// One drawing command from a path, as [`Path2D::edges`] reports it.
///
/// The verbs a path is made of, with the points each carries. A path is a
/// sequence of these, and feeding them back to a [`PathBuilder`] in order
/// rebuilds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathSegment {
    /// Starts a new contour at a point.
    MoveTo {
        /// Where the contour starts.
        x: f32,
        /// Where the contour starts.
        y: f32,
    },
    /// A straight line from the current point.
    LineTo {
        /// End point.
        x: f32,
        /// End point.
        y: f32,
    },
    /// A quadratic Bezier through one control point.
    QuadraticCurveTo {
        /// Control point.
        cx: f32,
        /// Control point.
        cy: f32,
        /// End point.
        x: f32,
        /// End point.
        y: f32,
    },
    /// A cubic Bezier through two control points.
    BezierCurveTo {
        /// First control point.
        c1x: f32,
        /// First control point.
        c1y: f32,
        /// Second control point.
        c2x: f32,
        /// Second control point.
        c2y: f32,
        /// End point.
        x: f32,
        /// End point.
        y: f32,
    },
    /// A rational quadratic, which is what draws a true elliptical arc.
    ///
    /// `weight` is the conic's, and is what a quadratic cannot express: 1
    /// makes it a plain quadratic, and higher values pull it toward the
    /// control point.
    ConicCurveTo {
        /// Control point.
        cx: f32,
        /// Control point.
        cy: f32,
        /// End point.
        x: f32,
        /// End point.
        y: f32,
        /// The conic weight.
        weight: f32,
    },
    /// Closes the current contour back to where it started.
    ClosePath,
}

impl Path2D {
    /// This path's geometry as SVG path data.
    ///
    /// The inverse of [`from_svg`](Self::from_svg), and the same syntax an
    /// SVG `d=""` attribute takes.
    pub fn to_svg(&self) -> String {
        self.inner.to_svg()
    }

    /// Whether `(x, y)` falls inside the path, under its fill rule.
    ///
    /// The same question `Context2D::is_point_in_path` asks, without needing
    /// a context to ask it through.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.inner.contains((x, y))
    }

    /// The result of combining this path with `other`.
    ///
    /// Returns `None` when Skia's path solver cannot produce a result, which
    /// it reports for some self-intersecting inputs rather than guessing.
    #[must_use]
    pub fn combine(&self, other: &Path2D, op: PathOp) -> Option<Self> {
        self.inner
            .op(&other.inner, op.to_skia())
            .map(Self::from_inner)
    }

    /// A path between this one and `other`, `weight` of the way across.
    ///
    /// 0 is this path and 1 is `other`. Returns `None` unless the two are
    /// *interpolatable*: same verbs in the same order, differing only in
    /// where their points sit. Two paths built by different code almost
    /// never are, which is what makes this a tool for animating one shape
    /// rather than for morphing between arbitrary ones.
    #[must_use]
    pub fn interpolate(&self, other: &Path2D, weight: f32) -> Option<Self> {
        // Reversed, so 0 is `self`: Skia's argument order makes the receiver
        // the destination, which reads backwards from `self.interpolate(to)`.
        other
            .inner
            .interpolate(&self.inner, weight)
            .map(Self::from_inner)
    }

    /// A path with its self-intersections resolved, filling the same region
    /// under `fill_rule`.
    ///
    /// Returns the path unchanged when Skia declines to simplify it.
    #[must_use]
    pub fn simplify(&self, fill_rule: FillRule) -> Self {
        let mut path = self.inner.clone();
        path.set_fill_type(fill_rule.to_skia());
        Self::from_inner(path.simplify().unwrap_or(path))
    }

    /// A path that fills the same region under [`FillRule::NonZero`] that
    /// this one fills under [`FillRule::EvenOdd`].
    ///
    /// For handing a shape to something that only fills non-zero -- the
    /// contours are rewound rather than the rule changed.
    #[must_use]
    pub fn unwind(&self) -> Self {
        let mut path = self.inner.clone();
        path.set_fill_type(PathFillType::EvenOdd);
        Self::from_inner(path.as_winding().unwrap_or(path))
    }

    /// A copy shifted by `(dx, dy)`.
    #[must_use]
    pub fn offset(&self, dx: f32, dy: f32) -> Self {
        Self::from_inner(self.inner.with_offset((dx, dy)))
    }

    /// A copy with `transform` applied to every point.
    #[must_use]
    pub fn transform(&self, transform: Affine) -> Self {
        Self::from_inner(
            self.inner.make_transform(&affine_to_matrix(transform)),
        )
    }

    /// A copy with every sharp corner replaced by an arc of `radius`.
    ///
    /// Corners already smoother than the radius are left alone, and the
    /// radius is reduced where a segment is too short to give it room.
    #[must_use]
    pub fn round(&self, radius: f32) -> Self {
        self.filtered(PathEffect::corner_path(radius))
    }

    /// The portion of the path between `start` and `end`, as fractions of
    /// its total length.
    ///
    /// `invert` keeps the two ends and drops the middle instead. Both
    /// fractions run 0 to 1 along the whole path, contours included, which
    /// is what makes this draw a line on progressively.
    #[must_use]
    pub fn trim(&self, start: f32, end: f32, invert: bool) -> Self {
        let mode = match invert {
            true => trim_path_effect::Mode::Inverted,
            false => trim_path_effect::Mode::Normal,
        };
        self.filtered(PathEffect::trim(start, end, mode))
    }

    /// A copy chopped into segments of `segment_length` whose points are
    /// then displaced randomly by up to `variance`.
    ///
    /// What gives a drawn line a hand-made edge. `seed` makes the
    /// displacement repeatable: the same seed and the same path give the
    /// same result, so a redraw does not shimmer.
    #[must_use]
    pub fn jitter(
        &self,
        segment_length: f32,
        variance: f32,
        seed: u32,
    ) -> Self {
        self.filtered(PathEffect::discrete(
            segment_length,
            variance,
            Some(seed),
        ))
    }

    /// The drawing commands this path is made of.
    ///
    /// In order, so feeding them to a [`PathBuilder`] rebuilds the path.
    pub fn edges(&self) -> Vec<PathSegment> {
        // Two iterators over the same path: the verbs and points come from
        // one, and a conic's weight is only reachable from an iterator that
        // has been stepped to the same place, which is what the second is
        // for.
        let mut weights = path::Iter::new(&self.inner, false);
        path::Iter::new(&self.inner, false)
            .filter_map(|(verb, points)| {
                weights.next();
                segment(verb, &points, || weights.conic_weight())
            })
            .collect()
    }

    /// The points along the path, every `step` units.
    ///
    /// Curves are flattened to straight segments of `step` before the points
    /// are taken, so this walks a curve at an even spacing rather than
    /// returning only its control points.
    pub fn points(&self, step: f32) -> Vec<(f32, f32)> {
        self.jitter(step, 0.0, 0)
            .edges()
            .into_iter()
            .filter_map(|segment| match segment {
                PathSegment::MoveTo { x, y }
                | PathSegment::LineTo { x, y }
                | PathSegment::QuadraticCurveTo { x, y, .. }
                | PathSegment::BezierCurveTo { x, y, .. }
                | PathSegment::ConicCurveTo { x, y, .. } => Some((x, y)),
                PathSegment::ClosePath => None,
            })
            .collect()
    }

    /// Runs `effect` over this path, returning it unchanged if the effect
    /// declines.
    ///
    /// The three effects below share this: each can return `None`, and a
    /// path that could not be rounded or trimmed is more useful than an
    /// error nobody can act on.
    fn filtered(&self, effect: Option<PathEffect>) -> Self {
        let bounds = self.inner.bounds();
        let stroke = StrokeRec::new_hairline();
        effect
            .and_then(|effect| effect.filter_path(&self.inner, &stroke, bounds))
            .map(|(mut builder, _)| Self::from_inner(builder.detach()))
            .unwrap_or_else(|| self.clone())
    }
}

/// One iterator step as a [`PathSegment`], or `None` for a verb that carries
/// no drawing -- `Done`, and the `Move` Skia synthesizes when closing.
fn segment(
    verb: Verb,
    points: &[SkPoint],
    weight: impl FnOnce() -> Option<f32>,
) -> Option<PathSegment> {
    // Skia hands back the contour's current point first for every verb but
    // `Move`, so the points this segment introduces start at index 1.
    let at = |i: usize| points.get(i).map(|p| (p.x, p.y));
    Some(match verb {
        Verb::Move => {
            let (x, y) = at(0)?;
            PathSegment::MoveTo { x, y }
        }
        Verb::Line => {
            let (x, y) = at(1)?;
            PathSegment::LineTo { x, y }
        }
        Verb::Quad => {
            let ((cx, cy), (x, y)) = (at(1)?, at(2)?);
            PathSegment::QuadraticCurveTo { cx, cy, x, y }
        }
        Verb::Cubic => {
            let ((c1x, c1y), (c2x, c2y), (x, y)) = (at(1)?, at(2)?, at(3)?);
            PathSegment::BezierCurveTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            }
        }
        Verb::Conic => {
            let ((cx, cy), (x, y)) = (at(1)?, at(2)?);
            PathSegment::ConicCurveTo {
                cx,
                cy,
                x,
                y,
                weight: weight()?,
            }
        }
        Verb::Close => PathSegment::ClosePath,
        Verb::Done => return None,
    })
}

impl Clone for Path2D {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Builds a [`Path2D`] segment by segment.
///
/// The segment methods are the ones [`Context2D`] draws with -- same names,
/// same argument order, same semantics -- minus the current transform, which
/// belongs to a context and not to a path. A path built here and a path traced
/// onto an untransformed context are the same geometry.
///
/// Building is separate from the path itself because a [`Path2D`] is immutable:
/// that is what makes it cheap to clone and cheap to hand to a draw call.
/// [`build`](PathBuilder::build) takes `&self`, so a builder can emit a path
/// and carry on extending.
///
/// [`Context2D`]: crate::context2d::Context2D
///
/// # Examples
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// let mut builder = PathBuilder::new();
/// builder
///     .move_to(0.0, 0.0)
///     .line_to(10.0, 0.0)
///     .line_to(5.0, 8.0);
/// builder.close_path();
///
/// let triangle = builder.build(FillRule::NonZero);
/// assert_eq!(triangle.bounds().right, 10.0);
/// ```
#[derive(Clone, Default)]
pub struct PathBuilder {
    inner: SkPathBuilder,
}

impl PathBuilder {
    /// Creates an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a builder that continues an existing path.
    ///
    /// The path is copied, so the original is untouched by what follows.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let base = Path2D::from_svg("M0 0 L10 0", FillRule::NonZero)?;
    /// let mut builder = PathBuilder::from_path(&base);
    /// builder.line_to(10.0, 10.0);
    ///
    /// assert_eq!(base.bounds().bottom, 0.0, "the original is unchanged");
    /// assert_eq!(builder.build(FillRule::NonZero).bounds().bottom, 10.0);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn from_path(path: &Path2D) -> Self {
        Self {
            inner: SkPathBuilder::new_path(&path.inner),
        }
    }

    /// Snapshots what has been built so far into a [`Path2D`].
    ///
    /// Takes `&self`, so building can continue afterwards; each call returns
    /// an independent path.
    pub fn build(&self, fill_rule: FillRule) -> Path2D {
        let mut path = self.inner.snapshot();
        path.set_fill_type(fill_rule.to_skia());
        Path2D { inner: path }
    }

    /// Whether nothing has been added yet.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Starts a new subpath at (`x`, `y`).
    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.inner.move_to((x, y));
        self
    }

    /// Adds a straight segment to (`x`, `y`).
    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.scoot(x, y);
        self.inner.line_to((x, y));
        self
    }

    /// Adds a cubic Bézier curve through two control points.
    pub fn bezier_curve_to(
        &mut self,
        cp1x: f32,
        cp1y: f32,
        cp2x: f32,
        cp2y: f32,
        x: f32,
        y: f32,
    ) -> &mut Self {
        self.scoot(cp1x, cp1y);
        self.inner.cubic_to((cp1x, cp1y), (cp2x, cp2y), (x, y));
        self
    }

    /// Adds a quadratic Bézier curve through one control point.
    pub fn quadratic_curve_to(
        &mut self,
        cpx: f32,
        cpy: f32,
        x: f32,
        y: f32,
    ) -> &mut Self {
        self.scoot(cpx, cpy);
        self.inner.quad_to((cpx, cpy), (x, y));
        self
    }

    /// Adds a conic curve through one control point, weighted.
    ///
    /// As [`Context2D::conic_curve_to`](crate::context2d::Context2D::conic_curve_to):
    /// a weight of zero or less degenerates to a straight line.
    pub fn conic_curve_to(
        &mut self,
        cpx: f32,
        cpy: f32,
        x: f32,
        y: f32,
        weight: f32,
    ) -> &mut Self {
        self.scoot(cpx, cpy);
        conic_or_line(&mut self.inner, (cpx, cpy), (x, y), weight);
        self
    }

    /// Adds a closed rectangular subpath.
    ///
    /// Exactly one negative dimension reverses the winding, so a rectangle
    /// drawn that way inside another punches a hole rather than filling it.
    /// Two negatives cancel and wind like a positive rectangle.
    pub fn rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> &mut Self {
        // Always clockwise, and never a normalised rectangle: a negative
        // dimension leaves the rect inverted, and traversing an inverted rect
        // is what reverses the winding. Choosing the direction from the signs
        // instead would reverse it a second time and cancel the effect.
        self.inner.add_rect(
            SkRect::from_xywh(x, y, width, height),
            PathDirection::CW,
            0,
        );
        self
    }

    /// Adds a circular arc centred on (`x`, `y`).
    ///
    /// Angles are in radians. `counterclockwise` reverses the sweep.
    ///
    /// # Errors
    ///
    /// As [`Context2D::arc`](crate::context2d::Context2D::arc): a negative
    /// or non-finite radius returns [`Error::InvalidRadius`].
    pub fn arc(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        counterclockwise: bool,
    ) -> Result<&mut Self, Error> {
        self.add_ellipse(
            x,
            y,
            radius,
            radius,
            0.0,
            start_angle,
            end_angle,
            counterclockwise,
        )
    }

    /// Adds an elliptical arc, with independent radii and a rotation.
    ///
    /// # Errors
    ///
    /// As [`PathBuilder::arc`], for either radius.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        x_radius: f32,
        y_radius: f32,
        rotation: f32,
        start_angle: f32,
        end_angle: f32,
        counterclockwise: bool,
    ) -> Result<&mut Self, Error> {
        self.add_ellipse(
            x,
            y,
            x_radius,
            y_radius,
            rotation,
            start_angle,
            end_angle,
            counterclockwise,
        )
    }

    /// Adds an arc tangent to the lines from the current point to
    /// (`x1`, `y1`) and from there to (`x2`, `y2`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRadius`] for a negative or non-finite `radius`,
    /// as
    /// [`Context2D::arc_to`](crate::context2d::Context2D::arc_to) does.
    pub fn arc_to(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        radius: f32,
    ) -> Result<&mut Self, Error> {
        if radius < 0.0 || !radius.is_finite() {
            return Err(Error::InvalidRadius { radius });
        }
        self.scoot(x1, y1);
        self.inner.arc_to_tangent((x1, y1), (x2, y2), radius);
        Ok(self)
    }

    /// Adds a rounded rectangle, with one circular radius per corner starting
    /// at the top left and running clockwise.
    ///
    /// # Errors
    ///
    /// As [`PathBuilder::round_rect_elliptical`].
    pub fn round_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: [f32; 4],
    ) -> Result<&mut Self, Error> {
        self.round_rect_elliptical(x, y, width, height, radii.map(|r| (r, r)))
    }

    /// Adds a rounded rectangle whose corners may be elliptical, each given as
    /// a horizontal and vertical radius.
    ///
    /// A negative dimension reverses the winding, as it does for
    /// [`rect`](PathBuilder::rect).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRadius`] when a radius is negative or
    /// non-finite, which Skia would otherwise clamp to a square corner.
    pub fn round_rect_elliptical(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: [(f32, f32); 4],
    ) -> Result<&mut Self, Error> {
        if let Some(radius) = radii
            .iter()
            .flat_map(|(rx, ry)| [*rx, *ry])
            .find(|r| *r < 0.0 || !r.is_finite())
        {
            return Err(Error::InvalidRadius { radius });
        }

        let corners = radii.map(|(rx, ry)| SkPoint::new(rx, ry));
        let rrect = RRect::new_rect_radii(
            SkRect::from_xywh(x, y, width, height),
            &corners,
        );
        // Start index pinned to 0, as the JavaScript `Path2D.roundRect` pins
        // it. Skia's own default is 6 or 7 depending on direction, which
        // reorders the contour's points and so moves where a dash phase
        // begins and where a later segment attaches.
        self.inner.add_rrect(rrect, direction(width, height), 0);
        Ok(self)
    }

    /// Appends another path as a new subpath.
    ///
    /// A new one, not a continuation: the added contour keeps its own start
    /// point, so it strokes and fills as a separate region.
    pub fn add_path(&mut self, path: &Path2D) -> &mut Self {
        self.inner.add_path(&path.inner, AddPathMode::Append);
        self
    }

    /// Closes the current subpath back to its start.
    pub fn close_path(&mut self) -> &mut Self {
        self.inner.close();
        self
    }

    /// Opens a subpath at the first point when a segment is added to an empty
    /// builder, as the Canvas API does.
    fn scoot(&mut self, x: f32, y: f32) {
        // `verbs()`, not `snapshot()`: this runs before every segment, and
        // snapshotting copies the whole path, which would make building
        // quadratic.
        if self.inner.verbs().is_empty() {
            self.inner.move_to((x, y));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_ellipse(
        &mut self,
        x: f32,
        y: f32,
        x_radius: f32,
        y_radius: f32,
        rotation: f32,
        start_angle: f32,
        end_angle: f32,
        ccw: bool,
    ) -> Result<&mut Self, Error> {
        check_radii(x_radius, y_radius)?;
        let mut arc = NodePath2D::default();
        arc.add_ellipse(
            (x, y),
            (x_radius, y_radius),
            rotation,
            start_angle,
            end_angle,
            ccw,
        );
        // Extend, not Append: an arc continues the current contour, which is
        // what makes `move_to` then `arc` draw a leading line.
        self.inner.add_path(&arc.path(), AddPathMode::Extend);
        Ok(self)
    }
}

impl std::fmt::Debug for PathBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathBuilder")
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

/// The winding direction a rounded rectangle of this size traces.
///
/// Unlike a plain rect, an `RRect` normalises the rectangle it is built from,
/// so an inverted one cannot carry the reversal that exactly one negative
/// dimension calls for. The direction argument has to carry it instead.
fn direction(width: f32, height: f32) -> PathDirection {
    if width.signum() == height.signum() {
        PathDirection::CW
    } else {
        PathDirection::CCW
    }
}

impl std::fmt::Debug for Path2D {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Path2D")
            .field("fill_rule", &self.fill_rule())
            .finish()
    }
}

#[cfg(test)]
mod path_operation_tests {
    use super::*;

    /// A square from (0,0) to (10,10).
    fn square() -> Path2D {
        Path2D::from_svg("M0 0 H10 V10 H0 Z", FillRule::NonZero)
            .expect("valid svg")
    }

    /// A square overlapping the first from (5,5) to (15,15).
    fn offset_square() -> Path2D {
        Path2D::from_svg("M5 5 H15 V15 H5 Z", FillRule::NonZero)
            .expect("valid svg")
    }

    #[test]
    fn a_boolean_operation_covers_the_region_it_names() {
        let (a, b) = (square(), offset_square());
        // Sampled rather than compared as geometry: what a set operation
        // means is which points end up inside, and Skia is free to describe
        // the same region with different contours.
        let inside = |path: &Path2D, x: f32, y: f32| path.contains(x, y);

        let union = a.combine(&b, PathOp::Union).expect("union");
        assert!(inside(&union, 2.0, 2.0) && inside(&union, 12.0, 12.0));

        let intersect = a.combine(&b, PathOp::Intersect).expect("intersect");
        assert!(inside(&intersect, 7.0, 7.0), "the overlap is inside");
        assert!(!inside(&intersect, 2.0, 2.0), "and only the overlap");

        let difference = a.combine(&b, PathOp::Difference).expect("difference");
        assert!(inside(&difference, 2.0, 2.0));
        assert!(!inside(&difference, 7.0, 7.0), "the overlap is removed");

        // Complement is difference with the operands swapped, which is the
        // one of the five whose name does not say which way round it goes.
        let complement = a.combine(&b, PathOp::Complement).expect("complement");
        assert!(inside(&complement, 12.0, 12.0));
        assert!(!inside(&complement, 2.0, 2.0));

        let xor = a.combine(&b, PathOp::Xor).expect("xor");
        assert!(inside(&xor, 2.0, 2.0) && inside(&xor, 12.0, 12.0));
        assert!(!inside(&xor, 7.0, 7.0), "what both cover is excluded");
    }

    #[test]
    fn interpolating_needs_two_paths_of_the_same_shape() {
        let (a, b) = (square(), offset_square());
        let half = a.interpolate(&b, 0.5).expect("same verbs");
        // Halfway between (0,0) and (5,5).
        let bounds = half.bounds();
        assert!((bounds.left - 2.5).abs() < 0.01, "{bounds:?}");

        // 0 is this path and 1 is the other, which is the reverse of the
        // argument order Skia takes.
        assert!(
            (a.interpolate(&b, 0.0).expect("at zero").bounds().left
                - a.bounds().left)
                .abs()
                < 0.01
        );
        assert!(
            (a.interpolate(&b, 1.0).expect("at one").bounds().left
                - b.bounds().left)
                .abs()
                < 0.01
        );

        // Different verbs cannot be interpolated between at all, which is
        // what makes this a tool for animating one shape rather than for
        // morphing between arbitrary ones.
        let triangle =
            Path2D::from_svg("M0 0 L10 0 L5 8 Z", FillRule::NonZero).unwrap();
        assert!(square().interpolate(&triangle, 0.5).is_none());
    }

    #[test]
    fn a_path_round_trips_through_its_svg_and_its_edges() {
        let original = square();
        let svg = original.to_svg();
        let reparsed =
            Path2D::from_svg(&svg, FillRule::NonZero).expect("its own output");
        assert_eq!(reparsed.to_svg(), svg);

        // And the verbs it reports rebuild it.
        let edges = original.edges();
        assert!(matches!(edges.first(), Some(PathSegment::MoveTo { .. })));
        assert!(matches!(edges.last(), Some(PathSegment::ClosePath)));

        let mut builder = PathBuilder::new();
        for segment in &edges {
            match *segment {
                PathSegment::MoveTo { x, y } => builder.move_to(x, y),
                PathSegment::LineTo { x, y } => builder.line_to(x, y),
                PathSegment::QuadraticCurveTo { cx, cy, x, y } => {
                    builder.quadratic_curve_to(cx, cy, x, y)
                }
                PathSegment::BezierCurveTo {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => builder.bezier_curve_to(c1x, c1y, c2x, c2y, x, y),
                PathSegment::ConicCurveTo {
                    cx,
                    cy,
                    x,
                    y,
                    weight,
                } => builder.conic_curve_to(cx, cy, x, y, weight),
                PathSegment::ClosePath => builder.close_path(),
            };
        }
        assert_eq!(
            builder.build(FillRule::NonZero).bounds(),
            original.bounds()
        );
    }

    #[test]
    fn a_conic_reports_the_weight_a_quadratic_cannot_carry() {
        // The one segment that carries a number beyond its points, and the
        // one the two-iterator dance in `edges` exists for.
        let mut builder = PathBuilder::new();
        builder
            .move_to(0.0, 0.0)
            .conic_curve_to(10.0, 0.0, 10.0, 10.0, 2.0);
        let edges = builder.build(FillRule::NonZero).edges();
        assert!(
            matches!(
                edges.get(1),
                Some(PathSegment::ConicCurveTo { weight, .. })
                    if (weight - 2.0).abs() < 1e-6
            ),
            "{edges:?}"
        );
    }

    #[test]
    fn offset_and_transform_move_the_geometry() {
        let moved = square().offset(5.0, -3.0);
        let bounds = moved.bounds();
        assert!((bounds.left - 5.0).abs() < 0.01);
        assert!((bounds.top + 3.0).abs() < 0.01);

        let scaled = square().transform(Affine {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            tx: 0.0,
            ty: 0.0,
        });
        assert!((scaled.bounds().right - 20.0).abs() < 0.01);
    }

    #[test]
    fn trimming_keeps_a_fraction_and_inverting_keeps_the_rest() {
        let line =
            Path2D::from_svg("M0 0 L100 0", FillRule::NonZero).expect("a line");
        let middle = line.trim(0.25, 0.75, false);
        let bounds = middle.bounds();
        assert!((bounds.left - 25.0).abs() < 0.5, "{bounds:?}");
        assert!((bounds.right - 75.0).abs() < 0.5, "{bounds:?}");

        // Inverted keeps the two ends, so it still reaches both extremes.
        let ends = line.trim(0.25, 0.75, true).bounds();
        assert!(
            (ends.left - 0.0).abs() < 0.5 && (ends.right - 100.0).abs() < 0.5
        );
    }

    #[test]
    fn jitter_is_repeatable_and_points_walk_the_result() {
        let line =
            Path2D::from_svg("M0 0 L100 0", FillRule::NonZero).expect("a line");
        // The same seed gives the same path, which is what stops a redraw
        // shimmering.
        assert_eq!(
            line.jitter(10.0, 3.0, 7).to_svg(),
            line.jitter(10.0, 3.0, 7).to_svg()
        );
        assert_ne!(
            line.jitter(10.0, 3.0, 7).to_svg(),
            line.jitter(10.0, 3.0, 8).to_svg()
        );

        // Zero variance is a pure flattening, which is what `points` uses:
        // a hundred-unit line at a step of ten is eleven points.
        let points = line.points(10.0);
        assert_eq!(points.len(), 11, "{points:?}");
        assert!((points[0].0 - 0.0).abs() < 0.01);
        assert!((points[10].0 - 100.0).abs() < 0.01);
    }

    #[test]
    fn rounding_pulls_a_corner_in_and_simplify_keeps_the_region() {
        // A rounded square no longer reaches its own corner.
        assert!(!square().round(3.0).contains(0.2, 0.2));
        assert!(square().contains(0.2, 0.2), "the sharp one does");

        // A self-intersecting bowtie filled even-odd has a hole in the
        // middle; simplifying under the same rule keeps that.
        let bowtie =
            Path2D::from_svg("M0 0 L10 10 L10 0 L0 10 Z", FillRule::EvenOdd)
                .expect("a bowtie");
        let simpler = bowtie.simplify(FillRule::EvenOdd);
        assert_eq!(simpler.contains(5.0, 2.0), bowtie.contains(5.0, 2.0));

        // Unwinding rewrites the contours so a non-zero fill draws what
        // even-odd drew.
        let unwound = bowtie.unwind();
        assert_eq!(unwound.fill_rule(), FillRule::NonZero);
    }
}
