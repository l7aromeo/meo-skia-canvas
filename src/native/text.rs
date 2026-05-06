use std::ops::Range;

use skia_safe::{
    FontMgr, FontStyle, Paint as SkPaint, Point as SkPoint,
    font_style::{Slant, Weight, Width},
    textlayout::{
        FontCollection, Paragraph as SkParagraph, ParagraphBuilder as SkParagraphBuilder,
        ParagraphStyle as SkParagraphStyle, RectHeightStyle, RectWidthStyle,
        TextAlign as SkTextAlign, TextDecoration as SkTextDecoration,
        TextDecorationStyle as SkTextDecorationStyle, TextShadow as SkTextShadow,
        TextStyle as SkTextStyle,
    },
};

use crate::native::color::{
    RgbaLinear, linear_srgb_color_space, rgba_linear_to_skia_color, rgba_linear_to_unpremul_color4f,
};
use crate::native::font::NativeFontManager;
use crate::native::geometry::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerticalAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextSlant {
    #[default]
    Upright,
    Italic,
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

/// Paragraph style. The paragraph-level fields (`align`,
/// `line_height_multiplier`) only apply when this style is used as the
/// base for a paragraph; per-span overrides via `RichTextSpan` see
/// only the per-span fields below them.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub font_families: Vec<String>,
    pub font_size: f32,
    pub font_weight: i32,
    pub slant: TextSlant,
    pub color: RgbaLinear,
    pub align: TextAlign,
    /// Multiplier applied to the font's natural line height. `1.0` keeps
    /// Skia's default. Values above `1.0` add line spacing.
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
    /// Vertical offset from the baseline, in pixels. Positive shifts
    /// downward; negative shifts upward (use for superscripts).
    pub baseline_shift: f32,
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
        }
    }
}

/// Underline / overline / line-through flags. Multiple flags can be
/// combined (e.g. underline + line-through together).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextDecoration {
    pub underline: bool,
    pub overline: bool,
    pub line_through: bool,
}

impl TextDecoration {
    pub const fn underline() -> Self {
        Self {
            underline: true,
            overline: false,
            line_through: false,
        }
    }

    pub const fn line_through() -> Self {
        Self {
            underline: false,
            overline: false,
            line_through: true,
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextDecorationStyle {
    #[default]
    Solid,
    Double,
    Dotted,
    Dashed,
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
}

/// Drop shadow applied behind glyphs. Multiple shadows on a single
/// `TextStyle` stack additively.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub color: RgbaLinear,
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_sigma: f32,
}

/// One span of rich text. Carries its own `TextStyle` for per-span
/// font, color, decoration, baseline shift, etc. Paragraph-level fields
/// (`align`, `line_height_multiplier`) on the span style are ignored;
/// only the base style governs them.
#[derive(Debug, Clone, PartialEq)]
pub struct RichTextSpan {
    pub text: String,
    pub style: TextStyle,
}

/// Per-line layout metrics. `start_index` and `end_index` are byte
/// offsets into the laid-out paragraph text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NativeLineMetrics {
    pub line_number: usize,
    pub start_index: usize,
    pub end_index: usize,
    pub ascent: f32,
    pub descent: f32,
    pub height: f32,
    pub width: f32,
    pub baseline: f32,
    pub left: f32,
    pub hard_break: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextBoxOptions {
    pub color: RgbaLinear,
    pub font_family: Option<String>,
    pub font_size: f32,
    pub font_weight: i32,
    pub horizontal_align: TextAlign,
    pub vertical_align: VerticalAlign,
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

/// Build laid-out text from a `TextStyle` and a maximum line width.
/// Construct with `new(font_manager)` to use a registered font registry,
/// or `with_system_fonts()` for the platform's default fonts only.
pub struct NativeTextEngine {
    pub(crate) collection: FontCollection,
}

impl NativeTextEngine {
    /// Build using `font_manager`'s registered typefaces plus system
    /// fallbacks for unmatched family names.
    pub fn new(font_manager: &NativeFontManager) -> Self {
        let mut collection = FontCollection::new();
        collection.set_default_font_manager(FontMgr::new(), None);
        collection.set_asset_font_manager(Some(font_manager.snapshot_provider().into()));
        Self { collection }
    }

    /// Build using the platform's system fonts only. Useful when no
    /// `NativeFontManager` is needed.
    pub fn with_system_fonts() -> Self {
        let mut collection = FontCollection::new();
        collection.set_default_font_manager(FontMgr::new(), None);
        Self { collection }
    }

    /// Lay out `text` against `style`, wrapping at `max_width`. Returns
    /// a `NativeTextLayout` that can be measured or drawn via
    /// `NativeCanvas::draw_text_layout`.
    pub fn layout_text(&self, text: &str, style: &TextStyle, max_width: f32) -> NativeTextLayout {
        let sk_text_style = build_text_style(style);
        let paragraph_style = build_paragraph_style(style, &sk_text_style);

        let mut builder = SkParagraphBuilder::new(&paragraph_style, self.collection.clone());
        builder.add_text(text);
        let mut paragraph = builder.build();
        paragraph.layout(max_width);
        NativeTextLayout {
            paragraph,
            max_width,
        }
    }

    /// Lay out a rich-text paragraph. The paragraph-level state
    /// (`align`, `line_height_multiplier`) comes from `base_style`;
    /// each `RichTextSpan` overlays its own per-span style for the
    /// span's text (font, color, decoration, baseline shift, etc.).
    pub fn layout_rich_text(
        &self,
        spans: &[RichTextSpan],
        base_style: &TextStyle,
        max_width: f32,
    ) -> NativeTextLayout {
        let base_sk_style = build_text_style(base_style);
        let paragraph_style = build_paragraph_style(base_style, &base_sk_style);

        let mut builder = SkParagraphBuilder::new(&paragraph_style, self.collection.clone());
        for span in spans {
            let span_sk_style = build_text_style(&span.style);
            builder.push_style(&span_sk_style);
            builder.add_text(&span.text);
            builder.pop();
        }
        let mut paragraph = builder.build();
        paragraph.layout(max_width);
        NativeTextLayout {
            paragraph,
            max_width,
        }
    }
}

/// Result of `NativeTextEngine::layout_text`. Owns the laid-out
/// paragraph; metrics queries are cheap and `draw_text_layout` paints
/// the same paragraph onto a canvas.
pub struct NativeTextLayout {
    pub(crate) paragraph: SkParagraph,
    max_width: f32,
}

impl NativeTextLayout {
    /// Measured width of the longest laid-out line, after wrapping.
    /// Matches the `TextLayout.width` semantics in the TypeScript
    /// renderer: the width that the laid-out content actually occupies,
    /// not the wrapping budget. Use `max_width()` to recover the
    /// caller-requested layout budget.
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

    /// Distance from the paragraph's top edge to the first line's
    /// baseline ascent. Useful for vertical alignment of text against a
    /// known baseline.
    pub fn first_line_ascent(&self) -> f32 {
        let metrics = self.paragraph.get_line_metrics();
        metrics.first().map(|m| m.ascent as f32).unwrap_or_default()
    }

    /// Per-line metrics for the laid-out paragraph. The vector is
    /// indexed by line number and ordered top-to-bottom.
    pub fn line_metrics(&self) -> Vec<NativeLineMetrics> {
        self.paragraph
            .get_line_metrics()
            .iter()
            .enumerate()
            .map(|(i, m)| NativeLineMetrics {
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

    /// Bounding rectangles for the byte range `[range.start, range.end)`
    /// in the laid-out paragraph. Useful for selection rendering and
    /// for placing baseline-shift overlays (e.g. superscripts) directly
    /// over the affected glyphs.
    pub fn get_rects_for_range(&self, range: Range<usize>) -> Vec<Rect> {
        self.paragraph
            .get_rects_for_range(range, RectHeightStyle::Tight, RectWidthStyle::Tight)
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
        let families: Vec<&str> = style.font_families.iter().map(String::as_str).collect();
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
            sk_style.set_decoration_thickness_multiplier(style.decoration_thickness);
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

fn build_paragraph_style(style: &TextStyle, base_sk_style: &SkTextStyle) -> SkParagraphStyle {
    let mut paragraph_style = SkParagraphStyle::new();
    paragraph_style.set_text_align(match style.align {
        TextAlign::Left => SkTextAlign::Left,
        TextAlign::Center => SkTextAlign::Center,
        TextAlign::Right => SkTextAlign::Right,
    });
    paragraph_style.set_text_style(base_sk_style);
    paragraph_style
}
