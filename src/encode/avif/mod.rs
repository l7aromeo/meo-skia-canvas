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

use crate::export::ChromaSampling as Requested;

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

/// Bits per channel written from an eight-bit canvas.
///
/// Ten, from eight-bit input, which is what `ravif` does by default and is
/// not the waste it looks: AV1's transforms work at higher precision anyway,
/// and the headroom keeps quantisation from banding a gradient that eight
/// bits would step through.
const SHALLOW_BITS: u8 = 10;

/// Bits per channel written from a canvas with more than eight to give.
///
/// Twelve is the deepest AV1 codes, and a float canvas has the range to fill
/// it. It costs reach: twelve bits is Professional profile, where eight and
/// ten at 4:4:4 are High, and fewer decoders take the former. Which is why
/// it is the default only for a canvas that was deliberately built deep --
/// `bit_depth` names another when the file has to travel.
const DEEP_BITS: u8 = 12;

/// The smallest picture AV1 codes as a sequence, on a side.
///
/// rav1e refuses anything narrower or shorter than this outside still
/// mode -- `invalid width 4 (expected >= 16, ..)` -- because the coding
/// tools a sequence uses are defined on blocks this size. A still has no
/// such floor, which is why a tiny canvas can be exported as one and not
/// as an animation.
const MIN_ANIMATED_SIDE: u32 = 16;

/// Every depth AV1 codes, and so every depth this encoder writes.
///
/// The export layer refuses anything else before a surface is rasterized,
/// so this list is the one statement of what AVIF takes.
pub(crate) const BIT_DEPTHS: &[u8] = &[8, SHALLOW_BITS, DEEP_BITS];

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

mod sequence;

pub(crate) struct Avif;

impl FrameEncoder for Avif {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        // Refused here rather than at the first frame, so a caller learns
        // before a surface has been rasterized for every page.
        let animated = spec.frames > 1;
        if animated
            && (spec.width < MIN_ANIMATED_SIDE
                || spec.height < MIN_ANIMATED_SIDE)
        {
            return Err(format!(
                "An animated AVIF is at least {MIN_ANIMATED_SIDE}x\
                 {MIN_ANIMATED_SIDE} (got {}x{}) -- AV1 codes a sequence in \
                 blocks that size. Export one page for a still.",
                spec.width, spec.height
            ));
        }
        Ok(Box::new(AvifSink {
            out,
            quality: spec.quality,
            color: spec.color,
            bits: spec.bits_or(SHALLOW_BITS, DEEP_BITS),
            chroma: spec.chroma,
            loops: spec.loops,
            pending: Vec::new(),
            // One page is a still, which is the form every AVIF this crate
            // wrote before now and the one every reader takes.
            animated,
            width: spec.width,
            height: spec.height,
        }))
    }
}

struct AvifSink<'a> {
    out: &'a mut dyn Sink,
    quality: f32,
    color: ColorProfile,
    bits: u8,
    /// How chroma is sampled, which is the caller's choice rather than this
    /// encoder's -- see `EncodeOptions::chroma` for why the default is full.
    chroma: Requested,
    width: u32,
    height: u32,
    /// How many times the animation plays; `None` is forever.
    loops: Option<u32>,
    /// The frames, held until `finish` because a sequence is coded as a
    /// whole: every frame after the first is stored as a difference from
    /// the ones before it, so none can be written until all have arrived.
    /// A single-page export writes the still form and holds nothing.
    pending: Vec<(Vec<u16>, u32)>,
    /// Whether this export gathers pages into an animation at all.
    animated: bool,
}

impl FrameSink for AvifSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        if self.animated {
            self.pending
                .push((frame.sixteen().into_owned(), frame.delay_ms));
            return Ok(());
        }
        let encoded =
            encode(frame, self.quality, &self.color, self.bits, self.chroma)?;
        self.out
            .write_all(&encoded)
            .map_err(|e| format!("Could not write the AVIF: {e}"))
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        let mut this = *self;
        if this.animated {
            let bytes = this.animate()?;
            this.out
                .write_all(&bytes)
                .map_err(|e| format!("Could not write the AVIF: {e}"))?;
        }
        this.out
            .flush()
            .map_err(|e| format!("Could not finish the AVIF: {e}"))
    }
}

impl AvifSink<'_> {
    /// The whole animation, once every frame has arrived.
    fn animate(&mut self) -> Result<Vec<u8>, String> {
        let frames = std::mem::take(&mut self.pending);
        let Some((first, _)) = frames.first() else {
            return Err("An animated AVIF needs at least one frame".to_string());
        };

        let (width, height) = (self.width as usize, self.height as usize);
        let quantizer =
            quality_to_quantizer(self.quality.clamp(QUALITY_FLOOR, 100.0))
                as usize;
        let primaries = primaries_named(self.color.cicp.primaries)?;
        let transfer = transfer_named(self.color.cicp.transfer)?;
        let description = ColorDescription {
            color_primaries: primaries_av1(primaries)?,
            transfer_characteristics: transfer_av1(transfer)?,
            matrix_coefficients: Av1Matrix::BT601,
        };

        // The still the `meta` box points at, coded on its own so a reader
        // that shows one frame has one that stands alone. See the note in
        // `sequence`: this is the format's duplication, not a shortcut.
        let still = encode_av1(
            width,
            height,
            self.bits,
            quantizer,
            sampling_av1(self.chroma),
            Some(description),
            |av1| {
                fill_ycbcr(
                    av1,
                    width,
                    height,
                    first,
                    self.bits,
                    sampling_av1(self.chroma),
                )
            },
        )?;

        let colour = Coding {
            width,
            height,
            bits: self.bits,
            quantizer,
            chroma: sampling_av1(self.chroma),
            description: Some(description),
        };
        let (config, samples) = encode_sequence(&colour, &frames, false)?;

        // Transparency, where any frame has some. A second monochrome
        // sequence and a second still, which the container hangs off the
        // colour ones -- without this an animation came out opaque and
        // nothing said so, while the still form beside it kept its alpha.
        let opaque = frames
            .iter()
            .all(|(px, _)| px.chunks_exact(4).all(|p| p[3] == u16::MAX));
        let alpha = match opaque {
            true => None,
            false => {
                let still = encode_av1(
                    width,
                    height,
                    self.bits,
                    quantizer,
                    ChromaSampling::Cs400,
                    None,
                    |av1| fill_alpha(av1, width, height, first, self.bits),
                )?;
                let (config, samples) = encode_sequence(
                    &Coding {
                        chroma: ChromaSampling::Cs400,
                        description: None,
                        ..colour
                    },
                    &frames,
                    true,
                )?;
                Some((still, config, samples))
            }
        };

        Ok(sequence::write(
            &sequence::Movie {
                width: width as u32,
                height: height as u32,
                bits: self.bits,
                still: &still,
                loops: self.loops,
                config: &config,
                alpha: alpha.as_ref().map(|(still, config, samples)| {
                    sequence::Alpha {
                        still,
                        config,
                        samples: samples.clone(),
                    }
                }),
            },
            &samples,
        ))
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

/// A sixteen-bit channel as the `bits` AV1 stores here: its top `bits`.
///
/// At ten this is also, exactly, what this crate wrote before frames could
/// be deeper than eight bits. An eight-bit channel arrives widened by `v *
/// 257` -- bit replication -- and `(v * 257) >> 6` equals `(v << 2) | (v >>
/// 6)` for all 256 values, checked rather than assumed. Rounding instead
/// would have been defensible and would have moved 42 of them by one,
/// changing every AVIF this crate has already written for no gain.
///
/// The same shift at eight is `v >> 8`, which is the low half of the bit
/// replication undone: `(v * 257) >> 8 == v`. So an eight-bit canvas
/// written at eight bits arrives at the AV1 encoder as the bytes it started
/// as, and nothing has been through a wider form and back.
fn narrow(value: u16, bits: u8) -> u16 {
    value >> (u16::BITS as u8 - bits)
}

/// One RGB pixel as `bits`-deep Y, Cb and Cr through [`BT601_LUMA`].
///
/// `ravif`'s conversion. Full range, so the scale is the coded maximum over
/// the sixteen-bit one and the chroma planes sit around the midpoint rather
/// than around zero.
///
/// Sixteen bits in rather than eight: a float canvas has more than eight to
/// give, and up to twelve of them survive here. An eight-bit canvas arrives
/// widened by `v * 257`, which is exact, so its result is unchanged.
fn rgb_to_ycbcr(red: u16, green: u16, blue: u16, bits: u8) -> [u16; 3] {
    let max = ((1u32 << bits) - 1) as f32;
    let scale = max / f32::from(u16::MAX);
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

/// The rav1e sampling our own [`Requested`] names.
///
/// Two enums for one idea, deliberately: the public one is this crate's API
/// and must not hand a caller a rav1e type, which would make the encoder
/// impossible to change without a breaking release.
fn sampling_av1(chroma: Requested) -> ChromaSampling {
    match chroma {
        Requested::Full => ChromaSampling::Cs444,
        Requested::Half => ChromaSampling::Cs422,
        Requested::Quarter => ChromaSampling::Cs420,
    }
}

/// AV1's Main profile: 4:2:0 and monochrome, at eight or ten bits.
///
/// The three profiles are defined by what they may carry rather than by how
/// well they compress, so the sampling picks one (AV1 specification § 6.4.1).
const PROFILE_MAIN: u8 = 0;

/// High profile: 4:4:4, at eight or ten bits.
const PROFILE_HIGH: u8 = 1;

/// Professional profile: 4:2:2 at any depth, and anything at twelve bits.
const PROFILE_PROFESSIONAL: u8 = 2;

/// The narrowest AV1 profile that can carry this sampling.
///
/// Narrowest because reach is the point: every decoder implements Main, most
/// implement High, and fewest implement Professional. `avif-serialize` raises
/// whatever it is given to Professional on its own where the depth is twelve,
/// so this only has to answer for the sampling.
fn profile_for(chroma: Requested) -> u8 {
    match chroma {
        Requested::Quarter => PROFILE_MAIN,
        Requested::Full => PROFILE_HIGH,
        Requested::Half => PROFILE_PROFESSIONAL,
    }
}

/// How far a sampling subsamples chroma against luma, as a shift per axis.
///
/// The three AV1 supports, and monochrome, which has no chroma planes to
/// shift at all.
fn chroma_shifts(chroma: ChromaSampling) -> (usize, usize) {
    match chroma {
        ChromaSampling::Cs444 | ChromaSampling::Cs400 => (0, 0),
        ChromaSampling::Cs422 => (1, 0),
        ChromaSampling::Cs420 => (1, 1),
    }
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
    bits: u8,
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
                bit_depth: bits as usize,
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
    bits: u8,
    chroma: Requested,
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
    // file. It is coded at the colour image's depth because the
    // specification requires the two to match, not because it needs it.
    // Sixteen-bit throughout, whatever `bits` turns out to be: the widest
    // form both an eight-bit and a float canvas fit into, narrowed once at
    // the point the planes are filled.
    let pixels = frame.sixteen();
    let opaque = pixels.chunks_exact(4).all(|px| px[3] == u16::MAX);

    let color_payload = encode_av1(
        width,
        height,
        bits,
        quantizer,
        sampling_av1(chroma),
        Some(description),
        |av1| {
            fill_ycbcr(av1, width, height, &pixels, bits, sampling_av1(chroma))
        },
    )?;
    let alpha_payload = match opaque {
        true => None,
        false => Some(encode_av1(
            width,
            height,
            bits,
            quantizer,
            ChromaSampling::Cs400,
            None,
            |av1| fill_alpha(av1, width, height, &pixels, bits),
        )?),
    };

    let mut aviffy = Aviffy::new();
    let (shift_x, shift_y) = chroma_shifts(sampling_av1(chroma));
    aviffy
        .matrix_coefficients(MatrixCoefficients::Bt601)
        .set_color_primaries(primaries)
        .set_transfer_characteristics(transfer)
        .set_full_color_range(true)
        // The `av1C` record has to agree with the bitstream, and
        // `avif-serialize` cannot see the bitstream: unasked it writes 4:4:4
        // High, which is its default and was this crate's only output until
        // `chroma` became a caller's choice. A 4:2:0 file went out declaring
        // 4:4:4, and Apple's decoder -- which trusts the record -- refused it
        // outright while this crate's own decoder read it fine, because that
        // one takes the subsampling from the sequence header instead.
        .set_chroma_subsampling((shift_x > 0, shift_y > 0))
        .set_seq_profile(profile_for(chroma))
        .premultiplied_alpha(false);

    Ok(aviffy.to_vec(
        &color_payload,
        alpha_payload.as_deref(),
        frame.width,
        frame.height,
        bits,
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
    pixels: &[u16],
    bits: u8,
    chroma: ChromaSampling,
) {
    let (shift_x, shift_y) = chroma_shifts(chroma);

    // Converted once and kept, because a subsampled chroma sample is an
    // average over several pixels and every one of them is also a luma
    // sample. Converting twice would double the arithmetic that dominates
    // this function.
    let converted: Vec<[u16; 3]> = pixels
        .chunks_exact(4)
        .map(|px| rgb_to_ycbcr(px[0], px[1], px[2], bits))
        .collect();

    let (first, rest) = av1.planes.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    let mut luma = first[0].mut_slice(Default::default());
    let mut blue = second[0].mut_slice(Default::default());
    let mut red = third[0].mut_slice(Default::default());

    for (row, out) in luma.rows_iter_mut().take(height).enumerate() {
        for (at, sample) in
            converted[row * width..(row + 1) * width].iter().enumerate()
        {
            out[at] = sample[0];
        }
    }

    // A chroma cell covers `1 << shift` pixels on each axis, and its value
    // is their mean rather than one of them. Picking a single pixel is
    // cheaper and visibly worse: it throws away three quarters of the
    // chroma at 4:2:0 instead of averaging it, which shows on any edge
    // between two saturated colours.
    let cells_across = width.div_ceil(1 << shift_x);
    let cells_down = height.div_ceil(1 << shift_y);
    let mut rows = blue.rows_iter_mut().zip(red.rows_iter_mut());
    for cell_y in 0..cells_down {
        let Some((blue_row, red_row)) = rows.next() else {
            break;
        };
        let from_y = cell_y << shift_y;
        let to_y = ((cell_y + 1) << shift_y).min(height);
        for cell_x in 0..cells_across {
            let from_x = cell_x << shift_x;
            let to_x = ((cell_x + 1) << shift_x).min(width);

            let covered = (from_y..to_y)
                .flat_map(|y| (from_x..to_x).map(move |x| y * width + x));
            let (mut blues, mut reds, mut count) = (0u32, 0u32, 0u32);
            for at in covered {
                blues += u32::from(converted[at][1]);
                reds += u32::from(converted[at][2]);
                count += 1;
            }
            // A zero count would mean a cell covering no pixel, which the
            // ceiling division above cannot produce.
            let count = count.max(1);
            blue_row[cell_x] = (blues / count) as u16;
            red_row[cell_x] = (reds / count) as u16;
        }
    }
}

/// Fills a monochrome AV1 frame's one plane from the alpha channel.
fn fill_alpha(
    av1: &mut Av1Frame<u16>,
    width: usize,
    height: usize,
    pixels: &[u16],
    bits: u8,
) {
    let mut plane = av1.planes[0].mut_slice(Default::default());
    for (row, out) in plane.rows_iter_mut().take(height).enumerate() {
        let source = &pixels[row * width * 4..(row + 1) * width * 4];
        for (at, px) in source.chunks_exact(4).enumerate() {
            out[at] = narrow(px[3], bits);
        }
    }
}

/// Encodes every frame as one AV1 sequence, coded against each other.
///
/// The difference from [`encode_av1`] is `still_picture`, and it is worth
/// what it costs: a still is a key frame, and eight of them are eight key
/// frames. Coded as a sequence, the same eight frames of a moving square
/// came to 333 bytes against 95 for one still -- three and a half times the
/// size for eight times the content, because seven of the eight are stored
/// as differences from what came before.
///
/// The key frame interval is the frame count, so exactly one key frame is
/// written: the first. Every later frame may reference it, which is where
/// the saving comes from, and a reader has to start at the beginning --
/// which is what an animation does anyway.
struct Coding {
    width: usize,
    height: usize,
    bits: u8,
    quantizer: usize,
    chroma: ChromaSampling,
    description: Option<ColorDescription>,
}

fn encode_sequence(
    coding: &Coding,
    frames: &[(Vec<u16>, u32)],
    alpha: bool,
) -> Result<(Vec<u8>, Vec<sequence::Sample>), String> {
    let Coding {
        width,
        height,
        bits,
        quantizer,
        chroma,
        description,
    } = *coding;
    let tiles = tiles_for(width, height);
    let mut settings = SpeedSettings::from_preset(SPEED);
    // Left on, unlike the still path: referencing several earlier frames is
    // the whole mechanism a sequence saves by.
    settings.scene_detection_mode = SceneDetectionSpeed::None;

    let count = frames.len().max(1);
    let config =
        Config::new()
            .with_threads(tiles)
            .with_encoder_config(EncoderConfig {
                width,
                height,
                bit_depth: bits as usize,
                chroma_sampling: chroma,
                chroma_sample_position: ChromaSamplePosition::Unknown,
                pixel_range: PixelRange::Full,
                color_description: description,
                still_picture: false,
                tiles,
                speed_settings: settings,
                // The container carries the real timing per sample, so this is
                // only what rav1e reasons about internally.
                time_base: Rational::new(1, 1),
                min_key_frame_interval: count as u64,
                max_key_frame_interval: count as u64,
                low_latency: false,
                quantizer,
                min_quantizer: quantizer as u8,
                bitrate: 0,
                ..EncoderConfig::default()
            });

    let mut context: Context<u16> = config
        .new_context()
        .map_err(|e| format!("Could not configure the AVIF encoder: {e}"))?;
    // rav1e builds the `av1C` record itself, to the AV1-ISOBMFF
    // specification -- profile, level and depth included -- so nothing here
    // reconstructs those bits by hand.
    let av1c = context.container_sequence_header();

    for (pixels, _) in frames {
        let mut frame = context.new_frame();
        match alpha {
            true => fill_alpha(&mut frame, width, height, pixels, bits),
            false => {
                fill_ycbcr(&mut frame, width, height, pixels, bits, chroma)
            }
        }
        context
            .send_frame(frame)
            .map_err(|e| format!("Could not encode as AVIF: {e}"))?;
    }
    context.flush();

    let mut coded = Vec::with_capacity(frames.len());
    loop {
        match context.receive_packet() {
            Ok(packet) => {
                coded.push((packet.data, packet.frame_type == FrameType::KEY))
            }
            Err(EncoderStatus::Encoded) => continue,
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => return Err(format!("Could not encode as AVIF: {e}")),
        }
    }
    if coded.len() != frames.len() {
        return Err(format!(
            "The AVIF encoder returned {} frames for {}",
            coded.len(),
            frames.len()
        ));
    }

    let samples = coded
        .into_iter()
        .zip(frames)
        .map(|((data, sync), (_, delay_ms))| sequence::Sample {
            data,
            duration: sequence::ticks(*delay_ms),
            sync,
        })
        .collect();
    Ok((av1c, samples))
}

#[cfg(test)]
mod tests {
    use crate::encode::{FrameDepth, Pixels};
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
            pixels: Pixels::Eight(pixels),
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
        encoded_deeply(source, quality, space, FrameDepth::Eight, None)
    }

    fn encoded_deeply(
        source: &Frame,
        quality: f32,
        space: PixelColorSpace,
        depth: FrameDepth,
        bits: Option<u8>,
    ) -> Vec<u8> {
        let spec = SequenceSpec {
            chroma: Requested::Full,
            width: source.width,
            height: source.height,
            frames: 1,
            loops: None,
            quality,
            density: 1.0,
            color: ColorProfile::of(space),
            space,
            depth,
            bits,
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

    /// What every `av1C` box in the file says about its own coding.
    ///
    /// One per coded image, so a transparent picture has two: the colour
    /// image's first, then the alpha image's. Read as the fields
    /// `AV1CodecConfigurationRecord` packs them into its second and third
    /// bytes -- profile and level in one, then a bit each for tier, high
    /// bit depth, twelve bit and monochrome, then the two subsampling bits.
    fn av1_configs(bytes: &[u8]) -> Vec<Av1Config> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = bytes[from..]
            .windows(4)
            .position(|w| w == b"av1C")
            .map(|at| from + at)
        {
            let profile = bytes[at + 5] >> 5;
            let flags = bytes[at + 6];
            found.push(Av1Config {
                profile,
                high_bitdepth: flags & 0b0100_0000 != 0,
                twelve_bit: flags & 0b0010_0000 != 0,
                monochrome: flags & 0b0001_0000 != 0,
                subsampled_x: flags & 0b0000_1000 != 0,
                subsampled_y: flags & 0b0000_0100 != 0,
            });
            from = at + 4;
        }
        found
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Av1Config {
        profile: u8,
        high_bitdepth: bool,
        twelve_bit: bool,
        monochrome: bool,
        subsampled_x: bool,
        subsampled_y: bool,
    }

    impl Av1Config {
        /// The depth the two flags spell between them.
        fn bits(&self) -> u8 {
            match (self.high_bitdepth, self.twelve_bit) {
                (false, _) => 8,
                (true, false) => 10,
                (true, true) => 12,
            }
        }
    }

    #[test]
    fn the_config_record_states_the_sampling_that_was_asked_for() {
        // The `av1C` record and the bitstream have to agree, and only the
        // container is checked here because only the container was wrong.
        // `avif-serialize` writes 4:4:4 High unless told otherwise, and it
        // cannot see the bitstream to know better -- so when `chroma` became
        // a caller's choice, every subsampled file went out declaring 4:4:4.
        //
        // Nothing in the suite noticed. Every other AVIF test decodes with
        // this crate's own decoder, which reads the sampling from the AV1
        // sequence header and never looks at the record, so a container that
        // lies about it round-trips perfectly. Apple's decoder trusts the
        // record and refused the file outright.
        //
        // The profiles are what AV1 § 6.4.1 allows each sampling in, and the
        // narrowest is chosen because reach is the point: fewest decoders
        // implement Professional.
        let cases = [
            (Requested::Full, PROFILE_HIGH, false, false),
            (Requested::Half, PROFILE_PROFESSIONAL, true, false),
            (Requested::Quarter, PROFILE_MAIN, true, true),
        ];

        for (chroma, profile, sub_x, sub_y) in cases {
            let source = frame(64, 48);
            let spec = SequenceSpec {
                chroma,
                width: source.width,
                height: source.height,
                frames: 1,
                loops: None,
                quality: 80.0,
                density: 1.0,
                color: ColorProfile::of(PixelColorSpace::Srgb),
                space: PixelColorSpace::Srgb,
                depth: FrameDepth::Eight,
                // Eight bits, so the depth cannot be what raises the
                // profile -- twelve is Professional whatever the sampling.
                bits: Some(8),
            };
            let mut bytes = Cursor::new(Vec::new());
            {
                let mut sink = start(ImageFormat::Avif, &spec, &mut bytes)
                    .expect("the spec is well formed");
                sink.write_frame(&source).expect("a well formed frame");
                sink.finish().expect("the encoder closes");
            }

            let configs = av1_configs(&bytes.into_inner());
            let colour = configs.first().expect("a colour configuration");
            assert_eq!(colour.profile, profile, "{chroma:?} profile");
            assert_eq!(colour.subsampled_x, sub_x, "{chroma:?} horizontally");
            assert_eq!(colour.subsampled_y, sub_y, "{chroma:?} vertically");
        }
    }

    #[test]
    fn every_depth_av1_codes_is_a_depth_this_writes() {
        // The whole of the claim: AVIF carries 8, 10 and 12, and asking for
        // one gets a file that says so in the place a decoder reads it --
        // the bitstream's own configuration record, not just the container.
        for bits in BIT_DEPTHS.iter().copied() {
            let bytes = encoded_deeply(
                &frame(64, 48),
                80.0,
                PixelColorSpace::Srgb,
                FrameDepth::Eight,
                Some(bits),
            );
            assert_eq!(&bytes[8..12], b"avif", "{bits} bits is still an AVIF");

            let [colour] = av1_configs(&bytes)[..] else {
                panic!("one av1C for an opaque picture at {bits} bits")
            };
            assert_eq!(colour.bits(), bits, "the depth at {bits}");
            assert!(
                !colour.subsampled_x && !colour.subsampled_y,
                "4:4:4 at {bits} bits"
            );
            assert!(!colour.monochrome, "colour at {bits} bits");
            // Profile 1 is High -- 4:4:4 at eight or ten bits. Twelve is
            // past what High allows at any subsampling, so it is Profile 2,
            // Professional. Both are what AV1's own profile table says, and
            // getting this wrong writes a file whose container claims less
            // than its bitstream needs.
            let expected = match bits {
                12 => 2,
                _ => 1,
            };
            assert_eq!(colour.profile, expected, "the profile at {bits} bits");
        }
    }

    #[test]
    fn the_depth_follows_the_canvas_when_nothing_asks_for_one() {
        let shallow = encoded_deeply(
            &frame(64, 48),
            80.0,
            PixelColorSpace::Srgb,
            FrameDepth::Eight,
            None,
        );
        let deep = encoded_deeply(
            &frame(64, 48),
            80.0,
            PixelColorSpace::Srgb,
            FrameDepth::Sixteen,
            None,
        );
        // Ten from eight bits is the headroom `SHALLOW_BITS` is named for,
        // and is what this crate wrote before it could write anything else:
        // an existing export must not have moved.
        assert_eq!(av1_configs(&shallow)[0].bits(), SHALLOW_BITS);
        assert_eq!(av1_configs(&deep)[0].bits(), DEEP_BITS);
    }

    #[test]
    fn alpha_is_coded_at_the_depth_the_colour_is() {
        // Not a preference: the specification requires the two images to
        // agree, so a decoder that trusted the alpha item's own `av1C`
        // would read the plane at the wrong depth.
        let mut source = frame(32, 32);
        let Pixels::Eight(pixels) = &mut source.pixels else {
            panic!("the helper builds eight-bit frames")
        };
        pixels[3] = 0;

        for bits in BIT_DEPTHS.iter().copied() {
            let bytes = encoded_deeply(
                &source,
                80.0,
                PixelColorSpace::Srgb,
                FrameDepth::Eight,
                Some(bits),
            );
            let configs = av1_configs(&bytes);
            let [colour, alpha] = configs[..] else {
                panic!("a colour and an alpha av1C at {bits} bits")
            };
            assert_eq!(colour.bits(), bits, "the colour depth at {bits}");
            assert_eq!(alpha.bits(), bits, "the alpha depth at {bits}");
            assert!(alpha.monochrome, "the alpha image is one plane");
        }
    }

    #[test]
    fn eight_bits_out_of_an_eight_bit_canvas_is_the_byte_itself() {
        // The widening on the way in is `v * 257`, so the narrowing back to
        // eight has to be exact for the shallowest AVIF to be the picture
        // the canvas holds rather than a rounded copy of it. Ten is checked
        // against the expression this encoder used before `narrow` took a
        // depth, which is the guarantee that no existing file moved.
        for value in 0..=u8::MAX {
            let wide = u16::from(value) * 257;
            assert_eq!(narrow(wide, 8), u16::from(value), "{value} at eight");
            assert_eq!(
                narrow(wide, 10),
                (u16::from(value) << 2) | (u16::from(value) >> 6),
                "{value} at ten"
            );
        }
    }

    /// Encodes `count` pages as one export, at `fps`.
    fn animated(count: usize, loops: Option<u32>) -> Vec<u8> {
        let spec = SequenceSpec {
            chroma: Requested::Full,
            width: 32,
            height: 32,
            frames: count,
            loops,
            quality: 80.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
            space: PixelColorSpace::Srgb,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Avif, &spec, &mut bytes)
                .expect("the spec is well formed");
            for step in 0..count {
                let mut source = frame(32, 32);
                if let Pixels::Eight(px) = &mut source.pixels {
                    // Something that actually moves, so the frames differ.
                    for (at, byte) in px.iter_mut().enumerate() {
                        if at % 4 == 0 {
                            *byte = (step * 20) as u8;
                        }
                    }
                }
                source.delay_ms = 40;
                sink.write_frame(&source).expect("a well formed frame");
            }
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    #[test]
    fn one_page_is_still_a_still() {
        // Every AVIF this crate wrote before animation was a still, and a
        // single-page export must go on being one -- an `avis` file with a
        // one-frame track would be a different file for the same drawing.
        let bytes = animated(1, None);
        assert_eq!(&bytes[8..12], b"avif", "the still brand");
        assert!(!bytes.windows(4).any(|w| w == b"moov"), "and no movie box");
    }

    #[test]
    fn several_pages_become_one_animation() {
        let bytes = animated(6, None);
        assert_eq!(&bytes[8..12], b"avis", "the animated brand");
        assert!(bytes.windows(4).any(|w| w == b"moov"));

        // Six samples, and only the first stands alone -- which is what
        // makes it an animation rather than six stills in a box.
        let stsz = bytes
            .windows(4)
            .position(|w| w == b"stsz")
            .expect("a size table");
        assert_eq!(
            u32::from_be_bytes(bytes[stsz + 12..stsz + 16].try_into().unwrap()),
            6
        );
        let stss = bytes
            .windows(4)
            .position(|w| w == b"stss")
            .expect("a sync table");
        assert_eq!(
            u32::from_be_bytes(bytes[stss + 8..stss + 12].try_into().unwrap()),
            1,
            "one key frame"
        );
    }

    #[test]
    fn coding_the_frames_together_beats_coding_them_apart() {
        // The reason the animated form exists. Six frames of one drawing,
        // coded as a sequence, against the same six as separate stills.
        let animation = animated(6, None).len();
        let stills: usize = (0..6).map(|_| encoded(32, 32, 80.0).len()).sum();
        assert!(
            animation < stills,
            "a sequence of six ({animation}) should beat six stills ({stills})"
        );
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
        let mut pixels = faded.eight().into_owned();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 128;
        }
        faded.pixels = Pixels::Eight(pixels);
        let transparent = encoded_frame(&faded, 80.0, PixelColorSpace::Srgb);

        // `auxC` is the box declaring the second image's role as alpha.
        let has_alpha = |b: &[u8]| b.windows(4).any(|window| window == b"auxC");
        assert!(!has_alpha(&opaque), "nothing transparent, no alpha plane");
        assert!(has_alpha(&transparent), "half transparent, alpha plane");
        assert!(transparent.len() > opaque.len());
    }
}
