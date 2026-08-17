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

use std::slice;

use rayon::prelude::*;

use super::{
    Frame, FrameDepth, FrameEncoder, FrameSink, SequenceSpec, Sink,
    changed_region, color::ColorProfile, crop_bytes,
};
use crate::pixels::PixelColorSpace;
use png::{
    BitDepth, BlendOp, ColorType, Compression, DisposeOp, Encoder, ScaledFloat,
    SourceChromaticities, Writer, chunk,
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

/// The multiple an `fcTL` offset has to land on.
///
/// One, meaning none: APNG stores both offsets as whole pixels, unlike
/// WebP, which halves them and so cannot start a frame on an odd column.
const FRAME_ORIGIN_GRAIN: u32 = 1;

/// The rectangle a frame carries when nothing changed at all.
///
/// A still passage of an animation is two identical frames, and an `fcTL`
/// with a zero width or height is a format error the `png` crate refuses
/// outright, so the frame still has to carry a pixel. Any pixel will do, and
/// the one at the origin is already the right colour.
const SMALLEST_FRAME: (u32, u32, u32, u32) = (0, 0, 1, 1);

/// The signature every PNG opens with, in bytes.
///
/// Only its length matters here: this module reads back files the `png` crate
/// has just written, so what the eight bytes are is not in question -- where
/// the first chunk starts is.
const SIGNATURE_LEN: usize = 8;

/// A chunk's type, which is four characters.
const CHUNK_TYPE_LEN: usize = 4;

/// What precedes a chunk's payload: its length, then its type.
const CHUNK_HEADER_LEN: usize = size_of::<u32>() + CHUNK_TYPE_LEN;

/// What follows a chunk's payload: the CRC over its type and data.
const CHUNK_CRC_LEN: usize = size_of::<u32>();

/// The most a chunk's payload may hold.
///
/// PNG caps the length field at 2^31 - 1 rather than at what its 32 bits
/// could express, so that a reader can hold one in a signed integer without
/// the top bit meaning anything. Clause 5.3 of the specification.
const MAX_CHUNK_PAYLOAD: usize = i32::MAX as usize;

/// The most of a frame's compressed stream one `fdAT` can carry.
///
/// The chunk limit less the sequence number that sits in front of the data,
/// which is part of the payload rather than of the chunk header.
const MAX_FDAT_STREAM: usize = MAX_CHUNK_PAYLOAD - size_of::<u32>();

/// An `fcTL` payload: a sequence number, the rectangle it describes, how long
/// to show it, and how it composes.
///
/// Derived from its fields rather than written as 26, which is what they come
/// to. The order is fixed by the APNG specification and matched by
/// `png::FrameControl::encode`, which is what wrote these chunks before this
/// module wrote them itself.
const FRAME_CONTROL_LEN: usize = size_of::<u32>()          // sequence number
    + 2 * size_of::<u32>()                                 // width, height
    + 2 * size_of::<u32>()                                 // x, y offsets
    + 2 * size_of::<u16>()                                 // delay fraction
    + 2; // dispose_op, blend_op

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
        // The `png` crate's two compressor paths are a strategy apart rather
        // than an implementation apart: `Balanced` and `High` go through
        // flate2, while `Fast` uses `fdeflate`, a DEFLATE written for PNG's
        // data. Nothing about the picture changes -- PNG is lossless, and
        // both the compression and the row filtering are reversible, so the
        // two settings decode to the same pixels. Checked rather than assumed:
        // a twelve-frame animation written both ways decoded to byte-identical
        // RGBA, 15,360,000 bytes at the same md5.
        //
        // What changes is time against size. Measured on release builds, a
        // thirty-frame 640x500 animation encoded in 66ms against 649, and a
        // still 1200x900 page in 13.9ms against 89.4 -- six to ten times
        // faster -- for files 16% to 42% larger, the spread depending on how
        // much redundancy the drawing has for the slower search to find.
        //
        // Taken as the default because the cost is bytes and the saving is
        // an order of magnitude: an animation is the common case here, one
        // page is one frame, and a caller who wanted the smaller file would
        // have to wait ten times as long for pixels they already had.
        //
        // Swapping flate2's backend instead -- the `zlib-rs` feature -- was
        // measured on the same benchmark and changed nothing, which is what
        // identifies the strategy rather than the implementation as what
        // mattered.
        encoder.set_compression(Compression::Fast);
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
            // The frames of an animation are written by this module rather
            // than by `Writer::write_image_data`, so that they can be
            // compressed on more than one thread -- see
            // [`ApngSink::write_batch`]. The crate counts frames only inside
            // that call, so leaving its check on would have `finish` report
            // an animation short of every frame it was given.
            //
            // Nothing is lost by turning it off. What it checks is that the
            // number of frames written matches the number `acTL` declared,
            // and `Checked` in this module's parent already refuses both
            // halves of that -- one more frame than declared, and a `finish`
            // with fewer.
            encoder.validate_sequence(false);
        }

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Could not write the PNG header: {e}"))?;
        write_cicp(&mut writer, &spec.color)?;

        Ok(Box::new(ApngSink {
            writer,
            animated,
            depth,
            previous: None,
            sequence: 0,
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
    /// The last frame written, in the bytes it was written as, so the next
    /// one can be reduced to the rectangle it differs from them in.
    ///
    /// `None` until the first frame, which is the whole canvas because there
    /// is nothing before it to differ from -- and because it is what a wrap
    /// of the animation repaints from.
    previous: Option<Vec<u8>>,
    /// The next APNG sequence number to hand out.
    ///
    /// Every `fcTL` and every `fdAT` carries one, and they run consecutively
    /// from zero across both -- an `fcTL` and the `fdAT`s that follow it do
    /// not share a number. Kept here rather than left to the `png` crate
    /// because this module writes those chunks itself, and the crate's own
    /// counter would otherwise be the source of a number nothing keeps in
    /// step with.
    sequence: u32,
}

/// Bytes in one pixel at the depth a file is being written at.
fn bytes_per_pixel(depth: BitDepth) -> usize {
    match depth {
        // RGBA at two bytes a channel.
        BitDepth::Sixteen => 8,
        _ => 4,
    }
}

/// A frame's pixels in the byte order PNG stores them in.
fn wide_bytes(frame: &Frame, depth: BitDepth) -> Vec<u8> {
    match depth {
        // PNG is big-endian, and the crate takes bytes either way.
        BitDepth::Sixteen => frame
            .sixteen()
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect(),
        _ => frame.eight().into_owned(),
    }
}

/// One frame taken as far as it can go before its turn to be written.
///
/// Both fields are a function of the frame's own pixels and its
/// predecessor's, so they can be produced on any thread. The sequence number
/// is deliberately not here: it belongs to the frame's position in the file,
/// which nothing knows until the frame is written.
struct Prepared {
    /// The rectangle of the canvas this frame carries.
    region: (u32, u32, u32, u32),
    /// Its pixels, filtered and deflated, as an `IDAT` or `fdAT` holds them.
    stream: Vec<u8>,
}

/// The zlib stream a PNG of just these pixels carries.
///
/// A whole throwaway file is written and all but its `IDAT` discarded, which
/// looks wasteful and is the point: `Writer::write_image_data` filters,
/// deflates and writes in one call, and the crate offers no way to do the
/// first two without the third. Encoding into a buffer is what puts the
/// compression somewhere a thread other than the writer's can do it.
///
/// The stream this produces is the stream the animated writer would have
/// produced for the same pixels. Checked before this was built rather than
/// assumed: for seven rectangles -- the whole canvas, one pixel, and five
/// odd sizes -- at both depths, a standalone PNG's joined `IDAT` was
/// byte-identical to the animated writer's `fdAT`. That had to be true or
/// this would be a change to every animation the crate has ever written,
/// rather than a change to how long one takes.
fn compressed(
    pixels: &[u8],
    width: u32,
    height: u32,
    depth: BitDepth,
) -> Result<Vec<u8>, String> {
    let mut file = Vec::new();
    {
        let mut encoder = Encoder::new(&mut file, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(depth);
        // The setting the frames of this animation are compressed at. A
        // different one here would still decode -- PNG is lossless either
        // way -- but the file would stop being the one the serial path
        // wrote.
        encoder.set_compression(Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| {
            format!("Could not start a PNG frame's compression: {e}")
        })?;
        writer
            .write_image_data(pixels)
            .map_err(|e| format!("Could not compress a PNG frame: {e}"))?;
        writer
            .finish()
            .map_err(|e| format!("Could not compress a PNG frame: {e}"))?;
    }
    idat_stream(&file)
}

/// The `IDAT` payloads of a PNG, joined back into the one zlib stream they
/// are a division of.
///
/// A PNG may split its stream across any number of `IDAT`s at any boundary,
/// so the chunking carries no information and joining loses none.
fn idat_stream(file: &[u8]) -> Result<Vec<u8>, String> {
    let short =
        || "A PNG frame was compressed into a truncated file".to_string();
    let mut stream = Vec::new();
    let mut at = SIGNATURE_LEN;

    while at + CHUNK_HEADER_LEN <= file.len() {
        let length = file
            .get(at..at + size_of::<u32>())
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(short)? as usize;
        let kind = &file[at + size_of::<u32>()..at + CHUNK_HEADER_LEN];
        let start = at + CHUNK_HEADER_LEN;
        let body = file.get(start..start + length).ok_or_else(short)?;
        if kind == chunk::IDAT.0 {
            stream.extend_from_slice(body);
        }
        at = start + length + CHUNK_CRC_LEN;
    }

    match stream.is_empty() {
        true => Err("A PNG frame compressed to nothing".to_string()),
        false => Ok(stream),
    }
}

/// Everything one frame needs that does not need the file.
fn prepare(
    previous: Option<&[u8]>,
    pixels: &[u8],
    frame: &Frame,
    depth: BitDepth,
) -> Result<Prepared, String> {
    // Only the rectangle that changed, which is what `fcTL` carries an
    // offset and a size for. A frame of an animation usually moves a
    // fraction of the page, and re-encoding the rest of it costs the file
    // its bytes and the decoder its time.
    let whole = (0, 0, frame.width, frame.height);
    let region = match previous {
        None => whole,
        Some(previous) => changed_region(
            previous,
            pixels,
            frame.width,
            frame.height,
            bytes_per_pixel(depth),
            FRAME_ORIGIN_GRAIN,
        )
        .unwrap_or(SMALLEST_FRAME),
    };

    let cropped;
    let payload = match region == whole {
        true => pixels,
        false => {
            cropped =
                crop_bytes(pixels, frame.width, bytes_per_pixel(depth), region);
            &cropped
        }
    };

    let (_, _, width, height) = region;
    Ok(Prepared {
        region,
        stream: compressed(payload, width, height, depth)?,
    })
}

impl ApngSink<'_> {
    /// One page as a still PNG, which is what a one-frame canvas asks for.
    fn write_still(&mut self, frame: &Frame) -> Result<(), String> {
        let bytes = wide_bytes(frame, self.depth);
        self.writer
            .write_image_data(&bytes)
            .map_err(|e| format!("Could not write a PNG frame: {e}"))
    }

    /// Writes one prepared frame: its control chunk, then its pixels.
    ///
    /// `first` says this frame is also the file's default image, which is
    /// carried by `IDAT` rather than `fdAT`. Every later frame is an `fdAT`,
    /// whose payload opens with a sequence number of its own.
    fn write_prepared(
        &mut self,
        frame: &Frame,
        prepared: Prepared,
        first: bool,
    ) -> Result<(), String> {
        let (x, y, width, height) = prepared.region;
        let (numerator, denominator) = delay_fraction(frame.delay_ms);

        let mut control = Vec::with_capacity(FRAME_CONTROL_LEN);
        control.extend_from_slice(&self.take_sequence().to_be_bytes());
        control.extend_from_slice(&width.to_be_bytes());
        control.extend_from_slice(&height.to_be_bytes());
        control.extend_from_slice(&x.to_be_bytes());
        control.extend_from_slice(&y.to_be_bytes());
        control.extend_from_slice(&numerator.to_be_bytes());
        control.extend_from_slice(&denominator.to_be_bytes());
        // Dispose nothing, blend nothing -- the same pair of answers
        // `webp.rs` gives, for the same reasons. The canvas has to survive
        // from one frame to the next, because everything outside a frame's
        // rectangle is still the last frame's; and the pixels inside it
        // replace what is under them rather than compositing over it, since
        // a translucent one is meant to *be* translucent rather than to be
        // blended onto the pixel it replaces.
        control.push(DisposeOp::None as u8);
        control.push(BlendOp::Source as u8);
        self.writer
            .write_chunk(chunk::fcTL, &control)
            .map_err(|e| format!("Could not place a frame: {e}"))?;

        let Self {
            writer, sequence, ..
        } = self;
        let wrote = |e| format!("Could not write a PNG frame: {e}");

        match first {
            true => {
                prepared
                    .stream
                    .chunks(MAX_CHUNK_PAYLOAD)
                    .try_for_each(|part| {
                        writer.write_chunk(chunk::IDAT, part).map_err(wrote)
                    })
            }
            false => {
                prepared
                    .stream
                    .chunks(MAX_FDAT_STREAM)
                    .try_for_each(|part| {
                        let mut payload =
                            Vec::with_capacity(size_of::<u32>() + part.len());
                        payload.extend_from_slice(&sequence.to_be_bytes());
                        *sequence += 1;
                        payload.extend_from_slice(part);
                        writer.write_chunk(chunk::fdAT, &payload).map_err(wrote)
                    })
            }
        }
    }

    /// The next sequence number, and moves past it.
    fn take_sequence(&mut self) -> u32 {
        let now = self.sequence;
        self.sequence += 1;
        now
    }
}

impl FrameSink for ApngSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        self.write_batch(slice::from_ref(frame))
    }

    /// Compresses the batch on every core and writes it on this one.
    ///
    /// A frame's rectangle comes from comparing it with the *pixels* of the
    /// frame before it, which the caller rasterized before this was called,
    /// so nothing here waits on a frame being deflated. What is left in
    /// order is the container: every chunk carries a sequence number, and
    /// those are handed out as the chunks go out.
    fn write_batch(&mut self, frames: &[Frame]) -> Result<(), String> {
        if !self.animated {
            return frames.iter().try_for_each(|frame| self.write_still(frame));
        }

        let depth = self.depth;
        let mut pixels = frames
            .par_iter()
            .map(|frame| wide_bytes(frame, depth))
            .collect::<Vec<_>>();

        // The frame before the batch, for the first frame in it. `None` on
        // the animation's opening frame, and only there -- which is also
        // what makes that frame the whole canvas and the default image.
        let carried = self.previous.as_deref();
        let opening = self.previous.is_none();

        let prepared = pixels
            .par_iter()
            .enumerate()
            .zip(frames)
            .map(|((nth, current), frame)| {
                let previous = match nth {
                    0 => carried,
                    _ => Some(&*pixels[nth - 1]),
                };
                prepare(previous, current, frame, depth)
            })
            .collect::<Result<Vec<_>, String>>()?;

        prepared.into_iter().zip(frames).enumerate().try_for_each(
            |(nth, (prepared, frame))| {
                self.write_prepared(frame, prepared, opening && nth == 0)
            },
        )?;

        // The batch boundary is not something the format sees, so what the
        // next batch compares against is the last frame of this one.
        self.previous = pixels.pop();
        Ok(())
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
            lossless: false,
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

    /// Every chunk of a PNG, in order: its four-byte type and its payload.
    fn walk(file: &[u8]) -> Vec<(&[u8], &[u8])> {
        let mut found = Vec::new();
        let mut at = SIGNATURE_LEN;
        while at + CHUNK_HEADER_LEN <= file.len() {
            let length = u32::from_be_bytes(
                file[at..at + size_of::<u32>()].try_into().unwrap(),
            ) as usize;
            let start = at + CHUNK_HEADER_LEN;
            found.push((
                &file[at + size_of::<u32>()..start],
                &file[start..start + length],
            ));
            at = start + length + CHUNK_CRC_LEN;
        }
        found
    }

    #[test]
    fn the_animation_chunks_are_numbered_consecutively_from_zero() {
        // This is the invariant that moved. The `png` crate used to number
        // `fcTL` and `fdAT` for us; the frames are compressed in parallel
        // now, which meant writing those chunks here, which meant owning the
        // counter. A decoder rejects a gap outright, so getting it wrong
        // fails loudly -- but only for a reader strict enough to check, and
        // the failure would be "corrupt animation" rather than anything
        // naming the cause.
        //
        // Six frames rather than two: the numbers only run out of step after
        // enough of them that an off-by-one has somewhere to hide.
        let file = encoded(&frames(6, 100), None);
        let chunks = walk(&file);

        let numbered = chunks
            .iter()
            .filter(|(kind, _)| {
                *kind == chunk::fcTL.0 || *kind == chunk::fdAT.0
            })
            .map(|(_, body)| {
                u32::from_be_bytes(body[..size_of::<u32>()].try_into().unwrap())
            })
            .collect::<Vec<_>>();

        // Six control chunks and five frames of data -- the first frame's
        // pixels are the default image and travel in `IDAT`, which carries
        // no number at all.
        assert_eq!(numbered, (0..11).collect::<Vec<_>>(), "{numbered:?}");

        // And that first frame really is an `IDAT`, before any `fdAT`.
        let order = chunks
            .iter()
            .filter_map(|(kind, _)| match *kind {
                k if k == chunk::IDAT.0 => Some("IDAT"),
                k if k == chunk::fdAT.0 => Some("fdAT"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(order.first(), Some(&"IDAT"), "{order:?}");
        assert_eq!(
            order.iter().filter(|kind| **kind == "IDAT").count(),
            1,
            "the default image is one frame's worth of data: {order:?}"
        );
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
