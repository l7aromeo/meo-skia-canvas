//! Tiled fill styles.
//!
//! A pattern repeats a source image, or the contents of another canvas,
//! across whatever it fills. It is the Canvas API's `createPattern`, built
//! through [`Context2D::create_pattern`](crate::context2d::Context2D::create_pattern)
//! and installed with
//! [`Context2D::set_fill_pattern`](crate::context2d::Context2D::set_fill_pattern).
//!
//! Patterns tile in the canvas's own coordinate space, not the shape's, so
//! two shapes filled with the same pattern line up rather than each starting
//! their tiling afresh.
//! [`Pattern::set_transform`](crate::pattern::Pattern::set_transform) moves the
//! tiling itself.

use skia_safe::TileMode;

use crate::{geometry::Affine, node::pattern::CanvasPattern};

/// Which axes a pattern repeats along.
///
/// The Canvas API spells these `"repeat"`, `"repeat-x"`, `"repeat-y"` and
/// `"no-repeat"`. An axis that does not repeat draws nothing beyond the
/// tile -- it does not smear the tile's edge pixels outwards, which is what
/// clamping would do and what the standard does not ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PatternRepeat {
    /// Tiles along both axes. The default.
    #[default]
    Repeat,
    /// Tiles horizontally; draws nothing above or below the tile.
    RepeatX,
    /// Tiles vertically; draws nothing left or right of the tile.
    RepeatY,
    /// Draws the tile once and nothing beyond it.
    NoRepeat,
}

impl PatternRepeat {
    pub(crate) fn to_skia(self) -> (TileMode, TileMode) {
        match self {
            // `Decal`, not `Clamp`: clamping smears the tile's edge texel
            // across the rest of the fill, which is not what "no-repeat"
            // means. Matched to `to_repeat_mode` in the Node binding.
            Self::Repeat => (TileMode::Repeat, TileMode::Repeat),
            Self::RepeatX => (TileMode::Repeat, TileMode::Decal),
            Self::RepeatY => (TileMode::Decal, TileMode::Repeat),
            Self::NoRepeat => (TileMode::Decal, TileMode::Decal),
        }
    }
}

/// A repeating fill built from an image or another canvas.
///
/// Cheap to clone: the tile is shared, so a clone is another handle to the
/// same source rather than a second copy of the pixels. A
/// [`Pattern::set_transform`] on one handle is therefore visible through the
/// others, matching how the JavaScript object behaves.
#[derive(Clone)]
pub struct Pattern {
    pub(crate) inner: CanvasPattern,
}

impl Pattern {
    pub(crate) fn from_inner(inner: CanvasPattern) -> Self {
        Self { inner }
    }

    /// Sets the transform applied to the tiling.
    ///
    /// Applies to the pattern's own grid, not to the shape being filled, so
    /// this rotates or scales the texture while leaving the geometry alone.
    /// Replaces any previous transform rather than composing with it.
    pub fn set_transform(&mut self, transform: Affine) {
        self.inner
            .set_matrix(crate::context2d::affine_to_matrix(transform));
    }

    /// The width of one tile, in pixels.
    pub fn width(&self) -> f32 {
        self.inner.dims().width
    }

    /// The height of one tile, in pixels.
    pub fn height(&self) -> f32 {
        self.inner.dims().height
    }
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pattern")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
