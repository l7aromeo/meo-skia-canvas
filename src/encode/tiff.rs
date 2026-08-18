//! TIFF, which is multi-page without being animated.
//!
//! Every other format this module writes that spans pages is an animation,
//! and TIFF is the one that shows those are separate questions: its pages
//! are pages. [`Frame::delay_ms`] and [`SequenceSpec::loops`] mean nothing
//! here and are ignored rather than approximated.
//!
//! The one format here that earns a dependency. A BMP or an icon is a header
//! wrapped around pixels; a TIFF is LZW and deflate with predictors, and a
//! directory of offsets that cannot be filled in until the strips are
//! written -- which is what [`Sink`] asks for `Seek` on behalf of.

use std::io::{Seek, Write};

use tiff::{
    encoder::{
        Compression, DeflateLevel, DirectoryEncoder, Predictor, Rational,
        TiffEncoder, TiffKind,
        colortype::{RGBA8, RGBA16},
    },
    tags::Tag,
};

use super::{
    Frame, FrameDepth, FrameEncoder, FrameSink, SequenceSpec, Sink,
    color::{Chromaticities, ColorProfile},
    rowfilter,
};

/// The denominator every chromaticity is written over.
///
/// TIFF stores `WhitePoint` and `PrimaryChromaticities` as RATIONALs, which
/// are a pair of `u32`. A hundred thousand keeps five decimal places, which
/// is two more than any of these coordinates carries -- Rec. 2020's `0.708`
/// is the longest at three -- so nothing is lost and the numerator stays far
/// from overflowing.
const CHROMATICITY_SCALE: f32 = 100_000.0;

/// `WhitePoint`, TIFF 6.0's tag for the xy coordinates of the white point.
///
/// The `tiff` crate's `Tag` enum does not name it, so it is written through
/// `Tag::Unknown`. The number is the standard's, not a choice made here.
const TAG_WHITE_POINT: u16 = 318;

/// `PrimaryChromaticities`, TIFF 6.0's tag for the xy coordinates of the red,
/// green and blue primaries, in that order.
const TAG_PRIMARY_CHROMATICITIES: u16 = 319;

pub(crate) struct Tiff;

impl FrameEncoder for Tiff {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        // The crate defaults to writing the pixels out whole, which for a
        // 1200x900 page is 4.2 MB -- the size of the raw buffer, a TIFF being
        // a header wrapped around one. Deflate is in TIFF 6.0's own tag list
        // and lossless, so the picture is the same either way.
        //
        // Balanced rather than Best: the last level buys a few percent for
        // several times the time, and an export is not an archive job.
        //
        // The predictor is left at TIFF's default of none until a frame is
        // in hand to ask about -- see [`TiffSink::predictor_for`].
        let encoder = TiffEncoder::new(out)
            .map_err(|e| format!("Could not start a TIFF: {e}"))?
            .with_compression(Compression::Deflate(DeflateLevel::Balanced));
        Ok(Box::new(TiffSink {
            encoder: Some(encoder),
            predictor: None,
            depth: spec.depth,
            width: spec.width,
            height: spec.height,
            color: spec.color,
        }))
    }
}

struct TiffSink<'a> {
    depth: FrameDepth,
    /// `None` only while the predictor is being applied to it, which is
    /// between two statements of [`TiffSink::settle`] and nowhere a caller
    /// can observe: `with_predictor` consumes the encoder and hands back
    /// another, so it has to leave the field to do it.
    encoder: Option<TiffEncoder<&'a mut dyn Sink>>,
    /// Whether this drawing's pixels are worth differencing, once a frame has
    /// been seen. See [`TiffSink::predictor_for`].
    ///
    /// Nothing releases this: the sink is built by `Tiff::start` and dropped
    /// when the file is finished, so the answer cannot outlive the export it
    /// was asked for.
    predictor: Option<Predictor>,
    width: u32,
    height: u32,
    color: ColorProfile,
}

impl<'a> TiffSink<'a> {
    /// Whether to difference this drawing against the pixel to the left.
    ///
    /// TIFF's horizontal predictor is the same idea as PNG's row filter --
    /// store a neighbour's difference rather than the value -- and it has
    /// the same answer, which is that it depends entirely on the drawing.
    /// Measured on five 1200x900 pages, deflated at the level below, with
    /// the predictor on and off:
    ///
    /// ```text
    ///            on               off
    ///   mixed     883.0 KB         703.4 KB
    ///   flat       76.3             56.1
    ///   gradient   99.9             52.5
    ///   photo     358.5           1708.4
    ///   noise    2739.1           2957.0
    /// ```
    ///
    /// So it was pinned on, and on three of those five that cost a fifth to
    /// a half of the file and up to 45% of the encode time. The comment that
    /// pinned it said the predictor "is what makes a gradient compress at
    /// all", which is the case it gets most wrong: 99.9 KB against 52.5.
    ///
    /// Asked once per export, of the first frame, by the same sampling the
    /// PNG writers probe with -- but along the row, because that is the
    /// difference this predictor takes. Borrowing PNG's own probe was tried
    /// first and is not the same question: on the noise page it answered no,
    /// where the predictor is 7% smaller.
    fn predictor_for(&mut self, frame: &Frame) -> Predictor {
        *self.predictor.get_or_insert_with(|| {
            // `eight()` is RGBA8 whatever the page's depth, and the
            // question is about the drawing rather than about how many bits
            // it will be written at.
            const RGBA: usize = 4;
            let row = frame.width as usize * RGBA;
            match rowfilter::pays_for_left(
                &frame.eight(),
                row,
                frame.height as i32,
                RGBA,
            ) {
                Some(true) | None => Predictor::Horizontal,
                Some(false) => Predictor::None,
            }
        })
    }

    /// Applies the predictor this drawing wants, before the first page.
    ///
    /// `with_predictor` consumes the encoder, so it is taken out of the field
    /// and put back. Only the first call does anything: after it the answer
    /// is already on the encoder, and every later page of the same export is
    /// the same drawing.
    fn settle(&mut self, frame: &Frame) {
        if self.predictor.is_some() {
            return;
        }
        let predictor = self.predictor_for(frame);
        if let Some(encoder) = self.encoder.take() {
            self.encoder = Some(encoder.with_predictor(predictor));
        }
    }
}

impl FrameSink for TiffSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        // One directory per page, in order. Written whole rather than in
        // strips: the pixels are already in memory, and a strip boundary
        // would only matter to a reader streaming a file larger than this
        // crate can hand it.
        //
        // `new_image` rather than the `write_image` convenience it replaced,
        // because the colour tags have to go into the same directory as the
        // pixels and `write_image` closes it before returning.
        // TIFF states its bits a channel in the directory, so sixteen is a
        // different `ColorType` rather than a flag -- which is why the two
        // depths are two branches rather than one call with a parameter.
        //
        // Before the first page, and only then: the predictor is a property
        // of the encoder and every page of one export is one drawing.
        self.settle(frame);
        let Some(encoder) = self.encoder.as_mut() else {
            return Err("The TIFF encoder went missing mid-file".to_string());
        };

        match self.depth {
            FrameDepth::Sixteen => {
                let mut image = encoder
                    .new_image::<RGBA16>(self.width, self.height)
                    .map_err(|e| format!("Could not start a TIFF page: {e}"))?;
                write_colorimetry(image.encoder(), &self.color)?;
                image
                    .write_data(&frame.sixteen())
                    .map_err(|e| format!("Could not write a TIFF page: {e}"))
            }
            FrameDepth::Eight => {
                let mut image = encoder
                    .new_image::<RGBA8>(self.width, self.height)
                    .map_err(|e| format!("Could not start a TIFF page: {e}"))?;
                write_colorimetry(image.encoder(), &self.color)?;
                image
                    .write_data(&frame.eight())
                    .map_err(|e| format!("Could not write a TIFF page: {e}"))
            }
        }
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        // Each page closes its own directory and links the one before it, so
        // there is nothing left to terminate.
        Ok(())
    }
}

/// `x` as the RATIONAL pair TIFF stores a chromaticity in.
fn rational(x: f32) -> Rational {
    Rational {
        n: (x * CHROMATICITY_SCALE).round().max(0.0) as u32,
        d: CHROMATICITY_SCALE as u32,
    }
}

/// The six primaries and the white point, in the order TIFF lists them.
fn chromaticity_tags(xy: &Chromaticities) -> ([Rational; 6], [Rational; 2]) {
    (
        [
            rational(xy.red.0),
            rational(xy.red.1),
            rational(xy.green.0),
            rational(xy.green.1),
            rational(xy.blue.0),
            rational(xy.blue.1),
        ],
        [rational(xy.white.0), rational(xy.white.1)],
    )
}

/// Writes the tags saying which colour space this page's pixels are in.
///
/// TIFF describes colour the way the CIE diagram does, as the xy coordinates
/// of the primaries and the white point. Both tags are baseline TIFF 6.0 --
/// `WhitePoint` is 318 and `PrimaryChromaticities` is 319 -- and predate ICC
/// by years, so a reader that understands nothing else understands these.
///
/// The `tiff` crate names neither in its `Tag` enum, which is why they are
/// written through `Tag::Unknown`. That is the numbers the standard assigns,
/// not numbers chosen here.
///
/// What this does *not* write is the transfer function. TIFF's tag for it is
/// a lookup table of three times 2^bits entries -- 1.5 KB per page at eight
/// bits -- and it cannot express PQ or HLG at all, which are curves rather
/// than tables. A reader takes the absence as the usual thing for the
/// primaries it was given, which is right for every space here.
fn write_colorimetry<W: Write + Seek, K: TiffKind>(
    directory: &mut DirectoryEncoder<'_, W, K>,
    color: &ColorProfile,
) -> Result<(), String> {
    let (primaries, white) = chromaticity_tags(&color.chromaticities);
    directory
        .write_tag(Tag::Unknown(TAG_WHITE_POINT), &white[..])
        .map_err(|e| format!("Could not write the TIFF white point: {e}"))?;
    directory
        .write_tag(Tag::Unknown(TAG_PRIMARY_CHROMATICITIES), &primaries[..])
        .map_err(|e| format!("Could not write the TIFF primaries: {e}"))
}

#[cfg(test)]
mod tests {
    use crate::export::ChromaSampling;
    use std::io::Cursor;

    use crate::encode::{FrameDepth, Pixels};
    use tiff::decoder::{Decoder, DecodingResult, ifd::Value};

    use super::*;
    use crate::{
        encode::{SequenceSpec, color::ColorProfile, start},
        export::ImageFormat,
        pixels::PixelColorSpace,
    };

    fn page(shade: u8) -> Frame {
        Frame {
            pixels: Pixels::Eight([shade, 64, 192, 255].repeat(8)),
            width: 4,
            height: 2,
            delay_ms: 100,
        }
    }

    fn encoded_in(pages: &[Frame], space: PixelColorSpace) -> Vec<u8> {
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width: 4,
            height: 2,
            frames: pages.len(),
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(space),
            space,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Tiff, &spec, &mut bytes)
                .expect("the spec is well formed");
            for page in pages {
                sink.write_frame(page).expect("a well formed page");
            }
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    fn encoded(pages: &[Frame]) -> Vec<u8> {
        encoded_in(pages, PixelColorSpace::Srgb)
    }

    /// A page of `height` rows, `width` pixels of RGBA each, from `pixel`.
    fn drawn(
        width: u32,
        height: u32,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> Frame {
        let mut pixels = Vec::with_capacity((width * height) as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        Frame {
            pixels: Pixels::Eight(pixels),
            width,
            height,
            delay_ms: 0,
        }
    }

    /// The RGBA of the first page of `bytes`, and the predictor it declares.
    fn decoded(bytes: &[u8]) -> (Vec<u8>, u16) {
        let mut decoder =
            Decoder::new(Cursor::new(bytes)).expect("a readable TIFF");
        let predictor = match decoder
            .get_tag(Tag::Predictor)
            .expect("the predictor tag is written")
        {
            Value::Short(value) => value,
            other => panic!("the predictor is not a SHORT: {other:?}"),
        };
        let pixels = match decoder.read_image().expect("a decodable page") {
            DecodingResult::U8(pixels) => pixels,
            other => panic!("not an eight-bit page: {other:?}"),
        };
        (pixels, predictor)
    }

    /// The spec `encoded_in` uses, at whatever size the page is.
    fn sized_spec(width: u32, height: u32) -> SequenceSpec {
        SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width,
            height,
            frames: 1,
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
            space: PixelColorSpace::Srgb,
            depth: FrameDepth::Eight,
            bits: None,
        }
    }

    fn one_page(frame: &Frame) -> Vec<u8> {
        let spec = sized_spec(frame.width, frame.height);
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Tiff, &spec, &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(frame).expect("a well formed page");
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    #[test]
    fn the_predictor_follows_the_drawing() {
        // TIFF's horizontal predictor is the same idea as PNG's row filter
        // and has the same answer: it depends on the drawing. Pinned on, it
        // cost a fifth to a half of the file on three of five 1200x900 pages
        // -- a gradient came to 99.9 KB with it and 52.5 without, against a
        // comment claiming it was "what makes a gradient compress at all".
        //
        // A vertical ramp, whose rows each hold one colour: nothing along a
        // row to difference against, so storing the values is smaller.
        let flat_rows = drawn(64, 64, |_, y| [y as u8, 128, 200, 255]);
        // A horizontal ramp: every row climbs by one, which is exactly what
        // a difference from the pixel to the left collapses.
        let climbing_rows =
            drawn(64, 64, |x, _| [x as u8, x as u8, x as u8, 255]);

        let (_, flat) = decoded(&one_page(&flat_rows));
        let (_, climbing) = decoded(&one_page(&climbing_rows));
        assert_eq!(
            flat,
            Predictor::None as u16,
            "a row of one colour has nothing to difference along it"
        );
        assert_eq!(
            climbing,
            Predictor::Horizontal as u16,
            "a row that climbs by one is what the predictor is for"
        );
    }

    #[test]
    fn either_predictor_decodes_to_the_pixels_that_were_drawn() {
        // The tag and the transform have to agree, and they are applied by
        // the same call -- but the whole point of choosing between them is
        // that both are now reachable, so both are checked.
        for frame in [
            drawn(64, 64, |_, y| [y as u8, 128, 200, 255]),
            drawn(64, 64, |x, _| [x as u8, x as u8, x as u8, 255]),
        ] {
            let Pixels::Eight(ref drawn_bytes) = frame.pixels else {
                panic!("the fixture is eight-bit");
            };
            let (read_back, _) = decoded(&one_page(&frame));
            assert_eq!(
                &read_back, drawn_bytes,
                "a TIFF is lossless whichever predictor it declares"
            );
        }
    }

    /// The value of a RATIONAL tag, as the numbers those fractions are.
    ///
    /// Matched on rather than converted. The decoder's `into_f64_vec` maps
    /// `into_f64` over a list and that accepts only DOUBLE, so a list of
    /// RATIONALs -- which is exactly what these two tags are -- comes back
    /// as `InvalidTypeForTag` rather than as numbers. The shape on the wire
    /// is `List([Rational(n, d), ..])`, and dividing is what the tag means.
    fn rationals<R: std::io::Read + std::io::Seek>(
        decoder: &mut Decoder<R>,
        tag: u16,
    ) -> Vec<f64> {
        let value = decoder
            .get_tag(Tag::Unknown(tag))
            .unwrap_or_else(|e| panic!("tag {tag} is absent: {e}"));
        let items = match value {
            Value::List(items) => items,
            single => vec![single],
        };
        items
            .into_iter()
            .map(|item| match item {
                Value::Rational(numerator, denominator) => {
                    f64::from(numerator) / f64::from(denominator)
                }
                other => panic!("tag {tag} holds {other:?}, not a RATIONAL"),
            })
            .collect()
    }

    #[test]
    fn a_page_records_the_chromaticities_of_the_space_it_holds() {
        // Display P3's red primary is at x = 0.68, which is the number that
        // makes the tag worth writing: sRGB's is 0.64, and a reader with no
        // tag assumes the latter.
        for (space, red_x) in [
            (PixelColorSpace::Srgb, 0.64_f32),
            (PixelColorSpace::DisplayP3, 0.68),
            (PixelColorSpace::Rec2020, 0.708),
        ] {
            let bytes = encoded_in(&[page(90)], space);
            let mut decoder =
                Decoder::new(Cursor::new(&bytes)).expect("a TIFF");
            let primaries = rationals(&mut decoder, TAG_PRIMARY_CHROMATICITIES);
            assert_eq!(primaries.len(), 6, "{space:?}: three xy pairs");
            assert!(
                (primaries[0] - f64::from(red_x)).abs() < 1e-4,
                "{space:?} red x: wrote {}, expected {red_x}",
                primaries[0]
            );
            // The white point is D65 for every space this crate offers, so
            // it is the same six digits each time and still has to be there.
            let white = rationals(&mut decoder, TAG_WHITE_POINT);
            assert!(
                (white[0] - 0.3127).abs() < 1e-4
                    && (white[1] - 0.3290).abs() < 1e-4,
                "{space:?} white point: {white:?}"
            );
        }
    }

    #[test]
    fn every_page_of_a_wide_gamut_tiff_carries_the_tags() {
        // The tags go in the directory, so a three-page TIFF needs them
        // three times -- and `write_image`, which this replaced, closed the
        // directory before there was anywhere to put them.
        let bytes = encoded_in(
            &[page(10), page(120), page(240)],
            PixelColorSpace::DisplayP3,
        );
        let mut decoder = Decoder::new(Cursor::new(&bytes)).expect("a TIFF");
        for page_number in 0..3 {
            let primaries = rationals(&mut decoder, TAG_PRIMARY_CHROMATICITIES);
            assert!((primaries[0] - 0.68).abs() < 1e-4, "page {page_number}");
            if page_number < 2 {
                decoder.next_image().expect("the next directory");
            }
        }
    }

    #[test]
    fn every_page_becomes_a_directory_and_reads_back_whole() {
        // Skia has no TIFF decoder at all -- its list is bmp, gif, ico,
        // jpeg, png, wbmp and webp -- so this is checked against the crate
        // that wrote it, as APNG is.
        let written = [page(10), page(120), page(240)];
        let bytes = encoded(&written);
        assert_eq!(&bytes[..4], b"II*\0", "little-endian TIFF");

        let mut decoder =
            Decoder::new(Cursor::new(&bytes)).expect("a TIFF this crate wrote");
        let mut pages = 0;
        loop {
            assert_eq!(decoder.dimensions().expect("dimensions"), (4, 2));
            match decoder.read_image().expect("pixels") {
                DecodingResult::U8(got) => {
                    assert_eq!(
                        got,
                        &written[pages].eight()[..],
                        "page {pages}"
                    );
                }
                other => panic!("expected eight-bit pixels, got {other:?}"),
            }
            pages += 1;
            if !decoder.more_images() {
                break;
            }
            decoder.next_image().expect("the next directory");
        }
        assert_eq!(pages, written.len());
    }

    #[test]
    fn a_single_page_is_still_a_tiff() {
        let bytes = encoded(&[page(200)]);
        let mut decoder = Decoder::new(Cursor::new(&bytes)).expect("decodes");
        assert!(!decoder.more_images(), "one directory, nothing after it");
        assert_eq!(decoder.dimensions().unwrap(), (4, 2));
    }
}
