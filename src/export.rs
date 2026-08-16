//! Encoded output: image formats and the options that shape them.
//!
//! [`Canvas::to_buffer`](crate::canvas::Canvas::to_buffer) and its siblings
//! take the types here. They mirror the internal export options without
//! exposing `skia_safe`, the same way [`PixelExportOptions`] mirrors the
//! pixel-readback types.
//!
//! [`PixelExportOptions`]: crate::pixels::PixelExportOptions

use std::ops::Range;

use crate::{
    color::{RgbaLinear, rgba_linear_to_skia_color},
    context::page::ExportOptions,
    encode::avif,
    error::Error,
    pixels::{PixelColorSpace, PixelDepth},
};

/// What a draw used that a vector backend may not be able to express.
///
/// Both document backends refuse some of what a canvas can draw, and they
/// refuse *different* things, which is why this records the features rather
/// than a yes or no. Measured against each backend rather than assumed --
/// the same drawing was written both ways and compared with the raster
/// export of it:
///
/// | feature                        | SVG      | PDF   |
/// |--------------------------------|----------|-------|
/// | sweep gradient, runtime shader | dropped  | fine  |
/// | image filter, shadow           | dropped  | fine  |
/// | mask filter                    | dropped  | fine  |
/// | blend mode past source-over    | dropped  | wrong |
///
/// A draw carrying something its backend cannot take is recorded in a layer
/// of its own and rasterized into the document at export time, so the file
/// says what the canvas drew. Everything else stays vector, which is the
/// point of asking per feature: PDF renders a shadowed, gradient-filled page
/// perfectly, and rasterizing it because SVG could not would cost fidelity
/// and size for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct VectorFeatures(u8);

impl VectorFeatures {
    /// A blend mode past source-over.
    pub(crate) const BLEND_MODE: Self = Self(1 << 3);
    /// A sweep gradient or a procedural shader. SVG names four paint
    /// servers and neither of these is among them.
    pub(crate) const EXOTIC_SHADER: Self = Self(1 << 0);
    /// An image filter, which is also how a shadow is drawn here.
    pub(crate) const IMAGE_FILTER: Self = Self(1 << 1);
    /// A mask filter.
    pub(crate) const MASK_FILTER: Self = Self(1 << 2);
    /// What the PDF backend gets wrong, which is blend modes and nothing
    /// else: a conic gradient, a shadow and a `blur()` all come out of it
    /// pixel-identical to the raster export, and a `multiply` moves a fifth
    /// of the page.
    pub(crate) const PDF_CANNOT: Self = Self::BLEND_MODE;
    /// Nothing a backend could object to.
    pub(crate) const PLAIN: Self = Self(0);
    /// Everything the SVG backend drops.
    pub(crate) const SVG_CANNOT: Self = Self(
        Self::EXOTIC_SHADER.0
            | Self::IMAGE_FILTER.0
            | Self::MASK_FILTER.0
            | Self::BLEND_MODE.0,
    );

    pub(crate) fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) fn any(self) -> bool {
        self.0 != 0
    }

    /// Whether this draw uses anything the backend cannot express.
    pub(crate) fn refused_by(self, backend: Self) -> bool {
        self.0 & backend.0 != 0
    }
}

/// The resolution a canvas exports at [`EncodeOptions::density`] of 1.
///
/// Seventy-two dots per inch, which is the CSS reference pixel and so the
/// only number a canvas has any claim to: a page is measured in pixels and
/// nothing tells it how large they are meant to be.
///
/// Visible outside this module for the PDF backend alone. Every other site
/// in the family goes through [`dots_per_inch`] or [`pixels_per_metre`];
/// Skia's `raster_dpi` wants a float and those round to integers, so it is
/// the one place that needs the number rather than a resolution derived
/// from it.
pub(crate) const NOMINAL_DPI: f32 = 72.0;

/// The top of the quality scale the encoders take.
///
/// A hundred, because they speak in percent while this crate's public dial
/// is `0.0` to `1.0` -- see [`encoder_quality`].
pub(crate) const QUALITY_SCALE: f32 = 100.0;

/// `quality` on the nought-to-a-hundred scale the encoders take.
///
/// One function so there is one rule. There were five sites and three of
/// them: two rescaled and clamped as `f32`, one rescaled and clamped as
/// `u32`, and the two the PDF backend uses did not clamp at all. None of
/// the three was reachably wrong -- [`EncodeOptions::validate`] refuses a
/// quality outside `0.0..=1.0` before any of them runs, which is what made
/// all three clamps dead code and made it impossible to tell which one was
/// meant to be load-bearing.
pub(crate) fn encoder_quality(quality: f32) -> f32 {
    (quality * QUALITY_SCALE).clamp(0.0, QUALITY_SCALE)
}

/// Inches in a metre, for the formats that record resolution per metre.
///
/// PNG's `pHYs` chunk and BMP's `bV4XPelsPerMeter` both do; JPEG's JFIF
/// header and WebP's EXIF record dots per inch directly.
const INCHES_PER_METRE: f64 = 39.3701;

/// The resolution `density` implies, in dots per inch.
///
/// Rounded, and rounded here rather than at each call site, because the
/// three sites that needed it had two different rules between them and one
/// of those silently truncated. See
/// [`pixels_per_metre`](self::pixels_per_metre) for what that cost.
pub(crate) fn dots_per_inch(density: f32) -> u16 {
    (NOMINAL_DPI * density)
        .round()
        .clamp(0.0, f32::from(u16::MAX)) as u16
}

/// The resolution `density` implies, in the pixels per metre PNG and BMP
/// record it in.
///
/// One function so there is one rounding rule. There were three sites and
/// two rules: JPEG truncated `density` to a whole number before multiplying
/// -- so a 1.5x export declared 72 DPI and a 0.5x export declared none at
/// all -- while PNG and BMP truncated the metre conversion instead, landing
/// on 2834 where the conventional 72-DPI value is 2835. Neither was visible
/// in a file, and a test that divided the value back and rounded could not
/// see the second one either, because 2834 and 2835 both return 72.
pub(crate) fn pixels_per_metre(density: f32) -> u32 {
    let dots = f64::from(NOMINAL_DPI) * f64::from(density);
    (dots * INCHES_PER_METRE)
        .round()
        .clamp(0.0, f64::from(u32::MAX)) as u32
}

/// A list of numbers as a reader would say it: `8`, `8 or 10`, `8, 10 or
/// 12`.
///
/// Written from the list rather than into each message, so a format that
/// gains a depth does not leave an error behind claiming otherwise.
fn listed(values: &[u8]) -> String {
    match values.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => {
            let rest: Vec<_> = rest.iter().map(u8::to_string).collect();
            format!("{} or {last}", rest.join(", "))
        }
        None => String::new(),
    }
}

/// What a format stores: pixels, or the geometry that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Content {
    /// Pixels. Needs a rasterizing pass, and its output can be cached.
    Raster,
    /// Geometry. Needs the recorded drawing commands instead, because
    /// rasterizing first would embed one bitmap and lose the paths.
    Vector,
}

/// What turns a rendered page into bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncoderKind {
    /// Skia's own encoders, which cover exactly JPEG, PNG and WebP, plus the
    /// PDF and SVG document backends.
    Skia,
    /// An encoder in this crate's `encode` module, fed rasterized frames.
    ///
    /// Not a fallback: it is where a format goes when Skia has no encoder for
    /// it at all, which is every format past those five.
    Foreign,
    /// No encoding. The pixel bytes as they sit.
    Unencoded,
}

/// Whether a file can say which color space its pixels are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorSignal {
    /// The container has somewhere to declare it, so an export keeps the
    /// canvas's own space and the file says which one it is.
    Declared,
    /// The container has nowhere to declare it, so the pixels are converted
    /// to sRGB on the way out and the file's silence is true.
    ///
    /// Only [`Gif`](ImageFormat::Gif). Its palette is bare eight-bit
    /// triples, and GIF89a's one extension mechanism -- the application
    /// extension block -- never had a color-management block registered for
    /// it. Writing Display P3 pixels into it would produce a file that says
    /// sRGB and is not, which is worse than narrowing.
    AssumedSrgb,
}

/// How many of a canvas's pages one file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageUse {
    /// One page -- the current one, or whichever
    /// [`EncodeOptions::page`] names.
    One,
    /// Every page, in order, in a single file.
    All,
}

/// What is fixed about a format, in one place.
///
/// These two axes used to be inferred from the format name at each site that
/// needed them -- `format != "pdf" && format != "svg"` for one,
/// `format == "pdf"` for the other -- which quietly assumed they were the
/// same axis, because every format then in the crate made them agree: vector
/// meant all-pages and raster meant one page. An animated raster format is
/// both raster and all-pages, so the assumption had to go before one could
/// exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FormatTraits {
    /// What turns a page into bytes.
    pub encoder: EncoderKind,
    /// How many pages one file carries.
    pub pages: PageUse,
    /// Whether those pages are frames with durations.
    ///
    /// Not the same question as [`pages`](Self::pages), which is why it is
    /// a separate field rather than derived from it: TIFF, ICO and PDF all
    /// gather every page and none of them has a clock. A format that is not
    /// animated has nowhere to put `fps`, `frame_delays` or `loops`, and
    /// being handed them is a caller error rather than something to ignore.
    pub animated: bool,
    /// Pixels or geometry.
    pub content: Content,
    /// Whether the file can say which color space it holds.
    ///
    /// Separate from every other axis here for the same reason
    /// [`animated`](Self::animated) is separate from [`pages`](Self::pages):
    /// nothing else predicts it. GIF is a raster format that spans pages and
    /// cannot describe colour; TIFF is a raster format that spans pages and
    /// can. Reading one from the other would get both wrong.
    pub color: ColorSignal,
    /// The IANA media type.
    pub mime: &'static str,
    /// What a caller asks for this format by, and what an error calls it.
    ///
    /// Usually the same word as [`extension`](Self::extension), and not for
    /// [`Raw`](ImageFormat::Raw): `toBuffer("raw")` asks by name, while a
    /// file written that way is `.bin`.
    pub name: &'static str,
    /// The conventional file extension, without a leading dot.
    pub extension: &'static str,
    /// Other names for the same format, such as `jpeg` for `jpg`.
    pub aliases: &'static [&'static str],
    /// Whether a filename may name this format.
    ///
    /// False for [`Raw`](ImageFormat::Raw): a file called `.bin` says
    /// nothing about its pixel layout, so inferring one would write bytes
    /// nothing can read back.
    pub inferable: bool,
    /// The depths a caller may ask this format's files to be written at,
    /// through [`EncodeOptions::bit_depth`].
    ///
    /// Empty for every format but [`Avif`](ImageFormat::Avif), which is not
    /// a claim that the rest write eight bits: PNG, APNG and TIFF all write
    /// sixteen from a canvas that has sixteen. It is that their depths are
    /// the ones a readback format already names, so
    /// [`color_type`](EncodeOptions::color_type) is the dial and a second
    /// one would be a second answer to the same question. AVIF's ten and
    /// twelve are the depths no readback format can name.
    pub depths: &'static [u8],
}

/// A container format for encoded output.
///
/// Twelve of them, and four axes tell them apart. [`Png`], [`Jpeg`],
/// [`Webp`], [`Bmp`] and [`Avif`] rasterize one page. [`Gif`] and [`Apng`]
/// rasterize every page into one animation, and [`Tiff`] and [`Ico`] gather
/// every page without any of them having a duration. [`Pdf`] and [`Svg`]
/// keep the drawing as vectors and need recorded content rather than a
/// rasterized surface. [`Raw`] skips encoding and returns the pixel bytes.
///
/// `FormatTraits` is where those axes live, so a question about a format is
/// answered by one table rather than by a match at each site that asks.
///
/// [`Png`]: ImageFormat::Png
/// [`Jpeg`]: ImageFormat::Jpeg
/// [`Webp`]: ImageFormat::Webp
/// [`Gif`]: ImageFormat::Gif
/// [`Apng`]: ImageFormat::Apng
/// [`Tiff`]: ImageFormat::Tiff
/// [`Ico`]: ImageFormat::Ico
/// [`Bmp`]: ImageFormat::Bmp
/// [`Avif`]: ImageFormat::Avif
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
    /// Animated raster, one frame per page, from a 256-color palette per
    /// frame. Timed by [`EncodeOptions::fps`] or
    /// [`EncodeOptions::frame_delays`].
    Gif,
    /// Animated raster, one frame per page, in full color with an alpha
    /// channel. Timed as [`Gif`](ImageFormat::Gif) is.
    Apng,
    /// Raster, one page per directory. Multi-page but not animated: the
    /// pages are pages, and carry no timing.
    Tiff,
    /// An icon, one image per page. Unlike every other format here the
    /// pages may differ in size, which is the point of the container: an
    /// `.ico` holds the same icon at 16, 32, 48 and 256 pixels.
    Ico,
    /// Uncompressed raster, one page. The lowest common denominator, and
    /// the only lossless format some Windows tooling will read.
    Bmp,
    /// Raster, one page, as an AV1 intra frame. The smallest files this
    /// crate writes and by far the slowest to produce. Honours
    /// [`EncodeOptions::quality`].
    Avif,
    /// Vector document. Preserves text and paths as vectors.
    Pdf,
    /// Vector markup. Preserves text and paths as vectors.
    Svg,
    /// Unencoded pixel bytes, in the surface's own layout.
    Raw,
}

impl ImageFormat {
    /// Everything fixed about this format.
    pub(crate) fn traits(self) -> FormatTraits {
        // Every derived answer below reads this match and nothing else, so a
        // new variant is one row rather than a hunt for the sites that would
        // otherwise disagree about it.
        match self {
            Self::Png => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Skia,
                animated: false,
                pages: PageUse::One,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/png",
                name: "png",
                extension: "png",
                aliases: &[],
                inferable: true,
            },
            Self::Jpeg => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Skia,
                animated: false,
                pages: PageUse::One,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/jpeg",
                name: "jpg",
                extension: "jpg",
                aliases: &["jpeg"],
                inferable: true,
            },
            // The one format whose still form Skia encodes and whose
            // animated form this crate muxes. `SkWebpEncoder::EncodeAnimated`
            // exists in C++ and nothing binds it, so `encode::webp` writes the
            // container around the frames Skia encodes one at a time --
            // `encoder` describes the still path, which is the one this field
            // routes.
            Self::Webp => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Skia,
                animated: true,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/webp",
                name: "webp",
                extension: "webp",
                aliases: &[],
                inferable: true,
            },
            Self::Gif => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Foreign,
                animated: true,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::AssumedSrgb,
                mime: "image/gif",
                name: "gif",
                extension: "gif",
                aliases: &[],
                inferable: true,
            },
            Self::Apng => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Foreign,
                animated: true,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::Declared,
                // Registered with IANA by the W3C on 2022-11-21, against
                // the PNG specification that first took the animated form
                // in. A file is usually called `.png`, which this
                // deliberately does not claim: `.png` belongs to the still
                // encoder, and a caller who wants an animation asks for one
                // by name.
                mime: "image/apng",
                name: "apng",
                extension: "apng",
                aliases: &[],
                inferable: true,
            },
            Self::Tiff => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Foreign,
                animated: false,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/tiff",
                name: "tiff",
                extension: "tiff",
                aliases: &["tif"],
                inferable: true,
            },
            Self::Ico => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Foreign,
                animated: false,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::Declared,
                // The type IANA registered. `image/x-icon` is what most of
                // the web sends, and is not a registered type at all.
                mime: "image/vnd.microsoft.icon",
                name: "ico",
                extension: "ico",
                aliases: &[],
                inferable: true,
            },
            Self::Bmp => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Foreign,
                animated: false,
                pages: PageUse::One,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/bmp",
                name: "bmp",
                extension: "bmp",
                aliases: &[],
                inferable: true,
            },
            Self::Avif => FormatTraits {
                depths: avif::BIT_DEPTHS,
                encoder: EncoderKind::Foreign,
                animated: true,
                pages: PageUse::All,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "image/avif",
                name: "avif",
                extension: "avif",
                aliases: &[],
                inferable: true,
            },
            Self::Pdf => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Skia,
                animated: false,
                pages: PageUse::All,
                content: Content::Vector,
                color: ColorSignal::Declared,
                mime: "application/pdf",
                name: "pdf",
                extension: "pdf",
                aliases: &[],
                inferable: true,
            },
            Self::Svg => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Skia,
                animated: false,
                pages: PageUse::One,
                content: Content::Vector,
                color: ColorSignal::Declared,
                mime: "image/svg+xml",
                name: "svg",
                extension: "svg",
                aliases: &[],
                inferable: true,
            },
            Self::Raw => FormatTraits {
                depths: &[],
                encoder: EncoderKind::Unencoded,
                animated: false,
                pages: PageUse::One,
                content: Content::Raster,
                color: ColorSignal::Declared,
                mime: "application/octet-stream",
                // The one format whose name and extension differ:
                // `toBuffer("raw")` asks for it by name, and a file written
                // that way is `.bin`.
                name: "raw",
                extension: "bin",
                aliases: &[],
                inferable: false,
            },
        }
    }

    /// The format listed after this one, or `None` at the end of the list.
    ///
    /// A chain rather than a `const` array because the compiler checks a
    /// chain: this match is exhaustive, so a new variant will not compile
    /// until it has been given a place in the order. An array would have
    /// accepted a variant nobody added to it, and
    /// [`from_extension`](Self::from_extension) -- which walks the list --
    /// would have quietly failed to recognize the new format's own files.
    fn following(self) -> Option<Self> {
        match self {
            Self::Png => Some(Self::Jpeg),
            Self::Jpeg => Some(Self::Webp),
            Self::Webp => Some(Self::Gif),
            Self::Gif => Some(Self::Apng),
            Self::Apng => Some(Self::Tiff),
            Self::Tiff => Some(Self::Ico),
            Self::Ico => Some(Self::Bmp),
            Self::Bmp => Some(Self::Avif),
            Self::Avif => Some(Self::Pdf),
            Self::Pdf => Some(Self::Svg),
            Self::Svg => Some(Self::Raw),
            Self::Raw => None,
        }
    }

    /// Every format, in declaration order.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        std::iter::successors(Some(Self::Png), |format| format.following())
    }

    /// Returns whether the format keeps drawings as vectors.
    ///
    /// Vector formats need recorded content: rasterizing first and encoding
    /// the result would embed a single bitmap rather than preserve the
    /// geometry.
    pub fn is_vector(self) -> bool {
        self.traits().content == Content::Vector
    }

    /// Returns the IANA media type for the format.
    ///
    /// [`Raw`](ImageFormat::Raw) has no registered type and reports
    /// `application/octet-stream`.
    pub fn mime_type(self) -> &'static str {
        self.traits().mime
    }

    /// Returns the conventional file extension, without a leading dot.
    pub fn extension(self) -> &'static str {
        self.traits().extension
    }

    /// Infers a format from a file extension, case-insensitively.
    ///
    /// Accepts the extension with or without a leading dot, and treats
    /// `jpeg` and `jpg` alike. Returns `None` for anything else, so a
    /// caller can decide between a default and an error.
    ///
    /// Deliberately not the inverse of [`ImageFormat::extension`]:
    /// [`Raw`](ImageFormat::Raw) reports `"bin"` but is not inferred from
    /// it, because a file named `.bin` says nothing about its pixel layout
    /// and guessing one would produce an unreadable file.
    pub fn from_extension(extension: &str) -> Option<Self> {
        Self::from_name(extension).filter(|format| format.traits().inferable)
    }

    /// The format answering to `name`, whether or not a filename may carry
    /// it.
    ///
    /// The binding's own token for [`Raw`](ImageFormat::Raw) is `"raw"`,
    /// which no file is ever called, so the JavaScript boundary parses with
    /// this and [`from_extension`](Self::from_extension) stays honest about
    /// what a filename can say.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        let wanted = name.trim_start_matches('.').to_ascii_lowercase();
        Self::all().find(|format| {
            let traits = format.traits();
            traits.name == wanted
                || traits.extension == wanted
                || traits.aliases.contains(&wanted.as_str())
        })
    }

    /// The name this format is reported by, for messages that have to say
    /// which one was asked for.
    pub(crate) fn as_str(self) -> &'static str {
        self.traits().name
    }

    /// Whether one file of this format carries every page.
    pub(crate) fn spans_pages(self) -> bool {
        self.traits().pages == PageUse::All
    }

    /// Whether this format's pages are frames with durations.
    pub(crate) fn is_animated(self) -> bool {
        self.traits().animated
    }

    /// The depths a caller may write a file of this format at, which is
    /// empty for every format whose depth follows the canvas instead.
    pub(crate) fn bit_depths(self) -> &'static [u8] {
        self.traits().depths
    }

    /// Whether a file of this format can say which color space it holds.
    ///
    /// When it cannot, an export narrows to sRGB rather than writing
    /// wide-gamut pixels under a silence every reader takes as sRGB.
    pub(crate) fn declares_color(self) -> bool {
        self.traits().color == ColorSignal::Declared
    }

    /// Every name a caller may pass, for a message listing what was
    /// expected.
    pub(crate) fn names() -> Vec<&'static str> {
        Self::all()
            .flat_map(|format| {
                let traits = format.traits();
                std::iter::once(traits.name)
                    .chain(traits.aliases.iter().copied())
            })
            .collect()
    }

    /// Every name a *filename* may carry, which leaves out
    /// [`Raw`](ImageFormat::Raw).
    pub(crate) fn inferable_names() -> Vec<&'static str> {
        Self::all()
            .filter(|format| format.traits().inferable)
            .flat_map(|format| {
                let traits = format.traits();
                std::iter::once(traits.extension)
                    .chain(traits.aliases.iter().copied())
            })
            .collect()
    }
}

/// How many chroma samples an encoder writes per pixel.
///
/// Luma is always kept in full; the saving comes from storing colour more
/// coarsely than brightness, which the eye notices far less. How much less
/// depends entirely on the picture -- see
/// [`EncodeOptions::chroma`] for what each costs on measured content.
///
/// Named for the fraction of the chroma kept rather than in the `4:2:0`
/// notation, which is a sampling ratio whose three numbers do not mean what
/// they appear to and cannot begin a Rust identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChromaSampling {
    /// A chroma sample per pixel, written `4:4:4`.
    ///
    /// The default, and the right one for text, line art and flat UI.
    #[default]
    Full,
    /// Chroma at half width and full height, written `4:2:2`.
    ///
    /// The middle, and rarely the best of the three: on flat UI it measured
    /// indistinguishable from [`Full`](Self::Full) while saving nothing, and
    /// on photographs [`Quarter`](Self::Quarter) was both smaller and no
    /// worse. Here because the format offers it.
    Half,
    /// Chroma at half width and half height, written `4:2:0`.
    ///
    /// What most encoders write by default and what photographs want: 30%
    /// smaller for 7 dB on a photograph. Ruinous on saturated edges, where
    /// it measured 22 dB worse and *larger*.
    Quarter,
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
    /// cost of no longer being selectable or searchable. Defaults to
    /// `false`, so a PDF or SVG carries real text.
    ///
    /// It defaulted to `true` here while the JavaScript binding defaulted it
    /// to `false`, so the same `to_file("card.svg")` produced live `<text>`
    /// from Node and a wall of `<path>` from Rust -- 73 KB against 205 KB on
    /// the example the two surfaces share.
    pub outline: bool,
    /// Pixel format the export is handed back in, or `None` for the
    /// canvas's own.
    ///
    /// This is the dial that makes `to_buffer(Raw, ..)` hand back `F16` or
    /// `F32` pixels from a canvas built at either, and it is also what the
    /// encoded formats read their own depth from: naming a float type writes
    /// a sixteen-bit PNG, APNG or TIFF, and naming
    /// [`Uint8`](PixelDepth::Uint8) on a float canvas writes eight. AVIF is
    /// the exception, and has [`bit_depth`](Self::bit_depth) of its own
    /// because ten and twelve are depths no readback format names.
    ///
    /// The JavaScript binding has taken a per-export `colorType` since
    /// before this crate had a Rust API, and this side had no field for it
    /// at all: a Rust caller could only choose at construction, through
    /// [`CanvasOptions`](crate::canvas::CanvasOptions). Compositing still
    /// follows the canvas rather than the call -- see
    /// `ExportOptions::surface_color_type` for why a readback format has no
    /// business choosing the precision a page is drawn at.
    pub color_type: Option<PixelDepth>,
    /// Bits a channel an [`Avif`](ImageFormat::Avif) codes its pixels at,
    /// or `None` to follow the canvas.
    ///
    /// AV1 codes 8, 10 and 12, and AVIF carries all three. Unasked, an
    /// eight-bit canvas is written at ten and a float one at twelve -- ten
    /// because AV1's transforms work above the input depth anyway and the
    /// headroom keeps quantisation from banding a gradient eight bits would
    /// step through, twelve because a canvas built in float has the range to
    /// fill it.
    ///
    /// The reason to name one is reach. Eight and ten at 4:4:4 are AV1's
    /// High profile; twelve is Professional, which fewer decoders implement.
    /// So a float canvas whose AVIF has to open anywhere asks for 10, and a
    /// caller who wants the smallest file a shallow drawing can make asks
    /// for 8 -- which is also the one depth that reaches the encoder as the
    /// bytes the canvas holds, with no widening in between.
    ///
    /// Refused for every other format rather than ignored: their depths are
    /// the ones [`color_type`](Self::color_type) already names.
    pub bit_depth: Option<u8>,
    /// How an [`Avif`](ImageFormat::Avif) samples chroma, or `None` for
    /// [`Full`](ChromaSampling::Full).
    ///
    /// The default is full chroma, which is the opposite of what most AVIF
    /// encoders choose, and deliberate: this library draws canvases. On text
    /// and flat UI, halving chroma in both axes measured 22 dB worse -- 50.07
    /// against 27.96 -- while making the file *larger*, because the artefacts
    /// it introduces cost bits of their own. Saturated colour against a light
    /// ground is precisely what it destroys.
    ///
    /// On photographs the trade is the usual one and worth taking: the same
    /// measurement put [`Quarter`](ChromaSampling::Quarter) 30% smaller for
    /// 7 dB. So a canvas exporting a photograph should ask for it, and one
    /// exporting a chart or a card should not.
    ///
    /// Refused for every other format rather than ignored. JPEG has a
    /// subsampling switch of its own in
    /// [`jpeg_downsample`](Self::jpeg_downsample), which predates this and is
    /// a plain boolean because JPEG offers the one alternative.
    pub chroma: Option<ChromaSampling>,
    /// Whether an [`Avif`](ImageFormat::Avif) is coded with no loss at all.
    ///
    /// Defaults to `false`, and deliberately: AVIF is reached for because it
    /// is small, and a lossless one is several times the size of a lossy one
    /// and often larger than the PNG it would replace. Every encoder in the
    /// ecosystem defaults to lossy for the same reason.
    ///
    /// This is lossless in *red, green and blue*, not merely in what the
    /// encoder was handed. Getting there needs two things beyond the flag,
    /// both of which this sets: full chroma, and the identity matrix, where
    /// the three coded planes are green, blue and red rather than a luma and
    /// two colour differences. Without the second the picture is rounded by
    /// the conversion before quantisation ever runs, and the file faithfully
    /// preserves data that was already lossy.
    ///
    /// Because of that, naming a [`chroma`](Self::chroma) other than
    /// [`Full`](ChromaSampling::Full) alongside this is refused rather than
    /// silently overridden: subsampled identity planes would be discarding
    /// literal red and blue samples.
    ///
    /// [`quality`](Self::quality) is ignored when this is set. It is not
    /// promoted at `1.0` either -- that means the finest quantizer, which is
    /// near-lossless but still filtered, and changing what it meant would
    /// change every file this crate has already written.
    pub lossless: bool,
    /// Color space the export is converted into.
    ///
    /// `None` -- the default -- exports in the canvas's own space, which is
    /// what the JavaScript side does: a Display P3 canvas hands back Display
    /// P3 pixels and a PNG carrying that profile. Naming a space converts
    /// into it on the way out.
    pub color_space: Option<PixelColorSpace>,
    /// Whether the JPEG encoder subsamples chroma. Defaults to `false`,
    /// which keeps full chroma resolution at a larger file size.
    pub jpeg_downsample: bool,
    /// Multisample count for the rasterizing pass, or `None` for the
    /// backend's default.
    pub msaa: Option<usize>,
    /// Which page a raster export encodes, `0` being the first added.
    ///
    /// `None` encodes the current page, which is what the Canvas API does.
    ///
    /// Naming one wins over the format spanning pages. The formats that
    /// gather every page -- [`Pdf`](ImageFormat::Pdf),
    /// [`Gif`](ImageFormat::Gif), [`Apng`](ImageFormat::Apng),
    /// [`Tiff`](ImageFormat::Tiff), [`Ico`](ImageFormat::Ico) -- do so only
    /// when no page is named; asked for one, they encode that page alone.
    /// So a `Gif` with `page: Some(0)` is a single frame, not an animation.
    ///
    /// An index past the end is an [`Error::Encode`] whatever the format,
    /// rather than a no-op.
    ///
    /// The JavaScript binding draws the line in the same place, and this
    /// used to claim so while doing the opposite: the spanning branch was
    /// taken before `page` was ever read, so naming one on a `Gif` was
    /// silently ignored and so was an index past the end.
    pub page: Option<usize>,
    /// Which pages a format that gathers them encodes, `0` being the first
    /// added and the end excluded, as a Rust range is.
    ///
    /// `None` -- the default -- takes every page. `Some(0..20)` takes the
    /// first twenty, which is how an intro is separated from the loop that
    /// follows it: two calls over one canvas, each with its own
    /// [`loops`](Self::loops), rather than one file that has to compromise
    /// between them. It serves the paged documents as well -- one chapter
    /// out of a long PDF, or a preview of the first second of an animation.
    ///
    /// The JavaScript binding spells this `pageRange`, numbered from one and
    /// inclusive at both ends, for the same reason its `page` is one-based
    /// while [`page`](Self::page) here is not: each side counts the way its
    /// own language does.
    ///
    /// Naming this and [`page`](Self::page) together is an
    /// [`Error::InvalidExportOption`], since they answer the same question
    /// differently. So is an empty range, a range reaching past the last
    /// page, and a range handed to a format that encodes one page and has
    /// nothing to gather.
    ///
    /// The pages are sliced before the encoder is built rather than skipped
    /// as it runs, which is what the animations need: WebP codes each frame
    /// as the rectangle it differs from its predecessor in, so a range whose
    /// first page still had a predecessor would open with a rectangle diffed
    /// against a page the file does not contain.
    pub page_range: Option<Range<usize>>,
    /// Frames per second for an animated format.
    ///
    /// One page is one frame, so this is the rate the pages play at.
    /// `None` -- the default -- uses 30. [`ImageFormat::Gif`] stores
    /// hundredths of a second, so its frame times round to the nearest
    /// 10ms.
    ///
    /// An `Option` rather than a plain rate so that "not asked for" is a
    /// state the crate can see. Naming one for a format with no clock, such
    /// as [`Png`](ImageFormat::Png) or [`Tiff`](ImageFormat::Tiff), is an
    /// [`Error::Encode`] rather than something quietly dropped.
    pub fps: Option<f32>,
    /// Per-frame durations in milliseconds, overriding [`fps`](Self::fps).
    ///
    /// Used only when it has one entry per page, which is what makes
    /// re-encoding an animation possible: the delays an [`Image`] reports
    /// can be handed straight back.
    ///
    /// [`Image`]: crate::image::Image
    pub frame_delays: Vec<u32>,
    /// How many times an animation plays. `None` -- the default -- plays it
    /// forever.
    ///
    /// `Some(1)` plays it once and stops, which
    /// [`Apng`](ImageFormat::Apng) states outright and
    /// [`Gif`](ImageFormat::Gif) cannot. GIF keeps its loop count in a block
    /// whose zero means "forever", so no number means "once" and the
    /// convention is to leave the block out. A GIF written that way declares
    /// nothing, and a decoder may answer either way depending on when it is
    /// asked -- Skia's says forever before it has decoded a frame and once
    /// afterwards. Every other count is stated plainly by both.
    pub loops: Option<u32>,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 0.92,
            density: 1.0,
            matte: None,
            outline: false,
            color_type: None,
            bit_depth: None,
            chroma: None,
            lossless: false,
            color_space: None,
            jpeg_downsample: false,
            msaa: None,
            page: None,
            page_range: None,
            fps: None,
            frame_delays: Vec::new(),
            loops: None,
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
    /// Refuses a value this crate would otherwise quietly substitute for.
    ///
    /// Every check here is one the JavaScript binding has always made. This
    /// side made none of them: a `quality` outside `0..=1` was clamped, a
    /// negative or `NaN` `fps` fell back to 30, a `frame_delays` of the
    /// wrong length was ignored, and a `density` of zero reached Skia and
    /// came back as `Could not allocate new 0x0 bitmap`. So the same call
    /// through the same crate behaved differently depending on which surface
    /// made it, and only one of the two told the caller anything.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidExportOption`] naming the field at fault.
    fn validate(&self, format: ImageFormat, pages: usize) -> Result<(), Error> {
        let refuse = |option: &'static str, reason: String| {
            Err(Error::InvalidExportOption { option, reason })
        };

        if let Some(bits) = self.bit_depth {
            let taken = format.bit_depths();
            if taken.is_empty() {
                return refuse(
                    "bit_depth",
                    format!(
                        "{} takes its depth from the canvas -- name a \
                         `color_type` instead",
                        format.as_str()
                    ),
                );
            }
            if !taken.contains(&bits) {
                return refuse(
                    "bit_depth",
                    format!(
                        "{} writes {} bits a channel, got {bits}",
                        format.as_str(),
                        listed(taken)
                    ),
                );
            }
        }

        // AVIF is the only format here that offers the choice. JPEG
        // subsamples too, through `jpeg_downsample`, and pointing at it is
        // more use than saying no.
        if self.chroma.is_some() && format != ImageFormat::Avif {
            return refuse(
                "chroma",
                match format {
                    ImageFormat::Jpeg => {
                        "jpeg subsamples through `jpeg_downsample`".to_string()
                    }
                    _ => format!(
                        "{} does not choose its chroma sampling",
                        format.as_str()
                    ),
                },
            );
        }

        if self.lossless {
            if format != ImageFormat::Avif {
                return refuse(
                    "lossless",
                    format!(
                        "{} is either lossless already or has no lossless \
                         form",
                        format.as_str()
                    ),
                );
            }
            // Refused rather than overridden: a caller who asked for both
            // wants something the format cannot give, and quietly picking one
            // would hand them a file that is not what either option promised.
            if matches!(
                self.chroma,
                Some(ChromaSampling::Half | ChromaSampling::Quarter)
            ) {
                return refuse(
                    "lossless",
                    "subsampled chroma discards colour before the encoder \
                     sees it, so it cannot be lossless"
                        .to_string(),
                );
            }
        }

        if !self.quality.is_finite() || !(0.0..=1.0).contains(&self.quality) {
            return refuse(
                "quality",
                format!(
                    "expected a number from 0.0 to 1.0, got {}",
                    self.quality
                ),
            );
        }
        if !self.density.is_finite() || self.density <= 0.0 {
            return refuse(
                "density",
                format!("expected a positive number, got {}", self.density),
            );
        }
        if let Some(fps) = self.fps
            && (!fps.is_finite() || fps <= 0.0)
        {
            return refuse(
                "fps",
                format!("expected a positive number, got {fps}"),
            );
        }
        // A list of the wrong length is a caller who miscounted, not one
        // asking for the default: silently falling back to `fps` would
        // retime the animation without saying so.
        if !self.frame_delays.is_empty() && self.frame_delays.len() != pages {
            return refuse(
                "frame_delays",
                format!(
                    "expected one entry per page, got {} for {pages}",
                    self.frame_delays.len()
                ),
            );
        }
        Ok(())
    }

    /// The pages this call encodes, as an index range into the canvas.
    ///
    /// Every [`page_range`](Self::page_range) check lives here rather than
    /// alongside the others in [`validate`](Self::validate), because that one
    /// is handed the number of frames the call will write and this is what
    /// decides that number.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidExportOption`] naming `page_range`.
    pub(crate) fn resolved_pages(
        &self,
        format: ImageFormat,
        total: usize,
    ) -> Result<Range<usize>, Error> {
        let Some(range) = self.page_range.clone() else {
            return Ok(0..total);
        };
        let refuse = |reason: String| {
            Err(Error::InvalidExportOption {
                option: "page_range",
                reason,
            })
        };

        if self.page.is_some() {
            return refuse(
                "`page` names one page and `page_range` names several; \
                 expected one or the other"
                    .to_string(),
            );
        }
        if !format.spans_pages() {
            return refuse(format!(
                "{} encodes one page and has nothing to gather -- name a \
                 `page` instead",
                format.as_str()
            ));
        }
        if range.is_empty() {
            return refuse(format!(
                "expected at least one page, got {}..{}",
                range.start, range.end
            ));
        }
        if range.end > total {
            return refuse(format!(
                "{}..{} reaches past the last page; the canvas has {total}",
                range.start, range.end
            ));
        }
        Ok(range)
    }

    /// # Errors
    ///
    /// Returns [`Error::InvalidExportOption`] for a value this crate will
    /// not act on -- see [`validate`](Self::validate) -- and [`Error`] when
    /// [`color_space`](Self::color_space) cannot be realized by this build.
    pub(crate) fn to_internal(
        &self,
        format: ImageFormat,
        canvas_space: PixelColorSpace,
        pages: usize,
    ) -> Result<ExportOptions, Error> {
        self.validate(format, pages)?;
        Ok(ExportOptions {
            format,
            quality: self.quality,
            bit_depth: self.bit_depth,
            chroma: self.chroma,
            lossless: self.lossless,
            density: self.density,
            outline: self.outline,
            matte: self.matte.map(rgba_linear_to_skia_color),
            msaa: self.msaa,
            // The space to convert *into*. Unasked, that is the canvas's
            // own, so a wide-gamut canvas exports wide rather than being
            // quietly narrowed to sRGB -- the JavaScript side has always
            // behaved this way. `surface_color_space`, set by the caller, is
            // the space being converted *out of*.
            color_space: self
                .color_space
                .unwrap_or(canvas_space)
                .to_skia_color_space()?,
            jpeg_downsample: self.jpeg_downsample,
            fps: self.fps,
            frame_delays: self.frame_delays.clone(),
            loops: self.loops,
            ..ExportOptions::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_depth_a_format_cannot_code_is_refused_rather_than_rounded() {
        let asking = |format, bits| {
            EncodeOptions {
                bit_depth: Some(bits),
                ..EncodeOptions::default()
            }
            .validate(format, 1)
        };

        for bits in avif::BIT_DEPTHS.iter().copied() {
            assert!(asking(ImageFormat::Avif, bits).is_ok(), "avif at {bits}");
        }
        // Nine is between two depths AV1 codes and is not one of them.
        // Sixteen is a depth PNG codes, which is what makes it the mistake
        // worth naming: it is a real number in the wrong place.
        for bits in [1, 9, 16, 24] {
            let Err(Error::InvalidExportOption { option, reason }) =
                asking(ImageFormat::Avif, bits)
            else {
                panic!("avif should refuse {bits} bits");
            };
            assert_eq!(option, "bit_depth");
            assert!(reason.contains("8, 10 or 12"), "{reason}");
        }

        // Every other format takes its depth from the canvas, and being
        // handed one here is a caller who will otherwise wonder why the
        // file came out at the depth it did.
        let Err(Error::InvalidExportOption { reason, .. }) =
            asking(ImageFormat::Png, 16)
        else {
            panic!("png should refuse a bit depth");
        };
        assert!(reason.contains("color_type"), "{reason}");
    }

    #[test]
    fn a_density_of_one_is_the_conventional_seventy_two_dpi() {
        // Asserted as the values themselves, not by converting back and
        // rounding. That is how a one-unit slip got in: `(72.0 * 39.3701)
        // as i32` truncates 2834.6472 to 2834 where the conventional value
        // is 2835, and a test that divides either by 39.3701 and rounds
        // gets 72 both ways. So the round trip could not see it.
        assert_eq!(dots_per_inch(1.0), 72);
        assert_eq!(pixels_per_metre(1.0), 2835);
    }

    #[test]
    fn a_density_scales_the_resolution_it_declares() {
        // Both halves of the reason this exists. A fractional density was
        // truncated to a whole number by the JPEG path before it multiplied
        // anything, and BMP ignored density altogether and wrote 72 DPI at
        // every scale.
        for (density, dpi) in [
            (0.5f32, 36u16),
            (1.0, 72),
            (1.5, 108),
            (2.0, 144),
            (3.0, 216),
        ] {
            assert_eq!(dots_per_inch(density), dpi, "at density {density}");
            // The per-metre form is the same resolution in other units, so
            // the two cannot disagree about what a density means.
            let per_metre = f64::from(pixels_per_metre(density));
            assert!(
                (per_metre / INCHES_PER_METRE - f64::from(dpi)).abs() < 0.5,
                "at density {density}: {per_metre} per metre is not {dpi} dpi"
            );
        }
    }

    #[test]
    fn a_density_of_zero_declares_no_resolution_rather_than_wrapping() {
        // JFIF reads a density of zero as "no units", which is a legal
        // thing for a file to say. What matters is that it is reached by
        // asking for zero rather than by a negative or enormous density
        // wrapping into it.
        assert_eq!(dots_per_inch(0.0), 0);
        assert_eq!(dots_per_inch(-1.0), 0, "clamped, not wrapped");
        assert_eq!(pixels_per_metre(-1.0), 0);
        assert_eq!(dots_per_inch(f32::MAX), u16::MAX, "saturated, not wrapped");
        assert_eq!(pixels_per_metre(f32::MAX), u32::MAX);
    }

    #[test]
    fn the_table_answers_what_the_scattered_matches_used_to() {
        // The values every derived method reads. Pinned against what the
        // per-format matches returned before the table replaced them, so a
        // row edited by mistake is caught rather than silently shipped.
        for (format, mime, extension) in [
            (ImageFormat::Png, "image/png", "png"),
            (ImageFormat::Jpeg, "image/jpeg", "jpg"),
            (ImageFormat::Webp, "image/webp", "webp"),
            (ImageFormat::Tiff, "image/tiff", "tiff"),
            (ImageFormat::Ico, "image/vnd.microsoft.icon", "ico"),
            (ImageFormat::Bmp, "image/bmp", "bmp"),
            (ImageFormat::Avif, "image/avif", "avif"),
            (ImageFormat::Gif, "image/gif", "gif"),
            // `.png` belongs to the still encoder; a caller who wants an
            // animation asks for one by name.
            (ImageFormat::Apng, "image/apng", "apng"),
            (ImageFormat::Pdf, "application/pdf", "pdf"),
            (ImageFormat::Svg, "image/svg+xml", "svg"),
            (ImageFormat::Raw, "application/octet-stream", "bin"),
        ] {
            assert_eq!(format.mime_type(), mime);
            assert_eq!(format.extension(), extension);
        }

        assert!(ImageFormat::Pdf.is_vector() && ImageFormat::Svg.is_vector());
        assert!(!ImageFormat::Png.is_vector() && !ImageFormat::Raw.is_vector());
    }

    #[test]
    fn spanning_pages_and_holding_pixels_are_separate_questions() {
        // The pair that shows it, and the reason the axes had to be split
        // before either animated format could exist: GIF is raster and
        // gathers every page, SVG is vector and does not. Reading one from
        // the other gets both of them wrong.
        assert!(
            ImageFormat::Gif.spans_pages() && !ImageFormat::Gif.is_vector()
        );
        assert!(
            !ImageFormat::Svg.spans_pages() && ImageFormat::Svg.is_vector()
        );

        let spanning: Vec<_> =
            ImageFormat::all().filter(|f| f.spans_pages()).collect();
        assert_eq!(
            spanning,
            vec![
                ImageFormat::Webp,
                ImageFormat::Gif,
                ImageFormat::Apng,
                ImageFormat::Tiff,
                ImageFormat::Ico,
                // AVIF joined them when it learned to animate: its pages
                // become samples of one coded sequence.
                ImageFormat::Avif,
                ImageFormat::Pdf
            ]
        );

        // WebP is the odd one: Skia encodes a still, this crate muxes the
        // animation, so the two halves of the format take different paths
        // out of `encoded_as`.
        assert_eq!(
            ImageFormat::Webp.traits().encoder,
            EncoderKind::Skia,
            "a one-page WebP is still Skia's to write"
        );
        assert!(ImageFormat::Webp.traits().animated);
    }

    #[test]
    fn a_format_skia_cannot_encode_is_routed_away_from_skia() {
        // The list is not an opinion: skia-safe 0.99 exposes exactly the
        // JPEG, PNG and WebP encoders, plus the PDF and SVG documents.
        for format in ImageFormat::all() {
            let expected = match format {
                ImageFormat::Gif
                | ImageFormat::Apng
                | ImageFormat::Tiff
                | ImageFormat::Ico
                | ImageFormat::Bmp
                | ImageFormat::Avif => EncoderKind::Foreign,
                ImageFormat::Raw => EncoderKind::Unencoded,
                _ => EncoderKind::Skia,
            };
            assert_eq!(format.traits().encoder, expected, "{format:?}");
        }
    }

    #[test]
    fn every_format_is_reachable_from_the_chain() {
        // `following` is exhaustive, so this catches a variant wired into the
        // chain twice or pointing back into it, which the compiler cannot.
        let all: Vec<_> = ImageFormat::all().collect();
        assert_eq!(all.len(), 12, "{all:?}");
        for format in &all {
            assert_eq!(all.iter().filter(|f| *f == format).count(), 1);
        }
    }

    #[test]
    fn an_extension_round_trips_through_the_name_it_is_inferred_from() {
        for format in ImageFormat::all().filter(|f| f.traits().inferable) {
            assert_eq!(
                ImageFormat::from_extension(format.extension()),
                Some(format)
            );
            // Case and a leading dot are both accepted.
            assert_eq!(
                ImageFormat::from_extension(&format!(
                    ".{}",
                    format.extension().to_uppercase()
                )),
                Some(format)
            );
        }

        assert_eq!(
            ImageFormat::from_extension("jpeg"),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::from_extension("tif"),
            Some(ImageFormat::Tiff),
            "the alias, as `jpeg` is for `jpg`"
        );
        assert_eq!(ImageFormat::from_extension("targa"), None);
    }

    #[test]
    fn raw_answers_to_its_own_name_but_never_to_a_filename() {
        // `toBuffer("raw")` names it; a file called `.bin` says nothing about
        // its pixel layout, so `saveAs` must not guess it.
        assert_eq!(ImageFormat::from_name("raw"), Some(ImageFormat::Raw));
        assert_eq!(ImageFormat::from_extension("raw"), None);
        assert_eq!(ImageFormat::from_extension("bin"), None);
        assert!(!ImageFormat::inferable_names().contains(&"raw"));
        assert!(ImageFormat::names().contains(&"raw"));
    }
}
