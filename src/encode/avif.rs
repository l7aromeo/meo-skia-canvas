//! AVIF, through rav1e and avif-serialize.
//!
//! The one format here whose encoder is a video codec. An AV1 intra frame is
//! what an AVIF holds, which buys compression nothing else in this crate
//! approaches and costs correspondingly: encoding is measured in tenths of a
//! second where PNG is measured in milliseconds.
//!
//! Still images only. AVIF has an animated form -- AVIS, a sequence of AV1
//! frames -- which is not written here. A canvas with several pages exports
//! the one chosen page rather than pretending the others were encoded.
//!
//! # Why not `ravif`
//!
//! This went through `ravif`, which is these same two crates with the
//! conversion in between, until the colour work. `ravif` hardcodes BT.709
//! primaries and the sRGB transfer function into the bitstream, never calls
//! `avif-serialize`'s `set_color_primaries`, and exposes no way to change
//! either -- so a Display P3 canvas could not be written as a Display P3
//! AVIF, and the container said nothing at all about colour. Checked against
//! 0.13.0, which is the latest release.
//!
//! Two workarounds were measured and rejected. Switching `ravif` to its RGB
//! colour model does make it emit a `colr` box, because the identity matrix
//! is not its serializer's default -- but identity means no chroma
//! decorrelation, and on a 320x240 gradient that cost 41% at quality 50, 33%
//! at 80 and 38% at 95. Patching a `colr` box into the finished file would
//! leave the container and the bitstream stating different primaries; the
//! specification says the container wins, but writing two answers to one
//! question is not something to ship.
//!
//! So the two crates `ravif` wraps are used directly, which is a smaller
//! dependency tree rather than a larger one -- `ravif`, `loop9` and
//! `quick-error` left with it. What had to come along is the arithmetic
//! between them: the BT.601 conversion and the quality-to-quantizer curve
//! below are `ravif`'s, and are noted as such where they appear.

use avif_serialize::{
    Aviffy,
    constants::{ColorPrimaries, MatrixCoefficients, TransferCharacteristics},
};
use rav1e::{
    color::{
        ChromaSamplePosition, ChromaSampling, ColorDescription,
        MatrixCoefficients as Av1Matrix, PixelRange,
    },
    config::{Config, EncoderConfig, SpeedSettings},
    prelude::{
        Context, EncoderStatus, Frame as Av1Frame, FrameType, Packet, Rational,
        SceneDetectionSpeed,
    },
};

use super::{
    Frame, FrameEncoder, FrameSink, SequenceSpec, Sink, color::ColorProfile,
};

/// How hard rav1e looks for a smaller file, from 0 to 10, slowest first.
///
/// 6 is `cavif`'s own default and sits where the curve bends: 4 is several
/// times slower for a percent or two, and 8 gives most of the time back and
/// noticeably more file. A canvas export is not a batch job, so the middle
/// is the right place to be.
const SPEED: u8 = 6;

/// The narrowest quality the quantizer curve is fed.
///
/// This crate's own range starts at zero, and zero quality is not a picture.
/// One is, barely, and is what `ravif` asserted on before this encoder took
/// the curve over directly -- `toBuffer("avif", {quality: 0})` panicked
/// across the FFI boundary rather than returning an error.
const QUALITY_FLOOR: f32 = 1.0;

/// Bits per channel in the AV1 payload.
///
/// Ten, from eight-bit input, which is what `ravif` does by default and is
/// not the waste it looks: AV1's transforms work at higher precision anyway,
/// and the headroom keeps quantisation from banding a gradient that eight
/// bits would step through.
const BIT_DEPTH: usize = 10;

/// The luma coefficients of BT.601, the matrix the chroma planes are built
/// with.
///
/// `ravif`'s choice, kept because changing it would change every file this
/// crate has written. It is also what a decoder assumes when a file says
/// nothing, so it is the one matrix that stays right under a missing `colr`.
const BT601_LUMA: [f32; 3] = [0.2990, 0.5870, 0.1140];

/// The widest quantizer rav1e takes, and the top of the curve below.
const QUANTIZER_MAX: f32 = 255.0;

/// Where [`quality_to_quantizer`] steepens, as a fraction of the dial.
///
/// Above this the quantizer has to fall quickly to zero, because the last
/// fifth of the quality range is where a small quantizer step is a large
/// visible one.
const FINE_KNEE: f32 = 0.82;

/// How fast the quantizer falls above [`FINE_KNEE`], per unit of quality.
///
/// Steeper than one, which is what makes the top of the dial spend
/// quantizer quickly: the segment runs from roughly 119 down to 0 across the
/// eighteen points above the knee.
const FINE_SLOPE: f32 = 2.6;

/// Where [`quality_to_quantizer`] flattens at the bottom.
///
/// Below this the curve gives up on being gentle and falls straight, since
/// a quarter of the dial is already past the point where the picture is
/// mostly artefact.
const COARSE_KNEE: f32 = 0.25;

/// How fast the quantizer falls between the two knees, per unit of quality.
///
/// Half, which is the shallow middle: most of the dial moves the quantizer
/// slowly, because most of the dial is where quality changes gradually.
const MIDDLE_SLOPE: f32 = 0.5;

/// How far below the top of the quantizer range the middle segment starts.
///
/// The segment is `1 - MIDDLE_DROP - MIDDLE_SLOPE * quality`, so this is
/// what keeps it continuous with the coarse segment below it at
/// [`COARSE_KNEE`].
const MIDDLE_DROP: f32 = 0.125;

/// Where zero chroma sits, as a fraction of the coded range.
///
/// Cb and Cr are signed differences and the coded value is unsigned, so
/// neutral grey sits at the middle of the range and colour moves away from
/// it in either direction. The same half appears twice in
/// [`rgb_to_ycbcr`] -- once as the offset that puts neutral in the middle,
/// once as the half-range a full-scale difference is scaled into -- because
/// it is one fact used two ways.
const CHROMA_HALF_RANGE: f32 = 0.5;

/// The primaries `avif-serialize` can name, for looking one up by code
/// point.
///
/// ITU-T H.273 numbers these and both crates carry those numbers as their
/// enum discriminants, so this is a search rather than a translation table.
/// Listing the variants is what keeps a literal out of it: `DisplayP3 as u8`
/// is 12 because the standard says so, and no line here repeats the 12.
const NAMED_PRIMARIES: &[ColorPrimaries] = &[
    ColorPrimaries::Bt709,
    ColorPrimaries::Bt601,
    ColorPrimaries::Bt2020,
    ColorPrimaries::DciP3,
    ColorPrimaries::DisplayP3,
];

/// The transfer functions `avif-serialize` can name. As [`NAMED_PRIMARIES`],
/// by code point.
const NAMED_TRANSFERS: &[TransferCharacteristics] = &[
    TransferCharacteristics::Bt709,
    TransferCharacteristics::Linear,
    TransferCharacteristics::Srgb,
    TransferCharacteristics::Bt2020_10,
    TransferCharacteristics::Bt2020_12,
    TransferCharacteristics::Smpte2084,
    TransferCharacteristics::Hlg,
];

pub(crate) struct Avif;

impl FrameEncoder for Avif {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        Ok(Box::new(AvifSink {
            out,
            quality: spec.quality,
            color: spec.color,
        }))
    }
}

struct AvifSink<'a> {
    out: &'a mut dyn Sink,
    quality: f32,
    color: ColorProfile,
}

impl FrameSink for AvifSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        let encoded = encode(frame, self.quality, &self.color)?;
        self.out
            .write_all(&encoded)
            .map_err(|e| format!("Could not write the AVIF: {e}"))
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.out
            .flush()
            .map_err(|e| format!("Could not finish the AVIF: {e}"))
    }
}

/// The `avif-serialize` name for a colour primaries code point.
fn primaries_named(code: u8) -> Result<ColorPrimaries, String> {
    NAMED_PRIMARIES
        .iter()
        .copied()
        .find(|named| *named as u8 == code)
        .ok_or_else(|| format!("An AVIF cannot name colour primaries {code}"))
}

/// The `avif-serialize` name for a transfer characteristics code point.
fn transfer_named(code: u8) -> Result<TransferCharacteristics, String> {
    NAMED_TRANSFERS
        .iter()
        .copied()
        .find(|named| *named as u8 == code)
        .ok_or_else(|| {
            format!("An AVIF cannot name transfer characteristics {code}")
        })
}

/// The quantizer, from 0 to 255, that `quality` asks for.
///
/// `ravif`'s curve, kept so the dial keeps meaning what it meant. Three
/// straight segments joined at [`FINE_KNEE`] and [`COARSE_KNEE`], each named
/// for the range it covers rather than for the number it is -- the
/// coefficients are one author's fitting, not values from a standard, and
/// what a reader needs from them is which part of the dial they shape.
///
/// The steep top segment is why the curve exists: above 82 a small step in
/// quality has to move the quantizer a long way, because that is where a
/// small quantizer change is a large change in the picture.
fn quality_to_quantizer(quality: f32) -> u8 {
    let q = quality / 100.0;
    let coarseness = if q >= FINE_KNEE {
        (1.0 - q) * FINE_SLOPE
    } else if q > COARSE_KNEE {
        q.mul_add(-MIDDLE_SLOPE, 1.0 - MIDDLE_DROP)
    } else {
        1.0 - q
    };
    (coarseness * QUANTIZER_MAX).round() as u8
}

/// An eight-bit channel widened to ten, keeping the top of the range.
///
/// Shifting alone would leave the maximum at 1020 rather than 1023, so the
/// top two bits fold back in: 255 becomes 1023 and 0 stays 0.
fn to_ten(value: u8) -> u16 {
    (u16::from(value) << 2) | (u16::from(value) >> 6)
}

/// One RGB pixel as ten-bit Y, Cb and Cr through [`BT601_LUMA`].
///
/// `ravif`'s conversion. Full range, so the scale is the ten-bit maximum
/// over the eight-bit one and the chroma planes sit around the midpoint
/// rather than around zero.
fn rgb_to_ycbcr(red: u8, green: u8, blue: u8) -> [u16; 3] {
    let max = ((1 << BIT_DEPTH) - 1) as f32;
    let scale = max / f32::from(u8::MAX);
    let neutral = (max * CHROMA_HALF_RANGE).round();
    let [kr, kg, kb] = BT601_LUMA;

    let (r, g, b) = (f32::from(red), f32::from(green), f32::from(blue));
    let y = (scale * kb).mul_add(b, (scale * kr).mul_add(r, scale * kg * g));
    // A blue-minus-luma difference spans `±(1 - kb)` of full scale, so
    // dividing by that and taking half maps it onto half the coded range on
    // either side of neutral. Same for red against `kr`.
    let cb = b
        .mul_add(scale, -y)
        .mul_add(CHROMA_HALF_RANGE / (1.0 - kb), neutral);
    let cr = r
        .mul_add(scale, -y)
        .mul_add(CHROMA_HALF_RANGE / (1.0 - kr), neutral);
    [y.round() as u16, cb.round() as u16, cr.round() as u16]
}

/// The most tiles a frame is split into, and so the most threads encoding it.
///
/// One frame is one tile by default, and a tile is what rav1e parallelises
/// over -- so a still picture encoded at that default runs on one core
/// whatever the machine has. A 1200x900 page took 5.6 seconds here and takes
/// 1.1 now.
///
/// Eight rather than the core count: tiles are coded independently, so each
/// one costs a little compression -- the entropy coder restarts at its
/// boundary and prediction cannot cross it -- and the exporter's own pool is
/// usually busy with the next page anyway.
const MAX_TILES: usize = 8;

/// The pixels a tile wants to itself before another is worth opening.
///
/// The compression a tile costs is roughly fixed while the time it saves
/// scales with the area, so on a small image the trade inverts: eight tiles
/// on a 320x120 strip made the file *larger* than the PNG of the same
/// drawing, which a test caught.
///
/// An eighth of a megapixel each, which puts a 1200x900 page on the full
/// eight and leaves anything under 128K pixels whole. A quarter-megapixel
/// was the first try and gave that page four tiles: 1.9 seconds against the
/// 1.1 eight take, for one kilobyte in 366.
const PIXELS_PER_TILE: usize = 131_072;

/// How many tiles a frame of this size is worth splitting into.
fn tiles_for(width: usize, height: usize) -> usize {
    (width * height / PIXELS_PER_TILE).clamp(1, MAX_TILES)
}

/// The rav1e settings a single still frame wants.
///
/// The three overrides are all consequences of there being one frame:
/// nothing to reference, nothing to look ahead to, and no scene to detect.
/// Beyond them this takes rav1e's own preset rather than retuning it.
fn speed_settings() -> SpeedSettings {
    let mut settings = SpeedSettings::from_preset(SPEED);
    settings.multiref = false;
    settings.rdo_lookahead_frames = 1;
    settings.scene_detection_mode = SceneDetectionSpeed::None;
    settings
}

/// Encodes one plane set as an AV1 key frame.
fn encode_av1(
    width: usize,
    height: usize,
    quantizer: usize,
    chroma: ChromaSampling,
    description: Option<ColorDescription>,
    fill: impl FnOnce(&mut Av1Frame<u16>),
) -> Result<Vec<u8>, String> {
    let tiles = tiles_for(width, height);
    let config =
        Config::new()
            .with_threads(tiles)
            .with_encoder_config(EncoderConfig {
                width,
                height,
                bit_depth: BIT_DEPTH,
                chroma_sampling: chroma,
                chroma_sample_position: ChromaSamplePosition::Unknown,
                pixel_range: PixelRange::Full,
                color_description: description,
                still_picture: true,
                tiles,
                speed_settings: speed_settings(),
                time_base: Rational::new(1, 1),
                min_key_frame_interval: 0,
                max_key_frame_interval: 0,
                low_latency: false,
                quantizer,
                min_quantizer: quantizer as u8,
                bitrate: 0,
                ..EncoderConfig::default()
            });

    let mut context: Context<u16> = config
        .new_context()
        .map_err(|e| format!("Could not configure the AVIF encoder: {e}"))?;
    let mut frame = context.new_frame();
    fill(&mut frame);
    context
        .send_frame(frame)
        .map_err(|e| format!("Could not encode as AVIF: {e}"))?;
    context.flush();

    let mut out = Vec::new();
    loop {
        match context.receive_packet() {
            Ok(Packet {
                frame_type: FrameType::KEY,
                mut data,
                ..
            }) => out.append(&mut data),
            Ok(_) => continue,
            Err(EncoderStatus::Encoded | EncoderStatus::LimitReached) => break,
            Err(e) => return Err(format!("Could not encode as AVIF: {e}")),
        }
    }
    Ok(out)
}

/// One frame as a complete AVIF file.
fn encode(
    frame: &Frame,
    quality: f32,
    color: &ColorProfile,
) -> Result<Vec<u8>, String> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let quantizer =
        quality_to_quantizer(quality.clamp(QUALITY_FLOOR, 100.0)) as usize;

    // The colour description goes into the AV1 sequence header as well as
    // into the container below, so the file answers the question once.
    let primaries = primaries_named(color.cicp.primaries)?;
    let transfer = transfer_named(color.cicp.transfer)?;
    let description = ColorDescription {
        color_primaries: primaries_av1(primaries)?,
        transfer_characteristics: transfer_av1(transfer)?,
        matrix_coefficients: Av1Matrix::BT601,
    };

    // Alpha is a second AV1 image, monochrome, and left out entirely where
    // nothing is transparent -- which is most canvas output and most of the
    // file.
    let opaque = frame.pixels.chunks_exact(4).all(|px| px[3] == u8::MAX);

    let color_payload = encode_av1(
        width,
        height,
        quantizer,
        ChromaSampling::Cs444,
        Some(description),
        |av1| fill_ycbcr(av1, width, height, &frame.pixels),
    )?;
    let alpha_payload = match opaque {
        true => None,
        false => Some(encode_av1(
            width,
            height,
            quantizer,
            ChromaSampling::Cs400,
            None,
            |av1| fill_alpha(av1, width, height, &frame.pixels),
        )?),
    };

    let mut aviffy = Aviffy::new();
    aviffy
        .matrix_coefficients(MatrixCoefficients::Bt601)
        .set_color_primaries(primaries)
        .set_transfer_characteristics(transfer)
        .set_full_color_range(true)
        .premultiplied_alpha(false);

    Ok(aviffy.to_vec(
        &color_payload,
        alpha_payload.as_deref(),
        frame.width,
        frame.height,
        BIT_DEPTH as u8,
    ))
}

/// The rav1e name for the same primaries, by code point.
///
/// Two crates, one standard, the same numbers as discriminants -- so this
/// converts by value rather than by a table that could disagree with either.
fn primaries_av1(
    primaries: ColorPrimaries,
) -> Result<rav1e::color::ColorPrimaries, String> {
    use rav1e::color::ColorPrimaries as Av1;
    [
        Av1::BT709,
        Av1::BT601,
        Av1::BT2020,
        Av1::SMPTE431,
        Av1::SMPTE432,
    ]
    .into_iter()
    .find(|named| *named as u8 == primaries as u8)
    .ok_or_else(|| format!("rav1e cannot name {primaries:?}"))
}

/// The rav1e name for the same transfer function, by code point.
fn transfer_av1(
    transfer: TransferCharacteristics,
) -> Result<rav1e::color::TransferCharacteristics, String> {
    use rav1e::color::TransferCharacteristics as Av1;
    [
        Av1::BT709,
        Av1::Linear,
        Av1::SRGB,
        Av1::BT2020_10Bit,
        Av1::BT2020_12Bit,
        Av1::SMPTE2084,
        Av1::HLG,
    ]
    .into_iter()
    .find(|named| *named as u8 == transfer as u8)
    .ok_or_else(|| format!("rav1e cannot name {transfer:?}"))
}

/// Fills an AV1 frame's three planes from RGBA pixels.
fn fill_ycbcr(
    av1: &mut Av1Frame<u16>,
    width: usize,
    height: usize,
    pixels: &[u8],
) {
    let (first, rest) = av1.planes.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    let mut y = first[0].mut_slice(Default::default());
    let mut cb = second[0].mut_slice(Default::default());
    let mut cr = third[0].mut_slice(Default::default());

    let rows = y
        .rows_iter_mut()
        .zip(cb.rows_iter_mut())
        .zip(cr.rows_iter_mut())
        .take(height);
    for (row, ((y, cb), cr)) in rows.enumerate() {
        let source = &pixels[row * width * 4..(row + 1) * width * 4];
        for (at, px) in source.chunks_exact(4).enumerate() {
            let [luma, blue, red] = rgb_to_ycbcr(px[0], px[1], px[2]);
            y[at] = luma;
            cb[at] = blue;
            cr[at] = red;
        }
    }
}

/// Fills a monochrome AV1 frame's one plane from the alpha channel.
fn fill_alpha(
    av1: &mut Av1Frame<u16>,
    width: usize,
    height: usize,
    pixels: &[u8],
) {
    let mut plane = av1.planes[0].mut_slice(Default::default());
    for (row, out) in plane.rows_iter_mut().take(height).enumerate() {
        let source = &pixels[row * width * 4..(row + 1) * width * 4];
        for (at, px) in source.chunks_exact(4).enumerate() {
            out[at] = to_ten(px[3]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{encode::start, export::ImageFormat, pixels::PixelColorSpace};

    fn frame(width: u32, height: u32) -> Frame {
        // A gradient rather than a flat fill: a solid colour compresses to
        // almost nothing at every quality, so it could not show the dial
        // moving.
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 255 / width.max(1)) as u8,
                    (y * 255 / height.max(1)) as u8,
                    128,
                    255,
                ]);
            }
        }
        Frame {
            pixels,
            width,
            height,
            delay_ms: 0,
        }
    }

    fn encoded_frame(
        source: &Frame,
        quality: f32,
        space: PixelColorSpace,
    ) -> Vec<u8> {
        let spec = SequenceSpec {
            width: source.width,
            height: source.height,
            frames: 1,
            loops: None,
            quality,
            density: 1.0,
            color: ColorProfile::of(space),
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Avif, &spec, &mut bytes)
                .expect("the spec is well formed");
            sink.write_frame(source).expect("a well formed frame");
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    fn encoded_in(
        width: u32,
        height: u32,
        quality: f32,
        space: PixelColorSpace,
    ) -> Vec<u8> {
        encoded_frame(&frame(width, height), quality, space)
    }

    fn encoded(width: u32, height: u32, quality: f32) -> Vec<u8> {
        encoded_in(width, height, quality, PixelColorSpace::Srgb)
    }

    /// The four-character names of the top-level ISOBMFF boxes.
    fn boxes(bytes: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = 0usize;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]) as usize;
            if size < 8 {
                break;
            }
            names.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into());
            at += size;
        }
        names
    }

    /// The dimensions the file records in its `ispe` box.
    fn spatial_extents(bytes: &[u8]) -> Option<(u32, u32)> {
        let at = bytes.windows(4).position(|w| w == b"ispe")?;
        // `ispe` is a full box: four bytes of version and flags, then the
        // width and height.
        let read = |i: usize| {
            u32::from_be_bytes([
                bytes[i],
                bytes[i + 1],
                bytes[i + 2],
                bytes[i + 3],
            ])
        };
        Some((read(at + 8), read(at + 12)))
    }

    /// The `colr` box's four code points, if the file has one.
    fn colr(bytes: &[u8]) -> Option<(u8, u8, u8, bool)> {
        let at = bytes.windows(4).position(|w| w == b"colr")?;
        // `colr` names its own form first. `nclx` is the code-point one; the
        // alternative is an embedded profile, which this does not write.
        assert_eq!(&bytes[at + 4..at + 8], b"nclx", "an nclx colr box");
        let short = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        Some((
            short(at + 8) as u8,
            short(at + 10) as u8,
            short(at + 12) as u8,
            bytes[at + 14] & 0x80 != 0,
        ))
    }

    #[test]
    fn the_file_is_an_avif_and_says_its_own_size() {
        let bytes = encoded(64, 48, 80.0);
        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(&bytes[8..12], b"avif", "the major brand");

        let names = boxes(&bytes);
        assert_eq!(
            names,
            vec!["ftyp", "meta", "mdat"],
            "the layout an AVIF still has"
        );
        assert_eq!(
            spatial_extents(&bytes),
            Some((64, 48)),
            "the dimensions the file reports are the ones it was given"
        );
    }

    #[test]
    fn the_container_names_the_colour_space_it_holds() {
        // `ravif` wrote no `colr` box at all: it only ever set the matrix,
        // and its serializer skips the box when every field is the default.
        // So an AVIF said nothing about colour and a reader fell back to the
        // sequence header -- right for sRGB, and no way to write anything
        // else. The numbers below are the ones ITU-T H.273 tabulates.
        for (space, primaries, transfer) in [
            (PixelColorSpace::DisplayP3, 12u8, 13u8),
            (PixelColorSpace::DisplayP3Linear, 12, 8),
            (PixelColorSpace::Rec2020, 9, 1),
            (PixelColorSpace::Rec2020Linear, 9, 8),
            (PixelColorSpace::Rec2020Pq, 9, 16),
            (PixelColorSpace::Rec2020Hlg, 9, 18),
            (PixelColorSpace::SrgbLinear, 1, 8),
        ] {
            let bytes = encoded_in(32, 32, 80.0, space);
            assert_eq!(
                colr(&bytes),
                // BT.601 is the matrix the chroma planes are built with, and
                // the pixels are full range.
                Some((
                    primaries,
                    transfer,
                    MatrixCoefficients::Bt601 as u8,
                    true
                )),
                "{space:?}"
            );
        }
    }

    #[test]
    fn plain_srgb_is_the_one_space_left_to_the_bitstream() {
        // `avif-serialize` writes the `colr` box only when some field
        // differs from its default, and its default is exactly sRGB:
        // BT.709 primaries, the sRGB curve, BT.601 matrix, full range. So
        // the box is absent for the one space where absence already means
        // the right thing.
        //
        // Nothing is lost by that. The AV1 sequence header carries the same
        // description -- `encode` hands it to rav1e as well -- and a decoder
        // falls back to it when the container is silent. What was missing
        // before was any way to write a file that is *not* sRGB, which is
        // what the test above covers.
        let bytes = encoded_in(32, 32, 80.0, PixelColorSpace::Srgb);
        assert_eq!(colr(&bytes), None);
        assert_eq!(&bytes[8..12], b"avif", "still a well formed AVIF");
    }

    #[test]
    fn a_quality_below_what_the_quantizer_curve_takes_is_clamped() {
        // This crate's range starts at zero, and `ravif` asserted on
        // anything below one -- across the FFI boundary, which took the
        // process with it rather than returning an error.
        for quality in [0.0, 0.4, 0.999] {
            let bytes = encoded(32, 32, quality);
            assert_eq!(&bytes[8..12], b"avif", "quality {quality}");
        }
    }

    #[test]
    fn the_quantizer_curve_is_the_shape_its_constants_are_named_for() {
        // The names claim three straight segments, joined and continuous,
        // steep at the top. Nothing about a fitted coefficient can be
        // checked against a standard, so what is checked is that it still
        // does what the name says.
        let at = |quality: f32| f32::from(quality_to_quantizer(quality));

        // Monotonic: more quality is never a coarser quantizer.
        let mut previous = at(0.0);
        for step in 1..=100 {
            let next = at(step as f32);
            assert!(
                next <= previous,
                "quality {step}: {next} after {previous}"
            );
            previous = next;
        }

        // Joined at both knees: crossing one costs no more than the steeper
        // of the two slopes would over the same span. A segment whose
        // offset had drifted from its neighbour's would jump much further
        // than that, which is the failure this is for.
        //
        // The bound is derived from the constants rather than picked, so it
        // stays honest if a slope is ever retuned. Plus one for the rounding
        // to a whole quantizer at each end.
        let knee = |fraction: f32| fraction * 100.0;
        let steepest = FINE_SLOPE.max(MIDDLE_SLOPE) * QUANTIZER_MAX / 100.0;
        for corner in [FINE_KNEE, COARSE_KNEE] {
            let (below, above) =
                (at(knee(corner) - 0.5), at(knee(corner) + 0.5));
            assert!(
                below - above <= steepest + 1.0,
                "a jump of {} at the knee at {corner}, where the steepest \
                 segment would cost {steepest:.1}",
                below - above
            );
        }

        // And the top segment is the steep one, which is the whole reason
        // the curve is not a straight line.
        let steep = at(knee(FINE_KNEE)) - at(100.0);
        let shallow = at(knee(COARSE_KNEE)) - at(knee(FINE_KNEE));
        let per_point = |drop: f32, span: f32| drop / span;
        assert!(
            per_point(steep, 100.0 - knee(FINE_KNEE))
                > per_point(shallow, knee(FINE_KNEE) - knee(COARSE_KNEE)),
            "the top of the dial should spend quantizer fastest"
        );
        assert_eq!(at(100.0), 0.0, "full quality is no quantization");
    }

    #[test]
    fn quality_moves_the_file_size() {
        let low = encoded(96, 96, 20.0).len();
        let mid = encoded(96, 96, 60.0).len();
        let high = encoded(96, 96, 95.0).len();
        assert!(low < mid, "20 should be smaller than 60 ({low} vs {mid})");
        assert!(mid < high, "60 should be smaller than 95 ({mid} vs {high})");
    }

    #[test]
    fn a_transparent_frame_carries_an_alpha_plane_and_an_opaque_one_does_not() {
        // The alpha plane is a second AV1 image, so leaving it out where
        // nothing is transparent is most of the file for most canvas output.
        let opaque = encoded(48, 48, 80.0);
        let mut faded = frame(48, 48);
        for pixel in faded.pixels.chunks_exact_mut(4) {
            pixel[3] = 128;
        }
        let transparent = encoded_frame(&faded, 80.0, PixelColorSpace::Srgb);

        // `auxC` is the box declaring the second image's role as alpha.
        let has_alpha = |b: &[u8]| b.windows(4).any(|window| window == b"auxC");
        assert!(!has_alpha(&opaque), "nothing transparent, no alpha plane");
        assert!(has_alpha(&transparent), "half transparent, alpha plane");
        assert!(transparent.len() > opaque.len());
    }
}
