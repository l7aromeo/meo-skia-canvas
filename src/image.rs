use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorType, Data, FontMgr, Image as SkImage,
    ImageInfo, Size,
    codec::{self, Codec},
    images, surfaces,
};

use crate::{
    error::Error,
    pixels::{PixelColorSpace, PixelFormat},
};

/// An immutable decoded raster image.
///
/// Cloning is cheap: Skia images are reference-counted and the pixels are
/// shared, not copied.
///
/// An image decoded from an animated file -- GIF or WebP -- carries every
/// frame. Drawing it draws the first one, and [`Image::frame`] hands back
/// any of the others.
#[derive(Debug, Clone)]
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
) -> Result<SkImage, Error> {
    // As in `frame_delays`: Skia would hand back the still `IDAT` for every
    // index, so every frame of an APNG would draw as the first.
    if crate::decode::apng::is_animated(data.as_bytes()) {
        return crate::decode::apng::frame(data.as_bytes(), index)
            .map_err(|reason| Error::DecodeImage { reason });
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
        let image = SkImage::from_encoded(data.clone()).ok_or_else(|| {
            Error::DecodeImage {
                reason: "skia could not decode the encoded image bytes"
                    .to_string(),
            }
        })?;
        let delays = frame_delays(&data);
        Ok(Self {
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

    /// Rasterizes an SVG XML document into a `Image` of the given dimensions.
    ///
    /// `from_encoded` does not decode SVG XML (it handles raster codecs only);
    /// this method is the explicit SVG bridge.
    ///
    /// SVG content is rendered into a transparent linear-light sRGB
    /// surface at the requested width and height, then snapshotted. The
    /// result is suitable for passing to `draw_image_rect` /
    /// `draw_image_src`.
    ///
    /// `width` and `height` set the SVG container size: the SVG's own
    /// `viewBox` and intrinsic dimensions are mapped into this box.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero,
    /// [`Error::DecodeImage`] if the XML cannot be parsed, and
    /// [`Error::SurfaceCreate`] if the rasterization surface cannot be
    /// allocated.
    pub fn from_svg_xml(
        svg: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let font_mgr = FontMgr::new();
        let mut dom = skia_safe::svg::Dom::from_bytes(svg.as_bytes(), font_mgr)
            .map_err(|_| Error::DecodeImage {
                reason: "could not parse SVG XML".to_string(),
            })?;
        dom.set_container_size(Size::new(width as f32, height as f32));

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
            dom.render(canvas);
        }
        Ok(Self::still(surface.image_snapshot()))
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
        decode_frame(data, index).map(Self::still)
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
}
