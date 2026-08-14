//! Hatch and stipple fill styles.
//!
//! A texture fills with a repeated line or path rather than with pixels, so
//! it stays crisp at any scale and survives export to
//! [`Pdf`](crate::export::ImageFormat::Pdf) and
//! [`Svg`](crate::export::ImageFormat::Svg) as vectors. That is the
//! difference from [`Pattern`](crate::pattern::Pattern), which tiles a
//! bitmap.
//!
//! Not part of the Canvas standard. Built with
//! [`Texture::new`](crate::texture::Texture::new) and installed through
//! [`Context2D::set_fill_texture`](crate::context2d::Context2D::set_fill_texture).

use std::fmt;

use crate::{
    color::{RgbaLinear, rgba_linear_to_skia_color},
    node::texture::CanvasTexture,
    paint::StrokeCap,
    path::Path2D,
};

/// How a texture's repeating mark is drawn.
///
/// Construct by updating the default, as with the other option structs in
/// this crate:
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// // 45-degree hatching, two-pixel lines, eight pixels apart.
/// let hatch = TextureOptions {
///     angle: std::f32::consts::FRAC_PI_4,
///     line: 2.0,
///     spacing: (8.0, 8.0),
///     ..TextureOptions::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct TextureOptions {
    /// The shape stamped at each grid position.
    ///
    /// `None` draws straight lines instead, which is the hatching case and
    /// the reason [`spacing`](TextureOptions::spacing) then collapses to its
    /// larger component: parallel lines have only one meaningful period, so
    /// `(4.0, 12.0)` and `(12.0, 12.0)` draw the same hatching.
    pub path: Option<Path2D>,
    /// Color of the mark. Defaults to opaque black.
    pub color: RgbaLinear,
    /// Stroke width of the mark, in pixels.
    ///
    /// `0.0` fills the path instead of stroking it, which is what a stipple
    /// of solid dots wants. Defaults to `1.0`.
    pub line: f32,
    /// End cap for the stroked mark. Ignored when
    /// [`line`](TextureOptions::line) is `0.0`.
    pub cap: StrokeCap,
    /// Rotation of the whole grid, in radians. Defaults to `0.0`.
    pub angle: f32,
    /// Whether the texture draws only the marks, rather than clipping the
    /// filled shape to them.
    ///
    /// Defaults to `false`, which is the fill behaviour.
    pub outline: bool,
    /// Horizontal and vertical period of the grid, in pixels.
    pub spacing: (f32, f32),
    /// Offset of the grid's origin, in pixels. Shifts the whole texture
    /// without moving what it fills.
    pub offset: (f32, f32),
}

impl Default for TextureOptions {
    fn default() -> Self {
        Self {
            path: None,
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            line: 1.0,
            cap: StrokeCap::Butt,
            angle: 0.0,
            outline: false,
            spacing: (8.0, 8.0),
            offset: (0.0, 0.0),
        }
    }
}

/// A fill made of a repeated vector mark.
///
/// Cheap to clone: the tile is shared rather than copied.
#[derive(Clone)]
#[doc(alias = "CanvasTexture")]
pub struct Texture {
    pub(crate) inner: CanvasTexture,
}

impl Texture {
    /// Builds a texture from its settings.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(100.0, 100.0);
    /// let hatch = Texture::new(&TextureOptions {
    ///     angle: std::f32::consts::FRAC_PI_4,
    ///     spacing: (6.0, 6.0),
    ///     ..TextureOptions::default()
    /// });
    ///
    /// let ctx = canvas.context();
    /// ctx.set_fill_texture(&hatch);
    /// ctx.fill_rect(0.0, 0.0, 100.0, 100.0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(options: &TextureOptions) -> Self {
        Self {
            inner: CanvasTexture::from_parts(
                options.path.as_ref().map(|path| path.inner.clone()),
                rgba_linear_to_skia_color(options.color),
                options.line,
                options.cap.to_skia(),
                options.angle,
                options.outline,
                options.spacing,
                options.offset,
            ),
        }
    }

    /// The grid period the texture draws at.
    ///
    /// The same pair [`TextureOptions::spacing`] was given, except for a
    /// texture with no path: parallel lines have one period, so the wider
    /// component wins on both axes and that is what is reported. Reporting
    /// the pair as given described a grid nothing drew.
    pub fn spacing(&self) -> (f32, f32) {
        let spacing = self.inner.spacing();
        (spacing.x, spacing.y)
    }
}

impl fmt::Debug for Texture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Texture")
            .field("spacing", &self.spacing())
            .finish_non_exhaustive()
    }
}
