//! APNG, demuxed and composited here because Skia reads none of it.
//!
//! # What the format asks of a reader
//!
//! An APNG is a PNG with three extra chunks. `acTL` counts the frames and
//! the plays; each frame is introduced by an `fcTL` giving its rectangle,
//! its duration and two operations; and every frame after the first carries
//! its pixels in `fdAT` rather than `IDAT`. The `IDAT` is frame zero when an
//! `fcTL` precedes it, and a still poster that is not part of the animation
//! when none does.
//!
//! Frames are sub-rectangles, so a reader has to composite. Two fields say
//! how:
//!
//! - `blend_op` -- how this frame's pixels meet what is under them. `Source`
//!   replaces the rectangle outright, alpha included; `Over` composites it.
//! - `dispose_op` -- what happens to the rectangle *after* the frame has been
//!   shown, before the next one arrives. `None` leaves it, `Background` clears
//!   it to transparent, and `Previous` puts back whatever was there before this
//!   frame drew.
//!
//! Getting either wrong produces a picture rather than an error, which is
//! why the tests here assert pixels and not just frame counts.
//!
//! # Why composite in sixteen bits
//!
//! The encoder writes sixteen-bit APNGs from a float canvas, so a reader
//! that composited in eight would lose on the way back in what the writer
//! was careful to keep. Every frame is widened to sixteen bits, composited
//! there, and narrowed again only if the file itself was eight -- the two
//! conversions being the exact ones [`Frame`](crate::encode::Frame) uses, so
//! a round trip through both is the identity.

use std::io::{BufRead, Cursor, Seek};

use png::{BlendOp, ColorType, Decoder, DisposeOp, Reader, Transformations};
use skia_safe::{
    AlphaType, ColorType as SkColorType, Data, Image as SkImage, ImageInfo,
    images,
};

use crate::{
    encode::{narrow_to_eight, widen_to_sixteen},
    pixels::PixelColorSpace,
};

/// The signature every PNG begins with.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A chunk's four-byte length and its four-byte type.
const CHUNK_HEADER: usize = 8;

/// Those eight bytes plus the four-byte checksum that follows the payload,
/// which is what separates one chunk's start from the next.
const CHUNK_OVERHEAD: usize = CHUNK_HEADER + 4;

/// The width of a chunk's length and of its type.
const WORD: usize = 4;

/// What a frame's delay is measured against when its denominator is zero.
///
/// The specification says a zero denominator is to be read as 100, which
/// makes the numerator hundredths of a second -- the same unit GIF uses, and
/// no accident.
const DEFAULT_DELAY_DENOMINATOR: u16 = 100;

/// Milliseconds in a second, for turning the delay fraction into one.
const MS_PER_SECOND: u32 = 1000;

/// Channels in the composited buffer: red, green, blue, alpha.
const CHANNELS: usize = 4;

/// Whether `bytes` is a PNG carrying an animation.
///
/// Cheap and structural: the signature, then an `acTL` somewhere before the
/// first `IDAT`. A file with the chunk after `IDAT` is not an animation --
/// the specification requires it first -- and is left to Skia as the still
/// image it is.
pub(crate) fn is_animated(bytes: &[u8]) -> bool {
    if !bytes.starts_with(PNG_MAGIC) {
        return false;
    }
    let mut at = PNG_MAGIC.len();
    while at + CHUNK_HEADER <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[at],
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
        ]) as usize;
        match &bytes[at + WORD..at + CHUNK_HEADER] {
            b"acTL" => return true,
            b"IDAT" => return false,
            _ => {}
        }
        let Some(next) = at
            .checked_add(CHUNK_OVERHEAD)
            .and_then(|n| n.checked_add(len))
        else {
            return false;
        };
        at = next;
    }
    false
}

/// One frame's rectangle, timing and compositing rules, with its pixels
/// already widened to sixteen bits of RGBA.
struct Subframe {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    dispose: DisposeOp,
    blend: BlendOp,
    pixels: Vec<u16>,
}

/// A decoded animation: the canvas size, the frames, and what the file said
/// about its colour.
///
/// Test-only now. [`frame`] reads incrementally through [`Playback`] and
/// [`delays`] walks the `fcTL` chunks, so nothing in a release build wants
/// every frame gathered at once -- which was the point of both changes. It
/// survives because the tests below check whole animations against what
/// this crate wrote, which is a different question from how a caller reads
/// one frame.
#[cfg(test)]
struct Animation {
    width: usize,
    height: usize,
    deep: bool,
    space: PixelColorSpace,
    frames: Vec<Subframe>,
}

/// Borrowed input the `png` reader can own.
///
/// `png::Reader` reads sequentially and cannot seek, so resuming a walk
/// means holding the reader between calls -- which means it must own what it
/// reads from. `Data` is refcounted, so this hands the reader a handle
/// rather than a copy of the file.
struct Shared(Data);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// A reader part-way through an animation, and the canvas it has built.
///
/// The sibling of [`avif::Playback`](super::avif::Playback), and here for
/// the same reason: a sub-rectangle frame means nothing without the frames
/// it was drawn over, so reaching frame `n` inflates every frame up to it.
/// Doing that from zero on every call makes playing an animation quadratic
/// -- the documented loop asks for one frame per output frame.
pub(crate) struct Playback {
    reader: Reader<Cursor<Shared>>,
    /// The canvas as the next frame will find it: every frame before `next`
    /// blended *and disposed of*. The frame handed back to a caller is the
    /// canvas before its own disposal, which is why the two differ.
    canvas: Vec<u16>,
    next: usize,
    /// The row buffer the `png` reader inflates into.
    ///
    /// Here rather than in `frame` because it was allocated and zeroed on
    /// every call -- about 16 MB at 1080p and sixteen bits, per output
    /// frame, whether the frame being read covered the canvas or four
    /// pixels of it. It is scratch, and scratch belongs with the reader that
    /// uses it.
    buffer: Vec<u8>,
    width: usize,
    height: usize,
    deep: bool,
    space: PixelColorSpace,
}

/// Starts a reader over `data`, with the transformations the rest of this
/// module expects.
fn open(data: &Data) -> Result<Reader<Cursor<Shared>>, String> {
    let mut decoder = Decoder::new(Cursor::new(Shared(data.clone())));
    decoder
        .set_transformations(Transformations::EXPAND | Transformations::ALPHA);
    decoder
        .read_info()
        .map_err(|e| format!("Could not read the APNG: {e}"))
}

/// Whether `IDAT` is a still poster rather than the animation's first frame.
///
/// The `fcTL` for a frame is read as part of reaching it, so a reader that has
/// read as far as the image data and still has no frame control passed an
/// `IDAT` no `fcTL` introduced. The APNG specification calls that a default
/// image: it is what a decoder that cannot animate shows, and it is not one of
/// the frames `acTL` counts. `png` agrees, and adds one to the frames it will
/// hand back rather than to `num_frames`.
fn separate_default_image<R: BufRead + Seek>(reader: &Reader<R>) -> bool {
    reader.info().animation_control.is_some()
        && reader.info().frame_control.is_none()
}

/// Reads past the default image, leaving the reader at the animation's frame 0.
///
/// It has to be inflated to be got past -- `png` reads forward only, and there
/// is no seek -- so this costs one canvas-sized decode when a file has a
/// poster, and nothing at all when it does not. What it buys is that every
/// index a caller can ask for names the frame [`delays`] timed: the reader's
/// own numbering counts the poster, so leaving it in shifted the whole
/// animation along by one and put its last frame past the count.
///
/// Called from two places, and only one of them is the caller's path:
/// [`start`], which every decoded frame goes through, and `read`, which is
/// test-only. A reviewer checking that the test still fails without this
/// disabled the `read` call and watched the test pass -- so if you are
/// proving the guard is load-bearing, disable the one in [`start`].
fn skip_default_image<R: BufRead + Seek>(
    reader: &mut Reader<R>,
    buffer: &mut [u8],
) -> Result<(), String> {
    reader.next_frame(buffer).map_err(|e| {
        format!("Could not read past the APNG's default image: {e}")
    })?;
    Ok(())
}

/// Reads the next frame from a reader already positioned at it.
fn next_subframe(
    reader: &mut Reader<Cursor<Shared>>,
    buffer: &mut [u8],
    deep: bool,
    index: usize,
) -> Result<Subframe, String> {
    let output = reader
        .next_frame(buffer)
        .map_err(|e| format!("Could not read APNG frame {index}: {e}"))?;
    // `fcTL` is read as part of the frame that follows it, so this is this
    // frame's control and not the last one's. A file whose first frame is
    // the still `IDAT` has none, and that frame covers the whole canvas with
    // nothing under it.
    let control = reader.info().frame_control;
    let (color, _) = reader.output_color_type();
    Ok(Subframe {
        x: control.map_or(0, |c| c.x_offset as usize),
        y: control.map_or(0, |c| c.y_offset as usize),
        width: output.width as usize,
        height: output.height as usize,
        dispose: control.map_or(DisposeOp::None, |c| c.dispose_op),
        blend: control.map_or(BlendOp::Source, |c| c.blend_op),
        pixels: widen(
            &buffer[..output.buffer_size()],
            color,
            deep,
            output.width as usize * output.height as usize,
        ),
    })
}

/// Reads every frame up to and including `wanted`, and no further.
///
/// Test-only, as [`Animation`] is.
///
/// Stopping early is the whole reason this takes an index: drawing frame 3
/// of a 300-frame animation should not decode the other 297. It cannot stop
/// *before* `wanted`, because a sub-rectangle frame means nothing without
/// the frames it was drawn over.
#[cfg(test)]
fn read(bytes: &[u8], wanted: usize) -> Result<Animation, String> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    // Palette to colour, low bit depths to eight, `tRNS` to a real alpha
    // channel -- so what comes out is grey or colour, with alpha, at eight
    // or sixteen bits, and nothing narrower to unpack by hand.
    decoder
        .set_transformations(Transformations::EXPAND | Transformations::ALPHA);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("Could not read the APNG: {e}"))?;

    let (width, height) = reader.info().size();
    let deep = reader.output_color_type().1 as u8 == 16;
    let space = space_of(reader.info());
    let count = reader
        .info()
        .animation_control
        .map(|control| control.num_frames as usize)
        .unwrap_or(1);

    let mut frames = Vec::new();
    let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    // The same shift [`start`] steps over, for the same reason: `count` is
    // what `acTL` declares, and the reader offers a poster ahead of it.
    if separate_default_image(&reader) {
        skip_default_image(&mut reader, &mut buffer)?;
    }
    for index in 0..count.min(wanted.saturating_add(1)) {
        let output = reader
            .next_frame(&mut buffer)
            .map_err(|e| format!("Could not read APNG frame {index}: {e}"))?;
        // `fcTL` is read as part of the frame that follows it, so this is
        // this frame's control and not the last one's. A file whose first
        // frame is the still `IDAT` has none, and that frame covers the
        // whole canvas with nothing under it.
        let control = reader.info().frame_control;
        let (color, _) = reader.output_color_type();
        frames.push(Subframe {
            x: control.map_or(0, |c| c.x_offset as usize),
            y: control.map_or(0, |c| c.y_offset as usize),
            width: output.width as usize,
            height: output.height as usize,
            dispose: control.map_or(DisposeOp::None, |c| c.dispose_op),
            blend: control.map_or(BlendOp::Source, |c| c.blend_op),
            pixels: widen(
                &buffer[..output.buffer_size()],
                color,
                deep,
                output.width as usize * output.height as usize,
            ),
        });
    }

    Ok(Animation {
        width: width as usize,
        height: height as usize,
        deep,
        space,
        frames,
    })
}

/// A frame's duration in milliseconds, from the fraction `fcTL` states it as.
///
/// Rounded to nearest rather than truncated, for the same reason the GIF
/// encoder rounds: 1/30 of a second is 33.33ms, and truncating every frame
/// of a 30fps animation runs it fast.
fn delay_ms(numerator: u16, denominator: u16) -> u32 {
    let denominator = match denominator {
        0 => DEFAULT_DELAY_DENOMINATOR,
        other => other,
    };
    let scaled = u32::from(numerator) * MS_PER_SECOND;
    let half = u32::from(denominator) / 2;
    (scaled + half) / u32::from(denominator)
}

/// The colour space the file names, or sRGB where it names none.
///
/// `cICP` is what this crate's own encoder writes for anything but sRGB, and
/// it is exact: two code points that name a set of primaries and a transfer
/// function. Looked up against the spaces this crate knows rather than
/// interpreted, so a file describing one it does not have falls back to sRGB
/// rather than to a guess.
fn space_of(info: &png::Info<'_>) -> PixelColorSpace {
    let Some(cicp) = info.coding_independent_code_points else {
        return PixelColorSpace::Srgb;
    };
    PixelColorSpace::all()
        .find(|space| {
            let traits = space.traits();
            traits.primaries as u8 == cicp.color_primaries
                && traits.transfer as u8 == cicp.transfer_function
        })
        .unwrap_or(PixelColorSpace::Srgb)
}

/// One subframe's bytes as sixteen-bit RGBA, whatever they arrived as.
///
/// Four shapes reach here -- grey or colour, each at eight or sixteen bits
/// -- and they become one, so the compositor below is written once.
fn widen(
    bytes: &[u8],
    color: ColorType,
    deep: bool,
    pixels: usize,
) -> Vec<u16> {
    let mut out = Vec::with_capacity(pixels * CHANNELS);
    // Sixteen-bit PNG samples are big-endian, which is the format's own
    // byte order and not the machine's.
    let sample = |at: usize| -> u16 {
        match deep {
            true => u16::from_be_bytes([bytes[at * 2], bytes[at * 2 + 1]]),
            false => widen_to_sixteen(bytes[at]),
        }
    };
    let per_pixel = match color {
        ColorType::GrayscaleAlpha => 2,
        _ => 4,
    };
    for pixel in 0..pixels {
        let base = pixel * per_pixel;
        match color {
            ColorType::GrayscaleAlpha => {
                let grey = sample(base);
                out.extend_from_slice(&[grey, grey, grey, sample(base + 1)]);
            }
            _ => out.extend_from_slice(&[
                sample(base),
                sample(base + 1),
                sample(base + 2),
                sample(base + 3),
            ]),
        }
    }
    out
}

/// Frame `index`, composited over the frames before it.
///
/// The canvas starts fully transparent, as the specification says, and each
/// frame is blended into its own rectangle and then disposed of -- except
/// the last, whose disposal belongs to the frame after it and so never
/// happens here.
///
/// An `index` past the end is an error, and comes back from the reader as
/// the frame it could not read. It used to be clamped to what `acTL`
/// declared, which read one count where [`delays`] read another: on a file
/// where the two disagreed, the same index answered with the `acTL`-final
/// frame from a fresh reader and with an error from a held one.
pub(crate) fn frame(
    data: &Data,
    index: usize,
    resume: Option<&mut Option<super::Playback>>,
) -> Result<SkImage, String> {
    // A held reader is only usable for a frame at or after the one it
    // stopped before. Anything else -- a seek backwards, a replay from the
    // start, a slot holding the other format -- opens the file again, which
    // is what this did on every call.
    let mut slot = resume;
    let mut state = match slot.as_mut().and_then(|slot| slot.take()) {
        Some(super::Playback::Apng(held)) if held.next <= index => *held,
        _ => start(data)?,
    };

    let (width, height) = (state.width, state.height);

    let mut image = None;
    for at in state.next..=index {
        let frame = next_subframe(
            &mut state.reader,
            &mut state.buffer,
            state.deep,
            at,
        )?;

        // Saved before the frame draws, because that is what `Previous`
        // restores: the canvas as it was, not as the frame leaves it. Only
        // the rectangle is kept -- nothing outside it can change.
        let saved = match frame.dispose {
            DisposeOp::Previous => Some(region(&state.canvas, width, &frame)),
            _ => None,
        };
        blend(&mut state.canvas, width, &frame);

        // Disposal happens after the frame has been shown, so the frame
        // being asked for keeps its own pixels -- which is why the picture
        // is taken here and the canvas carried on is the disposed one.
        if at == index {
            image = Some(raster(
                &state.canvas,
                width,
                height,
                state.deep,
                state.space,
            )?);
        }
        match frame.dispose {
            DisposeOp::None => {}
            DisposeOp::Background => clear(&mut state.canvas, width, &frame),
            DisposeOp::Previous => {
                if let Some(saved) = saved {
                    restore(&mut state.canvas, width, &frame, &saved);
                }
            }
        }
        state.next = at + 1;
    }

    // Defensive, and cannot fire: the reuse guard admits only a held reader
    // whose `next` is at or before `index`, a fresh one starts at zero, and
    // either way the loop ran with `at == index` on its last turn. A file
    // with fewer frames than `index` fails inside `next_subframe` above,
    // which names the frame it could not read.
    let image =
        image.ok_or_else(|| format!("The APNG has no frame {index}"))?;
    // Handed back to the caller's slot so the next frame resumes rather than
    // opening the file again.
    if let Some(slot) = slot {
        *slot = Some(super::Playback::Apng(Box::new(state)));
    }
    Ok(image)
}

/// Opens `data` and reads what the header says, with an empty canvas.
fn start(data: &Data) -> Result<Playback, String> {
    let mut reader = open(data)?;
    let (width, height) = reader.info().size();
    let deep = reader.output_color_type().1 as u8 == 16;
    let space = space_of(reader.info());
    let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    if separate_default_image(&reader) {
        skip_default_image(&mut reader, &mut buffer)?;
    }
    Ok(Playback {
        reader,
        canvas: vec![0u16; width as usize * height as usize * CHANNELS],
        next: 0,
        buffer,
        width: width as usize,
        height: height as usize,
        deep,
        space,
    })
}

/// The frame timings, one per frame, in milliseconds.
///
/// `None` where the bytes are not an animation this can read, which leaves
/// the caller's existing answer alone.
pub(crate) fn delays(bytes: &[u8]) -> Option<Vec<u32>> {
    if !is_animated(bytes) {
        return None;
    }
    // Read from the `fcTL` chunks rather than from decoded frames. This
    // inflated the whole animation to reach the timings -- `read` with
    // `usize::MAX` -- and every frame's pixels were alive at once to produce
    // one integer each: a hundred frames of 1080p is about 1.6 GB held to
    // answer with a hundred numbers.
    //
    // `frame_delays` runs on every image this crate constructs, so that was
    // the cost of opening an APNG at all, not of playing one. The sibling in
    // `avif::delays` was always a container walk and no pixels; this is the
    // same shape.
    let found: Vec<u32> = frame_controls(bytes)
        .map(|control| {
            delay_ms(
                u16::from_be_bytes([
                    control[DELAY_NUM_AT],
                    control[DELAY_NUM_AT + 1],
                ]),
                u16::from_be_bytes([
                    control[DELAY_DEN_AT],
                    control[DELAY_DEN_AT + 1],
                ]),
            )
        })
        // The animation is no longer than `acTL` says it is. The two counts
        // are required to agree, nothing enforces it, and they are read by
        // different halves of this module -- so a file where they differ had
        // this reporting one length while the reader below produced another.
        // Truncating here is what makes the count offered to a caller a
        // count that can be decoded: the `png` reader stops the animation at
        // `num_frames` whatever follows it, so an `fcTL` past that describes
        // a frame nothing will hand back.
        .take(declared_frames(bytes).unwrap_or(usize::MAX))
        .collect();
    match found.len() {
        0 | 1 => None,
        _ => Some(found),
    }
}

/// The number of frames `acTL` declares, where the chunk is there and whole.
///
/// The first field of the payload, and the file's own statement of its
/// length: the APNG specification requires it to equal the number of `fcTL`
/// chunks, so on a valid file this repeats what walking them says. It is
/// read because an invalid file exists and `png` 0.18 admits one -- it
/// checks subframe bounds and `fdAT` sequence numbers, and never that these
/// two counts match.
///
/// `None` where there is no readable `acTL`, which leaves the `fcTL` walk as
/// the only answer there is.
fn declared_frames(bytes: &[u8]) -> Option<usize> {
    let mut at = PNG_MAGIC.len();
    while at + CHUNK_HEADER <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[at],
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
        ]) as usize;
        let payload = at + CHUNK_HEADER;
        let tag = &bytes[at + WORD..payload];
        // As in `frame_controls`: cut to what is there rather than to what
        // the chunk claims, so a truncated or overlong `acTL` reads as absent
        // rather than indexing past the end.
        if tag == b"acTL" && payload + WORD <= bytes.len() {
            return Some(u32::from_be_bytes([
                bytes[payload],
                bytes[payload + 1],
                bytes[payload + 2],
                bytes[payload + 3],
            ]) as usize);
        }
        // An `acTL` after the first `IDAT` is not an animation's, and
        // `is_animated` has already said so.
        if tag == b"IDAT" {
            return None;
        }
        let next = at
            .checked_add(CHUNK_OVERHEAD)
            .and_then(|n| n.checked_add(len))?;
        at = next;
    }
    None
}

/// Where `fcTL` states the numerator of its delay, after the sequence
/// number, the rectangle and its offsets (APNG specification, `fcTL`).
const DELAY_NUM_AT: usize = 20;

/// And the denominator, two bytes after it.
const DELAY_DEN_AT: usize = DELAY_NUM_AT + 2;

/// The payload of every `fcTL` chunk, in the order they appear.
///
/// One per frame, including the first where the still `IDAT` is part of the
/// animation, which is what makes counting them a frame count.
fn frame_controls(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = PNG_MAGIC.len();
    std::iter::from_fn(move || {
        while at + CHUNK_HEADER <= bytes.len() {
            let len = u32::from_be_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]) as usize;
            let tag = &bytes[at + WORD..at + CHUNK_HEADER];
            let payload = at + CHUNK_HEADER;
            // Length, type, payload, checksum.
            let next = at
                .checked_add(CHUNK_OVERHEAD)
                .and_then(|n| n.checked_add(len))?;
            at = next;
            // Cut to what was checked, not to what the chunk claims. The
            // guard covers the twenty-four bytes the caller reads and the
            // slice used the file's own length, so a `fcTL` declaring more
            // than the file holds -- truncation, a flipped byte, `0xFFFFFFFF`
            // over the length -- indexed past the end and panicked. Reached
            // from `delays`, which runs on every image this crate opens.
            let end = payload + DELAY_DEN_AT + 2;
            if tag == b"fcTL" && end <= bytes.len() {
                return Some(&bytes[payload..end]);
            }
        }
        None
    })
}

/// The canvas pixels under `frame`'s rectangle.
fn region(canvas: &[u16], width: usize, frame: &Subframe) -> Vec<u16> {
    let mut saved = Vec::with_capacity(frame.width * frame.height * CHANNELS);
    for row in 0..frame.height {
        let start = ((frame.y + row) * width + frame.x) * CHANNELS;
        saved.extend_from_slice(&canvas[start..start + frame.width * CHANNELS]);
    }
    saved
}

/// Puts back what [`region`] took.
fn restore(canvas: &mut [u16], width: usize, frame: &Subframe, saved: &[u16]) {
    for row in 0..frame.height {
        let start = ((frame.y + row) * width + frame.x) * CHANNELS;
        let from = row * frame.width * CHANNELS;
        canvas[start..start + frame.width * CHANNELS]
            .copy_from_slice(&saved[from..from + frame.width * CHANNELS]);
    }
}

/// Clears `frame`'s rectangle to fully transparent.
fn clear(canvas: &mut [u16], width: usize, frame: &Subframe) {
    for row in 0..frame.height {
        let start = ((frame.y + row) * width + frame.x) * CHANNELS;
        canvas[start..start + frame.width * CHANNELS].fill(0);
    }
}

/// Draws `frame` into the canvas under whichever rule its `blend_op` names.
fn blend(canvas: &mut [u16], width: usize, frame: &Subframe) {
    for row in 0..frame.height {
        let start = ((frame.y + row) * width + frame.x) * CHANNELS;
        let from = row * frame.width * CHANNELS;
        for column in 0..frame.width {
            let to = start + column * CHANNELS;
            let source = &frame.pixels[from + column * CHANNELS..][..CHANNELS];
            match frame.blend {
                BlendOp::Source => {
                    canvas[to..to + CHANNELS].copy_from_slice(source);
                }
                BlendOp::Over => {
                    let over = composite(source, &canvas[to..to + CHANNELS]);
                    canvas[to..to + CHANNELS].copy_from_slice(&over);
                }
            }
        }
    }
}

/// `source` over `under`, both unpremultiplied, as the APNG specification
/// spells the arithmetic.
///
/// Done in `u64`, and that is not caution. Three sixteen-bit values multiply
/// together here, and 65535 cubed is eighteen thousand times what a `u32`
/// holds -- the first draft used one and a test of half-transparent blue
/// over red panicked on the overflow. Two of them alone fit, at 4294836225
/// against a ceiling of 4294967295, which is exactly the margin that makes
/// the mistake survive a casual reading.
fn composite(source: &[u16], under: &[u16]) -> [u16; CHANNELS] {
    let max = u64::from(u16::MAX);
    let (sa, ua) = (u64::from(source[3]), u64::from(under[3]));
    // The straight-alpha "over": what is underneath contributes in
    // proportion to the light the source lets through, the result covers
    // whatever the two cover between them, and each colour is then weighted
    // by its share of that result.
    let under_share = ua * (max - sa) / max;
    let alpha = sa + under_share;
    if alpha == 0 {
        return [0; CHANNELS];
    }
    let mut out = [0u16; CHANNELS];
    for channel in 0..3 {
        let s = u64::from(source[channel]) * sa;
        let u = u64::from(under[channel]) * under_share;
        out[channel] = ((s + u) / alpha) as u16;
    }
    out[3] = alpha as u16;
    out
}

/// The composited canvas as a Skia image, at the depth the file held.
fn raster(
    canvas: &[u16],
    width: usize,
    height: usize,
    deep: bool,
    space: PixelColorSpace,
) -> Result<SkImage, String> {
    let color_space = space.to_skia_color_space().ok();
    let (color_type, bytes) = match deep {
        true => (
            SkColorType::R16G16B16A16UNorm,
            canvas
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect::<Vec<_>>(),
        ),
        // Narrowed with the same rounding the encoder uses, so eight bits
        // in and eight bits out is the byte it started as.
        false => (
            SkColorType::RGBA8888,
            canvas
                .iter()
                .map(|v| narrow_to_eight(*v))
                .collect::<Vec<_>>(),
        ),
    };
    let info = ImageInfo::new(
        (width as i32, height as i32),
        color_type,
        AlphaType::Unpremul,
        color_space,
    );
    images::raster_from_data(
        &info,
        Data::new_copy(&bytes),
        info.min_row_bytes(),
    )
    .ok_or_else(|| "Could not build an image from the APNG".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::ChromaSampling;
    use png::{BitDepth, Encoder};

    /// Solid RGBA of `w` x `h`.
    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter().copied().cycle().take(w * h * 4).collect()
    }

    /// One frame's description for [`animation`].
    struct Written {
        w: u32,
        h: u32,
        x: u32,
        y: u32,
        dispose: DisposeOp,
        blend: BlendOp,
        pixels: Vec<u8>,
    }

    /// An APNG built to order, so the compositing rules this crate's own
    /// encoder never writes -- offsets, `Over`, `Background`, `Previous` --
    /// are exercised by something other than themselves.
    fn animation(canvas: (u32, u32), frames: Vec<Written>) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, canvas.0, canvas.1);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_animated(frames.len() as u32, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for frame in &frames {
                writer.set_frame_dimension(frame.w, frame.h).unwrap();
                writer.set_frame_position(frame.x, frame.y).unwrap();
                writer.set_dispose_op(frame.dispose).unwrap();
                writer.set_blend_op(frame.blend).unwrap();
                writer.set_frame_delay(1, 10).unwrap();
                writer.write_image_data(&frame.pixels).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    /// An APNG whose `IDAT` is a still poster rather than the animation's
    /// first frame: no `fcTL` precedes it, so `acTL` counts only the frames
    /// after it and the default image belongs to none of them.
    ///
    /// This crate's encoder never writes that shape -- it makes the first
    /// frame the default image -- so nothing else in this suite produces one.
    fn poster_animation(
        canvas: (u32, u32),
        poster: Vec<u8>,
        frames: Vec<Written>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, canvas.0, canvas.1);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_animated(frames.len() as u32, 0).unwrap();
            encoder.set_sep_def_img(true).unwrap();
            let mut writer = encoder.write_header().unwrap();
            // Written first and with no frame control of its own, which is
            // what makes it separate: it covers the canvas and is not counted.
            writer.write_image_data(&poster).unwrap();
            for frame in &frames {
                writer.set_frame_dimension(frame.w, frame.h).unwrap();
                writer.set_frame_position(frame.x, frame.y).unwrap();
                writer.set_dispose_op(frame.dispose).unwrap();
                writer.set_blend_op(frame.blend).unwrap();
                writer.set_frame_delay(1, 10).unwrap();
                writer.write_image_data(&frame.pixels).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    /// Rewrites the frame count `acTL` states, leaving in place the `fcTL`
    /// chunks it then contradicts, and fixes the chunk's checksum.
    ///
    /// The `png` encoder will not write such a file -- it counts what it
    /// wrote -- but a file on disk can hold any pair of numbers, and the two
    /// counts are read by different parts of this crate.
    fn declare_frames(bytes: &[u8], count: u32) -> Vec<u8> {
        let mut out = bytes.to_vec();
        let mut at = PNG_MAGIC.len();
        while at + CHUNK_HEADER <= out.len() {
            let len = u32::from_be_bytes([
                out[at],
                out[at + 1],
                out[at + 2],
                out[at + 3],
            ]) as usize;
            let payload = at + CHUNK_HEADER;
            if &out[at + WORD..payload] == b"acTL" {
                // `num_frames` is the first field of the payload; the
                // checksum covers the type and the payload together.
                out[payload..payload + WORD]
                    .copy_from_slice(&count.to_be_bytes());
                let crc = crc32fast::hash(&out[at + WORD..payload + len]);
                out[payload + len..payload + len + WORD]
                    .copy_from_slice(&crc.to_be_bytes());
                return out;
            }
            at = payload + len + WORD;
        }
        panic!("the fixture has an acTL");
    }

    /// The composited top-left pixel of a decoded frame, as eight-bit RGBA.
    ///
    /// Which frame came back, rather than whether one did: the frames in the
    /// fixture below differ only in colour.
    fn top_left(image: &SkImage) -> [u8; 4] {
        let info = ImageInfo::new(
            (1, 1),
            SkColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut pixel = [0u8; 4];
        assert!(image.read_pixels(
            &info,
            &mut pixel,
            info.min_row_bytes(),
            (0, 0),
            skia_safe::image::CachingHint::Allow,
        ));
        pixel
    }

    /// The composited RGBA of one pixel of one frame, as eight-bit values.
    fn pixel(bytes: &[u8], index: usize, x: usize, y: usize) -> [u8; 4] {
        let animation = read(bytes, index).expect("the fixture decodes");
        let width = animation.width;
        let mut canvas = vec![0u16; width * animation.height * CHANNELS];
        for (at, frame) in animation.frames.iter().enumerate() {
            let saved = match frame.dispose {
                DisposeOp::Previous => Some(region(&canvas, width, frame)),
                _ => None,
            };
            blend(&mut canvas, width, frame);
            if at == index {
                break;
            }
            match frame.dispose {
                DisposeOp::None => {}
                DisposeOp::Background => clear(&mut canvas, width, frame),
                DisposeOp::Previous => {
                    if let Some(saved) = saved {
                        restore(&mut canvas, width, frame, &saved);
                    }
                }
            }
        }
        let at = (y * width + x) * CHANNELS;
        [
            narrow_to_eight(canvas[at]),
            narrow_to_eight(canvas[at + 1]),
            narrow_to_eight(canvas[at + 2]),
            narrow_to_eight(canvas[at + 3]),
        ]
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const HALF_BLUE: [u8; 4] = [0, 0, 255, 128];

    #[test]
    fn a_still_png_is_not_taken_for_an_animation() {
        // The guard `frame_delays` leans on: every PNG this crate writes goes
        // past it, and only the animated ones may be diverted from Skia.
        let mut still = Vec::new();
        {
            let mut encoder = Encoder::new(&mut still, 2, 2);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&solid(2, 2, RED)).unwrap();
            writer.finish().unwrap();
        }
        assert!(!is_animated(&still));
        assert_eq!(delays(&still), None);

        assert!(!is_animated(b"not a png at all"));
        assert!(!is_animated(&still[..4]), "a truncated file is not one");
    }

    #[test]
    fn a_sub_rectangle_lands_where_its_offset_says() {
        // The whole point of the format, and the thing a reader that ignored
        // `fcTL` would get wrong while still producing a picture.
        let bytes = animation(
            (4, 4),
            vec![
                Written {
                    w: 4,
                    h: 4,
                    x: 0,
                    y: 0,
                    dispose: DisposeOp::None,
                    blend: BlendOp::Source,
                    pixels: solid(4, 4, RED),
                },
                Written {
                    w: 2,
                    h: 2,
                    x: 2,
                    y: 2,
                    dispose: DisposeOp::None,
                    blend: BlendOp::Source,
                    pixels: solid(2, 2, BLUE),
                },
            ],
        );
        assert!(is_animated(&bytes));

        // Frame 0 is red everywhere.
        assert_eq!(pixel(&bytes, 0, 3, 3), RED);
        // Frame 1 is blue only inside the rectangle it named.
        assert_eq!(pixel(&bytes, 1, 3, 3), BLUE, "inside the rectangle");
        assert_eq!(pixel(&bytes, 1, 0, 0), RED, "outside it, frame 0 survives");
    }

    #[test]
    fn over_composites_and_source_replaces() {
        // The asymmetry that makes `blend_op` worth having: the same
        // half-transparent blue over the same red, twice, and the two
        // answers must differ.
        let of = |blend| {
            animation(
                (2, 2),
                vec![
                    Written {
                        w: 2,
                        h: 2,
                        x: 0,
                        y: 0,
                        dispose: DisposeOp::None,
                        blend: BlendOp::Source,
                        pixels: solid(2, 2, RED),
                    },
                    Written {
                        w: 2,
                        h: 2,
                        x: 0,
                        y: 0,
                        dispose: DisposeOp::None,
                        blend,
                        pixels: solid(2, 2, HALF_BLUE),
                    },
                ],
            )
        };

        // `Source` takes the frame's own alpha with it -- the red is gone,
        // and what is left is half-transparent blue.
        assert_eq!(pixel(&of(BlendOp::Source), 1, 0, 0), HALF_BLUE);

        // `Over` composites: opaque, and purple rather than blue.
        let [r, g, b, a] = pixel(&of(BlendOp::Over), 1, 0, 0);
        assert_eq!(a, 255, "opaque red under it");
        assert_eq!(g, 0);
        assert!(r > 100 && r < 140, "about half the red: {r}");
        assert!(b > 100 && b < 140, "about half the blue: {b}");
    }

    #[test]
    fn each_disposal_leaves_what_it_promises() {
        // Three files differing in one byte of one `fcTL`, so the third
        // frame's pixel is the only thing that can explain the difference.
        let of = |dispose| {
            animation(
                (2, 2),
                vec![
                    Written {
                        w: 2,
                        h: 2,
                        x: 0,
                        y: 0,
                        dispose: DisposeOp::None,
                        blend: BlendOp::Source,
                        pixels: solid(2, 2, RED),
                    },
                    Written {
                        w: 1,
                        h: 1,
                        x: 0,
                        y: 0,
                        dispose,
                        blend: BlendOp::Source,
                        pixels: solid(1, 1, BLUE),
                    },
                    Written {
                        w: 1,
                        h: 1,
                        x: 1,
                        y: 1,
                        dispose: DisposeOp::None,
                        blend: BlendOp::Over,
                        // A corner this test never reads, so frame 2 leaves
                        // the pixel under test showing frame 1's disposal.
                        pixels: solid(1, 1, [0, 0, 0, 0]),
                    },
                ],
            )
        };

        // Frame 1 painted (0,0) blue. What frame 2 sees there is the test.
        assert_eq!(
            pixel(&of(DisposeOp::None), 2, 0, 0),
            BLUE,
            "None leaves the frame's own pixels"
        );
        assert_eq!(
            pixel(&of(DisposeOp::Background), 2, 0, 0),
            [0, 0, 0, 0],
            "Background clears the rectangle to transparent"
        );
        assert_eq!(
            pixel(&of(DisposeOp::Previous), 2, 0, 0),
            RED,
            "Previous puts back what frame 0 had drawn"
        );

        // And disposal is *after* display: frame 1 asked for it, so frame 1
        // itself must still be blue in every one of the three.
        for dispose in
            [DisposeOp::None, DisposeOp::Background, DisposeOp::Previous]
        {
            assert_eq!(pixel(&of(dispose), 1, 0, 0), BLUE, "{dispose:?}");
        }
    }

    #[test]
    fn a_sixteen_bit_file_keeps_its_depth() {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Sixteen);
            encoder.set_animated(2, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for level in [0x0102u16, 0x0304] {
                let row: Vec<u8> = std::iter::repeat_n(level, 2 * 2 * CHANNELS)
                    .flat_map(|v| v.to_be_bytes())
                    .collect();
                writer.set_frame_delay(1, 10).unwrap();
                writer.write_image_data(&row).unwrap();
            }
            writer.finish().unwrap();
        }
        let animation = read(&bytes, 1).expect("decodes");
        assert!(animation.deep, "the file is sixteen bits a channel");
        assert_eq!(animation.frames.len(), 2);
        assert_eq!(animation.frames[0].pixels[0], 0x0102, "read big-endian");
        assert_eq!(animation.frames[1].pixels[0], 0x0304);
        let data = Data::new_copy(&bytes);
        assert!(frame(&data, 1, None).is_ok(), "and Skia takes the image");
    }

    #[test]
    fn what_this_crate_writes_is_what_this_crate_reads() {
        // The loop the gap broke. Both halves are here, so this is the test
        // that would have failed before the decoder existed -- Skia reported
        // one frame and no timing for every one of these.
        use crate::{
            encode::{
                Frame as EncFrame, FrameDepth, Pixels, SequenceSpec,
                color::ColorProfile, start,
            },
            export::ImageFormat,
        };
        use std::io::Cursor;

        for space in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
            for depth in [FrameDepth::Eight, FrameDepth::Sixteen] {
                let spec = SequenceSpec {
                    chroma: ChromaSampling::Full,
                    lossless: false,
                    width: 2,
                    height: 2,
                    frames: 3,
                    loops: None,
                    quality: 100.0,
                    density: 1.0,
                    color: ColorProfile::of(space),
                    space,
                    depth,
                    bits: None,
                };
                let mut out = Cursor::new(Vec::new());
                {
                    let mut sink =
                        start(ImageFormat::Apng, &spec, &mut out).unwrap();
                    for step in 0..3u8 {
                        let value = 40 + step * 60;
                        sink.write_frame(&EncFrame {
                            pixels: Pixels::Eight(
                                [value, 0, 0, 255]
                                    .iter()
                                    .copied()
                                    .cycle()
                                    .take(2 * 2 * CHANNELS)
                                    .collect(),
                            ),
                            width: 2,
                            height: 2,
                            delay_ms: 40,
                        })
                        .unwrap();
                    }
                    sink.finish().unwrap();
                }
                let bytes = out.into_inner();
                let what = format!("{space:?} at {depth:?}");

                assert!(is_animated(&bytes), "{what}");
                assert_eq!(delays(&bytes), Some(vec![40, 40, 40]), "{what}");

                let animation = read(&bytes, 2).expect(&what);
                assert_eq!(animation.frames.len(), 3, "{what}");
                // The space survives, which is what `cICP` is written for --
                // a Display P3 animation read back as sRGB would be the same
                // silent narrowing the encoder refuses.
                assert_eq!(animation.space, space, "{what}");
                assert_eq!(
                    animation.deep,
                    depth == FrameDepth::Sixteen,
                    "{what}"
                );
                // And the pixels are the ones that went in, at either depth:
                // eight bits widened and narrowed again is the identity.
                assert_eq!(pixel(&bytes, 0, 0, 0), [40, 0, 0, 255], "{what}");
                assert_eq!(pixel(&bytes, 2, 1, 1), [160, 0, 0, 255], "{what}");
            }
        }
    }

    #[test]
    fn a_lying_chunk_length_does_not_panic() {
        // `delays` walks the chunks to reach the timings, and the walk
        // validated the twenty-four bytes it reads while slicing the length
        // the *file* declared. Any file whose `fcTL` claims more than it
        // holds indexed past the end: truncation, one flipped byte, or
        // `0xFFFFFFFF` written over a length field.
        //
        // Reached from `loadImage` of anything APNG-shaped, on every image
        // this crate constructs.
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 4, 4);
            encoder.set_color(ColorType::Rgba);
            encoder.set_animated(2, 0).unwrap();
            encoder.set_frame_delay(1, 10).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for _ in 0..2 {
                writer.write_image_data(&[255u8; 4 * 4 * 4]).unwrap();
            }
            writer.finish().unwrap();
        }
        assert!(delays(&bytes).is_some(), "the fixture is an animation");

        // Every length field in the file, made impossible in turn.
        let mut at = PNG_MAGIC.len();
        let mut lengths = Vec::new();
        while at + CHUNK_HEADER <= bytes.len() {
            let len = u32::from_be_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]) as usize;
            lengths.push(at);
            let Some(next) = at
                .checked_add(CHUNK_OVERHEAD)
                .and_then(|n| n.checked_add(len))
            else {
                break;
            };
            at = next;
        }

        for start in lengths {
            let mut broken = bytes.clone();
            broken[start..start + WORD]
                .copy_from_slice(&u32::MAX.to_be_bytes());
            // An answer or none, but not a panic.
            let _ = delays(&broken);
        }

        // And truncation at every length, which is the other way a chunk
        // ends up claiming more than is there.
        for cut in (8..bytes.len()).step_by(7) {
            let _ = delays(&bytes[..cut]);
        }
    }

    #[test]
    fn the_delay_fraction_becomes_milliseconds() {
        // A denominator of zero means hundredths, which is the one value a
        // reader has to special-case rather than divide by.
        assert_eq!(delay_ms(1, 0), 10, "zero denominator is hundredths");
        assert_eq!(delay_ms(1, 100), 10);
        assert_eq!(delay_ms(1, 10), 100);
        // 1/30 of a second is 33.33ms, and rounding rather than truncating
        // is what keeps a 30fps animation from running fast.
        assert_eq!(delay_ms(1, 30), 33);
        assert_eq!(delay_ms(2, 30), 67);
        assert_eq!(delay_ms(0, 30), 0);
    }

    #[test]
    fn an_actl_shorter_than_its_fctl_chunks_shortens_the_animation() {
        use crate::{error::Error, image::Image};

        // Four `fcTL` chunks under an `acTL` declaring two. The APNG
        // specification requires the two counts to equal each other and
        // `png` 0.18 enforces neither -- it validates subframe rectangles
        // and `fdAT` sequence numbers and never compares these.
        let bad = declare_frames(
            &animation(
                (2, 2),
                [RED, BLUE, GREEN, YELLOW]
                    .into_iter()
                    .map(|rgba| Written {
                        w: 2,
                        h: 2,
                        x: 0,
                        y: 0,
                        dispose: DisposeOp::None,
                        blend: BlendOp::Source,
                        pixels: solid(2, 2, rgba),
                    })
                    .collect(),
            ),
            2,
        );

        // The count a caller is given is the count that can be decoded. It
        // was the four `fcTL` chunks, while the reader stopped at two.
        let image = Image::from_encoded(&bad).expect("the file still opens");
        assert_eq!(image.frame_count(), 2, "the shorter of the two counts");
        assert!(
            matches!(image.frame(2), Err(Error::FrameOutOfRange { .. })),
            "and the frames past it are refused rather than substituted"
        );

        // The frames that are offered decode to themselves, cold and warm.
        // Backwards, because a forward walk cannot see any of this: the held
        // reader is only reused for an index at or after where it stopped.
        for index in [0, 1, 1, 0] {
            let picture = image.frame(index).expect("an offered frame decodes");
            assert_eq!(
                top_left(&picture.inner),
                [RED, BLUE][index],
                "frame {index}"
            );
        }

        // And underneath, the index the guard used to admit is an error from
        // a fresh reader and from a held one alike. It answered with the
        // `acTL`-final frame cold and with "no frames" warm.
        let data = Data::new_copy(&bad);
        let mut slot = None;
        let cold = frame(&data, 3, Some(&mut slot)).map(|f| top_left(&f));
        let warm = frame(&data, 3, Some(&mut slot)).map(|f| top_left(&f));
        assert!(cold.is_err(), "cold: {cold:?}");
        assert_eq!(cold, warm, "and the same answer either way");
    }

    #[test]
    fn a_separate_default_image_is_not_the_animation_s_first_frame() {
        use crate::image::Image;

        // A green poster under `IDAT`, then red and blue animation frames.
        // `acTL` counts two and so does the `fcTL` walk, but the `png` reader
        // hands back three subframes: a default image with no `fcTL` before it
        // is an extra frame for decoders that cannot animate.
        let bytes = poster_animation(
            (2, 2),
            solid(2, 2, GREEN),
            [RED, BLUE]
                .into_iter()
                .map(|rgba| Written {
                    w: 2,
                    h: 2,
                    x: 0,
                    y: 0,
                    dispose: DisposeOp::None,
                    blend: BlendOp::Source,
                    pixels: solid(2, 2, rgba),
                })
                .collect(),
        );

        assert_eq!(
            delays(&bytes).map(|found| found.len()),
            Some(2),
            "the poster is not one of the frames a caller is offered"
        );
        let image = Image::from_encoded(&bytes).expect("the file opens");
        assert_eq!(image.frame_count(), 2);

        // Every offered index is an animation frame. The poster used to sit at
        // index 0 and push the animation along by one, which left the last
        // frame at index 2 -- past the count, so unreachable.
        let data = Data::new_copy(&bytes);
        for (index, expected) in [(0, RED), (1, BLUE)] {
            let mut slot = None;
            let picture = frame(&data, index, Some(&mut slot))
                .expect("an offered frame decodes");
            assert_eq!(top_left(&picture), expected, "frame {index}");
        }

        // And a walk that holds its reader agrees with the cold reads above,
        // since the poster is skipped once when the reader opens rather than
        // counted into where it resumes from.
        let mut slot = None;
        for (index, expected) in [(0, RED), (1, BLUE)] {
            let picture = frame(&data, index, Some(&mut slot))
                .expect("a held reader reaches the frame");
            assert_eq!(top_left(&picture), expected, "held frame {index}");
        }
    }

    #[test]
    fn an_actl_longer_than_its_fctl_chunks_is_the_fctl_chunks() {
        // The other direction, where the header overstates: the frames that
        // are there are all there is, and nothing invents the rest.
        let bad = declare_frames(
            &animation(
                (2, 2),
                [RED, BLUE]
                    .into_iter()
                    .map(|rgba| Written {
                        w: 2,
                        h: 2,
                        x: 0,
                        y: 0,
                        dispose: DisposeOp::None,
                        blend: BlendOp::Source,
                        pixels: solid(2, 2, rgba),
                    })
                    .collect(),
            ),
            9,
        );
        assert_eq!(delays(&bad).map(|found| found.len()), Some(2));
    }
}
