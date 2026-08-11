use skia_safe::{
    Path as SkPath, PathBuilder as SkPathBuilder, PathDirection, PathFillType,
    Point as SkPoint, RRect, Rect as SkRect, path::AddPathMode,
    utils::parse_path,
};

use crate::{
    error::Error,
    geometry::Rect,
    node::path::{Path2D as NodePath2D, conic_or_line},
};

/// Path winding rule.
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
/// path data with [`Path::from_svg`], or segment by segment with
/// [`PathBuilder`].
pub struct Path {
    pub(crate) inner: SkPath,
}

impl Path {
    /// Wraps a Skia path built elsewhere in the crate.
    pub(crate) fn from_inner(inner: SkPath) -> Self {
        Self { inner }
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

    /// Parses SVG path data into a [`Path`].
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
    /// let triangle = Path::from_svg("M0 0 L10 0 L5 8 Z", FillRule::NonZero)?;
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

impl Clone for Path {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Builds a [`Path`] segment by segment.
///
/// The segment methods are the ones [`Context2D`] draws with -- same names,
/// same argument order, same semantics -- minus the current transform, which
/// belongs to a context and not to a path. A path built here and a path traced
/// onto an untransformed context are the same geometry.
///
/// Building is separate from the path itself because a [`Path`] is immutable:
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
    /// let base = Path::from_svg("M0 0 L10 0", FillRule::NonZero)?;
    /// let mut builder = PathBuilder::from_path(&base);
    /// builder.line_to(10.0, 10.0);
    ///
    /// assert_eq!(base.bounds().bottom, 0.0, "the original is unchanged");
    /// assert_eq!(builder.build(FillRule::NonZero).bounds().bottom, 10.0);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn from_path(path: &Path) -> Self {
        Self {
            inner: SkPathBuilder::new_path(&path.inner),
        }
    }

    /// Snapshots what has been built so far into a [`Path`].
    ///
    /// Takes `&self`, so building can continue afterwards; each call returns
    /// an independent path.
    pub fn build(&self, fill_rule: FillRule) -> Path {
        let mut path = self.inner.snapshot();
        path.set_fill_type(fill_rule.to_skia());
        Path { inner: path }
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
    pub fn arc(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        counterclockwise: bool,
    ) -> &mut Self {
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
    ) -> &mut Self {
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
    /// Returns [`Error::InvalidRect`] for a negative or non-finite `radius`,
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
            return Err(Error::InvalidRect {
                rect: Rect {
                    left: x1,
                    top: y1,
                    right: x2,
                    bottom: y2,
                },
            });
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
    /// Returns [`Error::InvalidRect`] when a radius is negative or
    /// non-finite, which Skia would otherwise clamp to a square corner.
    pub fn round_rect_elliptical(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: [(f32, f32); 4],
    ) -> Result<&mut Self, Error> {
        if radii.iter().any(|(rx, ry)| {
            *rx < 0.0 || *ry < 0.0 || !rx.is_finite() || !ry.is_finite()
        }) {
            return Err(Error::InvalidRect {
                rect: Rect {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                },
            });
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
    pub fn add_path(&mut self, path: &Path) -> &mut Self {
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
    ) -> &mut Self {
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
        self
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

impl std::fmt::Debug for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Path")
            .field("fill_rule", &self.fill_rule())
            .finish()
    }
}
