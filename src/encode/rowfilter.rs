//! Whether filtering a drawing's rows makes its PNG smaller.
//!
//! PNG stores each row as a difference from the one above it, or as it is,
//! and which is smaller depends entirely on the drawing. A gradient's rows
//! are nearly identical, so deflate's long-range matching finds whole
//! repeated rows in the unfiltered stream and finds nothing once the
//! differencing has broken them up; a photograph has no such repetition, and
//! there the differencing is what removes the redundancy deflate can use.
//! Measured on a 1200x900 page, turning filtering off made a gradient 4.3
//! times faster to encode and 3.4 times smaller, text 1.8 times faster and
//! 1.6 times smaller, and a flat fill 2.7 times faster at the same size --
//! while making a photographic page 57% larger.
//!
//! So neither answer is right, and this samples a few bands of rows to ask.
//! It is shared by the two writers that produce PNG streams -- Skia's, for
//! `png`, and this crate's own, for `apng` -- because they are writing the
//! same format and the question is about the pixels, not the writer.
//!
//! The answer is about size and speed alone. PNG is lossless and both
//! filtering and deflate are reversible, so every setting here decodes to the
//! same pixels.

use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

/// Consecutive rows in one sample band.
///
/// Long runs, because the unfiltered stream is what needs room. Deflate finds
/// matches across a whole image, and where rows repeat -- a page of flat
/// blocks, an interface, a chart -- storing them unfiltered compresses far
/// better than the sample can show if the sample never holds two of the
/// repeats at once. Pairs of rows spread down the page could not: a page of
/// flat blocks probed 0.24, meaning filtering would shrink it to a quarter,
/// and filtering actually took it from 45 KB to 67.
///
/// Pairs were chosen to stop a band of four making a gradient look filterable
/// when it is not, and they did. What was not noticed is that they broke the
/// other side, and that the fix for both is more rows rather than fewer: at
/// forty-eight, the gradient probes 2.29 and the flat blocks 1.01, and both
/// are read correctly.
pub(crate) const PROBE_BAND_ROWS: i32 = 48;

/// Bands taken down the page.
///
/// Two, so the sample sees more than one part of a drawing that is rarely
/// uniform -- a chart is flat at the top and dense at the bottom -- while each
/// band stays long enough for [`PROBE_BAND_ROWS`] to mean anything.
///
/// Two forty-eights was picked by measuring, not reasoning. Ten 1200x900
/// pages were encoded both ways to find which answer was actually smaller,
/// and every combination of one, two, four and eight bands against sixteen to
/// ninety-six rows was scored against that. Several reach the right answer on
/// all ten; this one does it across the widest band of thresholds, and leaves
/// the most room between the nearest page and the decision line -- 0.042,
/// against 0.003 for two bands of thirty-two, which lands on all ten by a
/// margin too thin to trust on a drawing not in the set.
pub(crate) const PROBE_BANDS: i32 = 2;

/// Row filtering is asked for when the filtered sample deflates to less than
/// this fraction of the unfiltered one.
///
/// One: filter when filtering is smaller, and not otherwise. There is no
/// margin because there is nothing for a margin to correct. It used to be
/// 0.8, and that number was compensation -- the sample was two rows at a
/// time, which flattered filtering, so the answer had to clear a bar before
/// it was believed. A sample long enough to hold what deflate actually
/// exploits does not need the handicap, and across ten pages measured both
/// ways every one lands on the correct side of one, the nearest by 0.042.
pub(crate) const PROBE_FILTER_BELOW: f64 = 1.0;

/// The deflate level, for the probe's sample and for the encoder.
///
/// Six, which is Skia's own default, and pinned rather than chosen.
///
/// It was chosen for a while, by deflating the winning sample again at level
/// four and taking the cheaper one where the deeper earned little. That cannot
/// work from a sample. Deflate's deeper search pays off over the whole image
/// and a few bands of rows are too small to show it: on a diagonal gradient
/// the sample put level four at 5.3% more bytes, and the page came out at
/// 128% more -- 91 KB where the same pixels fit in 40, to save 0.9 ms.
///
/// Nor is four a level to fall back to, which is what makes pinning easy
/// rather than a compromise. Across five 1200x900 pages -- the mixed scene,
/// a diagonal gradient, a flat interface, a text page and a noise page --
/// six is smaller than or equal to four everywhere. Where four is cheaper it
/// is cheaper by 26 to 40% of encode time, for 0.07 to 5.2% more bytes; on
/// the gradient it is 105% slower *and* 4.2 times larger, 178.6 KB against
/// 42.9. It can lose on both axes at once, so there is no page for which it
/// is the answer. On the 150-frame sequence the probe was tuned against,
/// pinning already chose six: 165 ms and 5.88 MB pinned, against 170 ms and
/// the same 5.88 MB probed.
///
/// The row filter is still probed, because the same ground truth says that
/// half gets it right.
pub(crate) const DEFLATE_LEVEL: u32 = 6;

/// How many rows a band of a page this tall may take.
///
/// As long as asked for where the page can spare them, and shared out evenly
/// where it cannot. A fixed forty-eight would mean any page under ninety-six
/// rows was never filtered at all -- a thumbnail, a sprite sheet row, a tiny
/// chart. The shape of the sample matters more than its size: two bands of
/// sixteen read a short page the way two of forty-eight read a tall one.
pub(crate) fn band_rows(height: i32) -> i32 {
    PROBE_BAND_ROWS.min(height / PROBE_BANDS).max(2)
}

/// Where the `n`th band of a page this tall starts.
pub(crate) fn band_top(n: i32, height: i32, band: i32) -> i32 {
    ((n * 2 + 1) * height / (PROBE_BANDS * 2))
        .min(height - band)
        .max(0)
}

/// Adds one band of rows to the two streams the probe compares.
///
/// The filtered stream is the Up filter -- each byte less the byte above it
/// -- which is the one a repeated row collapses under and the one an encoder
/// choosing adaptively reaches for most. The first row of each band is
/// skipped because it has no row above it inside the sample.
pub(crate) fn accumulate(
    sample: &[u8],
    row_bytes: usize,
    rows: usize,
    plain: &mut Vec<u8>,
    filtered: &mut Vec<u8>,
) {
    for r in 1..rows {
        let (above, here) =
            (&sample[(r - 1) * row_bytes..], &sample[r * row_bytes..]);
        plain.extend_from_slice(&here[..row_bytes]);
        filtered.extend((0..row_bytes).map(|i| here[i].wrapping_sub(above[i])));
    }
}

/// Whether filtering pays, or `None` if neither stream would deflate.
pub(crate) fn pays(plain: &[u8], filtered: &[u8]) -> Option<bool> {
    let deflate = |bytes: &[u8]| {
        let mut out =
            ZlibEncoder::new(Vec::new(), Compression::new(DEFLATE_LEVEL));
        out.write_all(bytes)
            .ok()
            .and_then(|()| out.finish().ok())
            .map(|v| v.len())
    };
    let (with, without) = (deflate(filtered)?, deflate(plain)?);
    Some(without > 0 && (with as f64) < (without as f64) * PROBE_FILTER_BELOW)
}

/// Whether filtering pays for a page already sitting in memory as rows.
///
/// For a caller holding the whole buffer, which is what this crate's own PNG
/// writer has. The other caller reads its bands off a Skia image instead, so
/// that it never materializes a page to ask a question about it.
pub(crate) fn pays_for(
    pixels: &[u8],
    row_bytes: usize,
    height: i32,
) -> Option<bool> {
    let band = band_rows(height);
    if row_bytes == 0 || height < band * 2 {
        // Too little to sample, and too little for the choice to matter.
        return Some(false);
    }
    let (mut plain, mut filtered) = (Vec::new(), Vec::new());
    for n in 0..PROBE_BANDS {
        let top = band_top(n, height, band) as usize;
        let start = top * row_bytes;
        let end = start + band as usize * row_bytes;
        accumulate(
            pixels.get(start..end)?,
            row_bytes,
            band as usize,
            &mut plain,
            &mut filtered,
        );
    }
    pays(&plain, &filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Which way round the probe answers is pinned in `context::page`, on the
    // images built there for it -- `png_row_filtering_is_asked_for_only_where
    // _it_pays`. Generated data will not stand in for that: six synthetic
    // shapes, including a gradient and a page of flat blocks, all read as
    // worth filtering, where the drawings they were meant to stand for do
    // not. What is left here is the arithmetic around the sample, which
    // generated data does answer for.

    /// One page of `height` rows, `width` pixels of RGBA each, filled by
    /// `pixel`.
    fn page(
        width: usize,
        height: usize,
        pixel: impl Fn(usize, usize) -> [u8; 4],
    ) -> (Vec<u8>, usize) {
        let row = width * 4;
        let mut bytes = vec![0u8; row * height];
        for y in 0..height {
            for x in 0..width {
                bytes[y * row + x * 4..y * row + x * 4 + 4]
                    .copy_from_slice(&pixel(x, y));
            }
        }
        (bytes, row)
    }

    #[test]
    fn a_page_too_short_to_sample_is_not_filtered() {
        // Under two bands there is nothing to compare, and the choice does
        // not matter at that size either.
        let (bytes, row) = page(16, 3, |x, _| [x as u8, 0, 0, 255]);
        assert_eq!(pays_for(&bytes, row, 3), Some(false));
    }

    #[test]
    fn the_bands_stay_inside_the_page() {
        // `band_top` is asked for a top that leaves a whole band below it,
        // however short the page and whichever band is asked for.
        for height in [4, 5, 17, 96, 97, 900] {
            let band = band_rows(height);
            for n in 0..PROBE_BANDS {
                let top = band_top(n, height, band);
                assert!(
                    top >= 0,
                    "band {n} of {height} starts before the page"
                );
                assert!(
                    top + band <= height,
                    "band {n} of {height} runs past the page"
                );
            }
        }
    }
}
