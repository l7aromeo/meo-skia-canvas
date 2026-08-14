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
        TiffEncoder, TiffKind, colortype::RGBA8,
    },
    tags::Tag,
};

use super::{
    Frame, FrameEncoder, FrameSink, SequenceSpec, Sink,
    color::{Chromaticities, ColorProfile},
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
        // and lossless, so the picture is the same either way; the horizontal
        // predictor in front of it stores each channel as a difference from
        // its left neighbour, which is what makes a gradient compress at all.
        //
        // Balanced rather than Best: the last level buys a few percent for
        // several times the time, and an export is not an archive job.
        let encoder = TiffEncoder::new(out)
            .map_err(|e| format!("Could not start a TIFF: {e}"))?
            .with_compression(Compression::Deflate(DeflateLevel::Balanced))
            .with_predictor(Predictor::Horizontal);
        Ok(Box::new(TiffSink {
            encoder,
            width: spec.width,
            height: spec.height,
            color: spec.color,
        }))
    }
}

struct TiffSink<'a> {
    encoder: TiffEncoder<&'a mut dyn Sink>,
    width: u32,
    height: u32,
    color: ColorProfile,
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
        let mut image = self
            .encoder
            .new_image::<RGBA8>(self.width, self.height)
            .map_err(|e| format!("Could not start a TIFF page: {e}"))?;
        write_colorimetry(image.encoder(), &self.color)?;
        image
            .write_data(&frame.pixels)
            .map_err(|e| format!("Could not write a TIFF page: {e}"))
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
    use std::io::Cursor;

    use tiff::decoder::{Decoder, DecodingResult, ifd::Value};

    use super::*;
    use crate::{
        encode::{SequenceSpec, color::ColorProfile, start},
        export::ImageFormat,
        pixels::PixelColorSpace,
    };

    fn page(shade: u8) -> Frame {
        Frame {
            pixels: [shade, 64, 192, 255].repeat(8),
            width: 4,
            height: 2,
            delay_ms: 100,
        }
    }

    fn encoded_in(pages: &[Frame], space: PixelColorSpace) -> Vec<u8> {
        let spec = SequenceSpec {
            width: 4,
            height: 2,
            frames: pages.len(),
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(space),
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
                    assert_eq!(got, written[pages].pixels, "page {pages}");
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
