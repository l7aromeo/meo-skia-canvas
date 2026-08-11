use std::ops::Range;

use skia_safe::{
    FontArguments, FontMgr, FontStyle, Paint as SkPaint, Point as SkPoint,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::{Slant, Weight, Width},
    textlayout::{
        FontCollection, Paragraph as SkParagraph,
        ParagraphBuilder as SkParagraphBuilder,
        ParagraphStyle as SkParagraphStyle, RectHeightStyle, RectWidthStyle,
        StrutStyle as SkStrutStyle, TextAlign as SkTextAlign,
        TextDecoration as SkTextDecoration,
        TextDecorationStyle as SkTextDecorationStyle,
        TextHeightBehavior as SkTextHeightBehavior, TextShadow as SkTextShadow,
        TextStyle as SkTextStyle, TypefaceFontProvider,
    },
};

use crate::{
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_skia_color,
        rgba_linear_to_unpremul_color4f,
    },
    font::{FontManager, FontVariation},
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
    /// The lower text layer is the exception: [`TextStyle`] and
    /// [`TextBoxOptions`] lay out a box rather than a canvas run, and both
    /// pin [`Left`](TextAlign::Left) as Skia's own layout does.
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    pub lines: usize,
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

/// Vertical placement of a laid-out block within its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerticalAlign {
    /// Aligns to the top edge.
    Top,
    /// Centres within the box height.
    Center,
    /// Aligns to the bottom edge.
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
    /// CSS numeric weight, `100` to `900`, where `400` is regular.
    pub font_weight: i32,
    /// Upright, italic, or oblique.
    pub slant: TextSlant,
    /// Glyph fill color.
    pub color: RgbaLinear,
    /// Horizontal alignment. Paragraph-level.
    pub align: TextAlign,
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
    /// [`TextLayout::did_exceed_max_lines`]. Mirrors CanvasKit's
    /// `ParagraphStyle.maxLines`.
    pub max_lines: Option<usize>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_families: Vec::new(),
            font_size: 16.0,
            font_weight: 400,
            slant: TextSlant::Upright,
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            align: TextAlign::Left,
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

/// Styling for the one-shot
/// [`DrawTarget::draw_text_box`](crate::recorder::DrawTarget::draw_text_box)
/// path.
///
/// A deliberately small subset of [`TextStyle`], for the common case of
/// dropping a single styled string into a rectangle.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBoxOptions {
    /// Glyph fill color.
    pub color: RgbaLinear,
    /// Family to use, or `None` for the system default.
    pub font_family: Option<String>,
    /// Em size in pixels.
    pub font_size: f32,
    /// CSS numeric weight, `100` to `900`.
    pub font_weight: i32,
    /// Horizontal alignment within the rectangle.
    pub horizontal_align: TextAlign,
    /// Vertical placement within the rectangle.
    pub vertical_align: VerticalAlign,
    /// Opacity multiplier, clamped to `0.0..=1.0`.
    pub opacity: f32,
}

impl Default for TextBoxOptions {
    fn default() -> Self {
        Self {
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            font_family: None,
            font_size: 16.0,
            font_weight: 400,
            horizontal_align: TextAlign::Left,
            vertical_align: VerticalAlign::Top,
            opacity: 1.0,
        }
    }
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
    /// Registered family aliases on the source `FontManager`, captured at
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
    pub fn new(font_manager: &FontManager) -> Self {
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
    /// `FontManager` is needed.
    pub fn with_system_fonts() -> Self {
        let mut collection = FontCollection::new();
        collection.set_default_font_manager(FontMgr::new(), None);
        collection.enable_font_fallback();
        Self {
            collection,
            asset_provider: None,
            registered_families: Vec::new(),
        }
    }

    /// Lays out `text` against `style`, wrapping at `max_width`.
    ///
    /// Returns a `TextLayout` that can be measured or drawn via
    /// `Canvas::draw_text_layout`.
    pub fn layout_text(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: f32,
    ) -> TextLayout {
        let collection = self.collection_for(style);
        let sk_text_style = build_text_style(style);
        let paragraph_style = build_paragraph_style(style, &sk_text_style);

        let mut builder = SkParagraphBuilder::new(&paragraph_style, collection);
        builder.add_text(text);
        let mut paragraph = builder.build();
        paragraph.layout(max_width);
        TextLayout {
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
    pub fn layout_rich_text(
        &self,
        spans: &[RichTextSpan],
        base_style: &TextStyle,
        max_width: f32,
    ) -> TextLayout {
        let collection = self.collection_for(base_style);
        let base_sk_style = build_text_style(base_style);
        let paragraph_style = build_paragraph_style(base_style, &base_sk_style);

        let mut builder = SkParagraphBuilder::new(&paragraph_style, collection);
        for span in spans {
            let span_sk_style = build_text_style(&span.style);
            builder.push_style(&span_sk_style);
            builder.add_text(&span.text);
            builder.pop();
        }
        let mut paragraph = builder.build();
        paragraph.layout(max_width);
        TextLayout {
            paragraph,
            max_width,
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
            Width::NORMAL,
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

/// Result of `TextEngine::layout_text`.
///
/// Owns the laid-out paragraph; metrics queries are cheap and
/// `draw_text_layout` paints the same paragraph onto a canvas.
pub struct TextLayout {
    pub(crate) paragraph: SkParagraph,
    max_width: f32,
}

impl TextLayout {
    /// Measured width of the longest laid-out line, after wrapping.
    ///
    /// The width the laid-out content actually occupies, not the wrapping
    /// budget. Use [`TextLayout::max_width`] to recover the layout budget the
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
    pub fn rects_for_range(&self, range: Range<usize>) -> Vec<Rect> {
        self.paragraph
            .get_rects_for_range(
                range,
                RectHeightStyle::Tight,
                RectWidthStyle::Tight,
            )
            .into_iter()
            .map(|tb| {
                let r = tb.rect;
                Rect {
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                }
            })
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
    pub fn rects_for_placeholders(&self) -> Vec<Rect> {
        self.paragraph
            .get_rects_for_placeholders()
            .into_iter()
            .map(|tb| {
                let r = tb.rect;
                Rect {
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                }
            })
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
    paint.set_color4f(rgba_linear_to_unpremul_color4f(style.color), Some(&cs));
    paint.set_anti_alias(true);
    sk_style.set_foreground_paint(&paint);

    sk_style.set_font_size(style.font_size);
    if !style.font_families.is_empty() {
        let families: Vec<&str> =
            style.font_families.iter().map(String::as_str).collect();
        sk_style.set_font_families(&families);
    }
    sk_style.set_font_style(FontStyle::new(
        Weight::from(style.font_weight),
        Width::NORMAL,
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
        if let Some(color) = style.decoration_color {
            // `set_decoration_color` takes a Skia `Color` (u32 ARGB,
            // sRGB-encoded by Skia convention), so we gamma-encode our
            // linear value before quantizing to u8 -- otherwise Skia's
            // implicit decode pass darkens the decoration.
            sk_style.set_decoration_color(rgba_linear_to_skia_color(color));
        }
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

fn build_paragraph_style(
    style: &TextStyle,
    base_sk_style: &SkTextStyle,
) -> SkParagraphStyle {
    let mut paragraph_style = SkParagraphStyle::new();
    paragraph_style.set_text_align(style.align.to_skia());
    paragraph_style.set_text_style(base_sk_style);

    if style.text_height_behavior != TextHeightBehavior::All {
        paragraph_style
            .set_text_height_behavior(style.text_height_behavior.to_skia());
    }

    if let Some(max_lines) = style.max_lines {
        paragraph_style.set_max_lines(max_lines);
    }

    if let Some(strut) = &style.strut {
        let mut sk_strut = SkStrutStyle::new();
        sk_strut.set_strut_enabled(true);
        if !strut.font_families.is_empty() {
            let families: Vec<&str> =
                strut.font_families.iter().map(String::as_str).collect();
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
