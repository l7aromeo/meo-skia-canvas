//! Animated WebP, muxed here around Skia's per-frame encoder.
//!
//! Unlike every other encoder in this module, the *picture* is not ours:
//! Skia encodes each frame to a complete single-frame WebP, and this takes
//! the compressed payload out of that file and writes it into an animation
//! container. Only the container is written here, which is the same division
//! of labour as APNG next door -- there the frames are deflated by the `png`
//! crate and this crate writes `acTL`, `fcTL` and `fdAT` around them.
//!
//! The reason it is written at all: Skia can encode an animated WebP --
//! `SkWebpEncoder::EncodeAnimated` is in the header shipped with
//! skia-bindings -- but nothing binds it. `skia-safe`'s `webp_encoder`
//! module ends at `// TODO: EncodeAnimated`, and the C shim skia-bindings
//! compiles exports only `C_SkWebpEncoder_Encode`. So the capability is
//! there in C++, unreachable from Rust, and the container is small enough to
//! write.
//!
//! # The format
//!
//! A WebP file is RIFF: a twelve-byte header, then chunks of a four-byte
//! tag, a four-byte little-endian length, and a payload padded to an even
//! length. A still image is one `VP8 ` chunk (lossy) or one `VP8L` chunk
//! (lossless), optionally preceded by `VP8X` and `ALPH`.
//!
//! An animation adds three:
//!
//! - `VP8X`, an extended header carrying the canvas size and a flag byte, which
//!   every animation must have because the `ANIMATION` flag lives in it.
//! - `ANIM`, the background colour and the loop count.
//! - `ANMF`, one per frame: position, size, duration, two flag bits, and then
//!   the frame's own image chunks.
//!
//! # Mixing lossy and lossless frames
//!
//! Each `ANMF` carries its own image data, so the specification allows a
//! lossy frame and a lossless one in the same file. The trap is alpha, and
//! it is not symmetrical:
//!
//! - a lossless frame stores alpha inside `VP8L` and must not carry `ALPH`;
//! - a lossy frame stores colour in `VP8 ` and alpha, if any, in a separate
//!   `ALPH` chunk that has to travel with it into the `ANMF`.
//!
//! Skia writes the still form, where `ALPH` sits beside `VP8 ` at the top
//! level. Copying only the image chunk into an `ANMF` would therefore drop
//! the alpha of every lossy frame -- silently, because the result decodes
//! perfectly well as an opaque animation. So the payload is rebuilt from
//! whichever chunks the frame actually produced, and the canvas-level
//! `ALPHA` flag is set if any frame brought one.

use std::{io::SeekFrom, slice};

use rayon::prelude::*;

use skia_safe::{
    AlphaType, ColorSpace, ColorType, ImageInfo, Pixmap, webp_encoder,
};

use little_exif::{
    exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata,
};

use crate::export::dots_per_inch;

use super::{
    Frame, FrameEncoder, FrameSink, Pixels, SequenceSpec, Sink, changed_region,
    crop_bytes,
};

/// A RIFF four-character code: `RIFF`, `WEBP`, `VP8X`, and every chunk tag.
const TAG_LEN: usize = 4;

/// The `RIFF` magic, the size field, and the `WEBP` form type.
const RIFF_HEADER_LEN: usize = TAG_LEN + size_of::<u32>() + TAG_LEN;

/// Every RIFF chunk begins with a four-byte tag and a four-byte length.
const CHUNK_HEADER_LEN: usize = TAG_LEN + size_of::<u32>();

/// `VP8X`'s payload: flags, three reserved bytes, then two 24-bit sizes.
const VP8X_PAYLOAD_LEN: u32 = 10;

/// `ANIM`'s payload: a BGRA background colour and a 16-bit loop count.
const ANIM_PAYLOAD_LEN: u32 = 6;

/// `ANMF`'s own fields, before the frame's image chunks.
const ANMF_HEADER_LEN: usize = 16;

/// The `VP8X` bit that says this file is an animation.
const VP8X_ANIMATION: u8 = 0b0000_0010;

/// The `VP8X` bit that says some frame carries alpha.
const VP8X_ALPHA: u8 = 0b0001_0000;

/// The `VP8X` bit that says an `ICCP` chunk describes the colour.
const VP8X_ICCP: u8 = 0b0010_0000;

/// The `VP8X` bit that says an `EXIF` chunk follows the image data.
const VP8X_EXIF: u8 = 0b0000_1000;

/// The `ANMF` bit that says a frame's pixels replace what is under them
/// rather than blending over it.
///
/// Which is what a rectangle of *this frame's* pixels means: it is not an
/// overlay, it is the region that changed. Blending would composite a
/// translucent pixel onto the one it is meant to replace.
const ANMF_DO_NOT_BLEND: u8 = 0b0000_0010;

/// `ANMF` stores a frame's offset in units of two pixels, so a rectangle can
/// only start on an even coordinate.
const OFFSET_GRAIN: u32 = 2;

/// A canvas dimension is stored one less than it is, in 24 bits, so the
/// largest a WebP can describe is this many pixels on a side.
const MAX_DIMENSION: u32 = 1 << 24;

/// A frame duration is 24 bits of milliseconds.
const MAX_DURATION_MS: u32 = (1 << 24) - 1;

/// Loop forever, which is how WebP spells it and how this crate's
/// [`SequenceSpec::loops`] of `None` is meant.
const LOOP_FOREVER: u16 = 0;

/// The quality at which a frame is encoded losslessly rather than lossily.
///
/// The same rule the still path applies, and for the same reason: `quality`
/// is a dial from 0 to 100 and the top of it means "do not lose anything".
const LOSSLESS_AT: f32 = 100.0;

/// Effort, not quality, once the encoder is lossless.
///
/// Kept in step with `WEBP_LOSSLESS_EFFORT` in `context::page`, which
/// measured the curve: everything above 25 is within two percent, so this
/// sits where it stops paying.
const LOSSLESS_EFFORT: f32 = 75.0;

pub(crate) struct AnimatedWebp;

impl FrameEncoder for AnimatedWebp {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        if spec.width == 0
            || spec.height == 0
            || spec.width > MAX_DIMENSION
            || spec.height > MAX_DIMENSION
        {
            return Err(format!(
                "A WebP is at most {MAX_DIMENSION}x{MAX_DIMENSION} (got \
                 {}x{})",
                spec.width, spec.height
            ));
        }

        // The RIFF length and the alpha flag are both facts about the whole
        // file, and neither is known yet: the first is the total size and
        // the second waits on a frame that carries alpha. Both are patched
        // in `finish`, which is what the `Seek` half of `Sink` is for.
        let mut header = Vec::with_capacity(RIFF_HEADER_LEN);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(b"WEBP");
        out.write_all(&header).map_err(io)?;

        let mut vp8x = Vec::with_capacity(VP8X_PAYLOAD_LEN as usize);
        vp8x.push(VP8X_ANIMATION);
        vp8x.extend_from_slice(&[0, 0, 0]); // reserved
        vp8x.extend_from_slice(&dimension(spec.width));
        vp8x.extend_from_slice(&dimension(spec.height));
        let flags_at = RIFF_HEADER_LEN + CHUNK_HEADER_LEN;
        write_chunk(out, b"VP8X", &vp8x)?;

        // `ANIM` is written by the first frame rather than here, because
        // `ICCP` has to precede it and the profile comes out of a frame Skia
        // has encoded. Skia writes one from the colour space the pixels
        // carry; dropping it would leave a Display P3 animation claiming
        // nothing, which a decoder reads as sRGB.
        Ok(Box::new(WebpSink {
            out,
            encoding: Encoding {
                quality: spec.quality,
                space: spec.space.to_skia_color_space().unwrap_or_else(|_| {
                    // Every space this crate exports is one Skia builds, so
                    // the fallback is unreachable rather than lenient -- and
                    // sRGB is what an unlabelled WebP means anyway.
                    ColorSpace::new_srgb()
                }),
            },
            density: spec.density,
            loops: spec.loops,
            flags_at: flags_at as u64,
            flags: VP8X_ANIMATION,
            opened: false,
            previous: None,
        }))
    }
}

/// What encoding one frame takes, and nothing about the file it lands in.
///
/// Apart from the sink because a sink holds `&mut dyn Sink`, which is neither
/// `Send` nor `Sync` and rightly so -- there is one write head and one order
/// to write in. This half has neither, so it is what the threads share.
struct Encoding {
    quality: f32,
    /// The space the frames are in, handed back to Skia so the profile it
    /// writes describes the pixels rather than assuming sRGB.
    space: ColorSpace,
}

struct WebpSink<'a> {
    out: &'a mut dyn Sink,
    encoding: Encoding,
    density: f32,
    loops: Option<u32>,
    /// Where the `VP8X` flag byte sits, so `finish` can rewrite it.
    flags_at: u64,
    /// The flags written in `start`, plus whatever the frames turned out to
    /// need. Kept rather than read back: a `Sink` writes and seeks, it does
    /// not read.
    flags: u8,
    /// Whether `ICCP` and `ANIM` have been written, which happens with the
    /// first frame.
    opened: bool,
    /// The canvas, so a frame can be compared with the one before it.
    ///
    /// Only the rectangle that changed is encoded: a frame of an animation
    /// usually moves a fraction of the page, and the format has offset and
    /// size fields on every `ANMF` for exactly this. libwebp's own encoder
    /// does it, and it saves the decoder the rest of the canvas as much as
    /// it saves the file the bytes.
    previous: Option<Vec<u8>>,
}

/// Bytes in one WebP pixel.
///
/// Eight-bit RGBA throughout: WebP has no deeper form, so a float canvas is
/// narrowed on the way in rather than carried and thrown away later.
const BYTES_PER_PIXEL: usize = 4;

/// Copies a rectangle out of a frame, as a frame of its own.
fn crop(frame: &Frame, eight: &[u8], region: (u32, u32, u32, u32)) -> Frame {
    let (_, _, width, height) = region;
    Frame {
        pixels: Pixels::Eight(crop_bytes(
            eight,
            frame.width,
            BYTES_PER_PIXEL,
            region,
        )),
        width,
        height,
        delay_ms: frame.delay_ms,
    }
}

/// One frame taken as far as it can go before its turn to be written.
///
/// Everything here is a function of the frame's own pixels and the ones
/// before it, so it can be produced on any thread. What is left needs the
/// file: which chunks have been written, which flags the ones already out
/// implied, and where the write head is.
struct Prepared {
    /// The rectangle of the canvas this frame carries.
    region: (u32, u32, u32, u32),
    payload: Payload,
    /// The `ICCP` profile Skia wrote beside this frame, on the frame that
    /// opens the animation and no other.
    ///
    /// Every frame is the same canvas in the same space, so it is one fact
    /// about the file that happens to be discoverable from any frame. Taken
    /// from the first only, rather than from each and thrown away, because
    /// the rest would be an allocation per frame that nothing reads.
    profile: Option<Vec<u8>>,
}

impl Encoding {
    /// The rectangle a frame differs from its predecessor in.
    ///
    /// The frame that opens an animation has none, and is the whole canvas.
    /// A still passage differs in nothing at all, and a frame still has to
    /// carry pixels, so that becomes the smallest rectangle the format
    /// allows.
    fn region(
        previous: Option<&[u8]>,
        pixels: &[u8],
        frame: &Frame,
    ) -> (u32, u32, u32, u32) {
        let Some(previous) = previous else {
            return (0, 0, frame.width, frame.height);
        };
        changed_region(
            previous,
            pixels,
            frame.width,
            frame.height,
            BYTES_PER_PIXEL,
            OFFSET_GRAIN,
        )
        .unwrap_or((
            0,
            0,
            OFFSET_GRAIN.min(frame.width),
            OFFSET_GRAIN.min(frame.height),
        ))
    }

    /// Everything one frame needs that does not need the file.
    ///
    /// `opening` asks for the colour profile, which only the first frame of
    /// the animation is read for. It is a parameter rather than a look at
    /// the sink's `opened` because this runs before any of the batch has
    /// been written, so that flag still describes the state the batch starts
    /// in rather than the state this frame will meet.
    fn prepare(
        &self,
        previous: Option<&[u8]>,
        pixels: &[u8],
        frame: &Frame,
        opening: bool,
    ) -> Result<Prepared, String> {
        let region = Self::region(previous, pixels, frame);
        let part = crop(frame, pixels, region);
        let still = encode_still(&part, self.quality, &self.space)?;
        let profile = match opening {
            true => chunk(&still, b"ICCP")?.map(<[u8]>::to_vec),
            false => None,
        };

        Ok(Prepared {
            region,
            payload: frame_payload(&still)?,
            profile,
        })
    }
}

impl WebpSink<'_> {
    /// Writes one prepared frame, and the chunks the first one drags with it.
    fn write_prepared(
        &mut self,
        frame: &Frame,
        prepared: Prepared,
    ) -> Result<(), String> {
        let Prepared {
            region,
            payload,
            profile,
        } = prepared;
        let (x, y, width, height) = region;

        if !self.opened {
            // Whatever colour Skia declared for the frame, declared once for
            // the file.
            if let Some(profile) = profile {
                write_chunk(self.out, b"ICCP", &profile)?;
                self.flags |= VP8X_ICCP;
            }

            let loops = match self.loops {
                None | Some(0) => LOOP_FOREVER,
                Some(count) => count.min(u32::from(u16::MAX)) as u16,
            };
            let mut anim = Vec::with_capacity(ANIM_PAYLOAD_LEN as usize);
            anim.extend_from_slice(&[0, 0, 0, 0]); // transparent background
            anim.extend_from_slice(&loops.to_le_bytes());
            write_chunk(self.out, b"ANIM", &anim)?;
            self.opened = true;
        }

        if payload.has_alpha {
            self.flags |= VP8X_ALPHA;
        }

        let mut anmf =
            Vec::with_capacity(ANMF_HEADER_LEN + payload.bytes.len());
        anmf.extend_from_slice(&three_bytes(x / OFFSET_GRAIN));
        anmf.extend_from_slice(&three_bytes(y / OFFSET_GRAIN));
        anmf.extend_from_slice(&dimension(width));
        anmf.extend_from_slice(&dimension(height));
        anmf.extend_from_slice(&three_bytes(
            frame.delay_ms.min(MAX_DURATION_MS),
        ));
        // Dispose nothing, blend nothing. The canvas has to survive from one
        // frame to the next, because everything outside this rectangle is
        // still the last frame's -- that is the whole point of sending a
        // rectangle. And these pixels replace what is under them rather than
        // compositing over it: they are not an overlay, they are what
        // changed, and a translucent one is meant to *be* translucent rather
        // than to be blended onto the pixel it replaces.
        anmf.push(ANMF_DO_NOT_BLEND);
        anmf.extend_from_slice(&payload.bytes);

        write_chunk(self.out, b"ANMF", &anmf)
    }
}

impl FrameSink for WebpSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        self.write_batch(slice::from_ref(frame))
    }

    /// Encodes the batch on every core and writes it on this one.
    ///
    /// A frame's rectangle is found by comparing it with the *pixels* of the
    /// one before it, which the caller rasterized before this was called, so
    /// nothing here waits on a frame being compressed. What is left in order
    /// is the container: the `VP8X` flags accumulate, `ICCP` and `ANIM` are
    /// written by whichever frame turns out to be first, and each `ANMF`
    /// follows the last.
    fn write_batch(&mut self, frames: &[Frame]) -> Result<(), String> {
        // Narrowed once. This called `frame.eight()` three times a frame --
        // to compare against the previous frame, to crop the rectangle out,
        // and to keep as the next frame's predecessor -- and on a float
        // canvas each one converts and allocates the whole page, about 8 MB
        // at 1080p, even where the rectangle being written is a few pixels.
        // On an eight-bit canvas the call borrows and two of the three were
        // free, which is why it went unnoticed.
        let mut pixels = frames
            .par_iter()
            .map(|frame| frame.eight().into_owned())
            .collect::<Vec<_>>();

        // The frame before the batch, for the first frame in it. `None` on
        // the animation's opening frame, and only there.
        let carried = self.previous.as_deref();
        let opening = !self.opened;
        let encoding = &self.encoding;

        let prepared = pixels
            .par_iter()
            .enumerate()
            .zip(frames)
            .map(|((nth, current), frame)| {
                let previous = match nth {
                    0 => carried,
                    _ => Some(&*pixels[nth - 1]),
                };
                encoding.prepare(previous, current, frame, opening && nth == 0)
            })
            .collect::<Result<Vec<_>, String>>()?;

        frames
            .iter()
            .zip(prepared)
            .try_for_each(|(frame, prepared)| {
                self.write_prepared(frame, prepared)
            })?;

        // The batch boundary is not something the format sees, so what the
        // next batch compares against is the last frame of this one.
        self.previous = pixels.pop();
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        // The resolution, in the one place WebP has for it. The still path
        // does the same through `little_exif`, and a viewer that reports a
        // still's DPI should not go quiet because the file animates.
        let dpi = f64::from(dots_per_inch(self.density));
        let mut exif = Metadata::new();
        exif.set_tag(ExifTag::XResolution(vec![dpi.into()]));
        exif.set_tag(ExifTag::YResolution(vec![dpi.into()]));
        if let Ok(bytes) = exif.as_u8_vec(FileExtension::WEBP) {
            // `as_u8_vec` hands back the whole chunk -- tag, length and
            // payload -- because that is what appending to a still needs.
            if bytes.len() > CHUNK_HEADER_LEN && &bytes[..TAG_LEN] == b"EXIF" {
                self.out.write_all(&bytes).map_err(io)?;
                self.flags |= VP8X_EXIF;
            }
        }

        let end = self.out.stream_position().map_err(io)?;

        if self.flags != VP8X_ANIMATION {
            self.out.seek(SeekFrom::Start(self.flags_at)).map_err(io)?;
            self.out.write_all(&[self.flags]).map_err(io)?;
        }

        // The RIFF size counts everything after its own eight-byte prefix.
        let size = u32::try_from(end - 8).map_err(|_| {
            "The WebP is too large for its RIFF header".to_string()
        })?;
        self.out.seek(SeekFrom::Start(4)).map_err(io)?;
        self.out.write_all(&size.to_le_bytes()).map_err(io)?;
        self.out.seek(SeekFrom::Start(end)).map_err(io)?;
        self.out.flush().map_err(io)
    }
}

/// The image chunks of one frame, ready to sit inside an `ANMF`.
struct Payload {
    bytes: Vec<u8>,
    has_alpha: bool,
}

/// Encodes one frame to a complete single-frame WebP, using Skia.
fn encode_still(
    frame: &Frame,
    quality: f32,
    space: &ColorSpace,
) -> Result<Vec<u8>, String> {
    let info = ImageInfo::new(
        (frame.width as i32, frame.height as i32),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        Some(space.clone()),
    );
    // `Pixmap::new` asks for a mutable slice although it only reads: the
    // type mirrors Skia's `SkPixmap`, which can be written through. The
    // frame is borrowed, so the copy is what makes the call possible rather
    // than a choice about performance -- and it is one frame, next to a WebP
    // encode.
    let row_bytes = frame.width as usize * 4;
    let mut pixels = frame.eight().into_owned();
    let pixmap = Pixmap::new(&info, &mut pixels, row_bytes)
        .ok_or_else(|| "Could not read the frame's pixels".to_string())?;

    let mut options = webp_encoder::Options::default();
    if quality >= LOSSLESS_AT {
        options.compression = webp_encoder::Compression::Lossless;
        options.quality = LOSSLESS_EFFORT;
    } else {
        options.compression = webp_encoder::Compression::Lossy;
        options.quality = quality;
    }

    let mut bytes = Vec::new();
    match webp_encoder::encode(&pixmap, &mut bytes, &options) {
        true => Ok(bytes),
        false => Err("Skia declined to encode a WebP frame".to_string()),
    }
}

/// Takes the image chunks out of a single-frame WebP.
///
/// `ALPH` travels with `VP8 `, and only with it: a lossless frame carries
/// its alpha inside `VP8L`, and a decoder given both would be reading the
/// same pixels twice. Anything else Skia wrote -- `VP8X`, `ICCP`, `EXIF` --
/// describes the file rather than the frame and is left behind.
fn frame_payload(still: &[u8]) -> Result<Payload, String> {
    let mut alpha: Option<&[u8]> = None;
    let mut image: Option<Chunk<'_>> = None;

    for (tag, body) in chunks(still)? {
        match tag {
            b"ALPH" => alpha = Some(body),
            b"VP8 " | b"VP8L" => {
                image = Some((tag, body));
                break;
            }
            _ => {}
        }
    }

    let Some((tag, body)) = image else {
        return Err("Skia wrote a WebP with no image data".to_string());
    };

    let lossless = tag == b"VP8L";
    let mut bytes = Vec::new();
    let mut has_alpha = false;

    if let Some(alpha) = alpha.filter(|_| !lossless) {
        push_chunk(&mut bytes, b"ALPH", alpha);
        has_alpha = true;
    }
    push_chunk(&mut bytes, tag, body);

    // A lossless frame's alpha is inside its own bitstream, and the one bit
    // that says so is the `alpha_is_used` flag: bit 28 of the five-byte
    // header, counting from the signature byte.
    if lossless && body.len() >= 5 {
        has_alpha = body[4] & 0b0001_0000 != 0;
    }

    Ok(Payload { bytes, has_alpha })
}

/// One chunk of a RIFF file: its four-byte tag and its payload.
type Chunk<'a> = (&'a [u8; 4], &'a [u8]);

/// The payload of the first chunk with this tag, if the file has one.
fn chunk<'a>(
    file: &'a [u8],
    want: &[u8; 4],
) -> Result<Option<&'a [u8]>, String> {
    Ok(chunks(file)?
        .into_iter()
        .find(|(tag, _)| *tag == want)
        .map(|(_, body)| body))
}

/// Walks a RIFF file, yielding each chunk's tag and payload.
fn chunks(file: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    if file.len() < RIFF_HEADER_LEN || &file[..TAG_LEN] != b"RIFF" {
        return Err("Not a RIFF file".to_string());
    }

    let mut found = Vec::new();
    let mut at = RIFF_HEADER_LEN;
    while at + CHUNK_HEADER_LEN <= file.len() {
        // SAFETY: both slices are `TAG_LEN` and `size_of::<u32>()` long by
        // construction, and the loop condition has already checked that a
        // whole chunk header is in bounds.
        let tag: &[u8; TAG_LEN] = file[at..at + TAG_LEN]
            .try_into()
            .expect("a four-byte slice is a four-byte array");
        let len = u32::from_le_bytes(
            file[at + TAG_LEN..at + CHUNK_HEADER_LEN]
                .try_into()
                .expect("a four-byte slice is a four-byte array"),
        ) as usize;
        let start = at + CHUNK_HEADER_LEN;
        let end = start.checked_add(len).filter(|end| *end <= file.len());
        let Some(end) = end else {
            return Err(
                "A WebP chunk runs past the end of the file".to_string()
            );
        };
        found.push((tag, &file[start..end]));
        // Chunks are padded to an even length, and the pad is not counted.
        at = end + (len & 1);
    }
    Ok(found)
}

/// Appends a chunk, with the padding byte an odd payload needs.
fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0);
    }
}

/// Writes a chunk straight to the sink.
fn write_chunk(
    out: &mut dyn Sink,
    tag: &[u8; 4],
    body: &[u8],
) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(CHUNK_HEADER_LEN + body.len() + 1);
    push_chunk(&mut bytes, tag, body);
    out.write_all(&bytes).map_err(io)
}

/// A 24-bit little-endian value, which is how WebP stores sizes and offsets.
fn three_bytes(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

/// A dimension as WebP writes it down: one below the number of pixels.
///
/// Both places a size appears -- the canvas size in `VP8X` and each frame's
/// size in `ANMF` -- store it this way, so the 24-bit field spans 1 to 2^24
/// rather than 0 to 2^24 - 1. Neither box can describe an empty image, so
/// the value it cannot express is the one nothing needs.
///
/// A function rather than four subtractions: the convention was written out
/// at each site with nothing saying what it was, which reads as an
/// off-by-one until you have the specification open. `start` refuses a
/// zero-sized canvas before any of this runs.
fn dimension(pixels: u32) -> [u8; 3] {
    three_bytes(pixels - 1)
}

fn io(error: std::io::Error) -> String {
    format!("Could not write the WebP: {error}")
}
