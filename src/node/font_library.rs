//
// Font collection management
//
#![allow(non_snake_case)]
use neon::{prelude::*, types::buffer::TypedArray};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    fs, iter,
    path::Path,
    rc::Rc,
    sync::{Arc, OnceLock},
};

use skia_safe::{
    Data, FontArguments, FontHinting, FontMgr, FourByteTag, Typeface,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_parameters::variation::Axis,
    font_style::{FontStyle, Slant},
    textlayout::{FontCollection, TextStyle, TypefaceFontProvider},
    typeface::TypefaceId,
};

use crate::{
    font::{
        FontFamily, GenericFamily, InstancedPositions, optical_size,
        variation_axes,
    },
    image::x_height_ratio,
    node::decorate,
    typography::{
        FontSpec, from_slant, from_width, typeface_details, typeface_wght_range,
    },
    utils::*,
};

#[cfg(target_os = "windows")]
use allsorts::{
    binary::read::ReadScope, subset::whole_font, tables::FontTableProvider,
    woff::WoffFont, woff2::Woff2Font,
};

#[cfg(target_os = "linux")]
use std::sync::Once;

/// Guards [`install_fontconfig_fallback`], so the process writes at most once.
#[cfg(target_os = "linux")]
static FONTCONFIG_FALLBACK: Once = Once::new();

/// Points fontconfig at the bundled `fonts.conf` where the machine has none.
///
/// A minimal Linux image often ships no `/etc/fonts/fonts.conf`. With no
/// config fontconfig sees only the directories its own built-in fallback
/// names, so a font installed anywhere else is invisible. This points it at
/// the `fonts` directory shipped beside the binary, which names several more.
///
/// `FONTCONFIG_PATH` is the only lever there is. Skia builds its font manager
/// with `SkFontMgr_New_FontConfig(nullptr, ...)`, and the null does not ask
/// for the current `FcConfig`: `SkFontMgr_fontconfig.cpp:707` reads
/// `fFC(config ? config : FcInitLoadConfigAndFonts())`, which loads a fresh
/// default config from the file list `FONTCONFIG_PATH` and `FONTCONFIG_FILE`
/// steer. A config installed with `FcConfigSetCurrent` is therefore never
/// read. That was tried and measured on a Linux build: with the fonts in a
/// directory only the bundled config names, the environment variable found
/// them and `FcConfigSetCurrent` found nothing.
///
/// UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
/// Re-check: a `FontMgr` constructor taking an `FcConfig`. At 0.153.3
/// `FontMgr::new()` passes null and `grep -rn fontconfig skia-safe/src` finds
/// nothing, so a config cannot be handed to Skia from Rust at all.
///
/// **The write is unsound, and calling it once is the most that can be done
/// about that.** `std::env::set_var` races any concurrent `getenv`, including
/// one inside a C library on another thread, and nothing here can rule that
/// out. The `Once` holds it to a single write per process, where the
/// `thread_local` initializer this used to sit in ran it once per thread; and
/// the Neon entry point calls it before building the rayon pool, so this
/// addon's own threads start after the write rather than around it. Neither
/// of those makes it safe.
pub(crate) fn install_fontconfig_fallback() {
    #[cfg(target_os = "linux")]
    FONTCONFIG_FALLBACK.call_once(|| {
        let configured = fs::exists(Path::new("/etc/fonts/fonts.conf"))
            .unwrap_or(false)
            || std::env::var_os("FONTCONFIG_PATH").is_some()
            || std::env::var_os("FONTCONFIG_FILE").is_some();
        if configured {
            return;
        }

        let Some(mut bundled) = process_path::get_dylib_path() else {
            return;
        };
        bundled.set_file_name("fonts");

        // SAFETY: nothing here makes this sound, and the note above says why
        // it stays. `Once` holds it to the one write this process performs,
        // and the Neon entry point calls it before starting any thread of
        // ours, which is as narrow as the window gets.
        unsafe { std::env::set_var("FONTCONFIG_PATH", bundled) };
    });
}

/// The CSS `font-stretch` percentage an OpenType `usWidthClass` names.
///
/// The nine classes of the OS/2 table, which CSS Fonts 4 defines the
/// keywords against: `ultra-condensed` through `ultra-expanded`, with
/// `normal` in the middle. The `wdth` variation axis is measured in these
/// percentages, which is why the lookup is here rather than a ratio applied
/// somewhere else.
///
/// The table is spelled out rather than computed. The steps are not uniform
/// -- 12.5 apart at the condensed end, then 12.5, then 25, then 50 -- so any
/// formula would be a worse way of writing nine numbers from a standard.
const WIDTH_CLASS_PERCENT: [f32; 9] =
    [50.0, 62.5, 75.0, 87.5, 100.0, 112.5, 125.0, 150.0, 200.0];

/// [`WIDTH_CLASS_PERCENT`] for `class`, or the normal width for a class the
/// standard does not define.
///
/// `usWidthClass` runs 1 to 9. Class 5 is normal, and used to share a
/// catch-all arm with every out-of-range value -- so a font declaring class
/// 0 or 12 was treated as normal without the reader of that arm being able
/// to tell the legitimate case from the malformed one. Same answer, because
/// normal is the only sensible fallback, and now it is one the code says on
/// purpose.
fn stretch_percent(class: i32) -> f32 {
    const NORMAL: f32 = WIDTH_CLASS_PERCENT[4];
    usize::try_from(class)
        .ok()
        .and_then(|class| class.checked_sub(1))
        .and_then(|index| WIDTH_CLASS_PERCENT.get(index))
        .copied()
        .unwrap_or(NORMAL)
}

thread_local!(
    static LIBRARY: OnceLock<RefCell<FontLibrary>> = const { OnceLock::new() };
);

/// The hinting a context draws text with when its `fontHinting` is turned
/// off: none, so a glyph is its outline as designed. FreeType draws it
/// unhinted, and on macOS it is drawn without the outline dilation
/// CoreGraphics applies at every other level -- Skia's CoreText port maps
/// None to undilated and anything else to Normal, dilated
/// (`SkTypeface_Mac::onFilterRec`).
const UNHINTED: FontHinting = FontHinting::None;

/// The hinting text is drawn with otherwise, a paragraph's always: Slight,
/// SkParagraph's own default and what Chrome draws with. On macOS it is the
/// dilated outline, and with the text gamma in `canvas::DEFAULT_TEXT_GAMMA`
/// Chrome 156's canvas matches it pixel for pixel. Under FreeType it fits
/// outlines to the pixel grid vertically only, so every advance stays as
/// designed and turning hinting off moves no text; Normal and Full fit
/// advances too: 13px Oswald's "Hamburgefonstiv quick brown" is 140.50 wide
/// unhinted and at Slight, and 144 at Normal and Full.
const HINTED: FontHinting = FontHinting::Slight;

/// The hinting to draw text with, where `hinted` says whether hinting was
/// asked for -- see [`UNHINTED`] and [`HINTED`].
pub(crate) fn text_hinting(hinted: bool) -> FontHinting {
    match hinted {
        true => HINTED,
        false => UNHINTED,
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CollectionKey {
    families: String,
    weight: i32,
    /// Part of the key because a variable font is now instanced at the `wdth`
    /// axis the width asks for. Without it the first width to be asked for
    /// cached a collection the rest reused, so `fontStretch` moved nothing.
    width: i32,
    slant: Slant,
    /// Axis values, fixed-point at [`AXIS_KEY_STEPS`] per unit.
    ///
    /// An integer because the key is hashed and `f32` is not [`Hash`], and
    /// quantized rather than transmuted because two axis values a
    /// ten-thousandth apart instance the same typeface and should not cache
    /// two of it.
    variations: Vec<(u32, i32)>,
    /// The font size, at [`AXIS_KEY_STEPS`] per pixel, where it is the
    /// `opsz` coordinate: `None` unless a matched face has the axis and
    /// `variations` leaves it to the size. Otherwise the size instances
    /// nothing, and keying on it would cache one collection per size for
    /// fonts that draw every size from the same one.
    optical_size: Option<i32>,
    /// Whether the collection is for text drawn hinted -- see
    /// [`FontLibrary::set_hinting`].
    hinted: bool,
}

/// Steps per axis unit in a [`CollectionKey`].
///
/// A thousandth of a unit is finer than any axis this can be asked for:
/// `wght` runs 1 to 1000 and `wdth` 50 to 200, so this distinguishes far more
/// positions than a typeface has distinguishable instances. Coarser would
/// merge two the caller can tell apart; finer would only cache duplicates.
const AXIS_KEY_STEPS: f32 = 1000.0;

impl CollectionKey {
    pub fn new(
        style: &TextStyle,
        variations: &[(FourByteTag, f32)],
        optical_size: Option<f32>,
        hinted: bool,
    ) -> Self {
        let families = style.font_families();
        let families = families.iter().collect::<Vec<&str>>().join(", ");
        let weight = *style.font_style().weight();
        let width = *style.font_style().width();
        let slant = style.font_style().slant();
        // Rounded, not truncated. Truncation is one-sided, so the step either
        // side of a whole number is not the same size -- and the `as` cast
        // rounds toward zero, which makes it asymmetric about zero as well.
        let variations = variations
            .iter()
            .map(|(tag, val)| (**tag, (*val * AXIS_KEY_STEPS).round() as i32))
            .collect();
        let optical_size =
            optical_size.map(|size| (size * AXIS_KEY_STEPS).round() as i32);
        CollectionKey {
            families,
            weight,
            width,
            slant,
            variations,
            optical_size,
            hinted,
        }
    }
}

/// Where `style` places each axis of a variable face with `axes`.
///
/// An axis `variations` names takes the value given, the last where it is
/// named twice, as in `font-variation-settings`. Otherwise `wdth` takes the
/// style's stretch, `wght` its weight and `opsz` its font size -- what
/// `font-stretch`, `font-weight` and `font-optical-sizing: auto` set -- and
/// any other axis its default. OpenType defines `wght` on the CSS
/// `font-weight` scale and `wdth` in the percentages `font-stretch` names,
/// so neither needs converting. Each value is pinned to its axis.
///
/// Every axis rather than the ones asked for, so the position does not
/// depend on the face it is applied to: a style pushed onto a paragraph
/// builder applies it over a face already instanced at the paragraph's, and
/// Skia moves only the axes a position names.
pub(crate) fn instance_coordinates(
    axes: &[Axis],
    style: &TextStyle,
    variations: &[(FourByteTag, f32)],
) -> Vec<Coordinate> {
    let wdth = FourByteTag::from_chars('w', 'd', 't', 'h');
    let wght = FourByteTag::from_chars('w', 'g', 'h', 't');
    axes.iter()
        .map(|axis| {
            let named = variations
                .iter()
                .rev()
                .find(|(tag, _)| *tag == axis.tag)
                .map(|(_, value)| *value);
            let value = named.unwrap_or_else(|| match axis.tag {
                tag if tag == wdth => {
                    stretch_percent(*style.font_style().width())
                }
                tag if tag == wght => *style.font_style().weight() as f32,
                _ => {
                    optical_size(std::slice::from_ref(axis), style.font_size())
                        .map_or(axis.def, |opsz| opsz.value)
                }
            });
            // `max` then `min` rather than `clamp`, which panics on an axis
            // whose minimum a malformed font puts above its maximum.
            Coordinate {
                axis: axis.tag,
                value: value.max(axis.min).min(axis.max),
            }
        })
        .collect()
}

/// How many instanced collections are kept.
///
/// Each entry holds a `FontCollection` with a `TypefaceFontProvider` and the
/// variable typefaces instanced into it, measured at about 9 KB: three
/// thousand distinct `wght` values cost 27 MB of map, which this bound
/// replaces with roughly one.
///
/// A hundred and twenty-eight is more distinct instances than a page holds --
/// a handful of families at a handful of weights each. What it deliberately
/// does not accommodate is an animation tweening an axis, or tweening the
/// font size of a face with an `opsz` axis, and that is the point:
/// consecutive frames of a tween ask for values no frame asks for again, so
/// every one is a miss whatever the bound, and an unbounded map only keeps
/// them.
///
/// A miss costs about 275 microseconds in a release build on macOS: 74 to
/// clone the face through CoreText, and most of the rest laying out with a
/// typeface whose shaping and glyph caches are cold. Chrome 152 pays about
/// 180 for a font size it has not seen on Amstelvar. Keeping more instances
/// would only help a caller cycling through more than this many values, at
/// about 34 KB each.
///
/// The process stays bounded with it. Measured in release on macOS, RSS
/// grows by 35 MB over three thousand `wght` values on Oswald and by 40 over
/// thirty thousand; Amstelvar at three thousand distinct sizes grows by 43
/// and at thirty thousand by 90, where a static font at the same thirty
/// thousand sizes grows by 65 with nothing instanced at all.
const COLLECTION_CACHE_SIZE: usize = 128;

/// How many resolved fonts to remember.
///
/// Matched to the memo the JavaScript CSS parser keeps of the same strings,
/// which holds 1024. A drawing uses a handful of fonts; the bound is here
/// because a program that animates a font size names a new one every frame,
/// and neither side should grow without one.
const RESOLVED_FONT_CACHE_SIZE: usize = 1024;

/// A font specification, and the typeface the library matched it to.
///
/// Behind an `Arc` because a cache hit hands one back and the alternative is
/// cloning the whole specification -- the family list, the feature names, the
/// variant and both forms of the font string -- to say what was already
/// known.
pub type ResolvedFont = Arc<(FontSpec, Typeface)>;

/// Fonts already resolved, by the canonical string naming them.
///
/// Resolving one costs a typeface lookup and, before it, reading ten keys
/// off a JavaScript object -- together over a microsecond, against the five
/// nanoseconds the CSS parse itself takes on a memo hit. The canonical string
/// determines the whole specification, so the same string names the same font
/// until the library changes underneath it. That is why it, rather than the
/// serialized form the `font` getter reports, is the key: the serialized form
/// drops the line height, so two fonts differing only there would share an
/// entry.
#[derive(Default)]
struct ResolvedFontCache {
    entries: HashMap<String, (ResolvedFont, u64)>,
    uses: u64,
}

impl ResolvedFontCache {
    fn get(&mut self, canonical: &str) -> Option<ResolvedFont> {
        self.uses += 1;
        let uses = self.uses;
        self.entries.get_mut(canonical).map(|(font, stamp)| {
            *stamp = uses;
            font.clone()
        })
    }

    fn insert(&mut self, canonical: String, font: ResolvedFont) {
        if self.entries.len() >= RESOLVED_FONT_CACHE_SIZE
            && !self.entries.contains_key(&canonical)
        {
            self.evict_half();
        }
        self.uses += 1;
        self.entries.insert(canonical, (font, self.uses));
    }

    /// Drops the least recently used half.
    ///
    /// Half rather than one, because finding the single oldest means
    /// scanning the map and that scan would then be paid on every insert. A
    /// drawing that names a new font each frame -- animating a size does --
    /// filled this and then spent ten microseconds a font looking for
    /// something to throw away, against the microsecond and a half it was
    /// trying to save. Dropping half amortises the scan over the next five
    /// hundred inserts.
    fn evict_half(&mut self) {
        let mut stamps: Vec<u64> =
            self.entries.values().map(|(_, stamp)| *stamp).collect();
        let middle = stamps.len() / 2;
        // `select_nth_unstable` partitions rather than sorts: the value at
        // `middle` is where it would be if sorted, which is the only thing
        // read here.
        let (_, cutoff, _) = stamps.select_nth_unstable(middle);
        let cutoff = *cutoff;
        self.entries.retain(|_, (_, stamp)| *stamp > cutoff);
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// A bounded memoization of [`FontLibrary::fonts_for_style`].
///
/// Pure caching of a deterministic build, so evicting costs one rebuild and
/// changes no output -- which is what makes a bound safe here. Least recently
/// used rather than least recently inserted: a page that alternates between
/// two families should keep both, and insertion order would drop whichever
/// was first seen.
///
/// The stamps are a counter rather than a clock, so nothing here reads the
/// time, and eviction scans for the smallest. That is linear in the cache
/// size, which is bounded by the constant above and only paid on a miss that
/// finds the map full.
#[derive(Default)]
struct CollectionCache {
    entries: HashMap<CollectionKey, (FontCollection, Option<FontStyle>, u64)>,
    uses: u64,
}

thread_local! {
    /// The face each instance an [`InstanceCache`] holds was cloned from, by
    /// id. Thread-local as the library is, and outside it so that a lookup
    /// never has to borrow the library.
    static INSTANCE_BASES: RefCell<HashMap<TypefaceId, TypefaceId>> =
        RefCell::new(HashMap::new());
}

/// The face `id` was instanced from by [`FontLibrary::fonts_for_style`], or
/// `id` itself for any other face.
///
/// What belongs to the font file rather than to a position on its axes --
/// its family name, which kerning path the shaper takes -- is the same for
/// every instance of it. Remembered per instance, it would be worked out
/// again for each size of a face with an `opsz` axis, since each size is an
/// instance of its own: the kerning probe alone lays out up to twenty-four
/// paragraphs, about two fifths of a `measureText` at a size not seen
/// before. Answered for as long as the [`InstanceCache`] holds the instance.
pub(crate) fn base_face(id: TypefaceId) -> TypefaceId {
    INSTANCE_BASES.with(|bases| bases.borrow().get(&id).copied().unwrap_or(id))
}

/// What one instance a [`InstanceCache`] holds is charged against
/// [`INSTANCE_CACHE_BYTES`]: an estimate, because the real cost cannot be
/// observed.
///
/// Neither Skia nor CoreText nor FreeType reports what a typeface holds, and
/// nearly all of it arrives on first use rather than at the clone -- the
/// caches CoreText keeps per font object on macOS, the `FT_Face`, its blend
/// data and the autohinter's globals on Linux -- and grows with the glyphs
/// drawn. Measured on 2026-10-23 in release builds, as the growth in resident
/// memory per instance held over three thousand made with
/// `clone_with_arguments`, cold and after shaping and outlining the fifteen
/// glyphs of `Hamburgefonstiv` at 24px:
///
/// ```text
///                        glyphs  file    macOS 26 (arm64)   Linux arm64 container
///                                        cold    warm       cold    warm
/// Amstelvar Alpha VF        115  145 KB  13.8    39.2 KB    0.9     52.2 KB
/// Oswald VF                 754  149 KB   8.8    35.3 KB    0.3     26.5 KB
/// Raleway VF               1067  302 KB   8.6    33.1 KB    0.0     30.9 KB
/// ```
///
/// It follows neither the file's size nor its glyph count, so nothing a face
/// reports would make a better per-face charge, and the bound this sets is a
/// count of instances in practice. Re-measure with the same shape of probe
/// before changing it.
const INSTANCE_BYTES_ESTIMATE: usize = 34 * 1024;

/// How many bytes of instances an [`InstanceCache`] keeps, at
/// [`INSTANCE_BYTES_ESTIMATE`] each.
///
/// 34 MB holds about a thousand: enough for a caller cycling through a
/// thousand font sizes of a face with an `opsz` axis to find every one
/// already made, which measured 290 microseconds a `measureText` rebuilt and
/// 51 held, at about 34 KB each. A tween never asks for a size twice and
/// gains nothing from any bound.
const INSTANCE_CACHE_BYTES: usize = 34 * 1024 * 1024;

/// An instance's face and position: the face's id and each coordinate by
/// its axis and the bits of its value, since Skia tells two positions apart
/// by exact equality.
type InstanceKey = (TypefaceId, Vec<(u32, u32)>);

/// The variable-font instances [`FontLibrary::fonts_for_style`] has made,
/// kept across the collections that hold them.
///
/// A collection evicted from the [`CollectionCache`] takes nothing with it
/// that is expensive to make again; its instances are. Each new typeface
/// costs a clone and starts with cold shaping and glyph caches, which Skia
/// keys on the typeface, so holding the instance is what makes a size seen
/// before cheap again. And a recording pins the typeface every text draw it
/// holds was made with, so a position asked for twice is one typeface
/// pinned rather than two.
///
/// Least recently used, by a counter, with the order kept in a second map so
/// that eviction does not scan.
#[derive(Default)]
struct InstanceCache {
    entries: HashMap<InstanceKey, (Typeface, u64)>,
    order: BTreeMap<u64, InstanceKey>,
    bytes: usize,
    uses: u64,
}

impl InstanceCache {
    /// `face` at `coordinates`, made now or held from before.
    fn instance(
        &mut self,
        face: &Typeface,
        coordinates: &[Coordinate],
    ) -> Option<Typeface> {
        let key = (
            face.unique_id(),
            coordinates
                .iter()
                .map(|coordinate| {
                    (*coordinate.axis, coordinate.value.to_bits())
                })
                .collect(),
        );
        self.uses += 1;
        let uses = self.uses;
        if let Some((instance, stamp)) = self.entries.get_mut(&key) {
            self.order.remove(stamp);
            *stamp = uses;
            self.order.insert(uses, key);
            return Some(instance.clone());
        }

        let args = FontArguments::new()
            .set_variation_design_position(VariationPosition { coordinates });
        let instance = face.clone_with_arguments(&args)?;
        INSTANCE_BASES.with(|bases| {
            bases
                .borrow_mut()
                .insert(instance.unique_id(), face.unique_id())
        });
        self.bytes += INSTANCE_BYTES_ESTIMATE;
        while self.bytes > INSTANCE_CACHE_BYTES
            && let Some((_, oldest)) = self.order.pop_first()
        {
            self.forget(&oldest);
        }
        self.order.insert(uses, key.clone());
        self.entries.insert(key, (instance.clone(), uses));
        Some(instance)
    }

    /// Drops the entry at `key`, and its instance's face with it.
    fn forget(&mut self, key: &InstanceKey) {
        if let Some((instance, _)) = self.entries.remove(key) {
            self.bytes -= INSTANCE_BYTES_ESTIMATE;
            INSTANCE_BASES
                .with(|bases| bases.borrow_mut().remove(&instance.unique_id()));
        }
    }

    fn clear(&mut self) {
        let keys: Vec<InstanceKey> = self.order.values().cloned().collect();
        keys.iter().for_each(|key| self.forget(key));
        self.order.clear();
    }
}

impl CollectionCache {
    fn get(
        &mut self,
        key: &CollectionKey,
    ) -> Option<(FontCollection, Option<FontStyle>)> {
        self.uses += 1;
        let uses = self.uses;
        self.entries
            .get_mut(key)
            .map(|(collection, matched, stamp)| {
                *stamp = uses;
                (collection.clone(), *matched)
            })
    }

    fn insert(
        &mut self,
        key: CollectionKey,
        collection: FontCollection,
        matched: Option<FontStyle>,
    ) {
        if self.entries.len() >= COLLECTION_CACHE_SIZE
            && !self.entries.contains_key(&key)
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (.., stamp))| *stamp)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.uses += 1;
        self.entries.insert(key, (collection, matched, self.uses));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn clear_caches(&mut self) {
        self.entries
            .values_mut()
            .for_each(|(collection, ..)| collection.clear_caches());
    }
}

/// The registered fonts as they stood between two changes to them.
///
/// Shared by everything laid out in that time, so a measurement whose ink
/// is worked out later can lay its run out again against the faces it was
/// measured with, even after a font has been registered or the library
/// reset. Only the registered faces: they are what a change replaces, and
/// the library holds them anyway while the epoch is current.
pub struct FontEpoch {
    fonts: Vec<(Typeface, Option<String>)>,
}

pub struct FontLibrary {
    mgr: FontMgr,
    /// The registered fonts as they stand, shared with what is laid out
    /// against them. Replaced by [`invalidate`](Self::invalidate).
    epoch: Rc<FontEpoch>,
    /// The collection over every registered and generic face, one for text
    /// drawn unhinted and one for text drawn hinted -- see
    /// [`set_hinting`](Self::set_hinting).
    collections: [Option<FontCollection>; 2],
    /// What every collection is built over, from
    /// [`collection_parts`](Self::collection_parts).
    collection_parts: Option<(FontMgr, Option<String>)>,
    fonts: Vec<(Typeface, Option<String>)>,
    generics_cache: Vec<(Typeface, Option<String>)>,
    collection_cache: CollectionCache,
    /// The instances the cached collections were built from, kept longer.
    instances: InstanceCache,
    resolved_fonts: ResolvedFontCache,
    /// Which of the two kinds of collection this hands out now -- see
    /// [`set_hinting`](Self::set_hinting).
    collection_hinted: bool,
    /// The positions paragraph builders have instanced through font
    /// arguments on the collections this hands out.
    positions: InstancedPositions,
    /// The system's family names, in lower case, read once for
    /// [`has_family`](Self::has_family) and dropped with everything else on
    /// [`invalidate`](Self::invalidate).
    system_families: Option<HashSet<String>>,
}

/// The prefix every private alias carries.
///
/// Recognisable rather than exotic: someone reading a stack trace or a font
/// dump should be able to attribute the name to this library instead of taking
/// it for corruption. What the name has to be is one no system font manager
/// answers `match_family_style` for, and any ordinary string the system does
/// not have satisfies that: measured, CoreText answers `None` for every name
/// not installed, including all six generics, and fontconfig for every one
/// not installed except `serif`, `sans-serif` and `monospace`, which it
/// aliases to faces of its choosing.
const PRIVATE_ALIAS_PREFIX: &str = "meo-skia-canvas private family: ";

/// The second name a claimed family is registered under.
///
/// Derived rather than stored, so there is no table to fall out of step with
/// `self.fonts` and nothing to rebuild when a registration changes. It is a
/// pure function of the claimed name, which makes it stable for as long as the
/// name is -- more than the process lifetime this needs.
///
/// A caller could in principle register under a name that already starts with
/// the prefix and collide with the private alias of its own suffix. That is a
/// deliberate non-guard: the name is absurd enough that a check for it would
/// cost every reader more than the case is worth.
fn private_alias(claimed: &str) -> String {
    format!("{PRIVATE_ALIAS_PREFIX}{claimed}")
}

/// The name a generic family's faces are registered under in a collection,
/// and asked for by: its keyword after [`PRIVATE_ALIAS_PREFIX`].
///
/// Not the keyword, so that a family named `serif` in quotes -- a family
/// name, by CSS Fonts 4 -- never reaches the faces standing in for the
/// generic. Not the keyword after U+0000 either, a prefix no CSS name can
/// spell: SkParagraph hands each family name to the font managers as a C
/// string -- `matchFamily(familyName.c_str())` and
/// `matchFamilyStyle(familyName.c_str(), ...)` in
/// skparagraph/src/FontCollection.cpp, lines 187 and 266 at skia-safe
/// 0.153.3 -- so the name would arrive empty. The prefix's collision
/// stance is [`private_alias`]'s.
pub(crate) fn generic_name(generic: GenericFamily) -> String {
    private_alias(generic.to_css())
}

/// Registers `face` in `provider` under `alias`, or its own family name
/// without one -- and, where the alias is a generic keyword, under
/// [`generic_name`] too: a claim on a generic sets the generic's faces,
/// which are asked for by that name.
fn register_under(
    provider: &mut TypefaceFontProvider,
    face: Typeface,
    alias: Option<&str>,
) {
    if let Some(generic) = alias.and_then(GenericFamily::from_css) {
        provider.register_typeface(
            face.clone(),
            Some(generic_name(generic).as_str()),
        );
    }
    provider.register_typeface(face, alias);
}

/// The name a family that spells a generic keyword is asked for by where no
/// family of that name exists: one nothing is registered under, so the list
/// moves on to its next family, as CSS has a missing family do.
pub(crate) fn unmatched_name(family: &str) -> String {
    private_alias(&format!("unmatched {family}"))
}

/// Whether `alias` is one the caller has already claimed.
///
/// Compared without case, because a family name is case-insensitive: a
/// caller writing `"Sans-Serif"` claims the same name the curated stack
/// would otherwise be filed under.
fn claims(claimed: &[String], alias: Option<&str>) -> bool {
    alias.is_some_and(|name| {
        claimed.iter().any(|held| held.eq_ignore_ascii_case(name))
    })
}

impl FontLibrary {
    /// A library holding `fonts` and nothing derived from them yet.
    fn with_fonts(fonts: Vec<(Typeface, Option<String>)>) -> Self {
        FontLibrary {
            mgr: FontMgr::default(),
            epoch: Rc::new(FontEpoch {
                fonts: fonts.clone(),
            }),
            fonts,
            collections: [None, None],
            collection_parts: None,
            collection_cache: CollectionCache::default(),
            instances: InstanceCache::default(),
            resolved_fonts: ResolvedFontCache::default(),
            collection_hinted: false,
            generics_cache: vec![],
            positions: InstancedPositions::default(),
            system_families: None,
        }
    }

    /// The registered fonts as they stand now.
    pub fn epoch(&self) -> Rc<FontEpoch> {
        self.epoch.clone()
    }

    /// [`fonts_for_style`](Self::fonts_for_style) against the fonts
    /// registered in `epoch`, which need not be the ones registered now.
    ///
    /// The shared library answers while `epoch` is current. After a change,
    /// a library is built from `epoch`'s fonts for the one search and
    /// dropped, its instances forgotten with it: a run laid out again after
    /// a font was registered is laid out in the faces it was first laid out
    /// in, rather than reporting one font's ink beside another's width.
    pub fn fonts_for_style_in(
        epoch: &Rc<FontEpoch>,
        hinting: bool,
        style: &TextStyle,
        variations: &[(FourByteTag, f32)],
    ) -> (FontCollection, Option<FontStyle>) {
        let current = FontLibrary::with_shared(|lib| {
            Rc::ptr_eq(&lib.epoch, epoch).then(|| {
                lib.set_hinting(hinting).fonts_for_style(style, variations)
            })
        });
        current.unwrap_or_else(|| {
            let mut then = FontLibrary::with_fonts(epoch.fonts.clone());
            let found =
                then.set_hinting(hinting).fonts_for_style(style, variations);
            then.instances.clear();
            found
        })
    }

    pub fn with_shared<T, F>(f: F) -> T
    where
        F: FnOnce(&mut FontLibrary) -> T,
    {
        LIBRARY.with(|lib_lock| {
            let shared_lib = lib_lock.get_or_init(|| {
                // Covers the crate channel, which has no entry point to run
                // this from, and any thread reaching a font before the Neon
                // one has. Idempotent, so the ordinary case pays a `Once`.
                install_fontconfig_fallback();

                RefCell::new(FontLibrary::with_fonts(vec![]))
            });

            f(&mut shared_lib.borrow_mut())
        })
    }

    /// The collection over every registered and generic face for the
    /// hinting [`set_hinting`](Self::set_hinting) last chose, made on first
    /// use.
    pub fn font_collection(&mut self) -> FontCollection {
        let slot = usize::from(self.collection_hinted);
        if self.collections[slot].is_none() {
            self.collections[slot] = Some(self.new_font_collection());
        };

        // SAFETY: the `if` above assigns `Some` when the slot is `None`, so
        // it is `Some` on every path to here.
        self.collections[slot].as_ref().unwrap().clone()
    }

    /// A collection over the registered and generic faces, falling back to
    /// the system's.
    ///
    /// The faces and the default family come from [`collection_parts`],
    /// which builds them once per set of registered fonts. Every
    /// instanced collection [`fonts_for_style`] makes is one of these, so
    /// building them per collection put a font-manager search on every
    /// font size a face with an `opsz` axis had not been drawn at: about
    /// 100 microseconds on Linux, where the system manager is fontconfig,
    /// and a few on macOS.
    ///
    /// [`collection_parts`]: Self::collection_parts
    /// [`fonts_for_style`]: Self::fonts_for_style
    fn new_font_collection(&mut self) -> FontCollection {
        let (assets, default_family) = self.collection_parts();
        let mut collection = FontCollection::new();
        // What a list naming no available family draws in: this library's
        // `sans-serif`, then the face `collection_parts` chose. Named in
        // full rather than appended to, because SkParagraph seeds the list
        // with its own `DEFAULT_FONT_FAMILY`, the keyword `sans-serif`,
        // which the curated faces are not registered under -- they sit
        // under `generic_name` -- so that entry would go to the system
        // manager, where CoreText answers nothing for it and fontconfig a
        // face of its own choosing.
        let defaults: Vec<String> =
            iter::once(generic_name(GenericFamily::SansSerif))
                .chain(default_family)
                .collect();
        collection.set_default_font_manager_and_family_names(
            self.mgr.clone(),
            &defaults,
        );
        collection.set_asset_font_manager(Some(assets));
        collection
    }

    /// The provider of registered and generic faces every collection is
    /// built over, and the family its default manager falls back to.
    ///
    /// Shared by every collection rather than built for each: the provider
    /// is not changed once built, and [`invalidate`](Self::invalidate)
    /// drops it with the collections whenever the set of registered fonts
    /// changes.
    fn collection_parts(&mut self) -> (FontMgr, Option<String>) {
        if let Some(parts) = &self.collection_parts {
            return parts.clone();
        }
        let mut assets = TypefaceFontProvider::new();
        let claimed = self.claimed_aliases();
        // Under `generic_name` rather than the keyword, so that a family
        // named after the keyword is not taken for the generic.
        for (font, alias) in self.generics() {
            if claims(&claimed, alias.as_deref()) {
                continue;
            }
            let name = alias
                .as_deref()
                .and_then(GenericFamily::from_css)
                .map(generic_name);
            assets.register_typeface(font.clone(), name.as_deref());
        }
        // No private alias for a claimed name here, deliberately -- only the
        // generic names above, which no caller types -- and this is the one
        // place the two providers differ on purpose. `font_mgr` registers every
        // aliased face a second time under `private_alias` so the SVG
        // rewrite has a name the system font manager cannot answer for;
        // canvas text has no rewrite and no such name to send, so the
        // second registration would buy nothing.
        //
        // It would also cost something. The alias would become a family a
        // caller can type, and one that resolves to neither the registered
        // face nor a fallback: measured, `36px Helvetica` painted the
        // registered face, an unknown family painted the fallback, and the
        // private alias painted a third thing. Canvas resolves a family
        // through the library's own matching rather than through the provider
        // by name, and the alias is not in `self.fonts` for the reasons on
        // `claimed_families`.
        //
        // So making the two providers match here is not a tidy-up. It puts
        // back a typeable name with an unexplained rendering.
        for (font, alias) in &self.fonts {
            register_under(&mut assets, font.clone(), alias.as_deref());
        }

        let mut style_set =
            assets.match_family(generic_name(GenericFamily::SystemUi));
        let default_fam = match style_set.count() > 1 {
            true => style_set.match_style(FontStyle::default()),
            false => self.mgr.legacy_make_typeface(None, FontStyle::default()),
        }
        .map(|f| f.family_name());

        let parts: (FontMgr, Option<String>) = (assets.into(), default_fam);
        self.collection_parts = Some(parts.clone());
        parts
    }

    /// The aliases a caller has registered faces under.
    ///
    /// A curated generic stack is not registered under one of these, and the
    /// order below is deliberate rather than incidental: **an explicit
    /// registration outranks a default.** The curated stacks are this
    /// library's default for what `sans-serif` and its five siblings mean,
    /// which is the job a browser's font preferences do; `FontLibrary.use`
    /// is the application's own configuration, so it sets that preference
    /// rather than overriding a user's. CSS forbids an `@font-face` rule
    /// from claiming a generic keyword for the opposite case -- a document
    /// must not redefine what the person reading it chose -- and that is not
    /// this.
    ///
    /// Restoring the plain order would look like a tidy-up and would put
    /// back a defect: both providers answer `match_family` with the face
    /// registered first, so filing the curated family under a name the
    /// caller has claimed leaves `FontLibrary.use("sans-serif", ..)`
    /// returning the faces it read while the alias still resolves to the
    /// curated stack -- success reported for something that did not
    /// happen.
    fn claimed_aliases(&self) -> Vec<String> {
        self.fonts
            .iter()
            .filter_map(|(_, alias)| alias.clone())
            .collect()
    }

    fn generics(&mut self) -> &Vec<(Typeface, Option<String>)> {
        // set up generic font family mappings
        if self.generics_cache.is_empty() {
            let mut generics = vec![];
            let mut font_stacks = HashMap::new();
            font_stacks.insert(
                "serif",
                vec![
                    "Times",
                    "Nimbus Roman",
                    "Times New Roman",
                    "Tinos",
                    "Noto Serif",
                    "Liberation Serif",
                    "DejaVu Serif",
                    "Source Serif Pro",
                ],
            );
            font_stacks.insert(
                "sans-serif",
                vec![
                    "Avenir Next",
                    "Avenir",
                    "Helvetica Neue",
                    "Helvetica",
                    "Arial Nova",
                    "Arial",
                    "Inter",
                    "Arimo",
                    "Roboto",
                    "Noto Sans",
                    "Liberation Sans",
                    "DejaVu Sans",
                    "Nimbus Sans",
                    "Clear Sans",
                    "Lato",
                    "Cantarell",
                    "Arimo",
                    "Ubuntu",
                ],
            );
            font_stacks.insert(
                "monospace",
                vec![
                    "Cascadia Code",
                    "Source Code Pro",
                    "Menlo",
                    "Consolas",
                    "Monaco",
                    "Liberation Mono",
                    "Ubuntu Mono",
                    "Roboto Mono",
                    "Lucida Console",
                    "Monaco",
                    "Courier New",
                    "Courier",
                ],
            );
            font_stacks.insert(
                "system-ui",
                vec![
                    "Helvetica Neue",
                    "Ubuntu",
                    "Segoe UI",
                    "Fira Sans",
                    "Roboto",
                    "DroidSans",
                    "Tahoma",
                ],
            );
            // see also: https://modernfontstacks.com | https://systemfontstack.com | https://www.ctrl.blog/entry/font-stack-text.html

            // Set up mappings for generic font names based on the first match
            // found on the system
            for (generic_name, families) in font_stacks.into_iter() {
                let best_match = families.iter().find_map(|fam| {
                    let mut style_set = self.mgr.match_family(fam);
                    match style_set.count() > 0 {
                        true => Some(style_set),
                        false => None,
                    }
                });

                let alias = Some(generic_name.to_string());
                if let Some(mut style_set) = best_match {
                    for style_index in 0..style_set.count() {
                        if let Some(font) = style_set.new_typeface(style_index)
                        {
                            generics.push((font, alias.clone()));
                        }
                    }
                }
            }
            self.generics_cache = generics;
        }

        &self.generics_cache
    }

    /// The concrete family each generic name resolves to, as pairs.
    ///
    /// `generics` registers the winner of each curated stack into the
    /// provider under the generic name, so `sans-serif` is a family this
    /// library owns. Since the system manager is asked first -- it has to be,
    /// or a family it cannot resolve takes the process down -- a system that
    /// answers `sans-serif` itself would win instead, and the curated choice
    /// would be lost. Handing this mapping to the SVG parser lets the
    /// document name the concrete family, which nothing else claims.
    ///
    /// One pair per generic rather than one per registered face: `generics`
    /// pushes every style in the matched family, and they all carry the same
    /// family name.
    /// Each name a caller has claimed, with the private alias it is also
    /// registered under.
    ///
    /// Shaped like [`FontLibrary::generic_families`] so a rewrite consumes the
    /// two the same way. The private alias keeps a claimed name and the
    /// system's own faces for that name apart. `font_mgr` registers a system
    /// face for every family the document asks for, so without the rewrite a
    /// claim on `Helvetica` and the system's Helvetica would land in one
    /// family in the provider and `matchStyle` would choose between them by
    /// how near each sits to the requested style. Rewriting the document to
    /// an alias the system cannot hold puts the caller's face in a family of
    /// its own, so it wins because it is the only candidate rather than
    /// because it was the nearer one.
    ///
    /// Never registered into `self.fonts`, so nothing that enumerates the
    /// library can report it -- `families`, `family()` and what `use()`
    /// returns all read that list, and each would otherwise have to filter
    /// this out separately.
    pub(crate) fn claimed_families(&self) -> Vec<(String, String)> {
        let mut seen: Vec<(String, String)> = Vec::new();
        for name in self.claimed_aliases() {
            if seen.iter().any(|(claimed, _)| claimed == &name) {
                continue;
            }
            let alias = private_alias(&name);
            seen.push((name, alias));
        }
        seen
    }

    pub(crate) fn generic_families(&mut self) -> Vec<(String, String)> {
        let mut seen: Vec<(String, String)> = Vec::new();
        for (font, alias) in self.generics() {
            let Some(alias) = alias else { continue };
            if seen.iter().any(|(name, _)| name == alias) {
                continue;
            }
            seen.push((alias.clone(), font.family_name()));
        }
        seen
    }

    pub fn font_mgr(&mut self, wanted: &[String]) -> FontMgr {
        let system = self.mgr.clone();
        self.svg_font_mgr(&system, wanted)
    }

    /// The manager [`FontLibrary::font_mgr`] builds, with the system manager
    /// passed in rather than read from `self`.
    ///
    /// Split out so a test can substitute one that answers nothing. The fault
    /// this shape exists to avoid is invisible on a machine whose system
    /// manager answers a null family -- macOS does -- so a test using the real
    /// one passes whatever this function does. `FontMgr::empty()` here stands
    /// in for the backend that declines, which is what glibc and Windows have,
    /// and makes the difference reproducible anywhere.
    fn svg_font_mgr(&mut self, system: &FontMgr, wanted: &[String]) -> FontMgr {
        // collect non-system fonts in a provider
        let mut dyn_mgr = TypefaceFontProvider::new();

        // A fallback first, so the family a null name resolves to is a
        // sensible one rather than whatever sorts first. It is also the face
        // `ex` is measured against for a document naming no family at all,
        // which is why the second arm matters: with nothing registered the
        // provider answers nothing, `ex_ratio_for` falls back to a constant
        // half an em, and `4ex` renders as `2em`. That is what `an ex is the
        // drawn face's x-height, not half an em` reported on Windows, where
        // the exact 0.5 says the constant was reached.
        //
        // Which of the two ways it was reached is not established: the curated
        // stack may resolve to nothing there, or it may resolve to a face
        // whose `x_height` metric is absent. The second arm below fixes the
        // first and not the second, so a Windows run is what says whether
        // this was enough.
        //
        // The first candidate that both resolves and *measures*, not the first
        // that resolves. Every candidate the curated stack offers, then the
        // system manager's own default, which is total wherever the machine
        // has any font at all.
        //
        // Measured with `x_height_ratio`, the same function `ex_ratio_for`
        // uses to read the ratio back. One definition of "has an x-height",
        // used to choose the face and to measure it, is the point: with two,
        // a face can pass the choosing test and fail the measuring one, and
        // `ex` silently becomes half an em. Windows did exactly that --
        // `4ex` came out at `2em` for a document naming no family, while a
        // named family in the same document measured 0.525.
        //
        // Which way the old chain failed there is not established and does
        // not need to be: whether the stack returned nothing, or returned a
        // face that reports no x-height and whose `x` does not measure, this
        // skips it either way.
        //
        // **`system` substitutes only the second source, not the first.**
        // `font_collection()` reads `self.mgr`, so passing a manager that
        // answers nothing does NOT produce a provider with no faces: the
        // curated stack still resolves through the real one and a fallback is
        // still registered. A comment here used to claim otherwise.
        //
        // Reproducing a backend that declines everything therefore needs the
        // curated stack taken from the substituted manager too -- a
        // `FontCollection` built from it rather than the cached one, which
        // `new_font_collection` composes from `self.mgr` in two places and
        // caches for cost. That is why it has not been done here rather than
        // an oversight; it puts a fresh collection on a per-document path.
        let candidates: Vec<Typeface> = self
            .font_collection()
            .find_typefaces(
                &[
                    GenericFamily::SystemUi,
                    GenericFamily::SansSerif,
                    GenericFamily::Serif,
                ]
                .map(generic_name),
                FontStyle::normal(),
            )
            .into_iter()
            .chain(system.legacy_make_typeface(None, FontStyle::normal()))
            .collect();
        // A face whose own family name is empty cannot be registered at all:
        // `register_typeface(face, None)` reaches
        // `TypefaceFontProvider::registerTypeface(sk_sp<SkTypeface>)`, which
        // reads `getFamilyName` and returns 0 without registering when it is
        // empty. Such a face would be selected here, silently dropped, and
        // family index 0 -- what a null family resolves to -- would become
        // whichever generic registered next.
        let usable = |face: &Typeface| {
            !face.family_name().is_empty()
                && x_height_ratio(face.clone()).is_some()
        };
        let fallback = candidates
            .iter()
            .find(|face| usable(face))
            // Nothing measures. Register the first that at least *can* be
            // registered: a face that draws and reports no x-height still
            // beats an empty provider, which answers a null family with
            // nothing at all. The name check is repeated rather than dropped
            // -- a nameless face here is refused by
            // `TypefaceFontProvider::registerTypeface` and leaves the
            // provider with nothing where it looked like it had something,
            // which is worse than skipping to the next candidate.
            .or_else(|| {
                candidates
                    .iter()
                    .find(|face| !face.family_name().is_empty())
            })
            .cloned();
        if let Some(fallback) = fallback {
            dyn_mgr.register_typeface(fallback, None);
        }

        // add generic mappings & user-loaded fonts
        let claimed = self.claimed_aliases();
        for (font, alias) in self.generics() {
            if claims(&claimed, alias.as_deref()) {
                continue;
            }
            dyn_mgr.register_typeface(font.clone(), alias.as_deref());
        }
        // A claimed face goes in under its private alias and NOT under the
        // name the caller claimed. The document is rewritten to that alias
        // wherever the claim applies, so the plain name is reachable only
        // from a spelling the rewrite deliberately leaves alone -- a list.
        // `font-family="Helvetica, monospace"` means "Helvetica, and failing
        // that a monospace", and Skia asks for the first item; registering
        // the claimed face under `Helvetica` too would answer that with the
        // caller's face and make a list behave like the bare name it is
        // documented not to.
        for (font, alias) in &self.fonts {
            match alias {
                Some(name) => {
                    let private = private_alias(name);
                    dyn_mgr.register_typeface(
                        font.clone(),
                        Some(private.as_str()),
                    );
                }
                None => {
                    dyn_mgr.register_typeface(font.clone(), None);
                }
            }
        }

        // Every family the document names, resolved through the system
        // manager and registered here under that same name.
        //
        // This is what lets the provider stand alone. It answers only what has
        // been registered into it, so without this a document naming a system
        // family -- or a generic, which `family_substitution` rewrites to a
        // concrete family name -- would find nothing and fall back.
        //
        // `match_family` rather than `legacy_make_typeface`, and the
        // difference decides a test. The legacy call is total: it answers
        // every name, returning a default for one the machine does not have.
        // Registering its answer would give a face to
        // `font-family="ZzzNoSuchFamily"` and a different one to a document
        // stating no family at all, where the two have to agree. `match_family`
        // returns nothing for a name the system does not hold, so an
        // unresolvable name stays unregistered and reaches the same fallback
        // as an absent one.
        //
        // The whole style set, not one face: a document may ask for bold or
        // italic, and registering only the face `match_family_style` picked
        // for one style would flatten the rest onto it.
        for name in wanted {
            let mut set = system.match_family(name.as_str());
            for i in 0..set.count() {
                if let Some(face) = set.new_typeface(i) {
                    dyn_mgr.register_typeface(face, Some(name.as_str()));
                }
            }
        }

        // UPSTREAM: skia-safe 0.153.3 -- #210 -- worked around
        // Re-check: cargo test the_provider_alone_answers_a_null_family
        // with the two managers composed again -- an `OrderedFontMgr` holding
        // `system` and then this provider. It dies rather than fails while
        // `SkOrderedFontMgr::onLegacyMakeTypeface` still gates each manager on
        // `fm->matchFamilyStyle(family, style)`, which reaches
        // `TypefaceFontProvider::onMatchFamily` and its unguarded
        // `find(familyName)`. When that stops bypassing the provider's own
        // null guard the two can be composed again and the pre-resolution
        // below becomes unnecessary.
        //
        // The shape of this function is the workaround: it exists to avoid a
        // combination that aborts, not because one manager is better than
        // two. #210 carries the isolation and the reproduction.
        //
        // **The defect is Skia's, not rust-skia's.** All three files are
        // Skia's own, vendored inside `skia-bindings` --
        // `src/utils/SkOrderedFontMgr.cpp`,
        // `modules/skparagraph/src/TypefaceFontProvider.cpp` and
        // `modules/svg/src/SkSVGText.cpp` -- and the same combination written
        // in C++ aborts identically. The version above is the `skia-safe` this
        // tree pins, which is how it is re-testable; it is not a claim about
        // where the code lives.
        //
        // The provider alone, with no `SkOrderedFontMgr` around it. That is
        // the whole of the crash fix and it is not a preference.
        //
        // `SkSVGText.cpp` resolves a family with
        // `fontMgr()->legacyMakeTypeface(family.c_str(), style)` and, when
        // that yields nothing, retries with a null family. `c_str()` is never
        // null, so the retry is where the null comes from, and it happens
        // whenever a family fails to resolve.
        //
        // `TypefaceFontProvider::onLegacyMakeTypeface` guards that null --
        // `if (familyName)`, and it then returns its first registered family.
        // So the provider on its own is safe and total, which is why the
        // fallback face above is registered before anything else: it is what a
        // null family, and any name the system does not hold, resolves to.
        //
        // What was unsafe was wrapping it.
        // `SkOrderedFontMgr::onLegacyMakeTypeface` walks its managers
        // calling `matchFamilyStyle` on each until one answers, and
        // `TypefaceFontProvider::onMatchFamilyStyle` reaches
        // `onMatchFamily`, which does `fRegisteredFamilies.find(familyName)` --
        // constructing a `std::string` from the null pointer. The ordered
        // wrapper therefore routes around the provider's own guard. Putting
        // the system manager first did not fix that; it only meant the
        // provider was asked for a null family solely when the system manager
        // declined one, which macOS and musl do not and glibc and Windows do.
        // That is why this crashed on four of seven release legs and passed
        // on the rest.
        dyn_mgr.into()
    }

    pub(crate) fn families(&self) -> Vec<String> {
        let mut names: Vec<String> = self.mgr.family_names().collect();
        for (font, alias) in &self.fonts {
            names.push(match alias {
                Some(name) => name.clone(),
                None => font.family_name(),
            })
        }
        names.sort();
        names.dedup();
        names
    }

    pub(crate) fn family_details(
        &self,
        family: &str,
    ) -> (Vec<f32>, Vec<String>, Vec<String>) {
        // merge the system fonts and our dynamically added fonts into one list
        // of FontStyles
        let mut dynamic = TypefaceFontProvider::new();
        for (font, alias) in &self.fonts {
            dynamic.register_typeface(font.clone(), alias.as_deref());
        }
        let std_mgr = self.mgr.clone();
        let dyn_mgr: FontMgr = dynamic.into();
        let mut std_set = std_mgr.match_family(family);
        let mut dyn_set = dyn_mgr.match_family(family);
        let std_styles = (0..std_set.count()).map(|i| std_set.style(i));
        let dyn_styles = (0..dyn_set.count()).map(|i| dyn_set.style(i));
        let all_styles = std_styles.chain(dyn_styles);

        // set up a collection to query for variable fonts who specify their
        // weights via the 'wght' axis rather than through distinct
        // files with different FontStyles
        let mut var_fc = FontCollection::new();
        var_fc.set_default_font_manager(self.mgr.clone(), None);
        var_fc.set_asset_font_manager(Some(dyn_mgr));

        // pull style values out of each matching font
        let mut weights: Vec<i32> = vec![];
        let mut widths: Vec<String> = vec![];
        let mut styles: Vec<String> = vec![];
        all_styles.for_each(|(style, _name)| {
            widths.push(from_width(style.width()));
            styles.push(from_slant(style.slant()));
            weights.push(*style.weight());
            if let Some(font) = var_fc.find_typefaces(&[&family], style).first()
            {
                // for variable fonts, report all the 100× sizes they support
                // within their wght range
                weights.append(&mut typeface_wght_range(font));
            }
        });

        // repackage collected values
        widths.sort_by(|a, b| {
            a.replace("normal", "_")
                .partial_cmp(&b.replace("normal", "_"))
                // SAFETY: `partial_cmp` on `String` always returns `Some`.
                .unwrap()
        });
        widths.dedup();
        styles.sort_by(|a, b| {
            a.replace("normal", "_")
                .partial_cmp(&b.replace("normal", "_"))
                // SAFETY: `partial_cmp` on `String` always returns `Some`.
                .unwrap()
        });
        styles.dedup();
        weights.sort_unstable();
        weights.dedup();
        let weights = weights.iter().map(|w| *w as f32).collect();
        (weights, widths, styles)
    }

    /// Adds `font` to the shared registry `ctx.font` resolves against.
    ///
    /// The Rust facade's [`FontLibrary`](crate::font::FontLibrary) registers
    /// here as well as with its own provider, so a family registered through
    /// it is visible to `Context2D::set_font` and not only to paragraphs.
    pub fn register_typeface(&mut self, font: Typeface, alias: Option<String>) {
        self.add_typeface(font, alias);
    }

    fn add_typeface(&mut self, font: Typeface, alias: Option<String>) {
        // replace any previously added font with the same metadata/alias
        if let Some(idx) = self.fonts.iter().position(|(old_font, old_alias)|
      match alias.is_some(){
        true => old_alias == &alias,
        false => old_font.family_name() == font.family_name()
      } && old_font.font_style() == font.font_style()
    ){
      self.fonts.remove(idx);
    }

        // add the new typeface/alias and recreate the FontCollection to include
        // it
        self.fonts.push((font, alias));
        self.invalidate();
    }

    /// The font `canonical` names, if it has been resolved since the
    /// library last changed.
    pub fn resolved(&mut self, canonical: &str) -> Option<ResolvedFont> {
        self.resolved_fonts.get(canonical)
    }

    /// Remembers `font` under another name than its canonical one: the
    /// string a caller set it with.
    pub fn remember(&mut self, name: String, font: ResolvedFont) {
        self.resolved_fonts.insert(name, font);
    }

    /// Whether a family named `name` exists, compared without case: one
    /// registered here, under its alias or its own name, or one the system
    /// lists among its families.
    ///
    /// The system's list is read once and kept until
    /// [`invalidate`](Self::invalidate); the registered fonts are read as
    /// they stand.
    pub(crate) fn has_family(&mut self, name: &str) -> bool {
        let registered = self.fonts.iter().any(|(font, alias)| match alias {
            Some(alias) => alias.eq_ignore_ascii_case(name),
            None => font.family_name().eq_ignore_ascii_case(name),
        });
        registered
            || self
                .system_families
                .get_or_insert_with(|| {
                    self.mgr
                        .family_names()
                        .map(|family| family.to_ascii_lowercase())
                        .collect()
                })
                .contains(&name.to_ascii_lowercase())
    }

    /// The names a font collection of this library is asked for `families`
    /// by, in order.
    ///
    /// A generic is asked for by [`generic_name`], under which its faces are
    /// registered. A name that spells a generic keyword -- quoted, so a
    /// family name by CSS Fonts 4 -- is passed on only where a family of that
    /// name exists, and otherwise by [`unmatched_name`]: fontconfig answers
    /// `serif`, `sans-serif` and `monospace` with faces of its choosing,
    /// which would draw a quoted "serif" as the generic it is not. Any other
    /// name is passed as it is and left to the platform, whose own aliases
    /// stand -- Chrome on Linux takes fontconfig's metric aliases too, such
    /// as Helvetica drawn in Nimbus Sans.
    pub(crate) fn collection_names(
        &mut self,
        families: &[FontFamily],
    ) -> Vec<String> {
        families
            .iter()
            .map(|family| match family {
                FontFamily::Generic(generic) => generic_name(*generic),
                FontFamily::Named(name) => match self.registered_spelling(name)
                {
                    Some(registered) => registered,
                    None if GenericFamily::from_css(name).is_some()
                        && !self.has_family(name) =>
                    {
                        unmatched_name(name)
                    }
                    None => name.clone(),
                },
            })
            .collect()
    }

    /// The name a registered family is filed under, where `name` is that
    /// family in another case.
    ///
    /// CSS Fonts 4 section 5.1: "User agents must match these names case
    /// insensitively". The system managers do, and the provider the
    /// registered faces are filed with matches a name exactly, so a family
    /// registered as `OswaldX` is asked for in that spelling whatever case
    /// the font names it in. Compared ASCII case-insensitively, as
    /// [`has_family`](Self::has_family) and a claim on a generic are; the
    /// section names Unicode's Default Caseless Matching, which also folds
    /// letters outside ASCII and is not done here.
    pub(crate) fn registered_spelling(&self, name: &str) -> Option<String> {
        self.fonts.iter().find_map(|(font, alias)| {
            let filed = alias.clone().unwrap_or_else(|| font.family_name());
            filed.eq_ignore_ascii_case(name).then_some(filed)
        })
    }

    /// Matches `spec` against the library, and remembers the answer.
    ///
    /// `None` where no family in the specification names a font this library
    /// has, which is the case a caller reads as "leave the font alone". Not
    /// remembered: a `FontLibrary::use` that registers the missing family
    /// clears this cache anyway, but a negative left in it would be a
    /// promise about fonts rather than a record of one.
    pub fn resolve(&mut self, mut spec: FontSpec) -> Option<ResolvedFont> {
        spec.collection_names = self.collection_names(&spec.families);
        let typeface = self
            .font_collection()
            .find_typefaces(&spec.collection_names, spec.style())
            .into_iter()
            .next()?;
        let font: ResolvedFont = Arc::new((spec, typeface));
        self.resolved_fonts
            .insert(font.0.canonical.clone(), font.clone());
        Some(font)
    }

    /// Drops everything derived from the set of registered fonts.
    ///
    /// Called wherever that set changes. The collection is rebuilt on the
    /// next request and the collection cache is answers that may no longer
    /// be right.
    ///
    /// The resolved fonts mostly survive a change on their own: what a
    /// layout matches against is the family list, which comes from the CSS
    /// and not from here, and it re-matches through the current collection
    /// every time. The typeface held beside the specification is only read
    /// where that re-match finds nothing. Cleared anyway -- it costs one
    /// call on a path taken when a program registers a font, and the
    /// alternative is a cache whose correctness rests on which of two
    /// lookups happens to win.
    fn invalidate(&mut self) {
        self.system_families = None;
        self.epoch = Rc::new(FontEpoch {
            fonts: self.fonts.clone(),
        });
        decorate::forget();
        self.collections = [None, None];
        self.collection_parts = None;
        self.collection_cache.clear();
        self.instances.clear();
        self.resolved_fonts.clear();
    }

    /// Records that a paragraph builder is about to instance `families` at
    /// `coordinates` through a style's font arguments, and answers whether
    /// the caller has to drop its own collection's caches first.
    ///
    /// Skia keeps every typeface a collection instances that way, and the
    /// builders share the collections handed out here, so past
    /// [`INSTANCED_POSITIONS`](crate::font::INSTANCED_POSITIONS) this drops
    /// the caches of every collection it holds. The caller's may be one the
    /// cache has already let go, which is why it is asked to drop its own.
    pub fn admit_position(
        &mut self,
        families: &[&str],
        coordinates: &[Coordinate],
    ) -> bool {
        let full = self.positions.admit(families, coordinates);
        if full {
            self.collection_cache.clear_caches();
            for collection in self.collections.iter_mut().flatten() {
                collection.clear_caches();
            }
        }
        full
    }

    /// Chooses the collections handed out from here on: those for text
    /// drawn hinted, or unhinted -- see [`text_hinting`]. Every caller
    /// states which before [`fonts_for_style`](Self::fonts_for_style).
    ///
    /// Two sets rather than one, because a collection's paragraph cache
    /// would hand a paragraph the runs of another that differs from it only
    /// in hinting, shaped and drawn with the other's: the cache matches
    /// styles without comparing their hinting. Kept apart, text of each
    /// hinting keeps every cache of its own, and choosing between them
    /// drops nothing.
    // UPSTREAM: skia-safe 0.153.3 -- https://github.com/google/skia/commit/e0d79efbf171fdd78063f7673d517491154c9a7a -- worked around
    // Re-check: cargo test hinted_and_unhinted_text_keep_their_own_hinting
    pub fn set_hinting(&mut self, hinting: bool) -> &mut Self {
        self.collection_hinted = hinting;
        self
    }

    /// The collection a style should be laid out with, and the face the
    /// style matched first.
    ///
    /// Two answers from one search. Skia's paragraph builder synthesises a
    /// bold or an italic where the face it finds is not the weight or slant
    /// that was asked for, so a caller decides from the match what it may
    /// synthesise -- which used to mean searching the collection a second
    /// time, in `Typesetter::layout`, on every call. It is the same search:
    /// for a family with no variable font in it, the collection handed back
    /// here is the one that was just searched.
    pub fn fonts_for_style(
        &mut self,
        style: &TextStyle,
        variations: &[(FourByteTag, f32)],
    ) -> (FontCollection, Option<FontStyle>) {
        let families = style.font_families();
        let families: Vec<&str> = families.iter().collect();
        let matches = self
            .font_collection()
            .find_typefaces(&families, style.font_style());

        let opsz_tag = FourByteTag::from_chars('o', 'p', 's', 'z');
        // One pass over the matches' axes, for both questions the cache key
        // needs answered before the cache can be asked.
        let (variable, sized) = matches.iter().filter_map(variation_axes).fold(
            (false, false),
            |(_, sized), axes| {
                (true, sized || axes.iter().any(|axis| axis.tag == opsz_tag))
            },
        );

        // if any of the matched typefaces is a variable font, create an
        // instance that matches the current weight settings and add it
        // to a dynamic font mgr
        if variable {
            // memoize the generation of FontCollections for instanced variable
            // fonts
            let explicit_opsz =
                variations.iter().any(|(tag, _)| *tag == opsz_tag);
            let optical = (sized && !explicit_opsz).then(|| style.font_size());
            let key = CollectionKey::new(
                style,
                variations,
                optical,
                self.collection_hinted,
            );
            if let Some(cached) = self.collection_cache.get(&key) {
                return cached;
            }

            // collect any instantiated variable fonts in a TFP to be used as
            // the 'dynamic' font mgr (which is searched before the
            // 'asset' or the 'default' mgr)
            let mut dynamic = TypefaceFontProvider::new();

            for font in matches.into_iter() {
                if let Some(params) = variation_axes(&font) {
                    let coords =
                        instance_coordinates(&params, style, variations);

                    if !coords.is_empty()
                        && let Some(face) =
                            self.instances.instance(&font, &coords)
                    {
                        let alias =
                            self.fonts.iter().find_map(|(orig, alias)| {
                                if Typeface::equal(&font, orig) {
                                    alias.clone()
                                } else {
                                    None
                                }
                            });
                        register_under(&mut dynamic, face, alias.as_deref());
                    }
                }
            }

            let mut collection = self.new_font_collection();
            collection.set_dynamic_font_manager(Some(dynamic.into()));
            // Searched here, in the collection being handed back, because an
            // instanced face reports the weight it was pinned to rather than
            // the one the family declares -- which is the whole point of
            // instancing it, and the reason this cannot reuse the match
            // above.
            let matched = collection
                .clone()
                .find_typefaces(&families, style.font_style())
                .first()
                .map(Typeface::font_style);
            self.collection_cache
                .insert(key, collection.clone(), matched);
            (collection, matched)
        } else {
            // Not variable, so the collection just searched is the one to lay
            // out with and the match is the one already in hand.
            let matched = matches.first().map(Typeface::font_style);
            (self.font_collection(), matched)
        }
    }
}

//
// Javascript Methods
//

pub fn get_families(mut cx: FunctionContext) -> JsResult<JsArray> {
    strings_to_array(&mut cx, &FontLibrary::with_shared(|lib| lib.families()))
}

pub fn has(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let family = string_arg(&mut cx, 1, "familyName")?;
    let found =
        FontLibrary::with_shared(|lib| lib.families().contains(&family));
    Ok(cx.boolean(found))
}

pub fn family(mut cx: FunctionContext) -> JsResult<JsValue> {
    let family = string_arg(&mut cx, 1, "familyName")?;
    let (weights, widths, styles) =
        FontLibrary::with_shared(|lib| lib.family_details(&family));

    if weights.is_empty() {
        return Ok(cx.undefined().upcast());
    }

    let name = cx.string(family);
    let weights = floats_to_array(&mut cx, &weights)?;
    let widths = strings_to_array(&mut cx, &widths)?;
    let styles = strings_to_array(&mut cx, &styles)?;

    let details = JsObject::new(&mut cx);
    let attr = cx.string("family");
    details.set(&mut cx, attr, name)?;
    let attr = cx.string("weights");
    details.set(&mut cx, attr, weights)?;
    let attr = cx.string("widths");
    details.set(&mut cx, attr, widths)?;
    let attr = cx.string("styles");
    details.set(&mut cx, attr, styles)?;

    Ok(details.upcast())
}

/// The typeface in the file at `filename`, or the message `use()` throws.
fn read_typeface(filename: &str) -> Result<Typeface, String> {
    let path = Path::new(filename);
    let bytes = fs::read(path)
        .map_err(|why| format!("{}: \"{}\"", why, path.display()))?;
    #[cfg(target_os = "windows")]
    let bytes = {
        fn decode_woff(bytes: &Vec<u8>) -> Option<Vec<u8>> {
            let woff = ReadScope::new(&bytes).read::<WoffFont>().ok()?;
            let tags = woff.table_tags()?;
            whole_font(&woff, &tags).ok()
        }

        fn decode_woff2(bytes: &Vec<u8>) -> Option<Vec<u8>> {
            let woff2 = ReadScope::new(&bytes).read::<Woff2Font>().ok()?;
            let tables = woff2.table_provider(0).ok()?;
            let tags = tables.table_tags()?;
            whole_font(&tables, &tags).ok()
        }

        match filename.to_ascii_lowercase() {
            name if name.ends_with(".woff") => decode_woff(&bytes),
            name if name.ends_with(".woff2") => decode_woff2(&bytes),
            _ => None,
        }
    }
    .unwrap_or(bytes);

    FontLibrary::with_shared(|lib| {
        lib.mgr.new_from_data(Data::new_copy(&bytes), None)
    })
    .ok_or_else(|| format!("Could not decode font data in {}", path.display()))
}

/// One alias, or none, and the typefaces to file under it, each with the
/// file it came from or `"<buffer>"`.
type FaceGroup = (Option<String>, Vec<(String, Typeface)>);

/// Registers every typeface in `groups`, or none of them, and describes
/// each.
///
/// All or nothing, as the crate's `FontLibrary::use_paths` is: every face is
/// read and parsed before any is registered, so a call that fails leaves the
/// library as it was. Registering as each one parsed kept the faces before
/// the failure -- `use(["Monoton.woff", "missing.ttf"])` threw and left
/// Monoton registered.
fn register_all<'a>(
    cx: &mut FunctionContext<'a>,
    groups: Vec<FaceGroup>,
) -> JsResult<'a, JsArray> {
    let results = JsArray::new(cx, groups.len());
    for (g, (alias, faces)) in groups.into_iter().enumerate() {
        let details = JsArray::new(cx, faces.len());
        for (i, (source, font)) in faces.into_iter().enumerate() {
            let face = typeface_details(cx, &source, &font, alias.clone())?;
            details.set(cx, i as u32, face)?;
            FontLibrary::with_shared(|lib| {
                lib.add_typeface(font, alias.clone())
            });
        }
        results.set(cx, g as u32, details)?;
    }
    Ok(results)
}

/// `FontLibrary.use()` from files: an array of `[alias, paths]` groups, one
/// per family a map names or one for a plain call, answered with one array
/// of face details per group.
pub fn addFamily(mut cx: FunctionContext) -> JsResult<JsValue> {
    let groups = cx.argument::<JsArray>(1)?.to_vec(&mut cx)?;
    let mut parsed = Vec::with_capacity(groups.len());
    for group in groups {
        let group = group.downcast_or_throw::<JsArray, _>(&mut cx)?;
        let alias = group
            .get_value(&mut cx, 0)?
            .downcast::<JsString, _>(&mut cx)
            .ok()
            .map(|alias| alias.value(&mut cx));
        let files: Handle<JsArray> = group.get(&mut cx, 1)?;
        let files = files.to_vec(&mut cx)?;
        let mut faces = Vec::with_capacity(files.len());
        for filename in strings_in(&mut cx, &files) {
            match read_typeface(&filename) {
                Ok(font) => faces.push((filename, font)),
                Err(why) => return cx.throw_error(why),
            }
        }
        parsed.push((alias, faces));
    }
    Ok(register_all(&mut cx, parsed)?.upcast())
}

/// `FontLibrary.use()` from buffers, all filed under `alias` or each under
/// the family it declares, and all or none of them registered.
pub fn addFamilyFromData(mut cx: FunctionContext) -> JsResult<JsValue> {
    let alias = opt_string_arg(&mut cx, 1);
    let buffers = cx.argument::<JsArray>(2)?.to_vec(&mut cx)?;
    let mut faces = Vec::with_capacity(buffers.len());
    for buf_val in buffers.iter() {
        let buf = buf_val.downcast_or_throw::<JsBuffer, _>(&mut cx)?;
        let bytes = buf.as_slice(&cx).to_vec();
        match FontLibrary::with_shared(|lib| {
            lib.mgr.new_from_data(Data::new_copy(&bytes), None)
        }) {
            Some(font) => faces.push(("<buffer>".to_string(), font)),
            None => {
                return cx
                    .throw_error("Could not decode font data from buffer");
            }
        }
    }
    let results = register_all(&mut cx, vec![(alias, faces)])?;
    results.get_value(&mut cx, 0)
}

pub fn reset(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    FontLibrary::with_shared(|lib| {
        lib.fonts.clear();
        lib.invalidate();
    });

    Ok(cx.undefined())
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        font_style::{Weight, Width},
        textlayout::{ParagraphBuilder as SkParagraphBuilder, ParagraphStyle},
    };

    use super::*;
    use crate::font::INSTANCED_POSITIONS;

    /// The manager handed to `SkSVGDOM` answers a null family.
    ///
    /// `SkSVGText.cpp` asks for a family by name and, when nothing answers,
    /// asks again with a null one. That second call reached
    /// `TypefaceFontProvider::onMatchFamily`, which builds a `std::string`
    /// from the pointer, and took the process down -- SIGABRT under
    /// libstdc++, SIGSEGV under libc++. Four of seven release legs died on
    /// it, reported as six suite files failing with no failing assertion.
    ///
    /// **What this does not catch.** On a platform whose system font manager
    /// answers a null family -- macOS does, and musl did -- the old ordered
    /// manager answered it too, so this passes there either way. It pins the
    /// property the fix rests on rather than reproducing the crash. What
    /// reproduces the crash on any platform is putting the provider behind a
    /// manager that declines: `FontMgr::empty()` in an `OrderedFontMgr` ahead
    /// of it kills this process on macOS. That cannot be a test, because the
    /// failure is a signal rather than a panic and takes the whole binary
    /// with it.
    #[test]
    fn the_svg_font_manager_answers_a_null_family() {
        let mgr = FontLibrary::with_shared(|lib| lib.font_mgr(&[]));
        assert!(
            mgr.legacy_make_typeface(None, FontStyle::default())
                .is_some(),
            "a null family has to resolve to the registered fallback",
        );
    }

    /// The provider answers a null family with no ordered manager around it.
    ///
    /// **This does not reproduce a declining backend, and it once claimed
    /// to.** `FontMgr::empty()` substitutes only the second source of the
    /// fallback; `svg_font_mgr` takes the first from `font_collection()`,
    /// which reads the real system manager. So the provider here is populated
    /// however empty the argument is, and this passes on any machine whose
    /// curated stack resolves. Reproducing the empty case needs the curated
    /// stack taken from the substituted manager as well -- see the note at
    /// the selection.
    ///
    /// What it does pin is the shape: the provider standing alone answers a
    /// null family. Composing it back into an `SkOrderedFontMgr` makes this
    /// die rather than fail, which is checked rather than assumed.
    ///
    /// Measured in the glibc container the release builds in: an SVG naming a
    /// family the machine lacks, and one naming no family at all, both took
    /// the process down with `Rust cannot catch foreign exceptions, aborting`,
    /// while one naming `DejaVu Sans` rendered.
    #[test]
    fn the_provider_alone_answers_a_null_family() {
        let mgr = FontLibrary::with_shared(|lib| {
            lib.svg_font_mgr(&FontMgr::empty(), &[])
        });
        assert!(
            mgr.legacy_make_typeface(None, FontStyle::default())
                .is_some(),
            "the provider has to answer a null family on its own",
        );
    }

    /// A name the system does not hold resolves to the same face as no name
    /// at all.
    ///
    /// The property `renders when the family cannot be resolved` pins from
    /// the JavaScript side, stated here in the terms the provider is built
    /// in. It is why the pre-resolution uses `match_family` rather than
    /// `legacy_make_typeface`: the latter answers every name, so registering
    /// its answer would give an unresolvable family a face of its own and
    /// these two would part.
    #[test]
    fn an_unresolvable_family_lands_where_an_absent_one_does() {
        let wanted = vec!["ZzzNoSuchFamilyAnywhere".to_string()];
        let mgr = FontLibrary::with_shared(|lib| lib.font_mgr(&wanted));
        let missing = mgr
            .legacy_make_typeface(
                Some("ZzzNoSuchFamilyAnywhere"),
                FontStyle::default(),
            )
            .map(|face| face.family_name());
        let absent = mgr
            .legacy_make_typeface(None, FontStyle::default())
            .map(|face| face.family_name());
        assert!(missing.is_some(), "the fallback has to answer at all");
        assert_eq!(missing, absent, "both have to reach the same fallback");
    }

    /// A key that is cheap to build, since the cache does not read it.
    fn key(n: i32) -> CollectionKey {
        CollectionKey {
            families: format!("family {n}"),
            weight: 400,
            width: 5,
            slant: Slant::Upright,
            variations: vec![],
            optical_size: None,
            hinted: false,
        }
    }

    #[test]
    fn the_collection_cache_evicts_the_least_recently_used() {
        // The map was unbounded and only ever cleared wholesale, so a page
        // tweening an axis added an entry per frame for the life of the
        // process. What matters about the bound is which entry goes: least
        // recently *used*, not least recently inserted, or a page alternating
        // between two families would drop whichever it saw first.
        let mut cache = CollectionCache::default();
        let collection = FontCollection::new();

        for n in 0..COLLECTION_CACHE_SIZE as i32 {
            cache.insert(key(n), collection.clone(), None);
        }
        assert_eq!(cache.entries.len(), COLLECTION_CACHE_SIZE);

        // Touch the oldest so it is no longer the least recently used, then
        // overflow by one. The second-oldest is what should go.
        assert!(cache.get(&key(0)).is_some());
        cache.insert(
            key(COLLECTION_CACHE_SIZE as i32),
            collection.clone(),
            None,
        );

        assert_eq!(cache.entries.len(), COLLECTION_CACHE_SIZE, "still bounded");
        assert!(cache.get(&key(0)).is_some(), "the touched entry survived");
        assert!(
            cache.get(&key(1)).is_none(),
            "the stalest entry was dropped"
        );
        assert!(
            cache.get(&key(COLLECTION_CACHE_SIZE as i32)).is_some(),
            "the new entry is in"
        );
    }

    #[test]
    fn an_instance_places_every_axis_of_the_face() {
        // Every axis, so a pushed style's position replaces the paragraph's
        // rather than moving only the axes it names over it.
        let bytes = std::fs::read("tests/assets/fonts/AmstelvarAlpha-VF.ttf")
            .expect("bundled Amstelvar is readable");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Amstelvar parses");
        let axes = variation_axes(&face).expect("Amstelvar is variable");
        let [wght, wdth, opsz] = [b"wght", b"wdth", b"opsz"].map(|t| {
            FourByteTag::from_chars(
                t[0] as char,
                t[1] as char,
                t[2] as char,
                t[3] as char,
            )
        });
        let other = axes
            .iter()
            .find(|axis| ![wght, wdth, opsz].contains(&axis.tag))
            .expect("Amstelvar has axes beyond the three CSS sets");
        let mut style = TextStyle::new();
        style.set_font_size(48.0);
        style.set_font_style(FontStyle::new(
            Weight::from(400),
            Width::CONDENSED,
            Slant::Upright,
        ));
        let value_of = |coordinates: &[Coordinate], tag: FourByteTag| {
            coordinates
                .iter()
                .find(|coordinate| coordinate.axis == tag)
                .map(|coordinate| coordinate.value)
        };

        let unnamed = instance_coordinates(&axes, &style, &[]);
        assert_eq!(unnamed.len(), axes.len(), "one position per axis");
        assert_eq!(value_of(&unnamed, wght), Some(400.0), "wght is the weight");
        assert_eq!(
            value_of(&unnamed, wdth),
            Some(75.0),
            "wdth is the stretch, condensed being 75%"
        );
        assert_eq!(value_of(&unnamed, opsz), Some(48.0), "opsz is the size");
        assert_eq!(
            value_of(&unnamed, other.tag),
            Some(other.def),
            "an axis nothing sets is at its default"
        );

        let named = instance_coordinates(
            &axes,
            &style,
            &[
                (wght, 300.0),
                (other.tag, other.max + 1000.0),
                (wght, 700.0),
            ],
        );
        assert_eq!(
            value_of(&named, wght),
            Some(700.0),
            "the last of two `wght` entries wins"
        );
        assert_eq!(
            value_of(&named, other.tag),
            Some(other.max),
            "a named value past its axis is pinned to it"
        );
    }

    #[test]
    fn a_builders_positions_past_the_bound_drop_every_held_collection() {
        // A paragraph builder instances a pushed style through font arguments
        // on whichever collection it was handed, cached or not, and Skia
        // keeps each typeface that makes. Laying a paragraph out through a
        // collection caches it there, which is what shows the drop.
        const FAMILY: &str = "AmstelvarBuilderBound";
        let bytes = std::fs::read("tests/assets/fonts/AmstelvarAlpha-VF.ttf")
            .expect("bundled Amstelvar is readable");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Amstelvar parses");
        let cache_paragraph = |collection: &FontCollection| {
            let mut builder =
                SkParagraphBuilder::new(&ParagraphStyle::new(), collection);
            builder.add_text("Hamburgefonstiv");
            builder.build().layout(f32::MAX);
        };
        let opsz = FourByteTag::from_chars('o', 'p', 's', 'z');
        let at = |n: usize| {
            [Coordinate {
                axis: opsz,
                value: 10.0 + n as f32 * 0.25,
            }]
        };

        FontLibrary::with_shared(|lib| {
            lib.register_typeface(face, Some(FAMILY.to_string()));
            let mut style = TextStyle::new();
            style.set_font_families(&[FAMILY]);
            let (mut instanced, _) = lib.fonts_for_style(&style, &[]);
            let mut main = lib.font_collection();
            cache_paragraph(&instanced);
            cache_paragraph(&main);
            let held = |instanced: &mut FontCollection,
                        main: &mut FontCollection| {
                (
                    instanced.paragraph_cache_mut().count(),
                    main.paragraph_cache_mut().count(),
                )
            };
            assert_eq!(held(&mut instanced, &mut main), (1, 1));

            for n in 0..INSTANCED_POSITIONS {
                assert!(!lib.admit_position(&[FAMILY], &at(n)), "position {n}");
            }
            assert!(
                !lib.admit_position(&[FAMILY], &at(0)),
                "a position already recorded drops nothing"
            );
            assert_eq!(
                held(&mut instanced, &mut main),
                (1, 1),
                "nothing is dropped up to the bound"
            );

            assert!(
                lib.admit_position(&[FAMILY], &at(INSTANCED_POSITIONS)),
                "one past the bound asks the caller to drop its own"
            );
            assert_eq!(
                held(&mut instanced, &mut main),
                (0, 0),
                "and the cached and the main collection are dropped here"
            );
        });
    }

    #[test]
    fn an_instance_outlives_its_collection_until_the_byte_bound() {
        // The collection cache lets a collection go after 128 others; the
        // instance in it is kept until the instances held reach their byte
        // bound, so the same position finds the same typeface again, and
        // what the face's instances share stays answerable by the face.
        const FAMILY: &str = "AmstelvarInstanceHeld";
        let bytes = std::fs::read("tests/assets/fonts/AmstelvarAlpha-VF.ttf")
            .expect("bundled Amstelvar is readable");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Amstelvar parses");
        let held = INSTANCE_CACHE_BYTES / INSTANCE_BYTES_ESTIMATE;
        // Distinct positions: Amstelvar's `opsz` runs 10 to 72, so every size
        // here lands on its own coordinate.
        let size = |step: usize| 10.0 + step as f32 * 0.05;
        assert!(size(held + COLLECTION_CACHE_SIZE + 1) < 72.0);
        let style = |size: f32| {
            let mut style = TextStyle::new();
            style.set_font_families(&[FAMILY]);
            style.set_font_size(size);
            style
        };

        FontLibrary::with_shared(|lib| {
            lib.register_typeface(face.clone(), Some(FAMILY.to_string()));
            let instance_at = |lib: &mut FontLibrary, size: f32| {
                let (mut collection, _) =
                    lib.fonts_for_style(&style(size), &[]);
                collection
                    .find_typefaces(&[FAMILY], FontStyle::default())
                    .first()
                    .map(Typeface::unique_id)
                    .expect("the family resolves")
            };
            let base = face.unique_id();

            let first = instance_at(lib, size(0));
            assert_ne!(first, base, "an instance is a face of its own");
            assert_eq!(base_face(first), base);

            for step in 1..=COLLECTION_CACHE_SIZE {
                instance_at(lib, size(step));
            }
            assert_eq!(
                instance_at(lib, size(0)),
                first,
                "a position whose collection was let go finds its instance"
            );

            for step in 1..=held {
                instance_at(lib, size(COLLECTION_CACHE_SIZE + step));
            }
            assert_eq!(lib.instances.entries.len(), held);
            assert!(lib.instances.bytes <= INSTANCE_CACHE_BYTES);
            assert_eq!(
                base_face(first),
                first,
                "past the bound the least recently used instance is let go"
            );
            assert_ne!(
                instance_at(lib, size(0)),
                first,
                "and its position is made again"
            );

            let last = instance_at(lib, size(1));
            assert_eq!(base_face(last), base);
            lib.instances.clear();
            assert_eq!(lib.instances.bytes, 0);
            assert_eq!(base_face(last), last, "clearing forgets every one");
        });
    }

    #[test]
    fn a_size_per_call_on_an_optical_axis_stays_within_the_cache() {
        // Each size instances its own `opsz`, so a caller animating a font
        // size asks for a collection no earlier call built. The map has to
        // stay at its bound and let the oldest go, or every frame of the
        // animation stays resident.
        const FAMILY: &str = "AmstelvarCacheBound";
        const SIZES: usize = 1000;
        let bytes = std::fs::read("tests/assets/fonts/AmstelvarAlpha-VF.ttf")
            .expect("bundled Amstelvar is readable");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Amstelvar parses");
        let style = |size: f32| {
            let mut style = TextStyle::new();
            style.set_font_families(&[FAMILY]);
            style.set_font_size(size);
            style
        };
        let first = style(10.0);

        FontLibrary::with_shared(|lib| {
            lib.register_typeface(face, Some(FAMILY.to_string()));
            lib.fonts_for_style(&first, &[]);
            let first_key = CollectionKey::new(&first, &[], Some(10.0), false);
            assert!(
                lib.collection_cache.entries.contains_key(&first_key),
                "the first size is cached under the key it was built for"
            );
            for step in 1..SIZES {
                lib.fonts_for_style(&style(10.0 + step as f32 * 0.05), &[]);
            }
            assert_eq!(
                lib.collection_cache.entries.len(),
                COLLECTION_CACHE_SIZE,
                "a thousand sizes leave the map at its bound"
            );
            assert!(
                !lib.collection_cache.entries.contains_key(&first_key),
                "and the size asked for first is the one let go"
            );
        });
    }

    #[test]
    fn a_font_registered_after_a_collection_is_built_is_found() {
        // Every collection shares the provider of registered faces, so a
        // registration has to replace it, not only the collections built
        // over it. A static face, so the answer comes from the provider and
        // not from an instance carrying its alias.
        let face = |path: &str| {
            let bytes = std::fs::read(path).expect(path);
            FontMgr::new()
                .new_from_data(Data::new_copy(&bytes), None)
                .expect(path)
        };
        let style = |family: &str| {
            let mut style = TextStyle::new();
            style.set_font_families(&[family]);
            style
        };
        let monoton = face("tests/assets/fonts/Monoton-Regular.woff");

        FontLibrary::with_shared(|lib| {
            lib.register_typeface(
                face("tests/assets/fonts/AmstelvarAlpha-VF.ttf"),
                Some("PartsFirst".to_string()),
            );
            lib.fonts_for_style(&style("PartsFirst"), &[]);
            lib.font_collection();

            lib.register_typeface(
                monoton.clone(),
                Some("PartsLater".to_string()),
            );
            let (mut collection, _) =
                lib.fonts_for_style(&style("PartsLater"), &[]);
            assert_eq!(
                collection
                    .find_typefaces(&["PartsLater"], FontStyle::default())
                    .first()
                    .map(Typeface::family_name),
                Some(monoton.family_name()),
            );
        });
    }

    #[test]
    fn re_inserting_a_key_does_not_evict() {
        // A repeated draw at the same style writes the same key back. That is
        // a replacement, not growth, and must not cost an unrelated entry.
        let mut cache = CollectionCache::default();
        let collection = FontCollection::new();
        for n in 0..COLLECTION_CACHE_SIZE as i32 {
            cache.insert(key(n), collection.clone(), None);
        }
        cache.insert(key(0), collection.clone(), None);
        assert_eq!(cache.entries.len(), COLLECTION_CACHE_SIZE);
        for n in 0..COLLECTION_CACHE_SIZE as i32 {
            assert!(cache.get(&key(n)).is_some(), "entry {n} survived");
        }
    }

    #[test]
    fn every_width_class_maps_to_the_percentage_css_names() {
        // The nine OS/2 classes and the CSS Fonts 4 keywords they carry.
        // Restated here rather than read out of the table, so a reordered
        // row fails instead of agreeing with itself.
        for (class, percent, keyword) in [
            (1, 50.0, "ultra-condensed"),
            (2, 62.5, "extra-condensed"),
            (3, 75.0, "condensed"),
            (4, 87.5, "semi-condensed"),
            (5, 100.0, "normal"),
            (6, 112.5, "semi-expanded"),
            (7, 125.0, "expanded"),
            (8, 150.0, "extra-expanded"),
            (9, 200.0, "ultra-expanded"),
        ] {
            assert_eq!(
                stretch_percent(class),
                percent,
                "class {class} is {keyword}"
            );
        }
    }

    #[test]
    fn a_class_the_standard_does_not_define_falls_back_to_normal() {
        // The same answer class 5 gives, which is why the two used to share
        // an arm. They no longer do: a font declaring 0 or 12 is malformed
        // and a font declaring 5 is not, and the code should be able to say
        // which case it is in even when the number it returns is the same.
        for class in [i32::MIN, -1, 0, 10, 12, 255, i32::MAX] {
            assert_eq!(
                stretch_percent(class),
                stretch_percent(5),
                "class {class}"
            );
        }
    }

    #[test]
    fn the_table_runs_from_condensed_to_expanded_without_repeating() {
        // A transposed pair would still pass the lookup test above if it
        // were read out of the table; this asks the question the table
        // exists to answer.
        assert!(
            WIDTH_CLASS_PERCENT.windows(2).all(|pair| pair[0] < pair[1]),
            "{WIDTH_CLASS_PERCENT:?} is not strictly increasing"
        );
        assert_eq!(WIDTH_CLASS_PERCENT[4], 100.0, "the middle class is normal");
    }
}
