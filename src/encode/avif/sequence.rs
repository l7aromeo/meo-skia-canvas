//! The ISOBMFF container an animated AVIF is, written here.
//!
//! `avif-serialize` writes the still form -- `ftyp`, `meta`, `mdat`, one
//! coded image described by a property list. An animation is a different
//! file: the frames become *samples of a video track*, and the boxes that
//! describe a track are the ones QuickTime defined in 1998 and MP4 inherited.
//! Nothing in the dependency tree writes them, so they are written here, the
//! same division of labour as the WebP muxer next door.
//!
//! # What the file has to contain
//!
//! ```text
//! ftyp   major brand `avis`, so a reader knows it animates before parsing
//! meta   the still image, for readers that show one frame and stop
//! moov   the movie: one video track, its timing, and where its samples are
//!   mvhd   duration of the whole movie -- and where the loop count hides
//!   trak
//!     tkhd   track dimensions
//!     edts   an edit list, whose flags say whether the movie repeats
//!     mdia
//!       mdhd   the timescale every duration below is counted in
//!       hdlr   `pict`, because this is pictures rather than video
//!       minf   sample tables: which byte range each frame occupies, how
//!              long it lasts, and which frames can be decoded alone
//! mdat   the coded frames themselves
//! ```
//!
//! # Two encodes of the first frame
//!
//! The `meta` box describes a still image and the track describes an
//! animation, and a decoder may read either. They cannot share a sample: the
//! still has to stand alone, while the track's first frame is a key frame
//! that later frames are coded against. A reference file written by libavif
//! shows the same thing -- its still item is 17288 bytes where the track's
//! first sample is 10170 -- so the duplication is the format's, not a
//! shortcut taken here.
//!
//! # Why the loop count is written as a duration
//!
//! ISOBMFF has no field for "play this five times". libavif spends the one
//! number it has: `mvhd`'s duration is set to the frame duration multiplied
//! by the number of plays, so a player that honours the movie duration stops
//! after the fifth. Playing forever is the absence of that -- the duration
//! covers one pass and the edit list's flag says to repeat. This follows
//! libavif, because a file only plays the way players expect if it is built
//! the way the files they were tested against are built.

/// The bytes of one ISOBMFF box, tag included.
///
/// Every box is a four-byte length, a four-character type and a payload, so
/// building one is the same three lines everywhere and this is those lines.
fn boxed(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    out
}

/// A full box: a version byte, three flag bytes, then the payload.
fn full_boxed(tag: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut head = Vec::with_capacity(4 + body.len());
    head.push(version);
    head.extend_from_slice(&flags.to_be_bytes()[1..]);
    head.extend_from_slice(body);
    boxed(tag, &head)
}

/// Several boxes end to end, for the containers that hold only other boxes.
fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// The timescale every duration in the track is counted in, in ticks a
/// second.
///
/// Ninety thousand is MPEG's own, and it is chosen for what divides it: 24,
/// 25, 30, 50 and 60 frames a second all land on whole numbers of ticks, so
/// the common rates are exact rather than drifting a tick a frame. A frame
/// duration in milliseconds converts as `ms * 90`, which is also whole.
pub(crate) const TIMESCALE: u32 = 90_000;

/// Ticks a millisecond, which is what [`TIMESCALE`] divided by a thousand
/// works out to.
const TICKS_PER_MS: u32 = TIMESCALE / 1000;

/// `iloc`'s size byte: four-byte offsets in the high nibble, four-byte
/// lengths in the low one.
///
/// The field packs two widths into one byte, and four each is what keeps a
/// file under 4 GB describable without the eight-byte forms.
const ILOC_FOUR_BYTE_SIZES: u8 = 0x44;

/// The resolution a visual sample entry declares, as 16.16 fixed point.
///
/// Seventy-two dots per inch, which is what every MP4 writes here whatever
/// the picture's real resolution: the field predates anything caring, and a
/// reader takes the display size from `tkhd` instead.
const SAMPLE_ENTRY_DPI: u32 = 72 << 16;

/// `depth` in a visual sample entry: twenty-four, meaning colour without
/// alpha.
///
/// A fixed part of the sample entry rather than a description of these
/// pixels -- the real depth is in `av1C`, and the alpha is a track of its
/// own. QuickTime defined this field when 24-bit colour was the question.
const SAMPLE_ENTRY_DEPTH: u16 = 24;

/// The `colour table` field of a visual sample entry, which means "none".
const NO_COLOUR_TABLE: i16 = -1;

/// `und`, the language `mdhd` states when nothing set one.
///
/// Three letters packed five bits each into sixteen, which is how ISOBMFF
/// spells a language code -- so `und` is not the ASCII but this.
const LANGUAGE_UNDETERMINED: u16 = 0x55C4;

/// The `elst` flag saying the edit repeats rather than playing once.
///
/// libavif sets the low flag bit on the edit list of a looping file and
/// leaves it clear otherwise, and players follow it.
const ELST_REPEATS: u32 = 1;

/// A frame's duration in track ticks.
pub(crate) fn ticks(delay_ms: u32) -> u32 {
    delay_ms.saturating_mul(TICKS_PER_MS)
}

/// One coded frame, and what the sample tables need to say about it.
#[derive(Clone)]
pub(crate) struct Sample {
    /// The AV1 bytes as rav1e produced them.
    pub data: Vec<u8>,
    /// How long it is shown, in ticks.
    pub duration: u32,
    /// Whether it can be decoded without any frame before it.
    pub sync: bool,
}

/// The alpha channel, as its own coded track.
///
/// AVIF has no way to put alpha in the same samples as colour: the channel
/// is a second monochrome AV1 image, and in an animation that means a second
/// track, marked auxiliary and pointed at the colour track by a `tref` of
/// type `auxl`. A reader that does not understand auxiliary tracks ignores
/// it and shows the animation opaque, which is the graceful half of why the
/// format is built this way.
pub(crate) struct Alpha<'a> {
    /// The still's alpha, for the `meta` view.
    pub still: &'a [u8],
    /// The alpha track's own `av1C`.
    pub config: &'a [u8],
    /// One monochrome sample per colour sample.
    pub samples: Vec<Sample>,
}

/// The URN that says an auxiliary image is alpha.
///
/// From ISO/IEC 23091-2 by way of the AVIF specification, and written into
/// both the still item's `auxC` property and the alpha track's `auxi` box.
/// The trailing NUL is part of it: both boxes carry a null-terminated
/// string.
const ALPHA_URN: &[u8] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0";

/// Everything the container needs that is not a sample.
pub(crate) struct Movie<'a> {
    pub width: u32,
    pub height: u32,
    /// Bits a channel, for the `av1C` the sample description carries.
    pub bits: u8,
    /// The still image, coded on its own -- see the module note.
    pub still: &'a [u8],
    /// `None` plays forever.
    pub loops: Option<u32>,
    /// The AV1 sequence header and configuration, as `av1C` records it.
    pub config: &'a [u8],
    /// The alpha channel, where anything is transparent.
    pub alpha: Option<Alpha<'a>>,
}

/// The finished file.
pub(crate) fn write(movie: &Movie<'_>, samples: &[Sample]) -> Vec<u8> {
    let total: u32 = samples.iter().map(|s| s.duration).sum();

    // Every sample offset in `stco` is absolute, so the tables have to know
    // where `mdat` begins -- which depends on how large the tables are. The
    // dependency is circular and the way out is to build them once against a
    // guess and once against the answer, since only the offsets change and
    // never their width.
    let mut mdat_at = 0u32;
    let mut file = Vec::new();
    for _ in 0..2 {
        let ftyp = boxed(b"ftyp", &brands());
        let meta = still_item(movie, mdat_at);
        let moov = movie_box(movie, samples, total, mdat_at);

        // Order inside `mdat`: the two stills the `meta` view points at,
        // then the colour samples, then the alpha samples. Each run is
        // contiguous so one `stco` entry describes each track.
        let mut mdat_body = Vec::new();
        mdat_body.extend_from_slice(movie.still);
        if let Some(alpha) = &movie.alpha {
            mdat_body.extend_from_slice(alpha.still);
        }
        for sample in samples {
            mdat_body.extend_from_slice(&sample.data);
        }
        if let Some(alpha) = &movie.alpha {
            for sample in &alpha.samples {
                mdat_body.extend_from_slice(&sample.data);
            }
        }

        file = concat(&[ftyp, meta, moov, boxed(b"mdat", &mdat_body)]);
        // Where the payload of `mdat` starts: the file up to that box, plus
        // its own header.
        mdat_at = (file.len() - mdat_body.len()) as u32;
    }
    file
}

/// The brands an animated AVIF declares.
///
/// `avis` leads, which is what tells a reader this animates before it has
/// parsed anything. `avif` follows so a still-only reader recognises the
/// file at all, then the two MIAF brands the specification requires.
fn brands() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"avis");
    out.extend_from_slice(&0u32.to_be_bytes()); // minor version
    for brand in [b"avis", b"avif", b"msf1", b"miaf", b"MA1B"] {
        out.extend_from_slice(brand);
    }
    out
}

/// The `meta` box: the still image, as a one-item file would carry it.
///
/// A reader that knows nothing about tracks finds a complete AVIF here and
/// shows it. The item's extent points into `mdat`, where the still is
/// written before the samples, so both views share one payload box.
fn still_item(movie: &Movie<'_>, mdat_at: u32) -> Vec<u8> {
    const ITEM: u16 = 1;

    let hdlr = full_boxed(b"hdlr", 0, 0, &{
        let mut b = vec![0u8; 4];
        b.extend_from_slice(b"pict");
        b.extend_from_slice(&[0u8; 12]);
        b.push(0); // an empty name
        b
    });
    let pitm = full_boxed(b"pitm", 0, 0, &ITEM.to_be_bytes());

    const ALPHA_ITEM: u16 = 2;
    let alpha = movie.alpha.as_ref();

    // One extent per item, naming where it sits and how long it is.
    let iloc = full_boxed(b"iloc", 0, 0, &{
        let mut b = Vec::new();
        b.push(ILOC_FOUR_BYTE_SIZES);
        b.push(0x00); // no base offset, no index
        let items = 1 + u16::from(alpha.is_some());
        b.extend_from_slice(&items.to_be_bytes());

        let extent = |item: u16, at: u32, len: u32, b: &mut Vec<u8>| {
            b.extend_from_slice(&item.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes()); // data reference index
            b.extend_from_slice(&1u16.to_be_bytes()); // one extent
            b.extend_from_slice(&at.to_be_bytes());
            b.extend_from_slice(&len.to_be_bytes());
        };
        extent(ITEM, mdat_at, movie.still.len() as u32, &mut b);
        if let Some(alpha) = alpha {
            extent(
                ALPHA_ITEM,
                mdat_at + movie.still.len() as u32,
                alpha.still.len() as u32,
                &mut b,
            );
        }
        b
    });

    let infe = full_boxed(b"infe", 2, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&ITEM.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // protection
        b.extend_from_slice(b"av01");
        b.extend_from_slice(b"Image\0");
        b
    });
    let infe_alpha = full_boxed(b"infe", 2, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&ALPHA_ITEM.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(b"av01");
        b.extend_from_slice(b"Alpha\0");
        b
    });
    let iinf = full_boxed(b"iinf", 0, 0, &{
        let count = 1 + u16::from(alpha.is_some());
        let mut b = count.to_be_bytes().to_vec();
        b.extend_from_slice(&infe);
        if alpha.is_some() {
            b.extend_from_slice(&infe_alpha);
        }
        b
    });

    // What ties the alpha item to the colour one, the still counterpart of
    // the track's `tref`.
    let iref = full_boxed(
        b"iref",
        0,
        0,
        &boxed(b"auxl", &{
            let mut b = ALPHA_ITEM.to_be_bytes().to_vec();
            b.extend_from_slice(&1u16.to_be_bytes()); // one reference
            b.extend_from_slice(&ITEM.to_be_bytes());
            b
        }),
    );

    // The properties the still needs: its size, its depth, and the AV1
    // configuration that decodes it.
    let ispe = full_boxed(b"ispe", 0, 0, &{
        let mut b = movie.width.to_be_bytes().to_vec();
        b.extend_from_slice(&movie.height.to_be_bytes());
        b
    });
    let pixi = full_boxed(b"pixi", 0, 0, &{
        let mut b = vec![3u8];
        b.extend_from_slice(&[movie.bits; 3]);
        b
    });
    let av1c = boxed(b"av1C", movie.config);
    let mut properties = vec![ispe, pixi, av1c];
    if let Some(alpha) = alpha {
        // The alpha item's own configuration, its one-channel depth, and
        // the `auxC` that says which kind of auxiliary image it is.
        properties.push(boxed(b"av1C", alpha.config));
        properties.push(full_boxed(b"pixi", 0, 0, &[1u8, movie.bits]));
        properties.push(full_boxed(b"auxC", 0, 0, ALPHA_URN));
    }
    let ipco = boxed(b"ipco", &concat(&properties));

    // Each property is essential, so a reader that does not understand one
    // must refuse the image rather than show it wrong.
    let ipma = full_boxed(b"ipma", 0, 0, &{
        let entries = 1 + u32::from(alpha.is_some());
        let mut b = entries.to_be_bytes().to_vec();
        // The colour item takes the size, depth and configuration.
        b.extend_from_slice(&ITEM.to_be_bytes());
        b.push(3);
        for index in 1u8..=3 {
            b.push(0x80 | index);
        }
        if alpha.is_some() {
            // The alpha item shares the size and takes the three that
            // follow it in `ipco`.
            b.extend_from_slice(&ALPHA_ITEM.to_be_bytes());
            b.push(4);
            for index in [1u8, 4, 5, 6] {
                b.push(0x80 | index);
            }
        }
        b
    });
    let iprp = boxed(b"iprp", &concat(&[ipco, ipma]));

    let mut parts = vec![hdlr, pitm, iloc, iinf];
    if alpha.is_some() {
        parts.push(iref);
    }
    parts.push(iprp);
    full_boxed(b"meta", 0, 0, &concat(&parts))
}

/// A fixed-point 16.16 one, which several of the boxes below want.
const FIXED_ONE: u32 = 1 << 16;

/// The identity 3x3 matrix ISOBMFF writes into `mvhd` and `tkhd`.
///
/// Nine 16.16 values, except the last which is 2.30 -- so the diagonal is
/// one, one, and a quarter of `1 << 32`. Written out because the format
/// requires it present, not because anything here transforms.
const UNITY_MATRIX: [u32; 9] =
    [FIXED_ONE, 0, 0, 0, FIXED_ONE, 0, 0, 0, 1 << 30];

/// The whole `moov` box.
fn movie_box(
    movie: &Movie<'_>,
    samples: &[Sample],
    total: u32,
    mdat_at: u32,
) -> Vec<u8> {
    const TRACK: u32 = 1;

    // The movie's duration is where the loop count lives -- see the module
    // note. One pass for a file that repeats forever, and as many passes as
    // it plays for one that stops.
    let movie_duration = match movie.loops {
        None | Some(0) => u64::from(total),
        Some(count) => u64::from(total) * u64::from(count),
    };

    let mvhd = full_boxed(b"mvhd", 0, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes()); // created
        b.extend_from_slice(&0u32.to_be_bytes()); // modified
        b.extend_from_slice(&TIMESCALE.to_be_bytes());
        b.extend_from_slice(
            &(movie_duration.min(u64::from(u32::MAX)) as u32).to_be_bytes(),
        );
        b.extend_from_slice(&FIXED_ONE.to_be_bytes()); // rate 1.0
        b.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        b.extend_from_slice(&[0u8; 10]); // reserved
        for value in UNITY_MATRIX {
            b.extend_from_slice(&value.to_be_bytes());
        }
        b.extend_from_slice(&[0u8; 24]); // predefined
        let tracks = 1 + u32::from(movie.alpha.is_some());
        b.extend_from_slice(&(TRACK + tracks).to_be_bytes()); // next track id
        b
    });

    let tkhd = full_boxed(b"tkhd", 0, 0b111, &{
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes()); // created
        b.extend_from_slice(&0u32.to_be_bytes()); // modified
        b.extend_from_slice(&TRACK.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // reserved
        b.extend_from_slice(&total.to_be_bytes());
        b.extend_from_slice(&[0u8; 8]); // reserved
        b.extend_from_slice(&0u16.to_be_bytes()); // layer
        b.extend_from_slice(&0u16.to_be_bytes()); // alternate group
        b.extend_from_slice(&0u16.to_be_bytes()); // volume, silent
        b.extend_from_slice(&0u16.to_be_bytes()); // reserved
        for value in UNITY_MATRIX {
            b.extend_from_slice(&value.to_be_bytes());
        }
        // Width and height as 16.16, which is why they are shifted.
        b.extend_from_slice(&(movie.width << 16).to_be_bytes());
        b.extend_from_slice(&(movie.height << 16).to_be_bytes());
        b
    });

    // The edit list, whose flag is the other half of the loop count.
    let repeats = matches!(movie.loops, None | Some(0));
    let elst =
        full_boxed(b"elst", 0, if repeats { ELST_REPEATS } else { 0 }, &{
            let mut b = 1u32.to_be_bytes().to_vec(); // one edit
            b.extend_from_slice(&total.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes()); // from the start
            b.extend_from_slice(&FIXED_ONE.to_be_bytes()); // at rate 1.0
            b
        });
    let edts = boxed(b"edts", &elst);

    let mdhd = full_boxed(b"mdhd", 0, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes()); // created
        b.extend_from_slice(&0u32.to_be_bytes()); // modified
        b.extend_from_slice(&TIMESCALE.to_be_bytes());
        b.extend_from_slice(&total.to_be_bytes());
        b.extend_from_slice(&LANGUAGE_UNDETERMINED.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // predefined
        b
    });

    let hdlr = full_boxed(b"hdlr", 0, 0, &{
        let mut b = vec![0u8; 4];
        // `pict`, not `vide`: the specification says an image sequence is
        // pictures, and a reader keying off this is what decides whether the
        // file is offered as an image or as a video.
        b.extend_from_slice(b"pict");
        b.extend_from_slice(&[0u8; 12]);
        b.push(0);
        b
    });

    let vmhd = full_boxed(b"vmhd", 0, 1, &[0u8; 8]);
    let dref = full_boxed(b"dref", 0, 0, &{
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&full_boxed(b"url ", 0, 1, &[]));
        b
    });
    let dinf = boxed(b"dinf", &dref);

    let colour = colour_at(movie, mdat_at);
    let stbl = sample_tables(movie, samples, movie.config, colour, false);
    let minf = boxed(b"minf", &concat(&[vmhd.clone(), dinf.clone(), stbl]));
    let mdia = boxed(b"mdia", &concat(&[mdhd.clone(), hdlr, minf]));
    let trak = boxed(b"trak", &concat(&[tkhd, edts.clone(), mdia]));

    // The alpha track, where there is one: the same shape, marked auxiliary
    // and pointed back at the colour track.
    let alpha_trak = movie.alpha.as_ref().map(|alpha| {
        let after: u32 = samples.iter().map(|s| s.data.len() as u32).sum();
        let stbl = sample_tables(
            movie,
            &alpha.samples,
            alpha.config,
            colour + after,
            true,
        );
        let minf = boxed(b"minf", &concat(&[vmhd.clone(), dinf.clone(), stbl]));
        // `auxv` rather than `pict`: this track is not a picture to show,
        // it is a channel of one.
        let hdlr = full_boxed(b"hdlr", 0, 0, &{
            let mut b = vec![0u8; 4];
            b.extend_from_slice(b"auxv");
            b.extend_from_slice(&[0u8; 12]);
            b.push(0);
            b
        });
        let mdia = boxed(b"mdia", &concat(&[mdhd.clone(), hdlr, minf]));
        // The reference that ties it to the colour track. Without it the
        // file has two tracks and nothing saying they belong together.
        let tref = boxed(b"tref", &boxed(b"auxl", &TRACK.to_be_bytes()));
        let tkhd = full_boxed(b"tkhd", 0, 0b111, &{
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&(TRACK + 1).to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&total.to_be_bytes());
            b.extend_from_slice(&[0u8; 8]);
            b.extend_from_slice(&0u16.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
            for value in UNITY_MATRIX {
                b.extend_from_slice(&value.to_be_bytes());
            }
            b.extend_from_slice(&(movie.width << 16).to_be_bytes());
            b.extend_from_slice(&(movie.height << 16).to_be_bytes());
            b
        });
        boxed(b"trak", &concat(&[tkhd, tref, edts.clone(), mdia]))
    });

    let mut parts = vec![mvhd, trak];
    parts.extend(alpha_trak);
    boxed(b"moov", &concat(&parts))
}

/// Where the colour samples begin: after both stills.
fn colour_at(movie: &Movie<'_>, mdat_at: u32) -> u32 {
    mdat_at
        + movie.still.len() as u32
        + movie.alpha.as_ref().map_or(0, |a| a.still.len() as u32)
}

/// The five tables that say where each sample is, how long it lasts, and
/// which ones stand alone.
///
/// `config` and `first` differ per track, which is why they are arguments
/// rather than read off the movie: the alpha track has its own `av1C` and
/// its samples sit further into `mdat`.
fn sample_tables(
    movie: &Movie<'_>,
    samples: &[Sample],
    config: &[u8],
    first: u32,
    alpha: bool,
) -> Vec<u8> {
    // The sample description: one entry, an `av01` visual sample entry with
    // the same `av1C` the still item carries.
    let av01 = boxed(b"av01", &{
        let mut b = vec![0u8; 6]; // reserved
        b.extend_from_slice(&1u16.to_be_bytes()); // data reference index
        b.extend_from_slice(&[0u8; 16]); // predefined and reserved
        b.extend_from_slice(&(movie.width as u16).to_be_bytes());
        b.extend_from_slice(&(movie.height as u16).to_be_bytes());
        b.extend_from_slice(&SAMPLE_ENTRY_DPI.to_be_bytes());
        b.extend_from_slice(&SAMPLE_ENTRY_DPI.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // reserved
        b.extend_from_slice(&1u16.to_be_bytes()); // one frame a sample
        b.extend_from_slice(&[0u8; 32]); // compressor name
        b.extend_from_slice(&SAMPLE_ENTRY_DEPTH.to_be_bytes());
        b.extend_from_slice(&NO_COLOUR_TABLE.to_be_bytes());
        b.extend_from_slice(&boxed(b"av1C", config));
        // An auxiliary track says what kind it is, here inside the sample
        // entry. Without it a reader has a monochrome track and no reason
        // to treat it as transparency.
        if alpha {
            b.extend_from_slice(&full_boxed(b"auxi", 0, 0, ALPHA_URN));
        }
        b
    });
    let stsd = full_boxed(b"stsd", 0, 0, &{
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&av01);
        b
    });

    // Durations, run-length encoded: a fixed frame rate is one entry.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for sample in samples {
        match runs.last_mut() {
            Some((count, duration)) if *duration == sample.duration => {
                *count += 1;
            }
            _ => runs.push((1, sample.duration)),
        }
    }
    let stts = full_boxed(b"stts", 0, 0, &{
        let mut b = (runs.len() as u32).to_be_bytes().to_vec();
        for (count, duration) in &runs {
            b.extend_from_slice(&count.to_be_bytes());
            b.extend_from_slice(&duration.to_be_bytes());
        }
        b
    });

    // Which samples can be decoded on their own. Every frame in an
    // all-key-frame file, one in a coded sequence.
    let sync: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.sync)
        .map(|(at, _)| at as u32 + 1)
        .collect();
    let stss = full_boxed(b"stss", 0, 0, &{
        let mut b = (sync.len() as u32).to_be_bytes().to_vec();
        for at in &sync {
            b.extend_from_slice(&at.to_be_bytes());
        }
        b
    });

    // One chunk holding every sample, so the mapping is a single entry and
    // the offset table has one row.
    let stsc = full_boxed(b"stsc", 0, 0, &{
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&1u32.to_be_bytes()); // first chunk
        b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes()); // description index
        b
    });
    let stsz = full_boxed(b"stsz", 0, 0, &{
        let mut b = 0u32.to_be_bytes().to_vec(); // sizes differ, so a table
        b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for sample in samples {
            b.extend_from_slice(&(sample.data.len() as u32).to_be_bytes());
        }
        b
    });
    let stco = full_boxed(b"stco", 0, 0, &{
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&first.to_be_bytes());
        b
    });

    boxed(b"stbl", &concat(&[stsd, stts, stss, stsc, stsz, stco]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The top-level box names, in order.
    fn boxes(bytes: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = 0usize;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes(
                bytes[at..at + 4].try_into().expect("four bytes"),
            ) as usize;
            if size < 8 {
                break;
            }
            names.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into());
            at += size;
        }
        names
    }

    /// The payload of the first box with this tag, header stripped.
    fn find<'a>(bytes: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
        let at = bytes.windows(4).position(|w| w == tag)?;
        let start = at - 4;
        let size = u32::from_be_bytes(
            bytes[start..start + 4].try_into().expect("four bytes"),
        ) as usize;
        Some(&bytes[at + 4..start + size])
    }

    fn sample(len: usize, duration: u32, sync: bool) -> Sample {
        Sample {
            data: vec![7u8; len],
            duration,
            sync,
        }
    }

    fn movie<'a>(still: &'a [u8], config: &'a [u8]) -> Movie<'a> {
        Movie {
            width: 64,
            height: 48,
            bits: 10,
            still,
            loops: None,
            config,
            alpha: None,
        }
    }

    #[test]
    fn the_file_declares_itself_an_animation() {
        let file = write(
            &movie(&[1, 2, 3], &[0x81, 0x00, 0x00, 0x00]),
            &[sample(10, 3600, true), sample(20, 3600, false)],
        );
        assert_eq!(&file[4..8], b"ftyp");
        // `avis` first: a reader knows the file animates before parsing it,
        // where `avif` alone would say a still image.
        assert_eq!(&file[8..12], b"avis");
        assert_eq!(
            boxes(&file),
            vec!["ftyp", "meta", "moov", "mdat"],
            "the layout an animated AVIF has"
        );
    }

    #[test]
    fn every_sample_is_described_once_and_found_where_it_says() {
        // The tables are the file: a size table that disagrees with the
        // offsets produces a decoder reading one frame's bytes as another's,
        // which is not an error anywhere -- just a broken picture.
        let samples = vec![
            sample(11, 3600, true),
            sample(22, 3600, false),
            sample(33, 3600, false),
        ];
        let still = vec![9u8; 5];
        let file = write(&movie(&still, &[0x81, 0, 0, 0]), &samples);

        let stsz = find(&file, b"stsz").expect("a size table");
        // version and flags, the uniform size, then the count.
        assert_eq!(u32::from_be_bytes(stsz[4..8].try_into().unwrap()), 0);
        assert_eq!(u32::from_be_bytes(stsz[8..12].try_into().unwrap()), 3);
        let sizes: Vec<u32> = (0..3)
            .map(|i| {
                let at = 12 + i * 4;
                u32::from_be_bytes(stsz[at..at + 4].try_into().unwrap())
            })
            .collect();
        assert_eq!(sizes, vec![11, 22, 33]);

        // The offset table names one chunk, and it must land on the first
        // sample -- which sits after the still inside `mdat`.
        let stco = find(&file, b"stco").expect("an offset table");
        let first =
            u32::from_be_bytes(stco[8..12].try_into().unwrap()) as usize;
        assert_eq!(
            &file[first..first + 11],
            &samples[0].data[..],
            "the offset points at the first sample"
        );
        let second = first + 11;
        assert_eq!(&file[second..second + 22], &samples[1].data[..]);
    }

    #[test]
    fn the_durations_are_run_length_encoded() {
        // A fixed rate is one entry however many frames there are, and a
        // varying one is an entry per run. Both have to be read the same way.
        let fixed = write(
            &movie(&[1], &[0x81, 0, 0, 0]),
            &[
                sample(1, 3600, true),
                sample(1, 3600, false),
                sample(1, 3600, false),
            ],
        );
        let stts = find(&fixed, b"stts").expect("a time table");
        assert_eq!(
            u32::from_be_bytes(stts[4..8].try_into().unwrap()),
            1,
            "one run for a fixed rate"
        );
        assert_eq!(u32::from_be_bytes(stts[8..12].try_into().unwrap()), 3);
        assert_eq!(u32::from_be_bytes(stts[12..16].try_into().unwrap()), 3600);

        let varied = write(
            &movie(&[1], &[0x81, 0, 0, 0]),
            &[
                sample(1, 900, true),
                sample(1, 900, false),
                sample(1, 4500, false),
            ],
        );
        let stts = find(&varied, b"stts").expect("a time table");
        assert_eq!(u32::from_be_bytes(stts[4..8].try_into().unwrap()), 2);
    }

    #[test]
    fn only_the_frames_that_stand_alone_are_listed_as_sync() {
        // `stss` is how a player knows where it may seek to. Listing every
        // frame would send it to one that cannot be decoded on its own.
        let file = write(
            &movie(&[1], &[0x81, 0, 0, 0]),
            &[
                sample(1, 3600, true),
                sample(1, 3600, false),
                sample(1, 3600, false),
                sample(1, 3600, true),
            ],
        );
        let stss = find(&file, b"stss").expect("a sync table");
        assert_eq!(u32::from_be_bytes(stss[4..8].try_into().unwrap()), 2);
        // One-based sample numbers, which is what trips a reader written
        // against zero-based tables everywhere else in the file.
        assert_eq!(u32::from_be_bytes(stss[8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(stss[12..16].try_into().unwrap()), 4);
    }

    #[test]
    fn the_loop_count_is_spent_on_the_movie_duration() {
        // ISOBMFF has no loop field, so libavif multiplies the movie
        // duration by the number of plays and flags the edit list when it
        // repeats. Both halves have to agree or a player picks the wrong one.
        let samples = vec![sample(1, 3600, true), sample(1, 3600, false)];
        let track_total = 7200u32;

        let forever = write(&movie(&[1], &[0x81, 0, 0, 0]), &samples);
        let mvhd = find(&forever, b"mvhd").expect("a movie header");
        assert_eq!(
            u32::from_be_bytes(mvhd[16..20].try_into().unwrap()),
            track_total,
            "one pass, and the edit list says to repeat it"
        );
        let elst = find(&forever, b"elst").expect("an edit list");
        assert_eq!(elst[3] & 1, 1, "the repeat flag");

        let thrice = write(
            &Movie {
                loops: Some(3),
                ..movie(&[1], &[0x81, 0, 0, 0])
            },
            &samples,
        );
        let mvhd = find(&thrice, b"mvhd").expect("a movie header");
        assert_eq!(
            u32::from_be_bytes(mvhd[16..20].try_into().unwrap()),
            track_total * 3,
            "three passes"
        );
        let elst = find(&thrice, b"elst").expect("an edit list");
        assert_eq!(elst[3] & 1, 0, "and no repeat");
    }

    #[test]
    fn alpha_arrives_as_a_second_track_tied_to_the_first() {
        let colour = vec![sample(10, 3600, true), sample(10, 3600, false)];
        let alpha_samples = vec![sample(4, 3600, true), sample(4, 3600, false)];
        let file = write(
            &Movie {
                alpha: Some(Alpha {
                    still: &[5, 5],
                    config: &[0x81, 0, 0, 0],
                    samples: alpha_samples,
                }),
                ..movie(&[1, 2, 3], &[0x81, 0, 0, 0])
            },
            &colour,
        );

        // Two tracks, and the second says what it is and what it belongs to.
        assert_eq!(
            file.windows(4).filter(|w| *w == b"trak").count(),
            2,
            "a track for colour and a track for alpha"
        );
        assert!(
            file.windows(4).any(|w| w == b"auxv"),
            "the alpha track's handler marks it auxiliary"
        );
        assert!(
            file.windows(4).any(|w| w == b"auxl"),
            "and a reference ties it to the colour track"
        );
        assert!(
            file.windows(ALPHA_URN.len()).any(|w| w == ALPHA_URN),
            "named as alpha rather than some other auxiliary kind"
        );

        // Without alpha, none of that appears -- an opaque animation should
        // not carry an empty second track.
        let opaque = write(&movie(&[1, 2, 3], &[0x81, 0, 0, 0]), &colour);
        assert_eq!(opaque.windows(4).filter(|w| *w == b"trak").count(), 1);
        assert!(!opaque.windows(4).any(|w| w == b"auxl"));
    }

    #[test]
    fn a_millisecond_is_a_whole_number_of_ticks_at_every_common_rate() {
        // The reason the timescale is 90000: the rates people actually use
        // divide it exactly, so a frame is a whole number of ticks and the
        // animation does not drift a tick a frame.
        for fps in [24u32, 25, 30, 50, 60] {
            assert_eq!(
                TIMESCALE % fps,
                0,
                "{fps}fps should divide the timescale"
            );
        }
        assert_eq!(ticks(40), 3600, "40ms is a 25fps frame");
        assert_eq!(ticks(1000), TIMESCALE, "a second is the timescale");
    }
}
