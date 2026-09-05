use std::sync::Mutex;

use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorType, Data, FontMgr, Image as SkImage,
    ImageInfo, Size as SkSize,
    codec::{self, Codec},
    images, surfaces,
    svg::{self, Length, LengthUnit},
};

use crate::{
    error::Error,
    geometry::Size,
    pixels::{PixelColorSpace, PixelFormat},
};

/// The height an SVG with no declared size of its own is rasterized at.
///
/// Chrome's replaced-element default: an `<svg>` whose `width` and `height`
/// both resolve to `100%` is laid out 150 CSS pixels tall, with the width
/// following from the `viewBox` aspect ratio. Matching it is what makes an
/// undimensioned SVG land where a browser puts it.
const DEFAULT_SVG_HEIGHT: f32 = 150.0;

/// Centimetres in one inch. The international inch, defined as exactly this
/// since 1959, and the number CSS itself derives `cm` from.
const CM_PER_INCH: f32 = 2.54;

/// CSS pixels in one inch.
///
/// CSS Values and Units 3, section 5.2, pins the absolute units to each other
/// and to the pixel: `1in` is 96 `px` exactly, whatever the output device
/// resolves to. SVG 2 defers to that definition.
///
/// Skia does not. It converts against SVG 1.1's 90, so every absolute length
/// it resolves comes back 6.25% short of what a browser lays out, which is
/// why the lengths here are converted rather than taken from
/// `SvgSvg::intrinsic_size`.
const PX_PER_INCH: f32 = 96.0;

/// CSS pixels in one centimetre.
const PX_PER_CM: f32 = PX_PER_INCH / CM_PER_INCH;

/// CSS pixels in one millimetre.
const PX_PER_MM: f32 = PX_PER_CM / 10.0;

/// CSS pixels in one point.
///
/// A CSS point is 1/72 inch. Skia divides by 72.272 instead --
/// `kPTMultiplier` in `SkSVGRenderContext.cpp`, with no comment saying where
/// the number is from -- which is where the extra 0.376% of its `pt` and `pc`
/// error comes from on top of the 90-versus-96 one.
const PX_PER_POINT: f32 = PX_PER_INCH / 72.0;

/// CSS pixels in one pica, which is twelve points.
const PX_PER_PICA: f32 = PX_PER_POINT * 12.0;

/// CSS pixels in one `em`, for a document that does not state a font size.
///
/// The initial value of `font-size` in CSS, and what every browser starts a
/// document at. It is a fallback rather than the only answer available: a
/// root stating `font-size="20"` answers for its own lengths, and
/// [`root_px_per_em`] reads it. This is what is left when the document states
/// no font size, or states one that is itself relative -- `2em`, `150%`, the
/// keyword `larger` -- because those need the parent element that a document
/// being measured before it is placed does not have.
///
/// Exact for a document dropped into an unrestyled page, and proportionally
/// wrong anywhere else; the assumption is stated on [`Svg::intrinsic_size`]
/// where a caller reads it rather than left in the arithmetic.
const PX_PER_EM: f32 = 16.0;

/// One `ex` as a fraction of an `em`.
///
/// CSS Values and Units 3, section 5.1.1: "In the cases where it is impossible
/// or impractical to determine the x-height, a value of 0.5em must be
/// assumed." Nothing here loads the font, so that is the case, and the modal
/// verb is the specification's own.
const EX_PER_EM: f32 = 0.5;

/// An immutable decoded raster image.
///
/// Cloning is cheap: Skia images are reference-counted and the pixels are
/// shared, not copied.
///
/// An image decoded from an animated file -- GIF, WebP, APNG or AVIF --
/// carries every frame. Drawing it draws the first one, and [`Image::frame`]
/// hands back any of the others.
pub struct Image {
    pub(crate) inner: SkImage,
    /// How long each frame is shown, in milliseconds, one entry per frame.
    ///
    /// A still image has one entry, of zero: it is shown until something
    /// else is drawn, which is not a duration.
    delays: Vec<u32>,
    /// The bytes this was decoded from, kept only while there is more than
    /// one frame in them.
    ///
    /// Frames are decoded on demand rather than up front, because a caller
    /// drawing a spinner needs one frame per output frame and holding all
    /// of them decoded would cost the whole animation's pixels for the
    /// lifetime of the image. The encoded bytes are what a still image
    /// would have thrown away, and are far smaller.
    encoded: Option<Data>,
    /// A decoder held part-way through an animation.
    ///
    /// Reaching frame `n` of a coded sequence means decoding every sample up
    /// to it, because each is stored as a difference from the ones before.
    /// Starting over on every request makes playing an animation quadratic:
    /// the documented loop -- one frame per output frame -- cost 11 325
    /// sample decodes for a 150-frame file where 150 would do.
    ///
    /// Behind a `Mutex` because [`Image::frame`] takes `&self`, which is the
    /// signature a caller drawing a spinner wants. Cloning an image leaves
    /// the clone without one: two images sharing a decoder would each move
    /// it, and rebuilding is only ever slower rather than wrong.
    playback: Mutex<Option<crate::decode::Playback>>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The held decoder is deliberately absent: it is a position in a
        // file rather than anything a reader of this would want.
        f.debug_struct("Image")
            .field("inner", &self.inner)
            .field("delays", &self.delays)
            .field("encoded", &self.encoded)
            .finish()
    }
}

impl Clone for Image {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            delays: self.delays.clone(),
            encoded: self.encoded.clone(),
            playback: Mutex::new(None),
        }
    }
}

/// The first bytes of a GIF, which both versions of the format share.
///
/// `GIF87a` and `GIF89a`, from the header block in the GIF89a
/// specification -- four bytes is all that is needed to tell a GIF from
/// anything else.
const GIF_MAGIC: &[u8] = b"GIF8";

/// The RIFF container's leading tag, from the RIFF specification.
const RIFF_MAGIC: &[u8] = b"RIFF";

/// The form type that says a RIFF file is a WebP, and where it sits.
///
/// A RIFF header is the tag, a four-byte size, then the form type, so the
/// form starts at byte eight. See the WebP container specification.
const RIFF_FORM_AT: usize = RIFF_MAGIC.len() + size_of::<u32>();
const WEBP_FORM: &[u8] = b"WEBP";

/// Whether Skia's codec could report more than one frame for these bytes.
///
/// GIF and WebP are the two it animates. APNG and AVIF animate as well, and
/// are answered before this by [`frame_delays`] itself -- Skia opens an APNG
/// as the still image its `IDAT` holds and opens no AVIF at all, so neither
/// reaches a codec here.
///
/// A false answer for something that can animate would report a still image
/// for it, so this errs toward opening the codec: it names the containers
/// rather than the encodings, and a RIFF file that is not a WebP simply
/// costs what it used to.
fn may_animate(bytes: &[u8]) -> bool {
    if bytes.starts_with(GIF_MAGIC) {
        return true;
    }
    bytes.starts_with(RIFF_MAGIC)
        && bytes.len() >= RIFF_FORM_AT + WEBP_FORM.len()
        && &bytes[RIFF_FORM_AT..RIFF_FORM_AT + WEBP_FORM.len()] == WEBP_FORM
}

/// The frame timings in `data`, one entry per frame, in milliseconds.
///
/// Returns a single zero delay for anything that is not animated, including
/// data the codec declines: the caller has already accepted the image by
/// then, and one with no frame list is a still one.
///
/// Shared with the Node binding, which keeps its own image type and would
/// otherwise have to agree with this one about GIF timing by hand.
pub(crate) fn frame_delays(data: &Data) -> Vec<u32> {
    // APNG first, because Skia opens one as the still image its `IDAT` holds
    // and reports a single frame -- so asking it would answer `[0]` for an
    // animation this crate itself wrote.
    if let Some(delays) = crate::decode::apng::delays(data.as_bytes()) {
        return delays;
    }
    // As APNG: Skia opens no AVIF at all, so it would answer for neither
    // the animated form nor the still one.
    if let Some(delays) = crate::decode::avif::delays(data.as_bytes()) {
        return delays;
    }
    // Everything still ends here, and a codec is expensive to open: it is a
    // second parse of bytes this crate has just decoded once, and it was
    // being paid by every JPEG and every plain PNG to be told they hold one
    // frame. Only two containers can answer otherwise, so only two are
    // asked.
    if !may_animate(data.as_bytes()) {
        return vec![0];
    }
    let Some(mut codec) = Codec::from_data(data.clone()) else {
        return vec![0];
    };
    let count = codec.get_frame_count();
    if count < 2 {
        return vec![0];
    }
    (0..count)
        .map(|index| {
            codec
                .get_frame_info(index)
                .map(|info| info.duration.max(0) as u32)
                .unwrap_or(0)
        })
        .collect()
}

/// Decodes one frame of `data`, composited against the frames before it.
///
/// A fresh codec each time, which is what makes any order work: Skia decodes
/// whatever earlier frames this one is built on, so frame 5 is reachable
/// without having asked for frames 0 through 4.
pub(crate) fn decode_frame(
    data: &Data,
    index: usize,
    resume: Option<&mut Option<crate::decode::Playback>>,
) -> Result<SkImage, Error> {
    // As in `frame_delays`: Skia would hand back the still `IDAT` for every
    // index, so every frame of an APNG would draw as the first.
    if crate::decode::apng::is_animated(data.as_bytes()) {
        return crate::decode::apng::frame(data, index, resume)
            .map_err(|reason| Error::DecodeImage { reason });
    }
    if crate::decode::avif::is_avif(data.as_bytes()) {
        let bytes = data.as_bytes();
        let decoded = match crate::decode::avif::is_animated(bytes) {
            true => crate::decode::avif::frame(bytes, index, resume),
            false => crate::decode::avif::still(bytes),
        };
        return decoded.map_err(|reason| Error::DecodeImage { reason });
    }
    let mut codec =
        Codec::from_data(data.clone()).ok_or_else(|| Error::DecodeImage {
            reason: "skia could not reopen the image to reach its frames"
                .to_string(),
        })?;
    let info = codec.info();
    let options = codec::Options {
        frame_index: index,
        ..codec::Options::default()
    };
    codec
        .get_image(info, Some(&options))
        .map_err(|result| Error::DecodeImage {
            reason: format!("skia could not decode frame {index}: {result:?}"),
        })
}

impl Image {
    /// Wraps a Skia image that came from somewhere with no frames to speak
    /// of: a pixel buffer, a rasterized SVG, or one frame of an animation.
    pub(crate) fn still(image: SkImage) -> Self {
        Self {
            playback: Mutex::new(None),
            inner: image,
            delays: vec![0],
            encoded: None,
        }
    }

    /// Decodes an encoded image (PNG, JPEG, WebP, etc.) into a `Image`.
    ///
    /// For raw decoded video frames or pixel buffers you already hold, prefer
    /// [`Image::from_pixels`] -- it skips the encode/decode round trip.
    ///
    /// Decoding is deferred: Skia validates the header here and decodes the
    /// pixels on first draw, so a header-valid but corrupt file returns
    /// `Ok` and fails later as a blank draw.
    ///
    /// Frame timings are not deferred. Reading them opens a second codec
    /// over the same bytes, so this pays for two header parses rather than
    /// one -- deliberately, and once: [`Image::frame_count`] and
    /// [`Image::frame_delays`] are plain field reads afterwards, which is
    /// what the JavaScript binding needs to expose them as properties that
    /// cannot fail or block.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DecodeImage`] when the header is unreadable or the
    /// format is not one this build of Skia supports.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, Error> {
        let data = Data::new_copy(bytes);
        // Skia first, because it reads everything but one format. An AVIF is
        // that one: it decodes none of them, so asking it would refuse the
        // file before the decoder that can read it was ever consulted.
        let image = match SkImage::from_encoded(data.clone()) {
            Some(image) => image,
            None if crate::decode::avif::is_avif(bytes) => {
                decode_frame(&data, 0, None)?
            }
            None => {
                return Err(Error::DecodeImage {
                    reason: "skia could not decode the encoded image bytes"
                        .to_string(),
                });
            }
        };
        let delays = frame_delays(&data);
        Ok(Self {
            playback: Mutex::new(None),
            inner: image,
            encoded: (delays.len() > 1).then_some(data),
            delays,
        })
    }

    /// Builds an [`Image`] directly from a raw pixel buffer.
    ///
    /// The intended bridge for decoded video frames and generated pixel
    /// data: no PNG/JPEG/WebP encode round trip is required.
    ///
    /// The caller specifies pixel layout and color metadata explicitly.
    /// `pixel_format` covers the pixel layout and alpha mode (premul vs
    /// unpremul); `color_space` is a `PixelColorSpace` (the same enum used
    /// for surface readback), so callers must explicitly state whether
    /// pixels are gamma-coded sRGB / Display P3 / Rec.2020 or their linear
    /// counterparts. There is no implicit fallback to sRGB.
    ///
    /// Validation:
    ///
    /// - `width` and `height` must be non-zero.
    /// - `stride` must be at least `width * pixel_format.bytes_per_pixel()`.
    /// - `bytes.len()` must equal `stride * height` exactly.
    ///
    /// Pixel data is copied; the returned image owns its storage. F16 / F32
    /// formats preserve HDR values without clamping.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero,
    /// [`Error::InvalidStride`] if `stride` is shorter than one row of
    /// `pixel_format`, [`Error::InvalidByteLength`] if `bytes` is not
    /// exactly `stride * height`, and [`Error::DecodeImage`] if Skia
    /// declines to wrap the buffer.
    pub fn from_pixels(
        bytes: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        pixel_format: PixelFormat,
        color_space: PixelColorSpace,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let bpp = pixel_format.bytes_per_pixel();
        let min_stride = (width as usize) * bpp;
        if stride < min_stride {
            return Err(Error::InvalidStride {
                expected: min_stride,
                actual: stride,
            });
        }
        let expected_len = stride * (height as usize);
        if bytes.len() != expected_len {
            return Err(Error::InvalidByteLength {
                expected: expected_len,
                actual: bytes.len(),
            });
        }

        let color_type = pixel_format.to_skia_color_type()?;
        let alpha_type = pixel_format.to_skia_alpha_type();
        let sk_color_space = color_space.to_skia_color_space()?;
        let info = ImageInfo::new(
            (width as i32, height as i32),
            color_type,
            alpha_type,
            sk_color_space,
        );

        let data = Data::new_copy(bytes);
        let image = images::raster_from_data(&info, data, stride).ok_or_else(|| {
            Error::DecodeImage {
                reason: format!(
                    "skia could not build image from raw pixels ({pixel_format:?} {color_space:?})"
                ),
            }
        })?;
        Ok(Self::still(image))
    }

    /// Rasterizes an SVG XML document into an `Image` of the given dimensions.
    ///
    /// `from_encoded` does not decode SVG XML (it handles raster codecs
    /// only); this method is the explicit SVG bridge.
    ///
    /// `width` and `height` set the SVG container size: the SVG's own
    /// `viewBox` and intrinsic dimensions are mapped into this box. A caller
    /// that needs the document's own extent -- to lay it out before choosing
    /// that box -- should go through [`Svg`] instead, which parses once and
    /// answers [`Svg::intrinsic_size`] before rasterizing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero, and
    /// [`Error::DecodeImage`] if the XML cannot be parsed or the
    /// rasterization surface cannot be allocated.
    pub fn from_svg_xml(
        svg: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        Svg::parse(svg)?.rasterize(width, height)
    }

    /// Returns the width in pixels.
    pub fn width(&self) -> u32 {
        self.inner.width().max(0) as u32
    }

    /// Returns the height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.height().max(0) as u32
    }

    /// Returns how many frames the image holds.
    ///
    /// `1` for a still image, and for an animated file with only one frame
    /// in it -- there is nothing to distinguish them by, and nothing a
    /// caller could do differently.
    ///
    /// Every animated format this crate writes reports honestly here,
    /// APNG included. Skia decodes no APNG -- `SkCodec` opens one as the
    /// still image its `IDAT` holds -- so an animation this crate had
    /// written came back claiming a single frame. This crate demuxes and
    /// composites APNG itself instead.
    pub fn frame_count(&self) -> usize {
        self.delays.len()
    }

    /// Returns how long each frame is shown, in milliseconds.
    ///
    /// One entry per frame, so this is always as long as
    /// [`Image::frame_count`]. A still image reports a single `0`: it is
    /// shown until something else is drawn, which is not a duration.
    ///
    /// A `0` on an animated frame is reported as it was stored, and is not
    /// the instant frame it reads as. Viewers clamp a very short GIF delay
    /// upward -- Firefox renders anything of 10ms or less at 100ms -- so a
    /// zero-delay frame is the slowest one, not the fastest.
    pub fn frame_delays(&self) -> &[u32] {
        &self.delays
    }

    /// Decodes one frame as an image of its own.
    ///
    /// Frames that cover only part of the canvas are composited against
    /// what came before, so every frame comes back whole and drawable, in
    /// any order. Frame `0` of a still image is the image itself.
    ///
    /// This crate has no clock, so nothing advances a frame on its own: an
    /// animation plays because a caller picks the frame each of its own
    /// output frames shows.
    ///
    /// ```no_run
    /// # use meo_skia_canvas::prelude::*;
    /// # fn main() -> Result<(), Error> {
    /// # let spinner = Image::from_encoded(&[])?;
    /// # let mut canvas = Canvas::new(64.0, 64.0);
    /// for output in 0..24 {
    ///     let frame = spinner.frame(output % spinner.frame_count())?;
    ///     canvas.context().draw_image(&frame, 0.0, 0.0);
    ///     canvas.new_page();
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::FrameOutOfRange`] when `index` is past the last
    /// frame, and [`Error::DecodeImage`] when the frame is present but will
    /// not decode.
    pub fn frame(&self, index: usize) -> Result<Self, Error> {
        if index >= self.frame_count() {
            return Err(Error::FrameOutOfRange {
                index,
                count: self.frame_count(),
            });
        }
        let Some(data) = self.encoded.as_ref() else {
            return Ok(self.clone());
        };
        // The slot is this image's own, so a caller walking the animation
        // forward keeps the decoder it built rather than rebuilding it.
        // A poisoned lock is not worth failing a decode over: the frame is
        // still correct without the shortcut.
        let mut held = self.playback.lock().ok();
        let resume = held.as_deref_mut();
        decode_frame(data, index, resume).map(Self::still)
    }

    /// Returns `true` when the color channels must not be divided by alpha
    /// to recover straight color.
    ///
    /// That covers two of Skia's three alpha modes: `Premul`, and `Opaque`
    /// where alpha is 1 throughout and the distinction does not arise. Only
    /// `Unpremul` returns `false`. Skia surfaces composite premultiplied;
    /// raw inputs may be either, depending on what produced them.
    pub fn is_premultiplied(&self) -> bool {
        matches!(
            self.inner.alpha_type(),
            AlphaType::Premul | AlphaType::Opaque
        )
    }
}

/// A parsed SVG document, held before anything decides how big to draw it.
///
/// [`Image::from_encoded`] handles raster codecs only, so an SVG arrives
/// through here. The split exists because sizing runs the opposite way round
/// from a bitmap: a bitmap tells you its extent as soon as it is decoded,
/// while a caller laying out an `auto`-sized SVG needs the document's own
/// extent *before* it can choose the box to rasterize into. Parse once, ask
/// [`Svg::intrinsic_size`], then [`Svg::rasterize`] at the size that came out
/// of layout.
pub struct Svg {
    dom: svg::Dom,
    intrinsic: Size,
    autosized: bool,
}

impl Svg {
    /// Parses an SVG XML document without rasterizing it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DecodeImage`] if the XML cannot be parsed.
    pub fn parse(xml: &str) -> Result<Self, Error> {
        let dom = svg::Dom::from_bytes(xml.as_bytes(), FontMgr::new())
            .map_err(|_| Error::DecodeImage {
                reason: "could not parse SVG XML".to_string(),
            })?;
        Ok(Self::from_dom(dom))
    }

    /// Wraps an already-parsed document, deriving its size once.
    ///
    /// The Neon binding parses with its own shared `FontMgr` and records the
    /// result into a `Picture` rather than a raster surface, so it needs the
    /// sizing without [`Svg::parse`]'s font manager or [`Svg::rasterize`]'s
    /// surface.
    pub(crate) fn from_dom(mut dom: svg::Dom) -> Self {
        let (intrinsic, autosized) = derive_intrinsic_size(&mut dom);
        Self {
            dom,
            intrinsic,
            autosized,
        }
    }

    /// The document's own size in pixels.
    ///
    /// A `width` and `height` in any of CSS's absolute units -- `px`, `in`,
    /// `cm`, `mm`, `pt`, `pc` -- is converted at the 96 dpi CSS fixes, so
    /// `width="1in"` is 96 here as it is in a browser. Skia's own answer for
    /// the same document is 90, because it converts against SVG 1.1's dpi
    /// rather than the one CSS Values and Units 3 pins the units to.
    ///
    /// **A font-relative `width` or `height` resolves against the root's own
    /// `font-size` where it states one, and against 16 px where it does
    /// not.** CSS defines `em` as the font size of the element the length is
    /// used on, so `<svg width="10em" font-size="20"/>` is 200 here as it is
    /// in a browser. What needs a parent element is a `font-size` that is
    /// itself relative -- `2em`, `150%` -- and a document being measured
    /// before it is placed has no *inherited* size to resolve that against;
    /// those fall back to 16, the initial value of CSS `font-size`. The
    /// fallback is exact for a document dropped into an unrestyled page and
    /// proportionally wrong anywhere else, and a caller that knows the font
    /// size it will render at can scale by the ratio. The alternative was
    /// refusing these lengths, which is what this did before, and that fell
    /// back to a 150 px default that is wrong in every case rather than in
    /// some of them.
    ///
    /// For a document declaring neither a usable `width`/`height` nor a
    /// `viewBox`, this is the fallback described on [`Svg::is_autosized`]
    /// rather than anything the file states.
    pub fn intrinsic_size(&self) -> Size {
        self.intrinsic
    }

    /// Whether the document declared no usable size of its own.
    ///
    /// True when neither `width` nor `height` resolves to a length. In
    /// practice that means a percentage on both -- including the `100%` Skia
    /// reports for an `<svg>` element carrying neither attribute -- since
    /// every unit CSS defines is converted. The size returned by
    /// [`Svg::intrinsic_size`] is then derived rather than read: the `viewBox`
    /// aspect ratio at the height named by `DEFAULT_SVG_HEIGHT`, or a square
    /// if there is no `viewBox` either.
    ///
    /// Also true for a document stating one dimension and leaving the other
    /// to itself, where the stated one is squared.
    ///
    /// A caller drawing into a fixed box can ignore this. One reproducing
    /// `drawImage`'s behaviour should scale an autosized document to the
    /// destination instead of to [`Svg::intrinsic_size`].
    pub fn is_autosized(&self) -> bool {
        self.autosized
    }

    /// Rasterizes the document into an [`Image`] of the given dimensions.
    ///
    /// The document's `viewBox` and intrinsic dimensions are mapped into a
    /// container of this size, then drawn into a transparent linear-light
    /// sRGB surface and snapshotted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero, and
    /// [`Error::DecodeImage`] if the rasterization surface cannot be
    /// allocated -- that variant covers the SVG surface, as its own
    /// documentation says.
    pub fn rasterize(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<Image, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        self.dom
            .set_container_size(SkSize::new(width as f32, height as f32));

        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBAF16,
            AlphaType::Premul,
            ColorSpace::new_srgb_linear(),
        );
        let mut surface =
            surfaces::raster(&info, None, None).ok_or_else(|| {
                Error::DecodeImage {
                    reason: format!(
                        "could not allocate {width}x{height} SVG render surface"
                    ),
                }
            })?;
        {
            let canvas = surface.canvas();
            canvas.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
            self.dom.render(canvas);
        }
        Ok(Image::still(surface.image_snapshot()))
    }

    /// The parsed document, for a caller that draws it somewhere other than a
    /// raster surface.
    pub(crate) fn dom_mut(&mut self) -> &mut svg::Dom {
        &mut self.dom
    }
}

/// A root `width` or `height` in CSS pixels, or `None` if it does not resolve
/// to a length on its own.
///
/// A percentage is the `None` that matters: it is relative to a containing
/// block, which a document being measured before it is placed does not have,
/// and it is also what Skia reports for an attribute that is absent. `Unknown`
/// is Skia's parse failure.
///
/// The absolute units are converted here rather than read from
/// `SvgSvg::intrinsic_size`, which resolves them at SVG 1.1's 90 dpi: see
/// [`PX_PER_INCH`]. Skia refuses the font-relative ones outright and returns
/// nothing, so `px_per_em` supplies what it will not guess at -- `None` where
/// no `em` is available, which is how a `font-size` that is itself in `em`
/// resolves to nothing rather than to a guess stacked on a guess.
fn svg_length_px(length: &Length, px_per_em: Option<f32>) -> Option<f32> {
    let px_per_unit = match length.unit {
        LengthUnit::Number | LengthUnit::PX => 1.0,
        LengthUnit::IN => PX_PER_INCH,
        LengthUnit::CM => PX_PER_CM,
        LengthUnit::MM => PX_PER_MM,
        LengthUnit::PT => PX_PER_POINT,
        LengthUnit::PC => PX_PER_PICA,
        LengthUnit::EMS => px_per_em?,
        LengthUnit::EXS => px_per_em? * EX_PER_EM,
        LengthUnit::Percentage | LengthUnit::Unknown => return None,
    };
    Some(length.value * px_per_unit)
}

/// The `em` the root's own lengths resolve against.
///
/// CSS Values and Units 3, section 5.1.1: `em` is "equal to the computed value
/// of the font-size property of the element on which it is used". The parent's
/// value is used only when the length is itself a `font-size`. So a root
/// carrying `font-size="20"` states the reference for its own `width`, and
/// nothing outside the document is needed to read it.
///
/// A `font-size` that is itself in `em` refers to the parent, and the same
/// section says what to do without one: "these units refer to the computed
/// font metrics of the parent element (or the computed font metrics
/// corresponding to the initial values of the `font` property, if the element
/// has no parent)". A root measured on its own has no parent, so the
/// parenthesis is the case, and the initial value is [`PX_PER_EM`] -- which
/// is why the inner resolution is handed that rather than nothing, making
/// `font-size="2em"` 32 rather than the fallback.
///
/// It cannot recurse: the inner call resolves against a constant, so there is
/// no second lookup to make.
///
/// `150%` still yields nothing and falls back. A percentage `font-size` is
/// defined by CSS Fonts rather than by the sentence above, and that has not
/// been read here -- so it is left unresolved rather than given an answer this
/// module cannot source. The keyword form -- `larger`, `medium` -- arrives as
/// no length at all, Skia reporting the `Inherit` variant, and lands in the
/// same place.
fn root_px_per_em(root: &svg::Svg) -> f32 {
    root.font_size()
        .and_then(|font_size| font_size.size())
        .and_then(|length| svg_length_px(length, Some(PX_PER_EM)))
        .unwrap_or(PX_PER_EM)
}

/// Whether a length is the `100%` Skia reports for an absent attribute.
///
/// A document actually written `width="100%"` is indistinguishable from one
/// that omits `width`, because Skia resolves the omission to the same value.
/// Both mean "as wide as you like", which is what the callers below treat it
/// as.
fn is_auto(length: &Length) -> bool {
    length.unit == LengthUnit::Percentage && length.value == 100.0
}

/// Works out how big an SVG wants to be, mirroring Chrome.
///
/// Returns the size and whether it had to be invented.
///
/// Every length the document states is converted by [`svg_length_px`], so
/// `10cm` and `10em` are read as readily as `10`. What is left over is the
/// genuinely undetermined case -- both dimensions a percentage, which is also
/// how Skia reports an `<svg>` carrying neither attribute -- and there the
/// size is derived from the `viewBox` aspect ratio at
/// [`DEFAULT_SVG_HEIGHT`], or a square if there is no `viewBox` either.
///
/// One dimension stated and the other left to itself gives a square of the
/// stated one. That is not Chrome's rule for a replaced element, and the rule
/// predates this: what changed is its reach, since a stated dimension in any
/// unit now squares where only a unitless one did. It is left alone because
/// this is about which lengths are read, not about what an under-specified
/// document resolves to.
///
/// # This size follows CSS and the document's contents do not
///
/// Only the root's own `width` and `height` are converted here. Every length
/// *inside* the document is resolved by Skia through a `SkSVGLengthContext`
/// built with no dpi argument -- `SkSVGDOM::render` and `SkSVGDOM::renderNode`
/// each build their own, and the constructor builds a third for
/// `fContainerSize` -- so those keep the 90 that [`PX_PER_INCH`] describes,
/// and nothing in skia-safe's `modules/svg` mentions dpi at all, so there is
/// no way to change it from here.
///
/// The two therefore disagree for a document that sizes itself in absolute
/// units *and* draws in them: `<svg width="1in"><rect width="1in"/></svg>`
/// gets a 96-pixel box holding a 90-pixel rect. A `viewBox` hides it, because
/// content is then scaled into the box rather than resolved against a
/// reference of its own, and so does content in user units, which is the
/// common case. Fixing it needs a dpi argument skia-safe does not expose.
fn derive_intrinsic_size(dom: &mut svg::Dom) -> (Size, bool) {
    let root = dom.root();
    let px_per_em = Some(root_px_per_em(&root));
    let width = root.width();
    let height = root.height();

    let derived = match (
        svg_length_px(width, px_per_em),
        svg_length_px(height, px_per_em),
    ) {
        (Some(width), Some(height)) => {
            return (Size::new(width, height), false);
        }
        (None, Some(height)) if is_auto(width) => Size::new(height, height),
        (Some(width), None) if is_auto(height) => Size::new(width, width),
        _ => {
            let aspect = root
                .view_box()
                .map(|vb| vb.width() / vb.height())
                .unwrap_or(1.0);
            Size::new(DEFAULT_SVG_HEIGHT * aspect, DEFAULT_SVG_HEIGHT)
        }
    };
    (derived, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-pixel PNG, which is a still image however it is asked about.
    fn still() -> Vec<u8> {
        // 1x1 opaque red, written by the crate this module's frames come
        // back through.
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer =
                encoder.write_header().expect("a header this crate wrote");
            writer
                .write_image_data(&[255, 0, 0, 255])
                .expect("one pixel");
            writer.finish().expect("the encoder closes");
        }
        bytes
    }

    #[test]
    fn a_still_image_does_not_hold_on_to_its_encoded_bytes() {
        // The field exists so an animation can decode frames on demand. A
        // still image has no other frame to reach, so keeping the bytes
        // would be a copy of every PNG ever loaded, held for as long as the
        // image is -- which is what the field's own documentation says it
        // avoids, and which nothing visible through the API would show.
        let image = Image::from_encoded(&still()).expect("decodes");
        assert_eq!(image.frame_count(), 1);
        assert!(image.encoded.is_none(), "nothing to reach, nothing kept");
    }

    #[test]
    fn an_animation_holds_on_to_them_because_it_has_frames_to_reach() {
        let bytes = std::fs::read("tests/assets/images/animated.gif")
            .expect("the fixture is checked in");
        let image = Image::from_encoded(&bytes).expect("decodes");
        assert!(image.frame_count() > 1);
        assert!(
            image.encoded.is_some(),
            "the other frames are still in there"
        );
    }

    /// The three ways a document can decline to state its own size, and the
    /// one where it states it plainly.
    ///
    /// Asserted against Chrome's replaced-element rules rather than against
    /// `derive_intrinsic_size` restating itself: an `<svg>` with no `width`
    /// or `height` is 150 CSS pixels tall with the width following the
    /// `viewBox` ratio, which is what a browser does with the same markup.
    #[test]
    fn an_svg_without_a_declared_size_falls_back_the_way_chrome_does() {
        let declared = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(declared.intrinsic_size(), Size::new(40.0, 20.0));
        assert!(
            !declared.is_autosized(),
            "a document stating its size is not autosized"
        );

        // No width or height, but a 2:1 viewBox: 150 tall, 300 wide.
        let boxed = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(boxed.intrinsic_size(), Size::new(300.0, 150.0));
        assert!(boxed.is_autosized(), "no declared size means autosized");

        // Neither: a square at the default height.
        let bare = Svg::parse(r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#)
            .expect("valid SVG");
        assert_eq!(bare.intrinsic_size(), Size::new(150.0, 150.0));
        assert!(bare.is_autosized());
    }

    /// A `<svg>` sized in `unit`, parsed.
    fn sized(unit: &str) -> Svg {
        Svg::parse(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10{unit}" height="10{unit}"/>"#
        ))
        .expect("valid SVG")
    }

    /// Every absolute unit resolves at the 96 dpi CSS fixes.
    ///
    /// The expected values are worked out from CSS Values and Units 3 section
    /// 5.2 rather than from this module's constants, which would only assert
    /// the table against itself: `1in` is 96 px, a centimetre is an inch over
    /// 2.54, a millimetre a tenth of that, a point an inch over 72 and a pica
    /// twelve points.
    ///
    /// Each row also names what Skia answers for the same document, because
    /// that is what these lengths used to resolve to and the difference is
    /// the whole of this test's subject. Skia converts at SVG 1.1's 90 dpi,
    /// and its point is an inch over 72.272 rather than CSS's 72 -- so `pt`
    /// and `pc` are out by more than the other three.
    #[test]
    fn an_absolute_length_resolves_at_the_dpi_css_fixes() {
        // unit, CSS pixels for `10<unit>`, what Skia alone reports.
        let expected = [
            ("px", 10.0, 10.0),
            ("in", 960.0, 900.0),
            ("cm", 377.952_76, 354.330_7),
            ("mm", 37.795_276, 35.433_07),
            ("pt", 13.333_333, 12.453_0),
            ("pc", 160.0, 149.435_5),
        ];
        for (unit, css, skia) in expected {
            let svg = sized(unit);
            let Size { width, height } = svg.intrinsic_size();
            assert!(
                (width - css).abs() < 1e-3 && (height - css).abs() < 1e-3,
                "10{unit} is {css} CSS pixels, got {width}x{height}"
            );
            assert!(
                !svg.is_autosized(),
                "10{unit} is a size the document states"
            );
            if unit != "px" {
                assert!(
                    (width - skia).abs() > 1e-3,
                    "10{unit} still reads as Skia's {skia}, so the conversion \
                     is not this module's"
                );
            }
        }
    }

    /// A document stating no font size resolves `em` against 16 px.
    ///
    /// `PX_PER_EM` states the assumption, and 16 is the initial value of CSS
    /// `font-size`. `1ex` is half of it, which CSS Values and Units 3 section
    /// 5.1.1 says "must be assumed" where the x-height cannot be determined --
    /// and nothing here loads the font.
    ///
    /// This replaces a test that asserted the opposite. It was written to
    /// assert the limitation deliberately, so that converting these lengths
    /// would fail it: `10em` used to fall through to a 150x150 square, which
    /// is not the document's size under any font size at all. 160 is right
    /// for an unrestyled page and proportionally wrong elsewhere, which is a
    /// better answer than one that is wrong everywhere.
    #[test]
    fn a_font_relative_length_falls_back_to_the_initial_font_size() {
        let em = sized("em");
        assert_eq!(em.intrinsic_size(), Size::new(160.0, 160.0));
        assert!(!em.is_autosized(), "the document did state a size");

        let ex = sized("ex");
        assert_eq!(ex.intrinsic_size(), Size::new(80.0, 80.0));
        assert!(!ex.is_autosized());
    }

    /// A root stating its own `font-size` answers for its own lengths.
    ///
    /// CSS Values and Units 3 section 5.1.1 defines `em` as the computed
    /// `font-size` of the element the length is used on -- the parent's only
    /// where the length is itself a `font-size`. So nothing outside the
    /// document is needed for this, and 16 is a fallback rather than the only
    /// answer available. The expectations are what a browser lays the same
    /// markup out at.
    #[test]
    fn a_root_font_size_is_the_em_its_own_lengths_resolve_against() {
        // 10em at 20px, against the 160 a document stating nothing gets.
        let stated = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="10em" font-size="20"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(stated.intrinsic_size(), Size::new(200.0, 200.0));

        // The font size is a length like any other, so it carries units too:
        // 1cm is 37.795 px, and 2ex of it is 37.795.
        let in_cm = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="2ex" height="2ex" font-size="1cm"/>"#,
        )
        .expect("valid SVG");
        let Size { width, height } = in_cm.intrinsic_size();
        let one_cm = 96.0 / 2.54;
        assert!(
            (width - one_cm).abs() < 1e-3 && (height - one_cm).abs() < 1e-3,
            "2ex of a 1cm em is one cm; got {width}x{height}"
        );
    }

    /// A `font-size` in `em` resolves against the initial value, not the
    /// fallback.
    ///
    /// CSS Values and Units 3 section 5.1.1 says an `em` inside `font-size`
    /// refers to the parent, "or the computed font metrics corresponding to
    /// the initial values of the `font` property, if the element has no
    /// parent". A root measured on its own is that case, so `font-size="2em"`
    /// is 32 and `10em` of it is 320.
    ///
    /// Asserted at 320 rather than at the 160 an earlier version of this test
    /// claimed. That version had a comment explaining why 160 was right, which
    /// is the durable way to be wrong -- a reader sees a decision rather than
    /// a gap.
    #[test]
    fn an_em_font_size_resolves_against_the_initial_value() {
        let doubled = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="10em" font-size="2em"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(doubled.intrinsic_size(), Size::new(320.0, 320.0));
    }

    /// The `font-size` forms that carry no length fall back to 16.
    ///
    /// A percentage is left unresolved deliberately: it is defined by CSS
    /// Fonts rather than by the section the `em` case comes from, and that has
    /// not been read here, so it gets the fallback rather than an answer this
    /// module cannot source. The keywords arrive as no length at all, Skia
    /// reporting the `Inherit` variant, and land in the same place.
    ///
    /// `is_autosized` stays false throughout: the document stated a size, and
    /// which reference resolved it is not the same question.
    #[test]
    fn a_font_size_with_no_length_falls_back_to_the_initial_value() {
        for font_size in ["150%", "larger", "inherit", "medium"] {
            let svg = Svg::parse(&format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="10em" font-size="{font_size}"/>"#
            ))
            .expect("valid SVG");
            assert_eq!(
                svg.intrinsic_size(),
                Size::new(160.0, 160.0),
                "font-size=\"{font_size}\" carries no length, so the em falls back"
            );
            assert!(
                !svg.is_autosized(),
                "font-size=\"{font_size}\": the document still stated a size"
            );
        }
    }

    /// A percentage is still the length that cannot be resolved.
    ///
    /// It is relative to a containing block, and a document being measured
    /// before it is placed has none -- so this is the case the fallback
    /// exists for, and the one unit conversion does not reach.
    #[test]
    fn a_percentage_is_the_length_that_stays_unresolved() {
        let half = sized("%");
        assert_eq!(half.intrinsic_size(), Size::new(150.0, 150.0));
        assert!(half.is_autosized());

        // 100% specifically, which is also how Skia reports an absent
        // attribute, still squares the dimension that was stated.
        let one_sided = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="2cm"/>"#,
        )
        .expect("valid SVG");
        let Size { width, height } = one_sided.intrinsic_size();
        let two_cm = 2.0 * 96.0 / 2.54;
        assert!(
            (width - two_cm).abs() < 1e-3 && (height - two_cm).abs() < 1e-3,
            "the stated dimension squares, converted: got {width}x{height}"
        );
        assert!(one_sided.is_autosized(), "one dimension was invented");
    }

    /// The raster size is the caller's, not the document's.
    #[test]
    fn rasterizing_uses_the_requested_size_not_the_intrinsic_one() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"/>"#;
        let image = Image::from_svg_xml(xml, 8, 4).expect("rasterizes");
        assert_eq!((image.width(), image.height()), (8, 4));

        let mut parsed = Svg::parse(xml).expect("valid SVG");
        assert_eq!(parsed.intrinsic_size(), Size::new(40.0, 20.0));
        let from_handle = parsed.rasterize(8, 4).expect("rasterizes");
        assert_eq!((from_handle.width(), from_handle.height()), (8, 4));
    }

    #[test]
    fn a_zero_dimension_is_refused_rather_than_allocated() {
        let xml =
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        assert!(matches!(
            Image::from_svg_xml(xml, 0, 4),
            Err(Error::InvalidDimensions { .. })
        ));
        assert!(matches!(
            Svg::parse(xml).expect("valid SVG").rasterize(4, 0),
            Err(Error::InvalidDimensions { .. })
        ));
    }

    #[test]
    fn unparseable_xml_is_an_error_rather_than_a_default_document() {
        assert!(matches!(Svg::parse("<svg"), Err(Error::DecodeImage { .. })));
    }
}
