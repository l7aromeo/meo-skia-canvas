//! Plain geometric value types shared across the public API.
//!
//! All coordinates are in pixels in the surface's own space, with the origin
//! at the top left and `y` increasing downwards, matching the Canvas 2D
//! convention rather than a mathematical one.

/// A point in surface space.
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
/// Nothing enforces that -- [`Rect::is_empty`] is how you check, and
/// [`Rect::from_xywh`] produces a well-formed rectangle for non-negative
/// extents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Smaller x edge.
    pub left: f32,
    /// Smaller y edge.
    pub top: f32,
    /// Larger x edge.
    pub right: f32,
    /// Larger y edge.
    pub bottom: f32,
}

/// 2D affine transform in `[a, b, c, d, tx, ty]` form, matching the
/// CSS `DOMMatrix2DInit` and `CanvasRenderingContext2D.setTransform`
/// convention. Acts on a column vector `[x, y, 1]^T`:
///
/// ```text
/// | a  c  tx |   | x |
/// | b  d  ty | * | y |
/// | 0  0  1  |   | 1 |
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
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
