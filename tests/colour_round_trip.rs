//! Every format that can be read back keeps the canvas's colour space.
//!
//! Four defects of one shape arrived within a week of each other: a file
//! states its colour space and the tag is lost somewhere between writing and
//! reading. Each was found by hand, by somebody who happened to look, and
//! nothing in the suite would have caught any of them.
//!
//! # The probe
//!
//! Paint a canvas in some space, export it, decode it, draw it onto an sRGB
//! canvas and read the pixel. A tag that survives converts the colour; a tag
//! that is lost passes the stored bytes through as though they were already
//! sRGB. Those two answers are computed rather than written down --
//! `honoured` is the same canvas exported straight to sRGB, `lost` is its own
//! bytes read back in its own space -- so a cell pins an identity between two
//! renders rather than a byte that a codec revision or a platform's rounding
//! can move.
//!
//! # Why the colour is neither red nor grey
//!
//! It has to be in gamut in sRGB *and* off the grey axis. A saturated primary
//! clips: `red` on a Display P3 canvas stores as `255, 0, 0` and converts to
//! sRGB as `255, 0, 0`, so `honoured` and `lost` are the same value and the
//! cell cannot fail. That is not a hypothetical -- the first run of this probe
//! used red, reported every cell honoured, and was measuring nothing. Grey
//! fails the same way from the other side: it is grey in every space.
//!
//! [`SEPARATION_FLOOR`] is what stops that recurring. Every space is required
//! to put `honoured` and `lost` at least that far apart before any verdict is
//! read from it, so a space or a colour that cannot discriminate fails loudly
//! instead of passing vacuously.

use meo_skia_canvas::prelude::*;

/// How far apart `honoured` and `lost` must sit for a cell to mean anything.
///
/// In levels, on the widest channel. Chosen against the two quantities it has
/// to separate: the conversions this measures are 12 levels for Display P3
/// and 31 for Rec. 2020, and the largest disagreement any working codec
/// showed on the same colour is 8 -- APNG and ICO at Rec. 2020, which carry
/// the profile through an eight-bit intermediate.
///
/// So it sits above the codec noise and below the smallest real conversion.
/// A space whose conversion is subtler than this needs a different colour,
/// not a smaller floor.
const SEPARATION_FLOOR: i16 = 10;

/// The colour every cell paints. In gamut in sRGB, off the grey axis.
const FILL: (f32, f32, f32) = (0.80, 0.35, 0.20);

/// The formats that can be written *and* read back, with how many pages each
/// cell writes.
///
/// **A copy, and it can drift.** Which formats declare a colour space is
/// decided by `ImageFormat::declares_color`, which is `pub(crate)` and so out
/// of reach from an integration test -- this list restates the part of that
/// table it needs rather than deriving it. A format that changes signal, or
/// one that gains a decoder, will not update this by itself.
///
/// TIFF is absent because this build of Skia has no TIFF decoder, so the cell
/// would measure that absence rather than the tag -- `src/decode/mod.rs` says
/// so and the README lists TIFF under export only. PDF and SVG are absent
/// because they are documents: their colour lives in drawing operators rather
/// than in a tagged pixel buffer, and a round trip through them would measure
/// rasterisation.
///
/// The animated rows are not padding. One of the two defects this file was
/// written for is animated AVIF, and a matrix that only wrote stills could
/// not see it.
const CELLS: &[(&str, ImageFormat, usize)] = &[
    ("png", ImageFormat::Png, 1),
    ("jpeg", ImageFormat::Jpeg, 1),
    ("webp", ImageFormat::Webp, 1),
    // GIF keeps the colour and carries no tag, which is not a contradiction.
    // `ColorSignal::AssumedSrgb` converts the pixels to sRGB on the way out,
    // so the file's silence is true and reading it back as sRGB is right. It
    // satisfies the same assertion as the rows above by narrowing rather than
    // by declaring -- do not read a green here as "GIF carries Display P3".
    ("gif", ImageFormat::Gif, 1),
    ("apng", ImageFormat::Apng, 1),
    ("ico", ImageFormat::Ico, 1),
    ("bmp", ImageFormat::Bmp, 1),
    ("avif", ImageFormat::Avif, 1),
    ("gif x3", ImageFormat::Gif, 3),
    ("apng x3", ImageFormat::Apng, 3),
    ("webp x3", ImageFormat::Webp, 3),
    ("avif x3", ImageFormat::Avif, 3),
];

/// The spaces a cell is measured in.
///
/// sRGB is not among them, and its absence is the point: painting sRGB and
/// reading sRGB makes `honoured` and `lost` the same value, so every cell
/// would pass whatever the code did. `an_srgb_round_trip_is_exact` covers
/// that path as a harness check instead, where it is honest about proving
/// only that the machinery works.
const SPACES: &[(&str, PixelColorSpace)] = &[
    ("display-p3", PixelColorSpace::DisplayP3),
    ("rec2020", PixelColorSpace::Rec2020),
];

/// Cells known to drop the tag, each with why.
///
/// Every entry is asserted to *still* fail. A fix that lands without removing
/// its entry breaks this test, which is the only thing that stops the list
/// outliving the defects -- an allowlist nobody is forced to prune becomes a
/// record of what used to be wrong.
const KNOWN_LOST: &[(&str, &str)] = &[
    // The V4 header is written correctly -- 108 bytes, primaries and gamma in
    // place -- and Skia's decoder discards it on the way back in.
    ("bmp", "display-p3"),
    ("bmp", "rec2020"),
];

fn painted(space: PixelColorSpace, pages: usize) -> Canvas {
    let mut canvas = Canvas::with_options(
        8.0,
        8.0,
        CanvasOptions {
            color_space: space,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas in every documented space");
    for page in 0..pages {
        let ctx = if page == 0 {
            canvas.context()
        } else {
            canvas.new_page()
        };
        ctx.set_fill_style(RgbaLinear::opaque(FILL.0, FILL.1, FILL.2));
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
    }
    canvas
}

fn first_pixel(canvas: &mut Canvas, space: PixelColorSpace) -> [u8; 4] {
    let raw = canvas
        .to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                color_space: Some(space),
                ..EncodeOptions::default()
            },
        )
        .expect("raw export");
    [raw[0], raw[1], raw[2], raw[3]]
}

/// What the cell should read as if the tag survived, and if it did not.
///
/// **A canvas each, and that is not tidiness.** Exporting a canvas into a
/// colour space converts it in place, so a second read of the same canvas
/// reports the first read's answer re-encoded rather than the original
/// pixels: asked for sRGB and then for its own space, a Display P3 canvas
/// holding `red` returns `255, 0, 0` and then `234, 51, 35`, where a canvas
/// read once in its own space returns `255, 0, 0`. Sharing one canvas here
/// silently made `lost` the wrong value.
fn candidates(space: PixelColorSpace) -> ([u8; 4], [u8; 4]) {
    (
        first_pixel(&mut painted(space, 1), PixelColorSpace::Srgb),
        first_pixel(&mut painted(space, 1), space),
    )
}

fn spread(a: [u8; 4], b: [u8; 4]) -> i16 {
    (0..3)
        .map(|i| (a[i] as i16 - b[i] as i16).abs())
        .max()
        .unwrap_or(0)
}

/// The pixel a cell comes back as, or `None` if it cannot be read at all.
fn round_trip(
    space: PixelColorSpace,
    format: ImageFormat,
    pages: usize,
) -> Option<[u8; 4]> {
    let bytes = painted(space, pages)
        .to_buffer(format, &EncodeOptions::default())
        .ok()?;
    read_back(&bytes)
}

fn read_back(bytes: &[u8]) -> Option<[u8; 4]> {
    let image = Image::from_encoded(bytes).ok()?;
    let mut sink = Canvas::with_options(
        8.0,
        8.0,
        CanvasOptions {
            color_space: PixelColorSpace::Srgb,
            ..CanvasOptions::default()
        },
    )
    .expect("an sRGB sink");
    sink.context().draw_image(&image, 0.0, 0.0);
    Some(first_pixel(&mut sink, PixelColorSpace::Srgb))
}

/// Whether the pixel landed nearer the converted answer or the raw one.
///
/// Nearest-of-two rather than a tolerance, so the comparison scales with the
/// conversion instead of needing a number per space. The margin requirement
/// is what keeps a near-tie from deciding by rounding.
fn keeps_the_tag(got: [u8; 4], honoured: [u8; 4], lost: [u8; 4]) -> bool {
    let to_honoured = spread(got, honoured);
    let to_lost = spread(got, lost);
    to_honoured < to_lost
        && (to_lost - to_honoured) * 2 >= spread(honoured, lost)
}

#[test]
fn every_readable_format_keeps_the_canvas_colour_space() {
    let mut lost_now: Vec<(&str, &str)> = Vec::new();

    for (space_name, space) in SPACES {
        let (honoured, lost) = candidates(*space);
        let separation = spread(honoured, lost);
        assert!(
            separation >= SEPARATION_FLOOR,
            "{space_name} converts to sRGB by only {separation} levels, so a \
             cell measured in it cannot tell a kept tag from a lost one. \
             Pick a colour further from the sRGB gamut boundary, or drop the \
             space -- do not lower SEPARATION_FLOOR.",
        );

        for (cell_name, format, pages) in CELLS {
            let Some(got) = round_trip(*space, *format, *pages) else {
                panic!(
                    "{cell_name} in {space_name} could not be written and read \
                     back at all. Every format here is one this crate both \
                     encodes and decodes; if that has stopped being true, take \
                     the row out and say why rather than leaving a cell that \
                     cannot run.",
                );
            };
            if !keeps_the_tag(got, honoured, lost) {
                lost_now.push((cell_name, space_name));
            }
        }
    }

    let known: Vec<(&str, &str)> = KNOWN_LOST.to_vec();
    let unexpected: Vec<_> =
        lost_now.iter().filter(|c| !known.contains(c)).collect();
    let fixed: Vec<_> =
        known.iter().filter(|c| !lost_now.contains(c)).collect();

    assert!(
        unexpected.is_empty(),
        "these cells dropped the colour space and are not in KNOWN_LOST: \
         {unexpected:?}. A file that states its space and is read back in \
         another one is the defect this file exists for.",
    );
    assert!(
        fixed.is_empty(),
        "these cells are in KNOWN_LOST and now keep the tag: {fixed:?}. \
         Delete the entry -- the list is only honest while every row in it \
         still fails.",
    );
}

/// The control: the probe reads the tag, not the pixels.
///
/// A round trip alone cannot tell "the tag was absent" from "the tag was
/// ignored", so this removes one. The same P3 PNG is read twice, once whole
/// and once with its `iCCP` chunk cut out, and the answer has to move from
/// the converted value to the raw one. Without this the matrix above could be
/// measuring anything that happens to correlate with the format.
#[test]
fn stripping_the_profile_flips_the_answer() {
    let (honoured, lost) = candidates(PixelColorSpace::DisplayP3);
    let png = painted(PixelColorSpace::DisplayP3, 1)
        .to_buffer(ImageFormat::Png, &EncodeOptions::default())
        .expect("png");

    let whole = read_back(&png).expect("a PNG this crate just wrote");
    assert!(
        keeps_the_tag(whole, honoured, lost),
        "the intact PNG has to read as converted, or the control starts from \
         the wrong place: got {whole:?} against {honoured:?}",
    );

    let stripped = without_chunk(&png, b"iCCP");
    assert!(
        stripped.len() < png.len(),
        "no iCCP chunk was found to remove, so this proves nothing about \
         what the probe reads",
    );
    let bare = read_back(&stripped).expect("a PNG missing only its profile");
    assert!(
        !keeps_the_tag(bare, honoured, lost),
        "with the profile gone the same bytes have to read as raw sRGB: got \
         {bare:?}, which is still nearer {honoured:?} than {lost:?}",
    );
}

/// An sRGB round trip is exact, for every cell.
///
/// Not evidence about tags -- painting sRGB and reading sRGB makes the two
/// candidates identical, which is why sRGB is not in [`SPACES`]. What it does
/// catch is the harness breaking: a decode that silently returns the wrong
/// frame, a sink that is not the size it asked for, an encoder that stopped
/// round-tripping at all.
#[test]
fn an_srgb_round_trip_is_exact() {
    let (honoured, lost) = candidates(PixelColorSpace::Srgb);
    assert_eq!(
        honoured, lost,
        "in sRGB the converted and raw answers are the same value by \
         construction; if they differ, `candidates` is not doing what its \
         name says",
    );

    for (cell_name, format, pages) in CELLS {
        let got = round_trip(PixelColorSpace::Srgb, *format, *pages)
            .unwrap_or_else(|| panic!("{cell_name} did not round trip"));
        // Lossy codecs move a level or two; nothing here should move more.
        let drift = spread(got, honoured);
        assert!(
            drift <= 2,
            "{cell_name} came back {got:?} against {honoured:?}, {drift} \
             levels away -- too far for a codec on a flat fill",
        );
    }
}

/// Removes every chunk of one type from a PNG, leaving the rest byte for byte.
///
/// Enough for the control above: `iCCP` is ancillary, so a decoder reads what
/// is left as a PNG with no profile, which is exactly the file being asked
/// about. No CRC is recomputed because none is touched.
fn without_chunk(png: &[u8], kind: &[u8; 4]) -> Vec<u8> {
    let mut out = png[..8].to_vec();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes([
            png[at],
            png[at + 1],
            png[at + 2],
            png[at + 3],
        ]) as usize;
        let whole = 12 + len;
        if &png[at + 4..at + 8] != kind {
            out.extend_from_slice(&png[at..at + whole]);
        }
        at += whole;
    }
    out
}
