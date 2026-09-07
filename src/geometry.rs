//! Plain geometric value types shared across the public API.
//!
//! The origin is at the top left and `y` increases downwards, matching the
//! Canvas 2D convention rather than a mathematical one.
//!
//! Units are pixels in the canvas's own space -- logical units, which the
//! export density then scales.

use crate::css::parse_transform;
use serde::Serialize;

/// A point in surface space.
///
/// The `Serialize` derive is a wire format, not a convenience: mouse
/// [`UiEvent`](crate::gui::event::UiEvent)s carry these to the JS side, which
/// destructures `x` and `y` by name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[doc(alias = "DOMPoint")]
pub struct Point {
    /// Horizontal offset from the left edge, in pixels.
    pub x: f32,
    /// Vertical offset from the top edge, in pixels.
    pub y: f32,
}

impl Point {
    /// Creates a point at `(x, y)`.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A width/height pair, with no position.
///
/// Serializable for the same reason as [`Point`]: a resize
/// [`UiEvent`](crate::gui::event::UiEvent) carries one to the JS side, which
/// reads `width` and `height` by name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Size {
    /// Extent along the x axis, in pixels.
    pub width: f32,
    /// Extent along the y axis, in pixels.
    pub height: f32,
}

impl Size {
    /// Creates a size of `width` by `height`.
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle, stored as its four edges.
///
/// A rectangle is well-formed when `left <= right` and `top <= bottom`.
/// Nothing enforces that, and no method reports it: [`Rect::is_empty`] is
/// `true` for a well-formed zero-area rectangle and `false` for one with
/// `NaN` edges. [`Rect::from_xywh`] produces a well-formed rectangle for
/// non-negative extents.
#[derive(Debug, Clone, Copy, PartialEq)]
#[doc(alias = "DOMRect")]
pub struct Rect {
    /// The x edge intended to be the smaller of the two.
    pub left: f32,
    /// The y edge intended to be the smaller of the two.
    pub top: f32,
    /// The x edge intended to be the larger of the two.
    pub right: f32,
    /// The y edge intended to be the larger of the two.
    pub bottom: f32,
}

/// A 3x3 transform, carrying the projective row an [`Affine`] cannot.
///
/// Built by [`Context2D::create_projection`], which solves for the transform
/// mapping one quadrilateral onto another. That is how perspective is
/// expressed on a 2D canvas: a rectangle mapped onto a trapezoid reads as a
/// plane receding from the viewer.
///
/// Kept separate from [`Affine`] rather than widening it, so the ordinary 2D
/// case stays six components with no projective row to reason about.
///
/// [`Context2D::create_projection`]: crate::context2d::Context2D::create_projection
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// Row-major 3x3, ordered as Skia stores it:
    /// `[sx, kx, tx, ky, sy, ty, p0, p1, p2]`.
    pub values: [f32; 9],
}

/// 2D affine transform in `[a, b, c, d, tx, ty]` form, matching the CSS
/// `DOMMatrix2DInit` and `CanvasRenderingContext2D.setTransform` convention.
///
/// Acts on a column vector `[x, y, 1]^T`:
///
/// ```text
/// | a  c  tx |   | x |
/// | b  d  ty | * | y |
/// | 0  0  1  |   | 1 |
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[doc(alias = "DOMMatrix")]
pub struct Affine {
    /// Row 0, column 0. Horizontal scale.
    pub a: f32,
    /// Row 1, column 0. Vertical shear.
    pub b: f32,
    /// Row 0, column 1. Horizontal shear.
    pub c: f32,
    /// Row 1, column 1. Vertical scale.
    pub d: f32,
    /// Horizontal translation, in pixels.
    pub tx: f32,
    /// Vertical translation, in pixels.
    pub ty: f32,
}

impl Affine {
    /// The transform that leaves every point where it is.
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// Creates a transform that shifts by `(tx, ty)` pixels.
    pub fn translation(tx: f32, ty: f32) -> Self {
        Self {
            tx,
            ty,
            ..Self::IDENTITY
        }
    }

    /// Creates a transform that scales about the origin.
    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    /// Creates a rotation about the origin, `angle` in radians.
    ///
    /// Positive angles turn clockwise on screen, because `y` grows
    /// downwards.
    pub fn rotation_radians(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Creates a rotation about the origin, `angle` in degrees.
    pub fn rotation_degrees(angle: f32) -> Self {
        Self::rotation_radians(angle.to_radians())
    }

    /// Creates a horizontal skew about the origin, `angle` in radians.
    ///
    /// `x` gains `tan(angle) * y` and `y` is untouched, so a vertical line
    /// tilts by `angle` while a horizontal one does not move.
    ///
    /// A quarter turn has no meaningful skew, and does not report one: `tan`
    /// diverges there, and the nearest `f32` to a right angle lies just past
    /// it, so `skew_x_degrees(90.0)` gives `c = -22877332` -- large, and
    /// negative rather than positive.
    ///
    /// This is the matrix `DOMMatrix.skewX` composes, so
    /// `m.multiply(&Affine::skew_x_radians(t))` is what that method returns.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// // A quarter of a quarter turn shears one unit of `y` into one of `x`.
    /// let skew = Affine::skew_x_radians(std::f32::consts::FRAC_PI_4);
    /// assert!((skew.c - 1.0).abs() < 1e-6);
    /// assert_eq!(skew.b, 0.0);
    ///
    /// // Composed onto a horizontal scale, the shear is scaled with it.
    /// let combined = Affine::scale(2.0, 1.0).multiply(&skew);
    /// assert!((combined.c - 2.0).abs() < 1e-6);
    /// ```
    pub fn skew_x_radians(angle: f32) -> Self {
        Self {
            c: angle.tan(),
            ..Self::IDENTITY
        }
    }

    /// Creates a horizontal skew about the origin, `angle` in degrees.
    pub fn skew_x_degrees(angle: f32) -> Self {
        Self::skew_x_radians(angle.to_radians())
    }

    /// Creates a vertical skew about the origin, `angle` in radians.
    ///
    /// `y` gains `tan(angle) * x` and `x` is untouched -- the transpose of
    /// [`skew_x_radians`](Self::skew_x_radians), writing `b` where that
    /// writes `c`.
    pub fn skew_y_radians(angle: f32) -> Self {
        Self {
            b: angle.tan(),
            ..Self::IDENTITY
        }
    }

    /// Creates a vertical skew about the origin, `angle` in degrees.
    pub fn skew_y_degrees(angle: f32) -> Self {
        Self::skew_y_radians(angle.to_radians())
    }

    /// Creates a skew about the origin in both axes, angles in radians.
    ///
    /// Both shears are written into one matrix rather than composed, and that
    /// is not the same transform as chaining the two single-axis skews.
    /// Composing feeds one shear the other's output, which scales exactly one
    /// of `a` and `d` away from 1 by `tan(x_angle) * tan(y_angle)` -- `a` or
    /// `d` according to the order, so neither chained form is this matrix and
    /// they are not each other.
    pub fn skew_radians(x_angle: f32, y_angle: f32) -> Self {
        Self {
            b: y_angle.tan(),
            c: x_angle.tan(),
            ..Self::IDENTITY
        }
    }

    /// Creates a skew about the origin in both axes, angles in degrees.
    pub fn skew_degrees(x_angle: f32, y_angle: f32) -> Self {
        Self::skew_radians(x_angle.to_radians(), y_angle.to_radians())
    }

    /// Creates a transform that mirrors horizontally, about the `y` axis.
    ///
    /// The sign of `x` is reversed and `y` is untouched.
    pub fn flip_x() -> Self {
        Self {
            a: -1.0,
            ..Self::IDENTITY
        }
    }

    /// Creates a transform that mirrors vertically, about the `x` axis.
    ///
    /// The sign of `y` is reversed and `x` is untouched.
    pub fn flip_y() -> Self {
        Self {
            d: -1.0,
            ..Self::IDENTITY
        }
    }

    /// Creates a rotation about the origin by the direction of `(x, y)`,
    /// measured from the positive `x` axis.
    ///
    /// The vector's length is discarded, so `(3, 4)` and `(6, 8)` give the
    /// same rotation. A zero vector has no direction and gives
    /// [`IDENTITY`](Self::IDENTITY); without that case a negative zero would
    /// reach `atan2(-0.0, -0.0)`, which is a half turn rather than none.
    pub fn rotation_from_vector(x: f32, y: f32) -> Self {
        match x == 0.0 && y == 0.0 {
            true => Self::IDENTITY,
            false => Self::rotation_radians(y.atan2(x)),
        }
    }

    /// Builds a transform from a CSS `transform` list.
    ///
    /// `"translate(10px, 20px) rotate(45deg)"` is the matrix those two
    /// functions compose to, applied left to right as CSS applies them: the
    /// leftmost function is the outermost, so a point meets the rightmost
    /// first. `"none"`, the empty string and whitespace are
    /// [`IDENTITY`](Self::IDENTITY), which is how the property spells "no
    /// transform".
    ///
    /// Accepts `translate`, `translateX`, `translateY`, `rotate`, `scale`,
    /// `scaleX`, `scaleY`, `skew`, `skewX`, `skewY` and `matrix`, with the
    /// units CSS defines for each.
    ///
    /// # Errors
    ///
    /// `None` for anything it cannot read, and the whole list is refused
    /// rather than the readable parts kept -- a transform list missing one
    /// step puts the drawing somewhere else, and dropping the step nobody
    /// could parse is how a typo becomes a rendering bug. An angle without a
    /// unit is one of those: CSS requires it and browsers reject
    /// `rotate(45)`.
    ///
    /// `Option` rather than an error type, matching the rest of the CSS
    /// parsing in this crate: "that is not a transform" is the whole of what
    /// there is to say.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let m = Affine::from_css("translate(10px, 20px)").unwrap();
    /// assert_eq!((m.tx, m.ty), (10.0, 20.0));
    ///
    /// assert_eq!(Affine::from_css("none"), Some(Affine::IDENTITY));
    /// assert_eq!(Affine::from_css("rotate(45)"), None);
    /// ```
    pub fn from_css(text: &str) -> Option<Affine> {
        parse_transform(text)
    }

    /// Concatenates `other` onto this transform, `other` applying first.
    ///
    /// The same composition [`Context2D::transform`] performs, available
    /// without a context: a caller building a transform out of parts had to
    /// route the composition through a drawing context, which meant touching
    /// one they might not want to disturb.
    ///
    /// Order follows `DOMMatrix.multiply`, where the argument is the
    /// right-hand operand and therefore the one a point meets first --
    /// `scale.multiply(&shift)` shifts, then scales.
    ///
    /// [`Context2D::transform`]: crate::context2d::Context2D::transform
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let shift = Affine::translation(10.0, 0.0);
    /// let scale = Affine::scale(2.0, 2.0);
    /// // The point (1, 0) is shifted to (11, 0), then scaled to (22, 0).
    /// let combined = scale.multiply(&shift);
    /// assert_eq!(combined.tx, 20.0);
    /// assert_eq!(combined.a, 2.0);
    /// ```
    pub fn multiply(&self, other: &Affine) -> Affine {
        Affine {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            tx: self.a * other.tx + self.c * other.ty + self.tx,
            ty: self.b * other.tx + self.d * other.ty + self.ty,
        }
    }

    /// The transform that undoes this one, or `None` where none exists.
    ///
    /// `None` for a singular transform -- one whose determinant is zero,
    /// which is any transform collapsing the plane onto a line or a point,
    /// such as `scale(0.0, 1.0)` -- and for one carrying a non-finite
    /// component, where the arithmetic has no meaning to return.
    ///
    /// `DOMMatrix.inverse()` answers the same question with a matrix full of
    /// `NaN`; an `Option` says it in the type instead, so a caller cannot
    /// carry the failure into a draw by accident.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let shift = Affine::translation(10.0, 20.0);
    /// let back = shift.inverse().expect("a translation is invertible");
    /// assert_eq!(back.tx, -10.0);
    ///
    /// // The two compose to the identity.
    /// assert_eq!(shift.multiply(&back), Affine::IDENTITY);
    ///
    /// // A transform that flattens the plane has no inverse.
    /// assert!(Affine::scale(0.0, 1.0).inverse().is_none());
    /// ```
    pub fn inverse(&self) -> Option<Affine> {
        let determinant = self.a * self.d - self.b * self.c;
        if determinant == 0.0 || !determinant.is_finite() {
            return None;
        }
        let inverted = Affine {
            a: self.d / determinant,
            b: -self.b / determinant,
            c: -self.c / determinant,
            d: self.a / determinant,
            tx: (self.c * self.ty - self.d * self.tx) / determinant,
            ty: (self.b * self.tx - self.a * self.ty) / determinant,
        };
        // A finite determinant does not make the result finite: a translation
        // large enough overflows when divided by a very small determinant.
        match [
            inverted.a,
            inverted.b,
            inverted.c,
            inverted.d,
            inverted.tx,
            inverted.ty,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            true => Some(inverted),
            false => None,
        }
    }

    /// Applies the transform to a point.
    ///
    /// The translation is included, which is what separates transforming a
    /// position from transforming a direction: a translation moves the point
    /// `(0, 0)` and would leave a direction alone.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// // The scale applies first, so (2, 3) doubles to (4, 6) and then
    /// // shifts to (5, 7).
    /// let m = Affine::translation(1.0, 1.0).multiply(&Affine::scale(2.0, 2.0));
    /// let p = m.transform_point(Point::new(2.0, 3.0));
    /// assert_eq!((p.x, p.y), (5.0, 7.0));
    /// ```
    pub fn transform_point(&self, point: Point) -> Point {
        Point {
            x: self.a * point.x + self.c * point.y + self.tx,
            y: self.b * point.x + self.d * point.y + self.ty,
        }
    }

    /// Returns `true` when the transform leaves every point where it is.
    ///
    /// Exact component equality rather than a tolerance, matching
    /// `DOMMatrix.isIdentity`. A transform that maps every point to within a
    /// rounding error of itself can still answer `false`: composing a
    /// four-degree rotation with its own inverse leaves `a` at 0.99999994.
    /// Most rotations do round-trip exactly -- 336 of the 360 whole degrees
    /// do -- which is why this cannot be relied on either way.
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Rect {
    /// Creates a rectangle from an origin and an extent.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let r = Rect::from_xywh(10.0, 20.0, 30.0, 40.0);
    /// assert_eq!(r.right, 40.0);
    /// assert_eq!(r.height(), 40.0);
    /// ```
    pub fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        }
    }

    /// Returns the horizontal extent. Negative for an inverted rectangle.
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    /// Returns the vertical extent. Negative for an inverted rectangle.
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    /// Returns `true` when the rectangle encloses no area, which includes
    /// the inverted case where an edge pair is the wrong way round.
    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::{Affine, Point};

    /// About eight `f32` ulp at the magnitudes below -- one ulp near 3.3 is
    /// 2.4e-7 -- which is loose enough for a four-term dot product and five
    /// orders of magnitude tighter than any difference these tests are asked
    /// to detect: the reversed composition order below misses by 0.29, not by
    /// a rounding error.
    const TOL: f64 = 2e-6;

    /// `tan(30 degrees)` is `1 / sqrt(3)`, evaluated from the identity rather
    /// than from `skew_x_radians`, which is the thing under test.
    const TAN_30: f64 = 0.577_350_269_189_625_7;

    /// `tan(20 degrees)`, from an external table.
    const TAN_20: f64 = 0.363_970_234_266_202_34;

    /// The expected value is `f64` so that a figure taken from the JavaScript
    /// reference can be written exactly as that reference printed it, rather
    /// than truncated to `f32` by hand at the point of comparison.
    fn close(actual: f32, expected: f64) {
        assert!(
            (f64::from(actual) - expected).abs() <= TOL,
            "expected {expected}, got {actual}"
        );
    }

    fn six(m: &Affine) -> [f32; 6] {
        [m.a, m.b, m.c, m.d, m.tx, m.ty]
    }

    /// A transform with no zero component and no symmetry, so that a
    /// composition performed in the wrong order cannot coincide with the
    /// right one.
    fn base() -> Affine {
        Affine {
            a: 2.0,
            b: 0.5,
            c: -0.25,
            d: 3.0,
            tx: 10.0,
            ty: 20.0,
        }
    }

    #[test]
    fn each_skew_writes_the_shear_its_axis_names() {
        let x = Affine::skew_x_degrees(30.0);
        close(x.c, TAN_30);
        // The horizontal skew must leave the vertical shear alone, or it is
        // the transpose of itself and every other assertion still passes.
        assert_eq!(x.b, 0.0);
        assert_eq!((x.a, x.d, x.tx, x.ty), (1.0, 1.0, 0.0, 0.0));

        let y = Affine::skew_y_degrees(30.0);
        close(y.b, TAN_30);
        assert_eq!(y.c, 0.0);

        close(Affine::skew_x_radians(std::f32::consts::FRAC_PI_4).c, 1.0);

        // The figure `skew_x_radians`'s doc comment quotes for a quarter
        // turn, asserted so that the comment cannot go stale in silence. The
        // sign is the surprising part and is the reason it is documented.
        assert_eq!(Affine::skew_x_degrees(90.0).c, -22_877_332.0);
    }

    /// The six components of `new DOMMatrix([2, 0.5, -0.25, 3, 10, 20])` with
    /// each operation applied, taken from `lib/classes/geometry.js` and
    /// reproduced by hand from its `multiply` helper. The base is deliberately
    /// not the identity: from the identity every ordering agrees, which is why
    /// the values a caller can recall do not pin the order down.
    #[test]
    fn the_operations_compose_the_way_dommatrix_does() {
        let skewed_x = base().multiply(&Affine::skew_x_degrees(30.0));
        // 2 * tan(30) - 0.25, not 2 + 0.5 * tan(30), which is what the
        // reversed order gives and which differs in `a` as well as `c`.
        for (got, want) in six(&skewed_x).iter().zip([
            2.0,
            0.5,
            0.904_700_538_379_251_5,
            3.288_675_134_594_813,
            10.0,
            20.0,
        ]) {
            close(*got, want);
        }

        let skewed_y = base().multiply(&Affine::skew_y_degrees(20.0));
        for (got, want) in six(&skewed_y).iter().zip([
            1.909_007_441_433_449_5,
            1.591_910_702_798_607,
            -0.25,
            3.0,
            10.0,
            20.0,
        ]) {
            close(*got, want);
        }

        let rotated = base().multiply(&Affine::rotation_from_vector(3.0, 4.0));
        for (got, want) in
            six(&rotated).iter().zip([1.0, 2.7, -1.75, 1.4, 10.0, 20.0])
        {
            close(*got, want);
        }
    }

    /// The doc comment on [`Affine::skew_radians`] claims the two-axis skew is
    /// not the two single-axis skews composed. That is a claim about numbers
    /// and is checkable.
    #[test]
    fn a_two_axis_skew_is_not_the_single_axis_skews_composed() {
        let both = Affine::skew_degrees(30.0, 20.0);
        close(both.c, TAN_30);
        close(both.b, TAN_20);
        assert_eq!((both.a, both.d), (1.0, 1.0));

        // Composing puts `tan(30) * tan(20)` into exactly one of `a` and `d`,
        // and which one is decided by the order -- so neither chained form is
        // this matrix, and they are not each other either.
        let x_then_y = Affine::skew_x_degrees(30.0)
            .multiply(&Affine::skew_y_degrees(20.0));
        close(x_then_y.a, 1.0 + TAN_30 * TAN_20);
        assert_eq!(x_then_y.d, 1.0);

        let y_then_x = Affine::skew_y_degrees(20.0)
            .multiply(&Affine::skew_x_degrees(30.0));
        assert_eq!(y_then_x.a, 1.0);
        close(y_then_x.d, 1.0 + TAN_30 * TAN_20);

        assert_ne!(x_then_y.a, both.a);
        assert_ne!(x_then_y, y_then_x);
    }

    #[test]
    fn flipping_reverses_one_axis_and_leaves_the_other() {
        assert_eq!(six(&Affine::flip_x()), [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(six(&Affine::flip_y()), [1.0, 0.0, 0.0, -1.0, 0.0, 0.0]);

        // Composed onto a transform, a flip reaches the column it owns and
        // not the translation -- the same answer `DOMMatrix.flipX` gives.
        assert_eq!(
            six(&base().multiply(&Affine::flip_x())),
            [-2.0, -0.5, -0.25, 3.0, 10.0, 20.0]
        );
        assert_eq!(
            six(&base().multiply(&Affine::flip_y())),
            [2.0, 0.5, 0.25, -3.0, 10.0, 20.0]
        );
    }

    #[test]
    fn a_rotation_from_a_vector_uses_its_direction_and_not_its_length() {
        // The 3-4-5 triangle: cosine 0.6 and sine 0.8, both exact, so this
        // does not go through anyone's `atan2`.
        let r = Affine::rotation_from_vector(3.0, 4.0);
        close(r.a, 0.6);
        close(r.b, 0.8);
        close(r.c, -0.8);
        close(r.d, 0.6);

        let longer = Affine::rotation_from_vector(6.0, 8.0);
        close(longer.a, f64::from(r.a));
        close(longer.b, f64::from(r.b));

        // A zero vector has no direction. `atan2(-0.0, -0.0)` is a half turn,
        // so the guard is what keeps this from being one.
        assert!(Affine::rotation_from_vector(0.0, 0.0).is_identity());
        assert!(Affine::rotation_from_vector(-0.0, -0.0).is_identity());
    }

    #[test]
    fn transforming_a_point_includes_the_translation() {
        let p = base().transform_point(Point::new(2.0, 3.0));
        // 2 * 2 + (-0.25) * 3 + 10, and 0.5 * 2 + 3 * 3 + 20.
        close(p.x, 13.25);
        close(p.y, 30.0);

        // Without the translation term this would be (4, 6).
        let shifted =
            Affine::translation(1.0, 1.0).transform_point(Point::new(2.0, 3.0));
        assert_eq!((shifted.x, shifted.y), (3.0, 4.0));
    }

    #[test]
    fn is_identity_is_exact_rather_than_approximate() {
        assert!(Affine::IDENTITY.is_identity());
        assert!(Affine::default().is_identity());
        assert!(!base().is_identity());

        // Four degrees is one of the 24 whole-degree rotations that do not
        // round-trip exactly through their own inverse in `f32`; the other
        // 336 do, so the angle here is load-bearing and not an example.
        let r = Affine::rotation_degrees(4.0);
        let round_trip =
            r.multiply(&r.inverse().expect("a rotation is invertible"));
        assert!(!round_trip.is_identity());
        close(round_trip.a, 1.0);
        close(round_trip.b, 0.0);
    }
}
