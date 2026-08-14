//! Encoders for the formats Skia has none for.
//!
//! Skia's encoder list is three modules -- JPEG, PNG and WebP -- plus the
//! PDF and SVG document backends. A fourth, `png_rust_encoder`, is present
//! in skia-safe's source and commented out of its module tree, so it is not
//! something this crate can reach. Everything past those is encoded here,
//! from pixels Skia hands back, and
//! [`EncoderKind::Foreign`](crate::export::EncoderKind::Foreign) is what
//! marks a format as belonging to this module.
//!
//! Every encoder is handed the same thing: [`Frame`]s in one layout,
//! eight-bit RGBA, unpremultiplied, in the colour space
//! [`SequenceSpec::color`] describes. That is deliberate. Colour conversion
//! is subtle enough that each encoder re-deriving it would be several
//! chances to get it differently wrong, so the conversion happens once, on
//! the way in, and an encoder's only job is the container -- including
//! writing down which space the pixels arrived in, in whatever vocabulary
//! that container speaks.
//!
//! A still image is a one-frame animation, so the same seam serves a format
//! with no animation at all.
//!
//! # Why the frames are pushed rather than returned
//!
//! An encoder here is opened, fed a frame at a time, and closed. It would be
//! shorter to hand it every frame at once and take back a `Vec<u8>`, and
//! that is what this was; both ends of it are bounded by memory in a way
//! that has nothing to do with the picture. A thousand frames of 1080p is
//! 8 GB of pixels before a byte is encoded, and the encoded bytes then
//! accumulate a second time on the way out.
//!
//! Pushing fixes both. The caller rasterizes a bounded number of frames
//! ahead and drops each one after it is written, and the bytes go straight
//! into whatever [`Sink`] was supplied -- a file, for
//! [`Canvas::to_file`](crate::canvas::Canvas::to_file), so the output is
//! never held whole. `to_buffer` still gathers into memory, because it
//! returns bytes and there is nowhere else for them to go.

pub(crate) mod apng;
pub(crate) mod avif;
pub(crate) mod bmp;
pub(crate) mod color;
pub(crate) mod gif;
pub(crate) mod ico;
pub(crate) mod tiff;

use std::io::{Seek, Write};

use crate::export::ImageFormat;

use color::ColorProfile;

/// Somewhere encoded bytes can be written.
///
/// `Seek` as well as `Write`, which costs nothing and buys the formats that
/// cannot do without it. GIF and APNG never look back -- but TIFF and ICO
/// keep a directory of offsets they can only fill in once the data is
/// written, so a seam that offered only `Write` would force them to buffer
/// themselves and quietly lose the point. Both sinks this crate uses, a file
/// and a `Cursor<Vec<u8>>`, are already seekable.
pub(crate) trait Sink: Write + Seek {}
impl<T: Write + Seek + ?Sized> Sink for T {}

/// One rendered page, in the layout every encoder here is promised.
///
/// `pixels` is `width * height * 4` bytes: red, green, blue, alpha, with the
/// colour channels *not* multiplied by alpha, in the space
/// [`SequenceSpec::color`] names.
pub(crate) struct Frame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// How long this frame is shown, in milliseconds.
    pub delay_ms: u32,
}

/// What an encoder has to know before the first frame arrives.
///
/// The frame count is here because APNG needs it before it can write
/// anything: its `acTL` chunk carries `num_frames` and has to precede the
/// first `IDAT`. A canvas knows how many pages it has, so that is a fact
/// rather than a running total -- which is the only reason streaming an
/// APNG is possible at all. An encoder fed frames from an open-ended source
/// could not write one without seeking back.
pub(crate) struct SequenceSpec {
    pub width: u32,
    pub height: u32,
    /// How many frames will be written. Never zero.
    pub frames: usize,
    /// How many times the animation plays. `None` plays it forever.
    pub loops: Option<u32>,
    /// Encoder quality from 0 to 100, for the formats that are lossy.
    ///
    /// Every format here was lossless until AVIF, which is why this arrived
    /// late: GIF loses colour to its palette and nothing else here loses
    /// anything, so there was no dial to pass along.
    pub quality: f32,
    /// The scale the pages were rasterized at, for the formats that record
    /// a resolution.
    ///
    /// Only BMP uses it, and only because BMP is the one format in this
    /// module with a field for it. It went out as a fixed 72 dots per inch
    /// at every scale, so a 3x export declared the same resolution as a 1x
    /// one while the PNG beside it declared 216.
    pub density: f32,
    /// The colour space the frames are in, for the encoder to write down.
    ///
    /// Always the truth about the pixels rather than a request: a format
    /// whose `ColorSignal` is `AssumedSrgb` is handed frames that were
    /// narrowed to sRGB before they got here, and this says sRGB to match.
    pub color: ColorProfile,
}

/// An encoder that has been opened and is waiting for frames.
pub(crate) trait FrameSink {
    /// Writes one frame. Called exactly [`SequenceSpec::frames`] times, in
    /// order, with frames of the declared size -- [`start`] wraps every sink
    /// in a check that makes those true.
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String>;

    /// Closes the file. Nothing is guaranteed complete until this returns.
    fn finish(self: Box<Self>) -> Result<(), String>;
}

/// A container format that this crate, rather than Skia, writes.
pub(crate) trait FrameEncoder {
    /// Writes the header and returns something to feed the frames to.
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String>;

    /// Whether every frame has to be the size the spec declared.
    ///
    /// True for anything with one canvas in its header, which is every
    /// format here but one: an icon holds the same picture at several sizes,
    /// and refusing that would refuse the point of the container.
    fn uniform_frames(&self) -> bool {
        true
    }
}

/// Opens `format` on `out` and hands back the sink to feed.
///
/// # Errors
///
/// Returns a message when `format` is not one this module writes, when the
/// spec describes no frames, or when the encoder cannot write its header.
pub(crate) fn start<'a>(
    format: ImageFormat,
    spec: &SequenceSpec,
    out: &'a mut dyn Sink,
) -> Result<Box<dyn FrameSink + 'a>, String> {
    if spec.frames == 0 {
        return Err(format!(
            "Cannot encode {} with no pages to draw",
            format.as_str()
        ));
    }

    let encoder: &dyn FrameEncoder = match format {
        ImageFormat::Gif => &gif::Gif,
        ImageFormat::Apng => &apng::Apng,
        ImageFormat::Tiff => &tiff::Tiff,
        ImageFormat::Ico => &ico::Ico,
        ImageFormat::Bmp => &bmp::Bmp,
        ImageFormat::Avif => &avif::Avif,
        other => {
            return Err(format!(
                "{} is not encoded by this crate",
                other.as_str()
            ));
        }
    };
    let uniform = encoder.uniform_frames();
    let inner = encoder.start(spec, out)?;

    Ok(Box::new(Checked {
        inner,
        width: spec.width,
        height: spec.height,
        uniform,
        expected: spec.frames,
        written: 0,
        format,
    }))
}

/// Holds the promises [`FrameSink::write_frame`] is documented to rely on,
/// so no encoder has to re-check them and none can disagree about them.
struct Checked<'a> {
    inner: Box<dyn FrameSink + 'a>,
    width: u32,
    height: u32,
    uniform: bool,
    expected: usize,
    written: usize,
    format: ImageFormat,
}

impl FrameSink for Checked<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        // Every format here but the icon declares one canvas size in its
        // header and places each page inside it. A page of another size is a
        // caller error rather than something to crop or letterbox in silence.
        //
        // The message says "page", not "frame of an animation", because this
        // is not only reachable from one: TIFF holds pages that are pages,
        // and a multi-page canvas whose sizes disagree used to be told that
        // an "animated tiff" needed uniform frames.
        if self.uniform
            && (frame.width != self.width || frame.height != self.height)
        {
            return Err(format!(
                "Every page of a {} must be the same size (found {}x{} and \
                 {}x{})",
                self.format.as_str(),
                self.width,
                self.height,
                frame.width,
                frame.height
            ));
        }
        if self.written == self.expected {
            return Err(format!(
                "More frames were written to a {} than the {} it declared",
                self.format.as_str(),
                self.expected
            ));
        }
        self.written += 1;
        self.inner.write_frame(frame)
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        // A short animation is worse than a failed one: APNG's frame count
        // is in a chunk already written, so the file would name frames it
        // does not carry, and a decoder is entitled to anything at that
        // point.
        if self.written != self.expected {
            return Err(format!(
                "A {} declared {} frames and was given {}",
                self.format.as_str(),
                self.expected,
                self.written
            ));
        }
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, SeekFrom},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{encode::color::ColorProfile, pixels::PixelColorSpace};

    /// A sink that says how many bytes have reached it, while an encoder
    /// still holds it.
    ///
    /// Also the only thing here that is not a `Cursor`, so it doubles as a
    /// check that the seam takes any [`Sink`] rather than the one it was
    /// written against.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        seen: Arc<AtomicUsize>,
    }

    impl Write for Counting {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let wrote = self.inner.write(buf)?;
            self.seen.fetch_add(wrote, Ordering::Relaxed);
            Ok(wrote)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(to)
        }
    }

    fn frame(width: u32, height: u32) -> Frame {
        Frame {
            pixels: [255u8, 0, 0, 255].repeat((width * height) as usize),
            width,
            height,
            delay_ms: 100,
        }
    }

    fn spec(frames: usize) -> SequenceSpec {
        SequenceSpec {
            width: 2,
            height: 1,
            frames,
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
        }
    }

    #[test]
    fn a_page_of_another_size_is_refused_rather_than_cropped() {
        // Every format but the icon declares one canvas in its header.
        // Cropping or letterboxing in silence would write a file nobody drew.
        //
        // TIFF is here because it is the one that showed the message was
        // wrong: only GIF was covered, so nothing noticed that a format with
        // no clock at all was being told its "animated" frames disagreed.
        for format in [ImageFormat::Gif, ImageFormat::Apng, ImageFormat::Tiff] {
            let mut bytes = Cursor::new(Vec::new());
            let mut sink = start(format, &spec(2), &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(&frame(2, 1)).expect("the declared size");
            let refused = sink
                .write_frame(&frame(4, 2))
                .expect_err("a page of another size");
            assert!(refused.contains("same size"), "{format:?}: {refused}");
            assert!(
                refused.contains("2x1") && refused.contains("4x2"),
                "{format:?}: {refused}"
            );
            assert!(
                !refused.contains("animated"),
                "a TIFF has no clock, and neither does this message: \
                 {refused}"
            );
        }
    }

    #[test]
    fn a_short_animation_is_refused_rather_than_written() {
        // APNG's frame count goes out in a chunk before any frame does, so
        // stopping early leaves a file naming frames it does not carry. The
        // encoder cannot take that back, which is why it is caught here.
        for format in [ImageFormat::Gif, ImageFormat::Apng] {
            let mut bytes = Cursor::new(Vec::new());
            let mut sink = start(format, &spec(3), &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(&frame(2, 1)).expect("the first frame");
            let refused = sink.finish().expect_err("only one of three frames");
            assert!(
                refused.contains("declared 3 frames and was given 1"),
                "{format:?}: {refused}"
            );
        }
    }

    #[test]
    fn more_frames_than_declared_are_refused() {
        let mut bytes = Cursor::new(Vec::new());
        let mut sink = start(ImageFormat::Apng, &spec(1), &mut bytes)
            .expect("the spec is well formed");
        sink.write_frame(&frame(2, 1)).expect("the only frame");
        let refused = sink
            .write_frame(&frame(2, 1))
            .expect_err("one more than declared");
        assert!(refused.contains("More frames"), "{refused}");
    }

    #[test]
    fn an_animation_of_no_pages_is_refused_at_the_door() {
        let mut bytes = Cursor::new(Vec::new());
        let refused = start(ImageFormat::Gif, &spec(0), &mut bytes)
            .err()
            .expect("no pages to draw");
        assert!(refused.contains("no pages to draw"), "{refused}");
    }

    #[test]
    fn a_format_this_module_does_not_write_is_named() {
        let mut bytes = Cursor::new(Vec::new());
        let refused = start(ImageFormat::Png, &spec(1), &mut bytes)
            .err()
            .expect("png is Skia's");
        assert!(refused.contains("png is not encoded by this crate"));
    }

    #[test]
    fn the_bytes_land_in_the_sink_as_they_are_written() {
        // The point of the seam: nothing is held back to be handed over at
        // the end. A GIF's header is in the sink before the second frame is
        // even offered.
        let seen = Arc::new(AtomicUsize::new(0));
        let mut out = Counting {
            inner: Cursor::new(Vec::new()),
            seen: Arc::clone(&seen),
        };
        let count = || seen.load(Ordering::Relaxed);

        let mut sink = start(ImageFormat::Gif, &spec(2), &mut out)
            .expect("the spec is well formed");

        sink.write_frame(&frame(2, 1)).expect("the first frame");
        let after_one = count();
        assert!(after_one > 0, "the header and first frame are already out");

        sink.write_frame(&frame(2, 1)).expect("the second frame");
        assert!(
            count() > after_one,
            "and the second lands without waiting for the close"
        );

        sink.finish().expect("the encoder closes");
        let bytes = out.inner.into_inner();
        assert_eq!(&bytes[..6], b"GIF89a");
        assert_eq!(bytes.last(), Some(&0x3b), "the trailer is written");
    }
}
