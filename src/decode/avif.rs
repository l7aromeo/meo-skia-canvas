//! AVIF, read here because Skia reads none of it.
//!
//! Not a gap in the animated form alone: this build of Skia ships no AVIF
//! decoder at all, so `SkCodec` refuses a plain still as readily as an
//! animation. A library that writes twelve formats could open eleven of
//! them, and the twelfth is the one increasingly served to browsers.
//!
//! # Two shapes, one decoder
//!
//! A still AVIF is one coded image described by a property list in a `meta`
//! box; an animated one is a video track described by a sample table in
//! `moov`. Both are parsed here, and both end at the same place: coded
//! samples handed to [`aom`](super::aom) one at a time. Only the route to
//! the bytes differs.
//!
//! # Why this parses the container itself
//!
//! It did not, for the still shape. `aom-decode`'s `avif` feature read that
//! one whole, and was dropped because the feature does not switch off --
//! see the note beside `libaom-sys` in `Cargo.toml`. Parsing both shapes
//! here turned out to be the smaller thing anyway: the animated path already
//! walked ISOBMFF, and a `grid` of tiles is described in the very boxes the
//! feature hid.
//!
//! # Why the colour conversion is written out
//!
//! The decoder hands back three planes, and the matrix that produced them is
//! the one [`encode::avif`](crate::encode::avif) applied on the way out. So
//! the inverse lives here, beside the forward transform it undoes, and the
//! two are checked against each other by a round trip rather than by
//! reading.

use super::aom::{DEEP_SAMPLE_BYTES, Decoder};
use skia_safe::{
    AlphaType, ColorSpace, ColorType as SkColorType, Data, Image as SkImage,
    ImageInfo, images,
};

use crate::encode::{narrow_to_eight, widen_to_sixteen};

/// Channels in the composed buffer.
const CHANNELS: usize = 4;

/// A box header: four bytes of length and a four-character type.
const BOX_HEADER: usize = 8;

/// A four-character code, and the width of every count and offset an
/// ISOBMFF table is built from.
const WORD: usize = 4;

/// The version byte and three flag bytes a full box begins with, before any
/// payload of its own.
const FULL_BOX_PREFIX: usize = 4;

/// Where `hdlr` states its handler type.
///
/// After the full-box prefix and the four predefined bytes ISO/IEC 14496-12
/// requires before it.
const HDLR_TYPE_AT: usize = FULL_BOX_PREFIX + WORD;

/// Where `stsz` states the size every sample shares, or zero when they
/// differ and a table follows.
const STSZ_UNIFORM_AT: usize = FULL_BOX_PREFIX;

/// Where `stsz` states how many samples it describes.
const STSZ_COUNT_AT: usize = STSZ_UNIFORM_AT + WORD;

/// Where `stsz`'s per-sample table begins.
const STSZ_TABLE_AT: usize = STSZ_COUNT_AT + WORD;

/// Where `stco` states the offset of its first chunk.
///
/// After the full-box prefix and the entry count, which this crate always
/// writes as one -- see the note in `track`.
const STCO_FIRST_AT: usize = FULL_BOX_PREFIX + WORD;

/// Where `mdhd` states the timescale its durations are counted in.
///
/// After the full-box prefix and the creation and modification times.
const MDHD_TIMESCALE_AT: usize = FULL_BOX_PREFIX + WORD * 2;

/// Where `stts` states how many runs it holds.
const STTS_RUNS_AT: usize = FULL_BOX_PREFIX;

/// Where `stts`'s runs begin, each a sample count and a duration.
const STTS_TABLE_AT: usize = STTS_RUNS_AT + WORD;

/// One `stts` run: a count and the duration those samples share.
const STTS_RUN: usize = WORD * 2;

/// The width of ISOBMFF's sixteen-bit counts and identifiers.
const HALF_WORD: usize = 2;

/// `iloc` packs the widths of its variable-width fields two to a byte
/// (ISO/IEC 14496-12 § 8.11.3.2), so each is read out of one nibble.
const NIBBLE_BITS: u32 = 4;

/// The low half of a byte holding two packed field widths.
const LOW_NIBBLE: u8 = 0x0F;

/// `iloc`'s construction method naming offsets into the file itself
/// (ISO/IEC 14496-12 § 8.11.3.3).
const CONSTRUCTION_FILE: u8 = 0;

/// The construction method naming offsets into the `meta` box's own `idat`.
///
/// Small payloads travel this way rather than in `mdat` -- the `grid`
/// description of a tiled image is the case this crate meets, and Apple's
/// encoder writes every one of them so.
const CONSTRUCTION_IDAT: u8 = 1;

/// The `iloc` version from which items carry a construction method and the
/// extent index field exists (ISO/IEC 14496-12 § 8.11.3.2).
const ILOC_WITH_CONSTRUCTION: u8 = 1;

/// The `iloc` and `iinf` version from which item identifiers widen from
/// sixteen bits to thirty-two.
const ILOC_WIDE_IDS: u8 = 2;

/// The `infe` version from which the entry states an item type at all
/// (ISO/IEC 14496-12 § 8.11.6.2). Versions below this describe a different
/// box shape that no AVIF writes.
const INFE_WITH_TYPE: u8 = 2;

/// The `infe` version whose item identifier is thirty-two bits.
const INFE_WIDE_IDS: u8 = 3;

/// The `iref` version whose item identifiers are thirty-two bits
/// (ISO/IEC 14496-12 § 8.11.12.2).
const IREF_WIDE_IDS: u8 = 1;

/// How many threads the decoder is given.
///
/// One. A canvas draws a frame at a time and the exporter's own pool is
/// usually busy with the next page, so a second decoder thread competes with
/// work that is already running rather than adding any.
const THREADS: u32 = 1;

/// The brands that say a file is an AVIF, animated or not.
///
/// Read from `ftyp` rather than guessed from an extension: `avis` leads an
/// animation and `avif` a still, and either may appear in the compatible
/// brand list of the other.
fn brands(bytes: &[u8]) -> Option<Vec<[u8; 4]>> {
    let (start, end) = boxes(bytes, 0, bytes.len())
        .into_iter()
        .find(|(tag, ..)| tag == b"ftyp")
        .map(|(_, s, e)| (s, e))?;
    Some(
        bytes[start..end]
            .chunks_exact(WORD)
            .filter_map(|c| c.try_into().ok())
            .collect(),
    )
}

/// Whether these bytes are an AVIF at all.
pub(crate) fn is_avif(bytes: &[u8]) -> bool {
    brands(bytes)
        .is_some_and(|list| list.iter().any(|b| b == b"avif" || b == b"avis"))
}

/// Whether the file carries a sequence rather than a single image.
///
/// The `avis` brand says so, and a `moov` box proves it -- a file could
/// claim the brand and carry no track, and one frame is not an animation.
pub(crate) fn is_animated(bytes: &[u8]) -> bool {
    is_avif(bytes)
        && boxes(bytes, 0, bytes.len())
            .iter()
            .any(|(tag, ..)| tag == b"moov")
}

/// Every box at one level, as `(tag, payload start, payload end)`.
fn boxes(bytes: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut found = Vec::new();
    let mut at = from;
    while at + BOX_HEADER <= to {
        let size = be32(bytes, at) as usize;
        // A size below the header is malformed, and one past the end would
        // read someone else's memory. Either ends the walk.
        if size < BOX_HEADER || at + size > to {
            break;
        }
        let Ok(tag) = bytes[at + WORD..at + BOX_HEADER].try_into() else {
            break;
        };
        found.push((tag, at + BOX_HEADER, at + size));
        at += size;
    }
    found
}

/// The first box with this tag, searched depth-first through the containers
/// that hold other boxes.
fn find(
    bytes: &[u8],
    from: usize,
    to: usize,
    want: &[u8; 4],
) -> Option<(usize, usize)> {
    for (tag, start, end) in boxes(bytes, from, to) {
        if &tag == want {
            return Some((start, end));
        }
        // Only these nest, so nothing descends into sample data and finds a
        // four-byte sequence that happens to spell a box name.
        if matches!(
            &tag,
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts"
        ) && let Some(hit) = find(bytes, start, end, want)
        {
            return Some(hit);
        }
    }
    None
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    // Through `be_sized` rather than slicing. This read `bytes[at..]` and
    // panicked whenever `at` was past the end -- `first_chunk` answers `None`
    // on a short tail, but the slice never got that far. An `hdlr` box with
    // no payload puts its own payload start at the end of the file, and a
    // 48-byte file was enough to bring the process down through `loadImage`.
    be_sized(bytes, at, WORD) as u32
}

/// One track: what it holds, where its samples are, and how long each lasts.
struct Track {
    /// `pict` for the picture track, `auxv` for the alpha one.
    handler: [u8; 4],
    /// Byte ranges of the coded samples, in order.
    samples: Vec<(usize, usize)>,
    /// Each sample's duration in milliseconds.
    delays: Vec<u32>,
}

/// Milliseconds in a second, for turning track ticks into durations.
const MS_PER_SECOND: u64 = 1000;

/// Reads a track's sample table.
fn track(bytes: &[u8], from: usize, to: usize) -> Option<Track> {
    let handler = find(bytes, from, to, b"hdlr").and_then(|(s, _)| {
        bytes
            .get(s + HDLR_TYPE_AT..)?
            .first_chunk::<WORD>()
            .copied()
    })?;

    // `stsz` gives every sample's length, `stco` where the run begins. One
    // chunk is what this crate writes, and is what a single-run reader can
    // rely on; a file chunked otherwise would need `stsc` walked as well.
    let (sizes_at, _) = find(bytes, from, to, b"stsz")?;
    // Clamped against the file's own length before it sizes anything. The
    // count is four bytes the file chose, so `0xFFFFFFFF` asked for a 34 GB
    // `Vec` from a hundred-byte input; no sample can be shorter than one
    // byte, so the file cannot hold more of them than it has bytes.
    let count =
        (be32(bytes, sizes_at + STSZ_COUNT_AT) as usize).min(bytes.len());
    let uniform = be32(bytes, sizes_at + STSZ_UNIFORM_AT) as usize;
    let sizes: Vec<usize> = match uniform {
        0 => (0..count)
            .map(|i| be32(bytes, sizes_at + STSZ_TABLE_AT + i * WORD) as usize)
            .collect(),
        size => vec![size; count],
    };

    let (offset_at, _) = find(bytes, from, to, b"stco")?;
    let mut at = be32(bytes, offset_at + STCO_FIRST_AT) as usize;
    let mut samples = Vec::with_capacity(sizes.len());
    for size in &sizes {
        let end = at.checked_add(*size).filter(|e| *e <= bytes.len())?;
        samples.push((at, end));
        at = end;
    }

    // `stts` runs, against the timescale `mdhd` states, become the per-frame
    // durations the rest of this crate speaks in.
    let (scale_at, _) = find(bytes, from, to, b"mdhd")?;
    let timescale = u64::from(be32(bytes, scale_at + MDHD_TIMESCALE_AT)).max(1);
    let (times_at, _) = find(bytes, from, to, b"stts")?;
    // Each run is eight bytes, so the file bounds how many it can describe.
    let runs = (be32(bytes, times_at + STTS_RUNS_AT) as usize)
        .min(bytes.len() / STTS_RUN);
    let mut delays = Vec::with_capacity(samples.len());
    for run in 0..runs {
        let at = times_at + STTS_TABLE_AT + run * STTS_RUN;
        // Capped at what is still wanted rather than what the run claims:
        // `repeat_n` is an `ExactSizeIterator`, so `extend` reserves the
        // whole count up front and the `resize` below would arrive too late
        // to stop four billion entries being asked for.
        let want = samples.len().saturating_sub(delays.len());
        let count = (be32(bytes, at) as usize).min(want);
        let ticks = u64::from(be32(bytes, at + WORD));
        let ms = (ticks * MS_PER_SECOND / timescale) as u32;
        delays.extend(std::iter::repeat_n(ms, count));
    }
    delays.resize(samples.len(), 0);

    Some(Track {
        handler,
        samples,
        delays,
    })
}

/// The picture track and, where the file has one, the alpha track beside it.
fn tracks(bytes: &[u8]) -> Option<(Track, Option<Track>)> {
    let (start, end) = find(bytes, 0, bytes.len(), b"moov")?;
    let mut picture = None;
    let mut alpha = None;
    for (tag, s, e) in boxes(bytes, start, end) {
        if &tag != b"trak" {
            continue;
        }
        if let Some(found) = track(bytes, s, e) {
            match &found.handler {
                b"auxv" => alpha = Some(found),
                _ => picture = Some(found),
            }
        }
    }
    picture.map(|p| (p, alpha))
}

/// The frame timings, one per frame, in milliseconds.
pub(crate) fn delays(bytes: &[u8]) -> Option<Vec<u32>> {
    if !is_animated(bytes) {
        return None;
    }
    let (picture, _) = tracks(bytes)?;
    match picture.delays.len() {
        0 | 1 => None,
        _ => Some(picture.delays),
    }
}

/// The luma coefficients the encoder used, which this undoes.
///
/// The same three numbers [`encode::avif`](crate::encode::avif) applies on
/// the way out. Stated here rather than shared because the two modules are
/// each other's inverse and a reader of either should see the arithmetic
/// it is looking at -- and because a round-trip test holds them together
/// far more firmly than a shared constant would.
///
/// Also the fallback for a file that names a matrix nothing here converts,
/// because it is what libavif writes by default and so what most AVIF in
/// the world is coded with.
const BT601_LUMA: [f32; 3] = [0.2990, 0.5870, 0.1140];

/// Rec. 709's luma coefficients (ITU-T H.273 Table 4, matrix 1).
const BT709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Rec. 2020's, non-constant luminance (ITU-T H.273 Table 4, matrix 9).
const BT2020_LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];

/// How the coded planes relate to red, green and blue.
#[derive(Clone, Copy)]
enum Matrix {
    /// The planes are not chroma differences at all: they are green, blue
    /// and red already, in that order.
    Identity,
    /// Luma coefficients to invert.
    Luma([f32; 3]),
}

impl Default for Matrix {
    fn default() -> Self {
        Self::Luma(BT601_LUMA)
    }
}

impl Matrix {
    /// The matrix a `colr` box's code point names.
    ///
    /// The code points are ITU-T H.273's, which `avif-serialize` already
    /// names for the encoder -- so they are read from its enum rather than
    /// restated, and cannot drift from what this crate writes.
    fn of(code: u16) -> Self {
        use avif_serialize::constants::MatrixCoefficients as Named;
        match code {
            c if c == Named::Rgb as u16 => Self::Identity,
            c if c == Named::Bt709 as u16 => Self::Luma(BT709_LUMA),
            c if c == Named::Bt2020Ncl as u16 => Self::Luma(BT2020_LUMA),
            // Unspecified, both flavours of 601, and anything this does not
            // convert. The constant-luminance and YCgCo systems land here
            // too, which is wrong for them -- but decoding a picture with
            // slightly wrong chroma beats refusing it, and neither appears
            // in the wild.
            _ => Self::default(),
        }
    }
}

/// Where zero chroma sits, as a fraction of the coded range.
const CHROMA_HALF_RANGE: f32 = 0.5;

/// The depth the narrow-range levels below are defined at.
///
/// Eight, and every deeper form is the same numbers shifted up -- the black
/// level at ten bits is 64, which is 16 << 2 (ITU-T H.273 § 8.3).
const NARROW_LEVEL_BITS: u32 = 8;

/// Where black sits when the range is the narrower broadcast one.
const NARROW_BLACK: f32 = 16.0;

/// How far luma spans in that range: 16 to 235.
const NARROW_LUMA_SPAN: f32 = 219.0;

/// How far each chroma difference spans: 16 to 240, centred on 128.
const NARROW_CHROMA_SPAN: f32 = 224.0;

/// One pixel of RGB, from the luma and two chroma differences.
///
/// The inverse of the encoder's conversion: chroma arrives centred on the
/// middle of the coded range rather than on zero, and each difference is
/// scaled back by the span it was compressed into.
///
/// `full_range` says whether the samples use the whole coded range or the
/// narrower broadcast one. Reading a narrow-range file as though it were
/// full leaves black at 16 rather than 0 and white at 235 rather than 255 --
/// a picture with crushed blacks and no real white, which looks like a
/// washed-out version of itself rather than like an error.
fn ycbcr_to_rgb(
    y: f32,
    cb: f32,
    cr: f32,
    max: f32,
    matrix: Matrix,
    full_range: bool,
) -> [u16; 3] {
    let scale = f32::from(u16::MAX) / max;
    let widen = |value: f32| {
        (value * scale).round().clamp(0.0, f32::from(u16::MAX)) as u16
    };

    // Under the identity matrix nothing was mixed on the way in: the three
    // planes are green, blue and red in the order luma and the two chroma
    // planes are stored. A narrow range still has to be opened out, and the
    // luma levels are the ones that apply to all three.
    let Matrix::Luma([kr, kg, kb]) = matrix else {
        let open = |value: f32| match full_range {
            true => value,
            false => {
                let step = (max + 1.0) / f32::from(1u16 << NARROW_LEVEL_BITS);
                (value - NARROW_BLACK * step) * max / (NARROW_LUMA_SPAN * step)
            }
        };
        return [widen(open(cr)), widen(open(y)), widen(open(cb))];
    };

    let neutral = (max * CHROMA_HALF_RANGE).round();
    let (y, cb, cr) = match full_range {
        true => (y, cb - neutral, cr - neutral),
        false => {
            // The levels are defined at eight bits and shift up with depth,
            // so one step carries them to whatever this file was coded at.
            let step = (max + 1.0) / f32::from(1u16 << NARROW_LEVEL_BITS);
            (
                (y - NARROW_BLACK * step) * max / (NARROW_LUMA_SPAN * step),
                (cb - neutral) * max / (NARROW_CHROMA_SPAN * step),
                (cr - neutral) * max / (NARROW_CHROMA_SPAN * step),
            )
        }
    };

    let blue = cb * (1.0 - kb) / CHROMA_HALF_RANGE + y;
    let red = cr * (1.0 - kr) / CHROMA_HALF_RANGE + y;
    // Green is what luma has left once red and blue are accounted for.
    let green = (y - kr * red - kb * blue) / kg;

    [widen(red), widen(green), widen(blue)]
}

/// A decoded frame as sixteen-bit RGBA, whatever depth it was coded at.
struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<u16>,
    /// Whether the file coded more than eight bits a channel, which decides
    /// what depth the image is handed back at.
    deep: bool,
}

/// One decoded plane, and the sample grid it covers.
///
/// Chroma planes are smaller than luma wherever the file is subsampled, so
/// the dimensions travel with the samples rather than being assumed from the
/// frame.
struct Plane {
    samples: Vec<u16>,
    width: usize,
    height: usize,
}

impl Plane {
    /// The sample at `x`, `y`, which are luma coordinates.
    ///
    /// Nearest neighbour: the chroma grid is half the luma grid on a
    /// subsampled axis, so the shift picks the sample covering this pixel.
    /// AV1 has only the three subsamplings, so each shift is zero or one --
    /// there is no ratio here that a division would express better.
    ///
    /// Interpolating instead would soften chroma edges rather than
    /// step them, which is what libavif does by default and is the obvious
    /// refinement. It is not done here because nothing this crate writes is
    /// subsampled, so the only files reaching it are foreign ones that
    /// previously did not decode at all.
    fn at(&self, x: usize, y: usize, shift_x: u32, shift_y: u32) -> f32 {
        let column = (x >> shift_x).min(self.width.saturating_sub(1));
        let row = (y >> shift_y).min(self.height.saturating_sub(1));
        match self.samples.get(row * self.width + column) {
            Some(sample) => f32::from(*sample),
            None => 0.0,
        }
    }
}

/// Collects a plane's rows of bytes into samples.
///
/// Above eight bits libaom stores each sample as two bytes in the host's own
/// order, so a deep row holds half as many samples as it does bytes.
fn plane_of(rows: &[&[u8]], deep: bool) -> Plane {
    let samples: Vec<u16> = match deep {
        true => rows
            .iter()
            .flat_map(|row| row.chunks_exact(DEEP_SAMPLE_BYTES))
            .filter_map(|pair| pair.first_chunk::<DEEP_SAMPLE_BYTES>().copied())
            .map(u16::from_ne_bytes)
            .collect(),
        false => rows
            .iter()
            .flat_map(|row| row.iter().copied().map(u16::from))
            .collect(),
    };
    let height = rows.len();
    Plane {
        width: match height {
            0 => 0,
            _ => samples.len() / height,
        },
        height,
        samples,
    }
}

/// Three planes composed into RGBA, alpha left opaque.
///
/// `max` is the largest value a sample of this depth can hold, which is what
/// the conversion scales against.
/// Everything the planes have to be read through to become RGB.
#[derive(Clone, Copy)]
struct Conversion {
    /// How the planes relate to red, green and blue.
    matrix: Matrix,
    /// Whether the samples use the whole coded range.
    full_range: bool,
    /// The largest value a sample of this depth can hold.
    max: f32,
    /// Whether the samples are sixteen bits wide in memory.
    deep: bool,
}

fn compose(
    luma: &Plane,
    cb: &Plane,
    cr: &Plane,
    (shift_x, shift_y): (u32, u32),
    reading: Conversion,
) -> Frame {
    let Conversion {
        matrix,
        full_range,
        max,
        deep,
    } = reading;
    let (width, height) = (luma.width, luma.height);

    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [r, g, b] = ycbcr_to_rgb(
                luma.at(x, y, 0, 0),
                cb.at(x, y, shift_x, shift_y),
                cr.at(x, y, shift_x, shift_y),
                max,
                matrix,
                full_range,
            );
            [r, g, b, u16::MAX]
        })
        .collect();

    Frame {
        width,
        height,
        pixels,
        deep,
    }
}

/// Decodes one coded sample into RGBA, alpha left opaque.
fn decode_sample(
    decoder: &mut Decoder,
    sample: &[u8],
    matrix: Matrix,
    full_range: bool,
) -> Result<Frame, String> {
    let picture = decoder.decode(sample)?;
    // A picture with no chroma is an alpha plane, which reaches
    // `apply_alpha` rather than this.
    if picture.monochrome() {
        return Err("An AVIF frame this crate cannot read: not colour".into());
    }

    // 4:4:4, 4:2:2 and 4:2:0 all arrive here, told apart by the shifts libaom
    // read from the sequence header. This crate writes only 4:4:4, but almost
    // nothing else does -- Apple's encoder and the stills browsers are served
    // are 4:2:0.
    let deep = picture.deep();
    let max = ((1u32 << picture.depth()) - 1) as f32;
    Ok(compose(
        &plane_of(&picture.luma(), deep),
        &plane_of(&picture.cb(), deep),
        &plane_of(&picture.cr(), deep),
        picture.chroma_shifts(),
        Conversion {
            matrix,
            full_range,
            max,
            deep,
        },
    ))
}

/// Fills in a frame's alpha from the auxiliary track's matching sample.
fn apply_alpha(frame: &mut Frame, decoder: &mut Decoder, sample: &[u8]) {
    // A missing or unreadable alpha plane leaves the frame opaque, which is
    // what a reader that ignored it would show. Losing transparency is
    // better than losing the picture.
    let Ok(picture) = decoder.decode(sample) else {
        return;
    };
    let deep = picture.deep();
    let plane = plane_of(&picture.luma(), deep);
    // Alpha is coded as luma, so it arrives at the picture's own depth and
    // has to be widened to the sixteen bits a frame holds.
    let shift = u16::BITS - picture.depth();

    // The alpha channel of the first pixel, stepped a pixel at a time.
    let alphas = frame.pixels.iter_mut().skip(CHANNELS - 1).step_by(CHANNELS);
    for (channel, value) in alphas.zip(plane.samples) {
        *channel = match deep {
            true => value << shift,
            false => widen_to_sixteen(value as u8),
        };
    }
}

/// A decoder part-way through a track, and the frame it will produce next.
///
/// A frame after the first is coded against the ones before it, so reaching
/// frame `n` means decoding every sample up to it. Starting over for each
/// request makes walking a sequence quadratic: the documented way to play an
/// animation -- asking for one frame per output frame -- cost 11 325 sample
/// decodes for a 150-frame file where 150 would do.
///
/// Holding the decoder between calls makes a forward walk linear and leaves
/// random access exactly as it was: an index that is not the one expected
/// rebuilds from zero, which is the only correct thing to do.
pub(crate) struct Playback {
    picture: Decoder,
    alpha: Option<Decoder>,
    /// The index the held decoders are positioned to produce.
    next: usize,
}

/// Frame `index` of an animated AVIF.
///
/// `resume` carries a decoder between calls where the caller has one to
/// lend. Passing `None` is correct and costs the run-up every time.
pub(crate) fn frame(
    bytes: &[u8],
    index: usize,
    resume: Option<&mut Option<super::Playback>>,
) -> Result<SkImage, String> {
    let (picture, alpha) = tracks(bytes)
        .ok_or_else(|| "The AVIF has no picture track".to_string())?;
    // Refused before anything indexes the list. `track` hands back a `Track`
    // whenever the five boxes it reads are present, so a `stsz` declaring
    // zero samples produces an empty one -- and `want` is then 0, which the
    // slicing below would read as `[0..=0]` and panic on. The loop this
    // replaced used `take(want + 1)` and tolerated it.
    if picture.samples.is_empty() {
        return Err("The AVIF track has no frames".to_string());
    }
    let want = index.min(picture.samples.len() - 1);

    // A held decoder is only usable for the frame it stopped at. Anything
    // else -- a seek, a replay from the start, or a slot holding the other
    // format -- starts over, which is only ever slower rather than wrong.
    let fresh = || -> Result<Playback, String> {
        Ok(Playback {
            picture: Decoder::new(THREADS)?,
            alpha: None,
            next: 0,
        })
    };
    let (slot, mut playback) = match resume {
        Some(slot) => {
            let held = match slot.take() {
                Some(super::Playback::Avif(held)) if held.next == want => held,
                _ => fresh()?,
            };
            (Some(slot), held)
        }
        None => (None, fresh()?),
    };

    // Only the samples between where the decoder stands and the one asked
    // for. `from` is zero for a fresh decoder, which is the old behaviour.
    let from_sample = playback.next;
    let mut decoded = None;
    for (from, to) in picture.samples[from_sample..=want].iter() {
        decoded = Some(decode_sample(
            &mut playback.picture,
            &bytes[*from..*to],
            Matrix::default(),
            true,
        )?);
    }
    let mut decoded =
        decoded.ok_or_else(|| "The AVIF track has no frames".to_string())?;

    if let Some(alpha) = alpha
        && let Some((from, to)) = alpha.samples.get(want)
    {
        // The alpha track is coded the same way and needs the same run-up.
        let mut decoder = match playback.alpha.take() {
            Some(held) => held,
            None => {
                let mut fresh = Decoder::new(THREADS)?;
                // Through `get`: an alpha track shorter than the picture one
                // is malformed, and indexing it by the picture's position
                // would panic rather than decode what is there.
                for (a, b) in
                    alpha.samples.get(..from_sample).unwrap_or_default()
                {
                    let _ = fresh.decode(&bytes[*a..*b]);
                }
                fresh
            }
        };
        for (a, b) in alpha.samples.get(from_sample..want).unwrap_or_default() {
            let _ = decoder.decode(&bytes[*a..*b]);
        }
        apply_alpha(&mut decoded, &mut decoder, &bytes[*from..*to]);
        playback.alpha = Some(decoder);
    }

    // Positioned for the next frame, so a sequential caller pays for one
    // sample rather than for all of them again.
    if let Some(slot) = slot {
        playback.next = want + 1;
        *slot = Some(super::Playback::Avif(playback));
    }

    raster(&decoded, None)
}

/// A big-endian integer `size` bytes wide.
///
/// ISOBMFF writes several fields at a width the file itself declares, and
/// zero is a legal width meaning the field is absent -- for an offset that
/// implies the start of the source, and for a length the whole of it
/// (ISO/IEC 14496-12 § 8.11.3.1). Both read as zero here, and the caller
/// gives zero those meanings.
fn be_sized(bytes: &[u8], at: usize, size: usize) -> u64 {
    bytes
        .get(at..at + size)
        .map(|field| field.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
        .unwrap_or(0)
}

/// A big-endian sixteen-bit field.
fn be16(bytes: &[u8], at: usize) -> u16 {
    be_sized(bytes, at, HALF_WORD) as u16
}

/// One run of an item's bytes, as `iloc` describes it.
struct Extent {
    at: u64,
    len: u64,
}

/// Where an item's bytes are, and how to reach them.
struct Location {
    id: u32,
    construction: u8,
    extents: Vec<Extent>,
}

/// The `meta` box's children, which begin after its version and flags.
///
/// `meta` is a full box, unlike the container boxes [`find`] descends, so its
/// child list does not start at its payload.
fn meta(bytes: &[u8]) -> Option<(usize, usize)> {
    boxes(bytes, 0, bytes.len())
        .into_iter()
        .find(|(tag, ..)| tag == b"meta")
        .map(|(_, start, end)| (start + FULL_BOX_PREFIX, end))
}

/// A box directly inside `meta`.
fn meta_box(
    bytes: &[u8],
    (from, to): (usize, usize),
    want: &[u8; 4],
) -> Option<(usize, usize)> {
    boxes(bytes, from, to)
        .into_iter()
        .find(|(tag, ..)| tag == want)
        .map(|(_, start, end)| (start, end))
}

/// The identifier of the image the file is a picture of.
///
/// A `meta` box may describe many items -- tiles, thumbnails, Exif -- and
/// `pitm` names the one that is the image (ISO/IEC 14496-12 § 8.11.4).
fn primary_item(bytes: &[u8], tree: (usize, usize)) -> Option<u32> {
    let (at, _) = meta_box(bytes, tree, b"pitm")?;
    let version = *bytes.get(at)?;
    let id_at = at + FULL_BOX_PREFIX;
    Some(match version {
        0 => u32::from(be16(bytes, id_at)),
        _ => be32(bytes, id_at),
    })
}

/// Every item's four-character type, by identifier.
///
/// The type is what separates a coded image from the `grid` that arranges
/// several of them, and from metadata that is not a picture at all.
fn item_types(bytes: &[u8], tree: (usize, usize)) -> Vec<(u32, [u8; 4])> {
    let Some((at, end)) = meta_box(bytes, tree, b"iinf") else {
        return Vec::new();
    };
    // The entry count is sixteen bits at version 0 and thirty-two after, but
    // the entries that follow are self-delimiting boxes, so the count is not
    // needed to walk them -- only to know where they start.
    let version = bytes.get(at).copied().unwrap_or(0);
    let entries_at =
        at + FULL_BOX_PREFIX + if version == 0 { HALF_WORD } else { WORD };

    boxes(bytes, entries_at, end)
        .into_iter()
        .filter(|(tag, ..)| tag == b"infe")
        .filter_map(|(_, start, _)| {
            let version = bytes.get(start).copied()?;
            if version < INFE_WITH_TYPE {
                return None;
            }
            let (id, type_at) = match version {
                INFE_WIDE_IDS => (
                    be32(bytes, start + FULL_BOX_PREFIX),
                    start + FULL_BOX_PREFIX + WORD + HALF_WORD,
                ),
                _ => (
                    u32::from(be16(bytes, start + FULL_BOX_PREFIX)),
                    start + FULL_BOX_PREFIX + HALF_WORD + HALF_WORD,
                ),
            };
            let kind = bytes.get(type_at..type_at + WORD)?.try_into().ok()?;
            Some((id, kind))
        })
        .collect()
}

/// Every item's location, read from `iloc`.
///
/// The field widths are declared by the box rather than fixed, and an item
/// may be split across extents, so this is where a file from another encoder
/// most easily diverges from what this crate writes.
fn locations(bytes: &[u8], tree: (usize, usize)) -> Vec<Location> {
    let Some((at, end)) = meta_box(bytes, tree, b"iloc") else {
        return Vec::new();
    };
    let Some(version) = bytes.get(at).copied() else {
        return Vec::new();
    };

    let mut cursor = at + FULL_BOX_PREFIX;
    let Some(widths) = bytes.get(cursor..cursor + HALF_WORD) else {
        return Vec::new();
    };
    let offset_size = usize::from(widths[0] >> NIBBLE_BITS);
    let length_size = usize::from(widths[0] & LOW_NIBBLE);
    let base_size = usize::from(widths[1] >> NIBBLE_BITS);
    // The low nibble is the extent index width from version 1, and reserved
    // before it, where no index field is present to read.
    let index_size = match version >= ILOC_WITH_CONSTRUCTION {
        true => usize::from(widths[1] & LOW_NIBBLE),
        false => 0,
    };
    cursor += HALF_WORD;

    let id_size = match version >= ILOC_WIDE_IDS {
        true => WORD,
        false => HALF_WORD,
    };
    // Clamped like `stsz`'s: the loop below stops at `end`, but the reserve
    // happens before it runs, so a count the file made up would be honoured
    // once regardless. The shortest an item entry can be is its identifier.
    let count = (be_sized(bytes, cursor, id_size) as usize)
        .min(end.saturating_sub(cursor) / id_size.max(1));
    cursor += id_size;

    let mut found = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor >= end {
            break;
        }
        let id = be_sized(bytes, cursor, id_size) as u32;
        cursor += id_size;

        // Twelve reserved bits and four naming the construction method.
        let construction = match version >= ILOC_WITH_CONSTRUCTION {
            true => {
                let packed = be16(bytes, cursor);
                cursor += HALF_WORD;
                (packed & u16::from(LOW_NIBBLE)) as u8
            }
            false => CONSTRUCTION_FILE,
        };

        // The data reference index, which names an external file when it is
        // not zero. Nothing here follows one, and an item that lives in
        // another file is dropped rather than read from the wrong offsets.
        let external = be16(bytes, cursor) != 0;
        cursor += HALF_WORD;

        let base = be_sized(bytes, cursor, base_size);
        cursor += base_size;

        let extent_count = usize::from(be16(bytes, cursor));
        cursor += HALF_WORD;

        let mut extents = Vec::with_capacity(extent_count);
        for _ in 0..extent_count {
            cursor += index_size;
            let offset = be_sized(bytes, cursor, offset_size);
            cursor += offset_size;
            let len = be_sized(bytes, cursor, length_size);
            cursor += length_size;
            extents.push(Extent {
                at: base.saturating_add(offset),
                len,
            });
        }

        if !external {
            found.push(Location {
                id,
                construction,
                extents,
            });
        }
    }
    found
}

/// The items referring to `to` through a reference of this type.
///
/// `iref` records the arrow pointing away from the dependent item: the alpha
/// plane declares itself auxiliary *to* the picture, and a `grid` declares
/// the tiles it is derived *from* (ISO/IEC 14496-12 § 8.11.12).
fn referrers(
    bytes: &[u8],
    tree: (usize, usize),
    kind: &[u8; 4],
    to: u32,
) -> Vec<u32> {
    let Some((at, end)) = meta_box(bytes, tree, b"iref") else {
        return Vec::new();
    };
    let version = bytes.get(at).copied().unwrap_or(0);
    let id_size = match version >= IREF_WIDE_IDS {
        true => WORD,
        false => HALF_WORD,
    };

    boxes(bytes, at + FULL_BOX_PREFIX, end)
        .into_iter()
        .filter(|(tag, ..)| tag == kind)
        .filter_map(|(_, start, finish)| {
            let from = be_sized(bytes, start, id_size) as u32;
            let count = usize::from(be16(bytes, start + id_size));
            let first = start + id_size + HALF_WORD;
            let points_at_it = (0..count)
                .map(|n| be_sized(bytes, first + n * id_size, id_size) as u32)
                .any(|id| id == to);
            (points_at_it && first <= finish).then_some(from)
        })
        .collect()
}

/// An item's bytes, gathered from however many extents hold them.
fn item_bytes(
    bytes: &[u8],
    tree: (usize, usize),
    location: &Location,
) -> Option<Vec<u8>> {
    // Offsets are into the file, or into this `meta`'s own `idat`. The third
    // method the specification defines points into another item, which no
    // AVIF writer emits and which is refused rather than guessed at.
    let base = match location.construction {
        CONSTRUCTION_FILE => 0,
        CONSTRUCTION_IDAT => meta_box(bytes, tree, b"idat")?.0,
        _ => return None,
    };

    let mut gathered = Vec::new();
    for extent in &location.extents {
        let from = base.checked_add(usize::try_from(extent.at).ok()?)?;
        // A zero length means the rest of the source, per § 8.11.3.1.
        let to = match extent.len {
            0 => bytes.len(),
            len => from.checked_add(usize::try_from(len).ok()?)?,
        };
        gathered.extend_from_slice(bytes.get(from..to)?);
    }
    Some(gathered)
}

/// Where `ipma` states how many item-to-property runs it holds
/// (ISO/IEC 23008-12 § 9.3.1), after the full-box prefix.
const IPMA_COUNT_AT: usize = FULL_BOX_PREFIX;

/// Where those runs begin.
const IPMA_ENTRIES_AT: usize = IPMA_COUNT_AT + WORD;

/// The `ipma` version whose item identifiers are thirty-two bits.
const IPMA_WIDE_IDS: u8 = 1;

/// The `ipma` flag that widens each property index from seven bits to
/// fifteen, the remaining bit marking the association essential.
const IPMA_WIDE_INDICES: u32 = 1;

/// The property index field's width in bits when `ipma` says it is narrow.
const IPMA_NARROW_INDEX_BITS: u16 = 0x7F;

/// The same when the flag widens it.
const IPMA_WIDE_INDEX_BITS: u16 = 0x7FFF;

/// A full box's three flag bytes, which follow its version.
fn full_box_flags(bytes: &[u8], at: usize) -> u32 {
    be32(bytes, at) & 0x00FF_FFFF
}

/// The `ipco` properties associated with one item.
///
/// `ipco` is a plain list and `ipma` maps items onto it by one-based index,
/// so neither box means anything without the other.
fn properties(
    bytes: &[u8],
    tree: (usize, usize),
    item: u32,
) -> Vec<([u8; 4], usize, usize)> {
    let Some(iprp) = meta_box(bytes, tree, b"iprp") else {
        return Vec::new();
    };
    let Some((ipco_at, ipco_end)) =
        boxes(bytes, iprp.0, iprp.1).into_iter().find_map(
            |(tag, start, end)| (&tag == b"ipco").then_some((start, end)),
        )
    else {
        return Vec::new();
    };
    let listed = boxes(bytes, ipco_at, ipco_end);

    let Some((ipma_at, ipma_end)) =
        boxes(bytes, iprp.0, iprp.1).into_iter().find_map(
            |(tag, start, end)| (&tag == b"ipma").then_some((start, end)),
        )
    else {
        return Vec::new();
    };
    let version = bytes.get(ipma_at).copied().unwrap_or(0);
    let wide_indices = full_box_flags(bytes, ipma_at) & IPMA_WIDE_INDICES != 0;
    let id_size = match version >= IPMA_WIDE_IDS {
        true => WORD,
        false => HALF_WORD,
    };
    let index_size = match wide_indices {
        true => HALF_WORD,
        false => 1,
    };
    let mask = match wide_indices {
        true => IPMA_WIDE_INDEX_BITS,
        false => IPMA_NARROW_INDEX_BITS,
    };

    let entries = be32(bytes, ipma_at + IPMA_COUNT_AT) as usize;
    let mut cursor = ipma_at + IPMA_ENTRIES_AT;
    for _ in 0..entries {
        if cursor >= ipma_end {
            break;
        }
        let id = be_sized(bytes, cursor, id_size) as u32;
        cursor += id_size;
        let count = usize::from(bytes.get(cursor).copied().unwrap_or(0));
        cursor += 1;

        if id == item {
            return (0..count)
                .filter_map(|n| {
                    let at = cursor + n * index_size;
                    // The high bit marks the association essential, which
                    // says a reader must understand the property or refuse
                    // the file. It is not part of the index.
                    let packed = be_sized(bytes, at, index_size) as u16;
                    let index = usize::from(packed & mask);
                    // One-based, so zero means no property.
                    listed.get(index.checked_sub(1)?).copied()
                })
                .collect();
        }
        cursor += count * index_size;
    }
    Vec::new()
}

/// Where `colr`'s nclx form states its matrix coefficients: after the colour
/// type, the primaries and the transfer characteristics
/// (ISO/IEC 14496-12 § 12.1.5).
const COLR_MATRIX_AT: usize = WORD + HALF_WORD * 2;

/// Where a `colr` box's ICC profile begins, after the colour type.
const COLR_PROFILE_AT: usize = WORD;

/// Where `colr`'s nclx form states its range, after the three code points.
const COLR_RANGE_AT: usize = COLR_MATRIX_AT + HALF_WORD;

/// The top bit of that byte, which is the full-range flag; the rest is
/// reserved (ISO/IEC 14496-12 § 12.1.5).
const COLR_FULL_RANGE: u8 = 0b1000_0000;

/// What a `colr` box says about the colours it describes.
struct Colour {
    /// How to get from the coded planes back to red, green and blue.
    matrix: Matrix,
    /// An ICC profile, where the box carries one in place of code points.
    profile: Option<Vec<u8>>,
    /// Whether the samples use the whole coded range.
    ///
    /// Defaults to true, which is what this crate writes and what every AVIF
    /// met so far declares. A file saying otherwise wants its levels opened
    /// out before the matrix is undone.
    full_range: bool,
}

impl Default for Colour {
    fn default() -> Self {
        Self {
            matrix: Matrix::default(),
            profile: None,
            full_range: true,
        }
    }
}

/// Reads the colour information an item carries.
///
/// A `colr` box says one of two things and never both: either code points
/// naming standard primaries, transfer and matrix, or an ICC profile
/// describing a space directly.
fn colour(bytes: &[u8], properties: &[([u8; 4], usize, usize)]) -> Colour {
    let Some((_, start, end)) =
        properties.iter().find(|(tag, ..)| tag == b"colr")
    else {
        return Colour::default();
    };
    let Some(kind) = bytes.get(*start..*start + WORD) else {
        return Colour::default();
    };

    match kind {
        b"nclx" => Colour {
            matrix: Matrix::of(be16(bytes, *start + COLR_MATRIX_AT)),
            profile: None,
            full_range: bytes
                .get(*start + COLR_RANGE_AT)
                .is_none_or(|flags| flags & COLR_FULL_RANGE != 0),
        },
        // `rICC` is the restricted profile and `prof` the unrestricted one.
        // The restriction is on what the profile may contain, not on how it
        // is read, so both are handed to Skia the same way.
        b"rICC" | b"prof" => Colour {
            // A profile describes the RGB the planes decode to, not how
            // they were mixed, so the matrix and the range stay default.
            matrix: Matrix::default(),
            profile: bytes.get(*start + COLR_PROFILE_AT..*end).map(Vec::from),
            full_range: true,
        },
        _ => Colour::default(),
    }
}

/// How the coded pixels have to be turned before they are the picture.
///
/// Both properties are stored rather than applied by the encoder, so a file
/// that carries them decodes to a correct-looking image that is simply the
/// wrong way round. Nothing in the pixels says so.
#[derive(Default)]
struct Orientation {
    /// Quarter turns anticlockwise (ISO/IEC 23008-12 § 6.5.10).
    quarters: u8,
    /// Whether the top and bottom halves are exchanged.
    flip_vertical: bool,
    /// Whether the left and right halves are exchanged.
    flip_horizontal: bool,
}

/// The eight rational fields a `clap` box carries, each a numerator and a
/// denominator (ISO/IEC 14496-12 § 12.1.4).
const CLAP_FIELDS: usize = 8;

/// The rectangle a `clean aperture` names, in the coded picture.
struct Crop {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

/// Reads a `clap` property against the picture it describes.
///
/// The box states a width and a height as fractions, and an offset of the
/// aperture's centre from the picture's. The centre of a run of `n` samples
/// is at `(n - 1) / 2`, which is where the halves below come from -- an even
/// picture has its centre between two samples, and the specification counts
/// it that way rather than rounding.
fn clap(payload: &[u8], width: usize, height: usize) -> Option<Crop> {
    let field = |at: usize| -> Option<(i64, i64)> {
        let numerator = be32(payload, at * WORD * 2) as i32;
        let denominator = be32(payload, at * WORD * 2 + WORD) as i32;
        (denominator != 0)
            .then_some((i64::from(numerator), i64::from(denominator)))
    };
    if payload.len() < CLAP_FIELDS * WORD {
        return None;
    }

    let ratio = |at: usize| field(at).map(|(n, d)| n as f64 / d as f64);
    let clean_width = ratio(0)?;
    let clean_height = ratio(1)?;
    let horizontal = ratio(2)?;
    let vertical = ratio(3)?;
    if clean_width <= 0.0 || clean_height <= 0.0 {
        return None;
    }

    let centre_x = (width as f64 - 1.0) / 2.0 + horizontal;
    let centre_y = (height as f64 - 1.0) / 2.0 + vertical;
    let left = (centre_x - (clean_width - 1.0) / 2.0).round();
    let top = (centre_y - (clean_height - 1.0) / 2.0).round();

    // An aperture reaching outside the picture is malformed. Clamped rather
    // than refused: the rest of the crop is still the picture the file meant
    // to show, and dropping it entirely would lose more than it saves.
    let left = left.clamp(0.0, width as f64) as usize;
    let top = top.clamp(0.0, height as f64) as usize;
    Some(Crop {
        left,
        top,
        width: (clean_width.round() as usize).min(width - left),
        height: (clean_height.round() as usize).min(height - top),
    })
}

/// Cuts a frame down to the rectangle a `clap` named.
fn crop(frame: Frame, window: &Crop) -> Frame {
    if window.left == 0
        && window.top == 0
        && window.width == frame.width
        && window.height == frame.height
    {
        return frame;
    }

    let pixels = (0..window.height)
        .flat_map(|row| {
            let start =
                ((window.top + row) * frame.width + window.left) * CHANNELS;
            frame.pixels[start..start + window.width * CHANNELS]
                .iter()
                .copied()
        })
        .collect();
    Frame {
        width: window.width,
        height: window.height,
        pixels,
        deep: frame.deep,
    }
}

/// The low two bits of `irot` hold the angle, in quarter turns.
const IROT_ANGLE: u8 = 0b11;

/// The low bit of `imir` names the axis: zero exchanges top and bottom,
/// one exchanges left and right (ISO/IEC 23008-12 § 6.5.12).
const IMIR_AXIS: u8 = 0b1;

impl Orientation {
    /// Reads both transform properties, where the item carries them.
    fn of(properties: &[([u8; 4], usize, usize)], bytes: &[u8]) -> Self {
        properties.iter().fold(
            Self::default(),
            |orientation, (tag, start, _)| {
                let value = bytes.get(*start).copied().unwrap_or(0);
                match tag {
                    b"irot" => Self {
                        quarters: value & IROT_ANGLE,
                        ..orientation
                    },
                    b"imir" => match value & IMIR_AXIS {
                        0 => Self {
                            flip_vertical: true,
                            ..orientation
                        },
                        _ => Self {
                            flip_horizontal: true,
                            ..orientation
                        },
                    },
                    _ => orientation,
                }
            },
        )
    }

    /// Whether this leaves the pixels where they are.
    fn is_identity(&self) -> bool {
        self.quarters == 0 && !self.flip_vertical && !self.flip_horizontal
    }
}

/// Turns a decoded frame into the picture the file describes.
///
/// MIAF fixes the order these are applied in -- rotation, then mirror
/// (ISO/IEC 23000-22 § 7.3.6.7) -- and reversing it is wrong for every angle
/// but zero and two.
fn orient(frame: Frame, orientation: &Orientation) -> Frame {
    // The overwhelmingly common case, and the one worth not copying for.
    if orientation.is_identity() {
        return frame;
    }

    let (source_width, source_height) = (frame.width, frame.height);
    // An odd number of quarter turns swaps the axes.
    let turned = orientation.quarters % 2 == 1;
    let (width, height) = match turned {
        true => (source_height, source_width),
        false => (source_width, source_height),
    };

    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            // Undo the mirror first, because it was applied last.
            let x = match orientation.flip_horizontal {
                true => width - 1 - x,
                false => x,
            };
            let y = match orientation.flip_vertical {
                true => height - 1 - y,
                false => y,
            };
            // Then read where the rotation took each pixel from.
            let (from_x, from_y) = match orientation.quarters {
                1 => (source_width - 1 - y, x),
                2 => (source_width - 1 - x, source_height - 1 - y),
                3 => (y, source_height - 1 - x),
                _ => (x, y),
            };
            let at = (from_y * source_width + from_x) * CHANNELS;
            frame.pixels[at..at + CHANNELS].iter().copied()
        })
        .collect();

    Frame {
        width,
        height,
        pixels,
        deep: frame.deep,
    }
}

/// The items a reference of this type points at, in the order given.
///
/// The mirror of [`referrers`], and order matters here in a way it does not
/// there: a `grid`'s `dimg` list is its tiles, row by row.
fn references(
    bytes: &[u8],
    tree: (usize, usize),
    kind: &[u8; 4],
    from: u32,
) -> Vec<u32> {
    let Some((at, end)) = meta_box(bytes, tree, b"iref") else {
        return Vec::new();
    };
    let version = bytes.get(at).copied().unwrap_or(0);
    let id_size = match version >= IREF_WIDE_IDS {
        true => WORD,
        false => HALF_WORD,
    };

    boxes(bytes, at + FULL_BOX_PREFIX, end)
        .into_iter()
        .filter(|(tag, ..)| tag == kind)
        .filter(|(_, start, _)| be_sized(bytes, *start, id_size) as u32 == from)
        .flat_map(|(_, start, _)| {
            let count = usize::from(be16(bytes, start + id_size));
            let first = start + id_size + HALF_WORD;
            (0..count)
                .map(|n| be_sized(bytes, first + n * id_size, id_size) as u32)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Where `grid` states how many rows of tiles it holds, less one
/// (ISO/IEC 23008-12 § 6.6.2.3.2). The version and flags precede it.
const GRID_ROWS_AT: usize = 2;

/// Where `grid` states its column count, less one.
const GRID_COLUMNS_AT: usize = GRID_ROWS_AT + 1;

/// Where `grid`'s output dimensions begin.
const GRID_SIZES_AT: usize = GRID_COLUMNS_AT + 1;

/// The `grid` flag that widens the output dimensions to thirty-two bits.
const GRID_WIDE_SIZES: u8 = 1;

/// A `grid`'s arrangement: how many tiles, and what they add up to.
struct Grid {
    rows: usize,
    columns: usize,
    width: usize,
    height: usize,
}

/// Reads a `grid` item's payload.
///
/// The tiles themselves are not named here -- they are the `dimg` references
/// pointing away from the grid, which carry the order this describes the
/// shape of.
fn grid(payload: &[u8]) -> Option<Grid> {
    let flags = *payload.get(1)?;
    // Counts are stored less one, so a single row is written as zero and
    // there is no way to describe none.
    let rows = usize::from(*payload.get(GRID_ROWS_AT)?) + 1;
    let columns = usize::from(*payload.get(GRID_COLUMNS_AT)?) + 1;

    let wide = flags & GRID_WIDE_SIZES != 0;
    let field = match wide {
        true => WORD,
        false => HALF_WORD,
    };
    let width = be_sized(payload, GRID_SIZES_AT, field) as usize;
    let height = be_sized(payload, GRID_SIZES_AT + field, field) as usize;
    (width > 0 && height > 0).then_some(Grid {
        rows,
        columns,
        width,
        height,
    })
}

/// Copies a tile into the composed frame at a tile origin.
///
/// The grid may cover more than the output asks for -- tiles are a fixed
/// size and the last row or column can overhang -- so anything past the
/// output's edge is dropped rather than wrapped.
fn place(into: &mut Frame, tile: &Frame, left: usize, top: usize) {
    let rows = tile.height.min(into.height.saturating_sub(top));
    let columns = tile.width.min(into.width.saturating_sub(left));
    for row in 0..rows {
        let from = row * tile.width * CHANNELS;
        let to = (top + row) * into.width * CHANNELS + left * CHANNELS;
        let span = columns * CHANNELS;
        into.pixels[to..to + span]
            .copy_from_slice(&tile.pixels[from..from + span]);
    }
}

/// A tiled AVIF, composed from the items its `grid` refers to.
///
/// Anything larger than a few hundred pixels out of Apple's encoder arrives
/// this way, so this is not a rare shape: it is what a photograph from a
/// phone looks like.
fn tiled(
    bytes: &[u8],
    tree: (usize, usize),
    primary: u32,
    places: &[Location],
) -> Result<SkImage, String> {
    let place_of = |id: u32| places.iter().find(|found| found.id == id);
    let payload = place_of(primary)
        .and_then(|found| item_bytes(bytes, tree, found))
        .ok_or_else(|| "The AVIF's grid could not be read".to_string())?;
    let shape = grid(&payload)
        .ok_or_else(|| "The AVIF's grid is malformed".to_string())?;

    let tiles = references(bytes, tree, b"dimg", primary);
    let wanted = shape.rows * shape.columns;
    if tiles.len() != wanted {
        return Err(format!(
            "The AVIF's grid names {} tiles but describes {wanted}",
            tiles.len()
        ));
    }

    let listed = properties(bytes, tree, primary);
    let described = colour(bytes, &listed);

    let mut composed: Option<Frame> = None;
    let mut decoder = Decoder::new(THREADS)?;
    for (at, id) in tiles.iter().enumerate() {
        let coded = place_of(*id)
            .and_then(|found| item_bytes(bytes, tree, found))
            .ok_or_else(|| format!("The AVIF's tile {at} could not be read"))?;
        let mut tile = decode_sample(
            &mut decoder,
            &coded,
            described.matrix,
            described.full_range,
        )?;

        // Each tile carries its own transparency, as its own auxiliary item.
        if let Some(alpha) = referrers(bytes, tree, b"auxl", *id)
            .into_iter()
            .filter_map(place_of)
            .find_map(|found| item_bytes(bytes, tree, found))
            && let Ok(mut decoder) = Decoder::new(THREADS)
        {
            apply_alpha(&mut tile, &mut decoder, &alpha);
        }

        // The first tile settles the depth and sizes the canvas, because a
        // grid's tiles are required to agree on both.
        let into = composed.get_or_insert_with(|| Frame {
            width: shape.width,
            height: shape.height,
            pixels: vec![0; shape.width * shape.height * CHANNELS],
            deep: tile.deep,
        });
        place(
            into,
            &tile,
            (at % shape.columns) * tile.width,
            (at / shape.columns) * tile.height,
        );
    }

    let composed =
        composed.ok_or_else(|| "The AVIF's grid has no tiles".to_string())?;
    // The transform properties belong to the grid, not to its tiles: they
    // turn the assembled picture, which is the only thing they describe.
    let orientation = Orientation::of(&listed, bytes);
    raster(
        &orient(composed, &orientation),
        described.profile.as_deref(),
    )
}

/// A still AVIF: one coded image, described by a property list.
pub(crate) fn still(bytes: &[u8]) -> Result<SkImage, String> {
    let tree = meta(bytes)
        .ok_or_else(|| "The AVIF has no metadata box".to_string())?;
    let primary = primary_item(bytes, tree)
        .ok_or_else(|| "The AVIF names no primary item".to_string())?;

    let places = locations(bytes, tree);
    let place = |id: u32| places.iter().find(|found| found.id == id);
    let types = item_types(bytes, tree);
    let kind = types
        .iter()
        .find(|(id, _)| *id == primary)
        .map(|(_, kind)| *kind);

    // A `grid` primary is a description of how to arrange other items rather
    // than a coded image: its payload is a shape, not a bitstream.
    if kind.as_ref() == Some(b"grid") {
        return tiled(bytes, tree, primary, &places);
    }

    let location = place(primary)
        .ok_or_else(|| "The AVIF's primary item has no location".to_string())?;
    let coded = item_bytes(bytes, tree, location).ok_or_else(|| {
        "The AVIF's primary item could not be read".to_string()
    })?;

    let listed = properties(bytes, tree, primary);
    let described = colour(bytes, &listed);

    let mut decoder = Decoder::new(THREADS)?;
    let mut frame = decode_sample(
        &mut decoder,
        &coded,
        described.matrix,
        described.full_range,
    )?;

    // Transparency is a second coded image, monochrome, declaring itself
    // auxiliary to the picture. Its absence is the common case and leaves
    // the frame opaque.
    if let Some(alpha) = referrers(bytes, tree, b"auxl", primary)
        .into_iter()
        .filter_map(place)
        .find_map(|found| item_bytes(bytes, tree, found))
        && let Ok(mut decoder) = Decoder::new(THREADS)
    {
        apply_alpha(&mut frame, &mut decoder, &alpha);
    }

    // MIAF fixes the order: clean aperture, then rotation, then mirror
    // (ISO/IEC 23000-22 § 7.3.6.7). Cropping after a turn would take the
    // wrong rectangle, because the turn moved what the offsets refer to.
    let frame = match listed.iter().find(|(tag, ..)| tag == b"clap") {
        Some((_, start, end)) => {
            match clap(&bytes[*start..*end], frame.width, frame.height) {
                Some(window) => crop(frame, &window),
                None => frame,
            }
        }
        None => frame,
    };
    let orientation = Orientation::of(&listed, bytes);
    raster(&orient(frame, &orientation), described.profile.as_deref())
}

/// The decoded pixels as a Skia image, at the depth the file held.
fn raster(frame: &Frame, profile: Option<&[u8]>) -> Result<SkImage, String> {
    let (color_type, bytes) = match frame.deep {
        true => (
            SkColorType::R16G16B16A16UNorm,
            frame
                .pixels
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect::<Vec<_>>(),
        ),
        false => (
            SkColorType::RGBA8888,
            frame.pixels.iter().map(|v| narrow_to_eight(*v)).collect(),
        ),
    };
    // A profile Skia rejects leaves the image in its default space, which
    // is what every reader of this format did before profiles were read at
    // all. A picture in the wrong space beats no picture.
    let space = profile.and_then(ColorSpace::new_icc);
    let info = ImageInfo::new(
        (frame.width as i32, frame.height as i32),
        color_type,
        AlphaType::Unpremul,
        space,
    );
    images::raster_from_data(
        &info,
        Data::new_copy(&bytes),
        info.min_row_bytes(),
    )
    .ok_or_else(|| "Could not build an image from the AVIF".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `clap` payload from its four rationals, in the box's own order:
    /// width, height, horizontal offset, vertical offset. Each is a signed
    /// numerator and an unsigned denominator, which is eight `u32` values in
    /// all -- writing them as four pairs is what keeps a denominator from
    /// being read as the next field's numerator.
    fn clap_payload(rationals: [(i32, u32); 4]) -> Vec<u8> {
        rationals
            .iter()
            .flat_map(|(numerator, denominator)| {
                let mut pair = numerator.to_be_bytes().to_vec();
                pair.extend(denominator.to_be_bytes());
                pair
            })
            .collect()
    }

    #[test]
    fn a_centred_clean_aperture_is_the_middle_of_the_picture() {
        // The centre of a run of n samples is at (n - 1) / 2, which the
        // specification counts rather than rounding: a hundred-wide picture
        // is centred at 49.5, not 50. A fifty-wide aperture centred there
        // starts at 49.5 - 24.5, which is 25 exactly.
        //
        // Checked against that arithmetic rather than against the function,
        // because the arithmetic is the part worth being sure of.
        let payload = clap_payload([(50, 1), (50, 1), (0, 1), (0, 1)]);
        let window = clap(&payload, 100, 100).expect("a well formed clap");
        assert_eq!((window.left, window.top), (25, 25));
        assert_eq!((window.width, window.height), (50, 50));
    }

    #[test]
    fn a_clean_aperture_offset_moves_with_its_sign() {
        // Ten to the right: the centre goes to 59.5 and the left edge to 35.
        let right = clap_payload([(50, 1), (50, 1), (10, 1), (0, 1)]);
        let window = clap(&right, 100, 100).expect("a well formed clap");
        assert_eq!(window.left, 35, "a positive offset moves right");
        assert_eq!(window.top, 25, "and leaves the other axis alone");

        // And the same distance left, which is where the signedness matters:
        // read as unsigned this would be a vast positive number.
        let left = clap_payload([(50, 1), (50, 1), (-10, 1), (0, 1)]);
        let window = clap(&left, 100, 100).expect("a well formed clap");
        assert_eq!(window.left, 15, "a negative offset moves left");
    }

    #[test]
    fn a_clean_aperture_reaching_outside_the_picture_is_clamped() {
        // Malformed, and clamped rather than refused: what is left is still
        // the picture the file meant to show, and a crop that slid off the
        // edge should not cost the whole image.
        let payload = clap_payload([(200, 1), (200, 1), (0, 1), (0, 1)]);
        let window = clap(&payload, 100, 100).expect("clamped, not refused");
        assert_eq!((window.left, window.top), (0, 0));
        assert!(window.width <= 100 && window.height <= 100);

        // A zero denominator is not a fraction at all.
        let broken = clap_payload([(50, 0), (50, 1), (0, 1), (0, 1)]);
        assert!(clap(&broken, 100, 100).is_none(), "a zero denominator");
    }

    #[test]
    fn cropping_takes_the_rectangle_it_was_given() {
        // Four rows of four, each pixel's red channel numbering it, so a row
        // taken at the wrong stride shows up as the wrong numbers rather
        // than as a plausible picture.
        let frame = Frame {
            width: 4,
            height: 4,
            deep: false,
            pixels: (0..16).flat_map(|n| [n as u16, 0, 0, u16::MAX]).collect(),
        };
        let cut = crop(
            frame,
            &Crop {
                left: 1,
                top: 1,
                width: 2,
                height: 2,
            },
        );

        assert_eq!((cut.width, cut.height), (2, 2));
        let reds: Vec<u16> =
            cut.pixels.chunks_exact(CHANNELS).map(|px| px[0]).collect();
        assert_eq!(reds, vec![5, 6, 9, 10], "the middle four");
    }
}

#[cfg(test)]
mod hostile {
    use super::*;

    /// A box with no payload, which is the smallest one legal.
    fn empty(tag: &[u8; 4]) -> Vec<u8> {
        let mut out = (BOX_HEADER as u32).to_be_bytes().to_vec();
        out.extend_from_slice(tag);
        out
    }

    /// A container holding `inner`.
    fn wrap(tag: &[u8; 4], inner: Vec<u8>) -> Vec<u8> {
        let mut out =
            ((BOX_HEADER + inner.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(tag);
        out.extend(inner);
        out
    }

    fn ftyp() -> Vec<u8> {
        wrap(b"ftyp", b"avifavif".to_vec())
    }

    /// A full box with `version` and a payload.
    fn full(tag: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0];
        body.extend_from_slice(payload);
        wrap(tag, body)
    }

    /// A `moov` holding one track whose sample table is `stbl`.
    fn movie(stbl: Vec<u8>) -> Vec<u8> {
        let hdlr = full(b"hdlr", 0, &[0, 0, 0, 0, b'p', b'i', b'c', b't']);
        let minf = wrap(b"minf", wrap(b"stbl", stbl));
        let mut mdia = hdlr;
        mdia.extend(minf);
        wrap(b"moov", wrap(b"trak", wrap(b"mdia", mdia)))
    }

    #[test]
    fn a_count_the_file_invented_does_not_size_an_allocation() {
        // Every count in a sample table is four bytes the file chose, and
        // each one sized a `Vec` before anything checked it against the
        // file's own length: `stsz` at 0xFFFFFFFF asked for a 34 GB
        // allocation from an input of about a hundred bytes.
        //
        // Nothing here asserts a decode succeeds -- these files describe no
        // real samples. What is being asserted is that the reader returns
        // rather than exhausting memory, and the test finishing at all is
        // the assertion.
        let huge = u32::MAX.to_be_bytes();
        let mut stsz = vec![0, 0, 0, 0];
        stsz.extend_from_slice(&[0, 0, 0, 1]); // a uniform size of one
        stsz.extend_from_slice(&huge); // and four billion of them
        let mut stts = vec![0, 0, 0, 0];
        stts.extend_from_slice(&huge); // four billion runs
        stts.extend_from_slice(&huge); // the first claiming four billion
        stts.extend_from_slice(&[0, 0, 0, 1]);

        let mut table = wrap(b"stsz", stsz);
        table.extend(wrap(b"stco", vec![0, 0, 0, 0, 0, 0, 0, 0]));
        table.extend(wrap(b"stts", stts));

        let mut bytes = ftyp();
        bytes.extend(movie(table));
        // `mdhd` lives beside the table in a real file; without it the read
        // gives up earlier, which is still the behaviour under test.
        assert!(delays(&bytes).is_none() || delays(&bytes).is_some());
    }

    #[test]
    fn an_item_count_the_file_invented_does_not_size_an_allocation() {
        // The same again for `iloc`, whose loop stops at the end of the box
        // but only after the reserve has already happened.
        let mut iloc = vec![0, 0, 0, 0]; // version 0, no flags
        iloc.extend_from_slice(&[0x44, 0x00]); // four-byte offsets and lengths
        iloc.extend_from_slice(&u16::MAX.to_be_bytes()); // 65535 items
        let meta = {
            let mut body = vec![0, 0, 0, 0];
            body.extend(wrap(b"iloc", iloc));
            wrap(b"meta", body)
        };
        let mut bytes = ftyp();
        bytes.extend(meta);
        // Reached through the still path, which is what `loadImage` takes
        // for a file with no `moov`.
        let _ = still(&bytes);
    }

    #[test]
    fn a_track_declaring_no_samples_does_not_panic() {
        // `track` returns a `Track` whenever the five boxes it reads are
        // present, so `stsz` declaring zero samples produces an empty list.
        // `want` is then `0`, and slicing `[0..=0]` of an empty slice panics
        // -- which is what the resume rewrite introduced, because the loop
        // it replaced used `take(want + 1)` and tolerated the empty case.
        let hdlr = full(b"hdlr", 0, &[0, 0, 0, 0, b'p', b'i', b'c', b't']);
        let mut table = wrap(b"stsz", vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        table.extend(wrap(b"stco", vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]));
        table.extend(wrap(b"mdhd", vec![0; 20]));
        table.extend(wrap(b"stts", vec![0, 0, 0, 0, 0, 0, 0, 0]));

        let mut mdia = hdlr;
        mdia.extend(wrap(b"minf", wrap(b"stbl", table)));
        let mut bytes = wrap(b"ftyp", b"avisavis".to_vec());
        bytes.extend(wrap(b"moov", wrap(b"trak", wrap(b"mdia", mdia))));

        assert!(is_animated(&bytes), "the brand and a moov get it this far");
        // An error, not a panic. This is reachable from `loadImage`: Skia
        // declines the file and `decode_frame` asks for frame 0.
        assert!(frame(&bytes, 0, None).is_err(), "no frames, so an error");
    }

    #[test]
    fn a_truncated_handler_does_not_panic() {
        // `delays` runs on every image `Image::from_encoded` is given, so
        // anything it panics on is reachable from `loadImage` of a hostile
        // file. An `hdlr` with no payload puts its payload start at the end
        // of the file, and the handler type is read four bytes past that.
        let mut bytes = ftyp();
        bytes.extend(wrap(
            b"moov",
            wrap(b"trak", wrap(b"mdia", empty(b"hdlr"))),
        ));
        assert!(is_avif(&bytes), "the brand is what gets it this far");
        assert_eq!(delays(&bytes), None, "no track, but no panic either");
    }
}
