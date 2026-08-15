//! APNG, which Skia can write no better than it can read.
//!
//! Its Rust PNG decoder is compiled out of skia-safe 0.99 -- the module is
//! `#[cfg(any())]` -- and the C++ one handles the still form only. Checked
//! rather than assumed, and against this crate's own output: a file written
//! here carries an `acTL` chunk and one `fcTL` per frame, and `Image` reads
//! it back as a single frame with no delay. So the `png` crate is both the
//! encoder here and the only thing able to read what it wrote.
//!
//! An earlier version of this note offered a different check -- that no
//! `acTL`, `fcTL` or `fdAT` string appears anywhere in a built addon. That
//! was true until this module existed, and linking the `png` crate put all
//! three in the binary as its own chunk-name tables. The claim was falsified
//! by the commit that made it.

use super::{
    Frame, FrameDepth, FrameEncoder, FrameSink, SequenceSpec, Sink,
    color::ColorProfile,
};
use crate::pixels::PixelColorSpace;
use png::{
    BitDepth, ColorType, Encoder, ScaledFloat, SourceChromaticities, Writer,
    chunk,
};

/// `matrix_coefficients` for a `cICP` chunk.
///
/// Identity, and not a choice: the PNG specification fixes it, because RGB is
/// the only colour model PNG has. H.273 assigns identity the value 0.
const CICP_MATRIX_IDENTITY: u8 = 0;

/// `video_full_range_flag` for a `cICP` chunk.
///
/// Full range, meaning 0 to 255 rather than the 16-to-235 studio swing. A
/// canvas hands back full-range pixels and PNG has no narrow-range form.
const CICP_FULL_RANGE: u8 = 1;

/// The `gAMA` value the PNG specification names for the sRGB transfer
/// function.
///
/// PNG stores gamma scaled by 100000, so this is 0.45455 -- one over 2.2,
/// not one over the 2.4 that appears in the curve's own formula. The
/// difference is the linear toe near black, which pulls the effective
/// exponent down; 11.3.2.5 of the specification gives this exact number
/// alongside the `sRGB` chunk, so it is quoted rather than derived.
const SRGB_GAMA: u32 = 45455;

/// The `gAMA` value for a linear transfer function: 1.0, scaled by 100000.
const LINEAR_GAMA: u32 = 100_000;

pub(crate) struct Apng;

impl FrameEncoder for Apng {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        let mut encoder = Encoder::new(out, spec.width, spec.height);
        encoder.set_color(ColorType::Rgba);
        // PNG carries eight or sixteen bits a channel, and a float canvas
        // has more than eight to give. Skia's still-PNG path already writes
        // sixteen from one; an APNG that narrowed would make the same
        // drawing shallower for being animated.
        let depth = match spec.depth {
            FrameDepth::Sixteen => BitDepth::Sixteen,
            FrameDepth::Eight => BitDepth::Eight,
        };
        encoder.set_depth(depth);
        // Full colour with an alpha channel, which is the whole reason to
        // reach for APNG over GIF: no palette, no one-bit alpha.
        //
        // The `sRGB` chunk only where the pixels are sRGB. It was written
        // unconditionally, which was true while every frame was converted to
        // sRGB on the way in and became a lie the moment they stopped being:
        // a Display P3 animation carrying an `sRGB` chunk is a file whose
        // own metadata contradicts its pixels.
        if spec.color.is_srgb() {
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        } else {
            describe_the_old_way(&mut encoder, &spec.color);
        }

        // A single page is a still PNG. Writing the animation chunks for it
        // would produce a file every decoder handles and none needs, and
        // would make `toBuffer("apng")` of a one-page canvas something other
        // than a PNG.
        //
        // The count has to be right before the header goes out: `acTL`
        // carries it and must precede the first `IDAT`. A canvas knows how
        // many pages it has, which is what makes this writable in one pass.
        let animated = spec.frames > 1;
        if animated {
            encoder
                .set_animated(spec.frames as u32, spec.loops.unwrap_or(0))
                .map_err(|e| format!("Could not start an APNG: {e}"))?;
        }

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Could not write the PNG header: {e}"))?;
        write_cicp(&mut writer, &spec.color)?;

        Ok(Box::new(ApngSink {
            writer,
            animated,
            depth,
        }))
    }
}

/// Adds `cHRM` and, where it can be stated honestly, `gAMA`.
///
/// `cICP` is exact and says everything, and is also from the third edition
/// of the PNG specification, published in 2022. A reader older than that
/// skips the chunk -- it is ancillary, so skipping is correct behaviour --
/// and falls back to assuming sRGB, which for a Display P3 file is the wrong
/// gamut with no warning.
///
/// `cHRM` is the answer to that. It has been in PNG since 1996, the `png`
/// crate writes it, and it holds the same primaries `cICP` names, as the xy
/// coordinates they are. So a modern reader takes the code points and an old
/// one still gets the gamut right.
///
/// `gAMA` is the other half and is only written where a single exponent is
/// the truth. Two cases qualify. The sRGB transfer function gets the number
/// the specification itself names for it -- 45455, which is 1/2.2 rather
/// than 1/2.4, because the curve's linear toe makes its effective exponent
/// lower than its formal one. A linear space gets 1.0. Everything else --
/// Rec. 709's curve, PQ, HLG -- is not a power law, and PNG's own advice is
/// to leave the chunk out rather than approximate, since a wrong `gAMA` is
/// worse than none.
pub(super) fn describe_the_old_way<W: std::io::Write>(
    encoder: &mut Encoder<'_, W>,
    color: &ColorProfile,
) {
    let xy = &color.chromaticities;
    encoder.set_source_chromaticities(SourceChromaticities::new(
        xy.white, xy.red, xy.green, xy.blue,
    ));

    let srgb_curve = ColorProfile::of(PixelColorSpace::Srgb).transfer;
    let linear_curve = ColorProfile::of(PixelColorSpace::SrgbLinear).transfer;
    let gamma = if color.transfer == srgb_curve {
        SRGB_GAMA
    } else if color.transfer == linear_curve {
        LINEAR_GAMA
    } else {
        return;
    };
    encoder.set_source_gamma(ScaledFloat::from_scaled(gamma));
}

/// Writes the `cICP` chunk naming the space these frames are in.
///
/// Four bytes -- primaries, transfer function, matrix, range -- and the first
/// two come straight from the table in `pixels`, which is where Skia was
/// asked for the space in the first place. That is the whole appeal over an
/// embedded profile: there is no arithmetic to get wrong, and it can say
/// PQ and HLG, which an ICC parametric curve cannot.
///
/// Written by hand because the `png` crate reads this chunk and does not
/// write it: `Info::coding_independent_code_points` is filled in by the
/// decoder and ignored by the encoder, so setting it would have compiled and
/// produced nothing. `Writer::write_chunk` is called before any image data,
/// which is where PNG requires `cICP` to sit.
///
/// Skipped for plain sRGB, which the `sRGB` chunk above already states in the
/// form every reader has understood for twenty-five years.
pub(super) fn write_cicp<W: std::io::Write>(
    writer: &mut Writer<W>,
    color: &ColorProfile,
) -> Result<(), String> {
    if color.is_srgb() {
        return Ok(());
    }
    writer
        .write_chunk(
            chunk::cICP,
            &[
                color.cicp.primaries,
                color.cicp.transfer,
                CICP_MATRIX_IDENTITY,
                CICP_FULL_RANGE,
            ],
        )
        .map_err(|e| format!("Could not write the PNG colour space: {e}"))
}

struct ApngSink<'a> {
    depth: BitDepth,
    writer: Writer<&'a mut dyn Sink>,
    animated: bool,
}

impl FrameSink for ApngSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        if self.animated {
            let (numerator, denominator) = delay_fraction(frame.delay_ms);
            self.writer
                .set_frame_delay(numerator, denominator)
                .map_err(|e| format!("Could not set a frame's delay: {e}"))?;
        }
        self.writer
            .write_image_data(&match self.depth {
                // PNG is big-endian, and the crate takes bytes either way.
                BitDepth::Sixteen => frame
                    .sixteen()
                    .iter()
                    .flat_map(|value| value.to_be_bytes())
                    .collect::<Vec<u8>>(),
                _ => frame.eight().into_owned(),
            })
            .map_err(|e| format!("Could not write a PNG frame: {e}"))
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.writer
            .finish()
            .map_err(|e| format!("Could not finish the PNG: {e}"))
    }
}

/// A delay in milliseconds as the numerator and denominator of a fraction of
/// a second, which is how APNG stores one.
///
/// Both fields are sixteen bits, so thousandths only reach 65.535 seconds.
/// Writing the milliseconds straight into the numerator wrapped past that: a
/// 70-second frame came back as 4.46 seconds, and 65536ms came back as zero
/// -- which is not even a long delay, it is the shortest one there is. It was
/// reachable from JavaScript, where `fps` is checked only for being positive
/// and finite, so any rate under about 0.0153 was silently wrong.
///
/// So the tick coarsens instead of wrapping. Thousandths as far as they go,
/// then hundredths, tenths, whole seconds -- and a delay past what even whole
/// seconds can hold stops at the longest expressible rather than folding back
/// to a short one.
fn delay_fraction(delay_ms: u32) -> (u16, u16) {
    for denominator in [1000u64, 100, 10, 1] {
        // In `u64`: `delay_ms * 1000` overflows `u32` well before
        // `delay_ms` itself does.
        let numerator = u64::from(delay_ms) * denominator / 1000;
        if numerator <= u16::MAX as u64 {
            return (numerator as u16, denominator as u16);
        }
    }
    (u16::MAX, 1)
}

#[cfg(test)]
mod tests {
    use crate::{
        encode::{FrameDepth, Pixels},
        export::ChromaSampling,
    };
    use png::Decoder;
    use std::io::Cursor;

    use super::*;
    use crate::{
        encode::color::ColorProfile, export::ImageFormat,
        pixels::PixelColorSpace,
    };

    /// `count` frames, one solid colour each, two pixels by one.
    fn frames(count: u32, delay_ms: u32) -> Vec<Frame> {
        (0..count)
            .map(|index| {
                let shade = (index * 40) as u8;
                Frame {
                    pixels: Pixels::Eight(vec![
                        shade, 0, 0, 255, shade, 0, 0, 128,
                    ]),
                    width: 2,
                    height: 1,
                    delay_ms: delay_ms + index,
                }
            })
            .collect()
    }

    /// Encodes `frames` through the sink seam, as a caller would.
    fn encoded_in(
        frames: &[Frame],
        loops: Option<u32>,
        space: PixelColorSpace,
    ) -> Vec<u8> {
        // SAFETY: every frame in these tests is well formed, so the checks
        // in `start` cannot fire.
        let first = &frames[0];
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            width: first.width,
            height: first.height,
            frames: frames.len(),
            loops,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(space),
            space,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink =
                super::super::start(ImageFormat::Apng, &spec, &mut bytes)
                    .expect("the spec is well formed");
            for frame in frames {
                sink.write_frame(frame).expect("a well formed frame");
            }
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    fn encoded(frames: &[Frame], loops: Option<u32>) -> Vec<u8> {
        encoded_in(frames, loops, PixelColorSpace::Srgb)
    }

    /// The payload of the first chunk called `name`, if the file has one.
    fn chunk_named(bytes: &[u8], name: &[u8; 4]) -> Option<Vec<u8>> {
        // Chunks run length, type, data, CRC from byte 8 -- after the
        // signature -- so the length sits four bytes before the name.
        let at = bytes.windows(4).position(|window| window == name)?;
        let length = u32::from_be_bytes([
            bytes[at - 4],
            bytes[at - 3],
            bytes[at - 2],
            bytes[at - 1],
        ]) as usize;
        Some(bytes[at + 4..at + 4 + length].to_vec())
    }

    #[test]
    fn a_wide_gamut_animation_says_which_space_it_is_in() {
        // The four bytes of a `cICP` chunk: primaries, transfer function,
        // matrix coefficients and range. Display P3 is 12 and the sRGB
        // transfer function is 13, both by ITU-T H.273.
        let bytes =
            encoded_in(&frames(2, 100), None, PixelColorSpace::DisplayP3);
        assert_eq!(
            chunk_named(&bytes, b"cICP"),
            Some(vec![12, 13, CICP_MATRIX_IDENTITY, CICP_FULL_RANGE])
        );
        // And it must not also claim to be sRGB, which is the contradiction
        // this file used to ship: the `sRGB` chunk went out unconditionally.
        assert_eq!(chunk_named(&bytes, b"sRGB"), None);
    }

    #[test]
    fn a_reader_too_old_for_cicp_is_still_told_the_gamut() {
        // `cICP` is from the 2022 edition of the specification. Anything
        // older skips it -- correctly, it is an ancillary chunk -- and would
        // assume sRGB, so a Display P3 file would show the wrong gamut with
        // nothing to warn on. `cHRM` has been in PNG since 1996 and carries
        // the same primaries as coordinates.
        let bytes =
            encoded_in(&frames(2, 100), None, PixelColorSpace::DisplayP3);
        let chrm = chunk_named(&bytes, b"cHRM").expect("the old description");
        // Eight big-endian values scaled by 100000, white first.
        let at = |i: usize| {
            u32::from_be_bytes([
                chrm[i * 4],
                chrm[i * 4 + 1],
                chrm[i * 4 + 2],
                chrm[i * 4 + 3],
            ])
        };
        assert_eq!(chrm.len(), 32);
        // Within one part in a hundred thousand rather than exact: the `png`
        // crate's `ScaledFloat` truncates where it multiplies, so `0.265`
        // stored as an `f32` comes back 26499 rather than 26500. That is a
        // ten-millionth of a chromaticity coordinate and cannot be seen; it
        // is worth stating rather than hiding behind a loose comparison.
        let close = |got: u32, want: u32, what: &str| {
            assert!(got.abs_diff(want) <= 1, "{what}: {got} against {want}");
        };
        close(at(0), 31270, "D65 white x");
        close(at(1), 32900, "D65 white y");
        close(at(2), 68000, "Display P3 red x");
        close(at(3), 32000, "Display P3 red y");
        close(at(4), 26500, "Display P3 green x");
        close(at(5), 69000, "Display P3 green y");

        // And `gAMA`, which for the sRGB curve is the number the
        // specification names rather than one over the curve's own exponent.
        let gama = chunk_named(&bytes, b"gAMA").expect("a stated gamma");
        assert_eq!(
            u32::from_be_bytes([gama[0], gama[1], gama[2], gama[3]]),
            SRGB_GAMA
        );
    }

    #[test]
    fn a_curve_that_is_not_a_power_law_states_no_gamma_at_all() {
        // PQ and HLG are not exponents, and PNG's own advice is that a wrong
        // `gAMA` is worse than none. The primaries are still exact, and
        // `cICP` still says the whole truth for a reader that knows it.
        for space in [PixelColorSpace::Rec2020Pq, PixelColorSpace::Rec2020Hlg] {
            let bytes = encoded_in(&frames(2, 100), None, space);
            assert!(chunk_named(&bytes, b"gAMA").is_none(), "{space:?}");
            assert!(chunk_named(&bytes, b"cHRM").is_some(), "{space:?}");
            assert!(chunk_named(&bytes, b"cICP").is_some(), "{space:?}");
        }
    }

    #[test]
    fn an_srgb_animation_says_so_the_way_it_always_has() {
        // No `cICP` for the space every reader assumes anyway -- the `sRGB`
        // chunk is two bytes rather than sixteen and is understood by
        // decoders a quarter-century older than H.273.
        let bytes = encoded(&frames(2, 100), None);
        assert_eq!(chunk_named(&bytes, b"cICP"), None);
        assert!(chunk_named(&bytes, b"sRGB").is_some());
    }

    #[test]
    fn the_space_is_named_before_any_pixels_are_written() {
        // `cICP` is only meaningful ahead of the image data, and the `png`
        // crate hands back a writer that has already emitted the header --
        // so this checks the chunk landed in the window between them rather
        // than after the first `IDAT`.
        let bytes = encoded_in(&frames(2, 100), None, PixelColorSpace::Rec2020);
        let cicp = bytes
            .windows(4)
            .position(|window| window == b"cICP")
            .expect("the chunk is present");
        let idat = bytes
            .windows(4)
            .position(|window| window == b"IDAT")
            .expect("a PNG has pixels");
        assert!(cicp < idat, "cICP at {cicp}, IDAT at {idat}");
    }

    #[test]
    fn an_animation_reads_back_with_the_frames_and_delays_it_was_given() {
        let written = frames(3, 100);
        let bytes = encoded(&written, None);

        let mut reader = Decoder::new(Cursor::new(&bytes))
            .read_info()
            .expect("a PNG this crate wrote");
        let control = reader
            .info()
            .animation_control
            .expect("three frames make an animation");
        assert_eq!(control.num_frames, 3);
        assert_eq!(control.num_plays, 0, "zero is how APNG spells forever");

        let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0)];
        for expected in &written {
            let info = reader.next_frame(&mut buffer).expect("a frame");
            let delay = reader
                .info()
                .frame_control
                .expect("an animated frame is controlled");
            // Milliseconds over a thousand, kept as the fraction it is.
            assert_eq!(delay.delay_den, 1000);
            assert_eq!(u32::from(delay.delay_num), expected.delay_ms);
            // And the pixels survive whole -- full colour, real alpha, which
            // is the reason to reach for APNG rather than GIF.
            assert_eq!(&buffer[..info.buffer_size()], &expected.eight()[..]);
        }
    }

    #[test]
    fn a_long_delay_coarsens_its_tick_rather_than_wrapping() {
        // Thousandths of a second hold 65.535s in sixteen bits. Past that
        // the numerator used to wrap: 70s became 4.46s, and 65536ms became
        // zero -- the shortest delay there is, from the longest request.
        assert_eq!(delay_fraction(0), (0, 1000));
        assert_eq!(delay_fraction(1), (1, 1000));
        assert_eq!(delay_fraction(100), (100, 1000));
        assert_eq!(delay_fraction(65535), (65535, 1000));
        // One millisecond further and the tick drops to hundredths, which
        // costs the sub-10ms remainder and nothing else.
        assert_eq!(delay_fraction(65536), (6553, 100));
        assert_eq!(delay_fraction(70000), (7000, 100));
        assert_eq!(delay_fraction(3_600_000), (36000, 10));
        assert_eq!(delay_fraction(60_000_000), (60000, 1));
        // And past 18 hours it stops at the longest that can be written,
        // rather than folding back around to a short one.
        assert_eq!(delay_fraction(u32::MAX), (u16::MAX, 1));

        // Every one of those still says the number of seconds asked for,
        // to within the tick it landed on.
        for delay_ms in [0u32, 1, 100, 65535, 65536, 70000, 3_600_000] {
            let (numerator, denominator) = delay_fraction(delay_ms);
            let written = f64::from(numerator) / f64::from(denominator);
            let asked = f64::from(delay_ms) / 1000.0;
            assert!(
                (written - asked).abs() <= asked * 0.001 + 0.001,
                "{delay_ms}ms became {written}s"
            );
        }
    }

    #[test]
    fn a_long_delay_survives_into_the_file() {
        // The fraction is one thing; what lands in the `fcTL` chunk is the
        // one that matters, and it is what wrapped.
        let written = [70_000u32, 100];
        let frames: Vec<Frame> = written
            .iter()
            .map(|delay_ms| Frame {
                pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, 255]),
                width: 2,
                height: 1,
                delay_ms: *delay_ms,
            })
            .collect();

        let bytes = encoded(&frames, None);
        let mut reader = Decoder::new(Cursor::new(&bytes))
            .read_info()
            .expect("a PNG this crate wrote");
        let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0)];
        for delay_ms in written {
            reader.next_frame(&mut buffer).expect("a frame");
            let delay = reader
                .info()
                .frame_control
                .expect("an animated frame is controlled");
            let seconds =
                f64::from(delay.delay_num) / f64::from(delay.delay_den);
            assert!(
                (seconds - f64::from(delay_ms) / 1000.0).abs() < 0.01,
                "{delay_ms}ms became {seconds}s \
                 ({}/{})",
                delay.delay_num,
                delay.delay_den
            );
        }
    }

    #[test]
    fn a_finite_animation_counts_its_plays() {
        let bytes = encoded(&frames(2, 40), Some(3));
        let reader = Decoder::new(Cursor::new(&bytes))
            .read_info()
            .expect("a PNG this crate wrote");
        let control = reader
            .info()
            .animation_control
            .expect("two frames make an animation");
        assert_eq!(control.num_plays, 3);
    }

    #[test]
    fn one_page_is_a_still_png_and_not_an_animation_of_one() {
        // Every decoder handles a one-frame APNG and none needs it, and
        // `toBuffer("apng")` of a single page should be a file anything can
        // open rather than an animation with nothing to animate.
        let bytes = encoded(&frames(1, 100), None);
        let reader = Decoder::new(Cursor::new(&bytes))
            .read_info()
            .expect("a PNG this crate wrote");
        assert!(reader.info().animation_control.is_none());
        assert_eq!(&bytes[1..4], b"PNG");
    }
}
