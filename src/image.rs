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
    /// For a document declaring neither a usable `width`/`height` nor a
    /// `viewBox`, this is the fallback described on [`Svg::is_autosized`]
    /// rather than anything the file states.
    pub fn intrinsic_size(&self) -> Size {
        self.intrinsic
    }

    /// Whether the document declared no usable size of its own.
    ///
    /// True when `width` and `height` both resolve to `100%`, which is what
    /// Skia reports for an `<svg>` element carrying neither attribute. The
    /// size returned by [`Svg::intrinsic_size`] is then derived rather than
    /// read: the `viewBox` aspect ratio at the height named by
    /// `DEFAULT_SVG_HEIGHT`, or a square if there is no `viewBox` either.
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

/// Works out how big an SVG wants to be, mirroring Chrome.
///
/// Returns the size and whether it had to be invented. Skia answers
/// `intrinsic_size` directly whenever the document states one; everything
/// below is the empty case, where `width` and `height` come back as `100%`.
///
/// Only unitless lengths are read. A `width="10em"` falls through to the
/// `viewBox` branch rather than being converted, so a document sized purely
/// in `em`, `ex`, `pt`, `pc`, `cm`, `mm` or `in` is rasterized at the default
/// height instead of its declared extent.
fn derive_intrinsic_size(dom: &mut svg::Dom) -> (Size, bool) {
    let root = dom.root();
    let size = root.intrinsic_size();
    if !size.is_empty() {
        return (Size::new(size.width, size.height), false);
    }

    let Length {
        value: width,
        unit: w_unit,
    } = root.width();
    let Length {
        value: height,
        unit: h_unit,
    } = root.height();

    let derived = match ((width, w_unit), (height, h_unit)) {
        ((100.0, LengthUnit::Percentage), (height, LengthUnit::Number)) => {
            Size::new(*height, *height)
        }
        ((width, LengthUnit::Number), (100.0, LengthUnit::Percentage)) => {
            Size::new(*width, *width)
        }
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

    /// Only unitless lengths are read, and a document sized in `em` is
    /// therefore rasterized at the fallback rather than at what it asked for.
    ///
    /// Asserting the limitation rather than the fix, so that implementing
    /// unit conversion fails this test and has to say so.
    #[test]
    fn a_length_carrying_a_unit_is_ignored() {
        let em = Svg::parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="10em"/>"#,
        )
        .expect("valid SVG");
        assert_eq!(
            em.intrinsic_size(),
            Size::new(150.0, 150.0),
            "em lengths are not converted; the document falls back"
        );
        assert!(em.is_autosized());
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
