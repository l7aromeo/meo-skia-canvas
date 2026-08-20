//! AVIF, through libaom and avif-serialize.
//!
//! The one format here whose encoder is a video codec. An AV1 intra frame is
//! what an AVIF holds, which buys compression nothing else in this crate
//! approaches and costs correspondingly: encoding is measured in tenths of a
//! second where PNG is measured in milliseconds.
//!
//! Stills and animations both: a single page is one coded image described by
//! a property list, and several pages are an AV1 sequence muxed by
//! [`sequence`].
//!
//! # Why libaom, and not rav1e
//!
//! This encoded through `ravif` and then through rav1e directly, and now
//! through libaom, for a reason each time.
//!
//! `ravif` went first because it hardcodes BT.709 primaries and the sRGB
//! transfer function into the bitstream and exposes no way to change either,
//! so a Display P3 canvas could not be written as a Display P3 AVIF. Using
//! the two crates it wraps directly fixed that and made the tree smaller.
//!
//! rav1e went because it cannot code losslessly. Its source says so -- the
//! lossless block is `not yet supported` -- and that is a coding tool rather
//! than a setting, so no quantizer reaches it. libaom has
//! `AV1E_SET_LOSSLESS`, and libaom was already linked in to *decode*, so the
//! encoder and decoder are now one library's reading of the specification
//! rather than two.
//!
//! Three things came with that move. A sequence no longer has a floor on its
//! size: rav1e refused anything under sixteen pixels a side, which this crate
//! reported as though it were the format's rule. The `av1C` record for a
//! sequence is written by hand here, because libaom offers no accessor for
//! it where rav1e did. And [`rgb_to_ycbcr`] gained a clamp -- a fully
//! saturated primary lands a chroma difference exactly on the top of the
//! coded range, and rounding pushed it one past; rav1e absorbed that
//! silently, libaom asserts.
//!
//! What survives from `ravif` is the arithmetic: the BT.601 conversion and
//! the quality-to-quantizer curve are its, and are noted where they appear.

use std::borrow::Cow;

use rayon::prelude::*;

use avif_serialize::{
    Aviffy,
    constants::{ColorPrimaries, MatrixCoefficients, TransferCharacteristics},
};

use super::{
    Frame, FrameEncoder, FrameSink, Pixels, SequenceSpec, Sink,
    aom::{Colour, DEEP_SAMPLE_BYTES, Encoder, Packet, Sampling, Settings},
    color::ColorProfile,
    widen_to_sixteen,
};

use crate::export::ChromaSampling as Requested;

/// How hard libaom looks for a smaller file, from 0 to 9, slowest first.
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

/// The widest quantizer libaom takes, and the top of the curve below.
///
/// Sixty-three, where rav1e's was 255. The curve itself is a fraction of
/// this rather than a number of steps, so swapping encoders moved the scale
/// and left the shape of the dial alone.
const QUANTIZER_MAX: f32 = 63.0;

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
        Ok(Box::new(AvifSink {
            out,
            quality: spec.quality,
            color: spec.color,
            bits: spec.bits_or(SHALLOW_BITS, DEEP_BITS),
            chroma: spec.chroma,
            lossless: spec.lossless,
            loops: spec.loops,
            coding: None,
            // One page is a still, which is the form every AVIF this crate
            // wrote before now and the one every reader takes.
            animated,
            frames: spec.frames,
            width: spec.width,
            height: spec.height,
        }))
    }
}

/// An animation part-way through being coded.
///
/// Holds the coded samples rather than the pixels that produced them, and
/// one frame of pixels: the `meta` box points at a still, which is the first
/// frame coded on its own.
struct Streaming {
    colour: Encoder,
    /// Started at the first frame that is not fully opaque, and not before.
    ///
    /// An animation with no transparency should not pay to code an alpha
    /// track it will not keep, and one cannot be started late without the
    /// frames it missed -- except that those frames were all opaque, which
    /// is a plane this can synthesize rather than remember. So the cost of
    /// waiting is a run of constant frames fed in at the point transparency
    /// first appears, and the saving is every fully opaque animation.
    alpha: Option<Encoder>,
    samples: Vec<Packet>,
    alpha_samples: Vec<Packet>,
    /// One duration per frame, in milliseconds.
    delays: Vec<u32>,
    /// The first frame's pixels, for the still the container points at.
    ///
    /// At the depth the canvas drew them rather than at the coding depth,
    /// because widening is what the still is coded from and not what it is:
    /// an eight-bit page held as sixteen is twice the bytes, kept for the
    /// length of the sequence, to say exactly what half as many already
    /// said. Widened once in [`animate`](AvifSink::animate) instead, where
    /// the still is actually coded.
    first: Pixels,
    /// How many frames have been fed to the colour encoder.
    count: usize,
}

struct AvifSink<'a> {
    out: &'a mut dyn Sink,
    quality: f32,
    color: ColorProfile,
    bits: u8,
    /// How chroma is sampled, which is the caller's choice rather than this
    /// encoder's -- see `EncodeOptions::chroma` for why the default is full.
    chroma: Requested,
    /// Whether to code with no loss at all, which also settles the matrix:
    /// lossless means the planes carry green, blue and red unmixed.
    lossless: bool,
    width: u32,
    height: u32,
    /// How many times the animation plays; `None` is forever.
    loops: Option<u32>,
    /// The animation, coded as its frames arrive.
    ///
    /// This held every frame's pixels until `finish`, which is what
    /// [`encode`](crate::encode) warns against in its own words: a thousand
    /// frames of 1080p is 16 GB of sixteen-bit pixels before a byte is
    /// coded. libaom hands back a packet for each frame as it is fed, so
    /// only the coded samples need keeping, and those are the file.
    ///
    /// A single-page export writes the still form and starts none of this.
    coding: Option<Streaming>,
    /// Whether this export gathers pages into an animation at all.
    animated: bool,
    /// How many frames the sequence will hold, which is how far apart its
    /// key frames go: one at the start and none after it.
    frames: usize,
}

impl FrameSink for AvifSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        if self.animated {
            return self.code_frame(frame);
        }
        let encoded = encode(
            frame,
            self.quality,
            &self.color,
            self.bits,
            self.chroma,
            self.lossless,
        )?;
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
    /// The coding settings with the colour description attached.
    fn coding_with_colour(&self) -> Coding {
        Coding {
            description: Some(Colour {
                primaries: self.color.cicp.primaries,
                transfer: self.color.cicp.transfer,
                matrix: MatrixCoefficients::Bt601 as u8,
                full_range: true,
            }),
            ..self.coding()
        }
    }

    /// The coding settings both tracks are built from.
    fn coding(&self) -> Coding {
        Coding {
            width: self.width as usize,
            height: self.height as usize,
            bits: self.bits,
            quantizer: u32::from(quality_to_quantizer(
                self.quality.clamp(QUALITY_FLOOR, 100.0),
            )),
            chroma: sampling_of(self.chroma),
            description: None,
            lossless: false,
            monochrome: false,
        }
    }

    /// Codes one frame into the sequence, starting it if this is the first.
    fn code_frame(&mut self, frame: &Frame) -> Result<(), String> {
        let (width, height) = (self.width as usize, self.height as usize);
        let bits = self.bits;
        let sampling = sampling_of(self.chroma);
        let pixels = widened(frame);

        if self.coding.is_none() {
            // Validated before a frame is coded rather than after: a code
            // point neither crate can name should be refused at the door.
            primaries_named(self.color.cicp.primaries)?;
            transfer_named(self.color.cicp.transfer)?;
            self.coding = Some(Streaming {
                colour: sequence_encoder(
                    &self.coding_with_colour(),
                    self.frames,
                )?,
                alpha: None,
                samples: Vec::new(),
                alpha_samples: Vec::new(),
                delays: Vec::new(),
                first: frame.pixels.clone(),
                count: 0,
            });
        }
        // Borrowed after the block above so the encoder is certainly there.
        let opaque = pixels.par_chunks_exact(4).all(|px| px[3] == u16::MAX);
        let coding = self.coding();
        let state = match self.coding.as_mut() {
            Some(state) => state,
            None => return Err("The AVIF sequence did not start".to_string()),
        };

        {
            let mut planes = state.colour.planes();
            fill_ycbcr(&mut planes, width, height, &pixels, bits, sampling);
        }
        let at = state.count as i64;
        state.samples.extend(state.colour.encode(at, 1)?);
        state.delays.push(frame.delay_ms);

        // Transparency starts the second track, and the frames before it
        // were opaque by definition -- that is why this waited -- so they
        // are fed in as constant planes rather than remembered.
        if !opaque && state.alpha.is_none() {
            let opacity = Coding {
                chroma: Sampling::Quarter,
                description: None,
                monochrome: true,
                ..coding
            };
            let mut encoder = sequence_encoder(&opacity, self.frames)?;
            for earlier in 0..state.count {
                {
                    let mut planes = encoder.planes();
                    fill_opaque(&mut planes, width, height, bits);
                }
                state
                    .alpha_samples
                    .extend(encoder.encode(earlier as i64, 1)?);
            }
            state.alpha = Some(encoder);
        }
        if let Some(encoder) = state.alpha.as_mut() {
            {
                let mut planes = encoder.planes();
                fill_alpha(&mut planes, width, height, &pixels, bits);
            }
            state.alpha_samples.extend(encoder.encode(at, 1)?);
        }

        state.count += 1;
        Ok(())
    }

    /// The whole animation, once every frame has arrived.
    fn animate(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width as usize, self.height as usize);
        let coding = self.coding_with_colour();
        let Some(mut state) = self.coding.take() else {
            return Err("An animated AVIF needs at least one frame".to_string());
        };

        // The flush: libaom holds nothing back with no lag configured, but
        // the call is what says the sequence is over.
        state.samples.extend(state.colour.finish()?);
        if let Some(encoder) = state.alpha.as_mut() {
            state.alpha_samples.extend(encoder.finish()?);
        }
        if state.samples.len() != state.delays.len() {
            return Err(format!(
                "The AVIF encoder returned {} frames for {}",
                state.samples.len(),
                state.delays.len()
            ));
        }
        // The same check for the second track, which had none. `timed` pairs
        // samples with durations by `zip`, so a short alpha track would be
        // truncated silently into a file whose two tracks disagree about how
        // many frames they hold -- and the guard above would still pass,
        // because it only ever looked at the colour one.
        if state.alpha.is_some()
            && state.alpha_samples.len() != state.delays.len()
        {
            return Err(format!(
                "The AVIF encoder returned {} alpha frames for {}",
                state.alpha_samples.len(),
                state.delays.len()
            ));
        }

        // The still the `meta` box points at, coded on its own so a reader
        // that shows one frame has one that stands alone. See the note in
        // `sequence`: this is the format's duplication, not a shortcut.
        let first = state.first.sixteen();
        let still = encode_av1(&coding, |planes| {
            fill_ycbcr(
                planes,
                width,
                height,
                &first,
                self.bits,
                sampling_of(self.chroma),
            )
        })?;
        let config = av1_config(self.bits, sampling_of(self.chroma), false);
        let samples = timed(state.samples, &state.delays);

        // Transparency, where any frame had some. A second monochrome
        // sequence and a second still, which the container hangs off the
        // colour ones -- without this an animation came out opaque and
        // nothing said so, while the still form beside it kept its alpha.
        let opacity = Coding {
            chroma: Sampling::Quarter,
            description: None,
            monochrome: true,
            ..coding
        };
        let alpha = match state.alpha.is_some() {
            false => None,
            true => {
                let still = encode_av1(&opacity, |planes| {
                    fill_alpha(planes, width, height, &first, self.bits)
                })?;
                let config = av1_config(self.bits, Sampling::Quarter, true);
                Some((still, config, timed(state.alpha_samples, &state.delays)))
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

/// The quantizer, from 0 to 63, that `quality` asks for.
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
    // Clamped, not just rounded. A fully saturated primary puts a chroma
    // difference exactly on the top of the range before rounding -- pure red
    // at ten bits computes 1023.5 for Cr -- so the round alone lands on 1024,
    // one past what the depth can hold. rav1e absorbed that silently; libaom
    // asserts on it inside `av1_count_colors_highbd`, which is how a
    // long-standing hole in this arithmetic finally surfaced.
    let hold = |value: f32| value.round().clamp(0.0, max) as u16;
    [hold(y), hold(cb), hold(cr)]
}

/// The encoder's sampling that our own [`Requested`] names.
///
/// Two enums for one idea, deliberately: the public one is this crate's API
/// and must not hand a caller the codec's own type, which would make the
/// encoder impossible to change without a breaking release -- as this move
/// from rav1e to libaom would otherwise have been.
/// A frame's pixels as sixteen-bit RGBA, widened on every core.
///
/// [`Frame::sixteen`](crate::encode::Frame::sixteen) answers the same
/// question on one, which is right for a still and wrong for a sequence: an
/// eight-bit canvas is what a page usually is, AVIF codes it at ten bits or
/// more, and so every frame of an animation converts and allocates the whole
/// page -- 8.6 MB at 1200x900 -- before any of it is coded.
///
/// The arithmetic is [`widen_to_sixteen`]'s, unchanged and deliberately so:
/// this is the same conversion on more threads, not a different one, and a
/// sequence that widened differently from a still would code the same drawing
/// into different pixels.
fn widened(frame: &Frame) -> Cow<'_, [u16]> {
    match &frame.pixels {
        Pixels::Sixteen(deep) => Cow::Borrowed(deep),
        Pixels::Eight(bytes) => Cow::Owned(
            bytes.par_iter().copied().map(widen_to_sixteen).collect(),
        ),
    }
}

fn sampling_of(chroma: Requested) -> Sampling {
    match chroma {
        Requested::Full => Sampling::Full,
        Requested::Half => Sampling::Half,
        Requested::Quarter => Sampling::Quarter,
    }
}

/// The most tiles a frame is split into, and so the most threads encoding it.
///
/// A tile is what the encoder parallelises over, and a frame is one tile
/// unless something says otherwise -- so a picture encoded at that default
/// codes on one core whatever the machine has.
///
/// This number was eight and bought nothing, because nothing was setting the
/// tile counts: [`tiling_for`]'s answer reached the encoder as a thread count
/// alone, and libaom cannot spend threads it has no tiles to spend them on.
/// What made a 1200x900 page 5.6 seconds into 1.1 was row-level threading,
/// which libaom turns on by itself.
///
/// Thirty-two, measured on that page: 240.7 milliseconds untiled, 142.2 at
/// eight tiles and 77.6 at thirty-two, for 580.8 KB, 582.0 and 585.6 at the
/// same 41.76 dB. Tiles are coded independently, so each one costs a little
/// compression -- the entropy coder restarts at its boundary and prediction
/// cannot cross it -- and 0.8% of the file for three times the speed is where
/// that stops being worth taking further.
const MAX_TILES: u32 = 32;

/// The pixels a tile wants to itself before another is worth opening.
///
/// The compression a tile costs is roughly fixed while the time it saves
/// scales with the area, so on a small image the trade inverts: forcing
/// thirty-two tiles onto a 320x120 strip took it from 1.2 KB to 1.4, and an
/// earlier eight-tile attempt made such a strip larger than the PNG of the
/// same drawing, which a test caught.
///
/// A thirty-second of a megapixel, which puts a 1200x900 page on the full
/// thirty-two and leaves that strip whole -- it is 38400 pixels, so the first
/// split would already halve it below this and never happens.
const PIXELS_PER_TILE: usize = 32_768;

/// How many times to halve a frame of this size across and down.
///
/// Returned as the base-two logarithms libaom's tile controls take. Split
/// along whichever side of the *tile* is currently longer, so the pieces stay
/// as square as the page allows rather than becoming strips: a 1200x900 page
/// comes out eight across and four down, at 150x225 each.
///
/// Halving stops when the next one would put a tile under
/// [`PIXELS_PER_TILE`] or past [`MAX_TILES`], which is what leaves a small
/// image alone without a special case for it.
fn tiling_for(width: usize, height: usize) -> (u32, u32) {
    let (mut across, mut down) = (0u32, 0u32);
    loop {
        let (tile_w, tile_h) = (width >> across, height >> down);
        let split_across = tile_w >= tile_h;
        let (next_w, next_h) = match split_across {
            true => (tile_w / 2, tile_h),
            false => (tile_w, tile_h / 2),
        };
        if next_w * next_h < PIXELS_PER_TILE
            || 1 << (across + down + 1) > MAX_TILES
        {
            return (across, down);
        }
        match split_across {
            true => across += 1,
            false => down += 1,
        }
    }
}

/// How many tiles [`tiling_for`] asks for, which is also how many threads are
/// worth giving the encoder.
fn tiles_for(width: usize, height: usize) -> u32 {
    let (across, down) = tiling_for(width, height);
    1 << (across + down)
}

/// Codes one AV1 image and returns its bitstream.
///
/// The `fill` closure is handed the encoder's own planes rather than a
/// buffer to copy from, so a frame is written once instead of twice.
fn encode_av1(
    coding: &Coding,
    fill: impl FnOnce(&mut [Vec<&mut [u8]>; 3]),
) -> Result<Vec<u8>, String> {
    let Coding {
        width,
        height,
        bits,
        quantizer,
        chroma,
        description,
        lossless,
        monochrome,
    } = *coding;
    let mut encoder = Encoder::new(&Settings {
        width: width as u32,
        height: height as u32,
        bits,
        sampling: chroma,
        quantizer,
        speed: u32::from(SPEED),
        lossless,
        monochrome,
        still: true,
        frames: 1,
        threads: tiles_for(width, height),
        tiling: tiling_for(width, height),
        colour: description,
    })?;
    {
        let mut planes = encoder.planes();
        fill(&mut planes);
    }

    // One frame in, then the flush that tells libaom nothing more is coming.
    // A still is a single temporal unit, so both halves belong to it.
    let mut packets = encoder.encode(0, 1)?;
    packets.extend(encoder.finish()?);
    Ok(packets.into_iter().flat_map(|packet| packet.data).collect())
}

/// One frame as a complete AVIF file.
fn encode(
    frame: &Frame,
    quality: f32,
    color: &ColorProfile,
    bits: u8,
    chroma: Requested,
    lossless: bool,
) -> Result<Vec<u8>, String> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let quantizer =
        u32::from(quality_to_quantizer(quality.clamp(QUALITY_FLOOR, 100.0)));

    // The colour description goes into the AV1 sequence header as well as
    // into the container below, so the file answers the question once. The
    // code points travel as ITU-T H.273 numbers, which is what both halves
    // speak -- no translation table between them to disagree.
    let primaries = primaries_named(color.cicp.primaries)?;
    let transfer = transfer_named(color.cicp.transfer)?;
    // Lossless codes green, blue and red directly, so the matrix that says
    // "nothing was mixed" is part of what makes it lossless rather than a
    // detail of how it is described.
    let matrix = match lossless {
        true => MatrixCoefficients::Rgb,
        false => MatrixCoefficients::Bt601,
    };
    let description = Colour {
        primaries: color.cicp.primaries,
        transfer: color.cicp.transfer,
        matrix: matrix as u8,
        full_range: true,
    };

    // Alpha is a second AV1 image, monochrome, and left out entirely where
    // nothing is transparent -- which is most canvas output and most of the
    // file. It is coded at the colour image's depth because the
    // specification requires the two to match, not because it needs it.
    // Sixteen-bit throughout, whatever `bits` turns out to be: the widest
    // form both an eight-bit and a float canvas fit into, narrowed once at
    // the point the planes are filled.
    let pixels = frame.sixteen();
    let opaque = pixels.as_chunks::<4>().0.iter().all(|px| px[3] == u16::MAX);

    let sampling = sampling_of(chroma);
    let colour = Coding {
        width,
        height,
        bits,
        quantizer,
        chroma: sampling,
        description: Some(description),
        lossless,
        monochrome: false,
    };
    let color_payload = encode_av1(&colour, |planes| match lossless {
        true => fill_identity(planes, width, height, &pixels, bits),
        false => fill_ycbcr(planes, width, height, &pixels, bits, sampling),
    })?;
    let alpha_payload = match opaque {
        true => None,
        false => Some(encode_av1(
            // Alpha is monochrome, and libaom lays a monochrome picture out
            // as 4:2:0 with the chroma planes left alone.
            &Coding {
                chroma: Sampling::Quarter,
                description: None,
                monochrome: true,
                ..colour
            },
            |planes| fill_alpha(planes, width, height, &pixels, bits),
        )?),
    };

    let mut aviffy = Aviffy::new();
    let (shift_x, shift_y) = sampling.shifts();
    aviffy
        .matrix_coefficients(matrix)
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
        .set_seq_profile(sampling.profile(bits >= DEEP_BITS) as u8)
        .premultiplied_alpha(false);

    Ok(aviffy.to_vec(
        &color_payload,
        alpha_payload.as_deref(),
        frame.width,
        frame.height,
        bits,
    ))
}

/// Writes one sample into a plane row at the width its depth needs.
///
/// libaom hands every plane back as bytes. Above eight bits a sample is two
/// of them in the host's own order, which is the one place byte order
/// matters on the way out as well as on the way in.
fn put(row: &mut [u8], at: usize, value: u16, deep: bool) {
    match deep {
        true => {
            let bytes = value.to_ne_bytes();
            let start = at * DEEP_SAMPLE_BYTES;
            if let Some(pair) = row.get_mut(start..start + DEEP_SAMPLE_BYTES) {
                pair.copy_from_slice(&bytes);
            }
        }
        false => {
            if let Some(sample) = row.get_mut(at) {
                *sample = value as u8;
            }
        }
    }
}

/// Fills an AV1 frame's three planes from RGBA pixels.
fn fill_ycbcr(
    planes: &mut [Vec<&mut [u8]>; 3],
    width: usize,
    height: usize,
    pixels: &[u16],
    bits: u8,
    sampling: Sampling,
) {
    let deep = bits > 8;
    let (shift_x, shift_y) = sampling.shifts();

    // Every part of this is a pure function of one pixel or one rectangle of
    // them, so all of it runs on the pool. It is worth spelling out why that
    // is allowed here when the coding it feeds is strictly serial: AV1 codes
    // a frame against the one before it, but *converting* a frame does not
    // look at any other frame, and this is the conversion.
    //
    // Converted once and kept, because a subsampled chroma sample is an
    // average over several pixels and every one of them is also a luma
    // sample. Converting twice would double the arithmetic that dominates
    // this function.
    let converted: Vec<[u16; 3]> = pixels
        .par_chunks_exact(4)
        .map(|px| rgb_to_ycbcr(px[0], px[1], px[2], bits))
        .collect();

    let [luma, blue, red] = planes;
    // A row at a time: each is its own `&mut [u8]`, so no two threads write
    // the same plane bytes and the borrow checker can see it.
    luma.par_iter_mut()
        .take(height)
        .enumerate()
        .for_each(|(row, out)| {
            for (at, sample) in
                converted[row * width..(row + 1) * width].iter().enumerate()
            {
                put(out, at, sample[0], deep);
            }
        });

    // A chroma cell covers `1 << shift` pixels on each axis, and its value is
    // their mean rather than one of them. Picking a single pixel is cheaper
    // and visibly worse: it throws away three quarters of the chroma at
    // 4:2:0 instead of averaging it, which shows on any edge between two
    // saturated colours.
    let cells_across = width.div_ceil(1 << shift_x);
    let cells_down = height.div_ceil(1 << shift_y);
    // Zipped rather than indexed, which is also what bounds the walk: a plane
    // shorter than the cell count stops it, the way the `get_mut` that was
    // here did.
    blue.par_iter_mut()
        .zip(red.par_iter_mut())
        .take(cells_down)
        .enumerate()
        .for_each(|(cell_y, (blue_row, red_row))| {
            let from_y = cell_y << shift_y;
            let to_y = ((cell_y + 1) << shift_y).min(height);
            for cell_x in 0..cells_across {
                let from_x = cell_x << shift_x;
                let to_x = ((cell_x + 1) << shift_x).min(width);

                let (mut blues, mut reds, mut count) = (0u32, 0u32, 0u32);
                for y in from_y..to_y {
                    for x in from_x..to_x {
                        let sample = converted[y * width + x];
                        blues += u32::from(sample[1]);
                        reds += u32::from(sample[2]);
                        count += 1;
                    }
                }
                // A cell covering no pixel is not something the ceiling
                // division above can produce.
                let count = count.max(1);
                put(blue_row, cell_x, (blues / count) as u16, deep);
                put(red_row, cell_x, (reds / count) as u16, deep);
            }
        });
}

/// Fills an AV1 frame's three planes with green, blue and red themselves.
///
/// The identity matrix (ITU-T H.273 matrix 0) does not decorrelate colour at
/// all: the three planes *are* G, B and R, in the order a luma and two chroma
/// differences would otherwise occupy. That costs a great deal on a lossy
/// file -- measured at 33 to 41% larger on a gradient, which is why it is not
/// the default -- and is the only way a lossless one is lossless in RGB
/// rather than merely in what the encoder was handed.
fn fill_identity(
    planes: &mut [Vec<&mut [u8]>; 3],
    width: usize,
    height: usize,
    pixels: &[u16],
    bits: u8,
) {
    let deep = bits > 8;
    let [green, blue, red] = planes;
    for row in 0..height {
        let (Some(g), Some(b), Some(r)) =
            (green.get_mut(row), blue.get_mut(row), red.get_mut(row))
        else {
            break;
        };
        let source = &pixels[row * width * 4..(row + 1) * width * 4];
        for (at, px) in source.as_chunks::<4>().0.iter().enumerate() {
            put(g, at, narrow(px[1], bits), deep);
            put(b, at, narrow(px[2], bits), deep);
            put(r, at, narrow(px[0], bits), deep);
        }
    }
}

/// Fills a monochrome AV1 frame's one plane from the alpha channel.
fn fill_alpha(
    planes: &mut [Vec<&mut [u8]>; 3],
    width: usize,
    height: usize,
    pixels: &[u16],
    bits: u8,
) {
    let deep = bits > 8;
    for (row, out) in planes[0].iter_mut().take(height).enumerate() {
        let source = &pixels[row * width * 4..(row + 1) * width * 4];
        for (at, px) in source.as_chunks::<4>().0.iter().enumerate() {
            put(out, at, narrow(px[3], bits), deep);
        }
    }
}

/// What one coded stream is built from, colour or alpha.
///
/// Shared by the still and the sequence paths, which differ in how many
/// frames they send rather than in how they are configured.
struct Coding {
    width: usize,
    height: usize,
    bits: u8,
    quantizer: u32,
    chroma: Sampling,
    description: Option<Colour>,
    /// Whether to code with no loss. Only ever true for colour: alpha is
    /// carried at the colour image's own fidelity.
    lossless: bool,
    /// Whether the stream is luma alone, which alpha is.
    monochrome: bool,
}

/// The `av1C` configuration record for a coded sequence.
///
/// The still path gets this from `avif-serialize`, which builds it from what
/// it is told. A sequence is muxed here instead, so the four bytes are
/// written here too -- to the AV1-ISOBMFF specification § 2.3.1.
///
/// rav1e used to hand this over through `container_sequence_header`, which
/// is the one thing lost by moving to libaom: libaom codes the sequence
/// header into the bitstream and offers no accessor for the record.
fn av1_config(bits: u8, chroma: Sampling, monochrome: bool) -> Vec<u8> {
    /// The marker bit and the record's version, which is 1.
    const MARKER_AND_VERSION: u8 = 0b1000_0001;
    /// The level meaning "no stated constraint".
    ///
    /// Computing a real level needs the bitstream parsed, and `avif-serialize`
    /// writes 31 here for the same reason; libavif does the same. A reader
    /// uses it to reject a stream it cannot handle, and 31 declines to make
    /// the promise rather than making a false one.
    const LEVEL_UNCONSTRAINED: u8 = 31;

    let (shift_x, shift_y) = chroma.shifts();
    let profile = chroma.profile(bits >= DEEP_BITS);
    vec![
        MARKER_AND_VERSION,
        ((profile as u8) << 5) | LEVEL_UNCONSTRAINED,
        (u8::from(bits >= SHALLOW_BITS) << 6)
            | (u8::from(bits >= DEEP_BITS) << 5)
            | (u8::from(monochrome) << 4)
            | (u8::from(shift_x > 0) << 3)
            | (u8::from(shift_y > 0) << 2),
        // No initial presentation delay, and the reserved bits above it are
        // zero.
        0,
    ]
}

/// Pairs coded packets with the durations their frames were given.
///
/// The encoder knows nothing about how long a frame is shown -- the
/// container carries that -- so the two lists are joined here, at the point
/// the sample table is built.
fn timed(packets: Vec<Packet>, delays: &[u32]) -> Vec<sequence::Sample> {
    packets
        .into_iter()
        .zip(delays)
        .map(|(packet, delay_ms)| sequence::Sample {
            data: packet.data,
            duration: sequence::ticks(*delay_ms),
            sync: packet.key,
        })
        .collect()
}

/// An encoder configured for one track of a sequence.
///
/// Split out of the old whole-sequence function so frames can be fed as they
/// arrive rather than gathered first.
fn sequence_encoder(coding: &Coding, frames: usize) -> Result<Encoder, String> {
    Encoder::new(&Settings {
        width: coding.width as u32,
        height: coding.height as u32,
        bits: coding.bits,
        sampling: coding.chroma,
        quantizer: coding.quantizer,
        speed: u32::from(SPEED),
        lossless: coding.lossless,
        monochrome: coding.monochrome,
        still: false,
        frames: frames.max(1) as u32,
        threads: tiles_for(coding.width, coding.height),
        tiling: tiling_for(coding.width, coding.height),
        colour: coding.description,
    })
}

/// Fills a monochrome frame with "fully opaque".
///
/// The alpha track can start late because everything it missed was opaque,
/// and an opaque plane is a constant rather than something to remember.
fn fill_opaque(
    planes: &mut [Vec<&mut [u8]>; 3],
    width: usize,
    height: usize,
    bits: u8,
) {
    let deep = bits > 8;
    let full = narrow(u16::MAX, bits);
    for out in planes[0].iter_mut().take(height) {
        for at in 0..width {
            put(out, at, full, deep);
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_frame_is_divided_until_a_tile_would_be_too_small() {
        // Tiles are what libaom parallelises over, and this used to answer
        // only a thread count -- so the threads had one tile between them.
        // A 1200x900 page comes out eight across and four down, which is
        // 240.7 milliseconds untiled against 77.6 tiled, for 0.8% more file.
        assert_eq!(tiling_for(1200, 900), (3, 2));
        assert_eq!(tiles_for(1200, 900), 32);

        // The pieces stay as square as the page allows rather than becoming
        // strips: 1200x900 divides to 150x225, not to 32 columns of 37.
        let (across, down) = tiling_for(1200, 900);
        assert_eq!((1200 >> across, 900 >> down), (150, 225));

        // Small enough that the first halving would already put a tile under
        // the budget, so it is left whole -- which is what keeps a strip from
        // paying the per-tile cost it cannot afford.
        assert_eq!(tiling_for(320, 120), (0, 0));
        assert_eq!(tiles_for(320, 120), 1);

        // The cap holds however large the page, and the count is always a
        // power of two so the two logarithms describe it exactly.
        for (width, height) in [(4000, 3000), (16000, 200), (200, 16000)] {
            let tiles = tiles_for(width, height);
            assert!(tiles <= MAX_TILES, "{width}x{height} asked for {tiles}");
            assert!(tiles.is_power_of_two());
            let (across, down) = tiling_for(width, height);
            assert!(
                (width >> across) * (height >> down) >= PIXELS_PER_TILE,
                "{width}x{height} split past the budget"
            );
        }
    }
    use crate::encode::FrameDepth;
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
            lossless: false,
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
        // The profile numbers are AV1 specification § 6.4.1's own -- Main
        // is 0, High is 1, Professional is 2 -- written out rather than
        // taken from `Sampling::profile`, which is the code under test. A
        // test that asked the same function would assert only that it agrees
        // with itself.
        let cases = [
            (Requested::Full, 1, false, false),
            (Requested::Half, 2, true, false),
            (Requested::Quarter, 0, true, true),
        ];

        for (chroma, profile, sub_x, sub_y) in cases {
            let source = frame(64, 48);
            let spec = SequenceSpec {
                chroma,
                lossless: false,
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
            lossless: false,
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
    fn a_tiny_canvas_animates_now_that_libaom_codes_it() {
        const SIDE_UNDER_TEST: u32 = 2;
        // rav1e refused a sequence narrower or shorter than sixteen pixels --
        // `invalid width 4 (expected >= 16, ..)` -- so this crate refused one
        // too, and said so in an error naming a limit that was the encoder's
        // rather than the format's. libaom has no such floor, and the limit
        // left with rav1e.
        let side = SIDE_UNDER_TEST;
        let spec = SequenceSpec {
            chroma: Requested::Full,
            lossless: false,
            width: side,
            height: side,
            frames: 3,
            loops: None,
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
                .expect("a sequence this small is allowed");
            for _ in 0..3 {
                let mut source = frame(side, side);
                source.delay_ms = 40;
                sink.write_frame(&source).expect("a well formed frame");
            }
            sink.finish().expect("the encoder closes");
        }

        let bytes = bytes.into_inner();
        assert_eq!(&bytes[8..12], b"avis", "the animated brand");
        assert!(
            bytes.windows(4).any(|w| w == b"moov"),
            "and a movie box to go with it"
        );
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
        for pixel in pixels.as_chunks_mut::<4>().0.iter_mut() {
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
