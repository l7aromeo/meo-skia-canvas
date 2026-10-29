use std::{
    borrow::Cow,
    cell::RefCell,
    hash::{BuildHasher, Hash, Hasher},
    iter::once,
    ops::Range,
    rc::Rc,
    sync::{Mutex, PoisonError},
};

use skia_safe::{
    Canvas as SkCanvas, FontArguments, FontMgr, FontStyle, FourByteTag,
    Paint as SkPaint, PaintStyle as SkPaintStyle, Point as SkPoint, Typeface,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_parameters::variation::Axis,
    font_style::{Slant, Weight},
    textlayout::{
        Affinity as SkAffinity, FontCollection, Paragraph as SkParagraph,
        ParagraphStyle as SkParagraphStyle,
        PlaceholderAlignment as SkPlaceholderAlignment, PlaceholderStyle,
        RectHeightStyle as SkRectHeightStyle,
        RectWidthStyle as SkRectWidthStyle, StrutStyle as SkStrutStyle,
        TextAlign as SkTextAlign, TextBaseline as SkTextBaseline,
        TextBox as SkTextBox, TextDecoration as SkTextDecoration,
        TextDecorationStyle as SkTextDecorationStyle,
        TextDirection as SkTextDirection,
        TextHeightBehavior as SkTextHeightBehavior, TextShadow as SkTextShadow,
        TextStyle as SkTextStyle,
    },
};

use crate::{
    cache_map::{CacheHasher, CacheMap},
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_skia_color,
        rgba_linear_to_unpremul_color4f,
    },
    context2d::{FontStretch, TextDirection},
    font::{
        FontAxisTag, FontFamily, FontLibrary, FontVariation,
        InstancedPositions, KnownAxes, ObliqueAngle, optical_size,
    },
    geometry::Rect,
    image::CSS_INITIAL_FONT_SIZE,
    memory::note_font_use,
    node::{
        font_library::{
            FontLibrary as SharedFontRegistry, font_generation, forgetful,
        },
        oblique::{
            ParagraphRecipe, Passes, add_slnt, match_oblique,
            without_paragraph_cache,
        },
    },
};

/// Horizontal alignment of text within its layout width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    /// Aligns to the left edge, whatever the reading direction.
    Left,
    /// Centres within the available width.
    Center,
    /// Aligns to the right edge, whatever the reading direction.
    Right,
    /// Aligns to whichever edge the text starts from.
    ///
    /// Left for left-to-right text and right for right-to-left, so unlike
    /// [`Left`](TextAlign::Left) this follows
    /// [`set_direction`](crate::context2d::Context2D::set_direction). It is
    /// what the Canvas API's `textAlign` defaults to, what a
    /// [`Context2D`](crate::context2d::Context2D) starts with, and therefore
    /// the default here.
    ///
    /// The lower text layer is the exception: [`TextStyle`] lays out a box
    /// rather than a canvas run, and pins [`Left`](TextAlign::Left) as
    /// Skia's own layout does.
    #[default]
    Start,
    /// Aligns to whichever edge the text ends at -- the mirror of
    /// [`Start`](TextAlign::Start).
    End,
    /// Stretches every line but the last to fill the available width.
    ///
    /// Only meaningful with a wrapping width, since a single line has
    /// nothing to stretch against.
    Justify,
}

impl TextDirection {
    pub(crate) fn from_skia(direction: SkTextDirection) -> Self {
        match direction {
            SkTextDirection::RTL => Self::RightToLeft,
            SkTextDirection::LTR => Self::LeftToRight,
        }
    }

    /// `Inherit` lays out left to right: it names the surrounding document's
    /// direction, and a canvas has no document, so there is nothing to take
    /// one from. The keyword is still reported back as `Inherit` -- it is
    /// carried beside the resolved direction rather than in it.
    pub(crate) fn to_skia(self) -> SkTextDirection {
        match self {
            Self::RightToLeft => SkTextDirection::RTL,
            Self::LeftToRight | Self::Inherit => SkTextDirection::LTR,
        }
    }
}

impl TextAlign {
    pub(crate) fn from_skia(align: SkTextAlign) -> Self {
        match align {
            SkTextAlign::Center => Self::Center,
            SkTextAlign::Right => Self::Right,
            SkTextAlign::Start => Self::Start,
            SkTextAlign::End => Self::End,
            SkTextAlign::Justify => Self::Justify,
            _ => Self::Left,
        }
    }

    pub(crate) fn to_skia(self) -> SkTextAlign {
        match self {
            Self::Left => SkTextAlign::Left,
            Self::Center => SkTextAlign::Center,
            Self::Right => SkTextAlign::Right,
            Self::Start => SkTextAlign::Start,
            Self::End => SkTextAlign::End,
            Self::Justify => SkTextAlign::Justify,
        }
    }
}

/// Measurements of a text run, as `measureText` reports them.
///
/// Distances are in pixels and are relative to the coordinate the draw would
/// be given, so the `actual_bounding_box_*` values describe the inked extent
/// of these specific glyphs while the `font_bounding_box_*` values describe
/// what the font could reach for any string.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMetrics {
    /// Advance width of the run.
    pub width: f32,
    /// Distance left from the alignment point to the inked extent. Positive
    /// leftwards, so a left-aligned run is usually near zero or negative.
    pub actual_bounding_box_left: f32,
    /// Distance right from the alignment point to the inked extent.
    pub actual_bounding_box_right: f32,
    /// Distance above the baseline to the top of the inked extent.
    pub actual_bounding_box_ascent: f32,
    /// Distance below the baseline to the bottom of the inked extent.
    pub actual_bounding_box_descent: f32,
    /// Distance above the baseline the font can reach, string-independent.
    ///
    /// Taken from the face's own metrics rather than the drawn glyphs, so a
    /// run of `"x"` reports the same value as one with ascenders.
    ///
    /// The value is the font's `hhea` ascender over its units per em, on
    /// every platform. Skia reaches fonts through CoreText on macOS,
    /// FreeType on Linux and DirectWrite on Windows, and a font may carry
    /// three different answers -- `hhea`, `OS/2` `sTypo*` and `OS/2`
    /// `usWin*` disagree in most files. Reporting `hhea` everywhere is what
    /// makes a line box computed on one machine correct on another, and
    /// `tests/suite/context2d.test.js` asserts it against two fonts chosen
    /// so that only `hhea` satisfies both.
    ///
    /// A browser is entitled to a different answer and usually gives one:
    /// CSS 2.1 §10.8.1 leaves `line-height: normal` to the user agent and
    /// only recommends 1.0 to 1.2. Chrome on macOS reports 0.9199em of
    /// ascent for Helvetica, which is no table in the file -- it comes from
    /// the platform text stack -- against the 0.7700em `hhea` holds. A
    /// design ported from a browser will lay out tighter here for that
    /// reason, and the difference is in the ascent.
    pub font_bounding_box_ascent: f32,
    /// Distance below the baseline the font can reach, string-independent.
    ///
    /// The font's `hhea` descender over its units per em, with the same
    /// cross-platform guarantee as
    /// [`font_bounding_box_ascent`](Self::font_bounding_box_ascent).
    pub font_bounding_box_descent: f32,
    /// Distance above the baseline to the top of the em square.
    ///
    /// The same number the JavaScript binding has always reported, and the
    /// same one `font_bounding_box_ascent` carries: Skia gives one ascent
    /// per face, and the Canvas specification's distinction between the em
    /// square and the font's own bounds needs metrics Skia does not expose
    /// separately. Reported rather than omitted because a caller porting
    /// from the browser reads it, and reporting the ascent is what every
    /// engine does here in practice.
    pub em_height_ascent: f32,
    /// Distance below the baseline to the bottom of the em square. As
    /// [`em_height_ascent`](Self::em_height_ascent).
    pub em_height_descent: f32,
    /// Offset from the selected baseline to the alphabetic one.
    pub alphabetic_baseline: f32,
    /// Offset from the selected baseline to the hanging one.
    pub hanging_baseline: f32,
    /// Offset from the selected baseline to the ideographic one.
    pub ideographic_baseline: f32,
    /// Height of the laid-out run, including line spacing when wrapped.
    pub height: f32,
    /// How many lines the run occupied.
    ///
    /// Always `1` while text wrapping is off, because the typesetter
    /// replaces newlines with spaces in that mode. With wrapping on this
    /// counts the lines a `\n` produces even when no width was given, and
    /// the lines a width forced on top of those.
    ///
    /// Named for what it holds rather than after
    /// [`lines`](Self::lines), which is the per-line detail and not a
    /// count.
    pub line_count: usize,
    /// Each line separately, with the single-font runs inside it.
    ///
    /// Empty where the measurement produced no lines. One entry otherwise,
    /// per line the run wrapped or broke into -- so a caller drawing its
    /// own selection, or placing something against a particular line, has
    /// the boxes without laying the text out a second time.
    ///
    /// The JavaScript binding has reported this since before this crate had
    /// a Rust text API, and this side reported only the count.
    pub lines: Vec<TextMetricsLine>,
}

/// Which horizontal line of the font a text draw sits on.
///
/// These are the values the Canvas API's `textBaseline` accepts. They shift
/// the drawn run vertically relative to the y coordinate given to the draw;
/// they do not change how the text is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextBaseline {
    /// The top of the em square.
    Top,
    /// The hanging baseline, used by Devanagari and related scripts.
    Hanging,
    /// Halfway up the em square.
    Middle,
    /// The line Latin glyphs rest on. The default.
    #[default]
    Alphabetic,
    /// The ideographic baseline, below the alphabetic one.
    Ideographic,
    /// The bottom of the em square.
    Bottom,
}

/// Whether glyphs are upright or slanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextSlant {
    /// Upright. The default.
    #[default]
    Upright,
    /// The font's designed italic, where it has one.
    Italic,
    /// An oblique at an angle, drawn as
    /// [`FontSlant::Oblique`](crate::context2d::FontSlant::Oblique) draws it:
    /// the family's face that leans the same way, or else its upright face
    /// sheared by `tan(angle)`, before any italic. At 0deg it draws upright.
    ///
    /// [`TextSlant::OBLIQUE`] is the default angle, 14deg.
    Oblique(ObliqueAngle),
}

impl TextSlant {
    /// An oblique at CSS Fonts 4's default angle, 14deg.
    pub const OBLIQUE: Self = Self::Oblique(ObliqueAngle::DEFAULT);

    /// The slant the face matcher is asked for. An oblique of 0deg leans no
    /// way, so it asks for the upright face that draws it, as a canvas font
    /// does.
    fn to_skia(self) -> Slant {
        match self {
            Self::Upright => Slant::Upright,
            Self::Italic => Slant::Italic,
            Self::Oblique(angle) if angle.degrees() == 0.0 => Slant::Upright,
            Self::Oblique(_) => Slant::Oblique,
        }
    }

    /// The angle of an oblique, in degrees, or `None` for another slant.
    fn oblique_degrees(self) -> Option<f32> {
        match self {
            Self::Oblique(angle) => Some(angle.degrees()),
            _ => None,
        }
    }
}

/// One OpenType feature applied to a text run, mirroring CanvasKit's
/// `TextFontFeatures { name, value }`.
///
/// `name` is an OpenType feature tag (`"smcp"`, `"liga"`, `"onum"`, `"ss01"`,
/// ...); `value` is the feature selector (`1`/`0` to enable/disable, or an
/// index for alternates). Unlike variable-font axes ([`FontVariation`]),
/// features are applied directly on the layout `TextStyle` and need no typeface
/// instancing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontFeature {
    /// Four-character OpenType feature tag, e.g. `"smcp"`.
    pub name: String,
    /// Feature selector: `1` or `0` for a boolean feature, or an index
    /// where the feature selects among alternates.
    pub value: i32,
}

impl FontFeature {
    /// Pairs a feature tag with a selector value.
    pub fn new(name: impl Into<String>, value: i32) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    /// Enables a boolean feature (`value = 1`), e.g.
    /// `FontFeature::on("smcp")` for small caps.
    pub fn on(name: impl Into<String>) -> Self {
        Self::new(name, 1)
    }

    /// Disables a boolean feature (`value = 0`).
    pub fn off(name: impl Into<String>) -> Self {
        Self::new(name, 0)
    }
}

/// A fixed line box independent of the per-run fonts, for deterministic leading
/// (captions, subtitles, vertically-aligned blocks).
///
/// Mirrors CanvasKit's `StrutStyle`. Attaching `Some(StrutStyle)` to a
/// [`TextStyle`] enables the strut; `None` leaves Skia's default (line box
/// driven by the run fonts).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StrutStyle {
    /// Strut font families. Empty falls back to the paragraph's fonts.
    ///
    /// A string converts as [`FontFamily`] says: a generic keyword is the
    /// generic family, anything else a name.
    pub font_families: Vec<FontFamily>,
    /// Strut font size in pixels. `None` uses the base text size.
    pub font_size: Option<f32>,
    /// Line-height multiplier for the strut line box. `None` leaves it
    /// unset (Skia uses the font's natural height).
    pub height: Option<f32>,
    /// Extra leading added to the strut line, as a multiple of the
    /// strut font size. `None` leaves Skia's default.
    pub leading: Option<f32>,
    /// Clamps every line to the strut height even when its content is taller.
    ///
    /// When `false` the strut acts as a minimum line height.
    pub force_height: bool,
    /// Distribute leading half above and half below the text
    /// (vertical centring within the line box).
    pub half_leading: bool,
}

/// How the line-height multiplier is applied to the first ascent and last
/// descent of a paragraph.
///
/// Mirrors CanvasKit's `TextHeightBehavior` and controls first/last-line
/// leading trim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextHeightBehavior {
    /// Applies height to both the first ascent and the last descent.
    #[default]
    All,
    /// Trim the leading above the first line.
    DisableFirstAscent,
    /// Trim the leading below the last line.
    DisableLastDescent,
    /// Trim both first-line and last-line leading.
    DisableAll,
}

impl TextHeightBehavior {
    pub(crate) fn to_skia(self) -> SkTextHeightBehavior {
        match self {
            Self::All => SkTextHeightBehavior::All,
            Self::DisableFirstAscent => {
                SkTextHeightBehavior::DisableFirstAscent
            }
            Self::DisableLastDescent => {
                SkTextHeightBehavior::DisableLastDescent
            }
            Self::DisableAll => SkTextHeightBehavior::DisableAll,
        }
    }
}

/// What a style says about variable-font axes: CSS `font-variation-settings`.
///
/// Three states rather than a list, because CSS has three: a style can take
/// the axes in force, reset them to the face's own, or set its own. A list
/// alone cannot tell the first two apart. The default is
/// [`Inherit`](Self::Inherit), so `..TextStyle::default()` keeps the base
/// style's axes.
///
/// Whatever the setting, `font_weight` supplies `wght` and `font_size` supplies
/// `opsz` unless the axes in force name them, as `font-weight` and
/// `font-optical-sizing` do under `font-variation-settings` in CSS.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum FontVariationSettings {
    /// The axes in force: a pushed style or a rich-text span is instanced at
    /// the base style's. On a base style, which has nothing to inherit, it is
    /// [`Normal`](Self::Normal).
    #[default]
    Inherit,
    /// CSS `normal`: no axes of the style's own. A span under a base style
    /// that set some is instanced at the face's defaults instead.
    Normal,
    /// These axes, replacing those in force, as a nested
    /// `font-variation-settings` replaces its parent's rather than merging
    /// with it. An axis the face lacks is ignored and a position outside its
    /// range clamped. An empty list is [`Normal`](Self::Normal).
    Axes(Vec<FontVariation>),
}

impl FontVariationSettings {
    /// The axes a style with these settings is instanced at, given those in
    /// force where it is used.
    fn resolve<'a>(
        &'a self,
        inherited: &'a [FontVariation],
    ) -> &'a [FontVariation] {
        match self {
            Self::Inherit => inherited,
            Self::Normal => &[],
            Self::Axes(axes) => axes,
        }
    }
}

impl From<Vec<FontVariation>> for FontVariationSettings {
    /// The list as [`Axes`](Self::Axes).
    fn from(axes: Vec<FontVariation>) -> Self {
        Self::Axes(axes)
    }
}

impl<const N: usize> From<[FontVariation; N]> for FontVariationSettings {
    /// The array as [`Axes`](Self::Axes).
    fn from(axes: [FontVariation; N]) -> Self {
        Self::Axes(axes.into())
    }
}

/// Paragraph style.
///
/// The paragraph-level fields -- `align`, `line_height_multiplier`,
/// `text_height_behavior`, `max_lines` and `strut` -- only apply when this
/// style is used as the base for a paragraph. A per-span override supplied
/// through [`RichTextSpan`] contributes only its per-span fields.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Families tried in order; the first that has a glyph wins. Empty
    /// uses the system default.
    ///
    /// A string converts as [`FontFamily`] says, so `"serif".into()` is the
    /// generic family and `"Helvetica".into()` a name;
    /// [`FontFamily::Named`] names a family that spells a keyword.
    pub font_families: Vec<FontFamily>,
    /// Em size in pixels.
    ///
    /// On a variable typeface with an `opsz` axis it is also the `opsz`
    /// coordinate, clamped to the face's range, unless
    /// [`font_variations`](Self::font_variations) names `opsz` -- what CSS
    /// `font-optical-sizing: auto` does.
    pub font_size: f32,
    /// CSS numeric weight, `1` to `1000`, where `400` is regular and `700`
    /// is bold.
    ///
    /// The full CSS Fonts 4 range, which is also OpenType's
    /// `usWeightClass`. This said `100` to `900` -- the nine named steps of
    /// CSS 2 -- which [`Font::weight`] has never agreed with, since it
    /// clamps to `1..=1000`, and which this file already contradicted
    /// itself: the note on synthesizing a `wght` axis uses 350 as its
    /// example.
    ///
    /// Not clamped here. [`Font::weight`] is a setter and can refuse a
    /// value; this is a plain field, and Skia takes whatever it is given.
    ///
    /// On a variable typeface it is also the `wght` coordinate, clamped to
    /// the face's range, unless [`font_variations`](Self::font_variations)
    /// names `wght`.
    ///
    /// [`Font::weight`]: crate::context2d::Font::weight
    pub font_weight: i32,
    /// Upright, italic, or oblique.
    pub slant: TextSlant,
    /// Condensed or expanded, the third `SkFontStyle` axis.
    ///
    /// Selects among the widths a family actually ships. Skia matches the
    /// nearest rather than synthesizing one, so asking a family with a single
    /// width for `Condensed` gets its regular back unchanged -- this is a
    /// selector, not a transform. A variable font with a `wdth` axis is
    /// reached through
    /// [`font_variations`](Self::font_variations) instead, which sets the axis
    /// directly.
    ///
    /// The same type [`Context2D`](crate::context2d::Context2D) takes for
    /// `fontStretch`, so canvas text and paragraph text name a width the same
    /// way.
    pub stretch: FontStretch,
    /// Glyph fill color.
    pub color: RgbaLinear,
    /// Paint the glyphs are filled with, overriding
    /// [`color`](Self::color).
    ///
    /// `color` is a fill and nothing else; this is the whole paint, so a
    /// run can be stroked, drawn with a gradient, or given a blend mode.
    /// The JavaScript binding has taken `foregroundColor` since before this
    /// crate had a Rust text API and this side had no field for it, so the
    /// same paragraph styled the same way differed between the two.
    pub foreground_color: Option<RgbaLinear>,
    /// Colour painted behind the glyphs, for a highlight.
    ///
    /// `None` -- the default -- draws nothing behind them. Unlike drawing a
    /// rectangle first, this follows the run through wrapping and bidi
    /// reordering, which is what makes it worth having.
    pub background_color: Option<RgbaLinear>,
    /// Horizontal alignment. Paragraph-level.
    pub align: TextAlign,
    /// Base reading direction. Paragraph-level.
    ///
    /// Not the direction of any particular run -- the bidi algorithm takes
    /// that from the characters themselves, so Arabic reads right to left in
    /// a left-to-right paragraph either way. What this sets is the direction
    /// the paragraph resolves *neutrals* against: which edge a line starts
    /// from, where [`TextAlign::Start`] and [`TextAlign::End`] point, and
    /// which side trailing punctuation lands on. A right-to-left paragraph
    /// with no strongly-directional characters at all still lays out from
    /// the right.
    ///
    /// Read back per box by [`TextBox::direction`], which reports what the
    /// algorithm decided rather than what was asked for.
    pub direction: TextDirection,
    /// Multiplier applied to the font's natural line height.
    ///
    /// `1.0` keeps Skia's default. Values above `1.0` add line spacing.
    pub line_height_multiplier: f32,
    /// Additional space between glyphs, in pixels.
    pub letter_spacing: f32,
    /// Additional space at word boundaries, in pixels.
    pub word_spacing: f32,
    /// Underline / overline / line-through bitmask.
    pub decoration: TextDecoration,
    /// Decoration line style. Ignored when `decoration` is empty.
    pub decoration_style: TextDecorationStyle,
    /// Decoration color override. `None` falls back to the text color.
    pub decoration_color: Option<RgbaLinear>,
    /// Multiplier applied to the default decoration line thickness.
    pub decoration_thickness: f32,
    /// Drop shadows applied behind the glyphs.
    pub shadows: Vec<TextShadow>,
    /// Vertical offset from the baseline, in pixels.
    ///
    /// Positive shifts downward; negative shifts upward (use for
    /// superscripts).
    pub baseline_shift: f32,
    /// Variable-font axis positions, as CSS `font-variation-settings` sets
    /// them: the base style's, the face's own, or a list -- see
    /// [`FontVariationSettings`].
    ///
    /// A variable typeface the families resolve to is instanced at the axes
    /// in force before layout, matching CanvasKit's `fontVariations`.
    /// `font_weight` drives face matching and supplies `wght` when those
    /// axes do not name it, as `font_size` supplies `opsz`. A
    /// `FontAxisTag::WGHT` entry wins over `font_weight`, as
    /// `font-variation-settings` wins over `font-weight` in CSS, and a
    /// `FontAxisTag::OPSZ` entry over `font_size` the same way.
    pub font_variations: FontVariationSettings,
    /// OpenType features applied to the run (small caps, ligatures,
    /// oldstyle/tabular figures, stylistic sets, ...).
    ///
    /// Mirrors CanvasKit's `TextStyle.fontFeatures`. Applied directly on the
    /// layout `TextStyle`; independent of `font_variations`.
    pub font_features: Vec<FontFeature>,
    /// Width of the stroke the glyphs are drawn with, in pixels.
    ///
    /// `None` -- the default -- fills them. A width outlines them instead,
    /// centred on the contour, so half of it falls inside the letterform and
    /// a heavy width visibly thins the shape. That is what CSS
    /// `-webkit-text-stroke` describes.
    ///
    /// A value that is not positive is ignored rather than refused, which is
    /// what `Context2D::set_line_width` does and what a browser does with
    /// `lineWidth`. Skia would read zero as a hairline -- one pixel however
    /// the run is transformed -- and that is not what a caller computing a
    /// width from a scale means by it.
    ///
    /// One paint draws one way, so a run is filled or stroked and never
    /// both. Both is two paragraphs drawn in the order the caller wants,
    /// which is also what makes `paint-order` expressible.
    pub stroke_width: Option<f32>,
    /// BCP 47 language tag the run is written in, such as `"ja"` or
    /// `"zh-Hans"`.
    ///
    /// `None` -- the default -- leaves the choice to the platform's
    /// fallback. It decides which face a shared codepoint is drawn from:
    /// Han characters are unified in Unicode, so 直 is one codepoint with
    /// one letterform in Japanese and another in Simplified Chinese, and
    /// nothing in the text says which a reader should see. Naming a font
    /// family answers the same question but gives up fallback for every
    /// codepoint that family lacks.
    pub locale: Option<String>,
    /// Distribute the run's leading half above and half below the text
    /// (vertical centring within the line box). Mirrors CanvasKit's
    /// `TextStyle.halfLeading`.
    pub half_leading: bool,
    /// Optional strut for deterministic line boxes (paragraph-level).
    ///
    /// `None` leaves Skia's font-driven line height. See [`StrutStyle`].
    pub strut: Option<StrutStyle>,
    /// First/last-line leading trim (paragraph-level). Mirrors
    /// CanvasKit's `ParagraphStyle.textHeightBehavior`.
    pub text_height_behavior: TextHeightBehavior,
    /// Maximum number of lines (paragraph-level).
    ///
    /// `None` is unbounded. When set, overflow past this limit is reported by
    /// [`Paragraph::did_exceed_max_lines`]. Mirrors CanvasKit's
    /// `ParagraphStyle.maxLines`.
    pub max_lines: Option<usize>,
    /// String appended to the last line when the text does not fit
    /// (paragraph-level).
    ///
    /// `None` -- the default -- cuts the text off where it runs out of
    /// room. Setting it to `"..."` is the usual choice, and Skia trims back
    /// far enough for it to fit rather than drawing it past the edge.
    ///
    /// Worth something only alongside [`max_lines`](Self::max_lines) or a
    /// layout width, since without a limit nothing overflows. The
    /// JavaScript binding has taken it since before this crate had a Rust
    /// text API.
    pub ellipsis: Option<String>,
}

/// CSS's initial `font-weight`, the value `normal` names.
///
/// CSS Fonts 4 section 2.4 defines the keyword as 400, and the numeric form
/// is what this struct carries, so the keyword never appears here.
const CSS_NORMAL_FONT_WEIGHT: i32 = 400;

/// The line-height multiplier that leaves a face's own metrics alone.
///
/// Not CSS's `line-height: normal`, which is the font's default leading and
/// varies by face. This is the factor applied *to* that leading, so 1.0 means
/// "whatever the face says" rather than a fixed ratio -- a distinction worth
/// the name, because a reader meeting a bare `1.0` reasonably assumes the
/// second.
const NATURAL_LINE_HEIGHT: f32 = 1.0;

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_families: Vec::new(),
            font_size: CSS_INITIAL_FONT_SIZE,
            font_weight: CSS_NORMAL_FONT_WEIGHT,
            slant: TextSlant::Upright,
            stretch: FontStretch::Normal,
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            foreground_color: None,
            background_color: None,
            align: TextAlign::Left,
            direction: TextDirection::LeftToRight,
            line_height_multiplier: NATURAL_LINE_HEIGHT,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            decoration: TextDecoration::default(),
            decoration_style: TextDecorationStyle::Solid,
            decoration_color: None,
            decoration_thickness: 1.0,
            shadows: Vec::new(),
            baseline_shift: 0.0,
            font_variations: FontVariationSettings::Inherit,
            font_features: Vec::new(),
            stroke_width: None,
            locale: None,
            half_leading: false,
            strut: None,
            text_height_behavior: TextHeightBehavior::All,
            max_lines: None,
            ellipsis: None,
        }
    }
}

/// Underline / overline / line-through flags.
///
/// Multiple flags can be combined (e.g. underline + line-through together).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextDecoration {
    /// Draws a line below the baseline.
    pub underline: bool,
    /// Draws a line above the text.
    pub overline: bool,
    /// Draws a line through the middle of the text.
    pub line_through: bool,
}

impl TextDecoration {
    /// Returns a decoration with only the underline set.
    pub const fn underline() -> Self {
        Self {
            underline: true,
            overline: false,
            line_through: false,
        }
    }

    /// Returns a decoration with only the overline set.
    pub const fn overline() -> Self {
        Self {
            underline: false,
            overline: true,
            line_through: false,
        }
    }

    /// Returns a decoration with only the line-through set.
    pub const fn line_through() -> Self {
        Self {
            underline: false,
            overline: false,
            line_through: true,
        }
    }

    /// Returns `true` when no line is set, so nothing would be drawn.
    pub const fn is_empty(self) -> bool {
        !self.underline && !self.overline && !self.line_through
    }

    /// The Skia flag set, for the paragraph decoration the context builds.
    pub(crate) fn to_skia_flags(self) -> SkTextDecoration {
        self.to_skia()
    }

    fn to_skia(self) -> SkTextDecoration {
        let mut bits = SkTextDecoration::NO_DECORATION;
        if self.underline {
            bits |= SkTextDecoration::UNDERLINE;
        }
        if self.overline {
            bits |= SkTextDecoration::OVERLINE;
        }
        if self.line_through {
            bits |= SkTextDecoration::LINE_THROUGH;
        }
        bits
    }
}

/// How a decoration line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextDecorationStyle {
    /// A single unbroken line. The default.
    #[default]
    Solid,
    /// Two parallel lines.
    Double,
    /// A dotted line.
    Dotted,
    /// A dashed line.
    Dashed,
    /// A sine-wave line, as used for spelling errors.
    Wavy,
}

impl TextDecorationStyle {
    pub(crate) fn to_skia(self) -> SkTextDecorationStyle {
        match self {
            Self::Solid => SkTextDecorationStyle::Solid,
            Self::Double => SkTextDecorationStyle::Double,
            Self::Dotted => SkTextDecorationStyle::Dotted,
            Self::Dashed => SkTextDecorationStyle::Dashed,
            Self::Wavy => SkTextDecorationStyle::Wavy,
        }
    }

    /// The Skia style, for the paragraph decoration the context builds.
    pub(crate) fn to_skia_decoration_style(self) -> SkTextDecorationStyle {
        self.to_skia()
    }
}

/// Drop shadow applied behind glyphs. Multiple shadows on a single
/// `TextStyle` stack additively.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    /// Shadow color.
    pub color: RgbaLinear,
    /// Horizontal offset from the glyphs, in pixels.
    pub offset_x: f32,
    /// Vertical offset from the glyphs, in pixels.
    pub offset_y: f32,
    /// Gaussian blur sigma. `0.0` gives a hard-edged shadow.
    pub blur_sigma: f32,
}

/// One span of rich text.
///
/// Carries its own `TextStyle` for per-span font, color, decoration, baseline
/// shift, etc. Paragraph-level fields (`align`, `line_height_multiplier`) on
/// the span style are ignored; only the base style governs them.
#[derive(Debug, Clone, PartialEq)]
pub struct RichTextSpan {
    /// The span's text.
    pub text: String,
    /// Style governing this span. Paragraph-level fields are ignored.
    pub style: TextStyle,
}

/// Where an inline placeholder sits relative to the line it is on.
///
/// The numbering is CanvasKit's, which the JavaScript surface exposes as
/// `PlaceholderAlignment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlaceholderAlignment {
    /// The placeholder's own baseline, named by
    /// [`Placeholder::baseline`], sits on the line's baseline. The
    /// default, and the only alignment that reads
    /// [`baseline_offset`](Placeholder::baseline_offset).
    #[default]
    Baseline,
    /// The placeholder's bottom edge rests on the line's baseline, so it
    /// sits entirely above it.
    AboveBaseline,
    /// The placeholder's top edge hangs from the line's baseline, so it sits
    /// entirely below it.
    BelowBaseline,
    /// The placeholder's top edge aligns with the line's top edge.
    Top,
    /// The placeholder's bottom edge aligns with the line's bottom edge.
    Bottom,
    /// The placeholder is centred on the line.
    Middle,
}

impl PlaceholderAlignment {
    pub(crate) fn to_skia(self) -> SkPlaceholderAlignment {
        match self {
            Self::Baseline => SkPlaceholderAlignment::Baseline,
            Self::AboveBaseline => SkPlaceholderAlignment::AboveBaseline,
            Self::BelowBaseline => SkPlaceholderAlignment::BelowBaseline,
            Self::Top => SkPlaceholderAlignment::Top,
            Self::Bottom => SkPlaceholderAlignment::Bottom,
            Self::Middle => SkPlaceholderAlignment::Middle,
        }
    }
}

/// Which baseline [`PlaceholderAlignment::Baseline`] aligns against.
///
/// Distinct from [`TextBaseline`], which is the Canvas API's six-value
/// `textBaseline` and shifts a drawn run rather than placing a box in a
/// paragraph. CanvasKit gives both the name `TextBaseline`; only one of them
/// can have it here, and the Canvas API's is the one callers meet first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[doc(alias = "TextBaseline")]
pub enum PlaceholderBaseline {
    /// The line Latin glyphs rest on. The default.
    #[default]
    Alphabetic,
    /// The ideographic baseline, below the alphabetic one.
    Ideographic,
}

impl PlaceholderBaseline {
    pub(crate) fn to_skia(self) -> SkTextBaseline {
        match self {
            Self::Alphabetic => SkTextBaseline::Alphabetic,
            Self::Ideographic => SkTextBaseline::Ideographic,
        }
    }
}

/// A box reserved in a paragraph for something the text engine does not draw.
///
/// The layout flows around it as though it were one very large glyph, and
/// [`Paragraph::rects_for_placeholders`] reports where each one landed so
/// the caller can draw an image, a chart or another canvas into it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placeholder {
    /// Width of the reserved box, in pixels.
    pub width: f32,
    /// Height of the reserved box, in pixels.
    pub height: f32,
    /// How the box sits on its line.
    pub alignment: PlaceholderAlignment,
    /// Which baseline [`PlaceholderAlignment::Baseline`] aligns against.
    /// Ignored by every other alignment.
    pub baseline: PlaceholderBaseline,
    /// Distance from the box's top edge down to the baseline named by
    /// [`baseline`](Self::baseline), in pixels.
    ///
    /// Read only under [`PlaceholderAlignment::Baseline`]. `0.0` puts the
    /// box's top edge on the baseline, which is rarely what is wanted --
    /// [`Placeholder::new`] defaults it to the full height, resting the box
    /// on the baseline the way an image in a line of HTML sits.
    pub baseline_offset: f32,
}

impl Placeholder {
    /// A `width` by `height` box resting on the alphabetic baseline.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            alignment: PlaceholderAlignment::default(),
            baseline: PlaceholderBaseline::default(),
            baseline_offset: height,
        }
    }

    /// Places the box with `alignment` instead of on the baseline.
    pub fn aligned(mut self, alignment: PlaceholderAlignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Aligns against `baseline` rather than the alphabetic one.
    pub fn on_baseline(mut self, baseline: PlaceholderBaseline) -> Self {
        self.baseline = baseline;
        self
    }

    /// Moves the baseline `offset` pixels down from the box's top edge.
    pub fn baseline_offset(mut self, offset: f32) -> Self {
        self.baseline_offset = offset;
        self
    }

    fn to_skia(self) -> PlaceholderStyle {
        PlaceholderStyle {
            width: self.width,
            height: self.height,
            alignment: self.alignment.to_skia(),
            baseline: self.baseline.to_skia(),
            baseline_offset: self.baseline_offset,
        }
    }
}

/// One rectangle covering part of a laid-out run, and the direction the
/// text inside it reads.
///
/// The direction is why this is a struct rather than a bare
/// [`Rect`]. A bidirectional line -- Arabic with a
/// Latin phrase in it, or any text with a number in it -- comes back as
/// several boxes whose visual order is not their logical order, and the
/// only thing that says which is which is this field. Skia supplies it per
/// box; this crate used to drop it, so a Rust caller could draw a selection
/// but not tell an RTL run from an LTR one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    /// The rectangle, in paragraph-local coordinates.
    pub rect: Rect,
    /// Which way the text inside it reads.
    pub direction: TextDirection,
}

/// How tall the rectangles [`Paragraph::rects_for_range`] returns are.
///
/// A selection highlight and a hit test want different answers from the
/// same range: the highlight should meet its neighbours with no gap, and
/// the hit test should cover only the glyphs. Skia offers both and this
/// crate pinned `Tight`, so the JavaScript binding could ask for either and
/// a Rust caller could not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RectHeightStyle {
    /// The glyphs and nothing more. The default, and what a hit test wants.
    #[default]
    Tight,
    /// The line's full height, so consecutive lines meet.
    Max,
    /// Half the line spacing above and below, except at the ends.
    IncludeLineSpacingMiddle,
    /// The line spacing above, so the first line reaches the paragraph top.
    IncludeLineSpacingTop,
    /// The line spacing below, so the last line reaches the bottom.
    IncludeLineSpacingBottom,
    /// The strut's height, where the paragraph style sets one.
    Strut,
}

impl RectHeightStyle {
    /// The Skia value this names.
    pub(crate) fn to_skia(self) -> SkRectHeightStyle {
        match self {
            Self::Tight => SkRectHeightStyle::Tight,
            Self::Max => SkRectHeightStyle::Max,
            Self::IncludeLineSpacingMiddle => {
                SkRectHeightStyle::IncludeLineSpacingMiddle
            }
            Self::IncludeLineSpacingTop => {
                SkRectHeightStyle::IncludeLineSpacingTop
            }
            Self::IncludeLineSpacingBottom => {
                SkRectHeightStyle::IncludeLineSpacingBottom
            }
            Self::Strut => SkRectHeightStyle::Strut,
        }
    }
}

/// How wide the rectangles [`Paragraph::rects_for_range`] returns are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RectWidthStyle {
    /// The glyphs and nothing more. The default.
    #[default]
    Tight,
    /// Widened to the line's full width, so a selection reaching the end of
    /// a wrapped line covers the space the wrap left behind.
    Max,
}

impl RectWidthStyle {
    /// The Skia value this names.
    pub(crate) fn to_skia(self) -> SkRectWidthStyle {
        match self {
            Self::Tight => SkRectWidthStyle::Tight,
            Self::Max => SkRectWidthStyle::Max,
        }
    }
}

/// Skia's name for a height style.
fn skia_height_style(style: RectHeightStyle) -> SkRectHeightStyle {
    match style {
        RectHeightStyle::Tight => SkRectHeightStyle::Tight,
        RectHeightStyle::Max => SkRectHeightStyle::Max,
        RectHeightStyle::IncludeLineSpacingMiddle => {
            SkRectHeightStyle::IncludeLineSpacingMiddle
        }
        RectHeightStyle::IncludeLineSpacingTop => {
            SkRectHeightStyle::IncludeLineSpacingTop
        }
        RectHeightStyle::IncludeLineSpacingBottom => {
            SkRectHeightStyle::IncludeLineSpacingBottom
        }
        RectHeightStyle::Strut => SkRectHeightStyle::Strut,
    }
}

/// Skia's name for a width style.
fn skia_width_style(style: RectWidthStyle) -> SkRectWidthStyle {
    match style {
        RectWidthStyle::Tight => SkRectWidthStyle::Tight,
        RectWidthStyle::Max => SkRectWidthStyle::Max,
    }
}

/// One of Skia's boxes, direction included.
fn text_box(box_: SkTextBox) -> TextBox {
    let r = box_.rect;
    TextBox {
        rect: Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        },
        direction: TextDirection::from_skia(box_.direct),
    }
}

/// One line of a measured run, and the single-font stretches inside it.
///
/// What [`TextMetrics`] reports per line, where the Canvas API's own
/// `TextMetrics` reports one set of numbers for the whole measurement. A
/// wrapped run has several lines and the standard shape cannot describe
/// them, which is why this is an extension on both surfaces rather than
/// something the browser has.
///
/// Every vertical value is relative to the same origin the measurement is:
/// the point the text would be drawn at, under the context's current
/// `text_baseline`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMetricsLine {
    /// Left edge of the line's box.
    pub x: f32,
    /// Top edge of the line's box.
    pub y: f32,
    /// Width of the line's box, trailing whitespace included.
    pub width: f32,
    /// Height of the line's box.
    pub height: f32,
    /// The selected baseline's position.
    pub baseline: f32,
    /// Highest ascent among the fonts used on this line.
    pub ascent: f32,
    /// Lowest descent among the fonts used on this line.
    pub descent: f32,
    /// The hanging baseline, whatever `text_baseline` selected.
    pub hanging_baseline: f32,
    /// The alphabetic baseline, whatever `text_baseline` selected.
    pub alphabetic_baseline: f32,
    /// The ideographic baseline, whatever `text_baseline` selected.
    pub ideographic_baseline: f32,
    /// UTF-16 index into the measured string where this line starts.
    ///
    /// UTF-16 rather than bytes, unlike [`LineMetrics`]: these indices are
    /// the ones the JavaScript surface reports, and a string index there is
    /// a UTF-16 offset. The two types answer different callers.
    pub start_index: usize,
    /// UTF-16 index one past where this line ends.
    pub end_index: usize,
    /// The single-font stretches this line is made of, in visual order.
    pub runs: Vec<TextMetricsRun>,
}

/// One stretch of a line drawn in a single font.
///
/// A line that falls back to a second family for an emoji, or mixes scripts,
/// is several of these. The font metrics differ per run, which is the reason
/// to report them separately: a line's `ascent` is the tallest of them, and
/// says nothing about where any particular glyph sits.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMetricsRun {
    /// Left edge of the run's inked bounds.
    pub x: f32,
    /// Top edge of the run's inked bounds.
    pub y: f32,
    /// Width of the run's inked bounds.
    pub width: f32,
    /// Height of the run's inked bounds.
    pub height: f32,
    /// The family this run was drawn in, as the typeface reports it.
    ///
    /// The resolved family, not the one asked for: this is where fallback
    /// becomes visible.
    pub family: String,
    /// This font's ascent.
    pub ascent: f32,
    /// This font's descent.
    pub descent: f32,
    /// Where this font's capital letters reach.
    pub cap_height: f32,
    /// Where this font's ascender-less letters reach.
    pub x_height: f32,
    /// Where an underline stroke sits, if the font says.
    pub underline: Option<f32>,
    /// Where a strikethrough stroke sits, if the font says.
    pub strikethrough: Option<f32>,
}

/// Per-line layout metrics. `start_index` and `end_index` are byte
/// offsets into the laid-out paragraph text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMetrics {
    /// Zero-based index of this line in the paragraph.
    pub line_number: usize,
    /// Byte offset of the line's first character.
    pub start_index: usize,
    /// Byte offset one past the line's last character.
    pub end_index: usize,
    /// Byte offset one past the last character that is not whitespace.
    ///
    /// Where a wrapped line's trailing spaces begin, which is what a
    /// selection highlight should stop at: `end_index` includes them and
    /// drawing to it puts a rectangle over blank space at the wrap point.
    pub end_excluding_whitespaces: usize,
    /// Byte offset one past the line's newline, where it ended at one.
    ///
    /// The counterpart to `end_excluding_whitespaces`, for a caller
    /// slicing the source text back into lines: the three offsets differ
    /// only on a line that wrapped or broke, and that is exactly where
    /// getting them confused is visible.
    pub end_including_newline: usize,
    /// Distance from the baseline to the top of the line, in pixels.
    pub ascent: f32,
    /// Distance from the baseline to the bottom of the line, in pixels.
    pub descent: f32,
    /// Running height of the paragraph through the end of this line, in pixels
    /// -- cumulative, not this line's own height.
    ///
    /// Skia derives it as `round(ascent + descent)` accumulated down the
    /// paragraph, so summing this across lines double-counts.
    pub height: f32,
    /// Width of the laid-out text on this line, in pixels.
    pub width: f32,
    /// Distance from the paragraph top to this line's baseline.
    pub baseline: f32,
    /// Left edge of the line after alignment, in pixels.
    pub left: f32,
    /// `true` when the line ended at an explicit newline rather than by
    /// wrapping.
    pub hard_break: bool,
}

/// Builds laid-out text from a `TextStyle` and a maximum line width.
///
/// Construct with `new(font_manager)` to use a registered font registry, or
/// `with_system_fonts()` for the platform's default fonts only.
pub struct TextEngine {
    /// The one collection every paragraph this engine builds resolves its
    /// fonts against.
    ///
    /// One for the engine's life rather than one per call, because Skia
    /// keeps its caches on it: the faces each family and style resolved to,
    /// each variable-font instance by typeface and axes, and each shaped
    /// paragraph. A variable font is instanced through the `FontArguments`
    /// on a paragraph's text style, so instancing needs no collection of
    /// its own.
    pub(crate) collection: FontCollection,
    /// The positions instanced through `collection` since its caches were
    /// last dropped, shared with every builder this engine opens.
    ///
    /// A `Mutex` for the unwind safety it keeps, not for threads: the
    /// engine is neither `Send` nor `Sync`, and a `RefCell` would take away
    /// `UnwindSafe` and `RefUnwindSafe`, which it has.
    positions: Rc<Mutex<InstancedPositions>>,
    /// What the engine has learned of the faces `collection` resolves
    /// styles to -- see [`FaceRecord`]. Shared with every builder this
    /// engine opens, and held, like `positions`, in a `Mutex` for the
    /// unwind safety it keeps.
    faces: Rc<Mutex<FaceRecord>>,
}

impl TextEngine {
    /// Builds using `font_manager`'s registered typefaces plus system
    /// fallbacks for unmatched family names.
    pub fn new(font_manager: &FontLibrary) -> Self {
        let asset_provider = font_manager.snapshot_provider();
        let mut collection = FontCollection::new();
        // The default manager needs a default *family*, not just a manager.
        // Without one, Skia's defaultFallback() has no name to resolve
        // and an unmatched family lands on the asset provider instead --
        // so once any font was registered, every lookup returned
        // it, including one that named no family at all. The Node FontLibrary
        // has always passed a name here; this mirrors it.
        let system_fonts = FontMgr::new();
        let default_family = system_fonts
            .legacy_make_typeface(None, FontStyle::default())
            .map(|face| face.family_name());
        collection.set_default_font_manager(
            system_fonts.clone(),
            default_family.as_deref(),
        );
        collection.set_asset_font_manager(Some(asset_provider.into()));
        // Resolve glyphs missing from the matched family against the
        // system fonts instead of rendering tofu -- matches CanvasKit's
        // `FontCollection.enableFontFallback`.
        collection.enable_font_fallback();
        Self {
            collection,
            positions: Rc::default(),
            faces: Rc::default(),
        }
    }

    /// Builds using the platform's system fonts only. Useful when no
    /// `FontLibrary` is needed.
    pub fn with_system_fonts() -> Self {
        let mut collection = FontCollection::new();
        // Named for the same reason `TextEngine::new` names one: a default
        // manager without a default *family* leaves `defaultFallback()` with
        // nothing to resolve. Text runs survive that, falling back glyph by
        // glyph, but anything that asks the collection for a face outright
        // does not -- a strut naming no family measured `-inf` on a machine
        // carrying only DejaVu, where the same strut naming `DejaVu Sans`
        // measured 64.
        let system_fonts = FontMgr::new();
        let default_family = system_fonts
            .legacy_make_typeface(None, FontStyle::default())
            .map(|face| face.family_name());
        collection.set_default_font_manager(
            system_fonts.clone(),
            default_family.as_deref(),
        );
        collection.enable_font_fallback();
        Self {
            collection,
            positions: Rc::default(),
            faces: Rc::default(),
        }
    }

    /// Lays out `text` against `style`, wrapping at `max_width`.
    ///
    /// Returns a `Paragraph` that can be measured or drawn via
    /// `Canvas::draw_text_layout`.
    pub fn layout_text(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: f32,
    ) -> Paragraph {
        let mut builder = self.paragraph_builder(style);
        builder.add_text(text);
        builder.build(max_width)
    }

    /// Lays out a rich-text paragraph.
    ///
    /// The paragraph-level state (`align`, `line_height_multiplier`) comes from
    /// `base_style`; each `RichTextSpan` overlays its own per-span style for
    /// the span's text (font, color, decoration, baseline shift, etc.).
    ///
    /// A span's [`font_variations`](TextStyle::font_variations) decide its
    /// axes: [`Inherit`](FontVariationSettings::Inherit) takes
    /// `base_style`'s, [`Normal`](FontVariationSettings::Normal) the face's
    /// own, and [`Axes`](FontVariationSettings::Axes) replaces `base_style`'s
    /// as a nested `font-variation-settings` replaces its parent's in CSS.
    /// Either way the span's own `font_weight` supplies `wght` unless the
    /// axes in force name it.
    ///
    /// Each span is pushed and popped in turn, so the styles do not nest.
    /// Use [`TextEngine::paragraph_builder`] for a paragraph that needs a
    /// style stack, or one that needs [`Placeholder`]s.
    pub fn layout_rich_text(
        &self,
        spans: &[RichTextSpan],
        base_style: &TextStyle,
        max_width: f32,
    ) -> Paragraph {
        let mut builder = self.paragraph_builder(base_style);
        for span in spans {
            builder.push_style(&span.style);
            builder.add_text(&span.text);
            builder.pop();
        }
        builder.build(max_width)
    }

    /// Opens a paragraph and returns the builder that fills it in.
    ///
    /// The incremental counterpart to [`layout_rich_text`]: styles nest
    /// through [`push_style`] and [`pop`], and [`add_placeholder`] reserves a
    /// box the text flows around. This is the surface the JavaScript side
    /// calls `ParagraphBuilder`.
    ///
    /// Paragraph-level state -- alignment, line-height multiplier, strut --
    /// is fixed here from `base_style` and cannot be changed once the
    /// builder exists, because Skia settles the paragraph style before the
    /// first character is added. `base_style`'s font variations are the ones
    /// a pushed style that inherits is instanced at, as
    /// [`layout_rich_text`] does for its spans.
    ///
    /// [`layout_rich_text`]: TextEngine::layout_rich_text
    /// [`push_style`]: ParagraphBuilder::push_style
    /// [`pop`]: ParagraphBuilder::pop
    /// [`add_placeholder`]: ParagraphBuilder::add_placeholder
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let engine = TextEngine::with_system_fonts();
    /// let mut paragraph = engine.paragraph_builder(&TextStyle::default());
    /// paragraph.add_text("before ");
    /// paragraph.add_placeholder(Placeholder::new(24.0, 24.0));
    /// paragraph.add_text(" after");
    /// let layout = paragraph.build(400.0);
    ///
    /// // The box the caller now knows where to draw into.
    /// assert_eq!(layout.rects_for_placeholders().len(), 1);
    /// ```
    #[doc(alias = "ParagraphBuilder")]
    pub fn paragraph_builder(
        &self,
        base_style: &TextStyle,
    ) -> ParagraphBuilder {
        let mut collection = self.collection.clone();
        let strut = strut_families(base_style, &mut collection.clone());
        // A base style has nothing to inherit, so `Inherit` is `Normal` here.
        let base_variations = base_style.font_variations.resolve(&[]);
        let mut instance = instance_for(
            &mut collection.clone(),
            &self.positions,
            &self.faces,
            &FaceQuery::of(
                base_style,
                &oblique_variations(base_style, base_variations),
            ),
        );
        if let TextSlant::Oblique(angle) = base_style.slant {
            add_slnt(&mut instance.coordinates, angle.degrees());
        }
        let mut base_sk_style = build_text_style(base_style, &instance);
        let base_oblique =
            base_style.slant.oblique_degrees().and_then(|degrees| {
                match_oblique(&mut base_sk_style, &mut collection, degrees)
            });
        let paragraph_style =
            build_paragraph_style(base_style, &base_sk_style, &strut);
        ParagraphBuilder {
            // The engine's collection is built over a library of its own,
            // which has no epoch to key a face by.
            recipe: ParagraphRecipe::new(
                paragraph_style,
                base_oblique,
                collection.clone(),
                None,
            ),
            collection,
            positions: self.positions.clone(),
            faces: self.faces.clone(),
            variations: base_variations.to_vec(),
        }
    }
}

/// Which side of a character boundary a text position sits on.
///
/// A caret between two characters belongs to one of them. At a line wrap
/// both sides are the same index, and this is what tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Affinity {
    /// The position belongs to the character before it -- the end of the
    /// previous line, at a wrap.
    Upstream,
    /// The position belongs to the character after it -- the start of the
    /// next line. The usual answer.
    #[default]
    Downstream,
}

/// A position within laid-out text, as
/// [`Paragraph::glyph_position_at_coordinate`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextPosition {
    /// Offset in UTF-8 bytes from the start of the laid-out text.
    pub index: usize,
    /// Which side of the boundary at `index` the point fell on.
    pub affinity: Affinity,
}

/// A paragraph under construction, from [`TextEngine::paragraph_builder`].
///
/// Text is added in the style on top of the stack, so a run inherits
/// everything pushed beneath it and [`pop`](Self::pop) returns to what came
/// before. That is the difference from
/// [`layout_rich_text`](TextEngine::layout_rich_text), whose spans each push
/// and pop in turn and so cannot nest.
///
/// The style stack starts empty, which means the paragraph's base style. A
/// [`pop`](Self::pop) on the empty stack is ignored rather than a panic:
/// Skia's builder does the same, and a mismatched pop is a bug in the
/// caller's bookkeeping, not a reason to take the process down mid-layout.
pub struct ParagraphBuilder {
    /// The steps the paragraph is built from.
    recipe: ParagraphRecipe,
    /// What a pushed style resolves its faces against, to decide whether
    /// they are variable and whether one leans for an oblique.
    collection: FontCollection,
    /// The engine's record of the positions instanced through `collection`.
    positions: Rc<Mutex<InstancedPositions>>,
    /// The engine's record of its faces -- see [`FaceRecord`].
    faces: Rc<Mutex<FaceRecord>>,
    /// The base style's axes, which a pushed style that inherits is
    /// instanced at.
    variations: Vec<FontVariation>,
}

impl ParagraphBuilder {
    /// Pushes `style` onto the stack. Text added from here on is drawn in it.
    ///
    /// Its [`font_variations`](TextStyle::font_variations) decide its axes:
    /// [`Inherit`](FontVariationSettings::Inherit) takes the base style's,
    /// [`Normal`](FontVariationSettings::Normal) the face's own, and
    /// [`Axes`](FontVariationSettings::Axes) replaces the base style's, as a
    /// nested `font-variation-settings` replaces its parent's in CSS. Either
    /// way its own `font_weight` supplies `wght` unless the axes in force
    /// name it.
    pub fn push_style(&mut self, style: &TextStyle) -> &mut Self {
        let variations = style.font_variations.resolve(&self.variations);
        let mut instance = instance_for(
            &mut self.collection.clone(),
            &self.positions,
            &self.faces,
            &FaceQuery::of(style, &oblique_variations(style, variations)),
        );
        if let TextSlant::Oblique(angle) = style.slant {
            add_slnt(&mut instance.coordinates, angle.degrees());
        }
        let mut sk_style = build_text_style(style, &instance);
        let oblique = style.slant.oblique_degrees().and_then(|degrees| {
            match_oblique(&mut sk_style, &mut self.collection, degrees)
        });
        match carries_taken(style) {
            true => self.recipe.push_style(sk_style, oblique),
            false => self.recipe.push_style_as_given(sk_style, oblique),
        }
        self
    }

    /// Pops the top style, returning to the one beneath.
    pub fn pop(&mut self) -> &mut Self {
        self.recipe.pop();
        self
    }

    /// Adds `text` in the style currently on top of the stack.
    pub fn add_text(&mut self, text: &str) -> &mut Self {
        self.recipe.add_text(text);
        self
    }

    /// Reserves a box the text flows around.
    ///
    /// Nothing is drawn into it. [`Paragraph::rects_for_placeholders`]
    /// reports where each one landed, in the order they were added, which is
    /// what the caller draws against.
    pub fn add_placeholder(&mut self, placeholder: Placeholder) -> &mut Self {
        self.recipe.add_placeholder(&placeholder.to_skia());
        self
    }

    /// Closes the paragraph and lays it out, wrapping at `max_width`.
    pub fn build(self, max_width: f32) -> Paragraph {
        let uncached = self.recipe.uncached();
        let (paragraph, passes) = self.recipe.build();
        let mut paragraph = Paragraph {
            paragraph: RefCell::new(paragraph),
            passes: RefCell::new(passes),
            uncached,
            max_width,
        };
        paragraph.layout(max_width);
        paragraph
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test to paint each paragraph as its caller asked for it,
    /// its shadows SkParagraph's to draw: the control for what
    /// `node::oblique::Passes` draws in their place.
    pub(crate) static PAINT_AS_ASKED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Result of `TextEngine::layout_text`.
///
/// Owns the laid-out paragraph; metrics queries are cheap and
/// `draw_text_layout` paints the same paragraph onto a canvas.
pub struct Paragraph {
    /// The laid-out paragraph, and what draws its oblique ranges and its
    /// decorations where it has any.
    ///
    /// Each in a `RefCell` because the first paint after a layout follows
    /// it, reading its glyph runs back with `Paragraph::visit`, which takes
    /// the paragraph mutably, through the `&Paragraph` that
    /// `Context2D::draw_paragraph` is given -- see
    /// `node::oblique::Passes::follow`. Each borrow lasts one call of this
    /// type's, and none of them calls back into it, so no borrow meets
    /// another. A `RefCell` takes away no auto trait the type had: it was
    /// neither `Send`, `Sync` nor unwind-safe before.
    pub(crate) paragraph: RefCell<SkParagraph>,
    passes: RefCell<Option<Passes>>,
    /// The collection whose paragraph cache a layout goes without -- see
    /// `node::oblique::without_paragraph_cache`.
    uncached: Option<FontCollection>,
    max_width: f32,
}

impl Paragraph {
    /// Lays the paragraph out again at a different width.
    ///
    /// Building one already lays it out, so this is for the second and
    /// every later width: a paragraph re-wrapped on a window resize, or
    /// measured at several widths to find one that fits. Rebuilding
    /// instead re-parses the runs, re-resolves the fonts and re-shapes
    /// every glyph, all of which this reuses -- which is what the
    /// JavaScript binding's `layout()` has always done and this side made
    /// a caller pay for once per frame.
    ///
    /// Every metric on this type reports the new layout afterwards.
    pub fn layout(&mut self, max_width: f32) {
        let Self {
            paragraph,
            passes,
            uncached,
            ..
        } = self;
        without_paragraph_cache(uncached.as_ref(), || {
            paragraph.get_mut().layout(max_width);
            if let Some(passes) = passes.get_mut() {
                passes.lay_out(max_width);
            }
        });
        self.max_width = max_width;
    }

    /// Paints the paragraph with its top-left corner at `origin`: through
    /// `Paragraph::paint`, with its oblique ranges sheared, its shadows and
    /// its decorations drawn where it has any -- see `node::oblique`.
    pub(crate) fn paint(&self, canvas: &SkCanvas, origin: SkPoint) {
        let mut passes = self.passes.borrow_mut();
        #[cfg(test)]
        if PAINT_AS_ASKED.get()
            && let Some(as_asked) =
                passes.as_ref().and_then(|p| p.as_asked.as_ref())
        {
            as_asked.paint(canvas, origin);
            return;
        }
        let mut paragraph = self.paragraph.borrow_mut();
        match passes.as_mut() {
            Some(passes) => passes.paint(&mut paragraph, canvas, origin),
            None => paragraph.paint(canvas, origin),
        }
    }

    /// How many times this paragraph's layouts have been followed for
    /// drawing -- see `node::oblique::Passes::follow` -- or `None` where it
    /// has nothing drawn by passes.
    #[cfg(test)]
    pub(crate) fn follows(&self) -> Option<usize> {
        self.passes.borrow().as_ref().map(|passes| passes.follows)
    }

    /// The distance from the top of the layout to the alphabetic baseline
    /// of the first line.
    ///
    /// Where Latin letters sit. Add it to a draw's y coordinate to place
    /// text by its baseline rather than by its top edge, which is what the
    /// Canvas `textBaseline` of `"alphabetic"` means.
    pub fn alphabetic_baseline(&self) -> f32 {
        self.paragraph.borrow().alphabetic_baseline()
    }

    /// The distance from the top of the layout to the ideographic baseline
    /// of the first line.
    ///
    /// Lower than the alphabetic one, and where CJK glyphs sit.
    pub fn ideographic_baseline(&self) -> f32 {
        self.paragraph.borrow().ideographic_baseline()
    }

    /// The narrowest width the text could be laid out in without a word
    /// having to break.
    ///
    /// The longest single word, in effect. Laying out narrower than this
    /// overflows or breaks mid-word depending on the style.
    pub fn min_intrinsic_width(&self) -> f32 {
        self.paragraph.borrow().min_intrinsic_width()
    }

    /// The width the text would take with no wrapping at all.
    ///
    /// The whole paragraph on one line. Together with
    /// [`min_intrinsic_width`](Self::min_intrinsic_width) these bracket
    /// every width worth laying out at -- wider than this changes nothing,
    /// and narrower than that cannot be honoured.
    pub fn max_intrinsic_width(&self) -> f32 {
        self.paragraph.borrow().max_intrinsic_width()
    }

    /// The character index nearest `(x, y)`, and which side of it the point
    /// falls on.
    ///
    /// What turns a click into a caret position. The index counts UTF-8
    /// bytes from the start of the laid-out text, and the affinity says
    /// whether the point was before or after the boundary -- which is what
    /// distinguishes the end of one line from the start of the next at a
    /// wrap, since both are the same index.
    pub fn glyph_position_at_coordinate(&self, x: f32, y: f32) -> TextPosition {
        let found = self
            .paragraph
            .borrow()
            .get_glyph_position_at_coordinate((x, y));
        TextPosition {
            index: found.position.max(0) as usize,
            affinity: match found.affinity {
                SkAffinity::Upstream => Affinity::Upstream,
                _ => Affinity::Downstream,
            },
        }
    }

    /// Measured width of the longest laid-out line, after wrapping.
    ///
    /// The width the laid-out content actually occupies, not the wrapping
    /// budget. Use [`Paragraph::max_width`] to recover the layout budget the
    /// caller asked for.
    pub fn width(&self) -> f32 {
        self.paragraph.borrow().longest_line()
    }

    /// The `max_width` (wrapping budget) requested at layout time.
    pub fn max_width(&self) -> f32 {
        self.max_width
    }

    /// Total height of the laid-out paragraph after wrapping.
    pub fn height(&self) -> f32 {
        self.paragraph.borrow().height()
    }

    /// Number of laid-out lines (0 if the input was empty).
    pub fn line_count(&self) -> usize {
        self.paragraph.borrow().line_number()
    }

    /// Distance from the paragraph's top edge to the first line's baseline
    /// ascent.
    ///
    /// Useful for vertical alignment of text against a known baseline.
    ///
    /// **A paragraph with no lines answers `0.0`.** Laying out `""` produces
    /// an empty metrics vector -- `" "` and `"\n"` do not, giving one line and
    /// two -- so this is reachable rather than theoretical. The answer is not
    /// ambiguous in practice, since a line that exists has a positive ascent,
    /// but it is stated because nothing else says which of the two a `0.0`
    /// means.
    pub fn first_line_ascent(&self) -> f32 {
        self.paragraph
            .borrow()
            .get_line_metrics()
            .first()
            .map(|m| m.ascent as f32)
            .unwrap_or_default()
    }

    /// Per-line metrics for the laid-out paragraph. The vector is
    /// indexed by line number and ordered top-to-bottom.
    pub fn line_metrics(&self) -> Vec<LineMetrics> {
        self.paragraph
            .borrow()
            .get_line_metrics()
            .iter()
            .enumerate()
            .map(|(i, m)| LineMetrics {
                line_number: i,
                start_index: m.start_index,
                end_index: m.end_index,
                end_excluding_whitespaces: m.end_excluding_whitespaces,
                end_including_newline: m.end_including_newline,
                ascent: m.ascent as f32,
                descent: m.descent as f32,
                height: m.height as f32,
                width: m.width as f32,
                baseline: m.baseline as f32,
                left: m.left as f32,
                hard_break: m.hard_break,
            })
            .collect()
    }

    /// Bounding rectangles for the byte range `[range.start, range.end)` in the
    /// laid-out paragraph.
    ///
    /// Useful for selection rendering and for placing baseline-shift overlays
    /// (e.g. superscripts) directly over the affected glyphs.
    pub fn rects_for_range(
        &self,
        range: Range<usize>,
        height_style: RectHeightStyle,
        width_style: RectWidthStyle,
    ) -> Vec<TextBox> {
        self.paragraph
            .borrow()
            .get_rects_for_range(
                range,
                skia_height_style(height_style),
                skia_width_style(width_style),
            )
            .into_iter()
            .map(text_box)
            .collect()
    }

    /// Whether layout dropped content because it exceeded the paragraph style's
    /// `max_lines`.
    ///
    /// Drives auto-fit / "text overflows" logic. Mirrors CanvasKit's
    /// `Paragraph.didExceedMaxLines`.
    pub fn did_exceed_max_lines(&self) -> bool {
        self.paragraph.borrow().did_exceed_max_lines()
    }

    /// Bounding boxes of the inline placeholders added during layout, in
    /// paragraph-local coordinates and in insertion order.
    ///
    /// Mirrors CanvasKit's `Paragraph.getRectsForPlaceholders` -- the readback
    /// counterpart to placeholder insertion, for positioning inline
    /// icons/images.
    pub fn rects_for_placeholders(&self) -> Vec<TextBox> {
        self.paragraph
            .borrow()
            .get_rects_for_placeholders()
            .into_iter()
            .map(text_box)
            .collect()
    }

    /// Codepoints that no font in the collection could resolve (tofu / missing
    /// glyphs), for validating automated multi-language renders.
    ///
    /// Mirrors CanvasKit's `Paragraph.unresolvedCodepoints`. Requires `&mut
    /// self`: Skia computes this lazily on the laid-out paragraph.
    pub fn unresolved_codepoints(&mut self) -> Vec<u32> {
        self.paragraph
            .get_mut()
            .unresolved_codepoints()
            .into_iter()
            .map(|cp| cp as u32)
            .collect()
    }
}

/// The `SkFontStyle` `style` asks the font matcher for.
fn font_style_for(style: &TextStyle) -> FontStyle {
    FontStyle::new(
        Weight::from(style.font_weight),
        style.stretch.to_skia(),
        style.slant.to_skia(),
    )
}

/// `variations`, with `slnt` set to the oblique's angle ahead of them where
/// `style` asks for oblique -- negated, as OpenType counts the axis
/// counter-clockwise -- so that a `slnt` they name wins.
fn oblique_variations<'a>(
    style: &TextStyle,
    variations: &'a [FontVariation],
) -> Cow<'a, [FontVariation]> {
    match style.slant {
        TextSlant::Oblique(angle) => Cow::Owned(
            once(FontVariation::new(FontAxisTag::SLNT, -angle.degrees()))
                .chain(variations.iter().copied())
                .collect(),
        ),
        _ => Cow::Borrowed(variations),
    }
}

/// What [`instance_for`] is asked about a style, and everything it reads of
/// it: the style's families, the weight, width and slant it matches faces
/// by, its size, and the variation axes it is instanced at.
///
/// A query rather than the style, so that [`InstanceKey`], built from all
/// of a query, is all an instance depends on.
struct FaceQuery<'a> {
    families: &'a [FontFamily],
    font_style: FontStyle,
    font_size: f32,
    variations: &'a [FontVariation],
}

impl<'a> FaceQuery<'a> {
    /// `style` instanced at `variations`, the axes its
    /// [`font_variations`](TextStyle::font_variations) resolve to.
    fn of(style: &'a TextStyle, variations: &'a [FontVariation]) -> Self {
        Self {
            families: &style.font_families,
            font_style: font_style_for(style),
            font_size: style.font_size,
            variations,
        }
    }
}

/// A [`FaceQuery`] as an engine remembers the instance it was answered
/// with: the family list by value and the numbers by their bits, so that a
/// query matches a key exactly where every part of the two is equal.
struct InstanceKey {
    families: Box<[FontFamily]>,
    weight: i32,
    width: i32,
    slant: Slant,
    font_size: u32,
    variations: Box<[(FontAxisTag, u32)]>,
}

impl InstanceKey {
    /// The key for `query`.
    fn new(query: &FaceQuery) -> Self {
        // Every field taken apart, here and in `matches` and `hash_query`,
        // so that a part added to the query and left out of the key is a
        // compile error rather than a reuse of an instance made for
        // something else.
        let FaceQuery {
            families,
            font_style,
            font_size,
            variations,
        } = query;
        Self {
            families: families.to_vec().into_boxed_slice(),
            weight: *font_style.weight(),
            width: *font_style.width(),
            slant: font_style.slant(),
            font_size: font_size.to_bits(),
            variations: variations
                .iter()
                .map(|variation| (variation.axis, variation.value.to_bits()))
                .collect(),
        }
    }

    /// Whether `query` is the query this key was made from.
    fn matches(&self, query: &FaceQuery) -> bool {
        let FaceQuery {
            families,
            font_style,
            font_size,
            variations,
        } = query;
        *self.families == **families
            && self.weight == *font_style.weight()
            && self.width == *font_style.width()
            && self.slant == font_style.slant()
            && self.font_size == font_size.to_bits()
            && self.variations.len() == variations.len()
            && self.variations.iter().zip(variations.iter()).all(
                |(&(axis, bits), variation)| {
                    axis == variation.axis && bits == variation.value.to_bits()
                },
            )
    }
}

/// `query` hashed as an [`InstanceKey`] made from it is, part for part, so
/// that a lookup hashes the borrowed query once and copies nothing.
fn hash_query(hasher: &CacheHasher, query: &FaceQuery) -> u64 {
    let FaceQuery {
        families,
        font_style,
        font_size,
        variations,
    } = query;
    let mut state = hasher.build_hasher();
    families.hash(&mut state);
    (
        *font_style.weight(),
        *font_style.width(),
        font_style.slant(),
        font_size.to_bits(),
    )
        .hash(&mut state);
    for variation in *variations {
        (variation.axis, variation.value.to_bits()).hash(&mut state);
    }
    state.finish()
}

/// The instances an engine remembers, each under its [`InstanceKey`] in a
/// bucket by the query's hash, looked up by the borrowed query.
#[derive(Default)]
struct InstanceMemo {
    hasher: CacheHasher,
    buckets: CacheMap<u64, Vec<(InstanceKey, Instance)>>,
    len: usize,
}

impl InstanceMemo {
    /// The instance remembered for `query`, if any.
    fn get(&self, query: &FaceQuery) -> Option<&Instance> {
        self.buckets
            .get(&hash_query(&self.hasher, query))?
            .iter()
            .find(|(key, _)| key.matches(query))
            .map(|(_, instance)| instance)
    }

    /// Remembers `instance` for `query`, dropping every instance first
    /// where [`MATCHES_KEPT`] are kept already.
    fn insert(&mut self, query: &FaceQuery, instance: Instance) {
        if self.len >= MATCHES_KEPT {
            self.clear();
        }
        self.buckets
            .entry(hash_query(&self.hasher, query))
            .or_default()
            .push((InstanceKey::new(query), instance));
        self.len += 1;
    }

    fn clear(&mut self) {
        self.buckets.clear();
        self.len = 0;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
}

/// How a style's variable faces are instanced.
#[derive(Clone, Default)]
struct Instance {
    /// The axis positions, handed to Skia as the text style's
    /// `FontArguments`.
    coordinates: Vec<Coordinate>,
    /// The weight the instance has: its `wght` position, clamped to the
    /// face's axis as Skia clamps it. `None` when nothing is instanced or
    /// the face has no `wght` axis.
    weight: Option<i32>,
    /// The names the style's families are asked for by, as
    /// [`collection_names`] gave them to find the faces, so that the text
    /// style is given the same ones without working them out again. `None`
    /// for a style that names no family.
    names: Option<Rc<[String]>>,
}

#[cfg(test)]
thread_local! {
    /// How many times [`FaceMatches::of`] has searched a collection on this
    /// thread, for the tests that hold it to remembering.
    pub(crate) static SEARCHES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many lists of coordinates [`instance_for`] has made on this
    /// thread, for the test that holds it to making none for a style at
    /// its faces' own position.
    pub(crate) static COORDINATE_LISTS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many styles [`instance_for`] has worked out on this thread, for
    /// the tests that hold it to reusing one it worked out before.
    pub(crate) static INSTANCES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many times this thread has asked the font library for a family
    /// list's names, for the test that holds a remembered instance to
    /// asking none.
    pub(crate) static NAME_LOOKUPS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// What an engine has learned of its faces: what each family list and
/// style matched, each face's axes, and the instance each query was
/// answered with. One record behind one lock, taken once a style rather
/// than once for the match and again for each face's axes.
#[derive(Default)]
pub(crate) struct FaceRecord {
    matches: FaceMatches,
    axes: KnownAxes,
    /// The instance each [`FaceQuery`] was answered with, so that a style
    /// met before -- a span pushed in the paragraph's own font, or any
    /// style a caller lays out again -- is not worked out again. Never
    /// stale for the reason a match is not: the collection is the engine's
    /// for its life and a face's axes are fixed. Dropped whole at
    /// [`MATCHES_KEPT`] and when the fonts change.
    instances: InstanceMemo,
    /// The [`font_generation`] the shared library's fonts were at when
    /// `matches` and `instances` were filled. When the fonts change the
    /// library spells every family list again, as a new list and perhaps
    /// differently, so both are dropped then. A face's axes stay, being
    /// fixed.
    generation: Option<u64>,
}

/// How many family lists and styles an engine remembers the matches of.
///
/// A bound rather than a working set: a weight that animates names a new
/// style every frame. Past it every remembered match is dropped and searched
/// again.
const MATCHES_KEPT: usize = 256;

/// The faces an engine's collection has matched each family list and style
/// to, searched once each.
///
/// The collection is the engine's for its life, built over a snapshot of
/// its library, so a search of it answers the same every time: an entry is
/// never stale, and goes at [`MATCHES_KEPT`], when the shared library's
/// fonts change -- see [`FaceRecord::epoch`] -- or with the engine. The
/// search was most of what instancing a style for its variable faces cost
/// on a style met before.
#[derive(Default)]
pub(crate) struct FaceMatches {
    matches: CacheMap<FaceMatchKey, Rc<[Typeface]>>,
}

/// What a search of an engine's collection is asked: a family list, as the
/// names [`collection_names`] handed out for it, and the style's weight,
/// width and slant.
///
/// The names are compared and hashed by which list they are, not by what
/// they spell: the library hands out one list for a family list for as long
/// as it keeps it, so this costs no hash of the strings, which the lookup
/// that found the list has already paid. The key holds the list, so its
/// allocation cannot be freed and its address taken by another while the
/// entry lives -- equal addresses are the same list. A list the library
/// works out again, after the fonts change or past its bound, is a new
/// allocation, and misses rather than answering for the one before.
struct FaceMatchKey {
    names: Rc<[String]>,
    weight: i32,
    width: i32,
    slant: Slant,
}

impl PartialEq for FaceMatchKey {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.names, &other.names)
            && (self.weight, self.width, self.slant)
                == (other.weight, other.width, other.slant)
    }
}

impl Eq for FaceMatchKey {}

impl Hash for FaceMatchKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.names).cast::<()>().hash(state);
        (self.weight, self.width, self.slant).hash(state);
    }
}

impl FaceMatches {
    /// The faces the family list `names` -- as [`collection_names`] handed
    /// it out -- in `style` match in `collection`.
    pub(crate) fn of(
        &mut self,
        collection: &mut FontCollection,
        names: &Rc<[String]>,
        style: FontStyle,
    ) -> Rc<[Typeface]> {
        let key = FaceMatchKey {
            names: names.clone(),
            weight: *style.weight(),
            width: *style.width(),
            slant: style.slant(),
        };
        let remember = !forgetful();
        if remember && let Some(faces) = self.matches.get(&key) {
            return faces.clone();
        }
        #[cfg(test)]
        SEARCHES.set(SEARCHES.get() + 1);
        let families: Vec<&str> = names.iter().map(String::as_str).collect();
        let faces: Rc<[Typeface]> =
            collection.find_typefaces(&families, style).into();
        if remember {
            if self.matches.len() >= MATCHES_KEPT {
                self.matches.clear();
            }
            self.matches.insert(key, faces.clone());
        }
        faces
    }
}

/// How the style `query` describes has its variable faces instanced: at the
/// explicit variations, at `wght` from its own weight unless they name it,
/// and at `opsz` from its font size unless they name that.
///
/// OpenType defines `wght` on the CSS `font-weight` scale, so the weight is
/// the coordinate unchanged. An explicit `wght` wins over it, as
/// `font-variation-settings` wins over `font-weight` in CSS -- Chrome 152
/// lays `font-weight: 700` with `"wght" 300` out at the width of 300. Skia
/// clamps each position to the face's range and ignores an axis the face
/// lacks. The `opsz` position is [`optical_size`]'s, per style, so a span
/// sized apart from its paragraph is drawn at its own size's design.
///
/// Worked out once for each query the engine meets and then remembered in
/// `record` -- see [`FaceRecord::instances`]. Every time, remembered or
/// not, an instanced position is recorded in `positions` before it is
/// handed back, and where it is one past
/// [`INSTANCED_POSITIONS`](crate::font::INSTANCED_POSITIONS) `collection`'s
/// caches are dropped first, since Skia keeps every typeface it instances
/// for one.
///
/// Nothing unless one of the families the query names resolves to a
/// variable face in `collection`. Instancing clones the typeface, and a
/// clone of a face with no axes is a new typeface with its own glyph cache
/// that draws nothing different. A style naming no family is left to the
/// platform's default, which the matcher already resolves by weight.
fn instance_for(
    collection: &mut FontCollection,
    positions: &Mutex<InstancedPositions>,
    record: &Mutex<FaceRecord>,
    query: &FaceQuery,
) -> Instance {
    if query.families.is_empty() {
        return Instance::default();
    }
    let generation = font_generation();
    // A poisoned record is still a record: the panic that poisoned it was
    // elsewhere, and the worst a stale entry can do is drop the caches early.
    let mut record = record.lock().unwrap_or_else(PoisonError::into_inner);
    if record.generation != Some(generation) {
        record.matches = FaceMatches::default();
        record.instances.clear();
        record.generation = Some(generation);
    }
    let remember = !forgetful();
    let instance = match record.instances.get(query).filter(|_| remember) {
        Some(instance) => {
            // As the library lookup a remembered instance skips would have:
            // text laid out without being drawn holds an idle release off.
            note_font_use();
            instance.clone()
        }
        None => {
            let names = collection_names(query.families);
            let instance = work_out(collection, &mut record, names, query);
            if remember {
                record.instances.insert(query, instance.clone());
            }
            instance
        }
    };
    drop(record);
    if !instance.coordinates.is_empty()
        && let Some(names) = &instance.names
    {
        let families: Vec<&str> = names.iter().map(String::as_str).collect();
        if positions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .admit(&families, &instance.coordinates)
        {
            collection.clear_caches();
        }
    }
    instance
}

/// The instance for `query`, whose families `names` stands for, worked out
/// from the faces they match in `collection` -- see [`instance_for`].
fn work_out(
    collection: &mut FontCollection,
    record: &mut FaceRecord,
    names: Rc<[String]>,
    query: &FaceQuery,
) -> Instance {
    #[cfg(test)]
    INSTANCES.set(INSTANCES.get() + 1);
    let FaceRecord {
        matches,
        axes: known,
        ..
    } = record;
    let faces = matches.of(collection, &names, query.font_style);
    let Some(axes) = faces.iter().find_map(|face| known.of(face)) else {
        return Instance {
            names: Some(names),
            ..Instance::default()
        };
    };
    let variations = query.variations;
    let wght_tag = u32::from_be_bytes(*FontAxisTag::WGHT.as_bytes());
    let explicit_wght = variations
        .iter()
        .find(|variation| variation.axis == FontAxisTag::WGHT)
        .map(|variation| variation.value);
    // `SkFontStyle` has already pinned the weight to `0..=1000`.
    let wght = explicit_wght.unwrap_or(*query.font_style.weight() as f32);
    let explicit_opsz = variations
        .iter()
        .any(|variation| variation.axis == FontAxisTag::OPSZ);
    let opsz = match explicit_opsz {
        true => None,
        false => optical_size(&axes, query.font_size),
    };
    // Walked rather than collected until an instance is made: a style at
    // its faces' own position, the common case, makes none.
    let coordinates = || {
        variations
            .iter()
            .filter(|variation| variation.axis != FontAxisTag::WGHT)
            .map(|variation| Coordinate {
                axis: FourByteTag::from(u32::from_be_bytes(
                    *variation.axis.as_bytes(),
                )),
                value: variation.value,
            })
            .chain([Coordinate {
                axis: FourByteTag::from(wght_tag),
                value: wght,
            }])
            .chain(opsz)
    };
    let weight = axes
        .iter()
        .find(|axis| *axis.tag == wght_tag)
        .map(|axis| wght.clamp(axis.min, axis.max).round() as i32);
    // At the faces' own position an instance draws as the faces do, glyph
    // for glyph and advance for advance, and costs a typeface, its font
    // arguments on every style copied and a position recorded; so where
    // every variable face the families resolve to is there, none is made.
    // Every face, since the arguments reach a fallback face as well as the
    // first, and its own defaults may differ.
    let at_default = faces.iter().all(|face| {
        known
            .of(face)
            .is_none_or(|axes| at_default_position(&axes, coordinates()))
    });
    if at_default {
        return Instance {
            coordinates: Vec::new(),
            weight,
            names: Some(names),
        };
    }
    #[cfg(test)]
    COORDINATE_LISTS.set(COORDINATE_LISTS.get() + 1);
    Instance {
        coordinates: coordinates().collect(),
        weight,
        names: Some(names),
    }
}

/// Whether `coordinates` leave a face with `axes` where it is without them:
/// each at its axis's default, a coordinate for an axis the face lacks being
/// one Skia ignores. Never for a face with an `opsz` axis, whose position
/// follows the font size, [`optical_size`]'s: uninstanced, FreeType draws
/// it at the axis's default whatever the size, and CoreText at an optical
/// size it picks for itself, which differs from the instance even at that
/// default.
fn at_default_position(
    axes: &[Axis],
    mut coordinates: impl Iterator<Item = Coordinate>,
) -> bool {
    let opsz =
        FourByteTag::from(u32::from_be_bytes(*FontAxisTag::OPSZ.as_bytes()));
    !axes.iter().any(|axis| axis.tag == opsz)
        && coordinates.all(|coordinate| {
            axes.iter()
                .find(|axis| axis.tag == coordinate.axis)
                .is_none_or(|axis| axis.def == coordinate.value)
        })
}

/// Whether [`build_text_style`] gives `style`'s Skia style a decoration,
/// shadows or a background, the three things a recipe takes off a style
/// to draw itself: decided here, from the fields it builds them from, so
/// that a style with none is pushed without its Skia style being read for
/// them.
fn carries_taken(style: &TextStyle) -> bool {
    style.decoration.to_skia() != SkTextDecoration::NO_DECORATION
        || !style.shadows.is_empty()
        || style.background_color.is_some()
}

/// The Skia text style for `style`, with its variable faces instanced as
/// `instance` says.
fn build_text_style(style: &TextStyle, instance: &Instance) -> SkTextStyle {
    let mut sk_style = SkTextStyle::new();

    let mut paint = SkPaint::default();
    let cs = linear_srgb_color_space();
    // `foreground_color` wins where it is set, which is what makes it an
    // override rather than a second colour with no rule between them.
    let fill = style.foreground_color.unwrap_or(style.color);
    paint.set_color4f(rgba_linear_to_unpremul_color4f(fill), Some(&cs));
    paint.set_anti_alias(true);
    // Not positive is ignored, as `lineWidth` is: Skia reads a zero width as
    // a hairline rather than as no stroke.
    if let Some(width) = style.stroke_width.filter(|w| *w > 0.0) {
        paint.set_style(SkPaintStyle::Stroke);
        paint.set_stroke_width(width);
    }
    sk_style.set_foreground_paint(&paint);

    if let Some(behind) = style.background_color {
        let mut background = SkPaint::default();
        background
            .set_color4f(rgba_linear_to_unpremul_color4f(behind), Some(&cs));
        background.set_anti_alias(true);
        sk_style.set_background_paint(&background);
    }

    sk_style.set_font_size(style.font_size);
    if !style.font_families.is_empty() {
        // The names instancing found the faces by, where it ran.
        let names = match &instance.names {
            Some(names) => names.clone(),
            None => collection_names(&style.font_families),
        };
        sk_style.set_font_families(&names);
    }
    // Pinned to the weight the instance has. Skia fakes a bold wherever the
    // weight asked for is 200 or more above the face's, and an instance is
    // the weight it was instanced at, with nothing missing to fake: Chrome
    // paints weight 700 with `"wght" 300` as weight 300, and weight 900 on a
    // face whose axis stops at 700 as 700. `Typesetter` pins canvas text the
    // same way.
    let mut font_style = font_style_for(style);
    if let Some(weight) = instance.weight {
        font_style = FontStyle::new(
            Weight::from(weight),
            font_style.width(),
            font_style.slant(),
        );
    }
    sk_style.set_font_style(font_style);
    if !instance.coordinates.is_empty() {
        sk_style.set_font_arguments(
            &FontArguments::new().set_variation_design_position(
                VariationPosition {
                    coordinates: &instance.coordinates,
                },
            ),
        );
    }
    if (style.line_height_multiplier - 1.0).abs() > f32::EPSILON {
        sk_style.set_height(style.line_height_multiplier);
        sk_style.set_height_override(true);
    }

    if style.letter_spacing != 0.0 {
        sk_style.set_letter_spacing(style.letter_spacing);
    }
    if style.word_spacing != 0.0 {
        sk_style.set_word_spacing(style.word_spacing);
    }
    if style.baseline_shift != 0.0 {
        sk_style.set_baseline_shift(style.baseline_shift);
    }

    for feature in &style.font_features {
        sk_style.add_font_feature(&feature.name, feature.value);
    }

    if let Some(locale) = &style.locale {
        sk_style.set_locale(locale);
    }

    if style.half_leading {
        sk_style.set_half_leading(true);
    }

    let sk_decoration = style.decoration.to_skia();
    if sk_decoration != SkTextDecoration::NO_DECORATION {
        sk_style.set_decoration_type(sk_decoration);
        sk_style.set_decoration_style(style.decoration_style.to_skia());
        // `set_decoration_color` takes a Skia `Color` (u32 ARGB, sRGB-encoded
        // by Skia convention), so we gamma-encode our linear value before
        // quantizing to u8 -- otherwise Skia's implicit decode pass darkens
        // the decoration.
        //
        // `None` means "follow the text colour", which the field has always
        // documented and which is what CSS `currentColor` does. It was
        // implemented by setting nothing, leaving whatever Skia's own
        // default is -- white, as an underline drawn under `#facc15` text
        // showed the moment this was compared against the Node binding,
        // which resolves the fallback itself. Setting it explicitly is what
        // makes the documented behaviour true.
        let decoration_color = style.decoration_color.unwrap_or(style.color);
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
        // Re-check: grep -n "fn set_decoration_color\|fn new(color" in
        // skia-safe's src/modules/paragraph/text_style.rs and text_shadow.rs
        //
        // SkParagraph takes a decoration's colour, and a text shadow's, as
        // an eight-bit sRGB `SkColor` and nothing wider, so a wide colour is
        // gamut-mapped into sRGB on the way in.
        sk_style
            .set_decoration_color(rgba_linear_to_skia_color(decoration_color));
        if (style.decoration_thickness - 1.0).abs() > f32::EPSILON {
            sk_style.set_decoration_thickness_multiplier(
                style.decoration_thickness,
            );
        }
    }

    for shadow in &style.shadows {
        // `TextShadow::new` takes a Skia `Color` (u32 ARGB, sRGB-encoded),
        // reached the way `decoration_color` is, above.
        sk_style.add_shadow(SkTextShadow::new(
            rgba_linear_to_skia_color(shadow.color),
            SkPoint::new(shadow.offset_x, shadow.offset_y),
            shadow.blur_sigma as f64,
        ));
    }

    sk_style
}

/// The names an engine's font collection is asked for `families` by.
///
/// An engine's collection holds what its library registered and nothing
/// standing in for the generics, so a generic is asked for by its keyword
/// and answered by the platform -- fontconfig has faces for `serif`,
/// `sans-serif` and `monospace`, CoreText none. A name that spells a generic
/// keyword is a family name, and is passed on only where a family of that
/// name exists, so that fontconfig's answer for the keyword is not taken
/// for it -- see `FontLibrary::collection_names`, the canvas's counterpart.
///
/// A registered family named in another case is asked for in the spelling
/// it was registered under, as the canvas's counterpart does: the provider
/// matches a name exactly, and CSS Fonts 4 matches family names without
/// regard to case.
///
/// Worked out once per family list while the library's fonts stand -- see
/// `FontLibrary::engine_names`.
fn collection_names(families: &[FontFamily]) -> Rc<[String]> {
    #[cfg(test)]
    NAME_LOOKUPS.set(NAME_LOOKUPS.get() + 1);
    SharedFontRegistry::with_shared(|library| library.engine_names(families))
}

/// The families a strut should be measured in.
///
/// A strut whose family cannot be resolved does not fall back the way a text
/// run does -- Skia hands back a line box of negative infinity, and
/// `Paragraph::height` passes it straight to the caller. Measured on a
/// machine carrying only DejaVu: a strut naming nothing, `sans-serif`, or a
/// missing family all laid out at `-inf`, while the same strut naming
/// `DejaVu Sans` laid out at 64.
///
/// So: the strut's own families if it named any, else the text style's --
/// which is what CSS means by a strut, the line box of the element's own font
/// -- and failing both, whatever face the collection falls back to for the
/// text itself.
fn strut_families(
    style: &TextStyle,
    collection: &mut FontCollection,
) -> Vec<String> {
    let Some(strut) = &style.strut else {
        return Vec::new();
    };
    if !strut.font_families.is_empty() {
        return collection_names(&strut.font_families).to_vec();
    }
    if !style.font_families.is_empty() {
        return collection_names(&style.font_families).to_vec();
    }
    collection
        .default_fallback()
        .map(|typeface| vec![typeface.family_name()])
        .unwrap_or_default()
}

fn build_paragraph_style(
    style: &TextStyle,
    base_sk_style: &SkTextStyle,
    strut_families: &[String],
) -> SkParagraphStyle {
    let mut paragraph_style = SkParagraphStyle::new();
    paragraph_style.set_text_align(style.align.to_skia());
    paragraph_style.set_text_direction(style.direction.to_skia());
    paragraph_style.set_text_style(base_sk_style);

    if style.text_height_behavior != TextHeightBehavior::All {
        paragraph_style
            .set_text_height_behavior(style.text_height_behavior.to_skia());
    }

    if let Some(max_lines) = style.max_lines {
        paragraph_style.set_max_lines(max_lines);
    }

    if let Some(ellipsis) = &style.ellipsis {
        paragraph_style.set_ellipsis(ellipsis);
    }

    if let Some(strut) = &style.strut {
        let mut sk_strut = SkStrutStyle::new();
        sk_strut.set_strut_enabled(true);
        if !strut_families.is_empty() {
            let families: Vec<&str> =
                strut_families.iter().map(String::as_str).collect();
            sk_strut.set_font_families(&families);
        }
        if let Some(size) = strut.font_size {
            sk_strut.set_font_size(size);
        }
        if let Some(height) = strut.height {
            sk_strut.set_height(height);
            sk_strut.set_height_override(true);
        }
        if let Some(leading) = strut.leading {
            sk_strut.set_leading(leading);
        }
        sk_strut.set_force_strut_height(strut.force_height);
        sk_strut.set_half_leading(strut.half_leading);
        paragraph_style.set_strut_style(sk_strut);
    }

    paragraph_style
}

#[cfg(test)]
mod tests {
    use super::{
        FaceQuery, SkPaint, SkTextStyle, TextEngine, TextSlant, TextStyle,
        instance_for,
    };
    use crate::{
        context2d::FontStretch,
        font::{
            FontAxisTag, FontLibrary, FontVariation, INSTANCED_POSITIONS,
            variation_axes,
        },
    };
    use skia_safe::{
        Data, FontArguments, FontMgr, FourByteTag,
        font_arguments::{VariationPosition, variation_position::Coordinate},
    };

    /// Whether skia-safe still turns an empty family list into a slice
    /// built from a null pointer, which a debug build aborts on: the defect
    /// the npm binding's `font_families` and this module's `is_empty` tests
    /// before `set_font_families` keep away from. Run in a child process,
    /// since the abort does not unwind. Fails once skia-safe takes an empty
    /// list, which is the cue to drop the guard.
    #[cfg(debug_assertions)]
    #[test]
    fn an_empty_family_list_is_a_null_slice_in_skia_safe() {
        /// Set for the child, which makes the call the parent watches.
        const CHILD: &str = "MEO_SKIA_CANVAS_EMPTY_FAMILY_CHILD";
        if std::env::var_os(CHILD).is_some() {
            SkTextStyle::new().set_font_families(&[] as &[&str]);
            return;
        }
        let status = std::process::Command::new(
            std::env::current_exe().expect("the test binary"),
        )
        .args([
            "--exact",
            "text::tests::an_empty_family_list_is_a_null_slice_in_skia_safe",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the test binary runs");
        assert!(
            !status.success(),
            "skia-safe takes an empty family list now: drop `font_families`' \
             guard in src/node/paragraph.rs and its UPSTREAM marker"
        );
    }

    #[test]
    fn an_engine_drops_its_caches_past_the_bound_on_instanced_positions() {
        // Each size of a face with an `opsz` axis is a position of its own,
        // and Skia keeps the typeface it instances for every one. Its
        // paragraph cache is dropped with them, which is what makes the drop
        // visible from here: one paragraph per size until the bound, and
        // only the newest after it.
        const FAMILY: &str = "AmstelvarEngineBound";
        let fonts = FontLibrary::new();
        fonts
            .register_font_from_path(
                FAMILY,
                "tests/assets/fonts/AmstelvarAlpha-VF.ttf",
            )
            .expect("bundled Amstelvar registers");
        let engine = TextEngine::new(&fonts);
        let lay_out = |step: usize| {
            let style = TextStyle {
                font_families: vec![FAMILY.into()],
                font_size: 10.0 + step as f32 * 0.25,
                ..TextStyle::default()
            };
            engine.layout_text("Hamburgefonstiv", &style, f32::MAX);
        };
        let cached = || engine.collection.clone().paragraph_cache_mut().count();

        (0..INSTANCED_POSITIONS).for_each(lay_out);
        assert!(
            cached() > 1,
            "a paragraph is cached per size until the bound, {} here",
            cached()
        );
        // A size laid out before is a position already recorded, and drops
        // nothing even with the record full.
        let full = cached();
        lay_out(0);
        assert_eq!(cached(), full, "a repeated size drops nothing");

        lay_out(INSTANCED_POSITIONS);
        assert!(
            cached() <= 1,
            "one past the bound drops the caches, leaving {}",
            cached()
        );
        assert_eq!(engine.positions.lock().map(|p| p.len()).ok(), Some(1));

        // A size from before the drop is an instance the engine remembers,
        // and a position recorded again all the same: the drop forgot it,
        // and Skia makes its typeface again on the next layout.
        lay_out(0);
        assert_eq!(
            engine.positions.lock().map(|p| p.len()).ok(),
            Some(2),
            "a remembered instance is recorded as a position again"
        );
    }

    /// The engine over the bundled Oswald, Raleway and Amstelvar, each its
    /// own family.
    fn variable_engine() -> TextEngine {
        let fonts = FontLibrary::new();
        for (family, file) in [
            (
                "OswaldDefault",
                "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
            ),
            (
                "RalewayDefault",
                "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
            ),
            (
                "AmstelvarDefault",
                "tests/assets/fonts/AmstelvarAlpha-VF.ttf",
            ),
        ] {
            fonts
                .register_font_from_path(family, file)
                .expect("a bundled variable font registers");
        }
        TextEngine::new(&fonts)
    }

    /// The coordinates a style is instanced at on `engine`.
    fn instanced(
        engine: &TextEngine,
        families: &[&str],
        weight: i32,
        size: f32,
    ) -> Vec<Coordinate> {
        let style = TextStyle {
            font_families: families
                .iter()
                .map(|family| (*family).into())
                .collect(),
            font_weight: weight,
            font_size: size,
            ..TextStyle::default()
        };
        instance_for(
            &mut engine.collection.clone(),
            &engine.positions,
            &engine.faces,
            &FaceQuery::of(&style, &[]),
        )
        .coordinates
    }

    /// `text` drawn in `face`, at `size`, instanced at `coordinates` where
    /// there are any: its pixels and its advance.
    fn drawn_in(
        face: &skia_safe::Typeface,
        coordinates: &[Coordinate],
        size: f32,
    ) -> (Vec<u8>, f32) {
        let face = match coordinates.is_empty() {
            true => face.clone(),
            false => face
                .clone_with_arguments(
                    &FontArguments::new().set_variation_design_position(
                        VariationPosition { coordinates },
                    ),
                )
                .expect("an instance"),
        };
        let font = skia_safe::Font::from_typeface(face, size);
        let text = "Hamburgefonstiv quick 0123";
        let mut surface = skia_safe::surfaces::raster_n32_premul((900, 120))
            .expect("a surface");
        surface.canvas().clear(skia_safe::Color::WHITE);
        let mut paint = SkPaint::default();
        paint.set_anti_alias(true);
        surface.canvas().draw_str(text, (4, 90), &font, &paint);
        let info = skia_safe::ImageInfo::new_n32_premul((900, 120), None);
        let mut pixels = vec![0u8; 900 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 900 * 4, (0, 0)));
        (pixels, font.measure_str(text, None).0)
    }

    /// A style at its faces' own position makes no list of coordinates; one
    /// off it makes one, for the instance.
    #[test]
    fn a_style_at_its_faces_own_position_makes_no_coordinates() {
        let engine = variable_engine();
        let made = |weight| {
            let before = super::COORDINATE_LISTS.get();
            let instance = instanced(&engine, &["OswaldDefault"], weight, 24.0);
            (super::COORDINATE_LISTS.get() - before, instance.is_empty())
        };
        assert_eq!(made(400), (0, true), "at Oswald's own 400");
        assert_eq!(made(401), (1, false), "at 401");
    }

    /// How many times pushing `style` onto a crate builder takes anything
    /// off it to be drawn by the passes.
    fn takes_pushing(style: &TextStyle) -> usize {
        let engine = variable_engine();
        let mut builder = engine.paragraph_builder(&amstelvar());
        let before = crate::node::oblique::TAKES.get();
        builder.push_style(style);
        crate::node::oblique::TAKES.get() - before
    }

    /// A style with no decoration, shadow or background is pushed without
    /// looking for any; one with any of the three is looked over.
    #[test]
    fn only_a_style_with_something_to_take_is_looked_over() {
        assert_eq!(takes_pushing(&amstelvar()), 0, "plain");
        let decorated = TextStyle {
            decoration: super::TextDecoration::underline(),
            ..amstelvar()
        };
        assert_eq!(takes_pushing(&decorated), 1, "decorated");
        let shadowed = TextStyle {
            shadows: vec![super::TextShadow {
                color: crate::color::RgbaLinear::from_srgb8(0, 0, 0, 1.0),
                offset_x: 2.0,
                offset_y: 2.0,
                blur_sigma: 0.0,
            }],
            ..amstelvar()
        };
        assert_eq!(takes_pushing(&shadowed), 1, "shadowed");
        let backed = TextStyle {
            background_color: Some(crate::color::RgbaLinear::from_srgb8(
                0, 0, 255, 1.0,
            )),
            ..amstelvar()
        };
        assert_eq!(takes_pushing(&backed), 1, "backed");
    }

    /// The style each of the reuse pins starts from: Amstelvar, whose `wght`
    /// and `opsz` axes make its instance follow both weight and size.
    fn amstelvar() -> TextStyle {
        TextStyle {
            font_families: vec!["AmstelvarDefault".into()],
            font_weight: 400,
            font_size: 24.0,
            ..TextStyle::default()
        }
    }

    /// How many times `instance_for` works a style out over `first` and
    /// then `second` on one engine, and `second`'s coordinates, which must
    /// be what a fresh engine makes of it whether or not it was reused.
    fn worked_out(
        first: (&TextStyle, &[FontVariation]),
        second: (&TextStyle, &[FontVariation]),
    ) -> usize {
        let instance = |engine: &TextEngine, (style, variations)| {
            instance_for(
                &mut engine.collection.clone(),
                &engine.positions,
                &engine.faces,
                &FaceQuery::of(style, variations),
            )
            .coordinates
        };
        let engine = variable_engine();
        let before = super::INSTANCES.get();
        instance(&engine, first);
        let reused = instance(&engine, second);
        let count = super::INSTANCES.get() - before;
        assert_eq!(
            reused,
            instance(&variable_engine(), second),
            "the second style's instance is the one a fresh engine makes"
        );
        count
    }

    /// A style instanced before is answered from the engine's memory without
    /// asking the font library for its family list's names again.
    #[test]
    fn a_remembered_instance_asks_for_no_names() {
        let engine = variable_engine();
        let style = amstelvar();
        let query = FaceQuery::of(&style, &[]);
        let instance = || {
            instance_for(
                &mut engine.collection.clone(),
                &engine.positions,
                &engine.faces,
                &query,
            )
            .coordinates
        };
        let first = instance();
        let (looked_up, used) =
            (super::NAME_LOOKUPS.get(), crate::memory::FONT_USES.get());
        assert_eq!(instance(), first);
        assert_eq!(super::NAME_LOOKUPS.get() - looked_up, 0);
        assert_eq!(
            crate::memory::FONT_USES.get() - used,
            1,
            "a use of the fonts noted all the same, for the idle release"
        );
    }

    /// A style equal to one instanced before in everything `instance_for`
    /// reads is not worked out again.
    #[test]
    fn a_style_instanced_before_is_reused() {
        let style = amstelvar();
        assert_eq!(worked_out((&style, &[]), (&style, &[])), 1);
        let coloured = TextStyle {
            color: crate::color::RgbaLinear::from_srgb8(255, 0, 0, 1.0),
            ..amstelvar()
        };
        assert_eq!(
            worked_out((&style, &[]), (&coloured, &[])),
            1,
            "a colour is not a part of the instance"
        );
    }

    /// An engine remembers at most `MATCHES_KEPT` instances: a weight that
    /// animates is a new query every frame.
    #[test]
    fn an_engine_remembers_a_bounded_number_of_instances() {
        let engine = variable_engine();
        for weight in 100..100 + super::MATCHES_KEPT as i32 + 1 {
            let style = TextStyle {
                font_weight: weight,
                ..amstelvar()
            };
            instance_for(
                &mut engine.collection.clone(),
                &engine.positions,
                &engine.faces,
                &FaceQuery::of(&style, &[]),
            );
        }
        let kept = engine
            .faces
            .lock()
            .map(|record| record.instances.len())
            .expect("an unpoisoned record");
        assert!(kept <= super::MATCHES_KEPT, "{kept} kept past the bound");
    }

    /// What an engine remembers of its faces is dropped when the shared
    /// library's fonts change, since a change can spell a family list
    /// differently: a style met before is worked out again, and in the
    /// spelling the fonts give it now. Here a family asked for in lower case
    /// is spelled as asked until a font is registered under it in another
    /// case.
    #[test]
    fn an_engine_forgets_its_instances_when_the_fonts_change() {
        let engine = variable_engine();
        let style = TextStyle {
            font_families: vec!["amstelvarrespelled".into()],
            ..amstelvar()
        };
        let names = || {
            let before = super::INSTANCES.get();
            let names = instance_for(
                &mut engine.collection.clone(),
                &engine.positions,
                &engine.faces,
                &FaceQuery::of(&style, &[]),
            )
            .names
            .map(|names| names.to_vec());
            (names, super::INSTANCES.get() - before)
        };
        let asked = Some(vec!["amstelvarrespelled".to_string()]);
        assert_eq!(names(), (asked.clone(), 1));
        assert_eq!(names(), (asked, 0), "remembered while the fonts stand");
        FontLibrary::new()
            .register_font_from_path(
                "AmstelvarRespelled",
                "tests/assets/fonts/AmstelvarAlpha-VF.ttf",
            )
            .expect("a bundled variable font registers");
        assert_eq!(
            names(),
            (Some(vec!["AmstelvarRespelled".to_string()]), 1),
            "worked out again, in the spelling registered"
        );
        let kept = engine.faces.lock().map(|record| record.instances.len());
        assert_eq!(kept.ok(), Some(1), "the instance from before is gone");
    }

    /// A family of static faces does no instancing work: no coordinates,
    /// no pinned weight, no position recorded, and its faces' axes asked
    /// of the platform once, on the style's first use and not again.
    #[test]
    fn a_static_face_does_no_instancing_work() {
        let fonts = FontLibrary::new();
        fonts
            .register_font_from_path(
                "MontserratStatic",
                "tests/assets/fonts/montserrat-latin/montserrat-v30-latin-regular.woff2",
            )
            .expect("a bundled static font registers");
        let engine = TextEngine::new(&fonts);
        let style = TextStyle {
            font_families: vec!["MontserratStatic".into()],
            ..TextStyle::default()
        };
        let asked = || {
            engine
                .faces
                .lock()
                .map(|record| record.axes.asked)
                .expect("an unpoisoned record")
        };
        let lists = super::COORDINATE_LISTS.get();
        for use_ in 0..2 {
            let instance = instance_for(
                &mut engine.collection.clone(),
                &engine.positions,
                &engine.faces,
                &FaceQuery::of(&style, &[]),
            );
            assert!(instance.coordinates.is_empty(), "use {use_}");
            assert_eq!(instance.weight, None, "use {use_}");
            assert_eq!(asked(), 1, "axes asked once, use {use_}");
        }
        assert_eq!(super::COORDINATE_LISTS.get() - lists, 0);
        assert_eq!(engine.positions.lock().map(|p| p.len()).ok(), Some(0));
    }

    /// The reuse pins, one for each thing `instance_for` reads: a style
    /// differing from the one before in that alone is worked out apart.
    #[test]
    fn a_style_in_other_families_is_instanced_apart() {
        let other = TextStyle {
            font_families: vec!["OswaldDefault".into()],
            ..amstelvar()
        };
        assert_eq!(worked_out((&amstelvar(), &[]), (&other, &[])), 2);
    }

    #[test]
    fn a_style_of_another_weight_is_instanced_apart() {
        let other = TextStyle {
            font_weight: 700,
            ..amstelvar()
        };
        assert_eq!(worked_out((&amstelvar(), &[]), (&other, &[])), 2);
    }

    #[test]
    fn a_style_of_another_width_is_instanced_apart() {
        let other = TextStyle {
            stretch: FontStretch::Condensed,
            ..amstelvar()
        };
        assert_eq!(worked_out((&amstelvar(), &[]), (&other, &[])), 2);
    }

    #[test]
    fn a_style_of_another_slant_is_instanced_apart() {
        let other = TextStyle {
            slant: TextSlant::Italic,
            ..amstelvar()
        };
        assert_eq!(worked_out((&amstelvar(), &[]), (&other, &[])), 2);
    }

    #[test]
    fn a_style_of_another_size_is_instanced_apart() {
        let other = TextStyle {
            font_size: 48.0,
            ..amstelvar()
        };
        assert_eq!(worked_out((&amstelvar(), &[]), (&other, &[])), 2);
    }

    #[test]
    fn a_style_at_other_variations_is_instanced_apart() {
        let wider = [FontVariation::new(FontAxisTag::WDTH, 60.0)];
        let style = amstelvar();
        assert_eq!(worked_out((&style, &[]), (&style, &wider)), 2);
    }

    /// A style at its faces' own position is not instanced, and drawn so it
    /// is the instance glyph for glyph and advance for advance; one off it
    /// by a single unit of weight is instanced, as is one whose fallback
    /// face's defaults differ, and one in a face with an optical size, at
    /// every size, its default among them.
    #[test]
    fn a_style_at_its_faces_own_position_is_not_instanced() {
        let engine = variable_engine();
        let wght = FourByteTag::from(u32::from_be_bytes(
            *FontAxisTag::WGHT.as_bytes(),
        ));
        let face = |file: &str| {
            FontMgr::new()
                .new_from_data(
                    Data::new_copy(&std::fs::read(file).expect("a font")),
                    None,
                )
                .expect("a face")
        };
        // Oswald stands at 400 and Raleway at 100 without arguments.
        for (family, file, weight) in [
            (
                "OswaldDefault",
                "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
                400,
            ),
            (
                "RalewayDefault",
                "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
                100,
            ),
        ] {
            assert!(
                instanced(&engine, &[family], weight, 24.0).is_empty(),
                "{family} at {weight}"
            );
            let at = [Coordinate {
                axis: wght,
                value: weight as f32,
            }];
            assert_eq!(
                drawn_in(&face(file), &[], 24.0),
                drawn_in(&face(file), &at, 24.0),
                "{family}: the face is its instance at {weight}"
            );
            assert!(
                !instanced(&engine, &[family], weight + 1, 24.0).is_empty(),
                "{family} at {}",
                weight + 1
            );
        }
        // Oswald stands at 400; Raleway behind it does not.
        assert!(
            !instanced(
                &engine,
                &["OswaldDefault", "RalewayDefault"],
                400,
                24.0
            )
            .is_empty(),
            "a fallback face away from its own position"
        );
        // Amstelvar's optical size follows the font size, so it is
        // instanced at every size -- even at its `opsz` default, where every
        // coordinate is the axis's own.
        let amstelvar = face("tests/assets/fonts/AmstelvarAlpha-VF.ttf");
        let opsz = FourByteTag::from(u32::from_be_bytes(
            *FontAxisTag::OPSZ.as_bytes(),
        ));
        let default = variation_axes(&amstelvar)
            .and_then(|axes| axes.iter().find(|a| a.tag == opsz).map(|a| a.def))
            .expect("an optical size axis");
        for size in [default, 24.0] {
            let coordinates =
                instanced(&engine, &["AmstelvarDefault"], 400, size);
            assert!(!coordinates.is_empty(), "an optical size at {size}px");
            let apart = drawn_in(&amstelvar, &[], size)
                != drawn_in(&amstelvar, &coordinates, size);
            // Uninstanced, FreeType draws the face at its `opsz` default
            // whatever the size, so it is the instance there and only
            // there; CoreText draws it at an optical size it picks for
            // itself, which is the instance at neither. DirectWrite's has
            // not been measured.
            if cfg!(target_os = "macos") {
                assert!(apart, "CoreText's own optical size at {size}px");
            } else if !cfg!(target_os = "windows") {
                assert_eq!(
                    apart,
                    size != default,
                    "FreeType's default optical size, asked at {size}px"
                );
            }
        }
    }
}
