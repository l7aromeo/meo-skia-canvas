//! The colour space a BMP's `BITMAPV4HEADER` states.
//!
//! Skia decodes BMP pixels correctly and throws the colour description away:
//! that path has no ICC reader and does not interpret a V4 header's
//! endpoints. So a Display P3 BMP arrived tagged sRGB and its pixels were
//! passed through untouched -- a mid green written as `[124, 203, 149]` came
//! back as `[124, 203, 149]` where PNG and WebP of the same canvas both gave
//! `[94, 205, 144]`.
//!
//! This reads the header back and names the space, so the caller can label
//! the image Skia returned. It is not a decoder: Skia's pixels are correct
//! and are not touched.
//!
//! **A file states its space three different ways and only one needs work.**
//! `LCS_sRGB` is a name, and so is silence -- a DIB header shorter than a
//! `BITMAPV4HEADER` has no colour fields at all, and sRGB is what a reader
//! assumes. `LCS_CALIBRATED_RGB` is the case this exists for: the endpoints
//! and gamma that follow are the space, spelled out.
//!
//! [`crate::encode::bmp`] writes that header and is the reference for the
//! field offsets; the constants it defines are used here rather than copied,
//! so the two halves cannot disagree about where a field sits or what a
//! value means.

use crate::{
    encode::bmp::{
        FILE_HEADER, FXPT2DOT30_ONE, GAMMA_16_16_ONE, LCS_CALIBRATED_RGB,
        LCS_SRGB, V4_HEADER,
    },
    pixels::PixelColorSpace,
};

/// `bV4CSType`'s offset from the start of the file.
///
/// The `BITMAPFILEHEADER` is 14 bytes and the field sits 56 into the
/// `BITMAPV4HEADER`, after the dimensions, the masks and the rest.
const CS_TYPE_AT: usize = FILE_HEADER as usize + 56;

/// `bV4Endpoints`, the nine `FXPT2DOT30` coordinates, immediately after
/// `bV4CSType`.
const ENDPOINTS_AT: usize = CS_TYPE_AT + 4;

/// `bV4GammaRed`, the first of three 16.16 fixed-point exponents, after the
/// nine endpoint coordinates.
const GAMMA_AT: usize = ENDPOINTS_AT + 9 * 4;

/// How far a recovered chromaticity may sit from a named space's own.
///
/// Endpoints are stored as fixed point, so exact equality would be a test of
/// the writer's rounding rather than of the space. The number is chosen
/// between two measured bounds. Recovering `x` and `y` from the triple this
/// crate writes moves them by about `2e-10`, so anything above that admits
/// our own files; and the closest two named spaces ever come on a primary
/// this can read is `0.028` -- Display P3's green against Rec. 2020's. At
/// `1e-4` the window is five orders of magnitude above the first and 280
/// times below the second, so it cannot both admit a file and confuse two
/// spaces.
const CHROMATICITY_TOLERANCE: f64 = 1e-4;

/// How far a recovered gamma may sit from a named space's exponent.
///
/// The field is 16.16 fixed point, so 2.4 stores as 2.399993896 -- a
/// quantisation of about `1.5e-5`. The only exponents that reach a BMP are
/// 1.0 and 2.4, which are 1.4 apart. `1e-3` is 65 times the quantisation and
/// 1400 times below the gap.
const GAMMA_TOLERANCE: f64 = 1e-3;

/// The space a BMP's header names, if this crate has a name for it.
///
/// `None` when the file is not a BMP, carries no colour fields, or describes
/// a space outside [`PixelColorSpace`] -- an embedded ICC profile, a
/// `LCS_WINDOWS_COLOR_SPACE`, or primaries this crate cannot name. The
/// caller leaves Skia's own answer alone in that case, which is what it does
/// for every other format.
pub(crate) fn space_of(bytes: &[u8]) -> Option<PixelColorSpace> {
    // Cheap enough to check here rather than at the call site, and it keeps
    // the "is this even a BMP" question in the module that knows the format.
    if bytes.len() < (FILE_HEADER + V4_HEADER) as usize
        || !bytes.starts_with(b"BM")
    {
        return None;
    }
    // A DIB header shorter than a `BITMAPV4HEADER` stops before `bV4CSType`,
    // so there is nothing to read rather than something to disbelieve.
    if u32_at(bytes, FILE_HEADER as usize)? < V4_HEADER {
        return None;
    }

    match u32_at(bytes, CS_TYPE_AT)? {
        LCS_SRGB => Some(PixelColorSpace::Srgb),
        LCS_CALIBRATED_RGB => calibrated(bytes),
        // `LCS_WINDOWS_COLOR_SPACE`, `PROFILE_LINKED` and `PROFILE_EMBEDDED`
        // all land here. The first names a space this crate has no entry
        // for; the other two put the description outside the header, which
        // is an ICC profile and a different job.
        _ => None,
    }
}

/// The space the endpoints and gamma describe, looked up rather than built.
///
/// **Looked up against the spaces this crate names, in the shape
/// [`PixelColorSpace::of_cicp`] uses for PNG and AVIF, and for a reason
/// stronger than consistency: a space built from these fields would be the
/// wrong space.** The gamma field is one exponent per channel, and neither
/// sRGB nor Display P3 is a pure exponent -- both are a power law with a
/// linear toe near black. A space assembled from a 2.4 read out of this
/// header would carry a pure 2.4 curve, which is not the curve the file was
/// written from and differs most in the shadows. Matching returns the real
/// transfer function instead, because the named space carries it.
///
/// So the endpoints are used as an identifier and never as a definition,
/// which is also why a file this crate cannot name yields `None` rather than
/// an approximation assembled here.
fn calibrated(bytes: &[u8]) -> Option<PixelColorSpace> {
    let red = chromaticity(bytes, 0)?;
    let green = chromaticity(bytes, 1)?;
    let gamma = f64::from(u32_at(bytes, GAMMA_AT)?) / GAMMA_16_16_ONE;

    PixelColorSpace::all().find(|space| {
        let traits = space.traits();
        let named = traits.chromaticities;
        // PQ and HLG carry a negative exponent -- skcms' marker for a curve
        // that is not a power law -- so they are excluded by arithmetic
        // rather than by a case here, a gamma field being unsigned. That
        // agrees with the encoder, which refuses to write either.
        close(gamma, f64::from(traits.transfer_fn.g), GAMMA_TOLERANCE)
            && matches_primary(red, (named.rx, named.ry))
            && matches_primary(green, (named.gx, named.gy))
    })
}

/// Whether a recovered chromaticity is the named one.
fn matches_primary(found: (f64, f64), named: (f32, f32)) -> bool {
    close(found.0, f64::from(named.0), CHROMATICITY_TOLERANCE)
        && close(found.1, f64::from(named.1), CHROMATICITY_TOLERANCE)
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

/// The `x` and `y` of endpoint `index`, from the `CIEXYZ` triple it is
/// stored as.
///
/// A chromaticity is a ratio, so `x` and `y` come back by dividing each
/// coordinate by the sum of its own three. **That is what makes this
/// independent of how the writer scaled the triple**, and it has to be: a
/// `CIEXYZ` endpoint is read two ways in the wild -- the primary at unit
/// luminance, and the column of the RGB-to-XYZ matrix, which is what
/// [`crate::encode::bmp`] writes and what sums to the white point. The two
/// differ by a per-primary factor that cancels here.
///
/// **Only red and green are read, and blue is deliberately not.** sRGB and
/// Display P3 share a blue primary exactly, so blue never separated them,
/// while red and green stay 0.028 apart at their closest. Blue is also where
/// a file written by this crate before its normalization was fixed carries
/// `0xFFFFFFFF`: at unit luminance blue's `Z` is 13.2 for sRGB and Display
/// P3 and 17.9 for Rec. 2020, and an `FXPT2DOT30` holds nothing above 4.
/// Skipping it reads those files correctly as well, which is worth keeping
/// rather than a reason on its own.
fn chromaticity(bytes: &[u8], index: usize) -> Option<(f64, f64)> {
    let at = ENDPOINTS_AT + index * 12;
    let x = f64::from(u32_at(bytes, at)?) / FXPT2DOT30_ONE;
    let y = f64::from(u32_at(bytes, at + 4)?) / FXPT2DOT30_ONE;
    let z = f64::from(u32_at(bytes, at + 8)?) / FXPT2DOT30_ONE;
    let sum = x + y + z;
    // A primary summing to zero is a header of zeroes, which is what
    // `LCS_sRGB` leaves behind. Reached only when `bV4CSType` claimed the
    // endpoints were meant, so it is a malformed file rather than a case.
    (sum > 0.0).then(|| (x / sum, y / sum))
}

/// The little-endian `u32` at `offset`, or `None` past the end.
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{
        encode::{
            Frame, FrameDepth, Pixels, SequenceSpec, color::ColorProfile, start,
        },
        export::{ChromaSampling, ImageFormat},
    };

    /// A BMP this crate wrote for `space`, which is the only writer whose
    /// header shape this has to read.
    fn encoded_in(space: PixelColorSpace) -> Vec<u8> {
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width: 2,
            height: 2,
            frames: 1,
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(space),
            space,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let frame = Frame {
            pixels: Pixels::Eight(vec![255; 16]),
            width: 2,
            height: 2,
            delay_ms: 0,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Bmp, &spec, &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(&frame).expect("a well formed frame");
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    /// Every space the encoder will write comes back as the one it wrote.
    ///
    /// Round-tripped through this crate's own writer rather than against
    /// pinned header bytes, so the two halves are tested against each other:
    /// a change to either that the other does not follow fails here.
    #[test]
    fn a_written_space_reads_back() {
        for space in [
            PixelColorSpace::Srgb,
            PixelColorSpace::SrgbLinear,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::DisplayP3Linear,
            PixelColorSpace::Rec2020,
            PixelColorSpace::Rec2020Linear,
        ] {
            assert_eq!(
                space_of(&encoded_in(space)),
                Some(space),
                "{space:?} round-trips through its own header"
            );
        }
    }

    /// The linear and non-linear halves of one primary set are told apart by
    /// gamma alone, which is the only thing separating them.
    ///
    /// Without this the suite would pass with the gamma comparison deleted:
    /// three primary sets cover six spaces, so matching on endpoints alone
    /// answers every case above with whichever of the pair comes first.
    #[test]
    fn gamma_separates_two_spaces_sharing_primaries() {
        for (curved, linear) in [
            (PixelColorSpace::Srgb, PixelColorSpace::SrgbLinear),
            (PixelColorSpace::DisplayP3, PixelColorSpace::DisplayP3Linear),
            (PixelColorSpace::Rec2020, PixelColorSpace::Rec2020Linear),
        ] {
            assert_eq!(space_of(&encoded_in(curved)), Some(curved));
            assert_eq!(space_of(&encoded_in(linear)), Some(linear));
        }
    }

    /// A header with no colour fields is not a header describing sRGB badly.
    #[test]
    fn a_header_too_short_to_carry_colour_names_nothing() {
        let mut bmp = encoded_in(PixelColorSpace::DisplayP3);
        // `BITMAPINFOHEADER`, which stops before `bV4CSType`.
        bmp[FILE_HEADER as usize..FILE_HEADER as usize + 4]
            .copy_from_slice(&40u32.to_le_bytes());
        assert_eq!(space_of(&bmp), None);
    }

    /// A `bV4CSType` this crate has no entry for leaves Skia's answer alone
    /// rather than guessing at the endpoints beside it.
    #[test]
    fn an_unnamed_cstype_names_nothing() {
        let mut bmp = encoded_in(PixelColorSpace::DisplayP3);
        // `LCS_WINDOWS_COLOR_SPACE`, which `wingdi.h` spells `Win `.
        bmp[CS_TYPE_AT..CS_TYPE_AT + 4]
            .copy_from_slice(&0x5769_6E20u32.to_le_bytes());
        assert_eq!(space_of(&bmp), None);
    }

    /// Endpoints that name no space this crate knows yield `None`, not the
    /// nearest match.
    ///
    /// Both primaries are moved, separately, and the second is the reason
    /// this is a loop. Red alone already separates the three primary sets --
    /// 0.640, 0.680 and 0.708 -- so a suite that only ever moved red passed
    /// with the green comparison deleted, which made that comparison
    /// decoration. Green is what refuses a file whose red happens to land on
    /// a space this crate names and whose green does not.
    #[test]
    fn primaries_outside_the_named_set_name_nothing() {
        for (primary, index) in [("red", 0), ("green", 1)] {
            let mut bmp = encoded_in(PixelColorSpace::DisplayP3);
            // A tenth off, which is far outside the tolerance and lands
            // between the named spaces rather than on another one.
            let at = ENDPOINTS_AT + index * 12;
            let moved = u32_at(&bmp, at).expect("an X coordinate")
                + (0.1 * FXPT2DOT30_ONE) as u32;
            bmp[at..at + 4].copy_from_slice(&moved.to_le_bytes());
            assert_eq!(space_of(&bmp), None, "{primary} moved off every space");
        }
    }

    /// Bytes that are not a BMP are not read as one.
    #[test]
    fn other_bytes_name_nothing() {
        assert_eq!(space_of(b"not a bitmap at all, but long enough"), None);
        assert_eq!(space_of(&[]), None);
    }
}
