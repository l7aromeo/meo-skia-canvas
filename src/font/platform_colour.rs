//! Faces whose colour glyphs the platform draws: images in an `sbix` or a
//! `CBDT` table, or SVG documents in an `SVG ` table. Skia draws them through
//! the platform's own rasterizer, in their own colours and never in the
//! paint's, so a run holding one is not a run in one flat colour. A face with
//! a `COLR` table is not one of these: this library draws its colour glyphs
//! itself -- see `font::colr`.
//!
//! Answered for the face, not for each glyph. Telling which glyphs an `sbix`
//! strike holds reads every strike's offsets, which lie across the whole
//! table -- most of Apple Color Emoji's 190 MB -- and a platform hands a
//! system font's tables back only as copies. A glyph of such a face drawn
//! from an outline is taken for a colour glyph too, which only costs its run
//! a route that assumes one flat colour.

use std::cell::RefCell;

use skia_safe::{
    Typeface,
    typeface::{FontTableTag, TypefaceId},
};

use crate::cache_map::CacheMap;

/// The tables holding colour glyphs a platform rasterizes, by their OpenType
/// tags: `sbix` (Apple's standard bitmap graphics), `CBDT` (colour bitmap
/// data) and `SVG ` (the OpenType specification's tables of those names).
const PLATFORM_COLOUR_TABLES: [FontTableTag; 3] = [
    u32::from_be_bytes(*b"sbix"),
    u32::from_be_bytes(*b"CBDT"),
    u32::from_be_bytes(*b"SVG "),
];

/// How many faces a thread keeps the answer for before it drops them all
/// and asks again -- a bound, since a variable font makes a typeface for
/// every instance asked for.
const FACES_KEPT: usize = 64;

thread_local! {
    /// The answer [`draws_own_colours`] gave for each typeface, by its
    /// unique id, which no other typeface in the process takes.
    static FACES: RefCell<CacheMap<TypefaceId, bool>> =
        RefCell::new(CacheMap::default());
}

/// Whether `typeface` holds colour glyphs the platform draws in their own
/// colours, rather than in the paint's: whether it has an `sbix`, `CBDT` or
/// `SVG ` table. Read from the face's list of tables, which copies none of
/// them.
pub(crate) fn draws_own_colours(typeface: &Typeface) -> bool {
    FACES.with_borrow_mut(|faces| {
        let id = typeface.unique_id();
        if let Some(&answer) = faces.get(&id) {
            return answer;
        }
        if faces.len() >= FACES_KEPT {
            faces.clear();
        }
        let answer = typeface.read_table_tags().is_some_and(|tags| {
            tags.iter().any(|tag| PLATFORM_COLOUR_TABLES.contains(tag))
        });
        faces.insert(id, answer);
        answer
    })
}
