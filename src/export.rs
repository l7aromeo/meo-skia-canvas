//! Encoded output: image formats and the options that shape them.
//!
//! [`Canvas::to_buffer`](crate::canvas::Canvas::to_buffer) and its siblings
//! take the types here. They mirror the internal export options without
//! exposing `skia_safe`, the same way [`PixelExportOptions`] mirrors the
//! pixel-readback types.
//!
//! [`PixelExportOptions`]: crate::pixels::PixelExportOptions

use crate::{
    color::{OutputColorSpace, RgbaLinear, rgba_linear_to_skia_color},
    context::page::ExportOptions,
    error::Error,
};

/// A container format for encoded output.
///
/// [`Png`], [`Jpeg`] and [`Webp`] rasterize; [`Pdf`] and [`Svg`] keep the
/// drawing as vectors and need recorded content rather than a rasterized
/// surface. [`Raw`] skips encoding and returns the pixel bytes.
///
/// [`Png`]: ImageFormat::Png
/// [`Jpeg`]: ImageFormat::Jpeg
/// [`Webp`]: ImageFormat::Webp
/// [`Pdf`]: ImageFormat::Pdf
/// [`Svg`]: ImageFormat::Svg
/// [`Raw`]: ImageFormat::Raw
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFormat {
    /// Lossless raster. The default.
    #[default]
    Png,
    /// Lossy raster. Honours [`EncodeOptions::quality`], and flattens
    /// transparency against [`EncodeOptions::matte`] or black.
    Jpeg,
    /// Raster, lossy or lossless depending on
    /// [`EncodeOptions::quality`].
    Webp,
    /// Vector document. Preserves text and paths as vectors.
    Pdf,
    /// Vector markup. Preserves text and paths as vectors.
    Svg,
    /// Unencoded pixel bytes, in the surface's own layout.
    Raw,
}

impl ImageFormat {
    /// Returns whether the format keeps drawings as vectors.
    ///
    /// Vector formats need recorded content: rasterizing first and encoding
    /// the result would embed a single bitmap rather than preserve the
    /// geometry.
    pub fn is_vector(self) -> bool {
        matches!(self, Self::Pdf | Self::Svg)
    }

    /// Returns the IANA media type for the format.
    ///
    /// [`Raw`](ImageFormat::Raw) has no registered type and reports
    /// `application/octet-stream`.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Pdf => "application/pdf",
            Self::Svg => "image/svg+xml",
            Self::Raw => "application/octet-stream",
        }
    }

    /// Returns the conventional file extension, without a leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Pdf => "pdf",
            Self::Svg => "svg",
            Self::Raw => "bin",
        }
    }

    /// Infers a format from a file extension, case-insensitively.
    ///
    /// Accepts the extension with or without a leading dot, and treats
    /// `jpeg` and `jpg` alike. Returns `None` for anything else, so a
    /// caller can decide between a default and an error.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "pdf" => Some(Self::Pdf),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// The token the internal encoder matches on.
    pub(crate) fn as_internal_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Pdf => "pdf",
            Self::Svg => "svg",
            Self::Raw => "raw",
        }
    }
}

/// Settings applied while encoding.
///
/// Construct by updating the default, as with the other option structs in
/// this crate:
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// let options = EncodeOptions {
///     quality: 0.8,
///     density: 2.0,
///     ..EncodeOptions::default()
/// };
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeOptions {
    /// Lossy-encoder quality, from `0.0` to `1.0`. Ignored by the lossless
    /// formats. Defaults to `0.92`, and is clamped into range.
    pub quality: f32,
    /// Scale applied while rasterizing, so `2.0` yields a double-resolution
    /// image from the same drawing. Defaults to `1.0`.
    pub density: f32,
    /// Color composited underneath the drawing before encoding.
    ///
    /// `None` keeps transparency where the format supports it. The formats
    /// that cannot represent it, such as [`Jpeg`](ImageFormat::Jpeg), fall
    /// back to black.
    pub matte: Option<RgbaLinear>,
    /// Whether the vector formats convert text to paths.
    ///
    /// Outlined text renders identically without the font installed, at the
    /// cost of no longer being selectable or searchable. Defaults to `true`.
    pub outline: bool,
    /// Color space tagged on the encoded output. Defaults to
    /// [`OutputColorSpace::Srgb`].
    pub color_space: OutputColorSpace,
    /// Whether the JPEG encoder subsamples chroma. Defaults to `false`,
    /// which keeps full chroma resolution at a larger file size.
    pub jpeg_downsample: bool,
    /// Multisample count for the rasterizing pass, or `None` for the
    /// backend's default.
    pub msaa: Option<usize>,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 0.92,
            density: 1.0,
            matte: None,
            outline: true,
            color_space: OutputColorSpace::Srgb,
            jpeg_downsample: false,
            msaa: None,
        }
    }
}

impl EncodeOptions {
    /// Lowers these settings onto the internal encoder's options.
    ///
    /// The text contrast and gamma the encoder also accepts are left at
    /// their tuned defaults rather than surfaced: they trade glyph weight
    /// against the rasterizer's gamma correction, and exposing them invites
    /// output that differs from the Node binding for no stated reason.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when [`color_space`](Self::color_space) cannot be
    /// realized by this build.
    pub(crate) fn to_internal(
        &self,
        format: ImageFormat,
    ) -> Result<ExportOptions, Error> {
        Ok(ExportOptions {
            format: format.as_internal_str().to_string(),
            quality: self.quality.clamp(0.0, 1.0),
            density: self.density,
            outline: self.outline,
            matte: self.matte.map(rgba_linear_to_skia_color),
            msaa: self.msaa,
            color_space: self.color_space.to_skia_color_space()?,
            jpeg_downsample: self.jpeg_downsample,
            ..ExportOptions::default()
        })
    }
}
