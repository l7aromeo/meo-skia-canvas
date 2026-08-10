use std::path::Path;

use parking_lot::Mutex;
use skia_safe::{FontMgr, textlayout::TypefaceFontProvider};

use crate::error::Error;

/// Four-byte OpenType axis tag (e.g. `"wght"`, `"wdth"`, `"opsz"`).
///
/// Use `FontAxisTag::new(b"wght")` for an axis-tag literal or the
/// `WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL` associated constants for
/// the common cases. `FontAxisTag::from_str("wght")` accepts a 4-byte
/// ASCII string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontAxisTag([u8; 4]);

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

    /// Constructs a tag from a literal `[u8; 4]`. Use the `b"wght"` byte-string
    /// literal form: `FontAxisTag::new(b"wght")`.
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

    /// Parses from a 4-character ASCII string. Use either
    /// `"wght".parse::<FontAxisTag>()` or the `FontAxisTag::WGHT` /
    /// `WDTH` / `OPSZ` / `SLNT` / `ITAL` associated constants for
    /// compile-time tags.
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

/// Variable-font axis position. Mirrors CanvasKit's `fontVariations`
/// shape and the `TextStyleInput.fontVariations` field on the Node
/// addon.
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

/// Owned font registry for the Rust facade. Holds typefaces registered
/// from disk or from in-memory bytes and exposes them for paragraph
/// layout. Internal state lives behind `parking_lot::Mutex`
/// so the same manager can be shared across threads without exposing
/// `RefCell` to consumers.
pub struct FontManager {
    inner: Mutex<FontManagerInner>,
}

struct FontManagerInner {
    /// Skia paragraph-side provider that maps registered family names
    /// to typefaces. Used internally when building paragraphs.
    provider: TypefaceFontProvider,
    /// System `FontMgr` used to parse byte streams into `Typeface`s.
    font_mgr: FontMgr,
    /// Registered family names in registration order.
    families: Vec<String>,
}

impl Default for FontManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FontManager {
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

    /// Registers a typeface loaded from `bytes` (TTF/OTF/WOFF/WOFF2,
    /// depending on Skia's available decoders) under the given family
    /// alias. Multiple typefaces can share a family alias; layout will
    /// pick one matching weight/slant.
    ///
    /// Fonts must be registered before the
    /// [`TextEngine`](crate::text::TextEngine) that should see them is
    /// built: the engine snapshots the registry at construction, so a
    /// typeface added afterwards is invisible to it.
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
        let typeface =
            inner.font_mgr.new_from_data(bytes, None).ok_or_else(|| {
                Error::FontRegister {
                    reason: format!(
                        "could not parse typeface for family {family:?}"
                    ),
                }
            })?;
        inner.provider.register_typeface(typeface, Some(family));
        if !inner.families.iter().any(|f| f == family) {
            inner.families.push(family.to_string());
        }
        Ok(())
    }

    /// Registers a typeface loaded from a file under `path` under the
    /// given family alias. To register multiple files (e.g. one per
    /// weight) for a single family, call this method multiple times.
    ///
    /// Subject to the same ordering constraint as
    /// [`FontManager::register_font_from_data`].
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

    /// All registered family aliases in registration order. Duplicates
    /// are deduplicated; calling `register_font_from_data` repeatedly
    /// for the same family does not duplicate the alias.
    pub fn families(&self) -> Vec<String> {
        let inner = self.inner.lock();
        inner.families.clone()
    }

    /// Internal accessor used by `TextEngine` to wire the registry
    /// into a paragraph `FontCollection`. Returns a clone of the
    /// `TypefaceFontProvider` (Skia-side ref-counted, so the clone
    /// shares typeface storage with the manager).
    pub(crate) fn snapshot_provider(&self) -> TypefaceFontProvider {
        let inner = self.inner.lock();
        inner.provider.clone()
    }

    /// Snapshot of the family names registered so far. Used internally
    /// by `TextEngine` to map an instantiated typeface back to
    /// the registered alias for the dynamic-font-manager provider.
    pub(crate) fn registered_family_names(&self) -> Vec<String> {
        self.inner.lock().families.clone()
    }
}
