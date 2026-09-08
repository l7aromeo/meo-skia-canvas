use std::{borrow::Cow, ops::Range, sync::Mutex};

use quick_xml::{Reader, events::Event};

use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorType, Data, Font, FontMgr, FontStyle,
    Image as SkImage, ImageInfo, Size as SkSize,
    codec::{self, Codec},
    images, surfaces,
    svg::{self, FontSize, Length, LengthUnit, TypedNode},
};

use crate::{
    color::{RgbaLinear, rgba_linear_to_skia_color},
    css,
    error::Error,
    geometry::Size,
    pixels::PixelExportOptions,
};

/// The size an SVG with no declared size of its own is laid out against.
///
/// CSS's default object size for a replaced element, 300 by 150. An
/// undimensioned document is **contained** in it rather than hung from its
/// height: the `viewBox` aspect ratio decides which of the two bounds binds,
/// so a document wider than 2:1 is limited by the width and everything else
/// by the height.
///
/// The height alone used to stand for both, with the width following from the
/// aspect ratio and nothing bounding it. That is right for every document
/// 2:1 or taller -- which is most of them, and why it survived -- and wrong
/// beyond that: a 4:1 document came out 600 wide where a browser gives 300,
/// and one with no `viewBox` came out square at 150 where a browser gives the
/// default object size itself.
const DEFAULT_SVG_WIDTH: f32 = 300.0;

/// The other half of [`DEFAULT_SVG_WIDTH`], which describes both.
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
    /// The caller states the layout explicitly, as
    /// [`PixelExportOptions`]: the bit depth, the color space, and whether
    /// color channels are already scaled by alpha. It is the same type
    /// [`ImageData::from_pixels`](crate::pixels::ImageData::from_pixels)
    /// takes and the same one surface readback returns, so a buffer read out
    /// of one canvas can be handed to another without restating what it is,
    /// and there is no implicit fallback to sRGB.
    ///
    /// Validation:
    ///
    /// - `width` and `height` must be non-zero.
    /// - `stride` must be at least one row at this depth.
    /// - `bytes.len()` must equal `stride * height` exactly.
    ///
    /// Pixel data is copied; the returned image owns its storage. F16 / F32
    /// depths preserve HDR values without clamping.
    ///
    /// Not every depth and alpha mode cross to something Skia will wrap --
    /// the depth alone decides the color type, so an unpremultiplied float
    /// buffer is nameable here and Skia may still decline it. That is
    /// reported as [`Error::DecodeImage`] rather than being unrepresentable,
    /// which is the trade this signature makes: one vocabulary for a layout,
    /// checked where the layout is used rather than where it is spelled.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero,
    /// [`Error::InvalidStride`] if `stride` is shorter than one row,
    /// [`Error::InvalidByteLength`] if `bytes` is not exactly
    /// `stride * height`, [`Error::UnsupportedPixelColorSpace`] if this Skia
    /// build cannot make the color space, and [`Error::DecodeImage`] if Skia
    /// declines to wrap the buffer.
    pub fn from_pixels(
        bytes: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        options: PixelExportOptions,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let bpp = options.depth.bytes_per_pixel();
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

        let color_type = options.depth.to_skia_color_type();
        let alpha_type = options.to_alpha_type();
        let sk_color_space = options.color_space.to_skia_color_space()?;
        let info = ImageInfo::new(
            (width as i32, height as i32),
            color_type,
            alpha_type,
            sk_color_space,
        );

        let data = Data::new_copy(bytes);
        let image = images::raster_from_data(&info, data, stride).ok_or_else(
            || Error::DecodeImage {
                reason: format!(
                    "skia could not build image from raw pixels ({options:?})"
                ),
            },
        )?;
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
    /// A `<style>` element in the document is ignored and its rules are lost,
    /// so paint declared only there renders as the initial black with nothing
    /// reported -- while an inline `style=` attribute is honoured. What that
    /// costs depends on what the stylesheet carried; [`Svg`] documents the
    /// whole of it, including the workaround.
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
///
/// # A `<style>` element is ignored, silently
///
/// Skia implements no `<style>` element: `gTagFactories` in `SkSVGDOM.cpp`
/// has no entry for the tag, so the element is discarded along with every
/// rule in it. **Anything declared only in a stylesheet is lost** -- paint,
/// `font-family`, `opacity`, any of it. A document whose fill is authored
/// that way parses, rasterizes and comes back drawn in the initial black,
/// byte-identical to the same document with no fill at all. Nothing reports
/// it.
///
/// An inline `style=` **attribute** is honoured, because Skia parses it into
/// presentation attributes. So the same declaration works one way and not the
/// other, which is what makes the failure baffling rather than merely
/// missing. Measured, first pixel of a 4x4 rasterization:
///
/// | markup | pixel |
/// | --- | --- |
/// | `<rect fill="#FF0000"/>` | `[255, 0, 0, 255]` |
/// | `<rect style="fill:#FF0000"/>` | `[255, 0, 0, 255]` |
/// | `<style>rect{fill:#FF0000}</style><rect/>` | `[0, 0, 0, 255]` |
/// | `<rect/>` | `[0, 0, 0, 255]` |
///
/// What that costs depends on what the stylesheet carried, and it is worth
/// knowing before concluding a document is broken -- or before concluding it
/// is fine. Rules that declare paint
/// -- the `.cls-1{fill:#fff}` shape a colour-deduplicating exporter emits --
/// are the case above, and those shapes come out black. An `@import` of a
/// webfont loses the font and nothing else, so the geometry is unaffected.
/// Hover and animation rules describe states a still raster never enters.
/// A document that declares its paint as attributes and uses `<style>` only
/// for a font renders correctly apart from the typeface.
///
/// This crate does not work around it. Expanding stylesheet rules before
/// parsing is a CSS cascade, and a partial one renders some documents
/// correctly and others not, which is worse than ignoring them uniformly.
/// The fix belongs upstream of here: svgo's `inlineStyles` plugin merges a
/// `<style>` element's declarations into each element's `style` attribute,
/// and that attribute is the form Skia does parse -- so the asymmetry above
/// is exactly what makes the workaround work.
///
/// **Run it with `onlyMatchedOnce: false`:**
///
/// ```text
/// { name: "inlineStyles", params: { onlyMatchedOnce: false } }
/// ```
///
/// The invocation rather than the option name, because the obvious
/// `svgo --enable=inlineStyles` takes the default and fails the same way the
/// original document does -- the shapes stay black, nothing is reported, and
/// the reader followed this paragraph to get there. Measured against svgo
/// 4.1.0; these are that version's defaults rather than a property of the
/// plugin for all time.
///
/// `onlyMatchedOnce` defaults to `true`, and `plugins/inlineStyles.js` then
/// reads `if (onlyMatchedOnce && matchedElements.length > 1) continue;` -- a
/// selector matching more than one element is skipped entirely. A class
/// shared by three rects is exactly the export shape that comes out black
/// here, so the plugin at its defaults fixes a single-match stylesheet and
/// does nothing for the common one. The default is right for a minifier,
/// which svgo is: inlining a shared rule into every match duplicates the
/// declaration and grows the file.
///
/// `useMqs` is the second one, and it is left as a rule rather than a list
/// because a list of literals reads as closed and is not. It defaults to
/// `['', 'screen']`, and **that `'screen'` entry matches nothing** -- the
/// string compared against the list is the at-rule's name followed by its
/// prelude, so a rule inside `@media screen` presents as `"media screen"`,
/// which `'screen'` never equals. The effective default is rules outside any
/// media query. Measured: `@media screen` is skipped at the defaults,
/// inlines with `["", "media screen"]`, and is skipped again with svgo's own
/// `["", "screen"]` -- so a reader reasoning from the shipped default, which
/// is an authoritative source, gets a wrong answer.
///
/// So a document whose paint sits inside any `@media` block needs that
/// block's own literal added, spelled name-then-prelude. Which blocks belong
/// is a judgement rather than a list: rasterizing is what a browser does for
/// a screen, so `"media screen"` belongs and `"media print"` does not, and
/// widening until everything matches diverges from a browser in the opposite
/// direction -- the failure this section exists to prevent.
///
/// `usePseudos` stays at `['']`, which skips `:hover` and its neighbours --
/// right for the same reason, since a still image never enters those states.
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
        Ok(Self::from_dom(Self::parse_dom(
            xml.as_bytes(),
            FontMgr::new(),
            &[],
            &[],
        )?))
    }

    /// Parses SVG bytes into a document, rewriting what has to be rewritten
    /// before Skia reads it.
    ///
    /// Both doors into this crate go through here -- [`Svg::parse`] and the
    /// Neon binding's `record_svg`, which parses its own bytes with a shared
    /// font manager -- so that the two cannot disagree about what a document
    /// says. They did once: the rewrite of text positioning lengths landed on
    /// `parse` alone would have left `loadImage` short by six per cent on
    /// exactly the documents the crate side had just fixed.
    pub(crate) fn parse_dom(
        xml: &[u8],
        font_mgr: FontMgr,
        generics: &[(String, String)],
        claimed: &[(String, String)],
    ) -> Result<svg::Dom, Error> {
        let rewritten =
            text_position_lengths_in_px(xml, generics, claimed, &font_mgr);
        let bytes = rewritten.as_deref().unwrap_or(xml);
        svg::Dom::from_bytes(bytes, font_mgr).map_err(|_| Error::DecodeImage {
            reason: "could not parse SVG XML".to_string(),
        })
    }

    /// Wraps an already-parsed document, deriving its size once.
    ///
    /// The Neon binding parses with its own shared `FontMgr` and records the
    /// result into a `Picture` rather than a raster surface, so it needs the
    /// sizing without [`Svg::parse`]'s font manager or [`Svg::rasterize`]'s
    /// surface.
    pub(crate) fn from_dom(mut dom: svg::Dom) -> Self {
        let (intrinsic, autosized) = derive_intrinsic_size(&mut dom);
        descend(&dom.root());
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
    /// The document is rewritten to agree with this before it is laid out:
    /// the root's stated `width` and `height` are replaced by their value in
    /// `px`, so a child at `100%` covers what was reported rather than Skia's
    /// six per cent less. Every `in`, `cm`, `mm`, `pt` and `pc` below the root
    /// is rewritten the same way, at any depth, so
    /// `<svg width="1in"><rect width="1in"/></svg>` is a 96-pixel box holding
    /// a 96-pixel rect. Skia's own dpi is still 90 and still unreachable --
    /// `SkSVGDOM::render` builds its length context with no dpi argument --
    /// but by the time it does there is no absolute unit left for it to
    /// resolve. Percentages and user units are left as written, since neither
    /// carries a dpi.
    ///
    /// **Text positioning is the exception.** `x`, `y`, `dx` and `dy` on
    /// `<text>`, `<tspan>` and `<textPath>` are lists that skia-safe exposes
    /// for reading only, so `<text x="1in">` still resolves at 90.
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
    /// aspect ratio contained in the 300-by-150 default object size, or that
    /// size unchanged if the document states no usable ratio.
    ///
    /// Also true for a document stating one dimension and leaving the other
    /// to itself, where the missing one comes from the `viewBox` ratio, or
    /// from the default object size when there is no ratio to use.
    ///
    /// A caller drawing into a fixed box can ignore this. One reproducing
    /// `drawImage`'s behaviour should scale an autosized document to the
    /// destination instead of to [`Svg::intrinsic_size`].
    pub fn is_autosized(&self) -> bool {
        self.autosized
    }

    /// Sets the colour every `currentColor` in the document resolves against.
    ///
    /// SVG 2 [section 13.3] defines `color` as an indirect value for `fill`
    /// and `stroke`: "The `color` property is used to provide a potential
    /// indirect value, `currentColor`, for the `fill`, `stroke`, ...
    /// properties." The specification's own example sets the paint of an
    /// inline SVG fragment from the `color` an HTML document inherits, which
    /// is the mechanism this exposes -- one asset drawn in several colours,
    /// without a copy per colour.
    ///
    /// The value is set on the root and reaches the rest of the document by
    /// ordinary inheritance, so it applies at any depth and to strokes as
    /// readily as to fills. Call it before [`Svg::rasterize`]; the override
    /// is read when the document renders, not when it was parsed.
    ///
    /// Alpha is carried. A colour at half alpha paints `currentColor` at half
    /// alpha rather than being flattened to opaque.
    ///
    /// # What it does not do
    ///
    /// Nothing to paint that is not `currentColor`. A `fill="#00FF00"` stays
    /// green and a `fill="url(#grad)"` keeps its gradient, because neither
    /// asks for the indirect value. This is not a recolour of every fill --
    /// that would overwrite an IRI paint rather than recolour it, flattening
    /// a gradient to a flat colour, which is rarely what "recolour this icon"
    /// means.
    ///
    /// A document with no `currentColor` anywhere is therefore unaffected,
    /// and a document whose paint is authored in a `<style>` element is
    /// unaffected for a different reason -- see the note on [`Svg`], which
    /// applies here too.
    ///
    /// Nor does it reach a subtree that declares a `color` of its own.
    /// `<g color="#0000FF">` resolves its descendants' `currentColor`
    /// against that blue, and this sets the root, so the nearer declaration
    /// wins. That is inheritance behaving correctly rather than a limit of
    /// the override -- but it means an asset whose author wrapped part of it
    /// in a coloured group recolours only the rest, and nothing here reports
    /// the difference. Setting the root does replace a `color` the root
    /// itself declared.
    ///
    /// [section 13.3]: https://www.w3.org/TR/SVG2/painting.html#ColorProperty
    pub fn set_current_color(&mut self, color: RgbaLinear) {
        self.dom.root().set_color(rgba_linear_to_skia_color(color));
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

/// The font size a document inherits where it states none.
///
/// CSS Values and Units 3 makes `medium` the initial `font-size`, which every
/// browser renders as 16 pixels, and Chrome computes 16 for an SVG `<text>`
/// that states nothing. Skia's initial value is 24 --
/// `result.fFontSize.init(SkSVGLength(24))` in `SkSVGAttribute.cpp` -- so a
/// document saying nothing renders half again too large. The root is given
/// this value explicitly when it states none, which fixes the text size and
/// gives `em` the same reference a browser uses.
pub(crate) const CSS_INITIAL_FONT_SIZE: f32 = 16.0;

/// The attributes whose value is a length, or a list of them.
///
/// `em` and `ex` can appear in any of these, so all of them are read. Skia
/// resolves neither unit anywhere -- `SkSVGLengthContext::resolve` has no case
/// for `kEMS` or `kEXS` and returns 0 -- so a length in `em` covers nothing
/// until it is rewritten here.
const LENGTH_ATTRIBUTES: [&[u8]; 18] = [
    b"x",
    b"y",
    b"width",
    b"height",
    b"rx",
    b"ry",
    b"cx",
    b"cy",
    b"r",
    b"x1",
    b"y1",
    b"x2",
    b"y2",
    b"fx",
    b"fy",
    b"dx",
    b"dy",
    b"stroke-width",
];

/// What the elements below one point in the tree inherit.
#[derive(Clone)]
struct Cascade {
    /// The computed `font-size` in pixels, which `em` resolves against.
    font_size: f32,
    /// The `font-family` in effect, which decides what `ex` is worth.
    family: Option<String>,
}

/// The value of `font-size` an element states, from `style` or the attribute.
///
/// `style` wins, which is CSS's rule and measured to be Skia's: `font-size="10"
/// style="font-size:32"` renders byte-identically to `font-size="32"`. A pass
/// reading only the attribute would compute the wrong `em` for every document
/// that uses the shorthand.
fn stated_font_size<'a>(
    attribute: &'a str,
    style: Option<&'a str>,
) -> Option<&'a str> {
    style_declaration(style, "font-size")
        .or(Some(attribute))
        .filter(|value| !value.is_empty())
}

/// The value one property takes in a `style` attribute.
///
/// Skia reads a `style` declaration for `font-family` as well as `font-size`:
/// `style="font-family:Courier"` renders byte-identically to the attribute
/// form, and so does `style="font-family:sans-serif"`. A pass reading only the
/// attribute therefore sees the wrong family, substitutes the wrong generic
/// and measures `ex` against the wrong face. Skia implements no `<style>`
/// *element*, so this attribute is the only stylesheet syntax that reaches it.
fn style_declaration<'a>(
    style: Option<&'a str>,
    property: &str,
) -> Option<&'a str> {
    style?.split(';').find_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(property)
            .then(|| value.trim())
    })
}

/// One length in pixels, resolved against the cascade it sits in.
///
/// `parent` is what a `font-size` resolves against and `own` what every other
/// length on the same element does -- two different references on one element,
/// which Chrome confirms: in `<g font-size="2em"><rect width="2em"/></g>` the
/// `g` computes to 32 against an inherited 16, and the rect's `2em` is 64.
///
/// A bare number is user units, which are pixels here, and is returned
/// unchanged so that a document already in them is not rewritten.
fn length_in_px(value: &str, reference: f32, ex_ratio: f32) -> Option<f32> {
    let (number, unit) = css::split_number(value.trim())?;
    let unit = unit.trim();
    let scale = match unit.to_ascii_lowercase().as_str() {
        "em" => reference,
        "ex" => reference * ex_ratio,
        "%" => reference / 100.0,
        "" | "px" => 1.0,
        "in" => PX_PER_INCH,
        "cm" => PX_PER_CM,
        "mm" => PX_PER_MM,
        "pt" => PX_PER_POINT,
        "pc" => PX_PER_PICA,
        _ => return None,
    };
    let px = number * scale;
    px.is_finite().then_some(px)
}

/// Whether a value is stated in a unit only this pass can resolve.
///
/// Absolute units are left to the pass that already handles them, so that a
/// document using nothing but `in` comes back from here byte-identical.
///
/// **A percentage counts only on `font-size`.** There it is a fraction of the
/// parent's computed size, which is what `percentage_resolves` says. Anywhere
/// else it is a fraction of the viewport -- `SkSVGLengthContext::resolve` has
/// a `kPercentage` case that reads `fViewport` -- and Skia gets it right, so
/// resolving it here against a font size would be doubly wrong: the wrong
/// reference, and a value frozen at parse time that should track the viewport
/// the document is rendered into.
fn is_font_relative(value: &str, percentage_resolves: bool) -> bool {
    css::split_number(value.trim())
        .map(
            |(_, unit)| match unit.trim().to_ascii_lowercase().as_str() {
                "em" | "ex" => true,
                "%" => percentage_resolves,
                _ => false,
            },
        )
        .unwrap_or(false)
}

/// A list of lengths with every font-relative item resolved, or `None` when
/// none of them is.
fn relative_list_in_px(
    value: &str,
    reference: f32,
    ex_ratio: f32,
) -> Option<String> {
    let mut moved = false;
    let items = value
        .split([' ', '\t', '\r', '\n', ','])
        .filter(|item| !item.is_empty())
        .map(|item| match is_font_relative(item, false) {
            true => match length_in_px(item, reference, ex_ratio) {
                Some(px) => {
                    moved = true;
                    px.to_string()
                }
                None => item.to_string(),
            },
            false => item.to_string(),
        })
        .collect::<Vec<_>>();
    moved.then(|| items.join(" "))
}

/// What one `ex` is worth as a fraction of the font size, for `family`.
///
/// **The x-height of the face that will actually be drawn with**, whether or
/// not the document's family resolved. CSS defines `ex` as the x-height and
/// browsers use the real one: Chrome renders `4ex` at `font-size="20"` as
/// 35.898 rather than 40, a ratio of 0.449. The ratio varies between faces by
/// more than that error -- 0.523, 0.468 and 0.454 for three families measured
/// here -- so half an em is neither the right answer nor a close one.
///
/// A family the document names but the machine does not have is the common
/// case, not an edge: Skia's own initial family is `"Sans"`, which macOS does
/// not have. There the face drawn with is whatever the font manager returns
/// for a null family, which is what `SkSVGText.cpp` falls back to, so asking
/// it the same question keeps the `ex` and the ink in agreement.
///
/// That question is only answerable because of how the Neon binding's
/// `font_mgr` composes its managers: it asks the system one first, and a
/// `legacy_make_typeface(None, ..)` reaching a `TypefaceFontProvider` first
/// segfaults rather than returning nothing. So this fallback depends on that
/// ordering, and not only the crash it was introduced for depends on it.
///
/// [`EX_PER_EM`] is reached only when no face resolves at all -- an empty
/// font set, where nothing will be drawn either. A constant is honest there
/// because there is no rendering for it to disagree with.
fn ex_ratio_for(family: Option<&str>, font_mgr: &FontMgr) -> f32 {
    let typeface = family
        .and_then(|family| {
            font_mgr.match_family_style(family, FontStyle::normal())
        })
        // The face Skia will draw with when the family does not resolve.
        .or_else(|| font_mgr.legacy_make_typeface(None, FontStyle::normal()));

    let Some(typeface) = typeface else {
        return EX_PER_EM;
    };
    // Measured at a nominal size and divided back out, so the ratio is the
    // face's rather than this call's.
    let (_, metrics) = Font::from_typeface(typeface, 100.0).metrics();
    match metrics.x_height.is_finite() && metrics.x_height > 0.0 {
        true => metrics.x_height / 100.0,
        false => EX_PER_EM,
    }
}

/// The x-height ratio of each family a document has asked about.
///
/// [`ex_ratio_for`] resolves a typeface and reads its metrics, which costs
/// more than parsing the element that asked. The walk needs the ratio for
/// every element, because any of them may carry a length in `ex` -- but a
/// document names very few families, and most name none at all, so the answer
/// is worth remembering rather than deriving again per element.
///
/// Measured on an 8000-element document with no `font-family` and no `ex`:
/// resolving per element cost 90 ms of the 95 ms the walk spent, against 9 ms
/// for Skia to parse and rasterise the same document. One lookup per distinct
/// family rather than one per element is the whole of that.
///
/// A `Vec` rather than a map because the key count is the number of distinct
/// families in one document -- one or two, in every document that is not
/// pathological -- and a linear scan over that beats hashing a string.
struct ExRatios<'a> {
    font_mgr: &'a FontMgr,
    seen: Vec<(Option<String>, f32)>,
}

impl<'a> ExRatios<'a> {
    fn new(font_mgr: &'a FontMgr) -> Self {
        Self {
            font_mgr,
            seen: Vec::new(),
        }
    }

    /// The ratio for `family`, resolving it the first time it is asked for.
    fn of(&mut self, family: Option<&str>) -> f32 {
        if let Some((_, ratio)) = self
            .seen
            .iter()
            .find(|(known, _)| known.as_deref() == family)
        {
            return *ratio;
        }
        let ratio = ex_ratio_for(family, self.font_mgr);
        self.seen.push((family.map(str::to_string), ratio));
        ratio
    }
}

/// The elements whose positioning attributes Skia will not let us write.
///
/// Matched by bare name, with no namespace resolution, because that is what
/// Skia does: a document with no `xmlns`, and one declaring an `xmlns` that
/// is not SVG's, both render, and a document using a prefix -- `<s:svg
/// xmlns:s="http://www.w3.org/2000/svg">` -- is refused outright. Resolving
/// namespaces here would skip the first two, which render today.
const TEXT_ELEMENTS: [&[u8]; 3] = [b"text", b"tspan", b"textPath"];

/// The attributes on those elements that hold a list of lengths.
///
/// `rotate` is a list of plain numbers rather than lengths, so it carries no
/// unit and is not here.
const TEXT_POSITION_ATTRIBUTES: [&[u8]; 4] = [b"x", b"y", b"dx", b"dy"];

/// A list of SVG lengths with every absolute one converted to `px`, or `None`
/// if the list holds none.
///
/// SVG's grammar separates the items by comma-wsp, so both `1in 2in` and
/// `1in,2in` are two lengths. The rebuilt list is space-separated, which is
/// why this returns `None` rather than an unchanged string when nothing
/// moves: an attribute with no absolute unit must not be rewritten at all,
/// or a document this cannot improve would still come out different from the
/// one that went in.
fn position_list_in_px(value: &str) -> Option<String> {
    let mut moved = false;
    let items = value
        .split([' ', '\t', '\r', '\n', ','])
        .filter(|item| !item.is_empty())
        .map(|item| match css::parse_length(item) {
            // Exactly the units `absolute_length_px` converts. `px` is
            // already what both sides read the same way, `q` is a CSS unit
            // that `SkSVGLength` has no case for, and everything relative
            // stays for the reasons given there.
            Some(length)
                if matches!(
                    length.unit.as_str(),
                    "in" | "cm" | "mm" | "pt" | "pc"
                ) && length.pixels.is_finite() =>
            {
                moved = true;
                length.pixels.to_string()
            }
            _ => item.to_string(),
        })
        .collect::<Vec<_>>();

    moved.then(|| items.join(" "))
}

/// The family a `font-family` value should be rewritten to, or `None` to leave
/// it as written.
///
/// Two substitutions, checked in that order:
///
/// A **claimed** name is one a caller registered a face under. Everything now
/// resolves from one provider -- see `FontLibrary::font_mgr` -- and that
/// provider is given a system face for each family the document names, so a
/// claim on a name the system also has, `Helvetica` or `Arial`, would put two
/// faces in one family and leave `matchStyle` to pick. Rewriting the claim to
/// the private alias the provider also files it under keeps the caller's face
/// in a family of its own, where it wins by being the only candidate.
///
/// A **generic** is rewritten to the family its curated stack picked, for the
/// same reason in reverse: a system that answers `sans-serif` itself would
/// otherwise win it. Claimed is checked first, though the two cannot collide
/// today -- a curated stack is no longer registered under a name the caller
/// has claimed -- because the precedence is a decision rather than a
/// coincidence of the registration order.
///
/// **A list is left alone, both kinds.** `font-family="Foo, sans-serif"` means
/// "Foo, and failing that a sans-serif". Rewriting one item would change what
/// the others fall back to while looking like a substitution, and choosing an
/// item would be a guess at which the author expected to win. Skia does not
/// implement the fall-through here in any case, so a document naming a
/// claimed family second in a list keeps the behaviour it has today rather
/// than gaining a different one. This is a decision, not an omission: express
/// fall-through and the rule can change.
///
/// A name that is neither is the caller's own and is left as written, whether
/// or not the machine has it.
fn family_substitution<'a>(
    value: &str,
    generics: &'a [(String, String)],
    claimed: &'a [(String, String)],
) -> Option<&'a str> {
    let value = value.trim();
    if value.contains(',') {
        return None;
    }
    // A family may be quoted in CSS, and SVG's presentation attribute takes
    // the same grammar.
    let name = value.trim_matches(['"', '\''].as_slice()).trim();
    let matching = |table: &'a [(String, String)]| {
        table.iter().find_map(|(from, to)| {
            (from.eq_ignore_ascii_case(name) && !to.is_empty())
                .then_some(to.as_str())
        })
    };
    matching(claimed).or_else(|| matching(generics))
}

/// Every family name the document will ask Skia to resolve, after the
/// substitutions [`family_substitution`] performs.
///
/// The font manager handed to `SkSVGDOM` answers from a
/// `TypefaceFontProvider` alone, which knows only what has been registered
/// into it -- so a document naming a system family resolves only if that
/// family was registered first, and this is the pass that says which ones to
/// register. See the note on `FontLibrary::font_mgr` for why the provider is
/// alone rather than behind the system manager.
///
/// # Why this walks the document a second time
///
/// `text_position_lengths_in_px` already computes these substitutions, and
/// reusing its walk would avoid a second parse. It cannot be reused: it takes
/// the font manager, because `ex` is a fraction of a resolved face's x-height,
/// and the manager is what this pass exists to build. Measuring `ex` against a
/// provisional manager instead would resolve it through whatever face stood in
/// for the real one, which is the wrong number rather than a slower one.
///
/// So the order is: collect the names here, build a manager that answers them,
/// then rewrite lengths against it. `ex` is measured against the face the
/// document actually draws with, which it was not before.
///
/// A malformed document yields what was collected before the error. Nothing
/// here refuses to parse -- the manager is a superset either way, and the
/// parser that decides whether the document is usable is Skia's.
pub(crate) fn families_named(
    xml: &[u8],
    generics: &[(String, String)],
    claimed: &[(String, String)],
) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(xml) else {
        return Vec::new();
    };

    let mut reader = Reader::from_str(text);
    let mut names: Vec<String> = Vec::new();
    let mut record = |value: &str| {
        // The substitution is what Skia will be asked for. Where there is
        // none the name is taken as written, minus the quotes CSS allows,
        // because that is the form the provider is keyed on.
        let resolved = match family_substitution(value, generics, claimed) {
            Some(concrete) => concrete.to_string(),
            None => value
                .trim()
                .trim_matches(['"', '\''].as_slice())
                .trim()
                .to_string(),
        };
        if !resolved.is_empty() && !names.contains(&resolved) {
            names.push(resolved);
        }
        // A list is not substituted -- see `family_substitution` -- and Skia
        // asks for its items, so each has to be registered on its own or the
        // list resolves to the fallback instead of to its first available
        // name.
        if value.contains(',') {
            for item in value.split(',') {
                let item =
                    item.trim().trim_matches(['"', '\''].as_slice()).trim();
                if !item.is_empty() && !names.iter().any(|seen| seen == item) {
                    names.push(item.to_string());
                }
            }
        }
    };

    loop {
        let element = match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => element,
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        for attribute in element.attributes().flatten() {
            let Ok(value) = std::str::from_utf8(attribute.value.as_ref())
            else {
                continue;
            };
            match attribute.key.as_ref() {
                b"font-family" => record(value),
                b"style" => {
                    if let Some(family) =
                        style_declaration(Some(value), "font-family")
                    {
                        record(family);
                    }
                }
                _ => {}
            }
        }
    }
    names
}

/// The document with every absolute length in a text positioning attribute
/// rewritten to `px`, or `None` to use the original bytes unchanged.
///
/// The other half of the fix `normalize_absolute_lengths` performs. That one
/// works on the parsed DOM and cannot reach these four attributes, because
/// skia-safe exposes them for reading only -- see the note there. This one
/// gets at them before Skia sees the document, by finding the byte range of
/// each attribute's value and splicing a converted list into it.
///
/// # Why this parses rather than scans
///
/// A scan for `x="` matches inside comments, `<desc>` and `<style>` bodies,
/// CDATA, and the values of other attributes. A parser distinguishes markup
/// from content, which is the whole reason this is allowed to exist:
/// `a_length_inside_a_comment_is_not_touched` is the case a scan would have
/// got wrong.
///
/// # Why it splices rather than re-serialises
///
/// Only the four attribute values are replaced, by byte range, and every
/// other byte of the document is passed through. Writing the document back
/// out through a serialiser would put the DTD, processing instructions,
/// entity declarations and significant whitespace at risk for the sake of
/// four values.
///
/// # Every failure is "did nothing"
///
/// A document that renders today has to render identically after this,
/// whether or not the rewrite fires. So this gives up -- returning `None`,
/// leaving the caller with the original bytes -- on input that is not UTF-8,
/// on a parse error, on an attribute value carrying an entity reference, and
/// on any offset that does not fall inside the input. None of those is
/// expected; the point is that the failure is refusal rather than a
/// half-rewritten document.
fn text_position_lengths_in_px(
    xml: &[u8],
    generics: &[(String, String)],
    claimed: &[(String, String)],
    font_mgr: &FontMgr,
) -> Option<Vec<u8>> {
    // Not UTF-8, so this cannot reason about the bytes. The Neon binding
    // hands over whatever a caller passed to `loadImage`, which is why this
    // takes bytes and checks rather than taking `&str` and assuming.
    let text = std::str::from_utf8(xml).ok()?;

    let mut reader = Reader::from_str(text);
    let mut splices: Vec<(Range<usize>, String)> = Vec::new();

    // The cascade, as a stack rather than a tree: an element's own entry is
    // pushed when it opens and dropped when it closes, so the top is always
    // what the element being read inherits. quick-xml gives a flat event
    // stream, and this is what makes a pre-order walk of it possible without
    // building a document.
    let mut ex_ratios = ExRatios::new(font_mgr);
    let mut cascade = vec![Cascade {
        font_size: CSS_INITIAL_FONT_SIZE,
        family: None,
    }];
    let mut depth_of_root = None;

    loop {
        // A borrowed event points into `text`, which is what makes the byte
        // ranges below obtainable at all.
        let (element, closes) = match reader.read_event() {
            Ok(Event::Start(element)) => (element, false),
            Ok(Event::Empty(element)) => (element, true),
            Ok(Event::End(_)) => {
                // The root's entry stays: it is not popped by its own close,
                // and nothing follows it.
                if cascade.len() > 1 {
                    cascade.pop();
                }
                continue;
            }
            Ok(Event::Eof) => break,
            Ok(_) => continue,
            // Malformed, or malformed in a way this build of quick-xml
            // rejects and Skia's parser might not. Either way, hands off.
            Err(_) => return None,
        };

        let inherited = cascade.last()?.clone();
        let positioned = TEXT_ELEMENTS.contains(&element.name().as_ref());
        let is_root = depth_of_root.is_none();
        if is_root {
            depth_of_root = Some(cascade.len());
        }

        // Read the element's own `font-size` and `font-family` first: a
        // `font-size` resolves against what the element inherits, and
        // everything else on the same element against what it computes to.
        let mut stated_size: Option<(Range<usize>, String)> = None;
        let mut own_family = inherited.family.clone();
        let mut style_value: Option<String> = None;
        let mut size_attribute: Option<(Range<usize>, String)> = None;

        for attribute in element.attributes() {
            let attribute = attribute.ok()?;
            let key = attribute.key.as_ref();
            let Cow::Borrowed(raw) = attribute.value else {
                return None;
            };
            if raw.contains(&b'&') {
                continue;
            }
            let value = std::str::from_utf8(raw).ok()?;
            match key {
                b"style" => style_value = Some(value.to_string()),
                b"font-size" => {
                    size_attribute =
                        Some((borrowed_range(text, raw)?, value.to_string()));
                }
                b"font-family" => {
                    own_family = family_substitution(value, generics, claimed)
                        .map(str::to_string)
                        .or_else(|| Some(value.trim().to_string()));
                }
                _ => {}
            }
        }

        // A `font-family` in the `style` attribute wins over the attribute
        // form, as CSS says and as Skia does, so it decides both what a
        // generic maps to and which face an `ex` is measured against.
        if let Some(styled) =
            style_declaration(style_value.as_deref(), "font-family")
        {
            own_family = family_substitution(styled, generics, claimed)
                .map(str::to_string)
                .or_else(|| Some(styled.trim().to_string()));
        }

        let ex_ratio = ex_ratios.of(own_family.as_deref());
        let mut own_size = inherited.font_size;
        let attribute_text = size_attribute
            .as_ref()
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        if let Some(px) = stated_font_size(
            attribute_text,
            style_value.as_deref(),
        )
        .and_then(|stated| {
            let px = length_in_px(stated, inherited.font_size, ex_ratio)?;
            // Only a font-relative size is written back. An absolute
            // one is already handled where absolute units are, and a
            // bare number is the pixels it says.
            Some((px, is_font_relative(stated, true)))
        }) {
            let (px, relative) = px;
            own_size = px;
            if let (true, Some((range, _))) = (relative, size_attribute.clone())
            {
                stated_size = Some((range, px.to_string()));
            }
        }

        if let Some(splice) = stated_size {
            splices.push(splice);
        }

        for attribute in element.attributes() {
            let attribute = attribute.ok()?;
            let key = attribute.key.as_ref();
            let Cow::Borrowed(raw) = attribute.value else {
                return None;
            };
            if raw.contains(&b'&') {
                continue;
            }
            let value = std::str::from_utf8(raw).ok()?;
            let range = borrowed_range(text, raw)?;

            let converted = if key == b"style" {
                // Only the family is substituted; the rest of the declaration
                // is passed through as written.
                style_declaration(Some(value), "font-family")
                    .and_then(|family| {
                        family_substitution(family, generics, claimed)
                            .map(|concrete| (family, concrete))
                    })
                    .map(|(family, concrete)| {
                        value.replacen(family, concrete, 1)
                    })
            } else if key == b"font-family" {
                family_substitution(value, generics, claimed)
                    .map(str::to_string)
            } else if positioned && TEXT_POSITION_ATTRIBUTES.contains(&key) {
                position_list_in_px(value)
                    .or_else(|| relative_list_in_px(value, own_size, ex_ratio))
            } else if LENGTH_ATTRIBUTES.contains(&key) {
                relative_list_in_px(value, own_size, ex_ratio)
            } else {
                None
            };
            let Some(converted) = converted else { continue };
            splices.push((range, converted));
        }

        // The root is given a `font-size` where it states none, so that the
        // text Skia draws and the `em` resolved here agree on what one is.
        //
        // "States none" is `stated_font_size`'s question and has to be asked
        // through it: a root carrying any `style` at all -- `style="fill:red"`
        // says nothing about fonts -- otherwise suppressed the injection, and
        // Skia's initial 24 then applied to text whose lengths had been
        // resolved against 16.
        let states_a_size =
            stated_font_size(attribute_text, style_value.as_deref()).is_some();
        if is_root && !states_a_size {
            let name = borrowed_range(text, element.name().as_ref())?;
            splices.push((
                name.end..name.end,
                format!(" font-size=\"{CSS_INITIAL_FONT_SIZE}\""),
            ));
        }

        if !closes {
            cascade.push(Cascade {
                font_size: own_size,
                family: own_family,
            });
        }
    }

    if splices.is_empty() {
        return None;
    }

    // Back to front, so that an earlier range is still valid after a later
    // one has changed length.
    splices.sort_by_key(|(range, _)| range.start);
    let mut out = text.as_bytes().to_vec();
    for (range, converted) in splices.into_iter().rev() {
        out.splice(range, converted.into_bytes());
    }
    Some(out)
}

/// Where a borrowed slice sits within the string it was borrowed from.
///
/// quick-xml gives an attribute's value as a slice of the input and no index
/// for it, so the index is recovered from the addresses. Comparing addresses
/// rather than dereferencing them, and the bounds check means a slice that
/// turns out to be borrowed from somewhere else yields `None` instead of a
/// range into the wrong buffer.
fn borrowed_range(haystack: &str, needle: &[u8]) -> Option<Range<usize>> {
    let base = haystack.as_ptr() as usize;
    let at = needle.as_ptr() as usize;
    let end = at.checked_add(needle.len())?;
    (at >= base && end <= base + haystack.len())
        .then(|| (at - base)..(end - base))
}

/// A length in one of CSS's absolute units, in CSS pixels, or `None` for every
/// other unit.
///
/// Narrower than [`svg_length_px`] on purpose, and the rule is the same one
/// each time a unit is added to CSS: **an absolute unit is converted, a
/// relative one never is.** An absolute unit states a physical size, which is
/// the only kind of length the two sides disagree about -- Skia converts them
/// at SVG 1.1's 90 dpi where CSS Values and Units 3 fixes 96. A relative unit
/// states a ratio against something else, carries no dpi of its own, and is
/// already correct once the thing it refers to is; rewriting one would freeze
/// the ratio at whatever the reference happened to be at parse.
///
/// So `Percentage` is left, because its reference depends on where the node
/// sits. `Number` and `PX` are left because they are already the unit both
/// sides read the same way. A unit added to this enum later falls on the
/// relative side by default, which is the safe direction: an unconverted
/// absolute unit is the defect this function exists for, and a converted
/// relative one is a new one.
///
/// `EMS` and `EXS` are left for a different and worse reason. Skia does not
/// resolve them at all: `SkSVGLengthContext::resolve` has a case for each
/// absolute unit and for `kPercentage`, and `kEMS` and `kEXS` fall to a
/// `default` that returns 0. So `2em` anywhere inside a document is zero
/// before this walk and zero after it, and rewriting `font-size` does not
/// change that -- there is no ratio being taken. Converting them here would
/// need the inherited `font-size` at each node, which this walk does not
/// track, and would be a new feature rather than this fix.
///
/// UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
/// Re-check: render `<rect font-size="16" width="2em" height="2em"/>` and see
/// whether it paints. It paints nothing today, because
/// `SkSVGLengthContext::resolve` in skia-bindings'
/// `skia/modules/svg/src/SkSVGRenderContext.cpp` returns 0 for `kEMS` and
/// `kEXS`.
///
/// Takes anything that is or converts to `Option<&Length>`, because the
/// generated accessors return `&Length` for a required attribute and
/// `Option<&Length>` for an optional one, and the two would otherwise need
/// separate call sites at every one of the attributes below.
fn absolute_length_px<'a>(
    length: impl Into<Option<&'a Length>>,
) -> Option<f32> {
    let length = length.into()?;
    let px_per_unit = match length.unit {
        LengthUnit::IN => PX_PER_INCH,
        LengthUnit::CM => PX_PER_CM,
        LengthUnit::MM => PX_PER_MM,
        LengthUnit::PT => PX_PER_POINT,
        LengthUnit::PC => PX_PER_PICA,
        LengthUnit::Number
        | LengthUnit::PX
        | LengthUnit::EMS
        | LengthUnit::EXS
        | LengthUnit::Percentage
        | LengthUnit::Unknown => return None,
    };
    Some(length.value * px_per_unit)
}

/// Rewrites the named attributes of one node to `px` where they are stated in
/// an absolute unit, and leaves every other unit alone.
///
/// Each attribute is named twice because the accessors are generated by
/// `skia_svg_macros::attrs!` as a `x` / `set_x` pair, and a macro cannot build
/// the second name from the first without a crate for it.
macro_rules! absolute_lengths_to_px {
    ($node:ident $(, $get:ident => $set:ident)+ $(,)?) => {{
        $(
            if let Some(px) = absolute_length_px($node.$get()) {
                $node.$set(Length::new(px, LengthUnit::PX));
            }
        )+
    }};
}

/// Rewrites every absolute length in the document to its value in `px`.
///
/// The root is handled by [`derive_intrinsic_size`], which has to resolve it
/// anyway to report a size. This is everything below the root, and it exists
/// for the same reason: Skia resolves `in`, `cm`, `mm`, `pt` and `pc` against
/// SVG 1.1's 90 dpi, so `<rect width="1in"/>` covered 90 pixels where a
/// browser gives it 96. Rewriting each such length to the `px` both sides
/// agree about settles it before Skia resolves anything, which is why no dpi
/// argument is needed -- `SkSVGDOM::render` builds its own length context and
/// skia-safe exposes no way to influence it, but by the time it does there is
/// no absolute unit left for it to get wrong.
///
/// A `viewBox` does not change the answer and does not need to be accounted
/// for here. Measured in Chrome, `1in` inside `viewBox="0 0 48 48"` on a
/// 96-pixel root reports `getBBox().width` of 96 -- the length resolves to
/// user units first and the viewBox transform then scales it like any other
/// coordinate. So the two rewrites compose by construction: this one puts the
/// right number of user units in the document, and the transform is applied
/// to it afterwards by machinery that never sees a unit.
///
/// Text positioning is the one thing this cannot reach, and it is a gap in the
/// bindings rather than in Skia. `SkSVGTextContainer` declares `x`, `y`, `dx`
/// and `dy` with Skia's own `SVG_ATTR` macro, which generates a setter
/// alongside the getter, so `setX` exists in C++. skia-bindings exposes only
/// the read side -- `C_SkSVGTextContainer_getX` and friends, with
/// `setXmlSpace` the sole `set` symbol for the class -- and skia-safe wraps
/// those by hand rather than through `attrs!`.
///
/// Those four attributes on those three elements are reached instead by
/// [`text_position_lengths_in_px`], which rewrites them in the document text
/// before Skia parses it. That is a workaround for this gap and exists only
/// because of it.
///
/// UPSTREAM: skia-safe 0.153.3 -- #179 -- worked around
/// Re-check: grep for `C_SkSVGTextContainer_setX` in skia-bindings. When it
/// is there, `text_position_lengths_in_px` and its tests can be deleted and
/// these four attributes handled in the match below like every other length.
/// Skia's own `SVG_ATTR(X, ...)` in `modules/svg/include/SkSVGText.h` already
/// generates the setter, so the shim and an `attrs!` block are the whole of
/// what is missing.
fn normalize_absolute_lengths(node: TypedNode) {
    // `stroke-width` is declared on `SkSVGNode`, so it is an attribute of
    // every variant below and is taken once here rather than in each arm.
    // The clone is a reference-count bump on the same node, which is what
    // makes writing through a handle from `children_typed` land on the
    // document that will be rendered.
    let mut shared = node.clone().into_node();
    absolute_lengths_to_px!(shared, stroke_width => set_stroke_width);

    // `font-size` is a length wearing a different type, and Skia resolves it
    // through the same length context as any other -- `SkSVGText.cpp` passes
    // it to `SkSVGLengthContext::resolve` -- so `font-size="0.5in"` set text
    // at 45 pixels where a browser sets it at 48. It does not carry `em` and
    // `ex` along with it: see `absolute_length_px` on why those are zero
    // either way.
    let font_size_px = shared
        .font_size()
        .and_then(|size| size.size())
        .and_then(absolute_length_px);
    if let Some(px) = font_size_px {
        shared.set_font_size(FontSize::new(Length::new(px, LengthUnit::PX)));
    }

    match node {
        TypedNode::Circle(mut n) => absolute_lengths_to_px!(
            n,
            cx => set_cx,
            cy => set_cy,
            r => set_r,
        ),
        TypedNode::Ellipse(mut n) => absolute_lengths_to_px!(
            n,
            cx => set_cx,
            cy => set_cy,
            rx => set_rx,
            ry => set_ry,
        ),
        TypedNode::Line(mut n) => absolute_lengths_to_px!(
            n,
            x1 => set_x1,
            y1 => set_y1,
            x2 => set_x2,
            y2 => set_y2,
        ),
        TypedNode::Rect(mut n) => absolute_lengths_to_px!(
            n,
            x => set_x,
            y => set_y,
            width => set_width,
            height => set_height,
            rx => set_rx,
            ry => set_ry,
        ),
        TypedNode::Use(mut n) => absolute_lengths_to_px!(
            n,
            x => set_x,
            y => set_y,
        ),

        // A nested `<svg>` states its own viewport in the same four
        // attributes the root does, and unlike the root it is not resolved
        // by `derive_intrinsic_size`.
        TypedNode::Svg(mut n) => {
            absolute_lengths_to_px!(
                n,
                x => set_x,
                y => set_y,
                width => set_width,
                height => set_height,
            );
            descend(&n);
        }
        // No `descend`, and not because `<image>` has no element children --
        // though it has none. skia-safe declares `SkSVGImage`'s base as
        // `SkSVGContainer` where Skia derives it from
        // `SkSVGTransformableNode`, so `children()` reads a vector that is
        // not there and the process segfaults. Reaching it through `Deref`
        // compiles and the crash is at run time.
        TypedNode::Image(mut n) => absolute_lengths_to_px!(
            n,
            x => set_x,
            y => set_y,
            width => set_width,
            height => set_height,
        ),
        TypedNode::Pattern(mut n) => {
            absolute_lengths_to_px!(
                n,
                x => set_x,
                y => set_y,
                width => set_width,
                height => set_height,
            );
            descend(&n);
        }
        TypedNode::Mask(mut n) => {
            absolute_lengths_to_px!(
                n,
                x => set_x,
                y => set_y,
                width => set_width,
                height => set_height,
            );
            descend(&n);
        }
        TypedNode::Filter(mut n) => {
            absolute_lengths_to_px!(
                n,
                x => set_x,
                y => set_y,
                width => set_width,
                height => set_height,
            );
            descend(&n);
        }
        TypedNode::LinearGradient(mut n) => {
            absolute_lengths_to_px!(
                n,
                x1 => set_x1,
                y1 => set_y1,
                x2 => set_x2,
                y2 => set_y2,
            );
            descend(&n);
        }
        TypedNode::RadialGradient(mut n) => {
            absolute_lengths_to_px!(
                n,
                cx => set_cx,
                cy => set_cy,
                r => set_r,
                fx => set_fx,
                fy => set_fy,
            );
            descend(&n);
        }
        TypedNode::TextPath(mut n) => absolute_lengths_to_px!(
            n,
            start_offset => set_start_offset,
        ),

        // `offset` on a gradient stop is a number or a percentage and never a
        // length, so there is nothing here for this to convert. The arm
        // exists to descend, and to record that the omission is deliberate:
        // `Stop::set_offset` would accept an absolute unit that the document
        // could not have stated.
        TypedNode::Stop(n) => descend(&n),

        // The filter primitives declare `x`, `y`, `width` and `height` once
        // on `SkSVGFe`, which each of them derefs to.
        TypedNode::FeBlend(n) => fe_subregion_to_px(&n),
        TypedNode::FeColorMatrix(n) => fe_subregion_to_px(&n),
        TypedNode::FeComponentTransfer(n) => fe_subregion_to_px(&n),
        TypedNode::FeComposite(n) => fe_subregion_to_px(&n),
        TypedNode::FeDiffuseLighting(n) => fe_subregion_to_px(&n),
        TypedNode::FeDisplacementMap(n) => fe_subregion_to_px(&n),
        TypedNode::FeFlood(n) => fe_subregion_to_px(&n),
        TypedNode::FeFuncA(n) => fe_subregion_to_px(&n),
        TypedNode::FeFuncR(n) => fe_subregion_to_px(&n),
        TypedNode::FeFuncG(n) => fe_subregion_to_px(&n),
        TypedNode::FeFuncB(n) => fe_subregion_to_px(&n),
        TypedNode::FeGaussianBlur(n) => fe_subregion_to_px(&n),
        TypedNode::FeImage(n) => fe_subregion_to_px(&n),
        TypedNode::FeMerge(n) => fe_subregion_to_px(&n),
        TypedNode::FeMorphology(n) => fe_subregion_to_px(&n),
        TypedNode::FeOffset(n) => fe_subregion_to_px(&n),
        TypedNode::FeSpecularLighting(n) => fe_subregion_to_px(&n),
        TypedNode::FeTurbulence(n) => fe_subregion_to_px(&n),

        // The light sources and `<feMergeNode>` derive from the container
        // rather than from `SkSVGFe`, so they carry no subregion.
        TypedNode::FeDistantLight(n) => descend(&n),
        TypedNode::FePointLight(n) => descend(&n),
        TypedNode::FeSpotLight(n) => descend(&n),
        TypedNode::FeMergeNode(n) => descend(&n),

        // Containers with no length of their own.
        TypedNode::ClipPath(n) => descend(&n),
        TypedNode::Defs(n) => descend(&n),
        TypedNode::G(n) => descend(&n),

        // Leaves. A path's geometry and a polygon's points are sequences of
        // user-unit coordinates, which take no unit at all, and a text
        // literal is a string.
        //
        // `Text` and `TSpan` are here rather than among the containers
        // because they must not be descended into: see the note on
        // `descend`. They carry nothing this could write in any case -- `x`,
        // `y`, `dx` and `dy` on a text container have no setter.
        TypedNode::Path(_)
        | TypedNode::Polygon(_)
        | TypedNode::Polyline(_)
        | TypedNode::Text(_)
        | TypedNode::TSpan(_)
        | TypedNode::TextLiteral(_) => {}
    }
}

/// Rewrites a filter primitive's subregion and then its children.
///
/// Split out because `x`, `y`, `width` and `height` are declared on `SkSVGFe`
/// and reached through `Deref`, so one body serves all eighteen primitives
/// that derive from it.
fn fe_subregion_to_px(node: &svg::fe::Fe) {
    let mut fe = node.clone();
    absolute_lengths_to_px!(
        fe,
        x => set_x,
        y => set_y,
        width => set_width,
        height => set_height,
    );
    descend(node);
}

/// Applies [`normalize_absolute_lengths`] to each child of a container.
///
/// Only call this for a node that really is a `SkSVGContainer` in Skia. Two of
/// skia-safe's `NodeSubtype` declarations say `SkSVGContainer` where the C++
/// class does not derive from it -- `SkSVGImage`, which derives from
/// `SkSVGTransformableNode`, and `SkSVGTextContainer`, which derives from
/// `SkSVGTextFragment` -- so `Deref` hands out a `Container` view of an object
/// that has no child vector at that offset. Both compile. `<image>` segfaults
/// and `<text>` trips skia-safe's own null assertion, which is how the two
/// were found. Every other variant reaches `SkSVGContainer` for real, most of
/// them through `SkSVGHiddenContainer`.
///
/// Two independent reasons agree on which nodes are leaves here, which is
/// worth more than either alone: the `Deref` chain makes `children()` reachable
/// on every variant except the six `Shape` subtypes, `Use` and `TextLiteral`,
/// and SVG's own content model gives none of those eight element children.
///
/// UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
/// Re-check: cargo test every_element_kind_survives_the_length_rewrite with
/// the `Image`, `Text` and `TSpan` arms of `normalize_absolute_lengths`
/// changed to call `descend`. Running it unchanged passes whatever skia-safe
/// declares, so the change is the check. The test binary dies today -- with
/// SIGSEGV on macOS -- and a crash of either kind is this defect rather than
/// a new one. The declarations are in skia-safe's `modules/svg/image.rs` and
/// `text.rs`; the C++ they should match is in skia-bindings'
/// `skia/modules/svg/include`.
fn descend(container: &svg::Container) {
    container
        .children_typed()
        .into_iter()
        .for_each(normalize_absolute_lengths);
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

/// Works out how big an SVG wants to be, mirroring Chrome, and normalises the
/// document to that answer.
///
/// Returns the size and whether it had to be invented. **The `dom` is left
/// modified**: a `width` or `height` the root states is rewritten in `px`, so
/// that what Skia lays the document out against is the size returned here
/// rather than its own reading of the same attribute. `&mut` says this may
/// mutate; the section below on why only the root is converted here says what
/// it mutates and why.
///
/// Every length the document states is converted by [`svg_length_px`], so
/// `10cm` and `10em` are read as readily as `10`. What is left over is the
/// genuinely undetermined case -- both dimensions a percentage, which is also
/// how Skia reports an `<svg>` carrying neither attribute -- and there the
/// `viewBox` aspect ratio is contained in the default object size described
/// on [`DEFAULT_SVG_WIDTH`]. A document stating no usable ratio, including
/// one whose `viewBox` has a zero side, takes that size unchanged.
///
/// One dimension stated and the other left to itself takes the missing one
/// from the ratio, and from the default object size where the document states
/// no ratio. It squared the stated dimension until this was written -- a rule
/// of this crate's own that no clause names, and that a browser does not
/// follow: `width="100"` on a 4:1 document is 100 by 25 in Chrome and was 100
/// square here. The squaring was left in place by the change that taught this
/// function to read `em` and `cm`, on the ground that that change was about
/// which lengths are read rather than what an under-specified document
/// resolves to. This one is about the latter, so it is in scope here.
///
/// # Only the root is converted here
///
/// This function converts the root's own `width` and `height`, because it has
/// to resolve them to report a size at all. Every length *inside* the document
/// is converted by [`normalize_absolute_lengths`], which runs immediately
/// after this and for the same reason: Skia resolves an absolute unit through
/// a `SkSVGLengthContext` built with no dpi argument -- `SkSVGDOM::render` and
/// `SkSVGDOM::renderNode` each build their own, and the constructor builds a
/// third for `fContainerSize` -- so all of them keep the 90 that
/// [`PX_PER_INCH`] describes.
///
/// That dpi is unreachable and rewriting the lengths does not need it. Skia
/// gets a document stating `px`, which is the one unit both sides read the
/// same way, so what its length context would have done with an inch never
/// arises. The split between the two functions is about which lengths each
/// one already has in hand, not about which are fixable.
fn derive_intrinsic_size(dom: &mut svg::Dom) -> (Size, bool) {
    let root = dom.root();
    let px_per_em = Some(root_px_per_em(&root));
    let width = root.width();
    let height = root.height();

    // What the two sides make of the root's own lengths, resolved once and
    // written back, so the document lays out against the size that is
    // reported for it. Skia converts an absolute unit against SVG 1.1's 90
    // dpi where CSS fixes 96, so `width="1in"` measured 96 here and laid
    // out as 90 there: a child at `100%` covered 90 of the 96 the image
    // claimed. Rewriting the root in `px` -- the one unit both sides read
    // the same way -- settles it before Skia resolves anything.
    //
    // Only the axes the document itself states are rewritten. A dimension
    // left to the default object size or derived from the `viewBox` is this
    // crate's answer to a question the document did not ask, and writing it
    // into the document would make that answer bind on the descendants too.
    //
    // This reaches the root. Everything below it is rewritten the same way
    // by `normalize_absolute_lengths`, which runs once the size is derived.
    let stated_width = svg_length_px(width, px_per_em);
    let stated_height = svg_length_px(height, px_per_em);
    // `is_auto` below still reads the originals, so both are copied out
    // before the root is borrowed again to write to.
    let (width, height) = (*width, *height);
    let mut root = dom.root();
    if let Some(px) = stated_width {
        root.set_width(Length::new(px, LengthUnit::PX));
    }
    if let Some(px) = stated_height {
        root.set_height(Length::new(px, LengthUnit::PX));
    }

    // The ratio the document states, where it states a usable one. A
    // `viewBox` with a zero or negative side states none, and dividing by it
    // gave an infinite, zero or NaN width -- `viewBox="0 0 40 0"` sized a
    // document `Infinity` by 150, and `viewBox="0 0 0 0"` sized it `NaN`.
    let aspect = root
        .view_box()
        .map(|view_box| view_box.width() / view_box.height())
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0);

    let derived = match (stated_width, stated_height) {
        (Some(width), Some(height)) => {
            return (Size::new(width, height), false);
        }
        // One dimension stated and the other left to itself: the ratio
        // supplies what is missing, and without a ratio the default object
        // size does. Squaring the stated dimension was this crate's alone --
        // no clause names it, and a browser derives from the ratio, so
        // `width="100"` on a 4:1 document is 100 by 25 rather than 100 square.
        (None, Some(height)) if is_auto(&width) => Size::new(
            aspect.map_or(DEFAULT_SVG_WIDTH, |ratio| height * ratio),
            height,
        ),
        (Some(width), None) if is_auto(&height) => Size::new(
            width,
            aspect.map_or(DEFAULT_SVG_HEIGHT, |ratio| width / ratio),
        ),
        // Contained in the default object size rather than hung from its
        // height, so whichever bound the ratio reaches first is the one that
        // binds. Without a usable ratio there is nothing to contain and the
        // default object size stands as it is.
        _ => match aspect {
            Some(ratio) if ratio > DEFAULT_SVG_WIDTH / DEFAULT_SVG_HEIGHT => {
                Size::new(DEFAULT_SVG_WIDTH, DEFAULT_SVG_WIDTH / ratio)
            }
            Some(ratio) => {
                Size::new(DEFAULT_SVG_HEIGHT * ratio, DEFAULT_SVG_HEIGHT)
            }
            None => Size::new(DEFAULT_SVG_WIDTH, DEFAULT_SVG_HEIGHT),
        },
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

    /// The ways a document can decline to state its own size, and the one
    /// where it states it plainly.
    ///
    /// Asserted against Chrome's replaced-element rules rather than against
    /// `derive_intrinsic_size` restating itself: an undimensioned `<svg>` is
    /// its `viewBox` ratio contained in the 300-by-150 default object size,
    /// which is what a browser does with the same markup.
    ///
    /// The 2:1 row below is deliberately not the only ratio here. It is the
    /// one aspect at which containing the ratio and hanging it from the
    /// height agree, so a test carrying only that row passes under either
    /// rule -- which is how this one asserted a bare document was 150 square,
    /// under a name claiming Chrome parity, while a browser gave 300 by 150.
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

        // A 2:1 viewBox reaches both bounds at once.
        let boxed = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(boxed.intrinsic_size(), Size::new(300.0, 150.0));
        assert!(boxed.is_autosized(), "no declared size means autosized");

        // Wider than 2:1, so the width binds and the height follows. Hung
        // from the height this was 600 wide.
        let wide = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 10"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(wide.intrinsic_size(), Size::new(300.0, 75.0));

        // Taller than 2:1, so the height binds -- the case the old rule got
        // right, kept so a fix in the other direction would be caught.
        let tall = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 40"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(tall.intrinsic_size(), Size::new(37.5, 150.0));

        // No ratio at all: the default object size stands.
        let bare = Svg::parse(r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#)
            .expect("valid SVG");
        assert_eq!(bare.intrinsic_size(), Size::new(300.0, 150.0));
        assert!(bare.is_autosized());
    }

    /// A `viewBox` with a zero side states no usable ratio.
    ///
    /// Dividing by it gave `Infinity`, `0` or `NaN` for the width, which
    /// reached `Size` and every caller sizing a surface from it.
    #[test]
    fn a_degenerate_view_box_falls_back_rather_than_dividing_by_zero() {
        for view_box in ["0 0 40 0", "0 0 0 40", "0 0 0 0", "0 0 -40 10"] {
            let svg = Svg::parse(&format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}"/>"#
            ))
            .expect("valid SVG");
            let size = svg.intrinsic_size();
            assert!(
                size.width.is_finite() && size.height.is_finite(),
                "viewBox=\"{view_box}\" gave {size:?}"
            );
            assert_eq!(size, Size::new(300.0, 150.0), "viewBox=\"{view_box}\"");
        }
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

        // `ex` is the drawn face's x-height rather than half an em, so the
        // expectation is computed from that face and not pinned: which face
        // it is depends on the machine.
        let ex = sized("ex");
        let expected = 10.0 * 16.0 * fallback_ex_ratio();
        let Size { width, height } = ex.intrinsic_size();
        assert!(
            (width - expected).abs() < 1e-3 && (height - expected).abs() < 1e-3,
            "10ex of the initial 16 is {expected}; got {width}x{height}"
        );
        assert_ne!(
            expected, 80.0,
            "and it is not half an em, or this asserts nothing"
        );
        assert!(!ex.is_autosized());
    }

    /// The x-height ratio of the face a document with no family is drawn
    /// with, read straight from the font manager.
    ///
    /// Derived here rather than through `ex_ratio_for` so the expectations
    /// below are not the implementation restating itself, and computed rather
    /// than pinned because which face this is depends on the machine.
    fn fallback_ex_ratio() -> f32 {
        let typeface = FontMgr::new()
            .legacy_make_typeface(None, FontStyle::normal())
            .expect("a machine with no fonts at all cannot draw text");
        let (_, metrics) = Font::from_typeface(typeface, 100.0).metrics();
        metrics.x_height / 100.0
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
        // 1cm is 37.795 px, and two of the face's x-heights of that is what
        // `2ex` comes to -- not one cm, which is what half an em would give.
        let in_cm = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="2ex" height="2ex" font-size="1cm"/>"#,
        )
        .expect("valid SVG");
        let Size { width, height } = in_cm.intrinsic_size();
        let one_cm = 96.0 / 2.54;
        let expected = 2.0 * one_cm * fallback_ex_ratio();
        assert!(
            (width - expected).abs() < 1e-3 && (height - expected).abs() < 1e-3,
            "2ex of a 1cm em is {expected}; got {width}x{height}"
        );
        assert!(
            (expected - one_cm).abs() > 1e-3,
            "and it is not one cm, which is what half an em would have given"
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
    fn a_font_size_keyword_falls_back_where_a_percentage_resolves() {
        let sized = |font_size: &str| {
            let svg = Svg::parse(&format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="10em" font-size="{font_size}"/>"#
            ))
            .expect("valid SVG");
            assert!(
                !svg.is_autosized(),
                "font-size=\"{font_size}\": the document still stated a size"
            );
            svg.intrinsic_size()
        };

        // A CSS keyword is not a length and there is nothing to resolve it
        // against, so the em keeps the initial value.
        for font_size in ["larger", "inherit", "medium"] {
            assert_eq!(
                sized(font_size),
                Size::new(160.0, 160.0),
                "font-size=\"{font_size}\" carries no length, so the em falls back"
            );
        }

        // A percentage does resolve: it is that fraction of what the element
        // inherits, which for a root is the initial 16. Chrome reports 240 by
        // 240 and a computed font-size of 24px for the same document.
        assert_eq!(
            sized("150%"),
            Size::new(240.0, 240.0),
            "a percentage font-size is a fraction of the inherited size"
        );
    }

    /// A percentage is still the length that cannot be resolved.
    ///
    /// It is relative to a containing block, and a document being measured
    /// before it is placed has none -- so this is the case the fallback
    /// exists for, and the one unit conversion does not reach.
    #[test]
    fn a_percentage_is_the_length_that_stays_unresolved() {
        // Neither dimension resolves and there is no `viewBox` to supply a
        // ratio, so the default object size stands unchanged. This asserted a
        // 150 square while the fallback hung everything from the height.
        let half = sized("%");
        assert_eq!(half.intrinsic_size(), Size::new(300.0, 150.0));
        assert!(half.is_autosized());

        // 100% specifically, which is also how Skia reports an absent
        // attribute. The stated dimension is read and converted; the missing
        // one comes from the default object size, there being no `viewBox`
        // ratio to take it from. This squared the stated dimension before.
        let one_sided = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="2cm"/>"#,
        )
        .expect("valid SVG");
        let Size { width, height } = one_sided.intrinsic_size();
        let two_cm = 2.0 * 96.0 / 2.54;
        assert!(
            (width - DEFAULT_SVG_WIDTH).abs() < 1e-3
                && (height - two_cm).abs() < 1e-3,
            "the stated dimension converts and the other is invented: \
             got {width}x{height}"
        );
        assert!(one_sided.is_autosized(), "one dimension was invented");

        // With a ratio the missing dimension comes from that instead.
        let ratioed = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" viewBox="0 0 40 10"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(ratioed.intrinsic_size(), Size::new(100.0, 25.0));
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

    /// The width and height of the painted region of a rasterization, in
    /// pixels.
    ///
    /// Measured from the pixels rather than asked of the DOM, because what is
    /// in question is the size Skia lays the document out against, which is
    /// not the size it reports and not the size the surface was allocated at.
    fn painted_extent(svg: &mut Svg, side: u32) -> (u32, u32) {
        let image = svg.rasterize(side, side).expect("rasterizes");
        let info = ImageInfo::new(
            (side as i32, side as i32),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            ColorSpace::new_srgb(),
        );
        let mut pixels = vec![0u8; (side * side * 4) as usize];
        assert!(
            image.inner.read_pixels(
                &info,
                &mut pixels,
                (side * 4) as usize,
                (0, 0),
                skia_safe::image::CachingHint::Allow,
            ),
            "the surface reads back"
        );
        let opaque =
            |x: u32, y: u32| pixels[((y * side + x) * 4 + 3) as usize] > 0;
        let width = (0..side).filter(|&x| opaque(x, 0)).count() as u32;
        let height = (0..side).filter(|&y| opaque(0, y)).count() as u32;
        (width, height)
    }

    /// Every element kind survives the walk that rewrites absolute lengths.
    ///
    /// A guard against skia-safe's node hierarchy, not against arithmetic.
    /// `NodeSubtype` declares each node's base, the walk reaches
    /// `children()` through the `Deref` that declaration sets up, and two of
    /// those declarations name `SkSVGContainer` for a class that does not
    /// derive from it. Both compiled. `<image>` segfaulted and `<text>` tripped
    /// skia-safe's own null-pointer assertion, and neither is a failure any
    /// other test in this file can produce -- a wrong length shows up as a
    /// wrong pixel, but a wrong base shows up as a dead process.
    ///
    /// So this asserts almost nothing and is worth keeping anyway: it parses
    /// one document per element kind, and a crash is the failure. The
    /// assertion at the end is there to make an empty or skipped run
    /// distinguishable from a passing one.
    #[test]
    fn every_element_kind_survives_the_length_rewrite() {
        let bodies = [
            r##"<rect width="1in" height="1in"/>"##,
            r##"<circle cx="1in" cy="1in" r="1cm"/>"##,
            r##"<ellipse cx="1in" cy="1in" rx="1cm" ry="1mm"/>"##,
            r##"<line x1="1in" y1="1in" x2="1pt" y2="1pc"/>"##,
            r##"<g><rect width="1in" height="1in"/></g>"##,
            r##"<defs><rect id="a" width="1in" height="1in"/></defs>"##,
            r##"<defs><rect id="a" width="1in" height="1in"/></defs><use href="#a" x="1in"/>"##,
            r##"<image width="1in" height="1in" href="data:image/gif;base64,R0lGODlhAQABAAAAACw="/>"##,
            r##"<text x="1in" y="1in">hi</text>"##,
            r##"<defs><path id="p" d="M0 0 L10 10"/></defs><text><textPath href="#p" startOffset="1in">hi</textPath></text>"##,
            r##"<defs><linearGradient id="g" x1="1in"><stop offset="0" stop-color="#000"/></linearGradient></defs><rect width="10" height="10" fill="url(#g)"/>"##,
            r##"<defs><radialGradient id="g" cx="1in"><stop offset="0" stop-color="#000"/></radialGradient></defs><rect width="10" height="10" fill="url(#g)"/>"##,
            r##"<defs><pattern id="p" width="1in" height="1in"><rect width="2" height="2"/></pattern></defs><rect width="10" height="10" fill="url(#p)"/>"##,
            r##"<defs><mask id="m" width="1in"><rect width="10" height="10" fill="#fff"/></mask></defs><rect width="10" height="10" mask="url(#m)"/>"##,
            r##"<defs><clipPath id="c"><rect width="1in" height="1in"/></clipPath></defs><rect width="10" height="10" clip-path="url(#c)"/>"##,
            r##"<defs><filter id="f" x="1in"><feGaussianBlur stdDeviation="1"/></filter></defs><rect width="10" height="10" filter="url(#f)"/>"##,
            r##"<defs><filter id="f"><feMerge><feMergeNode/></feMerge></filter></defs><rect width="10" height="10" filter="url(#f)"/>"##,
            r##"<defs><filter id="f"><feDiffuseLighting><fePointLight x="1" y="1" z="1"/></feDiffuseLighting></filter></defs><rect width="10" height="10" filter="url(#f)"/>"##,
            r##"<svg width="1in" height="1in"><rect width="1in" height="1in"/></svg>"##,
            r##"<path d="M0 0 L10 10" stroke-width="1in"/>"##,
            r##"<polygon points="0,0 10,0 10,10"/>"##,
        ];

        let parsed = bodies
            .iter()
            .filter(|body| {
                let xml = format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20">{body}</svg>"##
                );
                Svg::parse(&xml).is_ok()
            })
            .count();
        assert_eq!(
            parsed,
            bodies.len(),
            "every document above has to parse, or the walk never saw the \
             element it was written for"
        );
    }

    /// A document whose own length says one thing to us and another to Skia
    /// paints at our size, not Skia's.
    ///
    /// `1in` is 96 CSS pixels and 90 of Skia's, so a child at `100%` used to
    /// cover 90 of the 96 the image claimed -- short by exactly the ratio
    /// between the two dpi. The control is the same document in `px`, which
    /// both sides already agreed about and which must not move.
    #[test]
    fn a_document_in_physical_units_paints_the_size_it_reports() {
        // `r##` rather than `r#`: the fill colour contains `"#`, which ends a
        // single-hash raw string.
        let filled = |size: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}"><rect width="100%" height="100%" fill="#d11"/></svg>"##
            )
        };

        let mut inches = Svg::parse(&filled("1in")).expect("valid SVG");
        assert_eq!(
            inches.intrinsic_size(),
            Size::new(96.0, 96.0),
            "an inch is 96 CSS pixels"
        );
        assert_eq!(
            painted_extent(&mut inches, 96),
            (96, 96),
            "and the paint has to reach the size that was reported"
        );

        let mut pixels = Svg::parse(&filled("96px")).expect("valid SVG");
        assert_eq!(
            painted_extent(&mut pixels, 96),
            (96, 96),
            "the control: a document already in px was never short"
        );
    }

    /// An absolute length *inside* the document resolves at 96 dpi as well.
    ///
    /// The root was settled first, and left this behind: Skia resolves a
    /// descendant's `in`, `cm`, `mm`, `pt` or `pc` against SVG 1.1's 90 dpi, so
    /// `<rect width="1in"/>` covered 90 pixels where Chrome gives it 96. Both
    /// rows below measured 90 before the rewrite that fixes them.
    ///
    /// The two controls are the units that must not move. A percentage
    /// resolves against a reference that depends on where the node sits, and
    /// a bare number is a user unit, which carries no dpi to get wrong; both
    /// were already right and a rewrite that touched them would be a
    /// regression rather than a fix.
    #[test]
    fn a_descendant_in_physical_units_resolves_at_css_dpi() {
        let doc = |root: &str, child: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{root}" height="{root}"><rect width="{child}" height="{child}" fill="#d11"/></svg>"##
            )
        };
        let extent = |xml: String| {
            painted_extent(&mut Svg::parse(&xml).expect("valid SVG"), 200)
        };

        assert_eq!(
            extent(doc("96px", "1in")),
            (96, 96),
            "a child in inches covers 96 pixels, whatever the root says"
        );
        assert_eq!(
            extent(doc("1in", "1in")),
            (96, 96),
            "and still does when the root is stated in inches too"
        );

        assert_eq!(
            extent(doc("96px", "100%")),
            (96, 96),
            "the control: a percentage is not an absolute unit and must not \
             be rewritten"
        );
        assert_eq!(
            extent(doc("96px", "48")),
            (48, 48),
            "the control: a user unit carries no dpi and must not be \
             rewritten"
        );
    }

    /// `ex` is the face's real x-height, not half an em.
    ///
    /// CSS defines `ex` as the x-height and browsers use the true one: Chrome
    /// renders `4ex` at `font-size="20"` as 35.898 rather than 40, a ratio of
    /// 0.449. Half an em would be 11% over on that face alone, and the ratio
    /// varies between faces by more than that.
    ///
    /// The assertion is that *some* family on the machine has a real
    /// x-height, rather than that a named one does: which families exist is a
    /// property of the box, and a test naming `Helvetica` passes here and
    /// says nothing on a runner without it.
    #[test]
    fn an_ex_is_the_faces_x_height_rather_than_half_an_em() {
        let font_mgr = FontMgr::new();

        // A family the machine does not have still measures a real face --
        // the one the font manager returns for a null family, which is what
        // Skia draws with. Half an em would be a number attached to no
        // rendering, and none of the faces here has a ratio of 0.5, so this
        // discriminates.
        let unresolvable =
            ex_ratio_for(Some("ZzzNoSuchFamilyAnywhere"), &font_mgr);
        assert_ne!(
            unresolvable, EX_PER_EM,
            "an unresolvable family takes the drawn face's x-height, not a \
             constant"
        );
        assert_eq!(
            ex_ratio_for(None, &font_mgr),
            unresolvable,
            "and naming no family at all resolves to that same face"
        );
        assert!(
            unresolvable > 0.0 && unresolvable < 1.5,
            "which is a fraction of the em: {unresolvable}"
        );

        let measured: Vec<f32> = font_mgr
            .family_names()
            .map(|family| ex_ratio_for(Some(&family), &font_mgr))
            .collect();
        assert!(
            !measured.is_empty(),
            "the machine has to have some fonts, or nothing below means \
             anything"
        );
        assert!(
            measured.iter().any(|ratio| *ratio != EX_PER_EM),
            "at least one family has a real x-height, or this is reading the \
             fallback for every face and the lookup does nothing"
        );
        // A wide band on purpose. Display faces reach past 0.9 and this
        // machine has one, so a tight range would be asserting a property of
        // the font set rather than of the arithmetic. What it catches is the
        // failure that matters: a ratio read in font units rather than
        // divided back out lands near 1000, not near 1.
        assert!(
            measured.iter().all(|ratio| *ratio > 0.0 && *ratio < 1.5),
            "every ratio is a fraction of the em rather than a raw metric: \
             {measured:?}"
        );
    }

    /// A generic family name is replaced by the family it resolves to.
    ///
    /// The system font manager is asked before this library's registered
    /// faces, because a family it cannot resolve takes the process down. That
    /// costs the curated stacks on any system that answers a generic name
    /// itself -- Linux answers `sans-serif`, `serif` and `monospace` -- so
    /// the document is made to ask for the concrete family instead, which no
    /// system manager claims.
    #[test]
    fn a_generic_family_is_replaced_by_the_one_it_resolves_to() {
        let generics = [
            ("sans-serif".to_string(), "Liberation Sans".to_string()),
            ("serif".to_string(), "Tinos".to_string()),
        ];

        assert_eq!(
            family_substitution("sans-serif", &generics, &[]),
            Some("Liberation Sans"),
            "a generic takes the family its stack picked"
        );
        assert_eq!(
            family_substitution("  serif  ", &generics, &[]),
            Some("Tinos"),
            "surrounding space is not part of the name"
        );
        assert_eq!(
            family_substitution("\"sans-serif\"", &generics, &[]),
            Some("Liberation Sans"),
            "and neither are the quotes CSS allows around it"
        );
        assert_eq!(
            family_substitution("SANS-SERIF", &generics, &[]),
            Some("Liberation Sans"),
            "a family name is matched without regard to case"
        );

        assert_eq!(
            family_substitution("Helvetica", &generics, &[]),
            None,
            "a concrete family is the caller's choice and is left alone"
        );
        assert_eq!(
            family_substitution("monospace", &generics, &[]),
            None,
            "a generic this machine resolved nothing for is left alone \
             rather than guessed at"
        );
        assert_eq!(
            family_substitution("sans-serif", &[], &[]),
            None,
            "the control: with no mapping nothing is substituted, so the \
             crate's own door cannot be changed by this"
        );
    }

    /// A `style` that says nothing about the font still leaves the root
    /// stating no size.
    ///
    /// The root is given the CSS initial size where it states none, so that
    /// Skia's own initial of 24 does not apply while the lengths here were
    /// resolved against 16. Whether a size is stated is `stated_font_size`'s
    /// question -- attribute or declaration -- and asking instead whether a
    /// `style` attribute exists at all makes `style="fill:#d11"` suppress the
    /// injection. Measured through the rendering when that was the guard:
    /// glyph ink of 23 against the 16 a bare root gives, so an ordinary way
    /// of writing an SVG rendered its text half again too large.
    #[test]
    fn a_root_style_stating_no_font_size_still_takes_the_initial_size() {
        let rewritten = |root_attributes: &str| {
            let xml = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg"{root_attributes}><text x="1em">a</text></svg>"##
            );
            text_position_lengths_in_px(
                xml.as_bytes(),
                &[],
                &[],
                &FontMgr::new(),
            )
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        let states_the_initial_size = |rewrite: Option<String>| {
            rewrite.is_some_and(|text| {
                text.contains(&format!(
                    " font-size=\"{CSS_INITIAL_FONT_SIZE}\""
                ))
            })
        };

        assert!(
            states_the_initial_size(rewritten("")),
            "a bare root is given the initial size"
        );
        assert!(
            states_the_initial_size(rewritten(r##" style="fill:#d11""##)),
            "and so is one whose style declares something else entirely"
        );
        assert!(
            !states_the_initial_size(rewritten(r##" font-size="20""##)),
            "a root stating a size keeps it"
        );
        assert!(
            !states_the_initial_size(rewritten(r##" style="font-size:20""##)),
            "and so does one stating it in a declaration"
        );
    }

    /// A generic in a `style` declaration is substituted too.
    ///
    /// Skia reads `style="font-family:sans-serif"` exactly as it reads the
    /// attribute form -- measured byte-identical for both a concrete family
    /// and a generic -- so a pass that looked only at the attribute left the
    /// declaration form unmapped, and on a system whose own font manager
    /// answers the generic the curated stack lost.
    #[test]
    fn a_generic_in_a_style_declaration_is_substituted() {
        let generics =
            [("sans-serif".to_string(), "Liberation Sans".to_string())];
        let rewritten = |body: &str| {
            let xml = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40" font-size="16">{body}</svg>"##
            );
            text_position_lengths_in_px(
                xml.as_bytes(),
                &generics,
                &[],
                &FontMgr::new(),
            )
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };

        let styled =
            rewritten(r##"<text style="font-family:sans-serif">hi</text>"##)
                .expect("a generic in a declaration has to be rewritten");
        assert!(
            styled.contains("font-family:Liberation Sans"),
            "the declaration names the concrete family: {styled}"
        );

        assert!(
            rewritten(r##"<text style="fill:red;font-family:sans-serif;stroke:none">hi</text>"##)
                .expect("still rewritten among other declarations")
                .contains("fill:red;font-family:Liberation Sans;stroke:none"),
            "and the declarations around it are passed through as written"
        );

        assert_eq!(
            rewritten(r##"<text style="font-family:Georgia">hi</text>"##),
            None,
            "a concrete family in a declaration is left alone, like the \
             attribute form"
        );
    }

    /// A list of families is left exactly as written.
    ///
    /// `font-family="Foo, sans-serif"` means "Foo, and failing that a
    /// sans-serif". Rewriting the tail would change what the head falls back
    /// to while looking like a substitution, and picking one item would be a
    /// guess about which the author expected to win. Skia does not implement
    /// the fall-through here either way.
    #[test]
    fn a_list_of_families_is_not_substituted() {
        let generics =
            [("sans-serif".to_string(), "Liberation Sans".to_string())];

        for list in [
            "Foo, sans-serif",
            "sans-serif, Foo",
            "sans-serif,sans-serif",
        ] {
            assert_eq!(
                family_substitution(list, &generics, &[]),
                None,
                "a list is left alone: {list}"
            );
        }
    }

    /// A document naming no generic comes back byte-identical.
    ///
    /// The rewrite reads `font-family` on every element, since it inherits,
    /// so what it does to documents it cannot improve is what matters most.
    #[test]
    fn a_document_naming_no_generic_is_passed_through_untouched() {
        let generics =
            [("sans-serif".to_string(), "Liberation Sans".to_string())];
        let xml = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" font-size="16" font-family="Helvetica">
              <g font-family="Georgia"><text x="10" y="50">hello</text></g>
              <text x="10" y="80" font-family="Foo, sans-serif">list</text>
            </svg>"##;

        assert_eq!(
            text_position_lengths_in_px(
                xml.as_bytes(),
                &generics,
                &[],
                &FontMgr::new()
            ),
            None,
            "no generic stands alone anywhere, so nothing is rewritten"
        );

        let generic = xml.replace(
            r##"font-family="Georgia""##,
            r##"font-family="sans-serif""##,
        );
        let rewritten = text_position_lengths_in_px(
            generic.as_bytes(),
            &generics,
            &[],
            &FontMgr::new(),
        )
        .expect("the control: one bare generic has to make this fire");
        assert!(
            String::from_utf8_lossy(&rewritten)
                .contains(r##"font-family="Liberation Sans""##),
            "and the substitution is the family, not the generic"
        );
    }

    /// A `font-size` in an absolute unit sets text at the size CSS gives it.
    ///
    /// Skia resolves `font-size` through the same length context as any other
    /// length, so `font-size="0.5in"` set text at 45 pixels where a browser
    /// sets it at 48 -- the same six per cent, in the place it is hardest to
    /// see, because nothing about a paragraph says what size it was meant to
    /// be.
    ///
    /// Pinned as an identity rather than as a pixel count: `0.5in` and `48`
    /// are the same size in CSS, so the two documents must rasterize to the
    /// same bytes. An identity survives a change of font, of hinting or of
    /// platform, where a pinned width would have to be re-measured on each.
    /// The control is `45`, which is what Skia's own dpi gives for half an
    /// inch -- the comparison has to be able to tell those two apart, or
    /// equality with `48` would prove nothing.
    #[test]
    fn a_font_size_in_physical_units_sets_text_at_the_css_size() {
        let rendered = |size: &str| {
            let xml = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="200px" height="200px"><text x="10" y="100" font-size="{size}" fill="#d11">Wg</text></svg>"##
            );
            let mut svg = Svg::parse(&xml).expect("valid SVG");
            let image = svg.rasterize(200, 200).expect("rasterizes");
            let info = ImageInfo::new(
                (200, 200),
                ColorType::RGBA8888,
                AlphaType::Unpremul,
                ColorSpace::new_srgb(),
            );
            let mut pixels = vec![0u8; 200 * 200 * 4];
            assert!(
                image.inner.read_pixels(
                    &info,
                    &mut pixels,
                    200 * 4,
                    (0, 0),
                    skia_safe::image::CachingHint::Allow,
                ),
                "the surface reads back"
            );
            pixels
        };

        let half_an_inch = rendered("0.5in");
        assert!(
            half_an_inch.iter().skip(3).step_by(4).any(|&a| a > 0),
            "the text has to paint something, or every comparison below is \
             between two blank pages"
        );
        assert_eq!(
            half_an_inch,
            rendered("48"),
            "half an inch is 48 CSS pixels and has to set the same text"
        );
        assert_ne!(
            half_an_inch,
            rendered("45"),
            "the control: 45 is what Skia's 90 dpi gives for half an inch, \
             and this comparison has to be able to see the difference"
        );
    }

    /// A `Debug` dump with `SkPath::generation_id` blanked out.
    ///
    /// That field is a process-global counter incremented for each `SkPath`
    /// created, so two parses of the same document disagree there and nowhere
    /// else -- it says nothing about the document. Blanked rather than solved
    /// by dropping the `<path>` from the fixture: an element should not leave
    /// a test because it inconveniences the instrument.
    fn scrub_generation_ids(text: &str) -> String {
        const FIELD: &str = "generation_id: ";
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(FIELD) {
            let (head, tail) = rest.split_at(at + FIELD.len());
            out.push_str(head);
            out.push('_');
            rest = tail
                .find(|c: char| !c.is_ascii_digit())
                .map_or("", |end| &tail[end..]);
        }
        out.push_str(rest);
        out
    }

    /// A document with no absolute unit in it comes out byte-identical.
    ///
    /// The walk touches every node in the tree, so the strongest thing to
    /// assert about it is what it does *not* do. This compares the whole
    /// serialised DOM rather than a rendering: a rewrite that changed a unit
    /// tag without changing a value would paint the same pixels and still be
    /// wrong, and only the text shows it.
    ///
    /// No `<text>` in the document below, and not by choice. `Debug` on a DOM
    /// containing one panics inside skia-safe -- `Container::_dbg` formats its
    /// children through the same mis-declared base that stops `descend` from
    /// visiting them, and trips the null-pointer assertion in
    /// `from_non_null_sp_slice`. A document without one formats normally, so
    /// the omission is that upstream defect and not a gap in the fixture.
    /// Text carries nothing this walk can write in any case.
    #[test]
    fn a_document_in_relative_units_is_left_exactly_as_written() {
        let xml = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200" font-size="16">
              <g><rect x="10%" y="10%" width="50%" height="2em" stroke-width="0.5em" fill="#d11"/></g>
              <circle cx="100" cy="100" r="25%"/>
              <ellipse cx="50%" cy="50%" rx="50%" ry="10"/>
              <line x1="0" y1="0" x2="100%" y2="100%" stroke-width="1ex"/>
              <defs><linearGradient id="g" x1="0%" x2="100%"><stop offset="0.5" stop-color="#000"/></linearGradient></defs>
              <path d="M0 0 L10 10"/>
            </svg>"##;

        let dumped = |xml: &str| {
            let text =
                format!("{:?}", Svg::parse(xml).expect("valid SVG").dom.root());
            scrub_generation_ids(&text)
        };

        let before = dumped(xml);
        assert_eq!(
            before,
            dumped(xml),
            "the walk is deterministic, or nothing below means anything"
        );

        let absolute = xml.replace(r##"ry="10""##, r##"ry="1in""##);
        assert_ne!(
            dumped(&absolute),
            before,
            "the control: this comparison has to be able to see a rewrite, \
             and one absolute unit anywhere in the document must move it"
        );
    }

    /// The rewrite reaches a length at any depth, not just a child of the root.
    ///
    /// Separate from the test above because that one would pass on a rewrite
    /// that walked the root's children and stopped. Three groups deep, and a
    /// second rect in user units beside it that must come through untouched.
    #[test]
    fn a_physical_length_is_reached_through_nested_containers() {
        let xml = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200px" height="200px">
              <g><g><g><rect width="1in" height="1in" fill="#d11"/></g></g></g>
              <g><rect x="150" y="150" width="40" height="40" fill="#d11"/></g>
            </svg>"##;
        let mut svg = Svg::parse(xml).expect("valid SVG");
        assert_eq!(
            painted_extent(&mut svg, 200),
            (96, 96),
            "the buried inch resolves at 96 like any other"
        );

        let mut untouched = Svg::parse(xml).expect("valid SVG");
        let image = untouched.rasterize(200, 200).expect("rasterizes");
        let info = ImageInfo::new(
            (200, 200),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            ColorSpace::new_srgb(),
        );
        let mut pixels = vec![0u8; 200 * 200 * 4];
        assert!(
            image.inner.read_pixels(
                &info,
                &mut pixels,
                200 * 4,
                (0, 0),
                skia_safe::image::CachingHint::Allow,
            ),
            "the surface reads back"
        );
        let opaque = |x: usize, y: usize| pixels[(y * 200 + x) * 4 + 3] > 0;
        let width = (0..200).filter(|&x| opaque(x, 160)).count();
        assert_eq!(
            width, 40,
            "the control: the sibling in user units is still 40 wide"
        );
    }

    /// A `viewBox` scales an absolute length after it is resolved, so the two
    /// rewrites compose rather than multiplying.
    ///
    /// Worth its own test because the composition is not obvious and the
    /// plausible wrong answers bracket the right one. A 96-pixel root with
    /// `viewBox="0 0 48 48"` scales by two, and the inch inside it paints 192
    /// pixels: the length resolves to 96 *user units* first and the transform
    /// is applied to that. It measured 180 before this change -- 90 user
    /// units scaled by two, wrong in both factors.
    ///
    /// Chrome agrees, by a route that does not involve rasterizing anything:
    /// the same document inline reports `getBBox().width` of 96 for the rect
    /// at every viewBox scale tried -- 2, 4 and 0.5 -- with the on-screen
    /// width tracking the scale each time. So 96 user units is the resolution
    /// and the scaling is separate.
    #[test]
    fn a_view_box_scales_a_physical_length_after_resolving_it() {
        let xml = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1in" height="1in" viewBox="0 0 48 48"><rect width="1in" height="1in" fill="#d11"/></svg>"##;
        let mut svg = Svg::parse(xml).expect("valid SVG");
        assert_eq!(
            painted_extent(&mut svg, 200),
            (192, 192),
            "96 user units, scaled by the viewBox's factor of two"
        );
    }

    /// The ink of the first glyph, as the column range it covers.
    ///
    /// Text position is measured by where the glyphs land rather than by
    /// reading the DOM back, because the DOM is exactly what cannot be read
    /// here: skia-safe exposes no setter for these attributes and this fix
    /// works on the document text, so an assertion against the DOM would be
    /// asserting about the wrong artefact.
    fn painted_columns(xml: &str) -> Option<(u32, u32)> {
        let mut svg = Svg::parse(xml).expect("valid SVG");
        let image = svg.rasterize(400, 200).expect("rasterizes");
        let info = ImageInfo::new(
            (400, 200),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            ColorSpace::new_srgb(),
        );
        let mut pixels = vec![0u8; 400 * 200 * 4];
        assert!(
            image.inner.read_pixels(
                &info,
                &mut pixels,
                400 * 4,
                (0, 0),
                skia_safe::image::CachingHint::Allow,
            ),
            "the surface reads back"
        );
        let inked =
            |x: usize| (0..200).any(|y| pixels[(y * 400 + x) * 4 + 3] > 0);
        let first = (0..400).find(|&x| inked(x))? as u32;
        let last = (0..400).rev().find(|&x| inked(x))? as u32;
        Some((first, last))
    }

    /// Text positioned in a physical unit lands where CSS puts it.
    ///
    /// `x`, `y`, `dx` and `dy` on a text element are the four attributes
    /// `normalize_absolute_lengths` cannot reach, because skia-safe exposes
    /// them for reading only. They are rewritten in the document text
    /// instead, before Skia parses it.
    ///
    /// Each row is checked against the same position written in `px`, rather
    /// than against a pinned column: `1in` and `96` are the same place in
    /// CSS, so the two documents must ink the same columns whatever font the
    /// machine resolves. The control is the position Skia's own 90 dpi would
    /// have given, which must differ -- without it, equality with the `px`
    /// row would hold just as well if nothing had been rewritten.
    #[test]
    fn text_positioned_in_physical_units_lands_where_css_puts_it() {
        let doc = |element: &str, attrs: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" font-size="16"><text y="100" font-size="20" fill="#d11">{element}</text></svg>"##
            )
            .replace("{attrs}", attrs)
        };
        let text_at =
            |x: &str| doc(&format!(r##"<tspan x="{x}">|</tspan>"##), "");

        assert_eq!(
            painted_columns(&text_at("1in")),
            painted_columns(&text_at("96")),
            "an inch is 96 CSS pixels, so the glyph lands in the same column"
        );
        assert_ne!(
            painted_columns(&text_at("1in")),
            painted_columns(&text_at("90")),
            "the control: 90 is Skia's own answer for an inch and must differ"
        );

        let dx = |dx: &str| {
            doc(&format!(r##"<tspan x="10" dx="{dx}">|</tspan>"##), "")
        };
        assert_eq!(
            painted_columns(&dx("0.5in")),
            painted_columns(&dx("48")),
            "`dx` on a tspan is shifted the same way"
        );

        let dy = |dy: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" font-size="16"><text x="10" y="20" font-size="20" fill="#d11"><tspan dy="{dy}">|</tspan></text></svg>"##
            )
        };
        assert_eq!(
            painted_columns(&dy("2mm")),
            painted_columns(&dy("7.5590553")),
            "`dy` too -- 2mm is 96/25.4 times two"
        );
    }

    /// A positioning attribute holding a list converts each item on its own.
    ///
    /// This is the case a scan for `x="` could not have handled and the
    /// reason the rewrite parses instead. SVG separates the items by
    /// comma-wsp, so a mixed list has to convert the absolute items and carry
    /// the rest through untouched.
    #[test]
    fn a_list_of_positions_converts_each_item_separately() {
        assert_eq!(
            position_list_in_px("1in 2in").as_deref(),
            Some("96 192"),
            "both items move"
        );
        assert_eq!(
            position_list_in_px("1in 20").as_deref(),
            Some("96 20"),
            "a user unit beside an inch stays exactly as written"
        );
        assert_eq!(
            position_list_in_px("1in,2in").as_deref(),
            Some("96 192"),
            "comma-separated is the same list"
        );
        assert_eq!(
            position_list_in_px("10 20 30"),
            None,
            "a list with no absolute unit is not rewritten at all"
        );
        assert_eq!(
            position_list_in_px("50% 2em 3ex 4px"),
            None,
            "and neither is one written in relative units"
        );
    }

    /// A document with no absolute unit in a text attribute comes back
    /// byte-identical.
    ///
    /// The rewrite runs over every document this crate parses, so what it
    /// does to the ones it cannot improve matters more than what it does to
    /// the ones it can. Byte-identical is the only acceptable answer, and
    /// `text_position_lengths_in_px` returning `None` is how the caller gets
    /// the original bytes rather than a re-serialised copy of them.
    #[test]
    fn a_document_with_nothing_to_convert_is_passed_through_untouched() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" font-size="16">
  <desc>A description mentioning x="1in" in its prose.</desc>
  <!-- a comment mentioning x="1in" as well -->
  <style>text { fill: #d11; }</style>
  <text x="10 20" y="100" dx="0,5" font-size="20">hello</text>
  <rect width="1in" height="1in"/>
</svg>"##;
        assert_eq!(
            text_position_lengths_in_px(
                xml.as_bytes(),
                &[],
                &[],
                &FontMgr::new()
            ),
            None,
            "nothing in a text positioning attribute is absolute, so the \
             document is not rewritten -- and the `1in` on the rect is the \
             DOM walk's job, not this one's"
        );

        let absolute = xml.replace(r##"x="10 20""##, r##"x="1in 20""##);
        assert!(
            text_position_lengths_in_px(
                absolute.as_bytes(),
                &[],
                &[],
                &FontMgr::new()
            )
            .is_some(),
            "the control: one absolute unit in a text attribute has to make \
             this fire, or the assertion above passes for the wrong reason"
        );
    }

    /// A length inside a comment, a `<desc>` or a `<style>` is not touched.
    ///
    /// The specific failure a scan for `x="1in"` would have had, and the
    /// reason parsing was worth the dependency. Each of these documents
    /// contains the exact bytes the rewrite looks for, in a place where they
    /// are content rather than markup.
    #[test]
    fn a_length_that_is_not_markup_is_left_alone() {
        let hostile = [
            r##"<!-- <text x="1in"/> -->"##,
            r##"<desc>&lt;text x="1in"/&gt;</desc>"##,
            r##"<style>/* text x="1in" */ text { fill: #d11; }</style>"##,
            r##"<desc><![CDATA[<text x="1in"/>]]></desc>"##,
            r##"<rect data-note="text x=&quot;1in&quot;" width="1" height="1"/>"##,
        ];

        for body in hostile {
            let xml = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" font-size="16">{body}</svg>"##
            );
            assert_eq!(
                text_position_lengths_in_px(
                    xml.as_bytes(),
                    &[],
                    &[],
                    &FontMgr::new()
                ),
                None,
                "content is not markup: {body}"
            );
        }

        // The control: the same bytes as markup, which must be rewritten.
        let real = r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200" font-size="16"><text x="1in"/></svg>"##;
        assert!(
            text_position_lengths_in_px(
                real.as_bytes(),
                &[],
                &[],
                &FontMgr::new()
            )
            .is_some(),
            "the control: as an actual element the rewrite has to fire, or \
             every case above passes because the rewrite never fires at all"
        );
    }

    /// Input the rewrite cannot reason about is passed through, not guessed
    /// at.
    ///
    /// Each of these is a document that either renders today or does not, and
    /// in both cases this has to leave it exactly as it found it.
    #[test]
    fn input_that_cannot_be_reasoned_about_is_left_exactly_as_it_arrived() {
        let not_utf8 = b"<svg><text x=\"1in\">\xff\xfe</text></svg>";
        assert_eq!(
            text_position_lengths_in_px(not_utf8, &[], &[], &FontMgr::new()),
            None,
            "bytes that are not UTF-8 are not decoded and not rewritten"
        );

        // A truncated document. quick-xml reads the start tag and reports
        // EOF rather than an error, so the rewrite does fire here -- but
        // Skia refuses the document either way, before this change and
        // after it, which is the property that matters. Asserted through
        // `Svg::parse` rather than against the rewrite, because what must
        // not change is the answer a caller gets.
        let unclosed =
            r##"<svg xmlns="http://www.w3.org/2000/svg"><text x="1in">"##;
        assert!(
            matches!(Svg::parse(unclosed), Err(Error::DecodeImage { .. })),
            "a malformed document is still refused"
        );
        assert!(
            matches!(
                Svg::parse(&unclosed.replace(r##"x="1in""##, r##"x="96""##)),
                Err(Error::DecodeImage { .. })
            ),
            "the control: it is refused for being malformed and not for the \
             unit, so the rewrite is not what decides it"
        );

        let entity = br##"<svg xmlns="http://www.w3.org/2000/svg" font-size="16"><text x="&#x31;in"/></svg>"##;
        assert_eq!(
            text_position_lengths_in_px(entity, &[], &[], &FontMgr::new()),
            None,
            "an entity reference is left for Skia to resolve rather than \
             resolved by a second unescaper here"
        );
    }

    /// The first pixel of a 4x4 rasterization, as unpremultiplied sRGB bytes.
    ///
    /// Reading pixels rather than inspecting the DOM, because what is in
    /// question is whether an override survives to the render: Skia reads
    /// presentation attributes in `onPrepareToRender`, so a change that the
    /// DOM agrees with could still be ignored when the document is drawn.
    fn first_pixel(svg: &mut Svg) -> [u8; 4] {
        let image = svg.rasterize(4, 4).expect("rasterizes");
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            ColorSpace::new_srgb(),
        );
        let mut pixel = [0u8; 4];
        assert!(
            image.inner.read_pixels(
                &info,
                &mut pixel,
                4,
                (0, 0),
                skia_safe::image::CachingHint::Allow,
            ),
            "the surface reads back"
        );
        pixel
    }

    /// A 4x4 document wrapping `body`.
    fn doc(body: &str) -> Svg {
        Svg::parse(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{body}</svg>"#
        ))
        .expect("valid SVG")
    }

    /// Opaque red, the colour these tests override with.
    fn red() -> RgbaLinear {
        RgbaLinear::from_srgb8(255, 0, 0, 1.0)
    }

    /// The override reaches the pixels, at any depth, for fill and stroke.
    ///
    /// The undertone matters as much as the override: without one,
    /// `currentColor` resolves to the initial black, so a test asserting only
    /// the red would pass against an implementation that painted red
    /// unconditionally.
    #[test]
    fn a_current_color_override_reaches_the_rendered_pixels() {
        let fill = r#"<rect width="4" height="4" fill="currentColor"/>"#;

        let mut untouched = doc(fill);
        assert_eq!(
            first_pixel(&mut untouched),
            [0, 0, 0, 255],
            "with no override, currentColor is the initial black"
        );

        let mut overridden = doc(fill);
        overridden.set_current_color(red());
        assert_eq!(first_pixel(&mut overridden), [255, 0, 0, 255]);

        let mut nested = doc(&format!("<g><g>{fill}</g></g>"));
        nested.set_current_color(red());
        assert_eq!(
            first_pixel(&mut nested),
            [255, 0, 0, 255],
            "inheritance carries it down the tree"
        );

        let mut stroked = doc(
            r#"<rect width="4" height="4" fill="none" stroke="currentColor" stroke-width="4"/>"#,
        );
        stroked.set_current_color(red());
        assert_eq!(
            first_pixel(&mut stroked),
            [255, 0, 0, 255],
            "a stroke takes the same indirect value"
        );
    }

    /// A nearer `color` declaration wins, which is inheritance working.
    ///
    /// The depth test above cannot fail on this: neither of its `<g>`s
    /// declares a colour, so it proves depth and nothing about precedence.
    /// This pins the boundary the documentation describes -- and the root
    /// case, where the override replaces a declaration rather than losing to
    /// one, because the value is set on the root itself.
    #[test]
    fn a_nearer_color_declaration_wins_over_the_override() {
        let mut grouped = doc(
            r##"<g color="#0000FF"><rect width="4" height="4" fill="currentColor"/></g>"##,
        );
        grouped.set_current_color(red());
        assert_eq!(
            first_pixel(&mut grouped),
            [0, 0, 255, 255],
            "the group's own colour resolves its descendants"
        );

        let mut on_the_root = Svg::parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4" color="#0000FF"><rect width="4" height="4" fill="currentColor"/></svg>"##,
        )
        .expect("valid SVG");
        on_the_root.set_current_color(red());
        assert_eq!(
            first_pixel(&mut on_the_root),
            [255, 0, 0, 255],
            "the root's own declaration is what this replaces"
        );
    }

    /// Alpha is carried rather than flattened to opaque.
    #[test]
    fn a_current_color_override_keeps_its_alpha() {
        let mut half =
            doc(r#"<rect width="4" height="4" fill="currentColor"/>"#);
        half.set_current_color(RgbaLinear::from_srgb8(
            255,
            0,
            0,
            128.0 / 255.0,
        ));
        assert_eq!(first_pixel(&mut half), [255, 0, 0, 128]);
    }

    /// Paint that did not ask for the indirect value is left alone.
    ///
    /// This is the assertion that separates setting `color` from overwriting
    /// every fill: an implementation that walked the tree assigning paint
    /// would turn this rect red.
    #[test]
    fn paint_that_is_not_current_color_is_untouched() {
        let mut literal =
            doc(r##"<rect width="4" height="4" fill="#00FF00"/>"##);
        literal.set_current_color(red());
        assert_eq!(
            first_pixel(&mut literal),
            [0, 255, 0, 255],
            "a literal fill keeps its own colour"
        );
    }

    /// A `<style>` element is discarded and an inline `style=` is not.
    ///
    /// Asserting a limitation on purpose. Skia registers no factory for the
    /// tag, so the rules never reach the document, and the failure is silent:
    /// the stylesheet case is byte-identical to no fill at all. If Skia ever
    /// implements the element this test fails, which is the point -- the
    /// documentation on [`Svg`] would then be wrong and has to be rewritten
    /// rather than quietly left.
    #[test]
    fn a_style_element_is_ignored_where_a_style_attribute_is_honoured() {
        let mut attribute =
            doc(r##"<rect width="4" height="4" style="fill:#FF0000"/>"##);
        assert_eq!(
            first_pixel(&mut attribute),
            [255, 0, 0, 255],
            "an inline style attribute is parsed into presentation attributes"
        );

        let mut unfilled = doc(r#"<rect width="4" height="4"/>"#);
        assert_eq!(
            first_pixel(&mut unfilled),
            [0, 0, 0, 255],
            "an unfilled rect is the initial black"
        );

        let mut element = doc(
            r##"<style>rect{fill:#FF0000}</style><rect width="4" height="4"/>"##,
        );
        assert_eq!(
            first_pixel(&mut element),
            [0, 0, 0, 255],
            "a stylesheet changes nothing, and says nothing about it"
        );
    }

    #[test]
    fn unparseable_xml_is_an_error_rather_than_a_default_document() {
        assert!(matches!(Svg::parse("<svg"), Err(Error::DecodeImage { .. })));
    }
}
