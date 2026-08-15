use std::ops::Range;

use skia_safe::{
    FontArguments, FontMgr, FontStyle, Paint as SkPaint, Point as SkPoint,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::{Slant, Weight},
    textlayout::{
        Affinity as SkAffinity, FontCollection, Paragraph as SkParagraph,
        ParagraphBuilder as SkParagraphBuilder,
        ParagraphStyle as SkParagraphStyle,
        PlaceholderAlignment as SkPlaceholderAlignment, PlaceholderStyle,
        RectHeightStyle as SkRectHeightStyle,
        RectWidthStyle as SkRectWidthStyle, StrutStyle as SkStrutStyle,
        TextAlign as SkTextAlign, TextBaseline as SkTextBaseline,
        TextBox as SkTextBox, TextDecoration as SkTextDecoration,
        TextDecorationStyle as SkTextDecorationStyle,
        TextDirection as SkTextDirection,
        TextHeightBehavior as SkTextHeightBehavior, TextShadow as SkTextShadow,
        TextStyle as SkTextStyle, TypefaceFontProvider,
    },
};

use crate::{
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_skia_color,
        rgba_linear_to_unpremul_color4f,
    },
    context2d::{FontStretch, TextDirection},
    font::{FontLibrary, FontVariation},
    geometry::Rect,
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

    pub(crate) fn to_skia(self) -> SkTextDirection {
        match self {
            Self::LeftToRight => SkTextDirection::LTR,
            Self::RightToLeft => SkTextDirection::RTL,
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
    pub font_bounding_box_ascent: f32,
    /// Distance below the baseline the font can reach, string-independent.
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
    /// A slanted upright, used where no true italic exists.
    Oblique,
}

impl TextSlant {
    fn to_skia(self) -> Slant {
        match self {
            Self::Upright => Slant::Upright,
            Self::Italic => Slant::Italic,
            Self::Oblique => Slant::Oblique,
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
    pub font_families: Vec<String>,
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
    fn to_skia(self) -> SkTextHeightBehavior {
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
    pub font_families: Vec<String>,
    /// Em size in pixels.
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
    /// Variable-font axis positions.
    ///
    /// When non-empty, the paragraph engine instantiates a variable-typeface
    /// clone at the requested axes before layout, matching CanvasKit's
    /// `fontVariations`. `font_weight` continues to drive `SkFontStyle` bucket
    /// matching; add `FontAxisTag::WGHT` here to also vary the `wght` design
    /// axis (without it, the manager synthesizes one from `font_weight`).
    pub font_variations: Vec<FontVariation>,
    /// OpenType features applied to the run (small caps, ligatures,
    /// oldstyle/tabular figures, stylistic sets, ...).
    ///
    /// Mirrors CanvasKit's `TextStyle.fontFeatures`. Applied directly on the
    /// layout `TextStyle`; independent of `font_variations`.
    pub font_features: Vec<FontFeature>,
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

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_families: Vec::new(),
            font_size: 16.0,
            font_weight: 400,
            slant: TextSlant::Upright,
            stretch: FontStretch::Normal,
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            foreground_color: None,
            background_color: None,
            align: TextAlign::Left,
            direction: TextDirection::LeftToRight,
            line_height_multiplier: 1.0,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            decoration: TextDecoration::default(),
            decoration_style: TextDecorationStyle::Solid,
            decoration_color: None,
            decoration_thickness: 1.0,
            shadows: Vec::new(),
            baseline_shift: 0.0,
            font_variations: Vec::new(),
            font_features: Vec::new(),
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
    fn to_skia(self) -> SkTextDecorationStyle {
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
    fn to_skia(self) -> SkPlaceholderAlignment {
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
    fn to_skia(self) -> SkTextBaseline {
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
    pub(crate) collection: FontCollection,
    /// Asset-side `TypefaceFontProvider` snapshot kept so per-call font
    /// collections (used when a `TextStyle` carries `font_variations`) can re-
    /// attach it.
    ///
    /// `None` for `with_system_fonts()` engines.
    asset_provider: Option<TypefaceFontProvider>,
    /// Registered family aliases on the source `FontLibrary`, captured at
    /// construction time.
    ///
    /// Used to remap instantiated variable typefaces onto the alias the caller
    /// registered them under (instead of the typeface's intrinsic family
    /// name).
    registered_families: Vec<String>,
}

impl TextEngine {
    /// Builds using `font_manager`'s registered typefaces plus system
    /// fallbacks for unmatched family names.
    pub fn new(font_manager: &FontLibrary) -> Self {
        let asset_provider = font_manager.snapshot_provider();
        let registered_families = font_manager.registered_family_names();
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
        collection
            .set_default_font_manager(system_fonts, default_family.as_deref());
        collection.set_asset_font_manager(Some(asset_provider.clone().into()));
        // Resolve glyphs missing from the matched family against the
        // system fonts instead of rendering tofu -- matches CanvasKit's
        // `FontCollection.enableFontFallback`.
        collection.enable_font_fallback();
        Self {
            collection,
            asset_provider: Some(asset_provider),
            registered_families,
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
        collection
            .set_default_font_manager(system_fonts, default_family.as_deref());
        collection.enable_font_fallback();
        Self {
            collection,
            asset_provider: None,
            registered_families: Vec::new(),
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
        let collection = self.collection_for(style);
        let strut = strut_families(style, &mut collection.clone());
        let sk_text_style = build_text_style(style);
        let paragraph_style =
            build_paragraph_style(style, &sk_text_style, &strut);

        let mut builder = SkParagraphBuilder::new(&paragraph_style, collection);
        builder.add_text(text);
        let mut paragraph = builder.build();
        paragraph.layout(max_width);
        Paragraph {
            paragraph,
            max_width,
        }
    }

    /// Lays out a rich-text paragraph.
    ///
    /// The paragraph-level state (`align`, `line_height_multiplier`) comes from
    /// `base_style`; each `RichTextSpan` overlays its own per-span style for
    /// the span's text (font, color, decoration, baseline shift, etc.).
    ///
    /// `font_variations` are read from `base_style` -- the builder's
    /// font collection is fixed at construction time, so per-span axis
    /// changes are not supported. Set the variations on the base style
    /// for the paragraph as a whole.
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
    /// Paragraph-level state -- alignment, line-height multiplier, strut,
    /// font variations -- is fixed here from `base_style` and cannot be
    /// changed once the builder exists, because Skia settles the font
    /// collection and paragraph style before the first character is added.
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
        let collection = self.collection_for(base_style);
        let strut = strut_families(base_style, &mut collection.clone());
        let base_sk_style = build_text_style(base_style);
        let paragraph_style =
            build_paragraph_style(base_style, &base_sk_style, &strut);
        ParagraphBuilder {
            inner: SkParagraphBuilder::new(
                &paragraph_style,
                collection.clone(),
            ),
            _collection: collection,
        }
    }

    /// Builds the `FontCollection` for laying out `style`.
    ///
    /// Returns the engine's base collection when `style.font_variations` is
    /// empty; otherwise seeds a fresh collection with a dynamic
    /// `TypefaceFontProvider` carrying variable-typeface clones instantiated at
    /// the requested axes for the matched families.
    fn collection_for(&self, style: &TextStyle) -> FontCollection {
        if style.font_variations.is_empty() || style.font_families.is_empty() {
            return self.collection.clone();
        }
        let families: Vec<&str> =
            style.font_families.iter().map(String::as_str).collect();
        let sk_font_style = FontStyle::new(
            Weight::from(style.font_weight),
            style.stretch.to_skia(),
            style.slant.to_skia(),
        );
        // `find_typefaces` requires `&mut self` on `FontCollection`.
        // The collection is ref-counted internally (skia_safe), so the
        // clone shares storage with `self.collection` without copying
        // typefaces -- the temporary mutation stays on this method's
        // owned clone.
        let mut find_collection = self.collection.clone();
        let matches = find_collection.find_typefaces(&families, sk_font_style);
        if !matches
            .iter()
            .any(|tf| tf.variation_design_parameters().is_some())
        {
            return self.collection.clone();
        }

        let mut dynamic = TypefaceFontProvider::new();
        // `FourByteTag` is a `u32` packed in big-endian OpenType-tag
        // order; compare via the `u32` form so the match is a single
        // integer op.
        let explicit_tags: Vec<u32> = style
            .font_variations
            .iter()
            .map(|v| u32::from_be_bytes(*v.axis.as_bytes()))
            .collect();

        for face in matches {
            let Some(params) = face.variation_design_parameters() else {
                continue;
            };
            let mut coords: Vec<Coordinate> = Vec::new();

            for v in &style.font_variations {
                let axis_u32 = u32::from_be_bytes(*v.axis.as_bytes());
                if let Some(param) = params.iter().find(|p| *p.tag == axis_u32)
                {
                    coords.push(Coordinate {
                        axis: param.tag,
                        value: v.value.clamp(param.min, param.max),
                    });
                }
            }

            // Synthesize a `wght` axis from `font_weight` when the
            // caller did not pin one explicitly, so a `TextStyle` that
            // only sets `font_weight = 350` still drives variable
            // typefaces. Skia's `Weight::from(i32)` returns an i32 1
            // higher than the CSS weight value internally; subtract
            // `INVISIBLE` (=1) to get the design-space float.
            let wght_u32 = u32::from_be_bytes(*b"wght");
            if !explicit_tags.contains(&wght_u32)
                && let Some(param) = params.iter().find(|p| *p.tag == wght_u32)
            {
                let weight_f = (*sk_font_style.weight() - *Weight::INVISIBLE)
                    .max(0) as f32;
                coords.push(Coordinate {
                    axis: param.tag,
                    value: weight_f.clamp(param.min, param.max),
                });
            }

            if coords.is_empty() {
                continue;
            }
            let v_pos = VariationPosition {
                coordinates: &coords,
            };
            let args =
                FontArguments::new().set_variation_design_position(v_pos);
            let Some(instance) = face.clone_with_arguments(&args) else {
                continue;
            };

            // Map the instantiated typeface back to the alias the
            // caller registered with, if any. The instance retains the
            // intrinsic `family_name()`, which may differ from the
            // registered alias.
            let intrinsic = face.family_name();
            let alias = self
                .registered_families
                .iter()
                .find(|f| f.as_str() == intrinsic.as_str())
                .map(String::as_str);
            dynamic.register_typeface(instance, alias);
        }

        let mut collection = FontCollection::new();
        collection.set_default_font_manager(FontMgr::new(), None);
        if let Some(provider) = &self.asset_provider {
            collection.set_asset_font_manager(Some(provider.clone().into()));
        }
        collection.set_dynamic_font_manager(Some(dynamic.into()));
        collection.enable_font_fallback();
        collection
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
    inner: SkParagraphBuilder,
    /// Skia's builder borrows the collection internally rather than owning a
    /// reference the Rust side can see, so this keeps it alive for as long
    /// as the builder can reach it.
    _collection: FontCollection,
}

impl ParagraphBuilder {
    /// Pushes `style` onto the stack. Text added from here on is drawn in it.
    pub fn push_style(&mut self, style: &TextStyle) -> &mut Self {
        self.inner.push_style(&build_text_style(style));
        self
    }

    /// Pops the top style, returning to the one beneath.
    pub fn pop(&mut self) -> &mut Self {
        self.inner.pop();
        self
    }

    /// Adds `text` in the style currently on top of the stack.
    pub fn add_text(&mut self, text: &str) -> &mut Self {
        self.inner.add_text(text);
        self
    }

    /// Reserves a box the text flows around.
    ///
    /// Nothing is drawn into it. [`Paragraph::rects_for_placeholders`]
    /// reports where each one landed, in the order they were added, which is
    /// what the caller draws against.
    pub fn add_placeholder(&mut self, placeholder: Placeholder) -> &mut Self {
        self.inner.add_placeholder(&placeholder.to_skia());
        self
    }

    /// Closes the paragraph and lays it out, wrapping at `max_width`.
    pub fn build(mut self, max_width: f32) -> Paragraph {
        let mut paragraph = self.inner.build();
        paragraph.layout(max_width);
        Paragraph {
            paragraph,
            max_width,
        }
    }
}

/// Result of `TextEngine::layout_text`.
///
/// Owns the laid-out paragraph; metrics queries are cheap and
/// `draw_text_layout` paints the same paragraph onto a canvas.
pub struct Paragraph {
    pub(crate) paragraph: SkParagraph,
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
        self.paragraph.layout(max_width);
        self.max_width = max_width;
    }

    /// The distance from the top of the layout to the alphabetic baseline
    /// of the first line.
    ///
    /// Where Latin letters sit. Add it to a draw's y coordinate to place
    /// text by its baseline rather than by its top edge, which is what the
    /// Canvas `textBaseline` of `"alphabetic"` means.
    pub fn alphabetic_baseline(&self) -> f32 {
        self.paragraph.alphabetic_baseline()
    }

    /// The distance from the top of the layout to the ideographic baseline
    /// of the first line.
    ///
    /// Lower than the alphabetic one, and where CJK glyphs sit.
    pub fn ideographic_baseline(&self) -> f32 {
        self.paragraph.ideographic_baseline()
    }

    /// The narrowest width the text could be laid out in without a word
    /// having to break.
    ///
    /// The longest single word, in effect. Laying out narrower than this
    /// overflows or breaks mid-word depending on the style.
    pub fn min_intrinsic_width(&self) -> f32 {
        self.paragraph.min_intrinsic_width()
    }

    /// The width the text would take with no wrapping at all.
    ///
    /// The whole paragraph on one line. Together with
    /// [`min_intrinsic_width`](Self::min_intrinsic_width) these bracket
    /// every width worth laying out at -- wider than this changes nothing,
    /// and narrower than that cannot be honoured.
    pub fn max_intrinsic_width(&self) -> f32 {
        self.paragraph.max_intrinsic_width()
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
        let found = self.paragraph.get_glyph_position_at_coordinate((x, y));
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
        self.paragraph.longest_line()
    }

    /// The `max_width` (wrapping budget) requested at layout time.
    pub fn max_width(&self) -> f32 {
        self.max_width
    }

    /// Total height of the laid-out paragraph after wrapping.
    pub fn height(&self) -> f32 {
        self.paragraph.height()
    }

    /// Number of laid-out lines (0 if the input was empty).
    pub fn line_count(&self) -> usize {
        self.paragraph.line_number()
    }

    /// Distance from the paragraph's top edge to the first line's baseline
    /// ascent.
    ///
    /// Useful for vertical alignment of text against a known baseline.
    pub fn first_line_ascent(&self) -> f32 {
        let metrics = self.paragraph.get_line_metrics();
        metrics.first().map(|m| m.ascent as f32).unwrap_or_default()
    }

    /// Per-line metrics for the laid-out paragraph. The vector is
    /// indexed by line number and ordered top-to-bottom.
    pub fn line_metrics(&self) -> Vec<LineMetrics> {
        self.paragraph
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
        self.paragraph.did_exceed_max_lines()
    }

    /// Bounding boxes of the inline placeholders added during layout, in
    /// paragraph-local coordinates and in insertion order.
    ///
    /// Mirrors CanvasKit's `Paragraph.getRectsForPlaceholders` -- the readback
    /// counterpart to placeholder insertion, for positioning inline
    /// icons/images.
    pub fn rects_for_placeholders(&self) -> Vec<TextBox> {
        self.paragraph
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
            .unresolved_codepoints()
            .into_iter()
            .map(|cp| cp as u32)
            .collect()
    }
}

fn build_text_style(style: &TextStyle) -> SkTextStyle {
    let mut sk_style = SkTextStyle::new();

    let mut paint = SkPaint::default();
    let cs = linear_srgb_color_space();
    // `foreground_color` wins where it is set, which is what makes it an
    // override rather than a second colour with no rule between them.
    let fill = style.foreground_color.unwrap_or(style.color);
    paint.set_color4f(rgba_linear_to_unpremul_color4f(fill), Some(&cs));
    paint.set_anti_alias(true);
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
        let families: Vec<&str> =
            style.font_families.iter().map(String::as_str).collect();
        sk_style.set_font_families(&families);
    }
    sk_style.set_font_style(FontStyle::new(
        Weight::from(style.font_weight),
        style.stretch.to_skia(),
        style.slant.to_skia(),
    ));
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
        sk_style
            .set_decoration_color(rgba_linear_to_skia_color(decoration_color));
        if (style.decoration_thickness - 1.0).abs() > f32::EPSILON {
            sk_style.set_decoration_thickness_multiplier(
                style.decoration_thickness,
            );
        }
    }

    for shadow in &style.shadows {
        // `TextShadow::new` takes a Skia `Color` (u32 ARGB, sRGB-encoded);
        // gamma-encode the linear input the same way as
        // `decoration_color`.
        sk_style.add_shadow(SkTextShadow::new(
            rgba_linear_to_skia_color(shadow.color),
            SkPoint::new(shadow.offset_x, shadow.offset_y),
            shadow.blur_sigma as f64,
        ));
    }

    sk_style
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
        return strut.font_families.clone();
    }
    if !style.font_families.is_empty() {
        return style.font_families.clone();
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
