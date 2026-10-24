use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use parking_lot::Mutex;
use skia_safe::{
    Data, FontMgr, FourByteTag, Typeface,
    font_arguments::variation_position::Coordinate,
    font_parameters::variation::Axis, font_style::Slant,
    textlayout::TypefaceFontProvider,
};

// The process-wide registry the JavaScript `FontLibrary.use()` writes to.
// Aliased because the public type below now carries the same name: this one
// is the shared store, that one is the handle a caller holds.
use crate::{
    context2d::{FontSlant, FontStretch},
    error::Error,
    node::font_library::FontLibrary as SharedFontRegistry,
};

/// Four-byte OpenType axis tag (e.g. `"wght"`, `"wdth"`, `"opsz"`).
///
/// Use `FontAxisTag::new(b"wght")` for an axis-tag literal or the
/// `WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL` associated constants for
/// the common cases. `FontAxisTag::from_str("wght")` accepts a 4-byte
/// ASCII string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontAxisTag([u8; 4]);

/// The slant to ask Skia for when matching a face.
///
/// UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
/// Re-check: make the match below pass `Oblique` through unchanged, then
/// cargo test oblique_falls_back_to_the_italic_face. The test passes with the
/// substitution in place whatever Skia's matcher does, so running it
/// unchanged answers a different question.
///
/// `Oblique` is passed through as `Italic`. Skia's matcher does not fall back
/// from oblique to italic: asking for `Slant::Oblique` on a family with no
/// oblique face returns the upright one, so an oblique run painted exactly
/// what an upright one painted. Chrome 148 renders such a request as the
/// italic face, which is what CSS Fonts 4 asks for: an oblique request
/// prefers an oblique face and falls back to an italic one before an upright
/// one.
///
/// The identity is the whole of the change and needs no pixel count to state:
/// an oblique run was byte-identical to an upright one and is byte-identical
/// to an italic one. That holds for whatever is drawn, where a count holds
/// only for the scene it was taken on -- `CHANGELOG-crate.md` carries one
/// such measurement with its canvas, string, origin and ink threshold named.
///
/// This substitution is for matching only. The slant a caller set is stored
/// and reported unchanged, so `ctx.font` still reads back `oblique` and
/// [`FontSlant`](crate::context2d::FontSlant) keeps three distinct values.
///
/// Lives here because both surfaces resolve faces and both were wrong. The
/// binding had its own copy first and the crate kept the old behaviour for a
/// release-candidate's worth of time, which is the shape of defect two
/// translations of one concept produce.
///
/// The cost is a family shipping a true oblique face *and* a separate italic,
/// which now gets the italic. No face on any platform this project builds for
/// is known to do that, and rendering upright was wrong for every family
/// rather than one.
pub(crate) fn slant_for_matching(slant: Slant) -> Slant {
    match slant {
        Slant::Oblique => Slant::Italic,
        other => other,
    }
}

/// The variation axes `face` has, or `None` for a face with none.
///
/// `Typeface::variation_design_parameters` answers a static face
/// differently by platform. FreeType and DirectWrite report zero axes, which
/// is what `SkTypeface::getVariationDesignParameters` documents -- "the
/// number of axes, or -1 if there is an error" -- and CoreText reports the
/// -1, which skia-safe turns into `None`. So `Some` holds an empty list for
/// every static face on Linux and Windows, and asking whether a face is
/// variable by `is_some()` answers yes for all of them there. Ask through
/// this instead.
pub(crate) fn variation_axes(face: &Typeface) -> Option<Vec<Axis>> {
    face.variation_design_parameters()
        .filter(|axes| !axes.is_empty())
}

/// The `opsz` position for text set at `font_size` CSS pixels in a face with
/// `axes`: the size, clamped to the axis. `None` for a face with no `opsz`
/// axis.
///
/// What CSS `font-optical-sizing: auto` asks for, and a canvas has no way to
/// ask for anything else. Chrome 152 instances Amstelvar, whose axis runs 10
/// to 72, at the CSS size: its canvas widths match its DOM's at `"opsz"`
/// equal to the size, and 24px under `scale(2)` measures as 24px, not 48px.
/// A face left uninstanced draws whatever its platform picks instead --
/// FreeType the default instance, CoreText the optical size of its own 12pt
/// default -- which is right at one size at most.
///
/// Callers skip this where the axes asked for name `opsz`, as
/// `font-variation-settings` wins over `font-optical-sizing` in CSS.
pub(crate) fn optical_size(
    axes: &[Axis],
    font_size: f32,
) -> Option<Coordinate> {
    let opsz = FourByteTag::from(u32::from_be_bytes(FontAxisTag::OPSZ.0));
    axes.iter()
        .find(|axis| axis.tag == opsz)
        .map(|axis| Coordinate {
            axis: axis.tag,
            // `max` then `min` rather than `clamp`, which panics on an axis
            // whose minimum a malformed font puts above its maximum.
            value: font_size.max(axis.min).min(axis.max),
        })
}

/// How many variable-font positions a `FontCollection` instances through
/// a text style's `FontArguments` before its caches are dropped.
///
/// Skia's `FontCollection` keeps every typeface it clones for a style's font
/// arguments, in a face cache and a variation cache that nothing evicts and
/// only `clear_caches` empties. `opsz` makes every font size of a face with
/// the axis a position of its own, so a collection laying out an animated
/// size keeps a clone per frame unless something drops them. Measured in
/// release on macOS, the crate's `TextEngine` grows by 43 MB over three
/// thousand Amstelvar sizes and 45 over thirty thousand with this bound, and
/// by 91 and 167 without it; a static font grows by 16 at either count.
///
/// The same bound as the binding's cache of instanced collections, for the
/// same reason: it is more positions than a page uses, and a tween asks for
/// values no later frame asks for again, so a larger bound would only keep
/// them. Dropping the caches costs the next layouts a rebuild of what they
/// still use.
pub(crate) const INSTANCED_POSITIONS: usize = 128;

/// The positions a `FontCollection` has instanced through font arguments
/// since its caches were last dropped. See [`INSTANCED_POSITIONS`].
#[derive(Debug, Default)]
pub(crate) struct InstancedPositions {
    /// Families and coordinates, the coordinates by their bits because Skia
    /// tells two argument lists apart by exact equality.
    seen: HashSet<(String, Vec<(u32, u32)>)>,
}

impl InstancedPositions {
    /// Records that a collection is about to instance `families` at
    /// `coordinates`, and answers whether its caches have to be dropped
    /// first: when that position would be one past the bound. The record
    /// then starts again from it.
    #[must_use]
    pub(crate) fn admit(
        &mut self,
        families: &[&str],
        coordinates: &[Coordinate],
    ) -> bool {
        let key = (
            families.join(", "),
            coordinates
                .iter()
                .map(|coordinate| {
                    (*coordinate.axis, coordinate.value.to_bits())
                })
                .collect(),
        );
        if self.seen.contains(&key) {
            return false;
        }
        let full = self.seen.len() >= INSTANCED_POSITIONS;
        if full {
            self.seen.clear();
        }
        self.seen.insert(key);
        full
    }

    /// How many positions are recorded.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }
}

impl FontAxisTag {
    /// Common italic axis (`ital`).
    pub const ITAL: Self = Self(*b"ital");
    /// Common optical-size axis (`opsz`).
    pub const OPSZ: Self = Self(*b"opsz");
    /// Common slant axis (`slnt`).
    pub const SLNT: Self = Self(*b"slnt");
    /// Common width axis (`wdth`).
    pub const WDTH: Self = Self(*b"wdth");
    /// Common weight axis (`wght`).
    pub const WGHT: Self = Self(*b"wght");

    /// Constructs a tag from a literal `[u8; 4]`.
    ///
    /// Use the `b"wght"` byte-string literal form: `FontAxisTag::new(b"wght")`.
    pub const fn new(bytes: &[u8; 4]) -> Self {
        Self(*bytes)
    }

    /// Borrows the underlying 4-byte tag.
    pub fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

impl std::str::FromStr for FontAxisTag {
    type Err = InvalidFontAxisTag;

    /// Parses from a 4-character ASCII string.
    ///
    /// Use either `"wght".parse::<FontAxisTag>()` or the `FontAxisTag::WGHT` /
    /// `WDTH` / `OPSZ` / `SLNT` / `ITAL` associated constants for compile-time
    /// tags.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let b = s.as_bytes();
        if b.len() == 4 && b.iter().all(u8::is_ascii) {
            Ok(Self([b[0], b[1], b[2], b[3]]))
        } else {
            Err(InvalidFontAxisTag)
        }
    }
}

/// Returned by `FontAxisTag`'s `FromStr` impl when the input is not a
/// 4-character ASCII string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidFontAxisTag;

impl std::fmt::Display for InvalidFontAxisTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FontAxisTag requires exactly 4 ASCII bytes")
    }
}

impl std::error::Error for InvalidFontAxisTag {}

/// Variable-font axis position.
///
/// Mirrors CanvasKit's `fontVariations` shape and the
/// `TextStyleInput.fontVariations` field on the Node addon.
///
/// `value` is interpreted in the font's design space and clamped to
/// the typeface's declared `[min, max]` for that axis at instantiation
/// time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariation {
    /// The four-byte axis tag, e.g. `wght`.
    pub axis: FontAxisTag,
    /// Position along that axis, in the font's design space.
    pub value: f32,
}

impl FontVariation {
    /// Pairs an axis with a position on it.
    pub const fn new(axis: FontAxisTag, value: f32) -> Self {
        Self { axis, value }
    }
}

/// What a font family offers, as `FontLibrary.family()` reports it in
/// JavaScript.
///
/// The lists are of what is actually installed or registered under the name,
/// so a family with one file has one entry in each.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFamily {
    /// The family name as the platform reports it.
    pub family: String,
    /// Weights on offer, in CSS numbering: 400 is regular, 700 bold.
    pub weights: Vec<f32>,
    /// Widths on offer, in CSS `font-stretch` keywords -- `"condensed"`,
    /// `"normal"`, `"expanded"` and the rest.
    pub widths: Vec<String>,
    /// Slants on offer: `"normal"`, `"italic"`, `"oblique"`.
    pub styles: Vec<String>,
}

/// One face a [`FontLibrary::use_paths`] or [`FontLibrary::use_data`] call
/// registered, as JavaScript's `FontLibrary.use()` describes each `Font` it
/// returns.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFace {
    /// The name the face is filed under: the alias the call gave, or the
    /// family name the font itself declares.
    pub family: String,
    /// CSS numeric weight, as the face reports it. A variable font reports
    /// its default instance, so Raleway's reports 100.
    pub weight: u16,
    /// The face's slant.
    pub slant: FontSlant,
    /// The face's width.
    pub stretch: FontStretch,
    /// The file the face was read from, or `None` when it was registered
    /// from bytes -- where JavaScript reports the literal `"<buffer>"`.
    pub file: Option<PathBuf>,
}

/// Owned font registry for the Rust facade.
///
/// Holds typefaces registered from disk or from in-memory bytes and exposes
/// them for paragraph layout. Internal state lives behind `parking_lot::Mutex`
/// so the same manager can be shared across threads without exposing `RefCell`
/// to consumers.
pub struct FontLibrary {
    inner: Mutex<FontManagerInner>,
}

struct FontManagerInner {
    /// Skia paragraph-side provider that maps registered family names to
    /// typefaces.
    ///
    /// Used internally when building paragraphs.
    provider: TypefaceFontProvider,
    /// System `FontMgr` used to parse byte streams into `Typeface`s.
    font_mgr: FontMgr,
    /// Registered family names in registration order.
    families: Vec<String>,
}

impl Default for FontLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl FontLibrary {
    /// Creates an empty registry holding no typefaces.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FontManagerInner {
                provider: TypefaceFontProvider::new(),
                font_mgr: FontMgr::new(),
                families: Vec::new(),
            }),
        }
    }

    /// Forgets every registered typeface, returning the registry to the
    /// state [`new`](Self::new) leaves it in.
    ///
    /// The system fonts are untouched -- they were never registered here,
    /// they are found through the platform's own manager. What this clears
    /// is what [`register_font_from_data`](Self::register_font_from_data)
    /// and [`register_font_from_path`](Self::register_font_from_path) added.
    ///
    /// A layout already built keeps the typefaces it resolved; this affects
    /// what the next one can find.
    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.provider = TypefaceFontProvider::new();
        inner.families.clear();
    }

    /// Registers a typeface loaded from `bytes` (TTF/OTF/WOFF/WOFF2, depending
    /// on Skia's available decoders) under the given family alias.
    ///
    /// Multiple typefaces can share a family alias; layout will pick one
    /// matching weight/slant.
    ///
    /// Registration order does not matter to a
    /// [`TextEngine`](crate::text::TextEngine): one built before this call
    /// still sees the font, and still instances it at the axes a
    /// [`font_variations`](crate::text::TextStyle::font_variations) asks
    /// for. An engine holds a *handle* to the provider these registrations
    /// write into rather than a copy of it.
    ///
    /// This used to say the opposite -- that the engine snapshots the
    /// registry at construction, so a later typeface was invisible to it.
    /// Measured on Oswald registered after the engine was built: 359.04 at
    /// `wght` 200 and 446.46 at 700, the same pair an engine built after
    /// the registration reports, where a family resolving to nothing
    /// measures 483.78.
    ///
    /// The typeface is also added to the registry
    /// [`Context2D::set_font`](crate::context2d::Context2D::set_font)
    /// resolves against, so `Font::new("MyFamily", 32.0)` finds it -- the same
    /// registry the JavaScript `FontLibrary.use()` writes to, and the reason
    /// there is one of it rather than one per `FontLibrary`. That registry is
    /// per-thread: a canvas drawn on another thread will not see this
    /// registration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] when the bytes are not a font this
    /// build can parse.
    pub fn register_font_from_data(
        &self,
        family: &str,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock();
        let typeface = inner
            .font_mgr
            .new_from_data(Data::new_copy(bytes), None)
            .ok_or_else(|| Error::FontRegister {
                reason: format!(
                    "could not parse typeface for family {family:?}"
                ),
            })?;
        inner
            .provider
            .register_typeface(typeface.clone(), Some(family));
        SharedFontRegistry::with_shared(|library| {
            library.register_typeface(typeface, Some(family.to_string()));
        });
        if !inner.families.iter().any(|f| f == family) {
            inner.families.push(family.to_string());
        }
        Ok(())
    }

    /// Registers the font in each file, filed under `alias` or, without one,
    /// under the family name each font declares, and describes every face
    /// registered.
    ///
    /// JavaScript's `FontLibrary.use(paths)` and `FontLibrary.use(alias,
    /// paths)`: what [`register_font_from_path`](Self::register_font_from_path)
    /// does a file at a time with an alias it requires, and returning what was
    /// registered rather than nothing. The faces are in the order the files
    /// were given.
    ///
    /// All or nothing, as `use()` is: every file is read and parsed before
    /// any is registered, so a failure leaves the registry as it was.
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let library = FontLibrary::new();
    /// let faces = library.use_paths(None, ["fonts/Raleway.ttf"])?;
    /// assert_eq!(faces[0].family, "Raleway");
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] naming the first file that cannot be
    /// read or is not a font this build can parse.
    pub fn use_paths<P: AsRef<Path>>(
        &self,
        alias: Option<&str>,
        paths: impl IntoIterator<Item = P>,
    ) -> Result<Vec<FontFace>, Error> {
        let parsed = self.read_paths(paths)?;
        Ok(self.file_all(parsed, alias))
    }

    /// Registers several families at once, each group's files filed under
    /// its alias or, without one, under the family name each font declares,
    /// and describes every face registered, one list per group in the order
    /// given.
    ///
    /// JavaScript's `FontLibrary.use({ alias: paths, ... })`, and
    /// [`use_paths`](Self::use_paths) for several aliases in one call. All or
    /// nothing across the whole call, as `use()` is: every file of every
    /// group is read and parsed before any is registered, so a failure in one
    /// family leaves the families before it unregistered too, which a
    /// `use_paths` call per family cannot do.
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let library = FontLibrary::new();
    /// let faces = library.use_families([
    ///     (
    ///         Some("Body"),
    ///         vec!["fonts/Inter.ttf", "fonts/Inter-Bold.ttf"],
    ///     ),
    ///     (Some("Headline"), vec!["fonts/Playfair.ttf"]),
    /// ])?;
    /// assert_eq!(faces[1][0].family, "Headline");
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] naming the first file that cannot be
    /// read or is not a font this build can parse.
    pub fn use_families<'a, P: AsRef<Path>, I: IntoIterator<Item = P>>(
        &self,
        groups: impl IntoIterator<Item = (Option<&'a str>, I)>,
    ) -> Result<Vec<Vec<FontFace>>, Error> {
        let parsed = groups
            .into_iter()
            .map(|(alias, paths)| Ok((alias, self.read_paths(paths)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(parsed
            .into_iter()
            .map(|(alias, faces)| self.file_all(faces, alias))
            .collect())
    }

    /// The typeface in each file, with the file it came from, or the first
    /// file's failure.
    fn read_paths<P: AsRef<Path>>(
        &self,
        paths: impl IntoIterator<Item = P>,
    ) -> Result<Vec<(Typeface, Option<PathBuf>)>, Error> {
        paths
            .into_iter()
            .map(|path| {
                let path = path.as_ref();
                let bytes =
                    std::fs::read(path).map_err(|e| Error::FontRegister {
                        reason: format!(
                            "could not read font file {}: {e}",
                            path.display()
                        ),
                    })?;
                let typeface =
                    self.parse(&bytes, &path.display().to_string())?;
                Ok((typeface, Some(path.to_path_buf())))
            })
            .collect()
    }

    /// Registers every parsed face under `alias` or its own family name.
    fn file_all(
        &self,
        faces: Vec<(Typeface, Option<PathBuf>)>,
        alias: Option<&str>,
    ) -> Vec<FontFace> {
        faces
            .into_iter()
            .map(|(typeface, file)| self.file(typeface, alias, file))
            .collect()
    }

    /// Registers the font in `bytes`, filed under `alias` or, without one,
    /// under the family name the font declares, and describes the face.
    ///
    /// JavaScript's `FontLibrary.use(alias, buffer)`. The face's
    /// [`file`](FontFace::file) is `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] when the bytes are not a font this
    /// build can parse.
    pub fn use_data(
        &self,
        alias: Option<&str>,
        bytes: &[u8],
    ) -> Result<FontFace, Error> {
        let typeface = self.parse(bytes, "the data given")?;
        Ok(self.file(typeface, alias, None))
    }

    /// Registers the font in each of `faces`, all filed under `alias` or,
    /// without one, each under the family name it declares, and describes
    /// every face registered, in the order given. Each face's
    /// [`file`](FontFace::file) is `None`.
    ///
    /// JavaScript's `FontLibrary.use(alias, [buffer, ...])`: several faces of
    /// one family, typically its weights and styles. All or nothing, as
    /// `use()` is: every face is parsed before any is registered, which a
    /// [`use_data`](Self::use_data) call per face cannot do.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] naming the index of the first face
    /// this build cannot parse.
    pub fn use_data_all<'a>(
        &self,
        alias: Option<&str>,
        faces: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<FontFace>, Error> {
        let parsed = faces
            .into_iter()
            .enumerate()
            .map(|(i, bytes)| {
                let typeface =
                    self.parse(bytes, &format!("the data at index {i}"))?;
                Ok((typeface, None))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(self.file_all(parsed, alias))
    }

    /// The typeface in `bytes`, or an error naming `source`.
    fn parse(&self, bytes: &[u8], source: &str) -> Result<Typeface, Error> {
        self.inner
            .lock()
            .font_mgr
            .new_from_data(Data::new_copy(bytes), None)
            .ok_or_else(|| Error::FontRegister {
                reason: format!("could not parse a typeface in {source}"),
            })
    }

    /// Registers `typeface` under `alias` or its own family name, in both
    /// registries, and describes it.
    fn file(
        &self,
        typeface: Typeface,
        alias: Option<&str>,
        file: Option<PathBuf>,
    ) -> FontFace {
        let family =
            alias.map_or_else(|| typeface.family_name(), str::to_string);
        let style = typeface.font_style();
        let face = FontFace {
            family: family.clone(),
            weight: u16::try_from(*style.weight()).unwrap_or(u16::MAX),
            slant: FontSlant::from_skia(style.slant()),
            stretch: FontStretch::from_skia(style.width()),
            file,
        };
        let mut inner = self.inner.lock();
        inner
            .provider
            .register_typeface(typeface.clone(), Some(family.as_str()));
        SharedFontRegistry::with_shared(|library| {
            library.register_typeface(typeface, Some(family.clone()));
        });
        if !inner.families.contains(&family) {
            inner.families.push(family);
        }
        face
    }

    /// Registers a typeface loaded from a file under `path` under the given
    /// family alias.
    ///
    /// To register multiple files (e.g. one per weight) for a single family,
    /// call this method multiple times.
    ///
    /// Subject to the same ordering constraint as
    /// [`FontLibrary::register_font_from_data`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] when the file cannot be read or its
    /// bytes are not a font this build can parse.
    pub fn register_font_from_path(
        &self,
        family: &str,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|e| Error::FontRegister {
            reason: format!("could not read font file {}: {e}", path.display()),
        })?;
        self.register_font_from_data(family, &bytes)
    }

    /// Whether a family alias has at least one registered typeface.
    pub fn has_font(&self, family: &str) -> bool {
        let inner = self.inner.lock();
        inner.families.iter().any(|f| f == family)
    }

    /// All registered family aliases in registration order.
    ///
    /// Duplicates are deduplicated; calling `register_font_from_data`
    /// repeatedly for the same family does not duplicate the alias.
    pub fn families(&self) -> Vec<String> {
        let inner = self.inner.lock();
        inner.families.clone()
    }

    /// Every family this process can resolve: the platform's own plus
    /// anything registered here.
    ///
    /// [`FontLibrary::families`] lists only what this registry was given.
    /// This is the list `Font::new` can actually match against, and the one
    /// the JavaScript `FontLibrary.families` reports.
    pub fn installed_families(&self) -> Vec<String> {
        SharedFontRegistry::with_shared(|library| library.families())
    }

    /// What `family` offers, or `None` when nothing resolves under that name.
    ///
    /// The counterpart of the JavaScript `FontLibrary.family()`. Use it to ask
    /// whether a family has the face a draw needs before asking for it:
    /// [`Context2D::set_font_stretch`](crate::context2d::Context2D::set_font_stretch)
    /// picks a narrower face only where the family ships one, and there is
    /// otherwise no way to find out except by measuring.
    pub fn family_details(&self, family: &str) -> Option<FontFamily> {
        let (weights, widths, styles) =
            SharedFontRegistry::with_shared(|library| {
                library.family_details(family)
            });
        match weights.is_empty() {
            true => None,
            false => Some(FontFamily {
                family: family.to_string(),
                weights,
                widths,
                styles,
            }),
        }
    }

    /// Internal accessor used by `TextEngine` to wire the registry into a
    /// paragraph `FontCollection`.
    ///
    /// Returns a clone of the `TypefaceFontProvider` (Skia-side ref-counted, so
    /// the clone shares typeface storage with the manager).
    pub(crate) fn snapshot_provider(&self) -> TypefaceFontProvider {
        let inner = self.inner.lock();
        inner.provider.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use skia_safe::{Data, FontMgr, FourByteTag};

    use super::{FontAxisTag, optical_size, variation_axes};

    fn face(path: &str) -> skia_safe::Typeface {
        let bytes = fs::read(path).expect(path);
        FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect(path)
    }

    /// Only a FreeType or DirectWrite build can tell this test from one
    /// asking `is_some()`: CoreText answers a static face with `None`
    /// either way, and those two answer it with an empty list.
    #[test]
    fn a_static_face_has_no_variation_axes_and_a_variable_one_does() {
        let monoton = face("tests/assets/fonts/Monoton-Regular.woff");
        assert!(variation_axes(&monoton).is_none(), "Monoton is static");

        let wght = FourByteTag::from(u32::from_be_bytes(
            *FontAxisTag::WGHT.as_bytes(),
        ));
        let oswald =
            face("tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf");
        assert!(
            variation_axes(&oswald)
                .is_some_and(|axes| axes.iter().any(|axis| axis.tag == wght)),
            "Oswald varies its weight"
        );
    }

    /// Clamped here as well as by the platform. CoreText and FreeType both
    /// pin a position to the axis themselves, so no width can tell the two
    /// apart; this is the check that the coordinate handed over is the one
    /// `font-optical-sizing: auto` names.
    #[test]
    fn the_optical_size_is_the_font_size_clamped_to_the_axis() {
        // Amstelvar's fvar runs `opsz` from 10 to 72.
        let amstelvar =
            variation_axes(&face("tests/assets/fonts/AmstelvarAlpha-VF.ttf"))
                .expect("Amstelvar is variable");
        for (size, opsz) in [(8.0, 10.0), (24.0, 24.0), (100.0, 72.0)] {
            assert_eq!(
                optical_size(&amstelvar, size).map(|c| c.value),
                Some(opsz),
                "{size}px"
            );
        }

        let oswald = variation_axes(&face(
            "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
        ))
        .expect("Oswald is variable");
        assert_eq!(
            optical_size(&oswald, 24.0).map(|c| c.value),
            None,
            "Oswald varies its weight and nothing else"
        );
    }
}
