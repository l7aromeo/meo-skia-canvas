//! A transparency group on every page of a PDF Skia writes, naming the
//! colour space the page's blends are computed in.
//!
//! PDF composites a page's blend modes in its page group's colour space, and
//! where a page has none the viewer chooses (ISO 32000-1, 11.3.4 and 11.4.7).
//! Skia writes none, and viewers choose differently. On an sRGB canvas
//! poppler comes within a level of the canvas for every mode a PDF writes,
//! with a group or without; CoreGraphics -- Preview, Quick Look -- put
//! `multiply` 10 levels off on every pixel and `difference` 51 without one.
//! Given DeviceRGB, the space Skia writes a page's colours in, CoreGraphics
//! comes within 2 levels for every mode but `soft-light`, 16 off -- see
//! docs/browser-differences.md. An sRGB ICC profile in place of DeviceRGB
//! measured the same there, at the cost of a profile in every file.
//!
//! So the group is added once Skia has written the document: inserted into
//! each page's dictionary, with the cross-reference table moved to match.

use std::{ops::Range, str};

/// The entry each page is given: a transparency group (ISO 32000-1, table
/// 147) blending in DeviceRGB, the space Skia writes a page's colours in.
const PAGE_GROUP: &[u8] = b"/Group <</S /Transparency /CS /DeviceRGB>>\n";

/// What opens a page object's dictionary as Skia writes it, the group going
/// in straight after.
const PAGE_OPENS: &[u8] = b"<</Type /Page\n";

/// The key a page's group is written under (ISO 32000-1, table 30), looked
/// for so that a page that has one keeps it.
const GROUP_KEY: &[u8] = b"/Group";

/// The bytes of one cross-reference entry (ISO 32000-1, 7.5.4): a ten-digit
/// offset, a five-digit generation, a keyword and a two-byte end of line.
const XREF_ENTRY: usize = 20;

/// The digits of an entry's byte offset, at its start.
const OFFSET_DIGITS: usize = 10;

/// Where an entry's `n` or `f` keyword is -- see [`XREF_ENTRY`].
const KEYWORD_AT: usize = 17;

/// `pdf` with a transparency group on every page that has none -- see the
/// module. A document not laid out as Skia writes one -- a cross-reference
/// stream, an incremental update -- comes back as it is.
///
/// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
/// Re-check: cargo test skia_writes_a_page_with_no_group. Skia's
/// `SkPDF::Metadata` has no option for a page group; once a page carries
/// one, this has nothing left to add.
pub(crate) fn with_page_groups(pdf: Vec<u8>) -> Vec<u8> {
    grouped(&pdf).unwrap_or(pdf)
}

/// [`with_page_groups`], or `None` where `pdf` is not laid out as expected.
fn grouped(pdf: &[u8]) -> Option<Vec<u8>> {
    let (start, xref) = start_xref(pdf)?;
    let entries = xref_entries(pdf, xref)?;
    let mut inserts: Vec<usize> = entries
        .iter()
        .filter_map(|&(_, object)| page_group_at(pdf, object))
        .collect();
    inserts.sort_unstable();
    // How far the bytes at `at` move: the length of every group inserted
    // before them.
    let moved = |at: usize| {
        inserts.partition_point(|&insert| insert < at) * PAGE_GROUP.len()
    };

    let mut out =
        Vec::with_capacity(pdf.len() + inserts.len() * PAGE_GROUP.len());
    let mut copied = 0;
    for &insert in &inserts {
        out.extend_from_slice(&pdf[copied..insert]);
        out.extend_from_slice(PAGE_GROUP);
        copied = insert;
    }
    out.extend_from_slice(&pdf[copied..start.start]);
    for &(entry, object) in &entries {
        let at = entry + moved(entry);
        let offset = format!(
            "{:0width$}",
            object + moved(object),
            width = OFFSET_DIGITS
        );
        if offset.len() != OFFSET_DIGITS {
            return None;
        }
        out[at..at + OFFSET_DIGITS].copy_from_slice(offset.as_bytes());
    }
    out.extend_from_slice((xref + moved(xref)).to_string().as_bytes());
    out.extend_from_slice(&pdf[start.end..]);
    Some(out)
}

/// The digits after the last `startxref`, and the offset they name.
fn start_xref(pdf: &[u8]) -> Option<(Range<usize>, usize)> {
    const KEYWORD: &[u8] = b"startxref";
    let keyword = pdf.windows(KEYWORD.len()).rposition(|w| w == KEYWORD)?;
    let from = keyword
        + KEYWORD.len()
        + pdf[keyword + KEYWORD.len()..]
            .iter()
            .position(|b| !b.is_ascii_whitespace())?;
    let len = pdf[from..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let digits = str::from_utf8(&pdf[from..from + len]).ok()?;
    Some((from..from + len, digits.parse().ok()?))
}

/// Every object the cross-reference table at `xref` lists in use: where its
/// entry starts, and the offset it gives. `None` where there is no table
/// there -- a cross-reference stream -- or where the trailer chains to an
/// earlier one.
fn xref_entries(pdf: &[u8], xref: usize) -> Option<Vec<(usize, usize)>> {
    let mut at =
        xref + line(pdf, xref).filter(|line| line.trim() == "xref")?.len();
    let mut entries = Vec::new();
    loop {
        let header = line(pdf, at)?;
        if header.starts_with("trailer") {
            let trailer = &pdf[at..];
            let ends = trailer
                .windows(b"startxref".len())
                .position(|w| w == b"startxref")?;
            if trailer[..ends]
                .windows(b"/Prev".len())
                .any(|w| w == b"/Prev")
            {
                return None;
            }
            return Some(entries);
        }
        let mut fields = header.split_ascii_whitespace();
        let (_, count): (usize, usize) =
            (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
        at += header.len();
        for _ in 0..count {
            let entry = pdf.get(at..at + XREF_ENTRY)?;
            if entry[KEYWORD_AT] == b'n' {
                let offset = str::from_utf8(&entry[..OFFSET_DIGITS]).ok()?;
                entries.push((at, offset.parse().ok()?));
            }
            at += XREF_ENTRY;
        }
    }
}

/// The line starting at `at`, with its end of line.
fn line(pdf: &[u8], at: usize) -> Option<&str> {
    let rest = pdf.get(at..)?;
    let len = rest
        .iter()
        .position(|&b| b == b'\n')
        .map_or(rest.len(), |n| n + 1);
    str::from_utf8(&rest[..len]).ok()
}

/// Where a group goes in the object at `object`: straight after the opening
/// of its dictionary, where it is a page with no group. `None` for any other
/// object.
fn page_group_at(pdf: &[u8], object: usize) -> Option<usize> {
    let header = line(pdf, object)?;
    if !header.trim_end().ends_with(" obj") {
        return None;
    }
    let dictionary = object + header.len();
    if !pdf.get(dictionary..)?.starts_with(PAGE_OPENS) {
        return None;
    }
    let body = &pdf[dictionary..];
    let end = body.windows(b"endobj".len()).position(|w| w == b"endobj")?;
    match body[..end].windows(GROUP_KEY.len()).any(|w| w == GROUP_KEY) {
        true => None,
        false => Some(dictionary + PAGE_OPENS.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{BlendMode, Color, Paint, Rect, pdf};

    /// A PDF as Skia writes one: `pages` pages, each a translucent fill
    /// under `multiply` over an opaque one.
    fn skia_pdf(pages: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut document = pdf::new_document(&mut bytes, None);
        for _ in 0..pages {
            let mut page = document.begin_page((60.0, 40.0), None);
            let canvas = page.canvas();
            canvas.draw_rect(Rect::from_wh(60.0, 40.0), &Paint::default());
            let mut paint = Paint::default();
            paint.set_color(Color::from_argb(200, 200, 40, 120));
            paint.set_blend_mode(BlendMode::Multiply);
            canvas.draw_rect(Rect::from_xywh(10.0, 10.0, 30.0, 20.0), &paint);
            document = page.end_page();
        }
        document.close();
        bytes
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    /// Whether every object the table lists starts where it says, and the
    /// table where `startxref` says.
    fn resolves(pdf: &[u8]) -> bool {
        let Some((_, xref)) = start_xref(pdf) else {
            return false;
        };
        let Some(entries) = xref_entries(pdf, xref) else {
            return false;
        };
        !entries.is_empty()
            && entries.iter().enumerate().all(|(n, &(_, object))| {
                // The table lists object 0 free and the rest in order.
                pdf[object..].starts_with(format!("{} 0 obj", n + 1).as_bytes())
            })
    }

    /// The defect [`with_page_groups`] works around: a page Skia writes
    /// carries no group. Fails once it does.
    #[test]
    fn skia_writes_a_page_with_no_group() {
        let pdf = skia_pdf(1);
        assert_eq!(count(&pdf, b"/Type /Page\n"), 1);
        assert_eq!(count(&pdf, GROUP_KEY), 0);
    }

    /// Every page takes one group, and the cross-reference table still finds
    /// every object, the pages' and those after them, as it did before.
    #[test]
    fn every_page_takes_one_group_and_every_object_is_still_found() {
        let pdf = skia_pdf(3);
        assert!(resolves(&pdf), "the table Skia wrote resolves");
        let grouped = with_page_groups(pdf.clone());
        assert_eq!(count(&grouped, PAGE_GROUP), 3);
        assert_eq!(grouped.len(), pdf.len() + 3 * PAGE_GROUP.len());
        assert!(resolves(&grouped), "every offset moved with its object");
        // The group inserted with the table left where it was is not a
        // document: the objects after the first page are not where it says.
        let text = String::from_utf8_lossy(&pdf).replacen(
            "<</Type /Page\n",
            "<</Type /Page\n/Group <</S /Transparency /CS /DeviceRGB>>\n",
            1,
        );
        assert!(!resolves(text.as_bytes()), "the check finds a stale table");
    }

    /// A page that has a group keeps it, so a second pass adds nothing.
    #[test]
    fn a_page_that_has_a_group_keeps_it() {
        let grouped = with_page_groups(skia_pdf(2));
        assert_eq!(with_page_groups(grouped.clone()), grouped);
    }

    /// A document not laid out as Skia writes one comes back as it is.
    #[test]
    fn a_document_laid_out_otherwise_is_left_alone() {
        let pdf = skia_pdf(1);
        let chained = {
            let text = String::from_utf8_lossy(&pdf)
                .replace("<</Size", "<</Prev 9 /Size");
            text.into_bytes()
        };
        for (name, document) in [
            (
                "no startxref",
                b"%PDF-1.4\n1 0 obj\n<</Type /Page\n>>\nendobj\n".to_vec(),
            ),
            ("an incremental update", chained),
            ("a startxref to nothing", {
                let mut bytes = pdf.clone();
                let (digits, _) = start_xref(&bytes).expect("startxref");
                bytes.splice(digits, b"9".iter().copied());
                bytes
            }),
        ] {
            assert_eq!(with_page_groups(document.clone()), document, "{name}");
        }
    }
}
