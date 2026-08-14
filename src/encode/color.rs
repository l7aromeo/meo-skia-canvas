//! What an encoder is told about the colour of the pixels it is handed.
//!
//! Every container here declares colour in one of two vocabularies, and they
//! are the same fact spelled differently:
//!
//! - **Code points.** ITU-T H.273 assigns a small integer to each standard set
//!   of primaries and each standard transfer function. AVIF's `colr` box and
//!   PNG's `cICP` chunk carry exactly those integers. Four bytes, and no
//!   arithmetic anywhere.
//! - **Chromaticities.** TIFF and BMP predate H.273 by decades and describe
//!   colour the way the CIE diagram does: the xy coordinates of the red, green
//!   and blue primaries and of the white point.
//!
//! Both come out of [`PixelColorSpace::traits`], which is the one place the
//! two are paired, so an encoder that speaks either vocabulary is describing
//! the space the pixels were actually rendered in.
//!
//! Deliberately free of `skia_safe`. An encoder's job is the container; it
//! should not be able to reach a colour space object and convert anything,
//! because the conversion has already happened by the time a [`Frame`]
//! exists.
//!
//! [`Frame`]: super::Frame

use crate::pixels::PixelColorSpace;

/// A colour space as ITU-T H.273 numbers it.
///
/// The two values a container needs to name a standard space without
/// carrying a profile. `matrix_coefficients` is not here: it describes how a
/// container stores the channels rather than what the colours are, so it
/// belongs to whichever encoder is writing, not to the space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cicp {
    /// `colour_primaries`.
    pub primaries: u8,
    /// `transfer_characteristics`.
    pub transfer: u8,
}

/// The xy coordinates of a space's primaries and white point.
///
/// CIE 1931 chromaticities, which is what TIFF's `PrimaryChromaticities` and
/// `WhitePoint` tags hold and what BMP's `bV4Endpoints` field is derived
/// from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Chromaticities {
    pub red: (f32, f32),
    pub green: (f32, f32),
    pub blue: (f32, f32),
    pub white: (f32, f32),
}

/// A parametric transfer function, in the seven coefficients ICC and skcms
/// both use.
///
/// The curve is `(a * x + b)^g + e` above `d` and `c * x + f` below it, which
/// is the shape sRGB needs and a pure exponent is a special case of.
///
/// A negative `g` is skcms' marker for a curve that is *not* parametric at
/// all -- PQ and HLG both arrive that way, with the other fields repurposed.
/// [`ColorProfile::parametric_transfer`] is what asks that question, because
/// a container that can only express a parametric curve has to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TransferFunction {
    pub g: f32,
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

/// Everything an encoder can be told about the colour of a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColorProfile {
    pub cicp: Cicp,
    pub chromaticities: Chromaticities,
    pub transfer: TransferFunction,
}

impl ColorProfile {
    /// The description of `space`.
    pub(crate) fn of(space: PixelColorSpace) -> Self {
        let traits = space.traits();
        let xy = traits.chromaticities;
        Self {
            cicp: Cicp {
                // `as u8` rather than a literal: these enums are `#[repr(u8)]`
                // with the H.273 values as their discriminants, so Skia's own
                // name for a code point is the code point.
                primaries: traits.primaries as u8,
                transfer: traits.transfer as u8,
            },
            chromaticities: Chromaticities {
                red: (xy.rx, xy.ry),
                green: (xy.gx, xy.gy),
                blue: (xy.bx, xy.by),
                white: (xy.wx, xy.wy),
            },
            transfer: TransferFunction {
                g: traits.transfer_fn.g,
                a: traits.transfer_fn.a,
                b: traits.transfer_fn.b,
                c: traits.transfer_fn.c,
                d: traits.transfer_fn.d,
                e: traits.transfer_fn.e,
                f: traits.transfer_fn.f,
            },
        }
    }

    /// Whether this space's transfer function is one a parametric curve can
    /// hold.
    ///
    /// False for PQ and HLG. Neither is a power law with a linear toe -- PQ
    /// is a rational function of a power, HLG is a hybrid with a logarithmic
    /// upper half -- and skcms signals that by handing back a negative
    /// exponent instead of coefficients. A container that describes colour
    /// with code points does not care; one that can only write a curve has
    /// nothing honest to write.
    pub(crate) fn parametric_transfer(&self) -> bool {
        self.transfer.g >= 0.0
    }

    /// Whether this is plain sRGB, which most containers assume when a file
    /// says nothing.
    pub(crate) fn is_srgb(&self) -> bool {
        *self == Self::of(PixelColorSpace::Srgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_points_are_the_ones_h273_assigns() {
        // Spot-checked against the standard rather than against the enum
        // that produced them, because "the table matches itself" is the one
        // thing a table cannot prove. These four are the values H.273
        // tabulates: BT.709 primaries are 1, Display P3's are 12, BT.2020's
        // are 9, and the sRGB transfer function is 13.
        let srgb = ColorProfile::of(PixelColorSpace::Srgb);
        assert_eq!(
            srgb.cicp,
            Cicp {
                primaries: 1,
                transfer: 13
            }
        );
        assert_eq!(
            ColorProfile::of(PixelColorSpace::DisplayP3).cicp,
            Cicp {
                primaries: 12,
                transfer: 13
            }
        );
        assert_eq!(
            ColorProfile::of(PixelColorSpace::Rec2020Pq).cicp,
            Cicp {
                primaries: 9,
                transfer: 16
            }
        );
        assert_eq!(
            ColorProfile::of(PixelColorSpace::Rec2020Hlg).cicp,
            Cicp {
                primaries: 9,
                transfer: 18
            }
        );
    }

    #[test]
    fn pq_and_hlg_are_the_two_no_parametric_curve_can_hold() {
        for space in [PixelColorSpace::Rec2020Pq, PixelColorSpace::Rec2020Hlg] {
            let profile = ColorProfile::of(space);
            assert!(
                !profile.parametric_transfer(),
                "{space:?} reported a parametric curve: {:?}",
                profile.transfer
            );
        }
        for space in [
            PixelColorSpace::Srgb,
            PixelColorSpace::SrgbLinear,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::DisplayP3Linear,
            PixelColorSpace::Rec2020,
            PixelColorSpace::Rec2020Linear,
        ] {
            assert!(
                ColorProfile::of(space).parametric_transfer(),
                "{space:?} should be writable as a curve"
            );
        }
    }

    #[test]
    fn only_srgb_answers_to_being_srgb() {
        assert!(ColorProfile::of(PixelColorSpace::Srgb).is_srgb());
        // Same primaries, different curve -- and the other way round.
        assert!(!ColorProfile::of(PixelColorSpace::SrgbLinear).is_srgb());
        assert!(!ColorProfile::of(PixelColorSpace::DisplayP3).is_srgb());
    }
}
