//! BMP, written by hand.
//!
//! A dependency would buy nothing here. There is no compression to get
//! wrong and no offset table to keep consistent: a BMP is two headers and
//! the pixels, and the only things worth being careful about are that the
//! rows run bottom to top and that the alpha channel needs the later header
//! to be described at all.

use super::{
    Frame, FrameEncoder, FrameSink, SequenceSpec, Sink, color::ColorProfile,
};
use crate::export::pixels_per_metre;

/// `BITMAPV4HEADER`, which is the first version with channel masks.
///
/// The older `BITMAPINFOHEADER` can carry 32-bit pixels but has nowhere to
/// say that the fourth channel is alpha, so readers treat it as padding and
/// the transparency is lost. Everything since Windows 95 reads V4.
const V4_HEADER: u32 = 108;
const FILE_HEADER: u32 = 14;

/// `LCS_sRGB`, the `bV4CSType` value saying the pixels are already sRGB.
///
/// The four bytes spell `sRGB` when read most-significant first, which is how
/// the constant is written in `wingdi.h` and why it looks reversed in a hex
/// dump of the little-endian field.
///
/// Naming it is the point. The literal here used to be `0x7357_696E`, which
/// spells `sWin` -- the two halves of `LCS_sRGB` and `LCS_WINDOWS_COLOR_SPACE`
/// spliced together, matching no value the format defines, under a comment
/// claiming it was the former. Nothing caught it: readers overwhelmingly
/// ignore `bV4CSType` and take the pixels as sRGB regardless, so the file
/// looked right everywhere it was opened.
///
/// With this set, the endpoint and gamma fields that follow are ignored, so
/// leaving them zero is correct rather than merely unfinished.
const LCS_SRGB: u32 = 0x7352_4742;

/// `LCS_CALIBRATED_RGB`, the `bV4CSType` value saying the endpoint and gamma
/// fields that follow describe the space rather than being ignored.
///
/// Zero, and the only `bV4CSType` value that is not four characters: it is
/// what the field held before Windows had any named spaces to put in it.
const LCS_CALIBRATED_RGB: u32 = 0;

/// The fixed-point scale BMP writes an endpoint coordinate in.
///
/// `CIEXYZ` is three `FXPT2DOT30` values -- 2 integer bits and 30 fractional
/// -- so one is `1 << 30`. Every coordinate here is between 0 and 2, which is
/// exactly what two integer bits hold.
const FXPT2DOT30_ONE: f64 = (1u32 << 30) as f64;

/// The fixed-point scale BMP writes a gamma value in.
///
/// `bV4GammaRed` and its siblings are 16.16 fixed point, so one is `1 << 16`.
const GAMMA_16_16_ONE: f64 = (1u32 << 16) as f64;

/// `bV4Planes`. Always one; the field is a leftover from the planar
/// bitmaps of Windows 1.0 and no reader has accepted another value since.
const COLOUR_PLANES: u16 = 1;

/// `bV4BitCount`. Eight bits each of blue, green, red and alpha.
const BITS_PER_PIXEL: u16 = 32;

/// `BI_BITFIELDS`, the `bV4V4Compression` value saying the four masks below
/// describe where each channel sits rather than the pixels being compressed.
///
/// A `wingdi.h` name, as [`LCS_SRGB`] is. Nothing here is compressed --
/// `BI_RGB`, the uncompressed value, would say the fourth channel is padding
/// and lose the alpha.
const BI_BITFIELDS: u32 = 3;

/// `bV4ClrUsed` and `bV4ClrImportant`. Zero for both: a 32-bit BMP has no
/// palette, so there are no entries to count and none to call important.
const NO_PALETTE: u32 = 0;

/// The four `BI_BITFIELDS` channel masks, in the order the header lists
/// them.
///
/// These are what make the fourth channel alpha rather than padding, which
/// is the whole reason this writes a V4 header. Their pattern is the layout
/// the rows are written in below -- blue lowest, then green, red and alpha
/// -- so a change to one without the other would swap the channels.
const MASK_RED: u32 = 0x00FF_0000;
const MASK_GREEN: u32 = 0x0000_FF00;
const MASK_BLUE: u32 = 0x0000_00FF;
const MASK_ALPHA: u32 = 0xFF00_0000;

pub(crate) struct Bmp;

impl FrameEncoder for Bmp {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        let width = i32::try_from(spec.width)
            .map_err(|_| format!("A BMP is at most {} wide", i32::MAX))?;
        let height = i32::try_from(spec.height)
            .map_err(|_| format!("A BMP is at most {} tall", i32::MAX))?;
        Ok(Box::new(BmpSink {
            out,
            width,
            height,
            density: spec.density,
            color: spec.color,
        }))
    }
}

struct BmpSink<'a> {
    out: &'a mut dyn Sink,
    width: i32,
    height: i32,
    density: f32,
    color: ColorProfile,
}

impl FrameSink for BmpSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        let pixels =
            (self.width as u32 as u64) * (self.height as u32 as u64) * 4;
        let size = u32::try_from(u64::from(FILE_HEADER + V4_HEADER) + pixels)
            .map_err(|_| {
            "A BMP cannot describe a file this large".to_string()
        })?;

        let mut header = Vec::with_capacity((FILE_HEADER + V4_HEADER) as usize);
        header.extend_from_slice(b"BM");
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes()); // reserved
        header.extend_from_slice(&(FILE_HEADER + V4_HEADER).to_le_bytes());

        header.extend_from_slice(&V4_HEADER.to_le_bytes());
        header.extend_from_slice(&self.width.to_le_bytes());
        // Positive height means the rows are stored bottom to top, which is
        // the orientation every BMP reader expects.
        header.extend_from_slice(&self.height.to_le_bytes());
        header.extend_from_slice(&COLOUR_PLANES.to_le_bytes());
        header.extend_from_slice(&BITS_PER_PIXEL.to_le_bytes());
        header.extend_from_slice(&BI_BITFIELDS.to_le_bytes());
        header.extend_from_slice(&(pixels as u32).to_le_bytes());
        // `bV4XPelsPerMeter` and its vertical twin. Through the same
        // helper the PNG path uses, so the two formats cannot disagree
        // about what a density means -- which they did twice over: this
        // field was a fixed 2835 that ignored `density` entirely, and then
        // a derived 2834 that truncated where the convention rounds.
        let per_metre = pixels_per_metre(self.density) as i32;
        header.extend_from_slice(&per_metre.to_le_bytes());
        header.extend_from_slice(&per_metre.to_le_bytes());
        header.extend_from_slice(&NO_PALETTE.to_le_bytes());
        header.extend_from_slice(&NO_PALETTE.to_le_bytes());
        for mask in [MASK_RED, MASK_GREEN, MASK_BLUE, MASK_ALPHA] {
            header.extend_from_slice(&mask.to_le_bytes());
        }
        // sRGB gets the named constant, which is shorter and is what every
        // reader has a fast path for. Anything else gets the calibrated form:
        // the endpoints and gamma below are the space, spelled out.
        match self.color.is_srgb() {
            true => {
                header.extend_from_slice(&LCS_SRGB.to_le_bytes());
                // `LCS_sRGB` makes a reader ignore what follows, so leaving
                // the endpoints and the three gamma fields zero is correct
                // rather than merely unfinished.
                header.resize((FILE_HEADER + V4_HEADER) as usize, 0);
            }
            false => {
                header.extend_from_slice(&LCS_CALIBRATED_RGB.to_le_bytes());
                write_endpoints(&mut header, &self.color)?;
            }
        }

        self.out
            .write_all(&header)
            .map_err(|e| format!("Could not write the BMP header: {e}"))?;

        // Bottom to top, and BGRA rather than RGBA. No row padding to
        // compute: at 32 bits a pixel every row is already a multiple of
        // four bytes.
        let stride = (self.width as usize) * 4;
        let mut row = vec![0u8; stride];
        for y in (0..self.height as usize).rev() {
            let src = &frame.pixels[y * stride..y * stride + stride];
            for (out, pixel) in row.chunks_exact_mut(4).zip(src.chunks_exact(4))
            {
                out.copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
            self.out
                .write_all(&row)
                .map_err(|e| format!("Could not write a BMP row: {e}"))?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.out
            .flush()
            .map_err(|e| format!("Could not finish the BMP: {e}"))
    }
}

/// An xy chromaticity as the `CIEXYZ` triple BMP stores an endpoint in.
///
/// The conversion is the definition of a chromaticity: `x` and `y` are `X`
/// and `Y` divided by their own sum with `Z`, so recovering the triple at
/// unit luminance is `X = x/y`, `Y = 1`, `Z = (1 - x - y)/y`. No matrix and
/// no white-point adaptation -- those belong to a profile, and this field
/// takes the primaries as they are.
fn endpoint(x: f32, y: f32) -> [u32; 3] {
    // `y` is never zero for a real primary; guarding it costs nothing and
    // turns a malformed table into black rather than an infinity.
    if y <= 0.0 {
        return [0, 0, 0];
    }
    let (x, y) = (f64::from(x), f64::from(y));
    let fixed = |value: f64| (value * FXPT2DOT30_ONE).round().max(0.0) as u32;
    [fixed(x / y), fixed(1.0), fixed((1.0 - x - y) / y)]
}

/// Writes the nine endpoint coordinates and the three gamma values that make
/// up the rest of a `BITMAPV4HEADER`.
///
/// Reached only when the space is not sRGB, which is the case `LCS_sRGB`
/// cannot describe. The endpoints are exact. The gamma is not, and cannot be:
/// the field is one exponent per channel, and none of these spaces is a pure
/// exponent -- sRGB and Display P3 share a curve with a linear toe, and
/// Rec. 2020's is a different power with a different toe. The exponent is
/// what the curve tends to away from black, which is the closest a single
/// number gets.
///
/// # Errors
///
/// Returns a message for PQ and HLG. Neither is a power law at all, so there
/// is no exponent to round to, and a BMP claiming one would misdescribe every
/// pixel rather than approximate them.
fn write_endpoints(
    header: &mut Vec<u8>,
    color: &ColorProfile,
) -> Result<(), String> {
    if !color.parametric_transfer() {
        return Err(format!(
            "A BMP describes a colour space with one gamma value per \
             channel, which cannot express this canvas's transfer function \
             (code point {})",
            color.cicp.transfer
        ));
    }

    let xy = &color.chromaticities;
    for (x, y) in [xy.red, xy.green, xy.blue] {
        for coordinate in endpoint(x, y) {
            header.extend_from_slice(&coordinate.to_le_bytes());
        }
    }

    let gamma = (f64::from(color.transfer.g) * GAMMA_16_16_ONE).round() as u32;
    for _ in 0..3 {
        header.extend_from_slice(&gamma.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{
        encode::{color::ColorProfile, start},
        export::ImageFormat,
        pixels::PixelColorSpace,
    };

    /// Two pixels by two: red, green on the top row, blue and a
    /// half-transparent white below.
    fn frame() -> Frame {
        Frame {
            pixels: vec![
                255, 0, 0, 255, 0, 255, 0, 255, // top row
                0, 0, 255, 255, 255, 255, 255, 128, // bottom row
            ],
            width: 2,
            height: 2,
            delay_ms: 0,
        }
    }

    fn encoded_in(space: PixelColorSpace) -> Vec<u8> {
        let spec = SequenceSpec {
            width: 2,
            height: 2,
            frames: 1,
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(space),
            space,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Bmp, &spec, &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(&frame()).expect("a well formed frame");
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    fn encoded() -> Vec<u8> {
        encoded_in(PixelColorSpace::Srgb)
    }

    #[test]
    fn the_header_says_what_the_pixels_are() {
        let b = encoded();
        assert_eq!(&b[..2], b"BM");
        let u32at =
            |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        assert_eq!(u32at(2) as usize, b.len(), "the size field is the size");
        assert_eq!(u32at(10), FILE_HEADER + V4_HEADER, "pixels start after");
        assert_eq!(u32at(14), V4_HEADER, "a V4 header, which has alpha masks");
        assert_eq!(u32at(18), 2, "width");
        assert_eq!(u32at(22), 2, "height, positive so the rows run upward");
        assert_eq!(u32at(30), BI_BITFIELDS);
        // The masks are what make the fourth channel alpha rather than
        // padding; a V3 header has nowhere to say it and readers drop it.
        // Asserted against the constants, which are what the writer uses:
        // restating the numbers here would only be a second place to get
        // them wrong, and would pass if both were wrong the same way.
        assert_eq!(u32at(54), MASK_RED);
        assert_eq!(u32at(58), MASK_GREEN);
        assert_eq!(u32at(62), MASK_BLUE);
        assert_eq!(u32at(66), MASK_ALPHA);
    }

    #[test]
    fn the_masks_agree_with_the_order_the_rows_are_written_in() {
        // The masks and the byte order are one decision written twice, in
        // two places that cannot see each other. This is the test that ties
        // them: each mask names which byte of a little-endian pixel its
        // channel occupies, so the mask's trailing zero bits are that byte's
        // index times eight.
        for (mask, byte, channel) in [
            (MASK_BLUE, 0, "blue"),
            (MASK_GREEN, 1, "green"),
            (MASK_RED, 2, "red"),
            (MASK_ALPHA, 3, "alpha"),
        ] {
            assert_eq!(
                mask.trailing_zeros() / 8,
                byte,
                "{channel} is written at byte {byte} of each pixel"
            );
            assert_eq!(mask.count_ones(), 8, "{channel} is one whole byte");
        }
        // And nothing overlaps or is left out.
        assert_eq!(
            MASK_RED | MASK_GREEN | MASK_BLUE | MASK_ALPHA,
            u32::MAX,
            "the four masks cover a pixel exactly once"
        );
    }

    #[test]
    fn a_wide_gamut_bmp_spells_its_primaries_out() {
        // `LCS_sRGB` says "these are sRGB" and nothing else; the calibrated
        // form is the only way a BMP describes any other space, and the
        // endpoint fields it needs were being written as zeroes.
        let bytes = encoded_in(PixelColorSpace::DisplayP3);
        let u32at = |i: usize| {
            u32::from_le_bytes([
                bytes[i],
                bytes[i + 1],
                bytes[i + 2],
                bytes[i + 3],
            ])
        };
        assert_eq!(u32at(70), LCS_CALIBRATED_RGB, "the endpoints are meant");

        // Display P3's red primary is x = 0.68, y = 0.32, which the header
        // holds as X = x/y and Z = (1-x-y)/y at unit luminance.
        let coordinate = |i: usize| f64::from(u32at(i)) / FXPT2DOT30_ONE;
        let (x, y) = (0.68_f64, 0.32);
        for (offset, expected) in
            [(74, x / y), (78, 1.0), (82, (1.0 - x - y) / y)]
        {
            assert!(
                (coordinate(offset) - expected).abs() < 1e-6,
                "endpoint at {offset}: {} rather than {expected}",
                coordinate(offset)
            );
        }

        // And the three gamma fields, which are 16.16 fixed point. They sit
        // after the nine endpoint coordinates, so at 74 + 36. Display P3
        // shares sRGB's curve, whose exponent is 2.4 away from black.
        for offset in [110, 114, 118] {
            let gamma = f64::from(u32at(offset)) / GAMMA_16_16_ONE;
            assert!((gamma - 2.4).abs() < 1e-4, "gamma at {offset}: {gamma}");
        }

        // The calibrated branch fills the bytes the sRGB branch pads with
        // zeroes, so both must land on the same header size -- a short or
        // long one would move where the pixels start.
        assert_eq!(
            u32at(10) as usize,
            (FILE_HEADER + V4_HEADER) as usize,
            "the pixels start where the header says"
        );
        assert_eq!(bytes.len(), encoded().len(), "same header, same file");
    }

    #[test]
    fn a_space_that_is_not_a_power_law_is_refused_rather_than_rounded() {
        // A BMP's gamma is one exponent per channel. PQ and HLG are not
        // exponents at all, so there is no number to round to and a header
        // claiming one would misdescribe every pixel in the file.
        for space in [PixelColorSpace::Rec2020Pq, PixelColorSpace::Rec2020Hlg] {
            let spec = SequenceSpec {
                width: 2,
                height: 2,
                frames: 1,
                loops: None,
                quality: 90.0,
                density: 1.0,
                color: ColorProfile::of(space),
                space,
            };
            let mut bytes = Cursor::new(Vec::new());
            let mut sink = start(ImageFormat::Bmp, &spec, &mut bytes)
                .expect("the spec is well formed");
            let refused = sink
                .write_frame(&frame())
                .expect_err("{space:?} has no gamma to write");
            assert!(
                refused.contains("transfer function"),
                "{space:?}: {refused}"
            );
        }
    }

    #[test]
    fn the_colour_space_field_names_one_the_format_defines() {
        // This field held `0x7357696E` -- `sWin`, the front of `LCS_sRGB`
        // welded to the back of `LCS_WINDOWS_COLOR_SPACE` -- under a comment
        // saying it was sRGB. It is checked here rather than trusted because
        // nothing else can catch it: readers ignore `bV4CSType` and assume
        // sRGB, so every viewer showed the right picture from a header
        // naming a colour space that does not exist.
        let b = encoded();
        let cs = u32::from_le_bytes([b[70], b[71], b[72], b[73]]);
        assert_eq!(cs, LCS_SRGB);
        assert_eq!(
            &cs.to_be_bytes(),
            b"sRGB",
            "the constant spells its own name, high byte first"
        );
        // `LCS_sRGB` makes the reader ignore what follows, so the endpoints
        // and the three gamma fields are zero on purpose.
        assert!(
            b[74..(FILE_HEADER + V4_HEADER) as usize]
                .iter()
                .all(|byte| *byte == 0),
            "endpoints and gamma are unused under LCS_sRGB"
        );
    }

    #[test]
    fn the_rows_run_bottom_to_top_in_bgra() {
        let b = encoded();
        let pixels = &b[(FILE_HEADER + V4_HEADER) as usize..];
        // The bottom row of the picture comes first, and each pixel is
        // blue, green, red, alpha rather than the other way round.
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255], "blue, first in the file");
        assert_eq!(
            &pixels[4..8],
            &[255, 255, 255, 128],
            "and the half-transparent white beside it, alpha intact"
        );
        assert_eq!(&pixels[8..12], &[0, 0, 255, 255], "then the top row: red");
        assert_eq!(&pixels[12..16], &[0, 255, 0, 255], "green");
    }
}
