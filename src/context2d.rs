//! The stateful 2D drawing context, shaped like the Canvas API.
//!
//! Method names and call order match `CanvasRenderingContext2D` so knowledge
//! carries over from the JavaScript side, in Rust's snake_case. Arguments are
//! typed rather than CSS strings: the Canvas standard requires an unparseable
//! value to be *ignored*, and silently doing nothing is a poor trade in a
//! language that can report it. Where a string is convenient, it arrives
//! through a fallible constructor such as
//! [`Font::parse()`](crate::context2d::Font::parse), so the failure is
//! visible at the call site.
//!
//! A context is obtained from
//! [`Canvas::context`](crate::canvas::Canvas::context) and borrows its canvas,
//! so it is used in a scope rather than held for the canvas's lifetime as it
//! would be in JavaScript.

use skia_safe::{
    ColorSpace as SkColorSpace, Data, FourByteTag, IRect,
    ImageInfo as SkImageInfo, Matrix as SkMatrix, Paint as SkPaint,
    PaintStyle as SkPaintStyle, Path as SkPath, PathBuilder as SkPathBuilder,
    PathDirection, Picture as SkPicture, Point as SkPoint, RRect,
    Rect as SkRect, Size as SkSize,
    font_style::{Slant, Weight, Width},
    path::AddPathMode,
    path_1d_path_effect,
    textlayout::{
        Decoration, TextDecorationMode as SkDecorationMode,
        TextDirection as SkTextDirection,
    },
};

use crate::{
    canvas::Canvas,
    color::{
        RgbaLinear, rgba_css, rgba_linear_to_skia_color,
        rgba_linear_to_unpremul_color4f, skia_color_to_rgba_linear,
        unpremul_color4f_to_rgba_linear,
    },
    context::{Context2D as Inner, Dye, page::ExportOptions},
    css::{parse_decoration, parse_filter, parse_length},
    error::Error,
    export::VectorFeatures,
    filter::{ColorFilter, FilterOp, ImageFilter, MaskFilter},
    font::FontVariation,
    geometry::{Affine, Point, Projection, Rect},
    gpu::RenderingEngine,
    image::Image,
    node::{
        filter::{Filter, SamplingQuality},
        image::Content,
        path::{Path2D as NodePath2D, conic_or_line},
        pattern::CanvasPattern,
        typography::{Baseline, DecorationStyle, FontSpec, Spacing},
        utils::{css_to_color, css_to_color4f_in_space},
    },
    paint::{BlendMode, StrokeCap, StrokeJoin},
    path::{FillRule, Path2D},
    pattern::{Pattern, PatternRepeat},
    pixels::{ImageData, PixelColorSpace, PixelDepth, PixelExportOptions},
    shader::Shader,
    text::{
        FontFeature, Paragraph, TextAlign, TextBaseline, TextDecoration,
        TextDecorationStyle, TextMetrics,
    },
    texture::Texture,
};

/// The reading direction a run is laid out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextDirection {
    /// Left to right. The default.
    #[default]
    LeftToRight,
    /// Right to left, for Arabic, Hebrew and related scripts.
    RightToLeft,
}

/// How much work the filter does when an image is drawn at a size other than
/// its own.
///
/// These are the values `imageSmoothingQuality` accepts. `High` selects a
/// scale-aware sampler: Mitchell bicubic when magnifying, trilinear when
/// minifying, since a cubic resampler makes Skia ignore the mipmap chain and
/// alias under heavy downscales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SmoothingQuality {
    /// Bilinear. The default, and what the Canvas API defaults to.
    #[default]
    Low,
    /// Trilinear, off a mipmap chain.
    Medium,
    /// Scale-aware, as described above.
    High,
}

/// How wide a face to select within a family.
///
/// These are the nine CSS `font-stretch` keywords. Selection only: a family
/// that ships one width renders the same at every setting, since no glyph is
/// scaled to fake the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontStretch {
    /// The narrowest, 50% of normal.
    UltraCondensed,
    /// 62.5% of normal.
    ExtraCondensed,
    /// 75% of normal.
    Condensed,
    /// 87.5% of normal.
    SemiCondensed,
    /// The family's regular width. The default.
    #[default]
    Normal,
    /// 112.5% of normal.
    SemiExpanded,
    /// 125% of normal.
    Expanded,
    /// 150% of normal.
    ExtraExpanded,
    /// The widest, 200% of normal.
    UltraExpanded,
}

impl FontStretch {
    fn from_skia(width: Width) -> Self {
        match width {
            w if w == Width::ULTRA_CONDENSED => Self::UltraCondensed,
            w if w == Width::EXTRA_CONDENSED => Self::ExtraCondensed,
            w if w == Width::CONDENSED => Self::Condensed,
            w if w == Width::SEMI_CONDENSED => Self::SemiCondensed,
            w if w == Width::SEMI_EXPANDED => Self::SemiExpanded,
            w if w == Width::EXPANDED => Self::Expanded,
            w if w == Width::EXTRA_EXPANDED => Self::ExtraExpanded,
            w if w == Width::ULTRA_EXPANDED => Self::UltraExpanded,
            _ => Self::Normal,
        }
    }

    /// The CSS `font-stretch` keyword for this width.
    fn to_css(self) -> &'static str {
        match self {
            Self::UltraCondensed => "ultra-condensed",
            Self::ExtraCondensed => "extra-condensed",
            Self::Condensed => "condensed",
            Self::SemiCondensed => "semi-condensed",
            Self::Normal => "normal",
            Self::SemiExpanded => "semi-expanded",
            Self::Expanded => "expanded",
            Self::ExtraExpanded => "extra-expanded",
            Self::UltraExpanded => "ultra-expanded",
        }
    }

    /// The width a CSS `font-stretch` keyword names, if it names one.
    fn from_css(keyword: &str) -> Option<Self> {
        match keyword {
            "ultra-condensed" => Some(Self::UltraCondensed),
            "extra-condensed" => Some(Self::ExtraCondensed),
            "condensed" => Some(Self::Condensed),
            "semi-condensed" => Some(Self::SemiCondensed),
            "semi-expanded" => Some(Self::SemiExpanded),
            "expanded" => Some(Self::Expanded),
            "extra-expanded" => Some(Self::ExtraExpanded),
            "ultra-expanded" => Some(Self::UltraExpanded),
            // `normal` is deliberately absent: it is also a weight and a
            // style keyword, and the caller already treats it as a no-op.
            _ => None,
        }
    }

    pub(crate) fn to_skia(self) -> Width {
        match self {
            Self::UltraCondensed => Width::ULTRA_CONDENSED,
            Self::ExtraCondensed => Width::EXTRA_CONDENSED,
            Self::Condensed => Width::CONDENSED,
            Self::SemiCondensed => Width::SEMI_CONDENSED,
            Self::Normal => Width::NORMAL,
            Self::SemiExpanded => Width::SEMI_EXPANDED,
            Self::Expanded => Width::EXPANDED,
            Self::ExtraExpanded => Width::EXTRA_EXPANDED,
            Self::UltraExpanded => Width::ULTRA_EXPANDED,
        }
    }
}

/// A capitals variant, as CSS `font-variant-caps`.
///
/// Each maps to the OpenType feature the browser would enable. A font
/// without that feature renders unchanged: nothing here synthesizes caps by
/// scaling glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontVariantCaps {
    /// No caps feature. The default.
    #[default]
    Normal,
    /// Small capitals for lowercase letters (`smcp`).
    SmallCaps,
    /// Small capitals for both cases (`c2sc` and `smcp`).
    AllSmallCaps,
    /// Petite capitals, shorter than small caps (`pcap`).
    PetiteCaps,
    /// Petite capitals for both cases (`c2pc` and `pcap`).
    AllPetiteCaps,
    /// Lowercase rendered as small caps, uppercase left alone (`unic`).
    Unicase,
    /// Capitals adjusted for all-caps setting (`titl`).
    TitlingCaps,
}

impl FontVariantCaps {
    /// The CSS keyword, which the state stores for the `fontVariant` getter.
    fn to_css(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::SmallCaps => "small-caps",
            Self::AllSmallCaps => "all-small-caps",
            Self::PetiteCaps => "petite-caps",
            Self::AllPetiteCaps => "all-petite-caps",
            Self::Unicase => "unicase",
            Self::TitlingCaps => "titling-caps",
        }
    }

    /// Whether `tag` is one of the features a caps keyword controls.
    ///
    /// Used to merge rather than clobber: setting the caps variant replaces
    /// these tags and leaves every other feature in place.
    fn owns_tag(tag: &str) -> bool {
        matches!(tag, "smcp" | "c2sc" | "pcap" | "c2pc" | "unic" | "titl")
    }

    /// The keyword a set of enabled features spells, if any.
    ///
    /// The inverse of [`FontVariantCaps::to_features`], so the caps variant
    /// is read back off the features themselves rather than from a copy kept
    /// alongside them.
    fn from_features(tags: &[&str]) -> Self {
        let on = |tag: &str| tags.contains(&tag);
        match () {
            () if on("c2sc") => Self::AllSmallCaps,
            () if on("smcp") => Self::SmallCaps,
            () if on("c2pc") => Self::AllPetiteCaps,
            () if on("pcap") => Self::PetiteCaps,
            () if on("unic") => Self::Unicase,
            () if on("titl") => Self::TitlingCaps,
            () => Self::Normal,
        }
    }

    /// The OpenType features the keyword turns on.
    fn to_features(self) -> Vec<(String, i32)> {
        let on = |tags: &[&str]| {
            tags.iter().map(|tag| (tag.to_string(), 1)).collect()
        };
        match self {
            Self::Normal => vec![],
            Self::SmallCaps => on(&["smcp"]),
            Self::AllSmallCaps => on(&["c2sc", "smcp"]),
            Self::PetiteCaps => on(&["pcap"]),
            Self::AllPetiteCaps => on(&["c2pc", "pcap"]),
            Self::Unicase => on(&["unic"]),
            Self::TitlingCaps => on(&["titl"]),
        }
    }
}

/// What a fill or a stroke draws with.
///
/// Reported by [`Context2D::fill_style`] and
/// [`Context2D::stroke_style`]. Only a colour reads back
/// by value: a shader, pattern or texture is installed by reference and the
/// context keeps the lowered Skia object, not the handle it came from, so
/// what a getter can honestly report is which kind is in force.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaintSource {
    /// A solid colour, as set by [`Context2D::set_fill_style`].
    Color(RgbaLinear),
    /// A shader, as set by [`Context2D::set_fill_shader`] -- including every
    /// gradient, since [`Shader`] is what builds those.
    Shader,
    /// A bitmap pattern, as set by [`Context2D::set_fill_pattern`].
    Pattern,
    /// A vector texture, as set by [`Context2D::set_fill_texture`].
    Texture,
}

/// How a dash marker is placed along the path it follows.
///
/// Only meaningful with [`Context2D::set_line_dash_marker`]. Not in the
/// Canvas standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DashFit {
    /// Keeps the marker upright, translating it along the path.
    Move,
    /// Rotates the marker to the path's tangent. The default.
    #[default]
    Turn,
    /// Bends the marker to follow the path's curvature.
    Follow,
}

impl DashFit {
    fn from_skia(style: path_1d_path_effect::Style) -> Self {
        match style {
            path_1d_path_effect::Style::Translate => Self::Move,
            path_1d_path_effect::Style::Morph => Self::Follow,
            _ => Self::Turn,
        }
    }

    fn to_skia(self) -> path_1d_path_effect::Style {
        match self {
            Self::Move => path_1d_path_effect::Style::Translate,
            Self::Turn => path_1d_path_effect::Style::Rotate,
            Self::Follow => path_1d_path_effect::Style::Morph,
        }
    }
}

/// A font selection: families, size, weight, and slant.
///
/// The Canvas API packs these into one `font` string. Here they are fields,
/// with [`Font::parse()`](crate::context2d::Font::parse) available when the
/// shorthand is more convenient.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// Families in preference order. The first that resolves is used.
    pub families: Vec<String>,
    /// Em size in pixels.
    pub size: f32,
    /// CSS numeric weight, `1` to `1000`. `400` is regular, `700` bold.
    ///
    /// Set through [`Font::weight`], which clamps; assigning the field
    /// directly does not.
    pub weight: u16,
    /// Whether to select an italic face.
    pub italic: bool,
    /// How wide a face to select.
    ///
    /// Carried here so that [`Context2D::set_font`] does not silently undo
    /// an earlier [`Context2D::set_font_stretch`] -- the CSS `font`
    /// shorthand resets the stretch axis, so it has to be expressible in
    /// the same value.
    pub stretch: FontStretch,
    /// Line height in pixels, or `None` for the face's own.
    ///
    /// Only consulted when [`Context2D::set_text_wrap`] is on, since a
    /// single line has no leading to distribute.
    pub line_height: Option<f32>,
}

impl Font {
    /// Creates a font of `size` pixels in `family` at regular weight.
    pub fn new(family: impl Into<String>, size: f32) -> Self {
        Self {
            families: vec![family.into()],
            size,
            weight: 400,
            italic: false,
            stretch: FontStretch::Normal,
            line_height: None,
        }
    }

    /// Selects how wide a face to use.
    pub fn stretch(mut self, stretch: FontStretch) -> Self {
        self.stretch = stretch;
        self
    }

    /// Sets an explicit line height in pixels.
    pub fn line_height(mut self, pixels: f32) -> Self {
        self.line_height = Some(pixels);
        self
    }

    /// Sets the numeric weight.
    ///
    /// CSS allows 1 to 1000; the common named steps are 400 (regular) and
    /// 700 (bold). A value outside that range is clamped into it rather than
    /// passed to the font matcher, which would treat it as an unreachable
    /// target and pick the nearest face anyway.
    pub fn weight(mut self, weight: u16) -> Self {
        self.weight = weight.clamp(1, 1000);
        self
    }

    /// Selects an italic face.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Parses the Canvas `font` shorthand, as in `"italic 700 44px Helvetica"`.
    ///
    /// Accepts, before the size, any of: the keyword `italic` or `oblique`,
    /// a numeric weight or `normal` / `bold`, and one of the eight non-normal
    /// `font-stretch` keywords such as `condensed`. Order among those is not
    /// enforced -- `"700 italic 44px X"` parses the same as
    /// `"italic 700 44px X"` -- then `<size>px`, or
    /// `<size>px/<line-height>px`, and a comma-separated family list. Family
    /// names may be quoted, and the quotes are stripped.
    ///
    /// Anything else is rejected rather than skipped, so a typo surfaces
    /// here instead of rendering in the wrong face.
    ///
    /// Everything a [`Font`] carries round-trips through the shorthand this
    /// parses. It does not round-trip through [`Context2D::font`], which
    /// reports the serialized form the Canvas API asks a getter for: that
    /// has no line-height component, so a font set with one reads back
    /// without it. Keep the [`Font`] rather than the string to recover it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FontRegister`] when the size or a line height is
    /// missing or unparseable, when no family is named, when a weight falls
    /// outside the CSS range of 1 to 1000, or when a token is not one of the
    /// forms above.
    pub fn parse(shorthand: &str) -> Result<Self, Error> {
        let reject = |reason: String| Error::FontRegister { reason };

        let (head, families) =
            shorthand.split_once("px ").ok_or_else(|| {
                // Distinguish the two ways this fails. `"44px"` has a size and
                // no family, and saying it has no size sends the caller looking
                // at the wrong end of the string.
                match shorthand.trim_end().ends_with("px") {
                    true => reject(format!("no font family in {shorthand:?}")),
                    false => reject(format!("no `<size>px` in {shorthand:?}")),
                }
            })?;

        // CSS binds a number to its unit, so `44 px` is not a size. The
        // split above consumed `px ` wherever it appeared, which let a space
        // in front of it through unnoticed.
        if head.ends_with(char::is_whitespace) {
            return Err(reject(format!(
                "font size and unit are separated in {shorthand:?}"
            )));
        }

        let mut tokens = head.split_whitespace().collect::<Vec<_>>();
        // The split above consumed the first `px `, so what is left of the
        // size is either `16` or, when a line height rides along, `16px/24`.
        let (size, line_height) = tokens
            .pop()
            .map(|token| match token.split_once('/') {
                Some((size, leading)) => {
                    (size.trim_end_matches("px"), Some(leading))
                }
                None => (token, None),
            })
            .ok_or_else(|| reject(format!("no font size in {shorthand:?}")))?;
        let size = size
            .parse::<f32>()
            .ok()
            .ok_or_else(|| reject(format!("no font size in {shorthand:?}")))?;
        let line_height = line_height
            .map(|leading| {
                leading.parse::<f32>().map_err(|_| {
                    reject(format!("no line height in {shorthand:?}"))
                })
            })
            .transpose()?;

        let mut font = Self {
            families: families
                .split(',')
                // CSS family names are routinely quoted, and a name kept
                // with its quotes matches no installed face -- it falls
                // through to the next family in silence, which is exactly
                // what this constructor exists to prevent.
                .map(|family| {
                    family
                        .trim()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .trim()
                        .to_string()
                })
                .filter(|family| !family.is_empty())
                .collect(),
            size,
            weight: 400,
            italic: false,
            stretch: FontStretch::Normal,
            line_height,
        };
        if font.families.is_empty() {
            return Err(reject(format!("no font family in {shorthand:?}")));
        }

        for token in tokens {
            match token {
                "italic" | "oblique" => font.italic = true,
                "normal" => {}
                "bold" => font.weight = 700,
                _ if FontStretch::from_css(token).is_some() => {
                    // SAFETY: the guard above already matched it.
                    font.stretch = FontStretch::from_css(token)
                        .expect("guarded by from_css");
                }
                _ => {
                    let weight: u16 = token.parse().map_err(|_| {
                        reject(format!("unrecognized font token {token:?}"))
                    })?;
                    if !(1..=1000).contains(&weight) {
                        return Err(reject(format!(
                            "font weight {weight} is outside the CSS range \
                             1 to 1000"
                        )));
                    }
                    font.weight = weight;
                }
            }
        }

        Ok(font)
    }

    /// Lowers this selection onto the internal font spec.
    fn to_spec(&self) -> FontSpec {
        let families = self
            .families
            .iter()
            .map(|family| match family.contains(char::is_whitespace) {
                true => format!("\"{family}\""),
                false => family.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Everything this carries, in the order and spelling the JavaScript
        // binding uses, so the two name a font the same way and the string
        // identifies the specification uniquely -- which is what the addon's
        // resolved-font cache keys on. A slant, a stretch or a line height
        // left out here would collapse two different fonts onto one entry.
        let mut canonical = vec![if self.italic {
            "italic".to_string()
        } else {
            "normal".to_string()
        }];
        if self.italic {
            // The `font-variant` slot, which only appears once the style
            // slot is holding something other than `normal`.
            canonical.push("normal".to_string());
        }
        canonical.push(self.weight.to_string());
        if self.stretch != FontStretch::Normal {
            canonical.push(self.stretch.to_css().to_string());
        }
        canonical.push(match self.line_height {
            Some(leading) => format!("{}px/{leading}px", self.size),
            None => format!("{}px", self.size),
        });
        canonical.push(families.clone());
        let canonical = canonical.join(" ");

        // What `Context2D::font` reports, which is the string above with
        // every component at its CSS initial value dropped and no line
        // height -- "the serialized form of the current font of the context
        // (with no 'line-height' component)", as HTML puts it. Weight 700
        // is spelled `bold`, which is the spelling a browser returns.
        let serialized = [
            self.italic.then(|| "italic".to_string()),
            (self.weight != 400).then(|| match self.weight {
                700 => "bold".to_string(),
                weight => weight.to_string(),
            }),
            (self.stretch != FontStretch::Normal)
                .then(|| self.stretch.to_css().to_string()),
            Some(format!("{}px", self.size)),
            Some(families),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        FontSpec {
            families: self.families.clone(),
            size: self.size,
            line_height: self.line_height,
            weight: Weight::from(i32::from(self.weight)),
            width: self.stretch.to_skia(),
            slant: if self.italic {
                Slant::Italic
            } else {
                Slant::Upright
            },
            features: vec![],
            variant: "normal".to_string(),
            canonical,
            serialized,
        }
    }
}

/// The drawing surface of a [`Canvas`] page.
///
/// Carries the graphics state -- fill and stroke styles, the current font,
/// transform, and clip -- exactly as the Canvas API does.
#[doc(alias = "CanvasRenderingContext2D")]
pub struct Context2D {
    pub(crate) inner: Inner,
    /// Whether pixel readback may rasterize on the GPU.
    ///
    /// Only [`Context2D::get_image_data`] consults it; encoding takes the
    /// engine from the [`Canvas`], which keeps this
    /// in step through
    /// [`Canvas::set_gpu`](crate::canvas::Canvas::set_gpu).
    pub(crate) gpu: bool,
    /// The pixel format a readback with no layout of its own takes.
    ///
    /// The canvas's, so
    /// [`Canvas::with_options`](crate::canvas::Canvas::with_options) reaches
    /// `get_image_data` the way the JavaScript constructor's `colorType`
    /// reaches `getImageData`.
    pub(crate) canvas_depth: PixelDepth,
    /// The color space a readback with no space of its own is expressed in.
    ///
    /// The canvas's, as a browser does: `getImageData()` on a Display P3
    /// canvas hands back P3 components rather than converting them down.
    pub(crate) canvas_space: PixelColorSpace,
}

impl Context2D {
    /// Wraps a freshly built internal context.
    pub(crate) fn from_inner(
        inner: Inner,
        gpu: bool,
        canvas_depth: PixelDepth,
        canvas_space: PixelColorSpace,
    ) -> Self {
        Self {
            inner,
            gpu,
            canvas_depth,
            canvas_space,
        }
    }

    /// Sets the color subsequent fills use.
    ///
    /// The Canvas API's `fillStyle` also accepts gradients, patterns and
    /// textures; those arrive through their own setters rather than one
    /// union-typed property.
    pub fn set_fill_style(&mut self, color: RgbaLinear) {
        let working = self.inner.canvas_color_space.clone();
        self.inner.state.fill_style = to_dye(color, &working);
    }

    /// Sets the fill color from a CSS color string.
    ///
    /// The same notations the JavaScript `fillStyle` takes, parsed by the same
    /// code: named colors, `#rgb`, `rgb()`, `hsl()`, `hwb()`, `lab()`,
    /// `lch()`, `oklab()`, `oklch()` and `color(<space> r g b / a)`.
    ///
    /// Unlike [`Context2D::set_fill_style`], which takes a value already in
    /// the canvas's space, this keeps the space the string named: on a Display
    /// P3 canvas, `"color(display-p3 1 0 0)"` is that canvas's red exactly,
    /// while `"red"` is sRGB red converted into it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidColor`] when the string is not a CSS color.
    /// A browser ignores an unparseable `fillStyle` and keeps the previous
    /// one; this reports it, because a Rust caller has somewhere to put the
    /// answer.
    pub fn set_fill_style_css(&mut self, css: &str) -> Result<(), Error> {
        self.inner.state.fill_style = css_dye(css)?;
        Ok(())
    }

    /// Sets the color subsequent strokes use.
    ///
    /// `color` is premultiplied linear light, not a CSS triple -- reach for
    /// [`RgbaLinear::from_srgb8`](crate::color::RgbaLinear::from_srgb8) when
    /// porting one. Replaces any shader, pattern or texture previously set
    /// as the stroke style.
    pub fn set_stroke_style(&mut self, color: RgbaLinear) {
        let working = self.inner.canvas_color_space.clone();
        self.inner.state.stroke_style = to_dye(color, &working);
    }

    /// Sets the stroke color from a CSS color string.
    ///
    /// Takes the same notations as [`Context2D::set_fill_style_css`], with the
    /// same handling of the space the string names.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidColor`] when the string is not a CSS color.
    pub fn set_stroke_style_css(&mut self, css: &str) -> Result<(), Error> {
        self.inner.state.stroke_style = css_dye(css)?;
        Ok(())
    }

    /// Sets a shader as the fill style, replacing any color.
    ///
    /// Covers everything the Canvas API's `fillStyle` accepts beyond a color:
    /// the gradients from [`Shader::linear_gradient`] and its siblings, and
    /// the procedural noise shaders.
    pub fn set_fill_shader(&mut self, shader: &Shader) {
        self.inner.state.fill_style =
            Dye::Shader(shader.inner.clone(), shader.features);
    }

    /// Sets a shader as the stroke style, replacing any color.
    pub fn set_stroke_shader(&mut self, shader: &Shader) {
        self.inner.state.stroke_style =
            Dye::Shader(shader.inner.clone(), shader.features);
    }

    /// Builds a repeating fill from `image`.
    ///
    /// The tile is `image` at its own size; install the result with
    /// [`Context2D::set_fill_pattern`].
    ///
    /// To tile raw pixels, wrap them with
    /// [`Image::from_pixels`](crate::image::Image::from_pixels) first.
    ///
    /// The JavaScript `createPattern` additionally rescales an SVG that has
    /// no intrinsic size to the canvas's smaller side. That case cannot
    /// arise here: [`Image`] is always already
    /// rasterized, because
    /// [`Image::from_svg_xml`](crate::image::Image::from_svg_xml) takes the
    /// size to render at.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let tile = Image::from_encoded(&std::fs::read("tile.png")?)?;
    /// let mut canvas = Canvas::new(200.0, 200.0);
    /// let ctx = canvas.context();
    ///
    /// let pattern = ctx.create_pattern(&tile, PatternRepeat::Repeat);
    /// ctx.set_fill_pattern(&pattern);
    /// ctx.fill_rect(0.0, 0.0, 200.0, 200.0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_pattern(
        &self,
        image: &Image,
        repeat: PatternRepeat,
    ) -> Pattern {
        Pattern::from_inner(CanvasPattern::from_parts(
            Content::Bitmap(image.inner.clone()),
            SkSize::new(image.width() as f32, image.height() as f32),
            repeat.to_skia(),
            SkMatrix::new_identity(),
        ))
    }

    /// Builds a repeating fill from another canvas's current page.
    ///
    /// The tile is captured as vectors rather than rasterized, so it stays
    /// sharp under a transform and survives export to
    /// [`Pdf`](crate::export::ImageFormat::Pdf) and
    /// [`Svg`](crate::export::ImageFormat::Svg).
    ///
    /// Takes `source` by `&mut` because capturing the page closes its
    /// current recording.
    pub fn create_pattern_from_canvas(
        &self,
        source: &mut Canvas,
        repeat: PatternRepeat,
    ) -> Pattern {
        let context = source.context();
        let dims = context.inner.bounds.size();
        // The same rule as `capture` above, for the same reason: a pattern
        // made from a page that is itself painted through a pattern of an
        // earlier page doubles the rasterization each round.
        let content = match context.inner.replay_cost() > 0 {
            true => context
                .inner
                .get_source_image(true)
                .map(Content::Bitmap)
                .unwrap_or_default(),
            false => context
                .inner
                .get_picture()
                .map(|picture| Content::Vector(picture, dims))
                .unwrap_or_default(),
        };

        Pattern::from_inner(CanvasPattern::from_parts(
            content,
            dims,
            repeat.to_skia(),
            SkMatrix::new_identity(),
        ))
    }

    /// Sets a pattern as the fill style, replacing any color.
    pub fn set_fill_pattern(&mut self, pattern: &Pattern) {
        self.inner.state.fill_style = Dye::Pattern(pattern.inner.clone());
    }

    /// Sets a pattern as the stroke style, replacing any color.
    pub fn set_stroke_pattern(&mut self, pattern: &Pattern) {
        self.inner.state.stroke_style = Dye::Pattern(pattern.inner.clone());
    }

    /// Sets a texture as the fill style, replacing any color.
    ///
    /// Not in the Canvas standard. Unlike a
    /// [`Pattern`], a texture repeats a vector mark
    /// rather than a bitmap, so it stays crisp at any scale and exports to
    /// the vector formats as geometry.
    pub fn set_fill_texture(&mut self, texture: &Texture) {
        self.inner.state.fill_style = Dye::Texture(texture.inner.clone());
    }

    /// Sets a texture as the stroke style, replacing any color.
    pub fn set_stroke_texture(&mut self, texture: &Texture) {
        self.inner.state.stroke_style = Dye::Texture(texture.inner.clone());
    }

    /// Sets a color filter applied to source colors before blending.
    ///
    /// `None` removes it.
    pub fn set_color_filter(&mut self, filter: Option<&ColorFilter>) {
        self.inner.state.skia_color_filter = filter.map(|f| f.inner.clone());
    }

    /// Sets an image filter applied to the drawing as a whole.
    ///
    /// `None` removes it.
    pub fn set_image_filter(&mut self, filter: Option<&ImageFilter>) {
        self.inner.state.skia_image_filter = filter.map(|f| f.inner.clone());
    }

    /// Sets a mask filter, which blurs coverage rather than color.
    ///
    /// `None` removes it.
    pub fn set_mask_filter(&mut self, filter: Option<&MaskFilter>) {
        self.inner.state.skia_mask_filter = filter.map(|f| f.inner.clone());
    }

    /// Sets the filter chain applied to subsequent draws.
    ///
    /// The Canvas API's `filter` property parses a CSS string; this takes
    /// the operations directly, so `"blur(4px) saturate(150%)"` becomes
    /// `&[FilterOp::Blur(4.0), FilterOp::Saturate(1.5)]`. They apply in
    /// slice order, each to the result of the one before.
    ///
    /// An empty slice is `"none"` and clears the chain.
    ///
    /// Distinct from [`Context2D::set_image_filter`], which installs one
    /// prebuilt Skia filter. The two are independent and both apply.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when any operation carries a
    /// non-finite amount. The chain is left untouched in that case, so a
    /// rejected call cannot half-apply. The JavaScript side discards such a
    /// value in its CSS parser; a typed API has no parser to discard it, and
    /// passing it through makes Skia hand back a null filter that fails on
    /// the *next draw* instead of at the call that caused it.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(64.0, 64.0);
    /// let ctx = canvas.context();
    ///
    /// ctx.set_filter(&[FilterOp::Blur(4.0), FilterOp::Saturate(1.5)])?;
    /// assert!(ctx.set_filter(&[FilterOp::Blur(f32::NAN)]).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_filter(&mut self, ops: &[FilterOp]) -> Result<(), Error> {
        // Validate the whole chain before touching the state: a partially
        // applied filter is harder to reason about than a rejected one.
        for op in ops {
            op.validate()?;
        }

        let css = match ops.is_empty() {
            true => "none".to_string(),
            false => ops
                .iter()
                .map(|op| op.to_css())
                .collect::<Vec<_>>()
                .join(" "),
        };
        let specs = ops.iter().map(|op| op.to_spec()).collect::<Vec<_>>();
        self.inner.state.filter = Filter::new(&css, &specs);
        Ok(())
    }

    /// Sets the filter chain from a CSS `filter` string.
    ///
    /// The string form of [`set_filter`](Self::set_filter), the way
    /// [`set_fill_style_css`](Self::set_fill_style_css) is of
    /// [`set_fill_style`](Self::set_fill_style). Takes what the JavaScript
    /// `ctx.filter` property takes and what a stylesheet would carry, so a
    /// chain copied from either works unchanged.
    ///
    /// Reach for the typed form when writing new Rust: a misspelled
    /// function there is a compile error and here it is a runtime one.
    /// Reach for this when porting, or when the chain arrives as data.
    ///
    /// `"none"` and the empty string clear the filter.
    ///
    /// Lengths in `em`, `rem` and `%` resolve against the context's current
    /// font size, which is what a browser does -- so `blur(0.5em)` is half
    /// the current text size and changes when the font does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFilter`] naming the piece that did not
    /// parse, and [`Error::FilterCreate`] for a value the typed form would
    /// also refuse. The whole string is rejected rather than the readable
    /// parts kept.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(64.0, 64.0);
    /// let ctx = canvas.context();
    /// ctx.set_filter_css("blur(4px) saturate(150%)")?;
    /// assert_eq!(ctx.filter(), "blur(4px) saturate(150%)");
    ///
    /// ctx.set_filter_css("none")?;
    /// assert_eq!(ctx.filter(), "none");
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn set_filter_css(&mut self, css: &str) -> Result<(), Error> {
        let em = self.inner.state.char_style.font_size();
        let ops = parse_filter(css, em)?;
        self.set_filter(&ops)?;
        // Echo the caller's own text rather than what `set_filter` derived
        // from the parsed chain. `saturate(150%)` and `saturate(1.5)` are
        // the same filter, and re-serializing handed back the second when
        // the first was written -- where the JavaScript `filter` getter
        // returns the string it was given. A chain that parsed to nothing
        // keeps the canonical `"none"`, since there is no source form of
        // "no filter" worth preserving.
        if !ops.is_empty() {
            self.inner.state.filter.css = css.trim().to_string();
        }
        Ok(())
    }

    /// The current filter chain as CSS, or `"none"`.
    ///
    /// A chain set through [`set_filter_css`](Self::set_filter_css) comes
    /// back as it was written: `saturate(150%)` stays a percentage, and a
    /// `drop-shadow` keeps its `red`. That is what the JavaScript `filter`
    /// getter does, and what the Canvas API asks for.
    ///
    /// A chain set through the typed [`set_filter`](Self::set_filter) has no
    /// source text to echo, so it is serialized from the operations --
    /// numbers rather than percentages, and a colour as
    /// `rgba(255,0,0,1)`. The two spellings describe the same filter.
    pub fn filter(&self) -> String {
        self.inner.state.filter.to_string()
    }

    /// Sets the font subsequent text draws with.
    ///
    /// Resets the stretch and variant axes, as assigning the CSS `font`
    /// shorthand does -- the shorthand covers them, so anything it omits
    /// reverts. Verified against the JavaScript `font` setter, which clears
    /// both the same way.
    ///
    /// The stretch is therefore part of [`Font`] itself
    /// ([`Font::stretch`]); calling
    /// [`set_font_stretch`](Context2D::set_font_stretch) *before* this
    /// would be undone. [`set_font_variant`](Context2D::set_font_variant)
    /// has no equivalent field and must be set afterwards.
    pub fn set_font(&mut self, font: &Font) {
        self.inner.set_font_spec(font.to_spec());
    }

    /// Fills a rectangle with the current fill style.
    pub fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let path = SkPath::rect(SkRect::from_xywh(x, y, width, height), None);
        self.inner.draw_path(Some(path), SkPaintStyle::Fill, None);
    }

    /// Draws `text` with the current fill style, with its baseline at
    /// (`x`, `y`).
    ///
    /// `max_width` condenses the run to fit when supplied, matching
    /// `fillText`'s optional third argument.
    pub fn fill_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        max_width: Option<f32>,
    ) {
        self.inner
            .draw_text(text, x, y, max_width, SkPaintStyle::Fill);
    }

    /// Draws `text` outlined with the current stroke style.
    pub fn stroke_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        max_width: Option<f32>,
    ) {
        self.inner
            .draw_text(text, x, y, max_width, SkPaintStyle::Stroke);
    }

    // -- Graphics state ----------------------------------------------------

    /// Pushes the current state onto the stack.
    pub fn save(&mut self) {
        self.inner.push();
    }

    /// Pops the state most recently pushed by [`Context2D::save`].
    pub fn restore(&mut self) {
        self.inner.pop();
    }

    /// Whether the drawing context has been lost.
    ///
    /// Always `false`: there is no compositor here that could take the
    /// surface away. Present so code ported from the browser reads the same.
    pub fn is_context_lost(&self) -> bool {
        false
    }

    /// Discards every drawing on this page and resets the state to defaults.
    ///
    /// Matches `reset()`: the state stack, transform, clip, styles and the
    /// current path all return to what a fresh context has.
    pub fn reset(&mut self) {
        let (width, height) =
            (self.inner.bounds.width(), self.inner.bounds.height());
        self.inner = Inner::new(
            self.inner.canvas_color_space.clone(),
            self.canvas_depth.to_skia_color_type(),
            (width, height),
        );
    }

    // -- Text styling ------------------------------------------------------

    /// Sets how a drawn run is positioned horizontally against its x
    /// coordinate.
    pub fn set_text_align(&mut self, align: TextAlign) {
        self.inner.state.graf_style.set_text_align(align.to_skia());
    }

    /// Sets which line of the font a drawn run sits on vertically.
    pub fn set_text_baseline(&mut self, baseline: TextBaseline) {
        self.inner.state.text_baseline = match baseline {
            TextBaseline::Top => Baseline::Top,
            TextBaseline::Hanging => Baseline::Hanging,
            TextBaseline::Middle => Baseline::Middle,
            TextBaseline::Alphabetic => Baseline::Alphabetic,
            TextBaseline::Ideographic => Baseline::Ideographic,
            TextBaseline::Bottom => Baseline::Bottom,
        };
    }

    /// Whether text wraps at the width given to a draw rather than being
    /// condensed onto one line.
    ///
    /// Not in the Canvas standard, where `max_width` always condenses. With
    /// this on, that argument becomes a wrap width instead.
    pub fn set_text_wrap(&mut self, wrap: bool) {
        self.inner.state.text_wrap = wrap;
    }

    /// Sets the reading direction used when laying out a run.
    pub fn set_direction(&mut self, direction: TextDirection) {
        self.inner
            .state
            .graf_style
            .set_text_direction(match direction {
                TextDirection::LeftToRight => SkTextDirection::LTR,
                TextDirection::RightToLeft => SkTextDirection::RTL,
            });
    }

    /// Sets extra space added between characters, in pixels.
    pub fn set_letter_spacing(&mut self, pixels: f32) {
        // NaN is the only value the spacing parser rejects; ignoring it
        // keeps the previous spacing rather than poisoning layout.
        if let Some(spacing) = Spacing::parse(pixels, "px".to_string(), pixels)
        {
            self.inner.state.letter_spacing = spacing;
        }
    }

    /// Sets extra space added between words, in pixels.
    ///
    /// A `NaN` is ignored and the previous spacing stands, as for
    /// [`Context2D::set_letter_spacing`].
    pub fn set_word_spacing(&mut self, pixels: f32) {
        if let Some(spacing) = Spacing::parse(pixels, "px".to_string(), pixels)
        {
            self.inner.state.word_spacing = spacing;
        }
    }

    /// Whether glyph hinting is applied. Off by default, matching the Canvas
    /// API, which has no hinting control at all.
    pub fn set_font_hinting(&mut self, enabled: bool) {
        self.inner.state.font_hinting = enabled;
    }

    /// Selects how wide a face to use, where the family offers a choice.
    ///
    /// Has no effect on a family with only one width: this picks among the
    /// faces installed, it does not synthesize by scaling glyphs
    /// horizontally.
    pub fn set_font_stretch(&mut self, stretch: FontStretch) {
        self.inner.set_font_width(stretch.to_skia());
    }

    /// Selects a capitals variant, such as small caps.
    ///
    /// Applied through the font's OpenType features, so a family without the
    /// relevant feature table renders unchanged rather than having caps
    /// synthesized.
    pub fn set_font_variant_caps(&mut self, caps: FontVariantCaps) {
        // Merge, do not clobber. The JavaScript setter rebuilds the
        // `font-variant` string with only the caps token swapped, so a
        // feature like `onum` set alongside survives -- verified against the
        // binding. Reading the features back off the style keeps this
        // working through save/restore, since the style is what gets cloned.
        let kept = self
            .inner
            .state
            .char_style
            .font_features()
            .iter()
            .filter(|feature| !FontVariantCaps::owns_tag(feature.name()))
            .map(|feature| FontFeature::new(feature.name(), feature.value()))
            .collect::<Vec<_>>();

        self.set_font_variant(caps, &kept);
    }

    /// Sets the capitals variant together with arbitrary OpenType features.
    ///
    /// One feature list per context, so this replaces all of it -- caps and
    /// features alike. Anything an earlier call set is gone unless it is
    /// passed again here.
    ///
    /// [`Context2D::set_font_variant_caps`] is not the mirror of that: it
    /// swaps only the tags a caps keyword owns and leaves every other
    /// feature in place, so setting the caps after this call keeps the
    /// features this call installed.
    ///
    /// A feature the selected face does not implement is ignored by the
    /// shaper rather than reported, which is what the OpenType model
    /// specifies.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(200.0, 60.0);
    /// let ctx = canvas.context();
    ///
    /// // Small caps, old-style figures, no standard ligatures.
    /// ctx.set_font_variant(
    ///     FontVariantCaps::SmallCaps,
    ///     &[FontFeature::on("onum"), FontFeature::off("liga")],
    /// );
    /// ```
    pub fn set_font_variant(
        &mut self,
        caps: FontVariantCaps,
        features: &[FontFeature],
    ) {
        let mut all = caps.to_features();
        all.extend(
            features
                .iter()
                .map(|feature| (feature.name.clone(), feature.value)),
        );

        let described = std::iter::once(caps.to_css().to_string())
            .filter(|keyword| keyword != "normal")
            .chain(features.iter().map(|feature| {
                format!("\"{}\" {}", feature.name, feature.value)
            }))
            .collect::<Vec<_>>();
        let described = match described.is_empty() {
            true => "normal".to_string(),
            false => described.join(" "),
        };

        self.inner.set_font_variant(&described, &all);
    }

    /// Positions the current face along its variable-font axes.
    ///
    /// An empty slice clears them, returning the face to its default
    /// instance. Axes the face does not have are ignored.
    ///
    /// Distinct from [`Font::weight`], which picks among the installed
    /// static faces: this moves along a continuous design axis, so `wght`
    /// can be `537` rather than only `500` or `600`.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(200.0, 60.0);
    /// let ctx = canvas.context();
    ///
    /// ctx.set_font_variation_settings(&[
    ///     FontVariation::new(FontAxisTag::WGHT, 537.0),
    ///     FontVariation::new(FontAxisTag::WDTH, 87.5),
    /// ]);
    /// ```
    pub fn set_font_variation_settings(
        &mut self,
        variations: &[FontVariation],
    ) {
        self.inner.state.variations = variations
            .iter()
            .map(|variation| {
                let [a, b, c, d] = *variation.axis.as_bytes();
                (
                    FourByteTag::from_chars(
                        a as char, b as char, c as char, d as char,
                    ),
                    variation.value,
                )
            })
            .collect();

        self.inner.state.font_variation_settings = match variations.is_empty() {
            true => "normal".to_string(),
            false => variations
                .iter()
                .map(|variation| {
                    // Tags are 4-byte ASCII by construction, so the lossy
                    // conversion cannot actually lose anything.
                    format!(
                        "\"{}\" {}",
                        String::from_utf8_lossy(variation.axis.as_bytes()),
                        variation.value
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        };
    }

    /// Sets the text decoration from a CSS `text-decoration` shorthand.
    ///
    /// The string form of
    /// [`set_text_decoration`](Self::set_text_decoration). Order-insensitive
    /// as CSS is, so `"underline wavy red"` and `"red wavy underline"` are
    /// the same declaration, and any part may be left out.
    ///
    /// `"none"` clears it, as do the CSS global keywords -- there is no
    /// cascade here for `inherit` to reach into, so they can only mean the
    /// initial value.
    ///
    /// A thickness in `em` resolves against the current font size at the
    /// moment this is called, unlike
    /// [`set_letter_spacing_css`](Self::set_letter_spacing_css), which keeps
    /// its unit. The decoration is stored as a resolved style rather than as
    /// a spacing, so there is nowhere to keep the unit.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidColor`] for a token that is neither a
    /// keyword nor a length nor a colour, which is the one way to get this
    /// wrong short of misspelling a keyword.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(64.0, 64.0);
    /// let ctx = canvas.context();
    /// ctx.set_text_decoration_css("underline wavy red")?;
    /// assert_eq!(ctx.text_decoration(), "underline wavy red");
    ///
    /// ctx.set_text_decoration_css("none")?;
    /// assert_eq!(ctx.text_decoration(), "none");
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn set_text_decoration_css(&mut self, css: &str) -> Result<(), Error> {
        let em = self.inner.state.char_style.font_size();
        let parsed =
            parse_decoration(css, em).ok_or_else(|| Error::InvalidColor {
                reason: format!("{css:?}"),
            })?;
        self.set_text_decoration(
            parsed.lines,
            parsed.style,
            parsed.color,
            parsed.thickness,
        );
        // As with `set_filter_css`: report the shorthand the caller wrote.
        // The typed setter can only rebuild a canonical ordering, which
        // turns `underline red wavy` into `underline wavy rgba(255,0,0,1)`
        // -- the same decoration, spelled by the crate rather than by the
        // caller. The JavaScript `textDecoration` getter echoes its input.
        if self.inner.state.text_decoration.css != "none" {
            self.inner.state.text_decoration.css = css.trim().to_string();
        }
        Ok(())
    }

    /// Sets the lines drawn through, over and under subsequent text.
    ///
    /// The Canvas API spells this as the CSS `text-decoration` shorthand;
    /// the arguments here are the same information, split the way
    /// [`TextStyle`](crate::text::TextStyle) already splits it.
    ///
    /// `lines` is a bitmask, so underline and line-through can be drawn
    /// together. [`TextDecoration::default()`] draws nothing, which is how
    /// the decoration is cleared -- there is no separate "none".
    ///
    /// `color` of `None` follows the fill color, as CSS `currentColor`
    /// does. `thickness` of `None` takes the font's own metric, which is
    /// what stays proportional across sizes; a value is in pixels.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(200.0, 60.0);
    /// let ctx = canvas.context();
    ///
    /// // "underline wavy red"
    /// ctx.set_text_decoration(
    ///     TextDecoration::underline(),
    ///     TextDecorationStyle::Wavy,
    ///     Some(RgbaLinear::opaque(1.0, 0.0, 0.0)),
    ///     None,
    /// );
    ///
    /// // "none"
    /// ctx.set_text_decoration(
    ///     TextDecoration::default(),
    ///     TextDecorationStyle::Solid,
    ///     None,
    ///     None,
    /// );
    /// ```
    pub fn set_text_decoration(
        &mut self,
        lines: TextDecoration,
        style: TextDecorationStyle,
        color: Option<RgbaLinear>,
        thickness: Option<f32>,
    ) {
        let css = decoration_css(lines, style, color, thickness);
        if css == "none" {
            self.inner.state.text_decoration = DecorationStyle::default();
            return;
        }

        self.inner.state.text_decoration = DecorationStyle {
            css,
            decoration: Decoration {
                ty: lines.to_skia_flags(),
                style: style.to_skia_decoration_style(),
                // Skia's `Gaps` mode still breaks the line where there is
                // no descender, so the Node path pins `Through` too.
                mode: SkDecorationMode::Through,
                ..Decoration::default()
            },
            size: thickness
                .and_then(|px| Spacing::parse(px, "px".to_string(), px)),
            color: color.map(rgba_linear_to_skia_color),
        };
    }

    /// The current text decoration as CSS, or `"none"`.
    ///
    /// As with [`filter`](Self::filter): a decoration set through
    /// [`set_text_decoration_css`](Self::set_text_decoration_css) comes back
    /// as it was written, and one set through the typed
    /// [`set_text_decoration`](Self::set_text_decoration) is serialized in
    /// the CSS shorthand's own order -- line, style, colour, thickness --
    /// with the colour as `rgba(255,0,0,1)`.
    pub fn text_decoration(&self) -> String {
        self.inner.state.text_decoration.css.clone()
    }

    /// Measures `text` under the current font and text state, without
    /// drawing it.
    ///
    /// `max_width` behaves as it does for a draw: it condenses the run, or
    /// wraps it when [`Context2D::set_text_wrap`] is on.
    ///
    /// The `font_bounding_box_*` values on the result come from the font's
    /// `hhea` table on every platform, which a browser does not guarantee --
    /// see [`TextMetrics::font_bounding_box_ascent`].
    pub fn measure_text(
        &self,
        text: &str,
        max_width: Option<f32>,
    ) -> TextMetrics {
        let e = self.inner.measure_text_extents(text, max_width);

        // The Canvas API measures the bounding box outwards from the
        // alignment point, so left and ascent grow in the negative direction
        // of the ink rectangle and are reported positive.
        //
        // `0.0 - x` rather than `-x`: negating a zero gives a negative zero,
        // which a browser never reports here. Subtracting from zero gives
        // `+0.0` for a zero input and is otherwise the same negation.
        TextMetrics {
            width: e.width,
            actual_bounding_box_left: 0.0 - e.ink.left,
            actual_bounding_box_right: e.ink.right,
            actual_bounding_box_ascent: 0.0 - e.ink.top,
            actual_bounding_box_descent: e.ink.bottom,
            font_bounding_box_ascent: e.font_ascent,
            font_bounding_box_descent: e.font_descent,
            em_height_ascent: e.font_ascent,
            em_height_descent: e.font_descent,
            alphabetic_baseline: e.alphabetic,
            hanging_baseline: e.hanging,
            ideographic_baseline: e.ideographic,
            height: e.height,
            line_count: e.lines,
            lines: e.line_details,
        }
    }

    // -- Images ------------------------------------------------------------

    /// Draws `image` at its natural size with its top left at (`x`, `y`).
    pub fn draw_image(&mut self, image: &Image, x: f32, y: f32) {
        let (width, height) = (image.width() as f32, image.height() as f32);
        self.draw_image_sized(image, x, y, width, height);
    }

    /// Draws `image` scaled into the given rectangle.
    pub fn draw_image_sized(
        &mut self,
        image: &Image,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        let src = SkRect::from_wh(image.width() as f32, image.height() as f32);
        let dst = SkRect::from_xywh(x, y, width, height);
        self.inner.draw_image(&image.inner, &src, &dst);
    }

    /// Draws a sub-rectangle of `image` into a destination rectangle.
    ///
    /// The Canvas API expresses all three of these as one `drawImage` whose
    /// meaning depends on how many arguments were passed; separate names say
    /// which one is meant.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image_region(
        &mut self,
        image: &Image,
        src_x: f32,
        src_y: f32,
        src_width: f32,
        src_height: f32,
        dst_x: f32,
        dst_y: f32,
        dst_width: f32,
        dst_height: f32,
    ) {
        let src = SkRect::from_xywh(src_x, src_y, src_width, src_height);
        let dst = SkRect::from_xywh(dst_x, dst_y, dst_width, dst_height);
        self.inner.draw_image(&image.inner, &src, &dst);
    }

    /// Draws another canvas's current page with its top-left corner at
    /// (`x`, `y`).
    ///
    /// The source is drawn as vectors rather than as a rasterized snapshot,
    /// so it stays sharp under a transform and exports to
    /// [`Pdf`](crate::export::ImageFormat::Pdf) and
    /// [`Svg`](crate::export::ImageFormat::Svg) as geometry. That is the
    /// difference from rendering the source to an
    /// [`Image`] and calling
    /// [`Context2D::draw_image`].
    ///
    /// Not in the Canvas standard.
    pub fn draw_canvas(&mut self, source: &mut Canvas, x: f32, y: f32) {
        let (width, height) = (source.width(), source.height());
        self.draw_canvas_sized(source, x, y, width, height);
    }

    /// Draws another canvas's current page scaled into the given rectangle.
    pub fn draw_canvas_sized(
        &mut self,
        source: &mut Canvas,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        let Some(captured) = capture(source) else {
            return;
        };
        let src = SkRect::from_size(captured.size);
        let dst = SkRect::from_xywh(x, y, width, height);
        place_capture(&mut self.inner, &captured, &src, &dst);
    }

    /// Draws a sub-rectangle of another canvas into a destination rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_canvas_region(
        &mut self,
        source: &mut Canvas,
        src_x: f32,
        src_y: f32,
        src_width: f32,
        src_height: f32,
        dst_x: f32,
        dst_y: f32,
        dst_width: f32,
        dst_height: f32,
    ) {
        let Some(captured) = capture(source) else {
            return;
        };
        let src = SkRect::from_xywh(src_x, src_y, src_width, src_height);
        let dst = SkRect::from_xywh(dst_x, dst_y, dst_width, dst_height);
        place_capture(&mut self.inner, &captured, &src, &dst);
    }

    // -- Image smoothing ---------------------------------------------------

    /// Whether images are filtered when drawn at a size other than their own.
    pub fn set_image_smoothing_enabled(&mut self, enabled: bool) {
        self.inner.state.sampling_filter.smoothing = enabled;
    }

    /// Sets how much work the filter does when an image is resampled.
    ///
    /// Only consulted while [`Context2D::set_image_smoothing_enabled`] is on.
    pub fn set_image_smoothing_quality(&mut self, quality: SmoothingQuality) {
        self.inner.state.sampling_filter.quality = match quality {
            SmoothingQuality::Low => SamplingQuality::Low,
            SmoothingQuality::Medium => SamplingQuality::Medium,
            SmoothingQuality::High => SamplingQuality::High,
        };
    }

    /// Whether a dither pattern is applied, hiding banding in gradients at
    /// the cost of a little noise.
    ///
    /// Not in the Canvas standard.
    pub fn set_dither(&mut self, enabled: bool) {
        self.inner.state.dither = enabled;
    }

    // -- Transforms --------------------------------------------------------

    /// Translates the origin by (`x`, `y`).
    ///
    /// Composes with the current transform rather than replacing it, so
    /// successive calls accumulate. Undone by the matching
    /// [`Context2D::restore`], or by
    /// [`Context2D::reset_transform`].
    pub fn translate(&mut self, x: f32, y: f32) {
        self.inner.with_matrix(|ctm| ctm.pre_translate((x, y)));
    }

    /// Scales subsequent drawing by `x` horizontally and `y` vertically.
    pub fn scale(&mut self, x: f32, y: f32) {
        self.inner.with_matrix(|ctm| ctm.pre_scale((x, y), None));
    }

    /// Rotates subsequent drawing by `radians`, matching the Canvas API's
    /// unit rather than degrees.
    pub fn rotate(&mut self, radians: f32) {
        let degrees = radians.to_degrees();
        self.inner.with_matrix(|ctm| ctm.pre_rotate(degrees, None));
    }

    /// Multiplies `transform` into the current transform.
    pub fn transform(&mut self, transform: Affine) {
        let matrix = affine_to_matrix(transform);
        self.inner.with_matrix(|ctm| ctm.pre_concat(&matrix));
    }

    /// Replaces the current transform.
    pub fn set_transform(&mut self, transform: Affine) {
        // A non-finite component is **ignored** and the current transform
        // stands, which is what the standard asks of `setTransform` and what
        // the JavaScript binding does. Storing one poisoned the CTM: every
        // later draw mapped to NaN and painted nothing, for the life of the
        // context.
        let Affine { a, b, c, d, tx, ty } = transform;
        if [a, b, c, d, tx, ty].iter().any(|v| !v.is_finite()) {
            return;
        }

        let matrix = affine_to_matrix(transform);
        self.inner.with_matrix(|ctm| {
            *ctm = matrix;
            ctm
        });
    }

    /// Restores the identity transform.
    pub fn reset_transform(&mut self) {
        self.set_transform(Affine::IDENTITY);
    }

    /// Solves for the transform mapping `basis` onto `quad`.
    ///
    /// Both are four corners in clockwise order from the top left. `basis`
    /// defaults to the canvas rectangle, so passing only `quad` maps the whole
    /// canvas onto that shape. Apply the result with
    /// [`Context2D::set_projection`].
    ///
    /// Not in the Canvas standard. Unlike [`Context2D::set_transform`], the
    /// result can carry perspective, which is what makes a rectangle mapped
    /// onto a trapezoid read as a receding plane.
    ///
    /// Returns `None` when no such transform exists -- a degenerate quad, or
    /// one whose corners are collinear.
    pub fn create_projection(
        &self,
        quad: [Point; 4],
        basis: Option<[Point; 4]>,
    ) -> Option<Projection> {
        let to_skia =
            |points: [Point; 4]| points.map(|p| SkPoint::new(p.x, p.y));

        let basis = basis
            .map(to_skia)
            .unwrap_or_else(|| self.inner.bounds.to_quad(None));

        SkMatrix::from_poly_to_poly(&basis, &to_skia(quad)).and_then(|matrix| {
            let mut values = [0.0f32; 9];
            for (i, slot) in values.iter_mut().enumerate() {
                *slot = matrix[i];
            }
            // Skia reports success for some quads it cannot actually
            // solve, handing back a matrix of NaN -- four identical
            // corners does it, and so does a single non-finite corner.
            // Collinear corners it does reject. Returning `Some` there
            // contradicted the documented contract and poisoned the CTM
            // the moment the result was applied.
            values
                .iter()
                .all(|value| value.is_finite())
                .then_some(Projection { values })
        })
    }

    /// Multiplies a projective transform into the current one.
    ///
    /// The counterpart to [`Context2D::transform`] for the 3x3 case, and the
    /// one of the pair a drawing usually wants: it composes, so a projection
    /// applied inside a translated or clipped region lands in that region
    /// rather than back at the canvas origin.
    ///
    /// This is what the JavaScript side spells `ctx.transform(matrix)` --
    /// there, one method takes both the affine and the projective case
    /// because both arrive as the same object. Here they are separate
    /// because [`Affine`] carries six values and [`Projection`] nine, and a
    /// type that could be either would make every caller say which.
    ///
    /// Rust had only [`set_projection`](Self::set_projection), which
    /// replaces. A perspective drawn inside a panel translated to `(8, 34)`
    /// therefore threw the translation away and drew at `(0, 0)`, with no
    /// way to ask for the composing form.
    ///
    /// A projection holding a non-finite component is **ignored**, as with
    /// [`set_projection`](Self::set_projection).
    pub fn transform_projection(&mut self, projection: &Projection) {
        if projection.values.iter().any(|v| !v.is_finite()) {
            return;
        }
        let mut matrix = SkMatrix::new_identity();
        matrix.set_9(&projection.values);
        self.inner.with_matrix(|ctm| ctm.pre_concat(&matrix));
    }

    /// Replaces the current transform with a projective one.
    ///
    /// The counterpart to [`Context2D::set_transform`] for the 3x3 case. A
    /// projection holding a non-finite component is **ignored**.
    ///
    /// Replacing is rarely what a drawing wants -- see
    /// [`transform_projection`](Self::transform_projection) for the form
    /// that composes with the transform already in place.
    pub fn set_projection(&mut self, projection: &Projection) {
        // As [`Context2D::set_transform`]: a non-finite component is ignored
        // rather than stored. `Projection` has a public field, so one can be
        // built by hand as well as returned by
        // [`Context2D::create_projection`].
        if projection.values.iter().any(|v| !v.is_finite()) {
            return;
        }

        let mut matrix = SkMatrix::new_identity();
        matrix.set_9(&projection.values);
        self.inner.with_matrix(|ctm| {
            *ctm = matrix;
            ctm
        });
    }

    /// The current transform's affine part.
    ///
    /// [`Affine`] has six components and cannot carry a projective row, so
    /// after [`Context2D::set_projection`] this reports the transform
    /// flattened -- feeding it back through
    /// [`set_transform`](Context2D::set_transform) would drop the
    /// perspective. Use [`Context2D::projection`] to read all nine.
    ///
    /// The JavaScript `getTransform()` has the same six-component shape.
    pub fn get_transform(&self) -> Affine {
        let m = self.inner.state.matrix;
        Affine {
            a: m.scale_x(),
            b: m.skew_y(),
            c: m.skew_x(),
            d: m.scale_y(),
            tx: m.translate_x(),
            ty: m.translate_y(),
        }
    }

    /// The current transform in full, including the projective row.
    ///
    /// Round-trips through [`Context2D::set_projection`] where
    /// [`Context2D::get_transform`] would silently flatten a perspective.
    /// For an ordinary 2D transform the last row is `[0, 0, 1]`.
    ///
    /// Not in the Canvas standard, which has no way to read this back --
    /// and so a bare noun rather than `get_projection`: the `get_` prefix in
    /// this facade is for the methods that mirror a JavaScript `getX()`,
    /// where a bare name would collide with a different operation.
    pub fn projection(&self) -> Projection {
        let mut values = [0.0f32; 9];
        self.inner.state.matrix.get_9(&mut values);
        Projection { values }
    }

    // -- Compositing -------------------------------------------------------

    /// Sets the alpha multiplier applied to everything drawn, `0.0` to `1.0`.
    ///
    /// A value outside that range, or a non-finite one, is **ignored** and
    /// the previous alpha stands. That is what the Canvas standard requires
    /// of the `globalAlpha` setter, and what the JavaScript binding does;
    /// clamping instead would leave the two sides disagreeing on the same
    /// call, since `1.5` would mean "opaque" here and "unchanged" there.
    pub fn set_global_alpha(&mut self, alpha: f32) {
        if (0.0..=1.0).contains(&alpha) {
            self.inner.state.global_alpha = f64::from(alpha);
        }
    }

    /// Sets how subsequent drawing composites against what is already there.
    ///
    /// The Canvas API ignores an unrecognized name; taking the enum removes
    /// the question.
    pub fn set_global_composite_operation(&mut self, mode: BlendMode) {
        let mode = mode.to_skia();

        // Both, and neither on its own. The state field is what
        // `render_to_canvas` reads to decide whether a mode needs a layer of
        // its own, and what the getter reports; the paint is what an ordinary
        // draw actually composites with. Setting only the field left the six
        // layer-taking modes working and the other twenty-two rendering
        // source-over while the getter agreed with the caller.
        self.inner.state.global_composite_operation = mode;
        self.inner.state.paint.set_blend_mode(mode);
    }

    // -- Rectangles --------------------------------------------------------

    /// Strokes a rectangle with the current stroke style.
    pub fn stroke_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let path = SkPath::rect(SkRect::from_xywh(x, y, width, height), None);
        self.inner.draw_path(Some(path), SkPaintStyle::Stroke, None);
    }

    /// Clears a rectangle back to transparent, ignoring the current styles.
    pub fn clear_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.inner
            .clear_rect(&SkRect::from_xywh(x, y, width, height));
    }

    // -- Path2D construction -------------------------------------------------

    /// Discards the current path and starts a new one.
    pub fn begin_path(&mut self) {
        self.inner.path = SkPathBuilder::new();
    }

    /// Starts a new subpath at (`x`, `y`).
    pub fn move_to(&mut self, x: f32, y: f32) {
        if let Some(dst) = self.inner.map_points(&[x, y]).first().copied() {
            self.inner.path.move_to(dst);
        }
    }

    /// Adds a straight segment to (`x`, `y`).
    pub fn line_to(&mut self, x: f32, y: f32) {
        if let Some(dst) = self.inner.map_points(&[x, y]).first().copied() {
            self.inner.scoot(dst);
            self.inner.path.line_to(dst);
        }
    }

    /// Adds a cubic Bézier curve through two control points.
    pub fn bezier_curve_to(
        &mut self,
        cp1x: f32,
        cp1y: f32,
        cp2x: f32,
        cp2y: f32,
        x: f32,
        y: f32,
    ) {
        let points = self.inner.map_points(&[cp1x, cp1y, cp2x, cp2y, x, y]);
        if let [cp1, cp2, dst] = points[..] {
            self.inner.scoot(cp1);
            self.inner.path.cubic_to(cp1, cp2, dst);
        }
    }

    /// Adds a quadratic Bézier curve through one control point.
    pub fn quadratic_curve_to(&mut self, cpx: f32, cpy: f32, x: f32, y: f32) {
        let points = self.inner.map_points(&[cpx, cpy, x, y]);
        if let [cp, dst] = points[..] {
            self.inner.scoot(cp);
            self.inner.path.quad_to(cp, dst);
        }
    }

    /// Adds a closed rectangular subpath.
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let points = self.inner.map_points(&[x, y, x + width, y + height]);
        if let [origin, extent] = points[..] {
            let rect = SkRect::new(origin.x, origin.y, extent.x, extent.y);
            self.inner.path.add_rect(rect, None, None);
        }
    }

    /// Closes the current subpath back to its start.
    pub fn close_path(&mut self) {
        self.inner.path.close();
    }

    // -- Path2D painting -----------------------------------------------------

    /// Fills the current path with the current fill style.
    pub fn fill(&mut self, rule: FillRule) {
        self.inner
            .draw_path(None, SkPaintStyle::Fill, Some(rule.to_skia()));
    }

    /// Strokes the current path with the current stroke style.
    ///
    /// Paints the outline the current width, cap, join, miter limit and dash
    /// pattern describe, centred on the path. Leaves the path in place, so a
    /// fill-then-stroke pair needs no rebuild.
    pub fn stroke(&mut self) {
        self.inner.draw_path(None, SkPaintStyle::Stroke, None);
    }

    /// Intersects the clip with the current path.
    pub fn clip(&mut self, rule: FillRule) {
        self.inner.clip_path(None, rule.to_skia());
    }

    /// Fills `path` with the current fill style, leaving the current path
    /// alone.
    ///
    /// The Canvas API spells this as an overload, `fill(path, rule)`; Rust
    /// has no overloading, so the path-taking form gets its own name. Pair
    /// it with [`Context2D::outline_text`], which returns a
    /// [`Path2D`] this can draw.
    ///
    /// `rule` overrides the path's own [`FillRule`].
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(50.0, 50.0);
    /// let triangle = Path2D::from_svg("M0 0 L40 0 L20 30 Z", FillRule::NonZero)?;
    ///
    /// let ctx = canvas.context();
    /// ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    /// ctx.fill_path(&triangle, FillRule::NonZero);
    /// # Ok(())
    /// # }
    /// ```
    pub fn fill_path(&mut self, path: &Path2D, rule: FillRule) {
        self.inner.draw_path(
            Some(path.inner.clone()),
            SkPaintStyle::Fill,
            Some(rule.to_skia()),
        );
    }

    /// Strokes `path` with the current stroke style, leaving the current
    /// path alone.
    pub fn stroke_path(&mut self, path: &Path2D) {
        self.inner.draw_path(
            Some(path.inner.clone()),
            SkPaintStyle::Stroke,
            None,
        );
    }

    /// Intersects the clip with `path`, leaving the current path alone.
    ///
    /// The path-taking form of [`Context2D::clip`], named the way
    /// [`Context2D::fill_path`] and [`Context2D::stroke_path`] are: the
    /// Canvas API spells all three as overloads, and Rust has no
    /// overloading.
    ///
    /// Undone by the matching [`Context2D::restore`], as any clip is.
    pub fn clip_path(&mut self, path: &Path2D, rule: FillRule) {
        self.inner
            .clip_path(Some(path.inner.clone()), rule.to_skia());
    }

    // -- Stroke styling ----------------------------------------------------

    /// Sets the stroke width in pixels.
    ///
    /// The width is measured in user space, so it scales with the current
    /// transform: a width of `2.0` under `scale(3.0, 3.0)` paints six device
    /// pixels.
    ///
    /// Zero, negative and non-finite widths are **ignored** and the previous
    /// width stands, as the Canvas standard requires and as the JavaScript
    /// binding does. Storing them instead left the getter reporting a width
    /// nothing could paint, and `f32::INFINITY` made the stroke disappear
    /// outright.
    pub fn set_line_width(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        self.inner.state.paint.set_stroke_width(width);
    }

    /// Sets the dash pattern, in alternating on/off lengths. An empty slice
    /// restores a solid line.
    ///
    /// An odd-length pattern is **repeated once**, so the on and off lengths
    /// keep alternating: `[6.0, 2.0, 1.0]` is stored and drawn as
    /// `[6.0, 2.0, 1.0, 6.0, 2.0, 1.0]` -- six on, two off, one on, six off,
    /// two on, one off. The Canvas standard requires this, and Skia accepts
    /// no odd-length list, so without the repeat it built no dash effect at
    /// all and the stroke drew solid.
    ///
    /// A pattern holding a negative or non-finite length is **ignored** and
    /// the previous pattern stands, which the standard also requires. That is
    /// why the bad entries cannot simply be dropped: dropping them would
    /// reshape a pattern the caller never meant to replace, and clearing the
    /// list would turn a dashed stroke silently solid.
    pub fn set_line_dash(&mut self, segments: &[f32]) {
        if segments.iter().any(|n| !n.is_finite() || *n < 0.0) {
            return;
        }

        let mut list = segments.to_vec();
        if list.len() % 2 == 1 {
            list.extend_from_within(..);
        }
        self.inner.state.line_dash_list = list;
    }

    /// The current dash pattern.
    ///
    /// This is the pattern as stored, so one set at an odd length reads back
    /// already repeated. The standard specifies that, and the JavaScript
    /// binding does the same.
    pub fn get_line_dash(&self) -> Vec<f32> {
        self.inner.state.line_dash_list.clone()
    }

    /// Sets how far into the dash pattern the line starts.
    ///
    /// A non-finite offset is **ignored**. Storing one destroyed the pattern
    /// rather than shifting it: Skia built no dash effect at all, and the
    /// stroke came out solid.
    pub fn set_line_dash_offset(&mut self, offset: f32) {
        if !offset.is_finite() {
            return;
        }
        self.inner.state.line_dash_offset = offset;
    }

    /// Stamps `marker` along the dashed path instead of drawing dashes.
    ///
    /// `None` restores plain dashes. The dash list sets the spacing: the
    /// first interval becomes the period the marker repeats at. With an
    /// empty dash list there is no period to repeat at, so the marker is
    /// ignored and the stroke draws solid -- set a dash list first.
    ///
    /// Not in the Canvas standard.
    pub fn set_line_dash_marker(&mut self, marker: Option<&Path2D>) {
        self.inner.state.line_dash_marker =
            marker.map(|path| path.inner.clone());
    }

    /// How a dash marker follows the curve it is stamped along.
    ///
    /// Only consulted while [`Context2D::set_line_dash_marker`] is set.
    /// Not in the Canvas standard.
    pub fn set_line_dash_fit(&mut self, fit: DashFit) {
        self.inner.state.line_dash_fit = fit.to_skia();
    }

    /// Sets how the ends of an open stroked path are drawn.
    pub fn set_line_cap(&mut self, cap: StrokeCap) {
        self.inner.state.paint.set_stroke_cap(cap.to_skia());
    }

    /// Sets how two stroked segments are joined where they meet.
    pub fn set_line_join(&mut self, join: StrokeJoin) {
        self.inner.state.paint.set_stroke_join(join.to_skia());
    }

    /// Sets the ratio past which a miter join falls back to a bevel.
    ///
    /// The ratio is the miter's length over the stroke width, which grows
    /// without bound as a corner sharpens -- the limit is what stops a
    /// near-parallel pair producing a spike across the canvas. A right angle
    /// needs 1.415; the default is 10, which covers everything down to about
    /// 11 degrees. Only consulted while the join is
    /// [`StrokeJoin::Miter`].
    ///
    /// Zero, negative and non-finite limits are **ignored**, matching the
    /// standard and the JavaScript binding. Skia already declined a negative
    /// or NaN limit on its own, so only zero actually reached the paint --
    /// which is exactly the value that turns every miter into a bevel.
    pub fn set_miter_limit(&mut self, limit: f32) {
        if !limit.is_finite() || limit <= 0.0 {
            return;
        }
        self.inner.state.paint.set_stroke_miter(limit);
    }

    /// Adds a conic curve through one control point, weighted.
    ///
    /// Not in the Canvas standard. A weight of `1.0` is a circular arc;
    /// higher pulls the curve toward the control point. A weight of zero or
    /// less degenerates to a straight line rather than a curve whose
    /// denominator crosses zero.
    pub fn conic_curve_to(
        &mut self,
        cpx: f32,
        cpy: f32,
        x: f32,
        y: f32,
        weight: f32,
    ) {
        if let [ctrl, end] = self.inner.map_points(&[cpx, cpy, x, y])[..2] {
            self.inner.scoot(ctrl);
            conic_or_line(&mut self.inner.path, ctrl, end, weight);
        }
    }

    /// Opens a layer that subsequent drawing composites into, closed by the
    /// matching [`Context2D::restore`].
    ///
    /// The layer is composited as a unit when it closes, so a group of draws
    /// shares one alpha and one blend mode instead of each applying its own.
    /// That is the difference from setting
    /// [`set_global_alpha`](Context2D::set_global_alpha) and drawing
    /// normally: at 50% alpha two overlapping shapes accumulate to 75%
    /// coverage, while the same pair inside a 50% layer stays at 50%.
    ///
    /// Composites at full opacity under the current composite operation. Use
    /// [`Context2D::save_layer_with`] for a group alpha, a clipping bounds,
    /// or a backdrop filter.
    pub fn save_layer(&mut self) {
        self.save_layer_with(1.0, None, None);
    }

    /// As [`Context2D::save_layer`], with the layer's own compositing
    /// controls.
    ///
    /// `alpha` scales the whole group when the layer closes. `bounds`
    /// **clips** the layer, and `None` uses the current clip: Skia describes
    /// it as a sizing hint for the offscreen, but nothing outside it is
    /// drawn, so treat it as a clip. `backdrop` filters what is already on
    /// the canvas before the layer draws over it, which is how a
    /// frosted-glass effect is built.
    ///
    /// Not in the Canvas standard, but present in the JavaScript binding as
    /// `saveLayer(alpha, bounds, backdrop)`.
    pub fn save_layer_with(
        &mut self,
        alpha: f32,
        bounds: Option<Rect>,
        backdrop: Option<&ImageFilter>,
    ) {
        // The paint is what makes this a *group*: without it the layer is a
        // straight copy and neither the alpha nor the blend mode reaches the
        // composite. Passing `None` here made `save_layer` an expensive
        // no-op that still read as working.
        let mut paint = SkPaint::default();
        paint.set_anti_alias(true);
        paint.set_alpha_f(alpha);
        paint.set_blend_mode(self.inner.state.global_composite_operation);

        self.inner.save_layer(
            Some(paint),
            bounds.map(to_skia_rect),
            backdrop.map(|filter| filter.inner.clone()),
        );
    }

    /// Returns `text` as a path instead of drawing it.
    ///
    /// Not in the Canvas standard. Useful for exporting outlines, or for
    /// applying path operations to glyph shapes.
    pub fn outline_text(&self, text: &str, max_width: Option<f32>) -> Path2D {
        Path2D::from_inner(self.inner.outline_text(text, max_width))
    }

    /// Paints an already laid-out paragraph with its top-left corner at
    /// (`x`, `y`).
    ///
    /// Unlike [`Context2D::fill_text`], the styling comes from the layout,
    /// not from the context: font, color and decoration were fixed when the
    /// paragraph was built. What the context still contributes is the
    /// compositing -- transform, clip, global alpha and composite operation
    /// all apply.
    ///
    /// Reuse a layout across frames rather than rebuilding it; laying out is
    /// the expensive half.
    ///
    /// Not in the Canvas standard.
    pub fn draw_paragraph(&mut self, layout: &Paragraph, x: f32, y: f32) {
        // Composited, not bare: the paragraph carries its own paints, so
        // without this wrapper the context's alpha and blend mode would be
        // silently dropped.
        self.inner.with_composited_canvas(|canvas| {
            layout.paragraph.paint(canvas, SkPoint::new(x, y));
        });
    }

    // -- Arcs and rounded shapes -------------------------------------------

    /// Adds a circular arc centred on (`x`, `y`).
    ///
    /// Angles are in radians. `counterclockwise` reverses the sweep, as the
    /// Canvas API's optional last argument does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRadius`] for a negative or non-finite `radius`,
    /// as [`Context2D::arc_to`] does. A browser throws on the negative case
    /// and quietly does nothing on the other; a quiet nothing reads as
    /// success at a typed call site, so both are reported here.
    pub fn arc(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        counterclockwise: bool,
    ) -> Result<(), Error> {
        self.add_ellipse(
            x,
            y,
            radius,
            radius,
            0.0,
            start_angle,
            end_angle,
            counterclockwise,
        )
    }

    /// Adds an elliptical arc, with independent radii and a rotation.
    ///
    /// # Errors
    ///
    /// As [`Context2D::arc`], for either radius.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        x_radius: f32,
        y_radius: f32,
        rotation: f32,
        start_angle: f32,
        end_angle: f32,
        counterclockwise: bool,
    ) -> Result<(), Error> {
        self.add_ellipse(
            x,
            y,
            x_radius,
            y_radius,
            rotation,
            start_angle,
            end_angle,
            counterclockwise,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_ellipse(
        &mut self,
        x: f32,
        y: f32,
        x_radius: f32,
        y_radius: f32,
        rotation: f32,
        start_angle: f32,
        end_angle: f32,
        ccw: bool,
    ) -> Result<(), Error> {
        check_radii(x_radius, y_radius)?;
        let matrix = self.inner.state.matrix;
        let mut arc = NodePath2D::default();
        arc.add_ellipse(
            (x, y),
            (x_radius, y_radius),
            rotation,
            start_angle,
            end_angle,
            ccw,
        );
        // Extend, not Append: the arc continues the current contour.
        // Appending starts a new one, which strokes identically but fills as
        // a separate region.
        //
        // Transformed as it is added rather than copied first.
        // `make_transform` builds a whole second path -- points, verbs and
        // conic weights -- for one use. On its own this measured inside the
        // noise, and it is kept for being one allocation where there were
        // two, and for matching `Path2D::add_ellipse`, where the same shape
        // was worth 76 times the speed on a long path.
        self.inner.path.add_path_with_transform(
            &arc.path(),
            &matrix,
            AddPathMode::Extend,
        );
        Ok(())
    }

    /// Adds an arc tangent to the lines from the current point to
    /// (`x1`, `y1`) and from there to (`x2`, `y2`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRadius`] for a negative or non-finite `radius`,
    /// as [`Context2D::arc`] does. A browser throws `IndexSizeError` for the
    /// negative case and quietly does nothing for the other; a quiet nothing
    /// reads as success at a typed call site, so both are reported here.
    pub fn arc_to(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        radius: f32,
    ) -> Result<(), Error> {
        if radius < 0.0 || !radius.is_finite() {
            return Err(Error::InvalidRadius { radius });
        }
        if let [src, dst] = self.inner.map_points(&[x1, y1, x2, y2])[..2] {
            self.inner.scoot(src);
            self.inner.path.arc_to_tangent(src, dst, radius);
        }
        Ok(())
    }

    /// Adds a rounded rectangle, with one circular radius per corner
    /// starting at the top left and running clockwise.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRadius`] when a radius is negative or
    /// non-finite. Skia would clamp such a value to zero and draw a square
    /// corner; the Canvas API throws a `RangeError`, and quietly drawing the
    /// wrong shape is the worse of the two.
    pub fn round_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: [f32; 4],
    ) -> Result<(), Error> {
        self.round_rect_elliptical(x, y, width, height, radii.map(|r| (r, r)))
    }

    /// Adds a rounded rectangle whose corners may be elliptical, each given
    /// as a horizontal and vertical radius.
    ///
    /// The Canvas API accepts `{x, y}` per corner for the same reason. A
    /// pair with equal components is the circular case
    /// [`Context2D::round_rect`] covers.
    ///
    /// # Errors
    ///
    /// As [`Context2D::round_rect`].
    pub fn round_rect_elliptical(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: [(f32, f32); 4],
    ) -> Result<(), Error> {
        let rect = SkRect::from_xywh(x, y, width, height);
        if let Some(radius) = radii
            .iter()
            .flat_map(|(rx, ry)| [*rx, *ry])
            .find(|r| *r < 0.0 || !r.is_finite())
        {
            return Err(Error::InvalidRadius { radius });
        }

        let matrix = self.inner.state.matrix;
        let corners: Vec<SkPoint> = radii
            .iter()
            .map(|(rx, ry)| SkPoint::new(*rx, *ry))
            .collect();
        let rrect = RRect::new_rect_radii(
            rect,
            &[corners[0], corners[1], corners[2], corners[3]],
        );
        // Skia's legacy 6 (CW) / 7 (CCW) start corner, deliberately unlike
        // `Path2D::round_rect`, which pins 0. The start corner decides where
        // `Extend` attaches, where the current point lands, and where dash
        // phase begins, so the two entry points are meant to differ.
        let direction = if width.signum() == height.signum() {
            PathDirection::CW
        } else {
            PathDirection::CCW
        };
        // Transformed as it is added, as `add_ellipse` is, and for the same
        // reason.
        self.inner.path.add_path_with_transform(
            &SkPath::rrect(rrect, Some(direction)),
            &matrix,
            AddPathMode::Extend,
        );
        Ok(())
    }

    // -- Hit testing -------------------------------------------------------

    /// Whether (`x`, `y`) lies inside the current path when filled.
    pub fn is_point_in_path(&mut self, x: f32, y: f32, rule: FillRule) -> bool {
        let mut path = self.inner.path.snapshot();
        self.inner.hit_test_path(
            &mut path,
            (x, y),
            Some(rule.to_skia()),
            SkPaintStyle::Fill,
        )
    }

    /// Whether (`x`, `y`) lies inside `path` when filled.
    ///
    /// Uses the current transform, as the Canvas API's
    /// `isPointInPath(path, x, y, rule)` does.
    pub fn is_point_in_filled_path(
        &mut self,
        path: &Path2D,
        x: f32,
        y: f32,
        rule: FillRule,
    ) -> bool {
        let mut path = path.inner.clone();
        // A `Path2D` is in its own space and takes the current transform at
        // query time, so the point is mapped back through the inverse to meet
        // it there. The context's own path needs no such step: it is already
        // in device space, which is where the point is.
        let point = self.inner.in_local_coordinates(x, y);
        self.inner.hit_test_path(
            &mut path,
            point,
            Some(rule.to_skia()),
            SkPaintStyle::Fill,
        )
    }

    /// Whether (`x`, `y`) lies on `path` when stroked with the current
    /// stroke styling.
    pub fn is_point_in_stroked_path(
        &mut self,
        path: &Path2D,
        x: f32,
        y: f32,
    ) -> bool {
        let mut path = path.inner.clone();
        // As `is_point_in_filled_path`: the path is in user space, the point
        // is not, so the point is mapped to meet it.
        let point = self.inner.in_local_coordinates(x, y);
        self.inner
            .hit_test_path(&mut path, point, None, SkPaintStyle::Stroke)
    }

    /// Whether (`x`, `y`) lies on the current path when stroked.
    pub fn is_point_in_stroke(&mut self, x: f32, y: f32) -> bool {
        let mut path = self.inner.path.snapshot();
        self.inner
            .hit_test_path(&mut path, (x, y), None, SkPaintStyle::Stroke)
    }

    // -- Shadows -----------------------------------------------------------

    /// Sets the shadow blur radius. `0.0` disables blurring.
    ///
    /// A negative or non-finite radius is **ignored** and the previous radius
    /// stands, as in the standard and the JavaScript binding. Zero is a
    /// legitimate setting here, unlike [`Context2D::set_line_width`], because
    /// it means "no blur" rather than "nothing to draw".
    pub fn set_shadow_blur(&mut self, blur: f32) {
        if !blur.is_finite() || blur < 0.0 {
            return;
        }
        self.inner.state.shadow_blur = blur;
    }

    /// Sets the shadow color. A fully transparent color disables shadows.
    pub fn set_shadow_color(&mut self, color: RgbaLinear) {
        self.inner.state.shadow_color = rgba_linear_to_skia_color(color);
    }

    /// Sets the shadow color from a CSS color string.
    ///
    /// Takes the same notations as
    /// [`Context2D::set_fill_style_css`]. Shadows are drawn through an
    /// eight-bit color, so a wide-gamut string is converted rather than kept.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidColor`] when the string is not a CSS color.
    pub fn set_shadow_color_css(&mut self, css: &str) -> Result<(), Error> {
        self.inner.state.shadow_color =
            css_to_color(css).ok_or_else(|| Error::InvalidColor {
                reason: format!("{css:?}"),
            })?;
        Ok(())
    }

    /// Sets the shadow offset.
    ///
    /// The Canvas API splits this into `shadowOffsetX` and `shadowOffsetY`;
    /// one call taking both is the same information without the chance of
    /// setting one and forgetting the other.
    ///
    /// Any offset may be negative, but a non-finite one is **ignored** --
    /// and, because the pair arrives together here where the standard takes
    /// them separately, one bad component discards both rather than leaving
    /// the offset half-updated.
    pub fn set_shadow_offset(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        self.inner.state.shadow_offset = SkPoint::new(x, y);
    }

    // -- Reading the graphics state ----------------------------------------
    //
    // Every piece of state has a reader -- including the three filter slots,
    // which hand back an installable filter rather than a flag -- so the
    // ordinary
    // save-modify-restore idiom -- `let old = ctx.line_width(); ...;
    // ctx.set_line_width(old)` -- works without reaching for `save`/`restore`
    // and its all-or-nothing scope. The union-typed fill and stroke styles
    // report a [`PaintSource`], which round-trips a color and otherwise names
    // the kind in force: a shader, pattern or texture is installed by
    // reference and lowered on the way in, so there is no handle left to give
    // back.

    /// The alpha multiplier applied to everything drawn.
    pub fn global_alpha(&self) -> f32 {
        // Stored as the `double` the Canvas IDL specifies; an `f32` that went
        // in comes back out of the widening exactly.
        self.inner.state.global_alpha as f32
    }

    /// How subsequent drawing composites against what is already there.
    pub fn global_composite_operation(&self) -> BlendMode {
        BlendMode::from_skia(self.inner.state.global_composite_operation)
    }

    /// The color filter applied to source colors before blending, if any.
    ///
    /// Handed back as a filter rather than a flag, so the ordinary
    /// save-modify-restore idiom works on it:
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(20.0, 20.0);
    /// let ctx = canvas.context();
    ///
    /// let previous = ctx.color_filter();
    /// ctx.set_color_filter(Some(&ColorFilter::luma()));
    /// // ... draw ...
    /// ctx.set_color_filter(previous.as_ref());
    /// # Ok(())
    /// # }
    /// ```
    pub fn color_filter(&self) -> Option<ColorFilter> {
        self.inner
            .state
            .skia_color_filter
            .as_ref()
            .map(|inner| ColorFilter {
                inner: inner.clone(),
            })
    }

    /// The image filter applied to the drawing as a whole, if any.
    ///
    /// Distinct from the chain [`Context2D::set_filter`] installs; the two
    /// are independent and both apply.
    pub fn image_filter(&self) -> Option<ImageFilter> {
        self.inner
            .state
            .skia_image_filter
            .as_ref()
            .map(|inner| ImageFilter {
                inner: inner.clone(),
            })
    }

    /// The mask filter applied to coverage before painting, if any.
    pub fn mask_filter(&self) -> Option<MaskFilter> {
        self.inner
            .state
            .skia_mask_filter
            .as_ref()
            .map(|inner| MaskFilter {
                inner: inner.clone(),
            })
    }

    /// What fills are painted with.
    pub fn fill_style(&self) -> PaintSource {
        to_paint_source(&self.inner.state.fill_style)
    }

    /// What strokes are painted with.
    pub fn stroke_style(&self) -> PaintSource {
        to_paint_source(&self.inner.state.stroke_style)
    }

    /// The reading direction text is laid out in.
    pub fn direction(&self) -> TextDirection {
        match self.inner.state.graf_style.text_direction() {
            SkTextDirection::RTL => TextDirection::RightToLeft,
            _ => TextDirection::LeftToRight,
        }
    }

    /// The capitals variant in force.
    ///
    /// Read back off the font features themselves, so a variant set through
    /// [`Context2D::set_font_variant`] is reported here too.
    pub fn font_variant_caps(&self) -> FontVariantCaps {
        let features = self.inner.state.char_style.font_features();
        let tags = features
            .iter()
            .filter(|feature| feature.value() != 0)
            .map(|feature| feature.name())
            .collect::<Vec<_>>();
        FontVariantCaps::from_features(&tags)
    }

    /// The path stamped along a dashed stroke, or `None` for plain dashes.
    pub fn line_dash_marker(&self) -> Option<Path2D> {
        self.inner
            .state
            .line_dash_marker
            .as_ref()
            .map(|marker| Path2D::from_inner(marker.clone()))
    }

    /// How a dash marker follows the curve it is stamped along.
    pub fn line_dash_fit(&self) -> DashFit {
        DashFit::from_skia(self.inner.state.line_dash_fit)
    }

    /// The stroke width in pixels.
    ///
    /// Read from the paint the stroke is drawn with, not from a copy kept
    /// beside it, so what this reports is what would paint.
    pub fn line_width(&self) -> f32 {
        self.inner.state.paint.stroke_width()
    }

    /// How the ends of an open stroked path are drawn.
    pub fn line_cap(&self) -> StrokeCap {
        StrokeCap::from_skia(self.inner.state.paint.stroke_cap())
    }

    /// How two stroked segments are joined where they meet.
    pub fn line_join(&self) -> StrokeJoin {
        StrokeJoin::from_skia(self.inner.state.paint.stroke_join())
    }

    /// The miter limit beyond which a miter join falls back to a bevel.
    pub fn miter_limit(&self) -> f32 {
        self.inner.state.paint.stroke_miter()
    }

    /// How far into the dash pattern the first dash starts.
    pub fn line_dash_offset(&self) -> f32 {
        self.inner.state.line_dash_offset
    }

    /// The shadow blur radius. `0.0` means no blurring.
    pub fn shadow_blur(&self) -> f32 {
        self.inner.state.shadow_blur
    }

    /// The shadow color.
    ///
    /// Stored as 8-bit sRGB with 8-bit alpha, so a colour set from
    /// [`RgbaLinear::from_srgb8`] with an alpha on a whole 255th reads back
    /// exactly and anything else is quantised: `0.5` returns as `0.5019608`,
    /// and the premultiplied components shift with it.
    pub fn shadow_color(&self) -> RgbaLinear {
        skia_color_to_rgba_linear(self.inner.state.shadow_color)
    }

    /// The shadow offset, as `(x, y)` pixels.
    pub fn shadow_offset(&self) -> (f32, f32) {
        let offset = self.inner.state.shadow_offset;
        (offset.x, offset.y)
    }

    /// Whether images are filtered when drawn at a size other than their own.
    pub fn image_smoothing_enabled(&self) -> bool {
        self.inner.state.sampling_filter.smoothing
    }

    /// How much work the filter does when an image is resampled.
    pub fn image_smoothing_quality(&self) -> SmoothingQuality {
        match self.inner.state.sampling_filter.quality {
            SamplingQuality::Medium => SmoothingQuality::Medium,
            SamplingQuality::High => SmoothingQuality::High,
            // `None` is the internal no-resampling case, which the public
            // enum expresses as smoothing being off rather than a quality.
            SamplingQuality::Low | SamplingQuality::None => {
                SmoothingQuality::Low
            }
        }
    }

    /// Whether a dither pattern is applied.
    pub fn dither(&self) -> bool {
        self.inner.state.dither
    }

    /// The current font, as the Canvas API serializes it.
    ///
    /// The CSS `font` shorthand with every component at its initial value
    /// left out and no `line-height`, which is what a browser returns from
    /// the same getter: a font set from `"16px/24px Helvetica"` reads back
    /// as `"16px Helvetica"`, and one set from `"bold 16px Helvetica"` reads
    /// back unchanged rather than as `"normal 700 16px Helvetica"`.
    pub fn font(&self) -> String {
        self.inner.state.font.clone()
    }

    /// How wide a face is selected within the family.
    pub fn font_stretch(&self) -> FontStretch {
        FontStretch::from_skia(self.inner.state.font_width)
    }

    /// The `font-variant` string the current features describe.
    pub fn font_variant(&self) -> String {
        self.inner.state.font_variant.clone()
    }

    /// The `font-variation-settings` string, or `"normal"`.
    pub fn font_variation_settings(&self) -> String {
        self.inner.state.font_variation_settings.clone()
    }

    /// Whether glyph hinting is applied.
    pub fn font_hinting(&self) -> bool {
        self.inner.state.font_hinting
    }

    /// Sets the spacing between characters from a CSS length.
    ///
    /// The string form of [`set_letter_spacing`](Self::set_letter_spacing),
    /// and the one that can say `em`. A relative length stays relative:
    /// `"0.1em"` is a tenth of the font size *whenever the text is laid
    /// out*, so changing the font afterwards changes the spacing. The pixel
    /// form cannot express that, because by the time it is a number the
    /// relationship to the font is gone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCssLength`] for a unit CSS does not define,
    /// for a bare number other than zero, and for a percentage --
    /// `letter-spacing` is defined over a length, and a percentage is not
    /// one.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(64.0, 64.0);
    /// let ctx = canvas.context();
    /// ctx.set_font(&Font::parse("20px Helvetica")?);
    /// ctx.set_letter_spacing_css("0.1em")?;
    /// assert_eq!(ctx.letter_spacing(), 2.0);
    ///
    /// // The same spacing against a larger font is a larger gap.
    /// ctx.set_font(&Font::parse("40px Helvetica")?);
    /// assert_eq!(ctx.letter_spacing(), 4.0);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn set_letter_spacing_css(&mut self, css: &str) -> Result<(), Error> {
        self.inner.state.letter_spacing = css_spacing(css)?;
        Ok(())
    }

    /// Sets the spacing added at word boundaries from a CSS length.
    ///
    /// As [`set_letter_spacing_css`](Self::set_letter_spacing_css), applied
    /// between words rather than between characters.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCssLength`] on the same terms.
    pub fn set_word_spacing_css(&mut self, css: &str) -> Result<(), Error> {
        self.inner.state.word_spacing = css_spacing(css)?;
        Ok(())
    }

    /// Extra space added between glyphs, in pixels.
    pub fn letter_spacing(&self) -> f32 {
        self.inner
            .state
            .letter_spacing
            .in_px(self.inner.state.char_style.font_size())
    }

    /// Extra space added at word boundaries, in pixels.
    pub fn word_spacing(&self) -> f32 {
        self.inner
            .state
            .word_spacing
            .in_px(self.inner.state.char_style.font_size())
    }

    /// Where a text run sits horizontally relative to its origin.
    pub fn text_align(&self) -> TextAlign {
        TextAlign::from_skia(self.inner.state.graf_style.text_align())
    }

    /// Which horizontal line of the font a text draw sits on.
    pub fn text_baseline(&self) -> TextBaseline {
        match self.inner.state.text_baseline {
            Baseline::Top => TextBaseline::Top,
            Baseline::Hanging => TextBaseline::Hanging,
            Baseline::Middle => TextBaseline::Middle,
            Baseline::Ideographic => TextBaseline::Ideographic,
            Baseline::Bottom => TextBaseline::Bottom,
            Baseline::Alphabetic => TextBaseline::Alphabetic,
        }
    }

    /// Whether text wraps at the width given to a draw.
    pub fn text_wrap(&self) -> bool {
        self.inner.state.text_wrap
    }

    // -- Image data --------------------------------------------------------

    /// Allocates a transparent [`ImageData`] of `width` by `height`.
    ///
    /// The buffer is unattached: nothing is read from the page and nothing
    /// is drawn until it is passed to [`Context2D::put_image_data`]. Fill it
    /// through [`ImageData::pixels_mut`].
    ///
    /// The layout is the `putImageData` wire format -- sRGB, 8 bits per
    /// channel, unpremultiplied. Use [`Context2D::create_image_data_as`] for
    /// anything else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when either dimension is zero,
    /// as the Canvas API throws `IndexSizeError` for the same input, and for
    /// a buffer past the signed 32-bit byte count Skia can address -- 23170
    /// square at this depth. JavaScript never reaches the upper limit,
    /// since V8 caps a typed array first.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(64.0, 64.0);
    /// let ctx = canvas.context();
    ///
    /// let mut swatch = ctx.create_image_data(2, 2)?;
    /// swatch.pixels_mut().fill(255); // opaque white
    /// ctx.put_image_data(&swatch, 10.0, 10.0)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_image_data(
        &self,
        width: u32,
        height: u32,
    ) -> Result<ImageData, Error> {
        self.create_image_data_as(width, height, PixelExportOptions::default())
    }

    /// As [`Context2D::create_image_data`], in an explicit pixel layout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when either dimension is zero,
    /// or when the requested depth puts the buffer past the signed 32-bit
    /// byte count Skia can address.
    pub fn create_image_data_as(
        &self,
        width: u32,
        height: u32,
        options: PixelExportOptions,
    ) -> Result<ImageData, Error> {
        ImageData::blank(width, height, options)
    }

    /// Reads back the rendered pixels inside `rect`.
    ///
    /// The rectangle is in canvas coordinates and ignores the current
    /// transform, as `getImageData` does. An inverted rectangle is
    /// normalized rather than rejected, and the part lying outside the page
    /// reads back transparent.
    ///
    /// The result is in the `getImageData` wire format -- sRGB, 8 bits per
    /// channel, unpremultiplied. Use [`Context2D::get_image_data_as`] to
    /// read back at a higher depth, in a wider color space, or
    /// premultiplied.
    ///
    /// This rasterizes the page, so it is far from free inside a draw loop.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when the rectangle rounds to an
    /// empty one or its width or height is non-finite,
    /// [`Error::InvalidRect`] when the origin is non-finite or the rectangle
    /// reaches past the coordinate range Skia rounds into, and
    /// [`Error::PixelReadback`] when the surface declines the read --
    /// including a region so large that the buffer exceeds the signed 32-bit
    /// byte count Skia addresses pixels with, which is roughly 23000 square
    /// at 8 bits per channel.
    pub fn get_image_data(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<ImageData, Error> {
        self.get_image_data_as(
            x,
            y,
            width,
            height,
            PixelExportOptions {
                depth: self.canvas_depth,
                color_space: self.canvas_space,
                ..PixelExportOptions::default()
            },
        )
    }

    /// As [`Context2D::get_image_data`], in an explicit pixel layout.
    ///
    /// # Errors
    ///
    /// Additionally returns [`Error::UnsupportedPixelColorSpace`] when the
    /// requested color space cannot be built by this Skia build.
    pub fn get_image_data_as(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        options: PixelExportOptions,
    ) -> Result<ImageData, Error> {
        // Reject non-finite input before rounding. `SkRect::round` saturates
        // to i32::MIN/MAX, and the width subtraction that follows then
        // overflows -- a debug panic, and in release a nonsense -256 in the
        // error the caller sees.
        //
        // Only the extents are checked here. A non-finite origin falls
        // through to the range check below, which reports it as the rect it
        // belongs to: an `InvalidDimensions` carrying the width and height
        // names the two values that were fine and hides the one that was not.
        for (name, value) in [("width", width), ("height", height)] {
            if !value.is_finite() {
                return Err(Error::InvalidDimensions {
                    width: match name {
                        "width" => value,
                        _ => width,
                    },
                    height: match name {
                        "height" => value,
                        _ => height,
                    },
                });
            }
        }

        // Floor every value, then absify a negative extent by shifting the
        // origin -- the rule `getImageData` uses. Rounding the four edges of
        // a rectangle instead is not the same thing: it reads (2.2, 2.2,
        // 4.4, 4.4) as 5x5 where the Canvas API reads 4x4.
        let (mut x, mut y) = (x.floor(), y.floor());
        let (mut w, mut h) = (width.floor(), height.floor());
        if w < 0.0 {
            x += w;
            w = -w;
        }
        if h < 0.0 {
            y += h;
            h = -h;
        }

        // Finite is not enough. `SkRect::round` saturates each edge to
        // `i32::MIN`/`MAX`, and `IRect::width` then subtracts them -- so a
        // rect that *spans* the range, `(-3e9, -3e9, 6e9, 6e9)`, panics
        // inside skia-safe with "attempt to subtract with overflow" before
        // any of this crate's own checks are reached. The earlier guard here
        // only rejected non-finite values, and the test that covered it only
        // tried `(0, 0, n, n)`, which saturates one edge rather than both.
        //
        // Anything this large is refused a page later anyway, for exceeding
        // the byte count Skia can address, so failing here costs no
        // legitimate call -- it only turns the panic into that same error.
        let (l, t) = (f64::from(x), f64::from(y));
        let (dw, dh) = (f64::from(w), f64::from(h));
        let (edges_fit, extents_fit) = {
            let limit = f64::from(i32::MAX);
            (
                l >= -limit
                    && t >= -limit
                    && l + dw <= limit
                    && t + dh <= limit,
                dw <= limit && dh <= limit,
            )
        };
        if !edges_fit || !extents_fit {
            return Err(Error::InvalidRect {
                rect: Rect {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                },
            });
        }

        // Built from the `f64` edges above rather than through `SkRect`,
        // which holds `f32`s and stops representing consecutive integers past
        // 2^24 -- a bound a canvas can reach, since that is what #111 clamps
        // a dimension to. A six-pixel read at x=16777213 has a right edge of
        // 16777219, which rounds to 16777220 and yields a seven-pixel row;
        // one at x=16777216 has a right edge of 16777217, which rounds back
        // down to the origin and yields nothing at all. Every value here is
        // whole after the floors above, so the casts truncate exactly, and
        // the range check has already bounded all four into `i32`.
        let crop =
            IRect::new(l as i32, t as i32, (l + dw) as i32, (t + dh) as i32);
        let (width, height) = (crop.width(), crop.height());
        if width <= 0 || height <= 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }

        let internal = ExportOptions {
            // What this call converts into...
            color_type: options.depth.to_skia_color_type(),
            color_space: options.color_space.to_skia_color_space()?,
            // ...and what it converts out of, which is the canvas's own and
            // not the caller's business. Left at the defaults, a readback
            // rebuilt the page in sRGB at eight bits whatever the canvas was.
            surface_color_space: self.canvas_space.to_skia_color_space()?,
            surface_color_type: self.canvas_depth.to_skia_color_type(),
            ..ExportOptions::default()
        };
        let engine = self.engine();
        let pixels = self
            .inner
            .get_pixels_as(crop, internal, engine, options.to_alpha_type())
            .map_err(|reason| Error::PixelReadback { reason })?;

        ImageData::from_pixels(width as u32, height as u32, options, pixels)
    }

    /// Writes `data` onto the page with its top-left corner at (`x`, `y`).
    ///
    /// Deliberately not a draw: the current transform, clip, global alpha,
    /// composite operation and shadow are all bypassed, and the destination
    /// is cleared first, so the pixels land exactly as supplied. That is
    /// what `putImageData` specifies.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedPixelColorSpace`] when `data`'s color
    /// space cannot be built by this Skia build, and [`Error::PixelWrite`]
    /// when Skia declines the buffer -- which previously reported `Ok(())`
    /// for a write that never happened.
    pub fn put_image_data(
        &mut self,
        data: &ImageData,
        x: f32,
        y: f32,
    ) -> Result<(), Error> {
        let source = Rect::from_xywh(
            0.0,
            0.0,
            data.width() as f32,
            data.height() as f32,
        );
        self.blit(
            data,
            source,
            Rect::from_xywh(x, y, source.width(), source.height()),
        )
    }

    /// As [`Context2D::put_image_data`], writing only the `dirty` region.
    ///
    /// `dirty` is in `data`'s own coordinates, and the region lands at
    /// (`x`, `y`) plus the dirty origin -- the seven-argument
    /// `putImageData`. An inverted `dirty` rectangle is normalized.
    ///
    /// # Errors
    ///
    /// As [`Context2D::put_image_data`].
    #[allow(clippy::too_many_arguments)]
    pub fn put_image_data_region(
        &mut self,
        data: &ImageData,
        x: f32,
        y: f32,
        dirty_x: f32,
        dirty_y: f32,
        dirty_width: f32,
        dirty_height: f32,
    ) -> Result<(), Error> {
        let source = normalized(Rect::from_xywh(
            dirty_x,
            dirty_y,
            dirty_width,
            dirty_height,
        ));
        let destination = Rect::from_xywh(
            source.left + x,
            source.top + y,
            source.width(),
            source.height(),
        );
        self.blit(data, source, destination)
    }

    /// Lowers an [`ImageData`] onto the internal blit, which clears the
    /// destination and writes without transform, clip or blending.
    fn blit(
        &mut self,
        data: &ImageData,
        source: Rect,
        destination: Rect,
    ) -> Result<(), Error> {
        // The casts are safe by construction, not by luck: every
        // `ImageData` is sized through `byte_len`, which refuses a
        // buffer past `i32::MAX` bytes, and a buffer is at least four bytes
        // a pixel -- so neither dimension can reach `i32::MAX`. Without that
        // ceiling a width above it truncated to a negative `i32` and Skia
        // panicked on the resulting `ImageInfo`.
        // `pixel_buffer_dimensions_cannot_overflow_an_i32` pins the ceiling
        // so relaxing it fails there rather than here.
        let info = SkImageInfo::new(
            (data.width() as i32, data.height() as i32),
            data.depth().to_skia_color_type(),
            data.options().to_alpha_type(),
            data.color_space().to_skia_color_space()?,
        );
        match self.inner.blit_pixels_raw(
            info,
            Data::new_copy(data.pixels()),
            &to_skia_rect(source),
            &to_skia_rect(destination),
        ) {
            true => Ok(()),
            false => Err(Error::PixelWrite {
                reason: format!(
                    "Skia declined a {}x{} buffer in the requested layout",
                    data.width(),
                    data.height()
                ),
            }),
        }
    }

    /// The engine readback rasterizes through.
    ///
    /// Mirrors [`Canvas::set_gpu`](crate::canvas::Canvas::set_gpu), which
    /// keeps this flag in step across every page.
    fn engine(&self) -> RenderingEngine {
        if !self.gpu {
            return RenderingEngine::CPU;
        }
        let engine = RenderingEngine::default();
        // The same rule `Canvas::engine` follows: a float page the GPU
        // cannot composite is rasterised rather than narrowed to eight bits.
        // Both have to agree, or a readback and an export of one canvas
        // would be composited by different engines.
        match engine.can_composite(self.canvas_depth.to_skia_color_type()) {
            true => engine,
            false => RenderingEngine::CPU,
        }
    }
}

/// Closes a canvas's recording and hands back its page as vectors.
///
/// `None` when the page recorded nothing, which is a blank canvas rather
/// than a failure -- the callers treat it as nothing to draw.
/// What [`capture`] resolved a source canvas to.
struct Captured {
    content: Content,
    size: SkSize,
    features: VectorFeatures,
    cost: usize,
    /// The source's own picture when it was handed over as pixels, so the
    /// draw can replay just the region it shows rather than materializing
    /// the whole page. `None` when the content is already a picture.
    picture: Option<SkPicture>,
}

fn capture(source: &mut Canvas) -> Option<Captured> {
    let context = source.context();
    let size = context.inner.bounds.size();
    let features = context.inner.get_page().vector_features();

    // The rule `node::image::Source::of` follows, applied to the same
    // question asked through this API. A canvas is handed over as a picture
    // so a vector backend can see through it, and a picture reached by two
    // paths is replayed twice while being recorded once -- so a page drawn
    // into a canvas and that canvas drawn back, round after round, doubles
    // the eventual rasterization. A source already carrying someone else's
    // picture pays for its pixels here instead.
    //
    // The rasterizing happens at the draw rather than here, for the same
    // reason it does on the binding's path: only the destination knows how
    // much of this source it can show, and taking a whole page to put a
    // sliver of it on screen is most of what the flattening costs.
    let cost = context.inner.replay_cost();

    match cost > 0 {
        true => context.inner.get_source_image(false).map(|image| Captured {
            content: Content::Bitmap(image),
            size,
            features,
            cost,
            picture: context.inner.get_picture(),
        }),
        false => context.inner.get_picture().map(|picture| Captured {
            content: Content::Vector(picture, size),
            size,
            features,
            cost,
            picture: None,
        }),
    }
}

/// Draws whatever `capture` answered with, charging what replaying it costs.
fn place_capture(
    ctx: &mut Inner,
    captured: &Captured,
    src: &SkRect,
    dst: &SkRect,
) {
    let Captured {
        content,
        features,
        cost,
        picture,
        ..
    } = captured;
    let (features, cost) = (*features, *cost);
    match content {
        Content::Vector(picture, _) => {
            ctx.draw_picture_costing(picture, src, dst, features, cost.max(1))
        }
        // A bitmap here is a source that carries nesting and has yet to be
        // rasterized; `draw_nested_image` takes only the part this draw can
        // show. Charged as well, because the destination now carries it.
        Content::Bitmap(image) => {
            ctx.charge_replay(cost.max(1));
            ctx.draw_nested_image(image, picture.as_ref(), src, dst)
        }
        _ => {}
    }
}

/// The CSS shorthand a decoration corresponds to, or `"none"`.
///
/// The internal state stores the string as well as the parsed form, so it
/// has to be reconstructed here rather than left blank: an empty one makes
/// the Node setter treat the decoration as invalid and discard it.
/// The `text-decoration` shorthand for a decoration the caller described in
/// parts.
///
/// Order follows the CSS shorthand -- line, style, color, thickness -- so the
/// result parses back to what it came from. `color` of `None` is
/// `currentColor`, which the shorthand expresses by leaving it out.
fn decoration_css(
    lines: TextDecoration,
    style: TextDecorationStyle,
    color: Option<RgbaLinear>,
    thickness: Option<f32>,
) -> String {
    let mut parts = Vec::new();
    if lines.underline {
        parts.push("underline");
    }
    if lines.overline {
        parts.push("overline");
    }
    if lines.line_through {
        parts.push("line-through");
    }
    if parts.is_empty() {
        return "none".to_string();
    }

    parts.push(match style {
        TextDecorationStyle::Solid => "solid",
        TextDecorationStyle::Double => "double",
        TextDecorationStyle::Dotted => "dotted",
        TextDecorationStyle::Dashed => "dashed",
        TextDecorationStyle::Wavy => "wavy",
    });

    let mut css = parts.join(" ");
    if let Some(color) = color {
        css.push(' ');
        css.push_str(&rgba_css(color));
    }
    if let Some(thickness) = thickness {
        css.push_str(&format!(" {thickness}px"));
    }
    css
}

/// Lowers a public [`RgbaLinear`] onto the internal paint source.
///
/// Two conversions, both easy to omit and invisible until alpha drops below
/// one: [`RgbaLinear`] is premultiplied and Skia's paint color is not, and
/// Skia decodes an untagged `Color4f` as sRGB, so the linear-light values
/// need the linear tag to survive.
/// Reports which kind of source a [`Dye`] is, recovering the colour when it
/// holds one.
fn to_paint_source(dye: &Dye) -> PaintSource {
    match dye {
        Dye::Color(color, _) => {
            PaintSource::Color(unpremul_color4f_to_rgba_linear(*color))
        }
        // A gradient only reaches the state through the JavaScript binding;
        // the Rust facade builds every gradient as a `Shader`, and both are
        // the same thing to a caller who can only be told which kind it is.
        Dye::Gradient(_) | Dye::Shader(..) => PaintSource::Shader,
        Dye::Pattern(_) => PaintSource::Pattern,
        Dye::Texture(_) => PaintSource::Texture,
    }
}

/// Rejects an arc's radii the way the Canvas API does, reporting the radius
/// that was refused.
///
/// It took the arc's centre as well, to build the ellipse the radii described
/// and report *that* -- which named a shape nothing had rejected, and gave a
/// negative radius an ellipse with its edges crossed.
pub(crate) fn check_radii(x_radius: f32, y_radius: f32) -> Result<(), Error> {
    if let Some(radius) = [x_radius, y_radius]
        .into_iter()
        .find(|radius| *radius < 0.0 || !radius.is_finite())
    {
        return Err(Error::InvalidRadius { radius });
    }
    Ok(())
}

/// A [`Spacing`] from a CSS length.
///
/// Carries the unit through rather than resolving it, so a relative length
/// is still relative after it is set: `Spacing::in_px` takes the font size
/// at the moment the text is laid out.
fn css_spacing(css: &str) -> Result<Spacing, Error> {
    let invalid = || Error::InvalidCssLength {
        reason: format!("{css:?}"),
    };
    let length = parse_length(css).ok_or_else(invalid)?;
    Spacing::parse(length.value, length.unit, length.pixels).ok_or_else(invalid)
}

/// A CSS color string as a fill, keeping the space it was named in.
///
/// The parsing is the Node binding's, so both surfaces accept exactly the same
/// notations and land on the same color: one grammar, one place it is
/// implemented. `Dye::Color` already carries a color together with its source
/// space, which is what lets `color(display-p3 ...)` reach a P3 canvas without
/// a detour through sRGB.
fn css_dye(css: &str) -> Result<Dye, Error> {
    let (color, space) =
        css_to_color4f_in_space(css).ok_or_else(|| Error::InvalidColor {
            reason: format!("{css:?}"),
        })?;
    Ok(Dye::Color(color, Some(space)))
}

/// Tags a color with the canvas's own working space, in linear light.
///
/// [`RgbaLinear`] is defined as premultiplied linear light *in the
/// destination surface's working color space*, so pinning it to linear sRGB
/// made every color on a Display P3 canvas mean something the type does not
/// say -- and put wide-gamut colors out of a Rust caller's reach.
fn to_dye(color: RgbaLinear, working: &SkColorSpace) -> Dye {
    Dye::Color(
        rgba_linear_to_unpremul_color4f(color),
        Some(working.with_linear_gamma()),
    )
}

/// Sorts a rectangle's edges, so a negative extent describes the same region
/// rather than an empty one. `getImageData` and `putImageData` both accept
/// negative dimensions this way.
fn normalized(rect: Rect) -> Rect {
    Rect {
        left: rect.left.min(rect.right),
        top: rect.top.min(rect.bottom),
        right: rect.left.max(rect.right),
        bottom: rect.top.max(rect.bottom),
    }
}

/// Lowers a public [`Rect`] onto Skia's.
fn to_skia_rect(rect: Rect) -> SkRect {
    SkRect::new(rect.left, rect.top, rect.right, rect.bottom)
}

/// Lowers a public [`Affine`] onto Skia's matrix, which carries a projective
/// row this crate's affine type does not.
pub(crate) fn affine_to_matrix(t: Affine) -> SkMatrix {
    let mut matrix = SkMatrix::new_identity();
    matrix.set_affine(&[t.a, t.b, t.c, t.d, t.tx, t.ty]);
    matrix
}
