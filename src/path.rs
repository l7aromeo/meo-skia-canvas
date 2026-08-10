use skia_safe::{Path as SkPath, PathFillType, utils::parse_path};

use crate::error::Error;

/// Path winding rule. Matches SVG / Canvas semantics:
/// - `NonZero` (Skia's `Winding`) fills any region whose net winding is
///   non-zero.
/// - `EvenOdd` fills any region with an odd winding count.
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

/// Vector path. Currently only constructible from SVG path data (the same
/// `d=""` syntax used by SVG `<path>` elements).
/// More constructors land alongside their use cases.
pub struct Path {
    pub(crate) inner: SkPath,
}

impl Path {
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

impl std::fmt::Debug for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Path")
            .field("fill_rule", &self.fill_rule())
            .finish()
    }
}
