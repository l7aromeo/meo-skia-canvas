#![allow(dead_code)]
use neon::prelude::*;
use parking_lot::Mutex;
use skia_safe::{
    AlphaType, BlendMode, Canvas as SkCanvas, ClipOp, Color, Color4f,
    ColorFilter as SkColorFilter, ColorSpace, ColorType, Contains, Data, IRect,
    Image, ImageFilter as SkImageFilter, ImageInfo, MaskFilter as SkMaskFilter,
    Paint, PaintCap, PaintJoin, PaintStyle, Path, PathBuilder, PathFillType,
    PathOp, Picture, PictureRecorder, Point, Rect, RoundOut,
    Shader as SkShader, Size, Typeface, Vector,
    canvas::SrcRectConstraint::Strict,
    color_filters, dash_path_effect,
    font_style::{FontStyle, Width},
    image_filter::MapDirection,
    image_filters,
    matrix::{Matrix, TypeMask},
    path_1d_path_effect,
    path_utils::fill_path_with_paint,
    textlayout::{StrutStyle, TextStyle},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

pub mod api;
pub(crate) mod blur_mask;
pub(crate) mod chain;
pub(crate) mod chain_cache;
pub(crate) mod exact_blur;
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) mod exact_blur_gpu;
pub(crate) mod exact_gradient;
pub(crate) mod faint;
pub(crate) mod fold;
pub(crate) mod kept;
pub mod layer;
pub mod page;
pub(crate) mod resident;
pub(crate) mod run_mask;
pub mod typesetting;

#[cfg(test)]
use crate::node::typography::Painter;
use exact_gradient::GradientSpec;

use crate::{
    cache_map::KeptSets,
    context2d::{FontKerning, TextRendering, font_spec_from_css},
    css::{FontUnits, NamedColour, ROOT_FONT_PX},
    export::{VectorFeatures, blend_features},
    filter::onto::{Onto, PageFilter},
    font::GenericFamily,
    font_library::{FontLibrary, ResolvedFont, generic_name, text_hinting},
    gpu::{self, RenderingEngine},
    gradient::{BoxedCanvasGradient, CanvasGradient},
    node::{
        external::External,
        filter::{
            Coverage, Filter, SamplingFilter, SamplingQuality, ScalingOperation,
        },
        image::ImageData,
        pattern::{BoxedCanvasPattern, CanvasPattern},
        shader::BoxedShader,
        texture::{BoxedCanvasTexture, CanvasTexture},
        typography::with_style_axes,
    },
    pixels::PixelColorSpace,
    started,
    text::FontMetrics,
    typography::{
        Baseline, DecorationStyle, DrawnRun, DrawnText, FontSpec, PendingInk,
        RunPaint, Spacing, TextExtents, TypesetInputs, Typesetter,
        paragraph_style,
    },
    utils::*,
};
use chain::{Step, canvas_reach};
use exact_blur::REACH_SIGMAS;
use faint::Faint;
use layer::{
    BlurDraw, Composite, CoveragePicture, CoverageRun, DeferredPicture,
    Entries, ExactColour, Group, Layer, LayerRec, LazyPicture, Replay,
    coverage_files, holds_a_cut, holds_a_pair, reach_the_clip,
};
use page::{ExportOptions, Page, PageRecorder, Recording};
use run_mask::Kept;
use typesetting::{TextSettings, Typesetting, Typography};

/// A shadow cast by one run of a paragraph's text -- see
/// [`Context2D::draw_with_shadows`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShadowCast {
    /// Its blur, in pixels of the canvas whatever its transform, as a
    /// filter's length is.
    pub(crate) sigma: f32,
    /// What its sharp shadow draws in, in the coordinates the paragraph is
    /// drawn in.
    pub(crate) bounds: Rect,
}

impl ShadowCast {
    /// Whether the shadow is blurred: by a sigma that is a length, and more
    /// than none.
    fn blurs(&self) -> bool {
        self.sigma.is_finite() && self.sigma > 0.0
    }
}

/// The part of a paragraph [`Context2D::draw_with_shadows`] asks a draw
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShadowPart {
    /// What lies beneath every shadow: the backgrounds.
    Beneath,
    /// The cast at this index, sharp.
    Sharp(usize),
    /// The cast at this index, blurred by Skia's mask filter, glyph by
    /// glyph: what a document draws.
    Masked(usize),
    /// What lies over every shadow: the glyphs and their decorations.
    Over,
}

const BLACK: Color = Color::BLACK;
const TRANSPARENT: Color = Color::TRANSPARENT;

/// The native memory a context holds whatever is drawn on it, which
/// [`Context2D::report_resident`] tells the engine beside its page and its
/// recording.
///
/// Measured, not derived, so it goes stale when what a context holds
/// changes: the state, the path and the rest of [`Context2D`]. With heap
/// and MallocStackLogging on a debug build for arm64 macOS, 400 CPU
/// canvases of 1000 by 800 with a context, nothing drawn, held 4,800 bytes
/// apiece more than 400 without one: 3,680 in eleven allocations of this
/// crate's own, 640 Skia's paragraph style, 448 its path builder and 32 a
/// text style.
///
/// One figure for every platform, since the allocators count the same
/// allocations apart only by how they round them. Batches of 4,000 such
/// canvases, by the C allocator's own count of bytes in use: 4,590 to
/// 4,613 a context on glibc 2.43 (arm64 Linux), whose chunks carry an
/// 8-byte header at 16-byte alignment, and 5,001 to 5,045 on macOS, whose
/// small allocations round to 16 bytes. This stands at 1.04 of the one and
/// 0.95 of the other. `tests/suite/external-memory.test.js` holds it to
/// that count within a tenth.
pub(crate) const CONTEXT_BYTES: usize = 4_800;

/// The most grid positions a texture may be stamped at in a single draw.
///
/// Skia spends on the order of a hundred bytes on each, so this is several
/// hundred megabytes -- already past what any visible result needs, and set
/// to clear a two-pixel hatch covering a 4000×4000 page. See
/// [`Context2D::texture_lattice`], the only thing that reads it.
const MAX_TEXTURE_TILES: f32 = 4_000_000.0;

/// The Canvas API's initial `miterLimit`.
///
/// Ten, from the HTML spec's list of initial graphics-state values. Shares
/// its number with [`DEFAULT_FONT_SIZE`] and means nothing like it: this is
/// a ratio of miter length to stroke width, that is a size in pixels.
const DEFAULT_MITER_LIMIT: f32 = 10.0;

/// The size of the Canvas API's initial font.
///
/// HTML gives it as `10px sans-serif`, which is the whole reason unstyled
/// canvas text comes out so small.
const DEFAULT_FONT_SIZE: f32 = 10.0;

pub type BoxedContext2D = JsBox<RefCell<Context2D>>;
impl Finalize for Context2D {
    /// Releases what this context told the engine it holds.
    fn finalize<'a, C: Context<'a>>(mut self, cx: &mut C) {
        self.external.release(cx);
        self.beside_page.release(cx);
    }
}

pub struct Context2D {
    pub bounds: Rect,
    /// The canvas's color space, fixed when the canvas is built. The page's
    /// image is tagged with it -- see `Context2D::image`.
    pub canvas_color_space: ColorSpace,
    /// The canvas's pixel format.
    ///
    /// Read when this page is handed to another canvas as a source, to pick
    /// the depth that image carries -- see
    /// `PageRecorder::image_flattened`. Fixed when the canvas is built;
    /// there is no setter for it, so a copy here cannot go stale.
    pub canvas_color_type: ColorType,
    /// The text contrast and gamma the canvas rasterizes glyphs with, fixed
    /// when it is built: what an export or a readback of the page takes, and
    /// what a pattern made from it rasterizes it with -- see
    /// [`CanvasOptions::text_contrast`](crate::canvas::CanvasOptions::text_contrast).
    pub(crate) canvas_text: (f32, f32),
    /// `canvas_color_space` as the one of this crate's spaces it is, fixed
    /// with it: what a draw on a page keeping two recordings converts its
    /// colour into, found once rather than by comparing Skia's hashes of
    /// each space this crate names at every draw.
    canvas_pixels: Option<PixelColorSpace>,
    /// What the surface `canvas_color_type` composites in does with a
    /// paint too faint for eight bits, fixed with it.
    faint: Faint,
    recorder: RefCell<PageRecorder>,
    /// The last exact colour a pixels recording painted with and its shader,
    /// so that a run of draws in one colour holds one shader between them
    /// rather than one each -- see [`Self::exact_paint`].
    exact_shader: RefCell<Option<(ExactColour, SkShader)>>,
    pub state: State,
    stack: Vec<State>,
    /// Parallel to `stack`: whether each saved frame opened a Skia layer
    /// (via `save_layer`). On `pop`, a layer-owning frame triggers a
    /// matching `canvas.restore()` to composite the layer.
    layers: Vec<bool>,
    pub path: PathBuilder,
    /// What the JavaScript engine has been told this context's page holds
    /// in pixels. See [`Context2D::report_resident`].
    external: External,
    /// What the JavaScript engine has been told this context holds beside
    /// its page's pixels: its recording, and [`CONTEXT_BYTES`] for itself.
    /// Apart from `external`, because the budget the pages on a thread keep
    /// to drops a page's pixels and never these. See
    /// [`Context2D::report_resident`].
    beside_page: External,
}

/// A picture-backed image with its pixels taken now.
///
/// The fallback hands back the deferred image, which is slow rather than
/// wrong: it draws the same pixels and keeps carrying its picture. Nothing
/// reaches it. `make_raster_image` on a picture image replays into a raster
/// canvas it allocates itself and needs no context to do it, and no size
/// refuses -- 1x1, 30000 square and 100000x8 all rasterize, and a zero
/// dimension is rejected before a page exists. The assertion says so where a
/// future reader would otherwise have to rediscover it.
fn flatten_image(image: &Image) -> Image {
    image
        .make_raster_image(None, skia_safe::image::CachingHint::Allow)
        .unwrap_or_else(|| {
            debug_assert!(false, "a picture-backed image must rasterize");
            image.clone()
        })
}

/// One region of `image`, as pixels.
///
/// Drawn into a surface rather than asked for with `make_subset` and
/// `make_raster_image`. Those answer `None` for a picture-backed image with no
/// context to rasterize through, and the fallback then hands back the deferred
/// image -- which is the one thing this must not do, because the whole point
/// is to stop a picture travelling any further. Silently, too: every pixel
/// still lands in the right place and only the clock says anything is wrong.
///
/// The surface takes the source image's own format, so a `display-p3` or
/// float source keeps its gamut and depth through here. `raster_n32_premul`
/// would narrow every source to eight-bit sRGB.
///
/// `picture` is replayed into that surface where there is one, and the
/// deferred image is drawn only where there is not. Both put the same bytes
/// in the surface -- hashes of the whole page match on a plain draw, a
/// rotation, a scale and a blur -- but drawing the image goes through
/// `SkBitmapDevice::drawImageRect`, which calls `getROPixels`, and that has
/// no notion of a source rectangle: it materializes the *whole* page and the
/// region is copied out of the result, so every op in the source runs
/// however little of it shows. Replaying lets Skia cull against the surface
/// bounds, which makes the cost the region's rather than the page's. On a
/// 1400-square source behind a 180x24 clip, replaying holds 0.33ms at forty
/// ops, 0.34 at two thousand and 0.38 at twenty thousand, where drawing the
/// image climbs 0.68, 5.36, 45.26.
///
/// Both paths hold the same memory -- sixty draws grow the process by 84MB
/// either way -- so the choice buys time and not footprint.
fn rasterize_region(
    image: &Image,
    picture: Option<&Picture>,
    region: IRect,
) -> Option<Image> {
    let info = image.image_info().with_dimensions(region.size());
    let mut surface = started::raster(&info, None, None)?;

    match picture {
        Some(pict) => {
            let canvas = surface.canvas();
            canvas.translate((-region.left as f32, -region.top as f32));
            canvas.draw_picture(pict, None, None);
        }
        // No picture to replay: a source reaching here by another door still
        // has to be stopped from travelling as one.
        None => {
            surface.canvas().draw_image_rect(
                image,
                Some((
                    &Rect::from(region),
                    skia_safe::canvas::SrcRectConstraint::Fast,
                )),
                Rect::from_size(region.size()),
                &Paint::default(),
            );
        }
    }
    Some(surface.image_snapshot())
}

#[derive(Clone)]
pub struct State {
    pub clip: Option<Path>,
    pub matrix: Matrix,
    pub paint: Paint,

    pub fill_style: Dye,
    pub stroke_style: Dye,
    pub shadow_blur: f32,
    /// The shadow colour, unpremultiplied, and the space it is in: the space
    /// a CSS string named, or linear-light sRGB for an `RgbaLinear`. Never
    /// quantised, so a wide colour reaches a wide canvas.
    pub shadow_color: (Color4f, ColorSpace),
    /// The shadow colour as it was declared, which `shadowColor` serialises;
    /// `None` until a CSS string sets it, and again once anything else does.
    pub shadow_declared: Option<Declaration>,
    pub shadow_offset: Point,

    pub line_dash_offset: f32,
    pub line_dash_list: Vec<f32>,
    pub line_dash_marker: Option<Path>,
    pub line_dash_fit: path_1d_path_effect::Style,

    /// The `double` the Canvas IDL specifies, not the `f32` the paint takes:
    /// it is read back through the getter, and narrowing first made `0.37`
    /// return as `0.3700000047683716`.
    pub global_alpha: f64,
    pub global_composite_operation: BlendMode,
    pub sampling_filter: SamplingFilter,
    pub filter: Filter,

    // Skia filter objects for CanvasKit parity
    pub skia_color_filter: Option<PageFilter<SkColorFilter>>,
    pub skia_image_filter: Option<PageFilter<SkImageFilter>>,
    pub skia_mask_filter: Option<SkMaskFilter>,
    pub dither: bool,

    pub font: String,
    pub font_variant: String,
    pub font_width: Width,
    /// Every setting text is laid out with, and the typography built from
    /// them -- see [`typesetting`].
    pub text: Typesetting,
    /// Whether `direction` is the initial `inherit` rather than an explicit
    /// `ltr` or `rtl`.
    ///
    /// The text settings' `graf_style` holds only the resolved direction,
    /// because that is all Skia lays text out with, so the keyword the
    /// author wrote has to be carried beside it: the Canvas standard makes
    /// `inherit` the initial value and a state the attribute reports, not a
    /// synonym for `ltr`. With no document to inherit from, `inherit`
    /// resolves to `ltr` for rendering, which is what `graf_style` gets.
    pub direction_inherits: bool,
    pub font_variation_settings: String,
}

impl Default for State {
    fn default() -> Self {
        let mut paint = Paint::default();
        paint
            .set_stroke_miter(DEFAULT_MITER_LIMIT)
            .set_color4f(Color4f::from(BLACK), &ColorSpace::new_srgb())
            .set_anti_alias(true)
            .set_stroke_width(1.0)
            .set_style(PaintStyle::Fill);

        let graf_style = paragraph_style();
        let mut char_style = TextStyle::new();
        char_style.set_font_size(DEFAULT_FONT_SIZE);

        State {
            direction_inherits: true,
            clip: None,
            matrix: Matrix::new_identity(),

            paint,
            stroke_style: Dye::Color(
                Color4f::from(BLACK),
                Some(ColorSpace::new_srgb()),
                None,
            ),
            fill_style: Dye::Color(
                Color4f::from(BLACK),
                Some(ColorSpace::new_srgb()),
                None,
            ),
            line_dash_offset: 0.0,
            line_dash_list: vec![],
            line_dash_marker: None,
            line_dash_fit: path_1d_path_effect::Style::Rotate,

            global_alpha: 1.0,
            global_composite_operation: BlendMode::SrcOver,
            sampling_filter: SamplingFilter {
                smoothing: true,
                quality: SamplingQuality::Low,
            },
            filter: Filter::default(),

            skia_color_filter: None,
            skia_image_filter: None,
            skia_mask_filter: None,
            dither: false,

            shadow_blur: 0.0,
            shadow_color: (
                Color4f::new(0.0, 0.0, 0.0, 0.0),
                ColorSpace::new_srgb(),
            ),
            shadow_declared: None,
            shadow_offset: (0.0, 0.0).into(),

            font: "10px sans-serif".to_string(),
            font_variant: "normal".to_string(),
            font_width: Width::NORMAL,
            text: Typesetting::new(TextSettings {
                char_style,
                graf_style,
                text_baseline: Baseline::Alphabetic,
                letter_spacing: Spacing::default(),
                word_spacing: Spacing::default(),
                text_decoration: DecorationStyle::default(),
                text_wrap: false,
                line_height: None,
                font_hinting: true,
                font_kerning: FontKerning::default(),
                text_rendering: TextRendering::default(),
                variations: vec![],
                oblique_angle: None,
                weight: None,
            }),
            font_variation_settings: "normal".to_string(),
        }
    }
}

/// The face the generic `serif` family resolves to at the normal weight,
/// width and slant: what an initial root font is set in, and what a canvas
/// font shorthand's parent is set in.
pub(crate) fn initial_root_face() -> Option<Typeface> {
    FontLibrary::with_shared(|library| {
        let mut collection = library.face_collection();
        collection
            .find_typefaces(
                &[generic_name(ROOT_FONT_FAMILY)],
                FontStyle::normal(),
            )
            .into_iter()
            .next()
            .or_else(|| collection.default_fallback())
    })
}

/// The family an initial root font is set in: the generic `serif`, which is
/// CSS's usual initial value and Chrome's default standard font.
const ROOT_FONT_FAMILY: GenericFamily = GenericFamily::Serif;

impl State {
    /// Whether text is drawn hinted: as `fontHinting` says, except under
    /// `textRendering = "geometricPrecision"`, which is the standard's
    /// request for unhinted outlines and outranks it -- see
    /// [`TextRendering`]. The one value every hinting decision reads, so
    /// that the collection a layout picks and the runs it lays out never
    /// disagree.
    pub fn hinted(&self) -> bool {
        self.text.font_hinting
            && self.text.text_rendering != TextRendering::GeometricPrecision
    }

    /// The typeface the context font resolves to first, as text is drawn
    /// with it.
    ///
    /// Through `FontLibrary::fonts_for_style`, so a variable face comes back
    /// instanced at the weight, width and axes this state asks for, and its
    /// metrics and advances are that instance's: Chrome reads `1ch` of bold
    /// Oswald, declared with its weight range, as 22px at 40px, the advance
    /// of the `wght` 700 zero, where the default instance's is 20.68px.
    /// `None` only when nothing at all resolves.
    pub fn primary_face(&self) -> Option<Typeface> {
        FontLibrary::with_shared(|library| {
            let (mut collection, _) =
                library.set_hinting(self.hinted()).fonts_for_style(
                    &self.text.char_style,
                    &with_style_axes(
                        &self.text.variations,
                        None,
                        self.text.weight,
                    ),
                );
            let families = self.text.char_style.font_families();
            let families: Vec<&str> = families.iter().collect();
            collection
                .find_typefaces(&families, self.text.char_style.font_style())
                .into_iter()
                .next()
                .or_else(|| collection.default_fallback())
        })
    }

    /// What the font-relative length units resolve against: the context
    /// font's size, and [`primary_face`](Self::primary_face) when a unit
    /// needs more than the size.
    pub(crate) fn font_units(&self) -> FontUnits<'_> {
        FontUnits::new(self.text.char_style.font_size(), || self.primary_face())
    }

    /// [`font_units`](Self::font_units) as `ctx.filter` reads them, with the
    /// root forms resolving against a root font rather than this one.
    ///
    /// Chrome resolves a filter's `rem` against the document root: 16px
    /// whatever the context font, so `blur(1rem)` at 40px Oswald is
    /// `blur(16px)`. This library has no document, so the root is CSS's
    /// initial one -- `ROOT_FONT_PX`, set in the generic `serif` family,
    /// which is what Chrome's root uses unless a page says otherwise. Where
    /// that family resolves to a different face than Chrome's Times, `rex`,
    /// `rch` and `rcap` follow the face this library resolves;
    /// docs/browser-differences.md says so.
    pub(crate) fn filter_units(&self) -> FontUnits<'_> {
        self.font_units()
            .with_root(FontUnits::new(ROOT_FONT_PX, initial_root_face))
    }

    /// The OpenType features `fontKerning` and `textRendering` ask of
    /// `style`, added where the style's own features do not name the tag
    /// already: a `kern`, `liga` or `clig` set through `fontVariant` or the
    /// font's features is the caller's, as `font-feature-settings` and
    /// `font-variant-ligatures` outrank these in CSS. See [`FontKerning`] and
    /// [`TextRendering`] for what each value does and where that is from.
    fn add_rendering_features(&self, style: &mut TextStyle) {
        let mut off = Vec::new();
        if self.text.font_kerning == FontKerning::None {
            off.push("kern");
        }
        if self.text.text_rendering == TextRendering::OptimizeSpeed {
            off.extend(["liga", "clig"]);
        }
        let named: Vec<String> = style
            .font_features()
            .iter()
            .map(|feature| feature.name().to_string())
            .collect();
        for tag in off {
            if !named.iter().any(|name| name == tag) {
                style.add_font_feature(tag, 0);
            }
        }
    }

    /// The typography text is laid out with: built from the text settings
    /// and the fonts registered now, or shared with the last build where
    /// neither has changed since -- see [`typesetting`].
    pub fn typography(&self) -> Rc<Typography> {
        let epoch = FontLibrary::with_shared(|library| library.epoch());
        self.text.typography(epoch, || self.build_typography())
    }

    fn build_typography(&self) -> Typography {
        let mut char_style = self.text.char_style.clone(); // use font size & style to calculate spacing
        self.add_rendering_features(&mut char_style);
        // The face the run is drawn in, as the fonts registered now resolve
        // it. The baselines are placed by its metrics, which the style reads
        // off its typeface: left as the face the font resolved to when it
        // was set, a family registered again since would place them by the
        // face it replaced while the glyphs came from the new one.
        let face = self.primary_face();
        if let Some(face) = &face {
            char_style.set_typeface(face.clone());
        }
        let units = FontUnits::new(char_style.font_size(), || face.clone());
        char_style.set_word_spacing(self.text.word_spacing.in_px(&units));
        char_style.set_letter_spacing(self.text.letter_spacing.in_px(&units));
        char_style.set_baseline_shift(
            self.text.text_baseline.get_offset(&char_style),
        );

        let mut graf_style = self.text.graf_style.clone(); // inherit align & ltr/rtl settings
        let font_families = char_style.font_families(); // consult proper metrics for height & leading defaults

        if self.text.text_wrap {
            // handle multi-line spacing
            let mut strut_style = StrutStyle::new();
            strut_style
                .set_font_families(&font_families.iter().collect::<Vec<_>>())
                .set_font_style(char_style.font_style())
                .set_font_size(char_style.font_size())
                .set_force_strut_height(true)
                .set_strut_enabled(true);

            // With no line height the face's own spacing stands. A line
            // height is in pixels, and the strut takes a multiple of the
            // font size, every line box that tall with the space beyond the
            // ascent and descent split half above and half below -- the
            // half-leading of CSS 2.1 section 10.8.1.
            let size = char_style.font_size();
            if let Some(pixels) = self.text.line_height
                && size > 0.0
            {
                strut_style
                    .set_height(pixels / size)
                    .set_height_override(true)
                    .set_half_leading(true);
            }

            graf_style.set_strut_style(strut_style);
        } else {
            // omit anything that doesn't fit on a single line
            graf_style.set_max_lines(Some(1));
        }

        // The run fonts take the hinting; the paragraph style's flag is
        // what the layout code reads to choose the collections it lays out
        // with -- see `FontLibrary::set_hinting`.
        char_style.set_font_hinting(text_hinting(self.hinted()));
        if !self.hinted() {
            graf_style.turn_hinting_off();
        }

        (
            char_style,
            graf_style,
            self.text.text_decoration.clone(),
            self.text.text_baseline,
            self.text.text_wrap,
        )
    }

    fn dye(&self, style: PaintStyle) -> &Dye {
        if style == PaintStyle::Stroke {
            &self.stroke_style
        } else {
            &self.fill_style
        }
    }

    fn texture(&self, style: PaintStyle) -> Option<&CanvasTexture> {
        match self.dye(style) {
            Dye::Texture(texture) => Some(texture),
            _ => None,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test to paint text with one painter whatever the run needs,
    /// so the two can be compared on the same scene.
    pub(crate) static PAINTER_OVERRIDE: std::cell::Cell<Option<Painter>> =
        const { std::cell::Cell::new(None) };
}

impl Context2D {
    /// A context sized to the canvas that owns it.
    ///
    /// The size is taken here rather than applied afterwards because a
    /// recorder is built with it. Constructing at the default and calling
    /// `reset_size` built one recorder and immediately replaced it: two
    /// `PictureRecorder` allocations, two `begin_recording` calls and two
    /// saves, for a page that had never been drawn on.
    pub fn new(
        canvas_color_space: ColorSpace,
        canvas_color_type: ColorType,
        canvas_text: (f32, f32),
        dims: impl Into<Size>,
    ) -> Self {
        let bounds = Rect::from_size(dims);
        let dual = records_pixels_apart(&canvas_color_space, canvas_color_type);
        let canvas_pixels = PixelColorSpace::matching(&canvas_color_space);
        let external = External::default();
        let recorder = PageRecorder::recording(bounds, dual);
        recorder.reports_through(external.reported());

        Context2D {
            bounds,
            canvas_color_space,
            canvas_color_type,
            canvas_text,
            canvas_pixels,
            faint: Faint::of(canvas_color_type),
            recorder: RefCell::new(recorder),
            exact_shader: RefCell::default(),
            path: PathBuilder::new(),
            stack: vec![],
            layers: vec![],
            state: State::default(),
            external,
            beside_page: External::default(),
        }
    }

    /// Tells the JavaScript engine what this context holds natively now, so
    /// that a collection is scheduled by the memory a canvas holds and not
    /// only by its small JavaScript object: its page's pixels, a GPU page's
    /// multisample buffer with them -- see
    /// `page::RecordingSurface::budgeted_bytes` -- its recording -- see
    /// [`PageRecorder::recorded_bytes`] -- and itself, [`CONTEXT_BYTES`].
    ///
    /// Called by the binding as the context is made, after a read, which
    /// builds the page surface or its tiles and files what was drawn, and
    /// after a resize, which drops them, and released in the finalizer. A
    /// drawing that clears the whole page drops them too and is counted at
    /// the next of those, so the engine is told too much in between and
    /// never too little; what is drawn between them is told late. The
    /// budget the pages on a thread keep to drops the pixels, for a page
    /// read only once, when another page's read takes the thread past it --
    /// see [`resident`] -- and the engine is told of that by the next report
    /// any canvas or image on the thread makes, through the count the pixels
    /// share with the page -- see `External::reported`. The recording and
    /// the context itself are reported apart, since the budget drops
    /// neither: a canvas whose page was dropped still holds them until it is
    /// finalized, and the engine is told so until then.
    pub(crate) fn report_resident<'a, C: Context<'a>>(&mut self, cx: &mut C) {
        let recorder = self.recorder.borrow();
        let (pixels, recorded) =
            (recorder.budgeted_bytes(), recorder.recorded_bytes());
        drop(recorder);
        self.external.hold(cx, pixels);
        self.beside_page.hold(cx, recorded + CONTEXT_BYTES);
    }

    pub fn in_local_coordinates(&mut self, x: f32, y: f32) -> Point {
        match self.state.matrix.invert() {
            Some(inverse) => inverse.map_point((x, y)),
            None => (x, y).into(),
        }
    }

    pub fn with_recorder<'a, F>(&'a self, f: F)
    where
        F: FnOnce(std::cell::RefMut<'a, PageRecorder>),
    {
        f(self.recorder.borrow_mut());
    }

    /// Records `f` into every recording the page keeps -- see
    /// `page::Recording`.
    pub fn with_canvas<F>(&self, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        self.with_recorder(|mut recorder| {
            recorder.append(f);
        });
    }

    /// Records a draw, in a layer of its own when it carries anything a
    /// vector backend might refuse. See [`PageRecorder::append_isolated`].
    /// `alike` where `f` records the same into each recording a page keeps,
    /// which leaves them unforked -- see [`PageRecorder::append_alike`].
    ///
    /// [`PageRecorder::append_isolated`]: page::PageRecorder::append_isolated
    /// [`PageRecorder::append_alike`]: page::PageRecorder::append_alike
    fn append_draw<F>(&self, features: VectorFeatures, alike: bool, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        self.with_recorder(|mut recorder| match (features.any(), alike) {
            (false, false) => recorder.append(f),
            (false, true) => recorder.append_alike(f),
            (true, false) => recorder.append_isolated(features, f),
            (true, true) => recorder.append_isolated_alike(features, f),
        });
    }

    pub fn with_matrix<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Matrix) -> &Matrix,
    {
        f(&mut self.state.matrix);
        self.with_recorder(|mut recorder| {
            recorder.set_matrix(self.state.matrix);
        });
    }

    /// What a draw made with `paint` would ask of a vector backend.
    ///
    /// `style` names the dye to consult -- the shader is what decides it for
    /// a fill or a stroke -- and is `None` for a draw carrying no dye of its
    /// own, an image or a nested canvas.
    pub fn vector_features(
        &self,
        paint: &Paint,
        style: Option<PaintStyle>,
    ) -> VectorFeatures {
        let mut features = VectorFeatures::PLAIN;

        // Opaque wherever it inks: a colour or an opaque shader at full
        // alpha. An image or a canvas drawn in may hold translucent pixels.
        let opaque = style.is_some()
            && paint.alpha() == u8::MAX
            && paint.shader().is_none_or(|shader| shader.is_opaque());
        features = features.with(blend_features(
            self.state.global_composite_operation,
            opaque,
        ));
        // A shadow is drawn with an image filter of its own, so it counts as
        // one whether or not the caller set `ctx.filter`.
        if paint.image_filter().is_some()
            || self.paint_for_shadow(paint).is_some()
        {
            features = features.with(VectorFeatures::IMAGE_FILTER);
        }
        if paint.mask_filter().is_some() {
            features = features.with(VectorFeatures::MASK_FILTER);
        }
        if let Some(style) = style {
            features = features.with(self.state.dye(style).vector_features());
        }
        features
    }

    /// The paint a page's pixels recording draws `paint` with, where the
    /// page keeps one: the flat colour `style` dyes with, held exactly in the
    /// page's own space, where Skia's conversion of a paint colour would land
    /// it off -- see `page::Recorders` -- and its CanvasKit colour filter
    /// built for those pixels -- see [`Self::pixels_paint`] -- folded into
    /// that colour where it can be -- see [`Self::folded_fill`]. On an
    /// eight-bit page the colour and its alpha are first placed on the
    /// bytes their exact product rounds to -- see [`ExactColour::at`] --
    /// and drawn as a plain paint colour where Skia lands that on its bytes
    /// -- see [`ExactColour::verified_paint`]. Otherwise, and on a float
    /// page, [`ExactColour::shader`] paints the colour opaque and the
    /// paint's alpha scales it.
    ///
    /// `None` on a page that keeps one recording, and where neither the dye
    /// nor the filter has a colour this crate converts into the page's
    /// space exactly.
    pub fn exact_paint(
        &self,
        paint: &Paint,
        style: PaintStyle,
    ) -> Option<Paint> {
        let filtered = self.pixels_paint(paint);
        if paint.shader().is_some() {
            return filtered;
        }
        // A flat colour through no filter is the case a run of fills is in,
        // and what is made of it depends on nothing but the colour, the
        // alpha and the kind of page: kept on the thread for the next draw
        // in the same, on this page or another -- see [`FILLS`]. A page
        // keeping one recording makes nothing of it, and is not asked.
        let key = match (self.state.dye(style), self.canvas_pixels) {
            (Dye::Color(colour, Some(space), _), Some(page))
                if filtered.is_none() && self.recorder.borrow().dual() =>
            {
                Some(FillKey::of(
                    *colour,
                    space,
                    paint.alpha_f(),
                    page,
                    self.composites_in_floats(),
                ))
            }
            _ => None,
        };
        #[cfg(test)]
        let key = key.filter(|_| KEEPS_FILLS.get());
        if let Some(key) = &key
            && let Some(made) = FILLS.with_borrow(|fills| {
                let fill = fills.get(key.hash(), key)?;
                #[cfg(test)]
                FILLS_KEPT.set(FILLS_KEPT.get() + 1);
                match fill {
                    PlacedFill::Inexact => Some(None),
                    PlacedFill::Placed(colour, space) => {
                        let mut paint = paint.clone();
                        paint.set_color4f(*colour, space);
                        Some(Some(paint))
                    }
                    PlacedFill::Shaded => None,
                }
            })
        {
            return made;
        }
        let keep = |fill: PlacedFill| {
            if let Some(key) = key.clone() {
                FILLS
                    .with_borrow_mut(|fills| fills.file(key.hash(), key, fill));
            }
        };
        let Some(exact) = self.exact_colour(style) else {
            keep(PlacedFill::Inexact);
            return filtered;
        };
        // Through a filter built for the pixels, the colour is painted by
        // its shader and the filter folded into it, as on a float page: over
        // a plain paint colour Skia folds the filter in itself, through its
        // own conversion, and on a Rec. 2020 page a `copy` of a colour on a
        // channel of 0 stored up to 3 levels there.
        if exact.bytes
            && filtered.is_none()
            && matches!(self.state.dye(style), Dye::Color(..))
            && let Some((colour, space)) =
                exact.verified_colour(paint.alpha_f())
        {
            let mut placed = paint.clone();
            placed.set_color4f(colour, &space);
            keep(PlacedFill::Placed(colour, space));
            return Some(placed);
        }
        keep(PlacedFill::Shaded);
        let (exact, alpha) = exact.at(paint.alpha_f());
        let shader = {
            let mut held = self.exact_shader.borrow_mut();
            match &*held {
                Some((colour, shader)) if *colour == exact => shader.clone(),
                _ => {
                    let Some(shader) = exact.shader() else {
                        return filtered;
                    };
                    *held = Some((exact.clone(), shader.clone()));
                    shader
                }
            }
        };
        let folds = filtered.is_some();
        let mut paint = filtered.unwrap_or_else(|| paint.clone());
        paint.set_shader(shader).set_alpha_f(alpha);
        match folds {
            true => self.folded_fill(&paint, &exact).or(Some(paint)),
            false => Some(paint),
        }
    }

    /// `exact`, a paint the page's pixels draw a flat colour, `colour`,
    /// with through a CanvasKit colour filter built for them, as a paint
    /// that draws what that filter makes of it with no filter: the filter
    /// folded into the colour -- see [`fold`]. `None` where it cannot be
    /// folded, and the paint is drawn as it is.
    fn folded_fill(
        &self,
        exact: &Paint,
        colour: &ExactColour,
    ) -> Option<Paint> {
        let key = fold::FoldKey::new(
            self.canvas_pixels?,
            self.state.skia_color_filter.as_ref()?.identity()?,
            colour.rgb,
            exact.alpha_f(),
            None,
        );
        let fold::Folded::Shader(shader) = fold::folded(key, || {
            let pixel = fold::fold_pixel(
                &self.faint().drawable(exact),
                &self.canvas_color_space,
            )?;
            fold::premul_shader(pixel, &self.canvas_color_space)
                .map(fold::Folded::Shader)
        })?
        else {
            return None;
        };
        let mut folded = exact.clone();
        folded
            .set_shader(shader)
            .set_color_filter(None)
            .set_alpha_f(1.0);
        Some(folded)
    }

    /// `paint` as a page's pixels recording draws it, where the page keeps
    /// one: its CanvasKit colour filter built for those pixels -- see
    /// [`Self::pixels_onto`]. `None` where there is no such filter, or it
    /// holds no colour, so that the pixels draw `paint` as it is.
    ///
    /// On an eight-bit page its CanvasKit image filter is rebuilt for the
    /// pixels too, which Skia runs on the paint; a float page runs one
    /// through its chain, built for the pixels by [`Self::exact_steps`].
    /// Beside a CSS filter it is composed into the paint's, and left as it
    /// is there.
    pub fn pixels_paint(&self, paint: &Paint) -> Option<Paint> {
        let onto = self.pixels_onto()?;
        let colour = self
            .state
            .skia_color_filter
            .as_ref()
            .filter(|filter| filter.depends());
        let image = self.state.skia_image_filter.as_ref().filter(|filter| {
            filter.depends() && onto.bytes && self.state.filter.is_none()
        });
        if colour.is_none() && image.is_none() {
            return None;
        }
        let mut paint = paint.clone();
        if let Some(filter) = colour {
            paint.set_color_filter(filter.on(Some(&onto)));
        }
        if let Some(filter) = image {
            paint.set_image_filter(filter.on(Some(&onto)));
        }
        Some(paint)
    }

    /// The paint a page's pixels recording draws text in `paint` with, where
    /// the page keeps one, its CanvasKit colour filter built for those
    /// pixels -- see [`Self::pixels_paint`]: on an eight-bit sRGB page the
    /// colour nearest its own that stores the exact product -- see
    /// [`ExactColour::nearest_text_paint`] -- on any other eight-bit page the
    /// placed colour as a plain paint colour where Skia lands it and the
    /// glyph contrast stays -- see [`ExactColour::verified_text_paint`] --
    /// and otherwise `paint` over a colour shader of its own colour, opaque,
    /// with [`ExactColour::dye`] replacing that colour -- or, before a
    /// colour filter `paint` already carries,
    /// [`ExactColour::dye_keeping_plain_luminance`]. Without a shader Skia
    /// folds a colour filter into the paint's colour, through the
    /// conversion the dye exists to avoid; and a colour shader answers the
    /// luminance Skia sets a glyph mask's contrast from, which the shader
    /// [`ExactColour::shader`] makes does not.
    pub fn exact_text_paint(
        &self,
        paint: &Paint,
        style: PaintStyle,
    ) -> Option<Paint> {
        let filtered = self.pixels_paint(paint);
        if paint.shader().is_some() {
            return filtered;
        }
        let Some(exact) = self.exact_colour(style) else {
            return filtered;
        };
        match &filtered {
            Some(pixels) => {
                self.exact_text_paint_in(pixels, exact, paint.color_filter())
            }
            None => self.exact_text_paint_in(paint, exact, None),
        }
        .or(filtered)
    }

    /// [`Self::exact_text_paint`] in `exact`, the colour held exactly in the
    /// page's own space. Where the colour filter `paint` carries is built for
    /// the page's pixels, `plain_filter` is the one it was built from, whose
    /// glyph contrast the text keeps -- see
    /// [`ExactColour::keeping_luminance_of`].
    fn exact_text_paint_in(
        &self,
        paint: &Paint,
        exact: ExactColour,
        plain_filter: Option<SkColorFilter>,
    ) -> Option<Paint> {
        if paint.shader().is_some() {
            return None;
        }
        if exact.bytes
            && let Some(placed) = exact
                .nearest_text_paint(paint)
                .or_else(|| exact.verified_text_paint(paint))
        {
            return Some(placed);
        }
        let (exact, alpha) = exact.at(paint.alpha_f());
        let mut opaque = paint.color4f();
        opaque.a = 1.0;
        let dye = match paint.color_filter() {
            // Their filter follows the dye and could move its glyph
            // contrast into another bucket from the plain colour's, so the
            // dye is the one that keeps the plain colour's whatever it is.
            Some(theirs) => {
                let filtered = color_filters::compose(
                    &theirs,
                    &exact.dye_keeping_plain_luminance(opaque)?,
                )?;
                match &plain_filter {
                    Some(plain) => {
                        exact.keeping_luminance_of(filtered, plain, opaque)?
                    }
                    None => filtered,
                }
            }
            None => exact.dye(opaque)?,
        };
        let mut paint = paint.clone();
        paint.set_shader(layer::plain_colour_shader(opaque));
        paint.set_color_filter(dye).set_alpha_f(alpha);
        if let Some(plain) = &plain_filter
            && let Some(folded) =
                self.folded_text(&paint, &exact, plain, opaque)
        {
            paint.set_color_filter(folded);
        }
        Some(paint)
    }

    /// The colour filter for `paint`, text in a flat colour, `exact`,
    /// through a CanvasKit colour filter built for the page's pixels -- the
    /// one `paint` carries -- with that filter folded into the colour: what
    /// it makes of the colour, whatever the glyph, keeping the glyph
    /// contrast `plain_filter` gives `opaque` -- see
    /// [`ExactColour::keeping_luminance_of`] and [`fold`]. `None` where it
    /// cannot be folded.
    fn folded_text(
        &self,
        paint: &Paint,
        exact: &ExactColour,
        plain_filter: &SkColorFilter,
        opaque: Color4f,
    ) -> Option<SkColorFilter> {
        let key = fold::FoldKey::new(
            self.canvas_pixels?,
            self.state.skia_color_filter.as_ref()?.identity()?,
            exact.rgb,
            paint.alpha_f(),
            Some(opaque),
        );
        let fold::Folded::Filter(filter) = fold::folded(key, || {
            let pixel = fold::fold_pixel(
                &self.faint().drawable(paint),
                &self.canvas_color_space,
            )?;
            let constant = fold::constant_filter(pixel)?;
            exact
                .keeping_luminance_of(constant, plain_filter, opaque)
                .map(fold::Folded::Filter)
        })?
        else {
            return None;
        };
        Some(filter)
    }

    /// `paint`, built with a paragraph before the page it lands on was
    /// known, as this page draws it in `recording`, given `colour`, what
    /// it paints as its style named it -- see
    /// `node::oblique::Resolve`. On a page that keeps a recording for
    /// pixels, that recording paints the colour held exactly in the page's
    /// space, as text in a fill colour does, and the document recording
    /// beside it the colour sRGB-encoded on this side, which Skia then
    /// converts by the identity onto sRGB -- see [`skia_paint_color_onto`];
    /// as does a float page that keeps one recording. Every other page
    /// draws `paint` as it was built.
    pub(crate) fn paragraph_paint(
        &self,
        paint: &Paint,
        colour: Option<&NamedColour>,
        recording: Recording,
    ) -> Paint {
        let Some((colour, named_in)) = colour else {
            return paint.clone();
        };
        match recording {
            Recording::Pixels => self
                .canvas_pixels
                .and_then(|canvas| {
                    let rgb = color_in_canvas_space(*colour, named_in, canvas)?;
                    Some(ExactColour::new(
                        rgb,
                        self.canvas_color_space.clone(),
                        canvas,
                        !self.composites_in_floats(),
                    ))
                })
                .and_then(|exact| {
                    let mut plain = paint.clone();
                    plain.set_color4f(*colour, Some(named_in));
                    self.exact_text_paint_in(&plain, exact, None)
                })
                .unwrap_or_else(|| paint.clone()),
            Recording::Document => self.encoded(paint, *colour, named_in),
            Recording::Shared if self.composites_in_floats() => {
                self.encoded(paint, *colour, named_in)
            }
            Recording::Shared => paint.clone(),
        }
    }

    /// `paint` in `colour`, named in `space`, as a paint's colour is handed
    /// to Skia for this page -- see [`skia_paint_color_onto`].
    fn encoded(
        &self,
        paint: &Paint,
        colour: Color4f,
        space: &ColorSpace,
    ) -> Paint {
        let (colour, space) =
            skia_paint_color_onto(colour, space, &self.canvas_color_space);
        let mut paint = paint.clone();
        paint.set_color4f(colour, Some(&space));
        paint
    }

    /// Whether a paragraph is drawn run by run in paints made for this
    /// page, rather than in the paints it was built with -- see
    /// [`Self::paragraph_paint`]: on a page that composites in floats, and
    /// on one that keeps a recording for pixels.
    pub(crate) fn makes_paragraph_paints(&self) -> bool {
        self.composites_in_floats() || self.records_pixels_apart()
    }

    /// Whether this page keeps a recording for its pixels apart from its
    /// document -- see [`records_pixels_apart`]. Asked of the page's space
    /// and type rather than of the recorder, which a draw being recorded
    /// holds.
    fn records_pixels_apart(&self) -> bool {
        records_pixels_apart(&self.canvas_color_space, self.canvas_color_type)
    }

    /// The flat colour `style` dyes with -- a colour, or a texture's mark --
    /// held exactly in the page's own space, on a page that keeps a
    /// recording for pixels -- see [`Self::exact_paint`].
    pub fn exact_colour(&self, style: PaintStyle) -> Option<ExactColour> {
        if !self.recorder.borrow().dual() {
            return None;
        }
        let (color, space) = match self.state.dye(style) {
            Dye::Color(color, Some(space), _) => (*color, space.clone()),
            Dye::Texture(texture) => texture.color(),
            _ => return None,
        };
        let canvas = self.canvas_pixels?;
        let rgb = color_in_canvas_space(color, &space, canvas)?;
        Some(ExactColour::new(
            rgb,
            self.canvas_color_space.clone(),
            canvas,
            !self.composites_in_floats(),
        ))
    }

    /// `color`, unpremultiplied in `space`, converted exactly into the page's
    /// space and placed on the bytes an eight-bit page stores it as -- see
    /// [`ExactColour::at`] -- tagged with the page's space, on a page that
    /// keeps a recording for pixels; `None` elsewhere. Asked of the page's
    /// space rather than of the recorder, which a draw being recorded holds.
    fn placed_colour(
        &self,
        color: Color4f,
        space: &ColorSpace,
    ) -> Option<(Color4f, ColorSpace)> {
        if !keeps_two_recordings(&self.canvas_color_space) {
            return None;
        }
        let canvas = self.canvas_pixels?;
        let rgb = color_in_canvas_space(color, space, canvas)?;
        let exact = ExactColour::new(
            rgb,
            self.canvas_color_space.clone(),
            canvas,
            true,
        );
        let (placed, alpha) = exact.at(color.a);
        let [r, g, b] = placed.rgb;
        Some((
            Color4f::new(r, g, b, alpha),
            self.canvas_color_space.clone(),
        ))
    }

    /// Records the draw `f` makes with a paint it is handed, through the
    /// route its dye and filter take. `replay`, where the draw's kind can be
    /// kept whole, is what a canvas's pattern is drawn through at playback
    /// -- see [`Replay`]. `text` where the draw is a run of text -- see
    /// [`TextRun`]. `gradient` where the dye is a gradient, its description,
    /// from which a GPU canvas draws it in the CPU's bytes -- see
    /// [`GradientDye`]. `f` is told which [`Pass`] it draws.
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_canvas<F>(
        &self,
        paint: &Paint,
        exact: Option<&Paint>,
        features: VectorFeatures,
        replay: Option<Replay>,
        text: Option<TextRun>,
        gradient: Option<GradientDye<'_>>,
        f: F,
    ) where
        F: Fn(&SkCanvas, &Paint, Pass),
    {
        let lines = text.as_ref().is_some_and(|text| text.lines);
        if self.files_through_pattern(paint, features, replay, lines, &f)
            || self.files_through_chain(
                paint,
                exact,
                features,
                text.as_ref(),
                &f,
                None,
            )
        {
            return;
        }
        let paint_for = |recording: Recording| match (recording, exact) {
            (Recording::Pixels, Some(exact)) => exact,
            _ => paint,
        };
        // A run holding colour glyphs casts its shadow where it is filed --
        // see `Self::files_as_colour_text`.
        let colour_run =
            text.as_ref().is_some_and(|text| text.colour.is_some());
        let cast_apart = !colour_run
            && self.files_shadow_through_chain(
                paint,
                features,
                text.as_ref(),
                &f,
            );
        // A draw under a filter of its own whose shadow is filed apart is
        // run through the filter's chain, as it is casting none.
        if cast_apart
            && paint.image_filter().is_some()
            && self.files_through_chain(
                paint,
                exact,
                features,
                text.as_ref(),
                &f,
                self.exact_steps(),
            )
        {
            return;
        }
        let casts = self.casts_shadow() && !cast_apart;
        // Which recording it is reaches the draw only through `paint_for`
        // and the shadow's paint, so with no shadow and no exact paint apart
        // from the plain one it draws the same in each -- see
        // `PageRecorder::append_alike`.
        let alike = !casts && exact.is_none_or(|e| e == paint);
        let render_shadow =
            |canvas: &SkCanvas, paint: &Paint, recording: Recording| {
                if let Some((shadow_paint, pass)) = casts
                    .then(|| self.paint_for_shadow_in(paint, recording, lines))
                    .flatten()
                {
                    canvas.save();
                    canvas.set_matrix(
                        &Matrix::translate(self.state.shadow_offset).into(),
                    );
                    canvas.concat(&self.state.matrix);
                    f(canvas, &shadow_paint, pass);
                    canvas.restore();
                }
            };

        let beyond =
            covers_beyond_the_draw(self.state.global_composite_operation);
        // The draw into one recording, under the canvas's transform, in
        // `drawn` and casting its shadow from `cast`: for blend modes that
        // affect regions of the canvas outside of the bounds of the object
        // being drawn, through an intermediate picture, one for each
        // recording, each with that recording's paint.
        let draw_in = |canvas: &SkCanvas,
                       recording: Recording,
                       drawn: &Paint,
                       cast: &Paint| match beyond {
            true => {
                let mut layer_paint = drawn.clone();
                layer_paint.set_blend_mode(BlendMode::SrcOver);
                let mut layer_recorder = PictureRecorder::new();
                layer_recorder.begin_recording(self.bounds, true);
                if let Some(layer) = layer_recorder.recording_canvas() {
                    // draw the dropshadow (if applicable)
                    render_shadow(layer, &layer_paint, recording);
                    // draw normally
                    layer.set_matrix(&self.state.matrix.into());
                    f(layer, &layer_paint, Pass::Draw);
                }

                // transfer the picture contents to the canvas in a single
                // operation, applying the blend mode to the whole canvas
                // (regardless of the bounds of the text/path being drawn)
                if let Some(pict) =
                    layer_recorder.finish_recording_as_picture(None)
                {
                    canvas.save();
                    canvas.set_matrix(&Matrix::new_identity().into());
                    let mut blend_paint = Paint::default();
                    blend_paint.set_anti_alias(true);
                    blend_paint
                        .set_blend_mode(self.state.global_composite_operation);
                    canvas.draw_picture(&pict, None, Some(&blend_paint));
                    canvas.restore();
                }
            }
            false => {
                // draw the dropshadow (if applicable)
                render_shadow(canvas, cast, recording);
                // draw with the normal paint
                f(canvas, drawn, Pass::Draw);
            }
        };
        let draw = |canvas: &SkCanvas, recording: Recording| {
            draw_in(canvas, recording, paint_for(recording), paint)
        };
        if let Some(run) = text.as_ref().and_then(|text| text.colour.clone())
            && self
                .files_as_colour_text(paint, exact, features, run, lines, &draw)
        {
            return;
        }
        // A shadow filed as a group of its own leaves a draw with no filter
        // of its own nothing an image filter does: it asked for one only for
        // the shadow -- see `Self::vector_features` -- and counted as a
        // filtered entry, a GPU playback paced its flushes by the draw's ops
        // as well as by the shadow's group.
        let features = match cast_apart && paint.image_filter().is_none() {
            true => features.without(VectorFeatures::IMAGE_FILTER),
            false => features,
        };
        // A draw dyed by a gradient on an eight-bit page is filed as a
        // `GradientDraw`, which a GPU canvas draws in the CPU's bytes -- see
        // `layer::GradientDraw`. A float page is drawn on the CPU whichever
        // engine is asked for -- see `RenderingEngine::for_canvas` -- and
        // has nothing to choose.
        #[cfg(test)]
        let gradient = gradient.filter(|_| FILES_GRADIENTS.get());
        if let Some(dye) = gradient.filter(|_| !self.composites_in_floats()) {
            let dual = self.records_pixels_apart();
            let pixels = match dual {
                true => Recording::Pixels,
                false => Recording::Shared,
            };
            // Recorded for a document only where it draws apart from the
            // pixels -- a shadow, or an exact paint of its own; a piece with
            // no document plays its pixels for one, as a page's recordings
            // that have not forked do -- see `PageRecorder::append_alike`.
            // One recorded for every draw was 15% of a CPU canvas's time
            // filling 2000 small gradients.
            let document = || {
                (dual && !alike)
                    .then(|| {
                        self.recorded(&|canvas| {
                            draw(canvas, Recording::Document)
                        })
                    })
                    .flatten()
            };
            // A path the effect reproduces is kept rather than recorded.
            let piece = match self.gradient_path(
                &dye,
                paint,
                paint_for(pixels),
                beyond,
            ) {
                Some(path) => layer::GradientPiece {
                    pixels: layer::GradientPixels::Path(path.clone()),
                    document: document(),
                    gpu: layer::OnGpu::Path(path),
                },
                None => {
                    let Some(native) =
                        self.recorded(&|canvas| draw(canvas, pixels))
                    else {
                        return;
                    };
                    let gpu = self.gradient_on_gpu(
                        &dye,
                        paint,
                        paint_for(pixels),
                        beyond,
                        native.cull_rect(),
                        &|canvas, paint| draw_in(canvas, pixels, paint, paint),
                        &|canvas, paint| render_shadow(canvas, paint, pixels),
                        &|canvas, paint| f(canvas, paint, Pass::Draw),
                    );
                    let Some(gpu) = gpu else {
                        return;
                    };
                    layer::GradientPiece {
                        pixels: layer::GradientPixels::Recorded(native),
                        document: document(),
                        gpu,
                    }
                }
            };
            let op = layer::GradientDraw {
                draws: vec![piece],
                clip: None,
            };
            self.with_recorder(|mut recorder| {
                recorder.append_gradient(
                    Arc::new(op),
                    self.state.clip.as_ref(),
                    features,
                );
            });
            return;
        }
        self.append_draw(features, alike, draw);
    }

    /// Whether a draw made now casts its shadow through a chain -- see
    /// [`Self::files_shadow_through_chain`]: one casting a shadow, under no
    /// CanvasKit filter, through a blend that covers no more than it.
    fn casts_shadow_through_chain(&self) -> bool {
        self.casts_shadow()
            && self.state.skia_image_filter.is_none()
            && !covers_beyond_the_draw(self.state.global_composite_operation)
    }

    /// Whether a run of text drawn now is played through a chain, its own
    /// filter's or the one its shadow is cast through, and so answers what
    /// it can ink, which bounds the chain's layer: a picture of text is
    /// bounded by the clip, and a shadow's chain run over the page's every
    /// pixel took about 3 ms a line of 28px text on the CPU, where one
    /// bounded by the line's ink takes about 0.15.
    fn chains_text(&self) -> bool {
        self.draw_chain(true, false).is_some()
            || self.casts_shadow_through_chain()
    }

    /// Files a draw's `shadowColor` shadow as a group of its own, cast
    /// through a chain on pixels -- see [`Self::shadow_steps`] -- before
    /// the draw: the draw recorded in the paint it casts in, once for each
    /// recording the page keeps, under the transform it is made under, and
    /// the group moved by the shadow's offset. Through Skia's shadow filter
    /// the blur was three box blurs from a standard deviation of 2 on the
    /// CPU, up to 8.7 levels off the Gaussian, and Ganesh's scaled-down
    /// blur past 4 on the GPU. A document composites the group through
    /// Skia's shadow filter, as it drew the shadow before.
    ///
    /// A draw under a CSS filter casts the shadow of what the filter makes,
    /// as the HTML Standard orders the two: the chain runs the filter's
    /// steps and then the shadow's, on the draw recorded without the filter,
    /// and a document composites the group through Skia's shadow filter
    /// taking the CSS filter as its input. Through Skia's layer, the filter's
    /// blur and the shadow's were each Skia's approximation.
    ///
    /// `false`, filing nothing, for a draw casting no shadow; for one under
    /// a CanvasKit filter, which Skia's layer runs in the draw's own units;
    /// and for one under a blend that covers beyond it, which
    /// `render_to_canvas` composites with its shadow through one layer.
    fn files_shadow_through_chain<F>(
        &self,
        paint: &Paint,
        features: VectorFeatures,
        text: Option<&TextRun>,
        f: &F,
    ) -> bool
    where
        F: Fn(&SkCanvas, &Paint, Pass),
    {
        if !self.casts_shadow_through_chain() {
            return false;
        }
        // The filter's steps the shadow is cast after, where the draw is
        // under one.
        let filter = match paint.image_filter() {
            Some(_) => match self.exact_steps() {
                Some(steps) => steps,
                None => return false,
            },
            None => Arc::from([]),
        };
        let lines = text.is_some_and(|text| text.lines);
        let dual = self.recorder.borrow().dual();
        // The recording the shadow's pixels and its document are each cast
        // in: the page's two where it keeps two, and its one otherwise.
        let (pixels, document) = match dual {
            true => (Recording::Pixels, Recording::Document),
            false => (Recording::Shared, Recording::Shared),
        };
        let (Some(pixels_cast), Some(document_cast)) = (
            self.shadow_cast_in(paint, pixels, lines),
            self.shadow_cast_in(paint, document, lines),
        ) else {
            return false;
        };
        let steps: Arc<[Step]> = filter
            .iter()
            .cloned()
            .chain(self.shadow_steps(&pixels_cast).iter().cloned())
            .collect();
        let offset = self.state.shadow_offset;
        // Recorded over the page, as far again as the blur reaches, and
        // back along the offset, which brings a draw off the page onto it.
        let bounds = self.bounds.with_outset(
            canvas_reach(&steps, &self.state.matrix)
                + Vector::new(offset.x.abs(), offset.y.abs()),
        );
        let reached = text.map(|text| text.reached);
        let record = |cast: &CanvasShadow| {
            let mut drawn = cast.paint.clone();
            drawn
                .set_image_filter(None)
                .set_blend_mode(BlendMode::SrcOver);
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            if let Some(reached) = reached {
                reached.set(None);
            }
            f(canvas, &drawn, cast.pass);
            let reach = reached.and_then(Cell::get);
            recorder
                .finish_recording_as_picture(None)
                .map(|picture| (picture, reach))
        };
        let entry = match dual {
            false => record(&pixels_cast)
                .map(|(picture, reach)| (Layer::Picture(picture), reach)),
            true => record(&pixels_cast).zip(record(&document_cast)).map(
                |((pixels, pixels_reach), (document, document_reach))| {
                    let reach =
                        pixels_reach.zip(document_reach).map(|(mut a, b)| {
                            a.join(b);
                            a
                        });
                    (Layer::Targeted(Arc::new((pixels, document))), reach)
                },
            ),
        };
        let Some((entry, reach)) = entry else {
            return false;
        };
        // The group composites through the blend, and a document through
        // Skia's shadow filter in the colour it names, of what the draw's
        // own filter makes.
        let mut through = Paint::default();
        through
            .set_anti_alias(true)
            .set_blend_mode(self.state.global_composite_operation)
            .set_image_filter(image_filters::drop_shadow_only(
                (0.0, 0.0),
                (document_cast.sigma.x, document_cast.sigma.y),
                document_cast.colour,
                document_cast.space.clone(),
                paint.image_filter(),
                None,
            ));
        // The draw inside asks for what its paint does, less the filter and
        // the blend, which the group composites through.
        let drawn = features.without(
            VectorFeatures::IMAGE_FILTER
                .with(VectorFeatures::BLEND_MODE)
                .with(VectorFeatures::UNMATCHED_BLEND)
                .with(VectorFeatures::MASKED_BLEND),
        );
        self.with_recorder(|mut recorder| {
            recorder.append_group(
                Composite::Chain {
                    steps,
                    rec: LayerRec {
                        paint: Some(through),
                        ..LayerRec::default()
                    },
                    reach,
                    coverage: None,
                },
                None,
                Matrix::new_identity(),
                [(entry, drawn)].into_iter().collect(),
                features,
                offset,
            );
        });
        true
    }

    /// `draw`, recorded on its own under the canvas's transform.
    fn recorded(&self, draw: &dyn Fn(&SkCanvas)) -> Option<Picture> {
        let mut recorder = PictureRecorder::new();
        let canvas = recorder.begin_recording(self.bounds, true);
        canvas.set_matrix(&self.state.matrix.into());
        draw(canvas);
        recorder.finish_recording_as_picture(None)
    }

    /// How a GPU canvas draws a draw made with `paint`, dyed by `dye`, in
    /// the bytes the CPU draws it in -- see `layer::OnGpu`: with the effect
    /// where the effect reproduces the draw -- see [`Self::takes_the_effect`]
    /// -- made where the page is played for a path, which is kept -- see
    /// `layer::GradientPath` -- and now, over what the draw reaches,
    /// `reach`, in device pixels, for anything else -- see
    /// [`Self::gradient_effect`]; and otherwise made on the CPU, each layer
    /// of it composited through the
    /// blend by itself -- the draw's shadow, then the draw; and under a
    /// blend that changes what lies `beyond` the draw, the two in one layer,
    /// as a draw through such a blend is composited.
    ///
    /// `whole` draws with the given paint what the draw records, its shadow
    /// included; `shadow` and `shape` its shadow and itself. `pixels` is
    /// the paint the page's pixels recording draws with.
    #[allow(clippy::too_many_arguments)]
    fn gradient_on_gpu(
        &self,
        dye: &GradientDye<'_>,
        paint: &Paint,
        pixels: &Paint,
        beyond: bool,
        reach: Rect,
        whole: &dyn Fn(&SkCanvas, &Paint),
        shadow: &dyn Fn(&SkCanvas, &Paint),
        shape: &dyn Fn(&SkCanvas, &Paint),
    ) -> Option<layer::OnGpu> {
        let mut over = pixels.clone();
        over.set_blend_mode(BlendMode::SrcOver);
        let blend = self.state.global_composite_operation;
        if let Some(effect) = self.gradient_effect(dye, paint, reach) {
            // Recorded once more with `SrcOver` only where the draw blends
            // otherwise, for a layer to composite through its blend.
            let over = match blend {
                BlendMode::SrcOver => None,
                _ => Some(self.recorded(&|canvas| shape(canvas, &over))?),
            };
            return self.recorded(&|canvas| whole(canvas, &effect)).map(
                |picture| layer::OnGpu::Effect {
                    picture,
                    over,
                    blend,
                    beyond,
                },
            );
        }
        let mut cast = paint.clone();
        cast.set_blend_mode(BlendMode::SrcOver);
        let layers: Vec<Picture> = match beyond {
            true => self
                .recorded(&|canvas| {
                    shadow(canvas, &over);
                    shape(canvas, &over);
                })
                .into_iter()
                .collect(),
            false => [
                self.casts_shadow()
                    .then(|| self.recorded(&|canvas| shadow(canvas, &cast)))
                    .flatten(),
                self.recorded(&|canvas| shape(canvas, &over)),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        Some(layer::OnGpu::Raster {
            layers,
            blend,
            beyond,
        })
    }

    /// `dye`'s path kept as what it is made from -- see `layer::GradientPath`
    /// -- where the draw is a path the effect reproduces, through a blend
    /// that leaves what lies beyond it alone, drawn with `paint` on the GPU
    /// and `pixels` on the CPU: `None` for one that is not, or reaches
    /// nothing on the page, which is then recorded.
    fn gradient_path(
        &self,
        dye: &GradientDye<'_>,
        paint: &Paint,
        pixels: &Paint,
        beyond: bool,
    ) -> Option<Arc<layer::GradientPath>> {
        let (path, bounds) = dye.path?;
        // A blend that changes what lies beyond the draw composites a
        // recording of it over the whole page -- see `render_to_canvas` --
        // which a path drawn through the blend does not.
        if beyond || !self.takes_the_effect(paint) {
            return None;
        }
        let area =
            IRect::intersect(&bounds.round_out(), &self.bounds.round_out())?;
        let mut drawn = paint.clone();
        drawn
            .set_shader(None)
            .set_alpha_f(1.0)
            .set_anti_alias(paint.is_anti_alias() && !dye.whole_pixels);
        Some(Arc::new(layer::GradientPath {
            path: Mutex::new(path.clone()),
            bounds,
            matrix: self.state.matrix,
            paint: drawn,
            pixels: self.faint().drawable(pixels).into_owned(),
            spec: dye.spec.clone(),
            page: self.canvas_color_space.clone(),
            alpha: paint.alpha_f(),
            area,
            whole_pixels: dye.whole_pixels,
            blend: self.state.global_composite_operation,
            beyond,
        }))
    }

    /// The paint the effect draws `dye` in on a GPU canvas, for a draw that
    /// is not a path -- a run of text -- in the bytes the CPU stores for it,
    /// over what the draw reaches on the page, `reach`, in device pixels --
    /// see [`GradientSpec::sampled`]: `None` where the draw goes through
    /// anything the effect does not reproduce -- see
    /// [`Self::takes_the_effect`] -- which the CPU then draws -- see
    /// `layer::OnGpu::Raster` -- or reaches nothing on the page. A path is
    /// kept instead and its effect made where the page is played -- see
    /// `layer::GradientPath`.
    fn gradient_effect(
        &self,
        dye: &GradientDye<'_>,
        paint: &Paint,
        reach: Rect,
    ) -> Option<Paint> {
        if !self.takes_the_effect(paint) {
            return None;
        }
        let area =
            IRect::intersect(&reach.round_out(), &self.bounds.round_out())?;
        let shader = dye.spec.sampled(
            &self.canvas_color_space,
            paint.alpha_f(),
            &self.state.matrix,
            area,
        )?;
        let mut effect = paint.clone();
        effect.set_shader(shader).set_alpha_f(1.0);
        Some(effect)
    }

    /// Whether the effect reproduces what the CPU draws with `paint`.
    ///
    /// The effect hands the GPU the bytes the CPU's eight-bit pipeline
    /// (`lowp`) makes of the gradient. A filter, a mask, dither or a shadow
    /// works on the gradient's colours in the CPU's float pipeline or in
    /// Ganesh's half precision, and a blend `lowp` has no stage for -- see
    /// [`blends_in_eight_bits`] -- sends the whole draw through the float
    /// pipeline, which rounds once, after blending.
    fn takes_the_effect(&self, paint: &Paint) -> bool {
        !(self.casts_shadow()
            || paint.color_filter().is_some()
            || paint.image_filter().is_some()
            || paint.mask_filter().is_some()
            || paint.is_dither()
            || !blends_in_eight_bits(
                paint.as_blend_mode().unwrap_or(BlendMode::SrcOver),
            ))
    }

    /// Files a run of text holding colour glyphs as a
    /// [`ColourText`](layer::ColourText), which draws the run again where
    /// the page is played back on pixels, its colour glyphs made there for
    /// those pixels: `run`, the run kept whole, with its shadow and through
    /// the blend that covers beyond it, as `draw` -- the run's draw into one
    /// recording, as `render_to_canvas` records it -- draws them. Recorded
    /// as well, from the run, in the paints `draw` takes for each recording
    /// the page keeps, when something first asks -- see
    /// `layer::ColourText::recorded`; a test records them now, by `draw`,
    /// to hold the one to the other.
    fn files_as_colour_text<D>(
        &self,
        paint: &Paint,
        exact: Option<&Paint>,
        features: VectorFeatures,
        run: Replay,
        lines: bool,
        draw: &D,
    ) -> bool
    where
        D: Fn(&SkCanvas, Recording),
    {
        let dual = self.records_pixels_apart();
        let pixels = match dual {
            true => Recording::Pixels,
            false => Recording::Shared,
        };
        // The paints `draw` draws the run in for `recording` -- see
        // `render_to_canvas`: through a layer the blend composites where it
        // covers beyond the draw, the run's own over it there, and its
        // shadow cast from that.
        let blend = self.state.global_composite_operation;
        let beyond = covers_beyond_the_draw(blend);
        let paints = |recording: Recording| {
            let mut drawn = match recording {
                Recording::Pixels => exact.unwrap_or(paint).clone(),
                _ => paint.clone(),
            };
            if beyond {
                drawn.set_blend_mode(BlendMode::SrcOver);
            }
            let cast_from = if beyond { &drawn } else { paint };
            let shadow = self
                .paint_for_shadow_in(cast_from, recording, lines)
                .map(|(shadow, pass)| {
                    let replay = match pass {
                        Pass::Draw => run.clone(),
                        Pass::AlphaInShadow => run.opaque(),
                    };
                    layer::ColourShadow {
                        paint: shadow,
                        replay,
                        offset: self.state.shadow_offset,
                    }
                });
            (drawn, shadow)
        };
        let (drawn, shadow) = paints(pixels);
        #[cfg_attr(not(test), expect(unused_mut))]
        let mut recording = LazyPicture::default();
        #[cfg_attr(not(test), expect(unused_mut))]
        let mut document = dual.then(|| {
            let (paint, shadow) = paints(Recording::Document);
            layer::ColourRecording::later(paint, shadow)
        });
        #[cfg(test)]
        if COLOUR_RECORDED_AT_ONCE.get() {
            let record = |recording: Recording| {
                let mut recorder = PictureRecorder::new();
                let canvas = recorder.begin_recording(self.bounds, true);
                canvas.set_matrix(&self.state.matrix.into());
                draw(canvas, recording);
                recorder.finish_recording_as_picture(None)
            };
            let Some(recorded) = record(pixels) else {
                return false;
            };
            recording = LazyPicture::from(Some(recorded));
            if dual {
                let Some(recorded) = record(Recording::Document) else {
                    return false;
                };
                document = Some(layer::ColourRecording::recorded(recorded));
            }
        }
        #[cfg(not(test))]
        let _ = draw;
        let text = Arc::new(layer::ColourText {
            matrix: self.state.matrix,
            replay: Some(run),
            paint: drawn,
            inked: Rect::new_empty(),
            shadow,
            blend: beyond.then_some(blend),
            recording,
            bounds: self.bounds,
            document,
            clip: None,
            #[cfg(test)]
            watch: layer::COLOUR_WATCH.with_borrow(Clone::clone),
        });
        self.with_recorder(|mut recorder| {
            recorder.append_colour_text(
                text,
                self.state.clip.as_ref(),
                features,
            );
        });
        true
    }

    /// Files a draw dyed by a canvas's pattern as a
    /// [`PatternDraw`](layer::PatternDraw), which rasterizes the canvas's
    /// page where the draw is played, as the page composites itself -- see
    /// [`TileSource`](layer::TileSource) -- and paints it where the draw
    /// covers. Kept as Skia's picture shader, the pattern drew the page's
    /// blurs made at the density of its picture and resampled to any other
    /// -- a `blur(2px)` read at density 2 parted from the page by up to 16
    /// levels -- and on a platform whose native colour type is not the
    /// RGBA8888 Skia rasterizes the tile in, every translucent edge a level
    /// apart.
    ///
    /// A draw that composites only where it lands, through no filter and
    /// casting no shadow, is the tile alone, filed under the frame's clip as
    /// a blur is. Any other is held in a group, which composites through the
    /// draw's filter and blend mode as a canvas drawn through a paint does
    /// -- see [`Self::draw_picture_costing`] -- beside a group of its own
    /// for its shadow, whose pixels on an eight-bit sRGB page hold the draw
    /// without the `globalAlpha` its shadow's colour takes; under a blend
    /// that covers beyond the draw both are held in one more, which the
    /// blend composites, as
    /// [`Self::render_to_canvas`] holds them in one picture.
    ///
    /// `false`, filing nothing, for a draw dyed any other way.
    fn files_through_pattern<F>(
        &self,
        paint: &Paint,
        features: VectorFeatures,
        replay: Option<Replay>,
        lines: bool,
        f: &F,
    ) -> bool
    where
        F: Fn(&SkCanvas, &Paint, Pass),
    {
        // An image's paint carries no dye, whatever the fill style is.
        let Dye::Pattern(pattern) = self.state.dye(paint.style()) else {
            return false;
        };
        if paint.shader().is_none() {
            return false;
        }
        let Some((source, pattern_matrix, repeat)) = pattern.tiling() else {
            return false;
        };
        // A draw whose kind is not kept whole takes Skia's picture shader;
        // none that carries a dye is such a draw.
        let Some(replay) = replay else {
            return false;
        };
        // Its shadow is a group of its own, so the draw is run through the
        // chain it would be casting none; it has no `BlurOp`, so a `blur()`
        // alone is the chain's too.
        let steps = self.draw_chain(true, true);
        let blend = self.state.global_composite_operation;
        let beyond = covers_beyond_the_draw(blend);
        // Drawn on its own wherever it composites only where it lands and
        // through no filter; a shadow it casts is a group of its own beside
        // it.
        let alone =
            !beyond && steps.is_none() && paint.image_filter().is_none();
        // Painted in a document from the tile where that lands on whole
        // device pixels and nothing composites the draw but its own alpha
        // and blend mode -- see `PatternDraw::aligned`.
        let aligned = alone
            && !self.casts_shadow()
            && paint.color_filter().is_none()
            && source.lands_whole(
                &Matrix::concat(&self.state.matrix, &pattern_matrix),
                repeat,
            );
        let features = match aligned {
            true => features
                .without(VectorFeatures::CANVAS_PATTERN)
                .with(VectorFeatures::CANVAS_PATTERN_ALIGNED),
            false => features,
        };

        // Recorded over the page and as far again as the filter and the
        // shadow reach: a picture keeps nothing of a draw past its bounds,
        // and a draw off the page can cast its shadow onto it.
        // The filter's reach is the chain's where the draw runs one, and its
        // steps' where only the shadow does.
        let filter_steps = match paint.image_filter() {
            Some(_) => steps.clone().or_else(|| self.exact_steps()),
            None => None,
        };
        let chain = filter_steps.as_ref().map_or(Vector::default(), |steps| {
            canvas_reach(steps, &self.state.matrix)
        });
        let shadow = match self.casts_shadow() {
            true => {
                // Its sigma is half its blur, as `paint_for_shadow` takes it.
                let sigma = self.state.shadow_blur / 2.0;
                let blur = REACH_SIGMAS as f32 * sigma;
                let offset = self.state.shadow_offset;
                Vector::new(offset.x.abs() + blur, offset.y.abs() + blur)
            }
            false => Vector::default(),
        };
        let bounds = self.bounds.with_outset(chain + shadow);
        let record = |paint: &Paint| {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            f(canvas, paint, Pass::Draw);
            recorder.finish_recording_as_picture(None)
        };
        // The draw as a document writes it, less what a group composites
        // through where one does: its filter, and its blend.
        let mut plain = paint.clone();
        plain.set_image_filter(None);
        if !alone {
            plain.set_blend_mode(BlendMode::SrcOver);
        }
        let Some(recorded) = record(&plain) else {
            return false;
        };
        let mut through = plain;
        through.set_shader(None);
        let unit_of = |paint: Paint, replay: Replay| {
            let tile = Arc::new(layer::PatternDraw {
                source: Arc::clone(&source),
                matrix: self.state.matrix,
                pattern_matrix,
                repeat,
                plain: recorded.clone(),
                paint,
                clip: None,
                replay,
                aligned,
            });
            let unit = Entries::new(
                vec![Layer::Pattern(Arc::clone(&tile))],
                vec![features],
            );
            (tile, unit)
        };
        // On an eight-bit sRGB page the pixels of a shadow under a
        // `globalAlpha` are cast from the draw without the alpha the
        // shadow's colour takes -- see `paint_for_shadow_in` -- a run's named
        // line colour without its `globalAlpha`, so that the two are rounded
        // once, as they are for a draw dyed any other way. Cast from the draw
        // at its alpha, the shape was rounded to a byte before the
        // shadow's colour multiplied it: a pattern of a canvas cast up
        // to a level from a pattern of its pixels. A document plays the
        // draw as recorded, at its alpha, under the shadow as named.
        let through_alpha = through.alpha_f();
        let moves = self.casts_shadow()
            && through.alpha_f() < 1.0
            && through.color_filter().is_none()
            && self.onto().is_some_and(|onto| onto.bytes);
        // The shadow's pixels paint, and the group of the draw it is cast
        // from, holding whatever alpha the shadow's colour did not take.
        let moved = moves
            .then(|| {
                let mut cast = Paint::default();
                cast.set_anti_alias(true).set_alpha_f(through.alpha_f());
                self.paint_for_shadow_in(&cast, Recording::Shared, lines)
            })
            .flatten()
            .map(|(mut shadow, pass)| {
                let mut held = through.clone();
                held.set_alpha_f(shadow.alpha_f());
                shadow.set_alpha_f(1.0);
                let replay = match pass {
                    Pass::AlphaInShadow => replay.opaque(),
                    Pass::Draw => replay.clone(),
                };
                (shadow, unit_of(held, replay).1)
            });
        let (pixels_shadow, shadow_unit) = moved.unzip();
        let (tile, unit) = unit_of(through, replay);
        let shadow_unit = shadow_unit.unwrap_or_else(|| unit.clone());
        let rec = |paint: Paint| LayerRec {
            paint: Some(paint),
            ..LayerRec::default()
        };
        // The draw's filter, and its blend where that composites only where
        // the draw lands.
        let mut filter = Paint::default();
        filter
            .set_anti_alias(true)
            .set_image_filter(paint.image_filter());
        if !beyond {
            filter.set_blend_mode(blend);
        }
        let drawn = match steps {
            Some(steps) => Composite::Chain {
                steps,
                rec: rec(filter),
                reach: None,
                coverage: None,
            },
            None => Composite::Layer(rec(filter)),
        };
        // The shadow of what its group holds, its alpha already in it; on
        // pixels where the shadow's colour took it, the draw without it.
        // Cast from what the draw's filter makes, as the HTML Standard
        // orders the two.
        let mut cast = Paint::default();
        cast.set_anti_alias(true)
            .set_image_filter(paint.image_filter());
        // On pixels the shadow is cast through the exact chain, in the
        // colour the pixels' shadow takes -- see `Self::shadow_steps` --
        // after the steps of the draw's own filter where it has one; under
        // a filter with no steps of its own, from what Skia's layer makes.
        let cast_steps = match paint.image_filter() {
            Some(_) => filter_steps,
            None => Some(Arc::from([])),
        };
        let shadow_steps = cast_steps.and_then(|filter_steps| {
            let mut pixels_cast = cast.clone();
            if moves {
                pixels_cast.set_alpha_f(through_alpha);
            }
            self.shadow_cast_in(&pixels_cast, Recording::Shared, lines)
                .map(|cast| {
                    filter_steps
                        .iter()
                        .chain(self.shadow_steps(&cast).iter())
                        .cloned()
                        .collect::<Arc<[Step]>>()
                })
        });
        let shadow = self.paint_for_shadow(&cast).map(|shadow| {
            let rec = LayerRec {
                paint: Some(shadow),
                pixels_paint: pixels_shadow,
                ..LayerRec::default()
            };
            match shadow_steps {
                Some(steps) => Composite::Chain {
                    steps,
                    rec,
                    reach: None,
                    coverage: None,
                },
                None => Composite::Layer(rec),
            }
        });
        let offset = self.state.shadow_offset;
        let identity = Matrix::new_identity();
        // A group, which the SVG backend drops whole -- see
        // `VectorFeatures::LAYER` -- and so rasterizes, as it does a
        // `saveLayer`; the draw on its own asks only for what it does.
        let grouped = features.with(VectorFeatures::LAYER);
        self.with_recorder(|mut recorder| {
            if !beyond {
                if let Some(shadow) = shadow {
                    recorder.append_group(
                        shadow,
                        None,
                        identity,
                        shadow_unit.clone(),
                        features.with(VectorFeatures::LAYER),
                        offset,
                    );
                }
                if alone {
                    let clip = self.state.clip.as_ref();
                    recorder.append_tile(tile, clip, features);
                    return;
                }
                recorder.append_group(
                    drawn,
                    None,
                    identity,
                    unit,
                    grouped,
                    Point::default(),
                );
                return;
            }
            // Held in one group the blend composites, each under the
            // transform the draw was made under, moved as the shadow is.
            let group = |composite: Composite, contents: &Entries, shift| {
                let shift = Matrix::translate(shift);
                Layer::Group(Arc::new(Group {
                    composite,
                    matrix: Matrix::concat(&shift, &self.state.matrix),
                    clip: None,
                    entries_clipped: false,
                    within: None,
                    contents_matrix: shift,
                    contents: contents.clone(),
                }))
            };
            let mut held = Vec::with_capacity(2);
            if let Some(shadow) = shadow {
                held.push(group(shadow, &shadow_unit, offset));
            }
            held.push(group(drawn, &unit, Point::default()));
            let mut blended = Paint::default();
            blended.set_anti_alias(true).set_blend_mode(blend);
            recorder.append_group(
                Composite::Layer(rec(blended)),
                None,
                identity,
                held.into_iter().map(|group| (group, grouped)).collect(),
                grouped,
                Point::default(),
            );
        });
        true
    }

    /// The filter chain a draw is run through by the crate rather than by
    /// Skia -- see [`Self::files_through_chain`]: one holding a
    /// `drop-shadow()` or a step run in the draw's own space, and for a draw
    /// with no `BlurOp` of its own, `unblurrable`, one holding a `blur()`,
    /// for a draw casting no shadow of the context's own, or casting one
    /// `shadow_apart` as a group of its own, under a blend that composites
    /// only where it lands. Any other chain of blurs and colour functions
    /// keeps the route its draw takes otherwise: a `BlurOp`, or Skia's
    /// filter. A run of text, a pattern fill and a gradient have no
    /// `BlurOp`, and Skia's filter blurs them by three box blurs, which came
    /// up to 12.7 levels off the Gaussian on 40px text blurred by 3, as
    /// Chrome's does, and 5.78 on a striped pattern blurred by 4.
    fn draw_chain(
        &self,
        unblurrable: bool,
        shadow_apart: bool,
    ) -> Option<Arc<[Step]>> {
        if covers_beyond_the_draw(self.state.global_composite_operation)
            || (self.casts_shadow() && !shadow_apart)
        {
            return None;
        }
        self.exact_steps().filter(|steps| {
            steps.iter().any(|step| {
                matches!(step, Step::Shadow { .. })
                    || step.runs_in_local_space()
                    || (unblurrable && matches!(step, Step::Blur(_)))
            })
        })
    }

    /// Files a draw under a `drop-shadow()` the crate runs itself -- see
    /// [`Filter::steps`] -- or under a Skia image filter on a page that
    /// composites in floats -- see [`Self::exact_steps`] -- as a group of
    /// its own: the draw recorded without its filter, under the transform
    /// it is made under and once for each recording the page keeps, run
    /// through the chain on pixels. Through Skia's layer the shadow's blur
    /// is not the exact Gaussian, under a turn or a skew the draw itself is
    /// resampled, and a float page's layer is eight bits. In a document the
    /// group composites through the paint as it is, which Skia draws as it
    /// draws the filtered draw itself. `false`, filing nothing, for any
    /// other draw, and for one casting a shadow or under a blend that
    /// covers beyond it, which stay Skia's.
    ///
    /// `steps`, where given, is the chain run instead of the one
    /// [`Self::draw_chain`] picks: the whole of the draw's filter, for a
    /// draw whose shadow is filed as a group of its own -- see
    /// `render_to_canvas` -- which is then filed casting none.
    fn files_through_chain<F>(
        &self,
        paint: &Paint,
        exact: Option<&Paint>,
        features: VectorFeatures,
        text: Option<&TextRun>,
        f: &F,
        steps: Option<Arc<[Step]>>,
    ) -> bool
    where
        F: Fn(&SkCanvas, &Paint, Pass),
    {
        let reached = text.map(|text| text.reached);
        let lines = text.is_some_and(|text| text.lines);
        // A draw whose paint blurs what it draws, through an image filter --
        // a pattern, a gradient, a run of text -- has no `BlurOp`; one whose
        // paint blurs its coverage, through a mask filter, keeps it, which
        // the chain's recording would blur a second time.
        let unblurrable = reached.is_some()
            || (paint.image_filter().is_some()
                && paint.mask_filter().is_none());
        let Some(steps) = steps.or_else(|| self.draw_chain(unblurrable, false))
        else {
            return false;
        };
        // Recorded over the page and as far again as the chain reaches: a
        // picture keeps nothing of a draw past its bounds, and a draw off
        // the page can cast its shadow onto it.
        let bounds = self
            .bounds
            .with_outset(canvas_reach(&steps, &self.state.matrix));
        // The draw keeps its alpha, the colour's and the global one, as it
        // does through Skia's layer; the group composites it at one,
        // through the blend.
        //
        // A run of text answers what it can ink, `reached`, which bounds the
        // chain's layer where its picture cannot: a picture of text is
        // bounded by the clip -- see `layer::reach_the_clip` -- and a chain
        // run over the page's every pixel took 1.7 ms a 40px word under a
        // drop shadow. Where either recording does not say, the layer keeps
        // what the entries reach.
        let recorded = |paint: &Paint| {
            let mut paint = paint.clone();
            paint
                .set_image_filter(None)
                .set_blend_mode(BlendMode::SrcOver);
            paint
        };
        let record = |paint: &Paint| {
            let paint = recorded(paint);
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            if let Some(reached) = reached {
                reached.set(None);
            }
            f(canvas, &paint, Pass::Draw);
            let reach = reached.and_then(Cell::get);
            recorder
                .finish_recording_as_picture(None)
                .map(|p| (p, reach))
        };
        let dual = self.recorder.borrow().dual();
        // The paint the run's pixels are recorded in.
        let pixels_paint = match dual {
            false => paint,
            true => exact.unwrap_or(paint),
        };
        let colour_entry =
            |run: Replay, recorded_as: Picture, document: Option<Picture>| {
                Layer::ColourText(Arc::new(layer::ColourText {
                    matrix: self.state.matrix,
                    replay: Some(run),
                    paint: recorded(pixels_paint),
                    inked: Rect::new_empty(),
                    shadow: None,
                    blend: None,
                    recording: LazyPicture::from(Some(recorded_as)),
                    bounds: self.bounds,
                    document: document.map(layer::ColourRecording::recorded),
                    clip: None,
                    #[cfg(test)]
                    watch: None,
                }))
            };
        // A kept run is filed as the draw it is, and recorded when a target
        // first plays it -- see `layer::DeferredPicture` -- with what it can
        // ink worked out now: in the paint its pixels are recorded in, and on
        // a page that keeps a recording for each kind of target in its own
        // paint for a document too, the reach the two join to.
        // A run holding colour glyphs is drawn again where the chain plays
        // it, its colour glyphs made for those pixels -- see
        // `layer::ColourText` -- and is never deferred.
        let colour = text.and_then(|text| text.colour.clone());
        let deferred = text
            .and_then(|text| text.defer)
            .filter(|_| colour.is_none())
            .and_then(|defer| {
                let (pixels, mut reach) = defer(&recorded(pixels_paint))?;
                let document = match dual {
                    false => None,
                    true => {
                        let (document, document_reach) =
                            defer(&recorded(paint))?;
                        reach = reach.zip(document_reach).map(|(mut a, b)| {
                            a.join(b);
                            a
                        });
                        Some(document)
                    }
                };
                let deferred = Arc::new(DeferredPicture::new(
                    pixels,
                    document,
                    bounds,
                    self.state.matrix,
                ));
                Some((Layer::Deferred(deferred), reach))
            });
        let (entry, reach) = match dual {
            _ if deferred.is_some() => deferred,
            false => record(paint)
                .map(|(picture, reach)| (Layer::Picture(picture), reach)),
            true => record(exact.unwrap_or(paint)).zip(record(paint)).map(
                |((pixels, pixels_reach), (document, document_reach))| {
                    let reach =
                        pixels_reach.zip(document_reach).map(|(mut a, b)| {
                            a.join(b);
                            a
                        });
                    (Layer::Targeted(Arc::new((pixels, document))), reach)
                },
            ),
        }
        .unzip();
        let reach = reach.flatten();
        let entry = match (entry, colour) {
            (Some(Layer::Picture(recorded)), Some(run)) => {
                Some(colour_entry(run, recorded, None))
            }
            (Some(Layer::Targeted(pair)), Some(run)) => {
                let (recorded, document) = Arc::unwrap_or_clone(pair);
                Some(colour_entry(run, recorded, Some(document)))
            }
            (entry, _) => entry,
        };
        // A run of text in one flat colour that the coverage route could
        // play is drawn once more, in its colour opaque, through the paint
        // its pixels are drawn in -- see `layer::CoverageRun`. Where that is
        // the paint the run's pixels are recorded in, as it is for an opaque
        // colour placed on the page's bytes, the same calls would record the
        // same picture, and the run's own is taken -- recorded when first
        // played where the run's is. A run naming
        // a line colour of its own is drawn in two colours, and keeps the
        // chain.
        let pixels = match &entry {
            Some(Layer::Picture(picture)) => {
                Some(CoveragePicture::Recorded(picture.clone()))
            }
            Some(Layer::Targeted(pictures)) => {
                Some(CoveragePicture::Recorded(pictures.0.clone()))
            }
            Some(Layer::Deferred(deferred)) => {
                Some(CoveragePicture::Deferred(Arc::clone(deferred)))
            }
            _ => None,
        };
        // A run in one flat colour is what the coverage route paints; one
        // holding colour glyphs is not, and drawn there came out in the
        // run's colour, its glyphs' own colours gone: those this library
        // draws, and those the platform draws -- asked last, since a run
        // not kept is laid out again to answer.
        let holds_colour = matches!(entry, Some(Layer::ColourText(_)));
        let coverage = match exact {
            Some(exact)
                if reached.is_some()
                    && !lines
                    && !holds_colour
                    && paint.shader().is_none()
                    && matches!(
                        self.state.dye(paint.style()),
                        Dye::Color(..)
                    )
                    && !text.is_some_and(|text| (text.own_colours)()) =>
            {
                self.exact_colour(paint.style())
                    .filter(|colour| {
                        coverage_files(&steps, colour, exact.is_dither())
                    })
                    .and_then(|colour| {
                        let mut opaque = exact.clone();
                        opaque.set_alpha_f(1.0);
                        #[cfg(test)]
                        let pixels =
                            pixels.filter(|_| SHARES_COVERAGE_PICTURE.get());
                        let picture = match pixels.filter(|_| {
                            recorded(&opaque) == recorded(pixels_paint)
                        }) {
                            Some(picture) => {
                                #[cfg(test)]
                                SHARED_COVERAGE_PICTURES
                                    .set(SHARED_COVERAGE_PICTURES.get() + 1);
                                Some(picture)
                            }
                            // Its own, recorded when first played where the
                            // run is kept, as the run's pixels are.
                            None => match text
                                .and_then(|text| text.defer)
                                .and_then(|defer| defer(&recorded(&opaque)))
                            {
                                Some((drawn, _)) => {
                                    Some(CoveragePicture::Deferred(Arc::new(
                                        DeferredPicture::new(
                                            drawn,
                                            None,
                                            bounds,
                                            self.state.matrix,
                                        ),
                                    )))
                                }
                                None => record(&opaque).map(|(picture, _)| {
                                    CoveragePicture::Recorded(picture)
                                }),
                            },
                        };
                        picture.map(|picture| {
                            Arc::new(CoverageRun {
                                picture,
                                exact: colour,
                                alpha: paint.alpha_f(),
                                kept: text.and_then(|text| {
                                    Kept::new(
                                        text.kept.clone()?,
                                        Matrix::concat(
                                            &self.state.matrix,
                                            &text.anchor,
                                        ),
                                        &opaque,
                                    )
                                }),
                            })
                        })
                    })
            }
            _ => None,
        };
        let mut through = Paint::default();
        through
            .set_blend_mode(self.state.global_composite_operation)
            .set_image_filter(paint.image_filter());
        // The draw inside asks for what its paint does, less the filter and
        // the blend, which the group composites through.
        let drawn = features.without(
            VectorFeatures::IMAGE_FILTER
                .with(VectorFeatures::BLEND_MODE)
                .with(VectorFeatures::UNMATCHED_BLEND)
                .with(VectorFeatures::MASKED_BLEND),
        );
        let contents = entry.map(|entry| (entry, drawn)).into_iter().collect();
        self.with_recorder(|mut recorder| {
            recorder.append_group(
                Composite::Chain {
                    steps,
                    rec: LayerRec {
                        paint: Some(through),
                        ..LayerRec::default()
                    },
                    reach,
                    coverage,
                },
                None,
                Matrix::new_identity(),
                contents,
                features,
                Point::default(),
            );
        });
        true
    }

    pub fn map_points(&self, coords: &[f32]) -> Vec<Point> {
        // treat the flat array of floats as x/y pairs
        coords
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                self.state.matrix.map_point(Point::new(pair[0], pair[1]))
            })
            .collect()
    }

    pub fn reset_size(&mut self, dims: impl Into<Size>) {
        // called by the canvas when .width or .height are assigned to
        self.bounds = Rect::from_size(dims);
        self.path = PathBuilder::default();
        self.stack = vec![];
        // layers is parallel to stack; clearing one without the other leaves a
        // stale entry that makes a later restore() close a layer that is not
        // open.
        self.layers = vec![];
        self.state = State::default();

        // erase any existing content
        self.with_recorder(|mut recorder| {
            recorder.set_bounds(self.bounds);
        });
    }

    pub fn resize(&mut self, dims: impl Into<Size>) {
        // non-destructively resize the canvas (via the canvas.resize()
        // extension)
        self.bounds = Rect::from_size(dims);
        self.with_recorder(|mut recorder| {
            recorder.update_bounds(self.bounds);
        });
    }

    pub fn push(&mut self) {
        let new_state = self.state.clone();
        self.stack.push(new_state);
        self.layers.push(false);
    }

    pub fn pop(&mut self) {
        // don't do anything if we're already back at the initial stack frame
        if let Some(old_state) = self.stack.pop() {
            // If this frame opened a Skia layer (saveLayer), composite it
            // back onto the destination now with the layer's paint.
            if self.layers.pop().unwrap_or(false) {
                self.with_recorder(|mut recorder| {
                    recorder.close_layer();
                });
            }
            self.state = old_state;

            self.with_recorder(|mut recorder| {
                recorder.set_state(self.state.matrix, &self.state.clip);
            });
        }
    }

    /// Runs `f` against the canvas with the current `globalAlpha` and
    /// `globalCompositeOperation` applied to whatever it draws.
    ///
    /// `with_canvas` alone carries the transform and clip, because those live
    /// on the `SkCanvas`, but not the paint state -- every other draw folds
    /// that into a `Paint` it builds. A draw that paints with its own paints,
    /// as `Paragraph::paint` does, has nowhere to fold it, so it composited as
    /// though `globalAlpha` were 1 and the blend mode `source-over`.
    ///
    /// The layer is what makes the two apply to the drawing as a group, which
    /// is the Canvas2D semantic: `destination-out` has to erase where the
    /// glyphs land, not per-glyph against a blank layer. Skipped entirely when
    /// there is nothing to apply, so the common case costs no offscreen.
    ///
    /// `features` is what the paragraph's own drawing asks of a vector
    /// backend -- its colour glyphs, which this library draws through layers
    /// of their own -- see `typography::paragraph_colour_features`.
    pub fn with_composited_canvas<F>(&self, features: VectorFeatures, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        let alpha = self.state.global_alpha as f32;
        let blend = self.state.global_composite_operation;
        // A draw holding colour glyphs this library draws itself -- every one
        // composites through a layer of its own -- is rasterized on the CPU
        // where it is played on pixels -- see `layer::ColourText`.
        let colour = features
            .refused_by(VectorFeatures::LAYER)
            .then(|| self.colour_recording(&f))
            .flatten();

        if alpha == 1.0 && blend == BlendMode::SrcOver {
            match colour {
                Some(text) => self.with_recorder(|mut recorder| {
                    recorder.append_colour_text(
                        text,
                        self.state.clip.as_ref(),
                        features,
                    );
                }),
                None => self.append_draw(features, false, f),
            }
            return;
        }

        let mut paint = Paint::default();
        paint.set_alpha_f(alpha);
        paint.set_blend_mode(blend);
        let paint = self.faint().drawable(&paint).into_owned();

        // A group of our own rather than a `saveLayer` recorded around the
        // draw, so that the alpha composites exactly on pixels and as an
        // opacity in a document -- see `LayerRec::open_on`. The draw is
        // recorded under the transform it is made under; the group applies
        // the clip.
        let record = |recording: Recording| {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            f(canvas, recording);
            recorder.finish_recording_as_picture(None)
        };
        // Once for each recording the page keeps, as every draw is: the
        // pixels' first, as `Layer::Targeted` holds them.
        let entry = match (colour, self.recorder.borrow().dual()) {
            (Some(text), _) => Some(Layer::ColourText(text)),
            (None, false) => record(Recording::Shared).map(Layer::Picture),
            (None, true) => record(Recording::Pixels)
                .zip(record(Recording::Document))
                .map(|pictures| Layer::Targeted(Arc::new(pictures))),
        };
        // What it draws with its own paints, which ask for what its colour
        // glyphs do.
        let contents: Entries =
            entry.into_iter().map(|entry| (entry, features)).collect();
        self.with_recorder(|mut recorder| {
            recorder.append_group(
                Composite::Layer(LayerRec {
                    paint: Some(paint),
                    ..LayerRec::default()
                }),
                None,
                Matrix::new_identity(),
                contents,
                self.composited_features(),
                Point::default(),
            );
        });
    }

    /// The draw `f` makes as a [`ColourText`](layer::ColourText) with no
    /// replay, rasterized on the CPU where it is played on pixels: recorded
    /// under the canvas's transform into each recording the page keeps, and
    /// bounded by what it inks -- see [`layer::inked_by`]. `None` where
    /// Skia records nothing.
    fn colour_recording<F>(&self, f: &F) -> Option<Arc<layer::ColourText>>
    where
        F: Fn(&SkCanvas, Recording),
    {
        let record = |recording: Recording| {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            f(canvas, recording);
            recorder.finish_recording_as_picture(None)
        };
        let dual = self.recorder.borrow().dual();
        let pixels = match dual {
            true => Recording::Pixels,
            false => Recording::Shared,
        };
        let recorded = record(pixels)?;
        let document = match dual {
            true => Some(record(Recording::Document)?),
            false => None,
        };
        let inked =
            layer::inked_by(self.bounds, &self.state.matrix, |canvas| {
                f(canvas, pixels);
            });
        Some(Arc::new(layer::ColourText {
            matrix: self.state.matrix,
            replay: None,
            paint: Paint::default(),
            inked,
            shadow: None,
            blend: None,
            recording: LazyPicture::from(Some(recorded)),
            bounds: self.bounds,
            document: document.map(layer::ColourRecording::recorded),
            clip: None,
            #[cfg(test)]
            watch: None,
        }))
    }

    /// Records a paragraph that casts text shadows: what lies beneath them,
    /// each shadow, and the text over them, as one group composited through
    /// the context's alpha and blend, as [`Self::with_composited_canvas`]
    /// composites any paragraph. `draw` draws the part it is handed.
    ///
    /// A blurred shadow is its own group, whose chain blurs the shadow drawn
    /// sharp on pixels by the exact Gaussian -- see `context::chain` -- so
    /// that a run of text casts the blur of the whole run. What it blurs is
    /// confined to its cast's `bounds`, which the blur spreads past. A
    /// document draws the shadow as Skia's mask filter draws it, glyph by
    /// glyph, which a PDF holds as an image mask and an SVG, which has no
    /// mask filter to write, embeds as pixels. An unblurred shadow is drawn
    /// sharp on both.
    ///
    /// Under an alpha of 1 and `source-over` a group around them all would
    /// change nothing, since every part is drawn `source-over` too, so there
    /// is none: each part is filed on the page as it stands, which a vector
    /// backend then takes part by part -- an SVG keeps the text and the
    /// sharp shadows as vectors, where it writes nothing of a layer.
    ///
    /// `features` is what the paragraph's glyphs ask of a vector backend --
    /// see [`Self::with_composited_canvas`] -- which the parts that draw them
    /// ask for in turn.
    pub(crate) fn draw_with_shadows<F>(
        &self,
        casts: &[ShadowCast],
        features: VectorFeatures,
        draw: F,
    ) where
        F: Fn(&SkCanvas, ShadowPart, Recording),
    {
        if self.state.global_alpha == 1.0
            && self.state.global_composite_operation == BlendMode::SrcOver
        {
            self.append_draw(features, false, |canvas, recording| {
                draw(canvas, ShadowPart::Beneath, recording);
            });
            for (index, cast) in casts.iter().enumerate() {
                if !cast.blurs() {
                    self.append_draw(features, false, |canvas, recording| {
                        draw(canvas, ShadowPart::Sharp(index), recording);
                    });
                } else if let Some(group) =
                    self.shadow_group(cast, index, &draw)
                {
                    self.with_recorder(|mut recorder| {
                        recorder.append_device_group(
                            group,
                            VectorFeatures::IMAGE_FILTER,
                        );
                    });
                }
            }
            self.append_draw(features, false, |canvas, recording| {
                draw(canvas, ShadowPart::Over, recording);
            });
            return;
        }
        let mut paint = Paint::default();
        paint.set_alpha_f(self.state.global_alpha as f32);
        paint.set_blend_mode(self.state.global_composite_operation);
        let paint = self.faint().drawable(&paint).into_owned();
        let record = |part: ShadowPart, recording: Recording| {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            draw(canvas, part, recording);
            recorder.finish_recording_as_picture(None)
        };
        let dual = self.recorder.borrow().dual();
        // The recording a pair's pixels and its document are each drawn for:
        // the page's two where it keeps two, and its one otherwise.
        let (pixels, document) = match dual {
            true => (Recording::Pixels, Recording::Document),
            false => (Recording::Shared, Recording::Shared),
        };
        // Once for each recording the page keeps, as every draw is.
        let entry = |part: ShadowPart| match dual {
            false => record(part, Recording::Shared).map(Layer::Picture),
            true => record(part, pixels)
                .zip(record(part, document))
                .map(|pictures| Layer::Targeted(Arc::new(pictures))),
        };
        // The parts drawn sharp ask for what the glyphs do, and a blurred
        // shadow for its filter, as each does filed on the page.
        let plain = |part| entry(part).map(|e| (e, features));
        let mut contents: Vec<(Layer, VectorFeatures)> =
            Vec::with_capacity(casts.len() + 2);
        contents.extend(plain(ShadowPart::Beneath));
        for (index, cast) in casts.iter().enumerate() {
            match cast.blurs() {
                false => contents.extend(plain(ShadowPart::Sharp(index))),
                true => contents.extend(
                    self.shadow_group(cast, index, &draw).map(|group| {
                        (
                            Layer::Group(Arc::new(group)),
                            VectorFeatures::IMAGE_FILTER,
                        )
                    }),
                ),
            }
        }
        contents.extend(plain(ShadowPart::Over));
        self.with_recorder(|mut recorder| {
            recorder.append_group(
                Composite::Layer(LayerRec {
                    paint: Some(paint),
                    ..LayerRec::default()
                }),
                None,
                Matrix::new_identity(),
                contents.into_iter().collect(),
                self.composited_features(),
                Point::default(),
            );
        });
    }

    /// What a group composited through the context's alpha and blend asks
    /// of a vector backend: a layer, which an SVG has none of, and the blend
    /// -- see [`blend_features`] -- its source taken for translucent, since a
    /// group may hold anything.
    fn composited_features(&self) -> VectorFeatures {
        VectorFeatures::LAYER
            .with(blend_features(self.state.global_composite_operation, false))
    }

    /// The group a blurred shadow is drawn in for `draw_with_shadows`: its
    /// chain blurs the shadow drawn sharp on pixels, confined to the cast's
    /// `bounds` on the device, and a document plays the shadow Skia's mask
    /// filter draws instead. `None` where either recording draws nothing.
    fn shadow_group<F>(
        &self,
        cast: &ShadowCast,
        index: usize,
        draw: &F,
    ) -> Option<Group>
    where
        F: Fn(&SkCanvas, ShadowPart, Recording),
    {
        let record = |part: ShadowPart, recording: Recording| {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            draw(canvas, part, recording);
            recorder.finish_recording_as_picture(None)
        };
        // The recording a pair's pixels and its document are each drawn for:
        // the page's two where it keeps two, and its one otherwise.
        let (pixels, document) = match self.recorder.borrow().dual() {
            true => (Recording::Pixels, Recording::Document),
            false => (Recording::Shared, Recording::Shared),
        };
        let drawn = record(ShadowPart::Sharp(index), pixels)
            .zip(record(ShadowPart::Masked(index), document))?;
        Some(Group {
            composite: Composite::Chain {
                steps: Arc::from([Step::Blur(cast.sigma)]),
                rec: LayerRec::default(),
                reach: None,
                coverage: None,
            },
            matrix: Matrix::new_identity(),
            clip: None,
            entries_clipped: false,
            within: Some(self.state.matrix.map_rect(cast.bounds).0),
            contents_matrix: Matrix::new_identity(),
            // What a document plays of it is the shadow Skia's mask filter
            // draws.
            contents: Entries::new(
                vec![Layer::Targeted(Arc::new(drawn))],
                vec![VectorFeatures::MASK_FILTER],
            ),
        })
    }

    /// Pushes a save frame that also opens a Skia layer. Subsequent draws
    /// accumulate into the layer until the matching `restore()`/`pop`, which
    /// composites it onto the destination with `paint` (alpha / blend mode /
    /// image filter). `bounds` is an optional layer-bounds hint; `backdrop` is
    /// an image filter applied to the existing content behind the layer
    /// (blur-behind / frosted glass). Mirrors CanvasKit's
    /// `Canvas.saveLayer(paint?, bounds?, backdrop?)`.
    pub fn save_layer(
        &mut self,
        paint: Option<Paint>,
        bounds: Option<Rect>,
        backdrop: Option<PageFilter<SkImageFilter>>,
    ) {
        let paint =
            paint.map(|paint| self.faint().drawable(&paint).into_owned());
        let pixels_backdrop = backdrop
            .as_ref()
            .filter(|filter| filter.depends())
            .zip(self.pixels_onto())
            .map(|(filter, onto)| filter.on(Some(&onto)));
        let backdrop = backdrop.map(|filter| filter.on(self.onto().as_ref()));
        // open_layer, not with_canvas: the recorder draws the layer around
        // its contents once it closes -- see `PageRecorder::open_layer`.
        self.with_recorder(|mut recorder| {
            recorder.open_layer(LayerRec {
                paint,
                bounds,
                backdrop,
                pixels_backdrop,
                ..LayerRec::default()
            });
        });
        let new_state = self.state.clone();
        self.stack.push(new_state);
        self.layers.push(true);
    }

    pub fn scoot(&mut self, point: Point) {
        // update initial point if first drawing command isn't a moveTo.
        // verbs(), not snapshot(): see the note on Path2D::scoot -- snapshot()
        // copies the whole path, and this runs before every segment
        // append.
        if self.path.verbs().is_empty() {
            self.path.move_to(point);
        }
    }

    /// Works out how much a texture's grid has to be magnified to be drawable,
    /// and the rect to lay it out over.
    ///
    /// # Why a texture needs bounding at all
    ///
    /// `path_2d_path_effect` and `line_2d_path_effect` stamp one copy of the
    /// tile per grid position across the whole path being filled, in user
    /// space, accumulating every copy into one path. Nothing in that is
    /// bounded. The count is `area / spacing²`: the caller supplies the
    /// divisor and both the drawn geometry and the current transform inflate
    /// the numerator. Skia's answer at the top end is `SK_ABORT` --
    /// `SkContainers.cpp:80`, "Requested capacity is too large" -- which no
    /// `catch_unwind` can see, and which arrives only after tens of gigabytes
    /// and minutes of work. Measured on a 100×100 page, `spacing: 0.001`
    /// reaches it after 29 GB, and `spacing: 0.005` survives at 29.6 GB and
    /// five minutes.
    ///
    /// Two unrelated routes get there, and only one of them involves an odd
    /// texture:
    ///
    /// * a fine spacing, where `0.001` on that page is 10¹⁰ positions;
    /// * a small transform, where the default `spacing: 8.0` under
    ///   `scale(0.002)` measured 6.2 GB and aborts a little below that.
    ///
    /// The second is an ordinary texture drawn through an ordinary transform,
    /// so no check on the texture itself can catch it. The area is only known
    /// here.
    ///
    /// # What it does
    ///
    /// A grid finer than the pixel raster costs everything and shows nothing.
    /// Below one device pixel of period every pixel averages many cells, so
    /// only the pattern's *mean coverage* survives sampling. Magnifying the
    /// whole pattern -- period, tile and stroke width by one shared factor --
    /// is a similarity transform, and a similarity transform leaves that mean
    /// where it was, while dividing the position count by the square of the
    /// factor.
    ///
    /// That only holds because the factor reaches the tile as well. Widening
    /// the period alone would hold the count down just the same and quietly
    /// thin the pattern out, since coverage is the ratio between mark and
    /// period. Rendering a sub-pixel grid both ways confirms it: with the tile
    /// scaled the two are byte-identical, and without it a small tile loses
    /// most of its coverage.
    ///
    /// The equality is exact for a tile path. For the line variety Skia's own
    /// rendering stops tracking coverage well before a pixel -- holding the
    /// mark-to-period ratio at 25% and shrinking only the scale, alpha runs
    /// `8: 64, 4: 64, 2: 32, 1: 15, 0.5: 0` -- so below a period of about two
    /// there is no faithful result left to preserve, and magnifying up is
    /// simply the more stable of two wrong answers.
    ///
    /// [`MAX_TEXTURE_TILES`] is what actually bounds the count, and it bounds
    /// it whatever the area: past the ceiling the factor grows until the grid
    /// fits. Reaching it takes a texture spread over thousands of pixels at a
    /// period near the floor, and past it the pattern coarsens, which is the
    /// one case here a viewer could notice. It replaces aborting the process.
    ///
    /// # What was tried and does not work
    ///
    /// Culling the grid to the page looks free -- a position outside it is
    /// built and then thrown away, and growing a filled rect from 100 to
    /// 10000 on a 100×100 page leaves the PNG byte-identical while taking
    /// 2 MB to 246 MB. It is not free. `line_2d_path_effect` emits each row
    /// as a span across the whole lattice box rather than as a local mark, so
    /// a shortened box shortens the spans, and under rotation a position
    /// outside the visible area still draws into it. Culling changed 75 of
    /// 864 renders at ordinary spacings -- and none at all when the transform
    /// happened to leave the geometry inside the page, which is exactly how a
    /// narrower test would have missed it.
    /// Returns the magnification, the width of one device pixel in the user
    /// units a mark is measured in, and the rect to lay the grid over.
    fn texture_lattice(
        &self,
        spacing: Point,
        mark: Option<f32>,
        stencil: Rect,
    ) -> (f32, f32, Rect) {
        // The grid is laid out in user space, so the period has to be taken
        // into device space before it can be compared against a pixel. Each
        // axis's length under the transform is its column's magnitude: a
        // rotation contributes nothing to it, a scale contributes all of
        // itself.
        let matrix = &self.state.matrix;
        let axis_x = matrix.scale_x().hypot(matrix.skew_y());
        let axis_y = matrix.scale_y().hypot(matrix.skew_x());
        let finest = (spacing.x * axis_x).min(spacing.y * axis_y);

        // A zero, negative or NaN period makes the grid matrix singular, and
        // no magnification makes it mean anything. Skia recognises those and
        // drops the effect rather than expanding it, so they pass through
        // exactly as they behave today.
        // One device pixel in user units, taken on the axis the transform
        // shrinks most so the figure is never optimistic. A mark thinner than
        // this cannot be drawn as geometry.
        let device_pixel = 1.0 / axis_x.min(axis_y);

        if !finest.is_finite() || finest <= 0.0 {
            return (1.0, device_pixel, stencil.with_outset(spacing * 1.5));
        }

        let mut magnify = (1.0 / finest).max(1.0);

        // A resolvable period is not enough on its own: the mark inside it
        // has to survive rasterization too, and the GPU backend drops a tile
        // narrower than half a device pixel outright where the raster backend
        // antialiases it. Magnifying to that floor costs the raster backend
        // nothing -- a 0.25-wide tile on a period of 1 and the same pattern at
        // 0.5 on 2 come back byte-identical -- and takes the GPU from a blank
        // page to that same answer. Going further, to a whole pixel, would
        // start to cost: the raster backend reads 5.4% coverage where 6.25% is
        // right, so half a pixel is where both backends agree.
        if let Some(mark) = mark {
            let narrowest = mark * axis_x.min(axis_y) * magnify;
            if narrowest.is_finite() && narrowest > 0.0 && narrowest < 0.5 {
                magnify *= 0.5 / narrowest;
            }
        }

        // Both periods are positive here: each is at least `finest`, which the
        // check above established. A degenerate `stencil` leaves this
        // non-finite, and then the comparison is false and the grid passes
        // through -- Skia rejects such a path on its own.
        let positions = (stencil.width() / (spacing.x * magnify))
            * (stencil.height() / (spacing.y * magnify));
        if positions > MAX_TEXTURE_TILES {
            magnify *= (positions / MAX_TEXTURE_TILES).sqrt();
        }

        // Multiplied out before the outset rather than after: `magnify` runs
        // to the reciprocal of the period, so scaling it on its own can
        // overflow where the product it belongs to cannot.
        let period = Point::new(spacing.x * magnify, spacing.y * magnify);

        // Keep the original expansion past the path's own bounds, so a mark
        // straddling the edge is still drawn and then clipped back.
        (magnify, device_pixel, stencil.with_outset(period * 1.5))
    }

    pub fn draw_path(
        &mut self,
        path: Option<Path>,
        style: PaintStyle,
        rule: Option<PathFillType>,
    ) {
        let mut path = path.unwrap_or_else(|| {
            // the current path has already incorporated its transform state
            let inverse = self.state.matrix.invert().unwrap_or_default();
            self.path.snapshot().make_transform(&inverse)
        });
        path.set_fill_type(rule.unwrap_or(PathFillType::Winding));

        // A flat rectangle's shadow is a blurred rectangle, filed as a
        // `BlurOp`, whose blur has a closed form -- see `blur_mask` -- and
        // the draw is made without it.
        // Any other shadow the chain casts is filed as a group of its own
        // before it -- see `Self::files_shadow_through_chain` -- and the draw
        // then takes the route it takes casting none, so that its own pixels
        // are the ones it lands with no shadow: a `BlurOp`, or a cut its
        // clip makes, each of which refuses a shadow.
        if self.files_rect_shadow(&path, style)
            || self.files_path_shadow(&path, style)
        {
            let (color, space) = self.state.shadow_color.clone();
            self.state.shadow_color.0.a = 0.0;
            self.draw_path(Some(path), style, rule);
            self.state.shadow_color = (color, space);
            return;
        }

        // if path will fill the whole canvas and paint/blend are fully
        // opaque...
        if matches!(style, PaintStyle::Fill | PaintStyle::StrokeAndFill)
            && matches!(
                &self.state.global_composite_operation,
                BlendMode::SrcOver | BlendMode::Src | BlendMode::Clear
            )
            && self.state.fill_style.is_opaque()
            && self.state.global_alpha == 1.0
            && self.state.clip.is_none()
            // ...and nothing is waiting to composite it. Resetting the
            // recorder below discards its save stack, and an open layer
            // lives on that stack, so an opaque full-page fill inside one
            // threw the layer away wholesale -- not merely its alpha. A
            // 20x20 fill on a 20x20 page came out at 255 for *every* layer
            // alpha, while 19.5 was correct, since only the full-page fill
            // reached here. `global_alpha` above cannot stand in for this:
            // a layer carries its own alpha, and the state inside it is
            // usually 1.0.
            && !self.layers.iter().any(|owns_layer| *owns_layer)
            // ...and nothing stands between the fill and the page. A filter
            // decides what the draw finally puts down, and it may put down
            // less than the path covers -- or nothing at all. `MakeEmpty`
            // produces no output, so a full-page fill through it erased the
            // page and drew nothing in its place, while the same fill one
            // pixel smaller left the page alone. A colour filter can take the
            // alpha to zero, an offset can move the fill off the page, and a
            // mask filter can eat its edges.
            && self.state.skia_image_filter.is_none()
            && self.state.skia_color_filter.is_none()
            && self.state.skia_mask_filter.is_none()
            && self.state.filter.is_none()
            && holds(&path, self.bounds)
        {
            // ...erase existing vector content layers (but preserve CTM & clip
            // path)
            self.with_recorder(|mut recorder| {
                recorder.set_bounds(self.bounds);
                recorder.set_state(self.state.matrix, &self.state.clip);
            });
        }

        // A draw under a sole `blur()` may become a `BlurOp`, which reads
        // whether the path is a rectangle, as the coverage does; asked once
        // for both. Any other draw takes the coverage as it always has and
        // pays nothing for the op.
        let blurred = self.state.filter.sole_blur().is_some();
        let rect = blurred.then(|| path.is_rect()).flatten();
        let coverage = match blurred {
            true => coverage_given(&path, rect.is_some(), &self.state.matrix),
            false => coverage_of(&path, &self.state.matrix),
        };
        // A blurred draw the recorder keeps as a `BlurOp` is built from the
        // state without the paint `paint_for_drawing` makes, whose filters
        // the op replaces. Built this way it records in about the time an
        // unblurred draw does, 69 ns against 64 on the CPU; from that paint
        // it took about 94.
        if let Some(sigma) = blurred.then(|| self.blurs_as_op(style)).flatten()
        {
            let paint = self.paint_for_blur(style);
            let draw = BlurDraw::of(&path, rect, &paint, sigma, self.faint())
                .exact(self.exact_colour(style));
            let features = self.state.filter.blur_features(coverage);
            let (matrix, clip) = (self.state.matrix, self.state.clip.as_ref());
            self.with_recorder(|mut recorder| {
                recorder.append_blur(draw, matrix, clip, features);
            });
            return;
        }
        let paint = self.paint_for_drawing(style, coverage);
        let features = self.vector_features(&paint, Some(style));
        let exact = self.exact_paint(&paint, style);
        // A texture's mark held exactly, asked for here because the draw
        // below runs while the recorder is held -- see `place_mark`.
        let mark = matches!(self.state.dye(style), Dye::Texture(_))
            .then(|| self.exact_colour(style))
            .flatten();
        if let Some(clip) = self.cut_clip(&path, &paint, style) {
            let (matrix, faint) = (self.state.matrix, self.faint());
            // A cut draw dyed by a gradient is drawn on a GPU canvas in the
            // CPU's bytes, as one filed as a `GradientDraw` is.
            let gradient = self.state.dye(style).gradient();
            #[cfg(test)]
            let gradient = gradient.filter(|_| FILES_GRADIENTS.get());
            let gpu = gradient
                .filter(|_| !self.composites_in_floats())
                .and_then(|spec| {
                    let bounds =
                        drawn_bounds(&path, &paint, style, &matrix, false);
                    let dye = GradientDye {
                        spec,
                        path: Some((&path, bounds)),
                        whole_pixels: false,
                    };
                    let shape = |canvas: &SkCanvas, paint: &Paint| {
                        canvas.draw_path(&path, &faint.drawable(paint));
                    };
                    let pixels = exact.as_ref().unwrap_or(&paint);
                    let beyond = covers_beyond_the_draw(
                        self.state.global_composite_operation,
                    );
                    match self.gradient_path(&dye, &paint, pixels, beyond) {
                        Some(path) => Some(layer::OnGpu::Path(path)),
                        None => self.gradient_on_gpu(
                            &dye,
                            &paint,
                            pixels,
                            beyond,
                            bounds,
                            &shape,
                            &|_, _| {},
                            &shape,
                        ),
                    }
                });
            self.with_recorder(|mut recorder| {
                recorder.append_cut(
                    clip,
                    matrix,
                    features,
                    gpu,
                    |canvas, recording| {
                        let paint = match (recording, exact.as_ref()) {
                            (Recording::Pixels, Some(exact)) => exact,
                            _ => &paint,
                        };
                        canvas.draw_path(&path, &faint.drawable(paint));
                    },
                );
            });
            return;
        }
        // Kept only for a canvas's pattern, the one dye that takes it.
        let replay = matches!(self.state.dye(style), Dye::Pattern(_))
            .then(|| Replay::Path(path.serialize(), self.faint()));
        let whole_pixels = style == PaintStyle::Fill
            && self.state.clip.is_none()
            && fills_whole_pixels(&path, &self.state.matrix);
        self.render_to_canvas(
            &paint,
            exact.as_ref(),
            features,
            replay,
            None,
            self.state.dye(style).gradient().map(|spec| GradientDye {
                spec,
                path: Some((
                    &path,
                    drawn_bounds(
                        &path,
                        &paint,
                        style,
                        &self.state.matrix,
                        false,
                    ),
                )),
                whole_pixels,
            }),
            |canvas, paint, _| {
                self.fill_or_stroke(canvas, &path, paint, style, mark.as_ref());
            },
        );
    }

    /// Files the `shadowColor` shadow of a fill or stroke of `path` in
    /// `style` as a group of its own, cast through the chain -- see
    /// [`Self::files_shadow_through_chain`] -- from the draw in the paint it
    /// is drawn in. `false`, filing nothing, wherever that does, and for a
    /// path a canvas's pattern dyes, whose route casts the shadow apart from
    /// the canvas as it draws it at the density it is read at -- see
    /// `Self::files_through_pattern`. Cast from the path through Skia's
    /// picture shader instead, the shadow was a level off at densities 2
    /// and 3.
    fn files_path_shadow(&mut self, path: &Path, style: PaintStyle) -> bool {
        if !self.casts_shadow()
            || matches!(self.state.dye(style), Dye::Pattern(_))
        {
            return false;
        }
        // The coverage `draw_path` hands the paint, so that the shadow is
        // cast from the paint the draw is drawn in.
        let coverage = match self.state.filter.sole_blur().is_some() {
            true => coverage_given(
                path,
                path.is_rect().is_some(),
                &self.state.matrix,
            ),
            false => coverage_of(path, &self.state.matrix),
        };
        let paint = self.paint_for_drawing(style, coverage);
        let features = self.vector_features(&paint, Some(style));
        let mark = matches!(self.state.dye(style), Dye::Texture(_))
            .then(|| self.exact_colour(style))
            .flatten();
        self.files_shadow_through_chain(
            &paint,
            features,
            None,
            &|canvas, paint, _| {
                self.fill_or_stroke(canvas, path, paint, style, mark.as_ref());
            },
        )
    }

    /// Fills or strokes `path` in `paint` on `canvas`, as `draw_path` draws
    /// it in `style`: through the state's texture where it has one, its mark
    /// held exactly as `mark` says -- see `place_mark` -- and as the paint
    /// draws it otherwise.
    fn fill_or_stroke(
        &self,
        canvas: &SkCanvas,
        path: &Path,
        paint: &Paint,
        style: PaintStyle,
        mark: Option<&ExactColour>,
    ) {
        if let Some(tile) = self.state.texture(style) {
            // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
            // Re-check: mix the PathEffect into the paint and draw
            // directly; if the filled bounds are right the two-path
            // intersection below can go.
            //
            // Simply mixing the PathEffect into the paint and drawing
            // totally misjudges the boundaries of the
            // path being filled/stroked. Instead we'll create a path
            // with the texture and a path with the
            // desired outline separately, then draw their overlap

            // outline strokes on user path (if paint style is stroke)
            // so we can use a fill operation below.
            // As of skia-safe 0.94+,
            // `fill_path_with_paint` writes into a `PathBuilder`;
            // detach it back to a `Path` for the downstream
            // `bounds()` / `op()` / `clip_path()` calls.
            let mut stencil_builder = PathBuilder::new();
            fill_path_with_paint(path, paint, &mut stencil_builder, None, None);
            let stencil = stencil_builder.detach();

            // The grid has to be resolved against the area it covers,
            // not against the tile alone: see
            // `texture_lattice`.
            let (magnify, device_pixel, expanded_bounds) = self
                .texture_lattice(
                    tile.spacing(),
                    tile.mark(),
                    *stencil.bounds(),
                );

            // paint containing the PathEffect
            let mut tile_paint = paint.clone();
            let thinned = tile.mix_into(
                &mut tile_paint,
                self.state.global_alpha as f32,
                magnify,
                device_pixel,
                self.onto().as_ref(),
            );
            let mark_alpha = tile_paint.alpha_f();
            place_mark(&mut tile_paint, paint, mark, mark_alpha);

            let enclosing_frame = Path::rect(expanded_bounds, None);

            if tile.use_clip() {
                // apply the user path as a clipping mask and fill the
                // whole enclosing rect with
                // tile pattern
                canvas.save();
                canvas.clip_path(&stencil, Some(ClipOp::Intersect), Some(true));
                canvas.draw_path(
                    &enclosing_frame,
                    &self.faint().drawable(&tile_paint),
                );
                canvas.restore();
            } else {
                // create a path merging the the tile pattern outlines
                // and the enclosing rectangle
                let mut textured_builder = PathBuilder::new();
                fill_path_with_paint(
                    &enclosing_frame,
                    &tile_paint,
                    &mut textured_builder,
                    None,
                    None,
                );
                let textured_frame = textured_builder.detach();

                // intersect the rectangular texture with the user path
                // and fill with flat color
                let mut fill_paint = paint.clone();
                fill_paint.set_style(PaintStyle::Fill);

                // This branch fills geometry traced from `tile_paint`,
                // so a mark widened to a pixel
                // there has to be dimmed here
                // as well -- the colour comes from `paint`, which never
                // saw the widening.
                if thinned < 1.0 {
                    fill_paint.set_alpha_f(fill_paint.alpha_f() * thinned);
                }
                place_mark(&mut fill_paint, paint, mark, mark_alpha);
                if let Some(fill_path) =
                    stencil.op(&textured_frame, PathOp::Intersect)
                {
                    canvas.draw_path(
                        &fill_path,
                        &self.faint().drawable(&fill_paint),
                    );
                }
            }
        } else {
            canvas.draw_path(path, &self.faint().drawable(paint));
        }
    }

    /// The clip a fill or stroke of `path` in `paint` is recorded under as a
    /// [`CutOp`](layer::CutOp): the frame's clip, where it cuts the draw --
    /// covers part of where the draw can put coverage, and not all of it.
    ///
    /// `None` where it leaves the draw whole or misses it, which Skia draws
    /// alike clipped or not, and for the draws the op does not take: an
    /// axis-aligned rectangle filled, which Skia draws by a rectangle blitter
    /// the clip does not move; a texture, whose clip is its own outline; a
    /// shadow, a filter or a mask filter, each drawn through a layer of its
    /// own; and a blend that reaches past the draw, which is drawn through a
    /// picture of its own.
    fn cut_clip(
        &self,
        path: &Path,
        paint: &Paint,
        style: PaintStyle,
    ) -> Option<&Path> {
        #[cfg(test)]
        if !ROUTES_CUTS.get() {
            return None;
        }
        let clip = self.state.clip.as_ref()?;
        let matrix = &self.state.matrix;
        let rectangle = style == PaintStyle::Fill
            && matrix.rect_stays_rect()
            && path.is_rect().is_some();
        let plain = !rectangle
            && self.state.texture(style).is_none()
            && !matches!(
                self.state.global_composite_operation,
                BlendMode::SrcIn
                    | BlendMode::SrcOut
                    | BlendMode::DstIn
                    | BlendMode::DstOut
                    | BlendMode::DstATop
                    | BlendMode::Src
            )
            && paint.image_filter().is_none()
            && paint.mask_filter().is_none()
            && !self.casts_shadow();
        if !plain {
            return None;
        }
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
        // Re-check: cargo test the_page_edge_moves_pixels_far_inside_the_page
        // The page's own edge cuts a draw as a clip does -- Skia's analytic
        // anti-aliasing builds the path's edges against the device bounds
        // where they do not contain it -- and the op plays the draw back on
        // a raster of the page, so a draw the page also cuts moves inside
        // the clip as it does with no clip. Neither cutting the path to the
        // page first nor a raster wider than the page is exact; the
        // measurements are in docs/browser-differences.md.
        let cuts = |drawn: Rect| {
            !clip.conservatively_contains_rect(drawn)
                && clip.bounds().intersects(drawn)
        };
        // The estimate takes a mitred stroke as reaching its miter limit
        // everywhere, ten half-widths by default, which would route every
        // line drawn that near the clip. Where it says cut, a stroke's
        // outline settles it, which costs a stroker pass on a draw near the
        // clip and spares the op where the stroke does not reach it.
        let outlined = style != PaintStyle::Fill
            && paint.path_effect().is_none()
            && paint.stroke_width() > 0.0;
        let cut = cuts(drawn_bounds(path, paint, style, matrix, false))
            && (!outlined
                || cuts(drawn_bounds(path, paint, style, matrix, true)));
        cut.then_some(clip)
    }

    pub fn clip_path(&mut self, path: Option<Path>, rule: PathFillType) {
        let mut clip = match path {
            Some(path) => path.make_transform(&self.state.matrix),
            None => self.path.snapshot(),
        };
        clip.set_fill_type(rule);

        // update the clip with the intersection of the new path, unless it's
        // larger than the canvas itself in which case the whole clip is
        // discarded
        self.state.clip = self
            .state
            .clip
            .as_ref()
            .unwrap_or(&Path::rect(self.bounds, None))
            .op(&clip, PathOp::Intersect)
            .and_then(|path| match holds(&path, self.bounds) {
                true => None,
                false => Some(path),
            });

        self.with_recorder(|mut recorder| {
            recorder.set_clip(&self.state.clip);
        });
    }

    /// Whether `point` lies in `path`.
    ///
    /// `point` must already be in whatever space `path` is in, and the two
    /// callers differ. The context's own path is accumulated in device space
    /// -- every builder here transforms a segment as it is added -- so a
    /// query point goes in untouched. A `Path2D` is in its own user space and
    /// takes the current transform at query time, so its callers map the
    /// point through the inverse first.
    ///
    /// This used to do that mapping itself, for both. That is right for a
    /// `Path2D` and wrong for the context's path, which it left comparing a
    /// user-space point against device-space geometry the moment the matrix
    /// changed after the path was built -- and the standard says the point is
    /// *"treated as coordinates in the canvas coordinate space unaffected by
    /// the current transformation"* either way.
    pub fn hit_test_path(
        &mut self,
        path: &mut Path,
        point: impl Into<Point>,
        rule: Option<PathFillType>,
        style: PaintStyle,
    ) -> bool {
        let point = point.into();
        let rule = rule.unwrap_or(PathFillType::Winding);
        let prev_rule = path.fill_type();
        path.set_fill_type(rule);

        let is_in = match style {
            PaintStyle::Stroke => {
                let paint =
                    self.paint_for_drawing(PaintStyle::Stroke, Coverage::Path);
                let precision = 0.3; // this is what Chrome uses to compute this
                let scale = Matrix::scale((precision, precision));

                let mut traced_builder = PathBuilder::new();
                if fill_path_with_paint(
                    path,
                    &paint,
                    &mut traced_builder,
                    None,
                    Some(scale),
                ) {
                    traced_builder.detach().contains(point)
                } else {
                    path.contains(point)
                }
            }
            _ => path.contains(point),
        };

        path.set_fill_type(prev_rule);
        is_in
    }

    pub fn clear_rect(&mut self, rect: &Rect) {
        match self.state.matrix.map_rect(rect).0.contains(self.bounds) {
            // if rect fully encloses canvas, erase existing content (but
            // preserve CTM & clip path)
            true => self.with_recorder(|mut recorder| {
                recorder.set_bounds(self.bounds);
                recorder.set_state(self.state.matrix, &self.state.clip);
            }),

            // otherwise, paint over the specified region but preserve overdrawn
            // vectors
            // `Clear` is a blend mode, and Skia's SVG backend writes none
            // of those, so this lands as an opaque black rectangle unless
            // the draw is kept for rasterizing.
            // The same in each recording: a clear knows no colour.
            false => self.append_draw(
                blend_features(BlendMode::Clear, true),
                true,
                |canvas, _| {
                    let mut paint = Paint::default();
                    paint
                        .set_anti_alias(true)
                        .set_style(PaintStyle::Fill)
                        .set_blend_mode(BlendMode::Clear);
                    canvas.draw_rect(rect, &paint);
                },
            ),
        }
    }

    pub fn draw_picture(
        &mut self,
        picture: &Picture,
        src_rect: &Rect,
        dst_rect: &Rect,
        source: VectorFeatures,
    ) {
        self.draw_picture_costing(picture, None, src_rect, dst_rect, source, 1)
    }

    /// As [`Self::draw_picture`], charging this page for what replaying the
    /// source costs rather than for one draw.
    ///
    /// A nested picture is replayed whole every time the page around it is,
    /// so the charge is what makes a page that keeps drawing copies of itself
    /// declare its own cost. See `PageRecorder::replay_cost`.
    ///
    /// `entries` is the source page's own list where the picture is a
    /// canvas's, each entry with what it asks of a vector backend, so that a
    /// draw through its paint holds the page as it was recorded and each
    /// target composites its groups as it needs -- see `LayerRec::open_on`.
    /// `None` for a picture with no page behind it, which is then one entry
    /// asking for what `source` names.
    pub(crate) fn draw_picture_costing(
        &mut self,
        picture: &Picture,
        entries: Option<Entries>,
        src_rect: &Rect,
        dst_rect: &Rect,
        source: VectorFeatures,
        cost: usize,
    ) {
        self.recorder.borrow_mut().charge_replay(cost);
        let paint = self.paint_for_image();
        let mag = Point::new(
            dst_rect.width() / src_rect.width(),
            dst_rect.height() / src_rect.height(),
        );
        let mut matrix = Matrix::new_identity();
        matrix.pre_scale((mag.x, mag.y), None).pre_translate((
            dst_rect.x() / mag.x - src_rect.x(),
            dst_rect.y() / mag.y - src_rect.y(),
        ));

        // The picture carries the source canvas's own features: a conic
        // gradient drawn on one canvas and replayed into another is still a
        // conic gradient when a backend comes to write it out, and a
        // `destination-out` inside it is still a blend mode -- one inside a
        // group, since a source holding a blend is drawn through a layer.
        // And where it is drawn through its paint, it is drawn through a
        // layer -- see `carries` below -- which a backend may not have.
        let carried = match carries(source, &paint) {
            true => VectorFeatures::PICTURE_PAINT,
            false => VectorFeatures::PLAIN,
        };
        let features = self
            .vector_features(&paint, None)
            .with(source)
            .with(source.nested())
            .with(carried);

        // Drawn through its paint as a group of our own, which composites
        // the alpha exactly on pixels and as an opacity in a document -- see
        // `LayerRec::open_on` -- wherever the blend composites only where
        // the source lands, which `render_to_canvas` draws as it is: its
        // shadow first, as a group of its own offset from it, then the
        // source. A source holding a cut draw is drawn as its entries where
        // the paint has nothing to say, through no layer, so that each target
        // clips the draw its own way -- see `Composite::Plain` -- where the
        // picture `render_to_canvas` draws holds one way for all. So is one
        // from a page that keeps a pair of recordings, so that a document is
        // written from the source's own and not from the colours placed for
        // its pixels, which the picture holds -- and so is such a page whose
        // draws have not forked its recordings, so that it is drawn as it is
        // once they have: by the picture, the transforms are composed
        // otherwise, and a PDF wrote one coordinate an ulp apart. So is a
        // source the destination crops, so that a raster crops it by
        // coverage rather than cutting its paths -- see `Group::confine`.
        let drawn = self.faint().drawable(&paint).into_owned();
        let through_paint = carries(source, &drawn);
        let cut = entries.as_ref().is_some_and(|entries| {
            entries.apart
                || holds_a_cut(&entries.layers)
                || holds_a_pair(&entries.layers)
        });
        let crops = !dst_rect.contains(matrix.map_rect(picture.cull_rect()).0);
        if (through_paint || cut || crops)
            && !covers_beyond_the_draw(self.state.global_composite_operation)
        {
            let contents_matrix = Matrix::concat(&self.state.matrix, &matrix);
            let contents = entries.unwrap_or_else(|| {
                Entries::new(
                    vec![Layer::Picture(picture.clone())],
                    vec![source],
                )
            });
            let shadow = self
                .paint_for_shadow(&paint)
                .map(|shadow| self.faint().drawable(&shadow).into_owned());
            let offset = self.state.shadow_offset;
            let steps = self.exact_steps();
            // The shadow is cast through the exact chain on pixels: after
            // the steps of the source's own filter where it has one, which
            // it is cast from, as the HTML Standard orders the two. Under a
            // filter with no steps of its own, it is cast from the filtered
            // source Skia's layer makes.
            let cast_steps = match paint.image_filter() {
                Some(_) => steps.clone(),
                None => Some(Arc::from([])),
            };
            let shadow_steps = cast_steps.and_then(|filter_steps| {
                self.shadow_cast_in(&paint, Recording::Shared, false).map(
                    |cast| {
                        filter_steps
                            .iter()
                            .chain(self.shadow_steps(&cast).iter())
                            .cloned()
                            .collect::<Arc<[Step]>>()
                    },
                )
            });
            // The pixels composite the canvas through its colour filter
            // built for them -- see `Self::pixels_paint`.
            let pixels = self.pixels_paint(&drawn);
            self.with_recorder(|mut recorder| {
                let mut group = |paint: Paint,
                                 pixels_paint: Option<Paint>,
                                 shift: Point,
                                 own: bool| {
                    let rec = LayerRec {
                        paint: Some(paint),
                        pixels_paint,
                        ..LayerRec::default()
                    };
                    let chain = match own {
                        true => steps.clone(),
                        false => shadow_steps.clone(),
                    };
                    let composite = match chain {
                        Some(steps) => Composite::Chain {
                            steps,
                            rec,
                            reach: None,
                            coverage: None,
                        },
                        None if own && !through_paint => Composite::Plain,
                        None => Composite::Layer(rec),
                    };
                    recorder.append_group(
                        composite,
                        Some(*dst_rect),
                        contents_matrix,
                        contents.clone(),
                        features,
                        shift,
                    );
                };
                if let Some(shadow) = shadow {
                    group(shadow, None, offset, false);
                }
                group(drawn, pixels, Point::default(), true);
            });
            return;
        }

        let pixels = self.pixels_paint(&paint);
        self.render_to_canvas(
            &paint,
            pixels.as_ref(),
            features,
            None,
            None,
            None,
            |canvas, paint, _| {
                // A paint here makes Skia draw the picture through a temporary
                // layer, which is what keeps the source's own compositing to
                // itself: a `destination-out` inside it used to erase what was
                // already on this canvas, where drawing the source's pixels --
                // which is what the Canvas API says this does -- leaves them
                // alone. Only where the source needs it, because the SVG
                // backend omits a picture drawn with a paint altogether -- so
                // such a draw is `PICTURE_PAINT` and rasterized there -- and a
                // source with nothing but source-over in it has nothing to
                // leak. An explicit `save_layer` will not do: Skia's recording
                // optimizer folds one wrapping a single draw back into the
                // draw, layer and all.
                //
                // Past that, the paint is only worth carrying if it says
                // something -- alpha, a blend mode, a filter, a colour filter.
                // A faint paint drawn as its opaque stand-in is carried as
                // well: its alpha is in a colour filter -- see
                // `Faint::drawable`.
                let paint = self.faint().drawable(paint);
                let paint = carries(source, &paint).then_some(&*paint);
                canvas.save();
                canvas.clip_rect(dst_rect, ClipOp::Intersect, true);
                canvas.draw_picture(picture, Some(&matrix), paint);
                canvas.restore();
            },
        );
    }

    /// Draws `image`, rasterizing only the part of it this draw can show.
    ///
    /// For a source that carries a nested picture and would otherwise be
    /// rasterized whole. The subset is expanded to whatever a filter reads,
    /// and both rectangles move with it, so the pixels that land on the page
    /// are the ones that would have landed there anyway.
    pub fn draw_nested_image(
        &mut self,
        image: &Image,
        picture: Option<&Picture>,
        src_rect: &Rect,
        dst_rect: &Rect,
    ) {
        let Some(subset) = self.visible_source_rect(src_rect, dst_rect) else {
            return self.draw_image(&flatten_image(image), src_rect, dst_rect);
        };

        // The destination the subset covers, so the drawing does not move.
        let Some(to_dst) = Matrix::rect_2_rect(src_rect, dst_rect, None) else {
            return self.draw_image(&flatten_image(image), src_rect, dst_rect);
        };
        let sub = Rect::from(subset);
        let dst = to_dst.map_rect(sub).0;

        match rasterize_region(image, picture, subset) {
            Some(part) => {
                let src = Rect::from_wh(sub.width(), sub.height());
                self.draw_image(&part, &src, &dst)
            }
            // Whole rather than deferred: a picture that keeps travelling is
            // the defect this exists to stop.
            None => self.draw_image(&flatten_image(image), src_rect, dst_rect),
        }
    }

    /// The part of a source image this draw can actually put on the page, or
    /// `None` when that is most of it or cannot be worked out.
    ///
    /// Only asked when a source is about to be rasterized because it carries
    /// nesting -- see `node::image::Source::of`. Rasterizing the whole page to
    /// show a sliver of it is what this avoids: a 1200x1200 source clipped to
    /// 200x20 produced 5.49 MB and used about 0.09 of it.
    ///
    /// The clip alone is not the answer. A filter reads outside the pixels it
    /// writes, so cropping to the visible rectangle would starve a blur at its
    /// edges and leave a seam. `filter_bounds` run backwards asks the filter
    /// itself how far it reaches, which is the only source of that number that
    /// stays right when the filter changes.
    fn visible_source_rect(
        &mut self,
        src_rect: &Rect,
        dst_rect: &Rect,
    ) -> Option<IRect> {
        let matrix = self.state.matrix;
        let page = Rect::from_size(self.bounds.size());
        // The clip is kept in device space, and its absence means the page.
        let clip = self
            .state
            .clip
            .as_ref()
            .map_or(page, |path| path.bounds().to_owned());

        let mut visible = matrix.map_rect(dst_rect).0;
        if !visible.intersect(clip) || !visible.intersect(page) {
            return None;
        }

        // What the filter needs read in, rather than what it writes out.
        let paint = self.paint_for_image();
        let wanted = match paint.image_filter() {
            Some(filter) => filter.filter_bounds(
                RoundOut::<IRect>::round_out(&visible),
                &matrix,
                MapDirection::Reverse,
                None,
            ),
            None => RoundOut::<IRect>::round_out(&visible),
        };

        // Back to the image's own pixels: device -> user -> source.
        let to_user = matrix.invert()?;
        let to_source = Matrix::rect_2_rect(dst_rect, src_rect, None)?;
        let mut needed =
            to_source.map_rect(to_user.map_rect(Rect::from(wanted)).0).0;
        if !needed.intersect(*src_rect) {
            return None;
        }
        let needed = RoundOut::<IRect>::round_out(&needed);

        // Only worth the subset when it saves most of the work. Below that the
        // rasterization is the same size and the extra step is loss.
        let whole = src_rect.width() * src_rect.height();
        let part = needed.width() as f32 * needed.height() as f32;
        match whole > 0.0 && part / whole < 0.5 {
            true => Some(needed),
            false => None,
        }
    }

    pub fn draw_image(
        &mut self,
        image: &Image,
        src_rect: &Rect,
        dst_rect: &Rect,
    ) {
        let scaling = ScalingOperation::for_matrix(&Matrix::concat(
            &self.state.matrix,
            &Matrix::rect_2_rect(src_rect, dst_rect, None)
                .unwrap_or_else(Matrix::new_identity),
        ));
        let paint = self.paint_for_image();
        let features = self.vector_features(&paint, None);
        let sampling = self.state.sampling_filter.sampling_for(scaling);

        // A shadow the chain casts is filed as a group of its own before the
        // image, which is then drawn as it is casting none -- see
        // `Self::draw_path`. Drawn with it, the image under a filter of its
        // own left the group below for `render_to_canvas`, which composites
        // its alpha before the filter rather than after.
        let cast_apart = self.casts_shadow() && {
            let draw = |canvas: &SkCanvas, paint: &Paint, _: Pass| {
                canvas.draw_image_rect_with_sampling_options(
                    image,
                    Some((src_rect, Strict)),
                    dst_rect,
                    sampling,
                    &self.faint().drawable(paint),
                );
            };
            self.files_shadow_through_chain(&paint, features, None, &draw)
        };
        if cast_apart {
            let (color, space) = self.state.shadow_color.clone();
            self.state.shadow_color.0.a = 0.0;
            self.draw_image(image, src_rect, dst_rect);
            self.state.shadow_color = (color, space);
            return;
        }

        // Under a filter the crate runs itself, the image is drawn unfiltered
        // and opaque as a group of its own, which runs the filter on its
        // pixels exactly and composites the result through the alpha and the
        // blend, in the order the Canvas drawing model applies them. A shadow
        // filed above is not here; one it could not file is Skia's, and so is
        // a blend that covers beyond the draw, which composites the whole
        // canvas.
        if let Some(steps) = self.exact_steps()
            && !covers_beyond_the_draw(self.state.global_composite_operation)
            && !self.casts_shadow()
        {
            let record = |paint: &Paint| {
                let mut drawn = paint.clone();
                drawn.set_image_filter(None).set_alpha_f(1.0);
                let mut recorder = PictureRecorder::new();
                let canvas = recorder.begin_recording(*dst_rect, false);
                canvas.draw_image_rect_with_sampling_options(
                    image,
                    Some((src_rect, Strict)),
                    dst_rect,
                    sampling,
                    &drawn,
                );
                recorder.finish_recording_as_picture(None)
            };
            // Recorded for each recording where the pixels draw the paint's
            // colour filter built for them -- see `Self::pixels_paint`. The
            // image drawn inside asks for nothing: its filter and its blend
            // are the group's.
            let entry = match self.pixels_paint(&paint) {
                None => record(&paint).map(Layer::Picture),
                Some(pixels) => record(&pixels)
                    .zip(record(&paint))
                    .map(|pictures| Layer::Targeted(Arc::new(pictures))),
            };
            let contents = entry
                .map(|entry| (entry, VectorFeatures::PLAIN))
                .into_iter()
                .collect();
            let mut through = Paint::default();
            through
                .set_alpha_f(paint.alpha_f())
                .set_blend_mode(self.state.global_composite_operation)
                .set_image_filter(paint.image_filter());
            let through = self.faint().drawable(&through).into_owned();
            let matrix = self.state.matrix;
            self.with_recorder(|mut recorder| {
                recorder.append_group(
                    Composite::Chain {
                        steps,
                        rec: LayerRec {
                            paint: Some(through),
                            ..LayerRec::default()
                        },
                        reach: None,
                        coverage: None,
                    },
                    // Unconfined: the image's own edge is where it ends,
                    // and a clip to it would take its partial pixels a
                    // second time. The picture is recorded within it.
                    None,
                    matrix,
                    contents,
                    features,
                    Point::default(),
                );
            });
            return;
        }

        let pixels = self.pixels_paint(&paint);
        self.render_to_canvas(
            &paint,
            pixels.as_ref(),
            features,
            None,
            None,
            None,
            |canvas, paint, _| {
                canvas.draw_image_rect_with_sampling_options(
                    image,
                    Some((src_rect, Strict)),
                    dst_rect,
                    sampling,
                    &self.faint().drawable(paint),
                );
            },
        );
    }

    pub fn page(&self) -> Page {
        self.recorder.borrow_mut().page()
    }

    pub fn page_for_export(
        &self,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Page {
        self.recorder.borrow_mut().page_for_export(opts, engine)
    }

    pub fn image(&self) -> Option<Image> {
        self.recorder
            .borrow_mut()
            .image(self.canvas_color_type, &self.canvas_color_space)
    }

    /// This canvas as an image for another canvas to draw, rasterized on the
    /// spot when `flatten`. See `PageRecorder::image_flattened`.
    pub fn source_image(&self, flatten: bool) -> Option<Image> {
        self.recorder.borrow_mut().image_flattened(
            flatten,
            self.canvas_color_type,
            &self.canvas_color_space,
        )
    }

    /// Charges this page for replaying a source that costs `cost`.
    pub fn charge_replay(&mut self, cost: usize) {
        self.recorder.borrow_mut().charge_replay(cost);
    }

    /// What replaying this canvas as a nested picture would cost; see
    /// `PageRecorder::replay_cost`.
    pub fn replay_cost(&self) -> usize {
        self.recorder.borrow().replay_cost()
    }

    /// Takes `&self` because a source is resolved while its destination is
    /// already borrowed, and the two are one object when a canvas is drawn
    /// into itself. A `&mut self` here makes that draw panic on the second
    /// borrow. The mutation is the recorder's own `RefCell`, as it is for
    /// `source_image` beside this.
    pub fn picture(&self) -> Option<Picture> {
        self.recorder.borrow_mut().page().picture(None)
    }

    /// The page's own list of entries, for another canvas to draw this one
    /// through a paint without flattening it -- see
    /// [`Self::draw_picture_costing`].
    pub(crate) fn entries(&self) -> Entries {
        let mut recorder = self.recorder.borrow_mut();
        let page = recorder.page();
        Entries {
            apart: recorder.dual(),
            ..Entries::shared(page.layers, page.features)
        }
    }

    /// The page flattened for another canvas to draw, with what an SVG
    /// export can do with it.
    ///
    /// Flattening loses the per-layer marks, so the verdict travels beside
    /// the picture and the destination applies it to the whole draw.
    pub fn picture_with_features(
        &mut self,
    ) -> Option<(Picture, VectorFeatures)> {
        let page = self.recorder.borrow_mut().page();
        let features = page.vector_features();
        page.picture(None).map(|picture| (picture, features))
    }

    /// The pixels of `crop`, played back and read for `opts` on `engine`.
    ///
    /// In an autorelease pool of its own, as every entry point that touches
    /// Metal is -- see [`gpu::autorelease`]. Reading a GPU page back makes
    /// Metal objects that are autoreleased; read from Node's main thread,
    /// which has no pool, they were never drained, and each whole-page read
    /// of a 2000 by 1600 canvas kept 12.2 MB, about the size of what it
    /// read.
    pub fn pixels(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
    ) -> Result<Vec<u8>, String> {
        gpu::autorelease(|| {
            self.recorder.borrow_mut().pixels(crop, opts, engine)
        })
    }

    /// How many bytes [`Context2D::pixels_into`] writes for `crop` read for
    /// `opts`.
    pub(crate) fn pixels_size(
        crop: IRect,
        opts: &ExportOptions,
    ) -> Result<usize, String> {
        PageRecorder::pixels_size(crop, opts, opts.alpha_type)
    }

    /// [`Context2D::pixels`] into `pixels`, which holds zeros and
    /// [`Context2D::pixels_size`] bytes: the read made straight into the
    /// caller's memory, with no buffer of its own between.
    pub(crate) fn pixels_into(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let alpha_type = opts.alpha_type;
        gpu::autorelease(|| {
            self.recorder
                .borrow_mut()
                .pixels_into(crop, opts, engine, alpha_type, pixels)
        })
    }

    /// As [`Context2D::pixels`], with the destination alpha mode chosen
    /// by the caller.
    pub fn pixels_as(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
        alpha_type: AlphaType,
    ) -> Result<Vec<u8>, String> {
        gpu::autorelease(|| {
            self.recorder
                .borrow_mut()
                .pixels_as(crop, opts, engine, alpha_type)
        })
    }

    pub fn blit_pixels(
        &mut self,
        image_data: ImageData,
        src_rect: &Rect,
        dst_rect: &Rect,
    ) {
        // Discarded, not propagated: `putImageData` returns undefined and
        // has no channel for this.
        let _ = self.blit_pixels_raw(
            image_data.image_info(),
            image_data.buffer,
            src_rect,
            dst_rect,
        );
    }

    /// As [`Context2D::blit_pixels`], taking the layout and bytes directly
    /// rather than an [`ImageData`].
    ///
    /// [`ImageData`] carries its own alpha mode -- unpremultiplied unless the
    /// `ImageData` says otherwise, which is what `putImageData` means for
    /// the standard's kind -- so a premultiplied readback goes back in as
    /// the colour it was. A Rust caller supplies its own [`ImageInfo`] and
    /// can write premultiplied or high-depth pixels directly.
    /// Returns `false` when Skia declines to wrap `buffer` in `info`'s
    /// layout, in which case nothing was drawn. The Node `putImageData`
    /// has no way to report that and ignores it; the Rust API turns it into
    /// an error rather than claiming a write that did not happen.
    pub fn blit_pixels_raw(
        &mut self,
        info: ImageInfo,
        buffer: Data,
        src_rect: &Rect,
        dst_rect: &Rect,
    ) -> bool {
        // works just like draw_image in terms of src/dst rects, but clears the
        // dst_rect and then draws without clips, transforms, alpha,
        // blend, or shadows
        if let Some(bitmap) =
            started::raster_from_data(&info, buffer, info.min_row_bytes())
        {
            self.push(); // cache matrix & clip in self.state
            self.with_recorder(|mut recorder| {
                recorder.append_unframed(|canvas, _| {
                    let paint = Paint::default();
                    let mut eraser = Paint::default();
                    eraser.set_blend_mode(BlendMode::Clear);
                    canvas.draw_image_rect(
                        &bitmap,
                        Some((src_rect, Strict)),
                        dst_rect,
                        &eraser,
                    );
                    canvas.draw_image_rect(
                        &bitmap,
                        Some((src_rect, Strict)),
                        dst_rect,
                        &paint,
                    );
                });
            });
            self.pop(); // restore discarded matrix & clip
            return true;
        }
        false
    }

    /// Applies a font the library has already matched to a typeface.
    ///
    /// Built onto the current character style rather than replacing it, so
    /// the spacing and decoration set separately survive a font change --
    /// which is what they did when this cloned the style inside the library.
    pub fn set_font(&mut self, font: &ResolvedFont) {
        let (spec, typeface) = &**font;
        let mut style = self.state.text.char_style.clone();
        style.set_typeface(typeface.clone());
        style.set_font_families(&spec.collection_names);
        style.set_font_style(spec.style());
        // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
        // Re-check: "17.3px measures as 17.296875px on FreeType" in
        // tests/suite/text.test.js, on Linux, goes red once fixed.
        // Skia's FreeType scaler sets the face's size as 26.6 fixed point
        // through `SkScalarToFDot6`, a truncation, in `FT_Set_Char_Size`
        // (`SkFontHost_FreeType.cpp`), and corrects its matrix for the size
        // chosen only below 1px: on Linux every size is laid out at the 64th
        // of a pixel below it, while its metrics take the size asked for.
        // Not worked around, as Chrome on Linux takes its advances from the
        // same scaler -- see docs/browser-differences.md.
        style.set_font_size(spec.size);
        style.reset_font_features();
        for (feat, val) in &spec.features {
            style.add_font_feature(feat, *val);
        }

        // The serialized form rather than the canonical one: the getter
        // this feeds is specified to report the font without its
        // `line-height` component or any component at its initial value.
        // The canonical string keys the cache the spec was just fetched
        // from and says more than a caller may read back.
        self.state.font = spec.serialized.clone();
        self.state.font_variant = spec.variant.clone();
        self.state.font_width = spec.width;
        // A font that sets the text settings to what they hold changes
        // nothing laid out, so the typography built from them stays kept: a
        // drawing loop sets the same font before every draw. The style's own
        // equality leaves its typeface out, and need not compare it: the
        // build sets the face the fonts registered now resolve to, and a
        // registration changes their epoch, which builds the typography
        // again whatever the settings hold.
        let text = &self.state.text;
        if text.line_height == spec.line_height
            && text.oblique_angle == spec.oblique_angle()
            && text.weight == spec.fractional_weight
            && text.char_style == style
        {
            return;
        }
        self.state.text.edit().char_style = style;
        self.state.text.edit().line_height = spec.line_height;
        self.state.text.edit().oblique_angle = spec.oblique_angle();
        self.state.text.edit().weight = spec.fractional_weight;
    }

    /// Applies the font a CSS `font` shorthand names, answering whether it
    /// parsed.
    ///
    /// Read by the crate's own shorthand parser, so the two surfaces take one
    /// grammar from one implementation, and remembered under the string as
    /// given: a drawing loop sets the same few strings over and over, and a
    /// hit is one lookup, which skips the parse and the match. A string that
    /// does not parse, or names no family the library has, leaves the font
    /// alone, as the Canvas API says.
    pub fn set_font_css(&mut self, css: &str) -> bool {
        if let Some(font) = FontLibrary::with_shared(|lib| lib.resolved(css)) {
            self.set_font(&font);
            return true;
        }
        let Some(spec) = font_spec_from_css(css) else {
            return false;
        };
        let font = FontLibrary::with_shared(|lib| {
            let font = lib.resolve(spec)?;
            lib.remember(css.to_string(), font.clone());
            Some(font)
        });
        if let Some(font) = font {
            self.set_font(&font);
        }
        true
    }

    /// Matches a specification against the font library and applies it.
    ///
    /// The path a caller with a specification in hand takes, as
    /// `Context2D::set_font` with a [`Font`](crate::context2d::Font) does; one
    /// starting from a CSS string takes [`set_font_css`](Self::set_font_css).
    pub fn set_font_spec(&mut self, spec: FontSpec) {
        if let Some(font) = FontLibrary::with_shared(|lib| lib.resolve(spec)) {
            self.set_font(&font);
        }
    }

    pub fn set_font_variant(
        &mut self,
        variant: &str,
        features: &[(String, i32)],
    ) {
        // The variant already set changes nothing -- see `set_font`.
        let held = self.state.text.char_style.font_features();
        if self.state.font_variant == variant
            && held.len() == features.len()
            && held.iter().zip(features).all(|(held, (name, value))| {
                held.name() == name && held.value() == *value
            })
        {
            return;
        }
        self.state.text.edit().char_style.reset_font_features();
        for (feat, val) in features {
            self.state
                .text
                .edit()
                .char_style
                .add_font_feature(feat, *val);
        }
        self.state.font_variant = variant.to_string();
    }

    pub fn set_font_width(&mut self, width: Width) {
        let style = self.state.text.char_style.font_style();
        let font_style = FontStyle::new(style.weight(), width, style.slant());
        // The width already set changes nothing -- see `set_font`.
        if self.state.font_width == width && style == font_style {
            return;
        }
        self.state.text.edit().char_style.set_font_style(font_style);
        self.state.font_width = width;
    }

    pub fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        width: Option<f32>,
        style: PaintStyle,
    ) {
        let paint = self.paint_for_drawing(style, Coverage::Path);
        let mut typesetter = Typesetter::new(&self.state, text, width);
        let origin = Point::new(x, y);

        if self.state.texture(style).is_some() {
            // if dye is a texture, convert text to path first
            self.draw_path(Some(typesetter.path(origin)), style, None);
        } else {
            let kept = typesetter.laid_out();
            // What the colour glyphs this library draws itself ask for, beside
            // what the paint does -- see `typography::colour_features`.
            let colour = match &kept {
                Some(kept) => kept.colour_features(&paint),
                None => typesetter.colour_features(&paint),
            };
            let features =
                self.vector_features(&paint, Some(style)).with(colour);
            let global_alpha = self.state.global_alpha as f32;
            let faint = self.faint();
            // The run kept whole, where it holds colour glyphs, to draw them
            // where the page is played back -- see `layer::ColourText`.
            // Every colour glyph composites through a layer of its own.
            let colour = colour.refused_by(VectorFeatures::LAYER).then(|| {
                let (run, offset, condensation) =
                    typesetter.replayed(kept.as_deref());
                Replay::Text {
                    run,
                    at: origin,
                    offset,
                    condensation,
                    global_alpha,
                    faint,
                }
            });
            // On a page that keeps a recording for pixels, text there takes
            // its colour exactly through a dye -- see `exact_text_paint` --
            // rather than the shader a fill takes: Skia sets a glyph mask's
            // contrast from the luminance of the paint's colour, which that
            // shader does not report, so text through one is
            // read as mid-grey and drawn with heavier edges -- 360 pixels of
            // a bold 40px run on a Display P3 F32 page.
            let dyed = self.exact_text_paint(&paint, style);
            let dyed = dyed.as_ref();
            // Kept only for a canvas's pattern, the one dye that takes it.
            let replay =
                matches!(self.state.dye(style), Dye::Pattern(_)).then(|| {
                    let (run, offset, condensation) =
                        typesetter.replayed(kept.as_deref());
                    Replay::Text {
                        run,
                        at: origin,
                        offset,
                        condensation,
                        global_alpha,
                        faint,
                    }
                });
            // A run filed through a filter chain answers what it can ink, which
            // bounds the chain's layer -- see `files_through_chain` -- and the
            // first time it is drawn it is laid out as runs to answer it,
            // which draw what `Paragraph::paint` does.
            let reached = Cell::new(None);
            let chained = self.chains_text();
            // A kept run is drawn when a target first plays it, from the
            // blobs it keeps for the paint -- see `layer::DeferredPicture` --
            // as the draw below would have drawn it: at its offset, not
            // condensed, and answering what it can ink as it does.
            let defer = |paint: &Paint| {
                let kept = kept.as_ref()?;
                #[cfg(test)]
                if !DEFERS.get()
                    || PAINTER_OVERRIDE.get() == Some(Painter::Paragraph)
                {
                    return None;
                }
                if kept.condensation() != 1.0 {
                    return None;
                }
                let (drawn, reach) = kept.deferred(
                    origin + kept.offset(),
                    paint,
                    global_alpha,
                    faint,
                )?;
                // The draw is recorded under the transform alone, which maps
                // what it can ink as the recording canvas would.
                let reach =
                    reach.map(|reach| self.state.matrix.map_rect(reach).0);
                #[cfg(test)]
                if reach.is_some() {
                    TEXT_REACHED.set(reach);
                }
                reached.set(reach);
                Some((drawn, reach))
            };
            let own_colours = || match &kept {
                Some(kept) => kept.draws_own_colours(),
                None => typesetter.draws_own_colours(),
            };
            self.render_to_canvas(
                &paint,
                dyed,
                features,
                replay,
                Some(TextRun {
                    lines: typesetter.names_a_line_colour(),
                    reached: &reached,
                    kept: kept.as_ref().map(|kept| kept.drawn()),
                    anchor: kept.as_ref().map_or_else(
                        || Matrix::translate(origin),
                        |kept| {
                            drawn_from(
                                origin,
                                kept.offset(),
                                kept.condensation(),
                            )
                        },
                    ),
                    defer: Some(&defer),
                    colour,
                    own_colours: &own_colours,
                }),
                self.state.dye(style).gradient().map(|spec| GradientDye {
                    spec,
                    path: None,
                    whole_pixels: false,
                }),
                |canvas, paint, pass| {
                    // The kept run, or, the first time a run is drawn, the run
                    // laid out for this paint -- see `Typesetter::laid_out`.
                    let run = match &kept {
                        Some(kept) => DrawnRun::Kept(kept.clone()),
                        None if chained => {
                            typesetter.drawn_as_runs(&faint.drawable(paint))
                        }
                        None => typesetter
                            .drawn_once(&faint.drawable(paint), canvas),
                    };
                    let (offset, condensation) =
                        (run.offset(), run.condensation());
                    // In a test, an undecorated run can be laid out for this
                    // paint and drawn by `Paragraph::paint` instead, as the
                    // oracle the kept runs are held to. A decorated one is
                    // drawn from its runs either way, its
                    // lines between them.
                    #[cfg(test)]
                    let oracle = (PAINTER_OVERRIDE.get()
                        == Some(Painter::Paragraph)
                        && typesetter.decoration(1.0).is_none())
                    .then(|| typesetter.layout(&faint.drawable(paint)));
                    let draw = |canvas: &SkCanvas, at: Point| {
                        #[cfg(test)]
                        if let Some(layout) = &oracle {
                            layout.paragraph.paint(canvas, at);
                            return reach_the_clip(canvas);
                        }
                        // A shadow whose colour took the `globalAlpha` draws
                        // a named line colour without it.
                        let lines = match pass {
                            Pass::Draw => global_alpha,
                            Pass::AlphaInShadow => 1.0,
                        };
                        // What the run can ink is read only to bound a
                        // filter chain's layer -- see `files_through_chain`
                        // -- so a draw outside one does not work it out. The
                        // tests read it on every draw, to hold it to the ink.
                        let reach = run.paint(
                            canvas,
                            at,
                            paint,
                            lines,
                            faint,
                            chained || cfg!(test),
                        );
                        let reach = reach.map(|reach| {
                            canvas.local_to_device_as_3x3().map_rect(reach).0
                        });
                        #[cfg(test)]
                        if reach.is_some() {
                            TEXT_REACHED.set(reach);
                        }
                        reached.set(reach);
                        reach_the_clip(canvas);
                    };
                    match condensation {
                        // Chrome condenses with a plain anisotropic transform
                        // rather than a narrower face, and the pen goes with
                        // it: at 200px with `lineWidth` 12, a half-width
                        // `strokeText("H")` draws its stems 6 pixels wide and
                        // its crossbar still 12. A canvas scale is that, so the
                        // stroke needs no separate handling.
                        1.0 => draw(canvas, origin + offset),
                        scale => {
                            canvas.save();
                            canvas.translate(origin).scale((scale, 1.0));
                            draw(canvas, offset);
                            canvas.restore();
                        }
                    }
                },
            );
        }
    }

    pub fn outline_text(&self, text: &str, width: Option<f32>) -> Path {
        Typesetter::new(&self.state, text, width).path((0.0, 0.0))
    }

    /// Structured measurements of `text`.
    ///
    /// The one measurement path there is: the Rust API returns these, and the
    /// Node binding builds `measureText`'s object straight from them. It used
    /// to have a sibling that produced the same numbers as a
    /// `serde_json::Value` for the binding alone, which is what the two had
    /// to be kept from drifting apart.
    pub fn measure_text_extents(
        &self,
        text: &str,
        width: Option<f32>,
    ) -> TextExtents {
        Typesetter::new(&self.state, text, width).extents()
    }

    /// The ascent, descent and line gap of the current font's first
    /// available face, at the alphabetic baseline, whatever the string.
    pub fn font_metrics(&self) -> FontMetrics {
        Typesetter::new(&self.state, "", None).font_metrics()
    }

    /// Measurements of `text` except its ink, and what the ink is worked
    /// out from, for a caller that may never read it. See
    /// [`Typesetter::measure`].
    pub fn measure_text_lazily(
        &self,
        text: &str,
        width: Option<f32>,
    ) -> (TextExtents, PendingInk) {
        let inputs = TypesetInputs::new(&self.state, text, width);
        let (extents, measured) = inputs.typesetter().measure();
        (extents, PendingInk::new(inputs, measured))
    }

    /// What this canvas's pixels do with a paint too faint for eight bits.
    pub(crate) fn faint(&self) -> Faint {
        self.faint
    }

    pub fn paint_for_drawing(
        &mut self,
        style: PaintStyle,
        coverage: Coverage,
    ) -> Paint {
        let mut paint = self.state.paint.clone();

        // 1. Apply Skia colorFilter (operates on source colors before blend)
        if let Some(cf) = &self.state.skia_color_filter {
            paint.set_color_filter(cf.on(self.onto().as_ref()));
        }

        // 2. Apply CSS filter (image_filter + mask_filter)
        //
        // A shader asks for the drawn result to be blurred rather than the
        // shape's coverage. The coverage blur is only the same picture while
        // the paint is one flat colour; a pattern or a gradient has
        // structure of its own that a coverage blur leaves untouched, and a
        // shader painting nothing outside its own bounds has nothing for the
        // blur to spread into. See `Filter::filters_for`. And where the
        // coverage blur differs from Chrome's or costs more, the result is
        // blurred for a flat colour too -- see `Coverage`.
        //
        // A filter that leaves the draw untouched puts nothing on the paint,
        // which holds no filter of its own, so it is not asked: setting none
        // twice was 7% of what recording a plain fill cost.
        if !self.state.filter.is_none() {
            let has_shader = !matches!(self.state.dye(style), Dye::Color(..));
            let rendered =
                has_shader || !self.state.filter.blurs_coverage(coverage);
            self.state
                .filter
                .mix_into(&mut paint, self.state.matrix, rendered);
        }

        // 3. Compose Skia imageFilter with CSS imageFilter (if both present)
        if let Some(skia_imgf) = &self.state.skia_image_filter {
            let skia_imgf = skia_imgf.on(self.onto().as_ref());
            let final_image_filter = match paint.image_filter() {
                Some(css_imgf) => image_filters::compose(skia_imgf, css_imgf),
                None => Some(skia_imgf),
            };
            paint.set_image_filter(final_image_filter);
        }

        // 4. Apply Skia maskFilter (coverage blur with BlurStyle) and dither.
        if let Some(mf) = &self.state.skia_mask_filter {
            paint.set_mask_filter(mf.clone());
        }
        paint.set_dither(self.state.dither);

        self.state.dye(style).mix_into(
            &mut paint,
            self.state.global_alpha as f32,
            self.state.sampling_filter,
            &self.canvas_color_space,
            self.onto().as_ref(),
        );
        paint.set_style(style);

        if style == PaintStyle::Stroke && !self.state.line_dash_list.is_empty()
        {
            // if marker is set, apply the 1d_path_effect instead of the
            // dash_path_effect
            let effect = match &self.state.line_dash_marker {
                Some(path) => {
                    let marker = match path.is_last_contour_closed() {
                        true => path.clone(),
                        false => {
                            let mut traced_builder = PathBuilder::new();
                            fill_path_with_paint(
                                path,
                                &paint,
                                &mut traced_builder,
                                None,
                                None,
                            );
                            traced_builder.detach()
                        }
                    };
                    path_1d_path_effect::new(
                        &marker,
                        self.state.line_dash_list[0],
                        self.state.line_dash_offset,
                        self.state.line_dash_fit,
                    )
                }
                None => dash_path_effect::new(
                    &self.state.line_dash_list,
                    self.state.line_dash_offset,
                ),
            };

            paint.set_path_effect(effect);
        }

        paint
    }

    pub fn paint_for_image(&mut self) -> Paint {
        let mut paint = self.state.paint.clone();

        // 1. Apply Skia colorFilter (operates on source colors before blend)
        if let Some(cf) = &self.state.skia_color_filter {
            paint.set_color_filter(cf.on(self.onto().as_ref()));
        }

        // 2. Apply CSS filter (image_filter + mask_filter)
        self.state
            .filter
            .mix_into(&mut paint, self.state.matrix, true)
            .set_alpha_f(self.state.global_alpha as f32);

        // 3. Compose Skia imageFilter with CSS imageFilter (if both present)
        if let Some(skia_imgf) = &self.state.skia_image_filter {
            let skia_imgf = skia_imgf.on(self.onto().as_ref());
            let final_image_filter = match paint.image_filter() {
                Some(css_imgf) => image_filters::compose(skia_imgf, css_imgf),
                None => Some(skia_imgf),
            };
            paint.set_image_filter(final_image_filter);
        }

        if let Some(mf) = &self.state.skia_mask_filter {
            paint.set_mask_filter(mf.clone());
        }
        paint.set_dither(self.state.dither);

        paint
    }

    /// The blur's standard deviation where a draw in `style` would be kept
    /// as a [`BlurOp`](layer::BlurOp), whose blur is made at playback: a
    /// flat-colour fill or stroke blurred by a `blur()` filter alone. `None`
    /// for any other draw, which records its blur as it always has.
    ///
    /// Left out: a draw with a shadow, which is drawn beside it in one
    /// recorded frame; a blend mode other than source-over, which
    /// `render_to_canvas` composites through a layer of its own; and
    /// anything carrying a CanvasKit filter, a texture or a dash, which the
    /// op's paint does not keep.
    fn blurs_as_op(&self, style: PaintStyle) -> Option<f32> {
        let sigma = self.state.filter.sole_blur()?;
        let dashed =
            style != PaintStyle::Fill && !self.state.line_dash_list.is_empty();
        let plain = self.state.global_composite_operation == BlendMode::SrcOver
            && matches!(self.state.dye(style), Dye::Color(..))
            && self.state.texture(style).is_none()
            && self.state.skia_image_filter.is_none()
            && self.state.skia_mask_filter.is_none()
            && self.state.skia_color_filter.is_none()
            && !dashed
            && !self.casts_shadow();
        plain.then_some(sigma)
    }

    /// Files the `shadowColor` shadow of a fill of `path` in `style` as a
    /// [`BlurOp`](layer::BlurOp) of its own, before the fill, where it is a
    /// blurred rectangle in one flat colour: a filled rectangle the
    /// transform keeps one, dyed by a colour, on an sRGB page, under a blur
    /// and a blend that composites only where it lands. The op's blur of a
    /// rectangle is worked in closed form, and both engines draw it from the
    /// same profiles -- see `blur_mask::draw_rect`; through the shadow's
    /// chain the same rectangle was rasterised and convolved. `false`,
    /// filing nothing, for any other draw, and for a shadow with no blur,
    /// which the op does not draw exactly and the chain does.
    ///
    /// The shadow's colour takes the fill's alpha, its colour's and the
    /// global one: a flat fill's coverage is its rectangle's, so the
    /// shadow's alpha is the product, rounded once by the op.
    fn files_rect_shadow(&self, path: &Path, style: PaintStyle) -> bool {
        let plain = self.casts_shadow()
            && self.state.shadow_blur > 0.0
            && PixelColorSpace::matching(&self.canvas_color_space)
                == Some(PixelColorSpace::Srgb)
            && style == PaintStyle::Fill
            && self.state.global_composite_operation == BlendMode::SrcOver
            && self.state.filter.is_none()
            && matches!(self.state.dye(style), Dye::Color(..))
            && self.state.texture(style).is_none()
            && self.state.skia_image_filter.is_none()
            && self.state.skia_mask_filter.is_none()
            && self.state.skia_color_filter.is_none()
            && self.state.matrix.rect_stays_rect();
        let Some(rect) = plain.then(|| path.is_rect()).flatten() else {
            return false;
        };
        let fill = self.paint_for_blur(style);
        let (named, space) = &self.state.shadow_color;
        let colour = Color4f {
            a: named.a * fill.alpha_f(),
            ..*named
        };
        let mut paint = fill;
        paint.set_color4f(colour, Some(space));
        // Not placed on the bytes a fill of the colour stores, as a blurred
        // fill's colour is -- see `blur_mask::draw_rect`: the shadow is the
        // colour times the blurred coverage, rounded once, and placed it
        // was scaled throughout by its alpha's rounding, 77 where a shadow
        // of `rgba(0, 0, 0, 0.3)` is 76.5. On an sRGB page Skia converts the
        // colour to the page's space unchanged.
        let sigma = self.state.shadow_blur / 2.0;
        let draw = BlurDraw::of(path, Some(rect), &paint, sigma, self.faint());
        let matrix = Matrix::concat(
            &Matrix::translate(self.state.shadow_offset),
            &self.state.matrix,
        );
        let clip = self.state.clip.as_ref();
        self.with_recorder(|mut recorder| {
            recorder.append_blur(
                draw,
                matrix,
                clip,
                VectorFeatures::MASK_FILTER,
            );
        });
        true
    }

    /// What `paint_for_drawing` makes for a draw [`Self::blurs_as_op`] lets
    /// through, less the filters: the state's paint, dither, the colour with
    /// the global alpha in it, and `style`.
    fn paint_for_blur(&self, style: PaintStyle) -> Paint {
        let mut paint = self.state.paint.clone();
        paint.set_dither(self.state.dither);
        self.state.dye(style).mix_into(
            &mut paint,
            self.state.global_alpha as f32,
            self.state.sampling_filter,
            &self.canvas_color_space,
            self.onto().as_ref(),
        );
        paint.set_style(style);
        paint
    }

    /// The filter as steps the crate runs on a draw's pixels exactly -- see
    /// [`Filter::steps`] -- or `None` where Skia's filter draws it: no
    /// filter, or one Skia has to run itself.
    ///
    /// A Skia image filter is one Skia runs, and on a page that composites
    /// in floats the crate runs it on a float layer of its own, as the one
    /// step of its chain -- see [`Step::Image`]; Skia's own layer for it
    /// would be eight bits. Beside a CSS filter it keeps Skia's layer on any
    /// page: the two would run in different spaces, the CSS filter's lengths
    /// in canvas pixels and the image filter's in the draw's own units.
    fn exact_steps(&self) -> Option<Arc<[Step]>> {
        match &self.state.skia_image_filter {
            None => self.state.filter.steps().map(Arc::<[Step]>::from),
            Some(filter) => (self.composites_in_floats()
                && self.state.filter.is_none())
            .then(|| {
                let pixels = self
                    .pixels_onto()
                    .filter(|_| filter.depends())
                    .map(|onto| filter.on(Some(&onto)));
                Arc::from([Step::Image {
                    filter: filter.on(self.onto().as_ref()),
                    pixels,
                }])
            }),
        }
    }

    /// The page a CanvasKit filter or a texture drawn here is built for --
    /// see [`PageFilter::on`] -- on a page that composites in floats, and on
    /// an eight-bit sRGB page: their colours handed over as a fill's are --
    /// see [`Onto::color`] -- and a shadow's placed -- see [`Onto::placed`].
    /// Handed over as built, a colour went through Skia's approximate
    /// conversion, which moved a product sitting on a half a level: on the
    /// GPU a blend filter over a fill of `rgb(77 179 51 / 0.5)` stored blue
    /// 26 where the fill stores 25. Any other eight-bit page's document
    /// draws them as built; its pixels place them -- see
    /// [`Self::pixels_onto`].
    pub(crate) fn onto(&self) -> Option<Onto> {
        let space = self.canvas_color_space.clone();
        match self.composites_in_floats() {
            true => Some(Onto::shared(space)),
            false => (self.canvas_pixels == Some(PixelColorSpace::Srgb))
                .then(|| Onto::bytes(space)),
        }
    }

    /// The page a CanvasKit filter drawn into this page's pixels recording
    /// is built for, where it keeps one -- see `page::Recorders`. On a float
    /// page the CPU draws, which is every float page while the GPU cannot
    /// composite one -- see [`floats_draw_on_the_cpu`] -- built
    /// [`Onto::exact`], since the CPU alone draws the exact blend to the
    /// bit in floats; on an eight-bit page, built [`Onto::bytes`], its
    /// colours placed on the bytes a fill of them stores, which either
    /// engine's exact blend lands on.
    pub(crate) fn pixels_onto(&self) -> Option<Onto> {
        if !self.recorder.borrow().dual() {
            return None;
        }
        let space = self.canvas_color_space.clone();
        match self.composites_in_floats() {
            true => floats_draw_on_the_cpu(self.canvas_color_type)
                .then(|| Onto::exact(space)),
            false => Some(Onto::bytes(space)),
        }
    }

    /// Whether the page composites in floats, `F16` or `F32`: a page whose
    /// Skia image filters the crate runs itself, as Skia's layer would
    /// round them to eight bits -- see [`Step::Image`].
    pub(crate) fn composites_in_floats(&self) -> bool {
        composites_in_floats(self.canvas_color_type)
    }

    /// Whether a draw now casts a shadow: what `paint_for_shadow` answers
    /// without making the paint.
    fn casts_shadow(&self) -> bool {
        let (color, _) = &self.state.shadow_color;
        color.a != 0.0
            && !(self.state.shadow_blur == 0.0
                && self.state.shadow_offset.is_zero())
    }

    pub fn paint_for_shadow(&self, base_paint: &Paint) -> Option<Paint> {
        self.paint_for_shadow_in(base_paint, Recording::Shared, false)
            .map(|(paint, _)| paint)
    }

    /// [`Self::paint_for_shadow`] in `recording`. On an eight-bit page that
    /// keeps a recording for pixels, that recording casts the shadow in its
    /// colour's exact conversion placed on the bytes it stores -- see
    /// [`ExactColour::at`] -- which is how a fill of the colour lands, and
    /// which Skia's approximate conversion of the colour the shadow filter
    /// holds cannot move off them: it moves a colour by about 6e-5, under
    /// a fiftieth of a level. Converted from sRGB instead, the shadow of a
    /// rectangle in `color(display-p3 0.00196 0.00196 0.00196)` stored
    /// 1, 0, 0 where the fill stores 1, 1, 1.
    ///
    /// `lines` where the draw is a run naming a line colour, which holds
    /// the `globalAlpha` apart from the paint -- see [`Pass`]. With it, the
    /// [`Pass`] says how the shadow is to be drawn.
    fn paint_for_shadow_in(
        &self,
        base_paint: &Paint,
        recording: Recording,
        lines: bool,
    ) -> Option<(Paint, Pass)> {
        self.shadow_cast_in(base_paint, recording, lines)
            .map(|cast| {
                let mut paint = cast.paint;
                // Cast from what the draw's own filter makes, which the shadow
                // filter takes as its input, as the HTML Standard orders the
                // two.
                let filtered = paint.image_filter();
                paint.set_image_filter(image_filters::drop_shadow_only(
                    (0.0, 0.0),
                    (cast.sigma.x, cast.sigma.y),
                    cast.colour,
                    cast.space,
                    filtered,
                    None,
                ));
                (paint, cast.pass)
            })
    }

    /// The chain a draw's `shadowColor` shadow is cast through on pixels,
    /// in the colour `cast` names: the exact Gaussian of the draw's
    /// coverage, of a standard deviation half the `shadowBlur` in canvas
    /// pixels whatever the transform, as the HTML Standard sets it, made by
    /// itself -- see [`Step::Shadow`]. Its offset is none: the group the
    /// draw is cast in is moved by the shadow's offset, so that the draw is
    /// rasterized where its shadow lies.
    fn shadow_steps(&self, cast: &CanvasShadow) -> Arc<[Step]> {
        Arc::from([Step::Shadow {
            offset: Vector::default(),
            sigma: self.state.shadow_blur / 2.0,
            colour: cast.named,
            space: cast.named_space.clone(),
            shadow_only: true,
        }])
    }

    /// What [`Self::paint_for_shadow_in`] makes, before Skia's shadow filter
    /// is put on the paint: the paint the draw casts in, how it is drawn,
    /// and the shadow's colour as `recording` places it.
    fn shadow_cast_in(
        &self,
        base_paint: &Paint,
        recording: Recording,
        lines: bool,
    ) -> Option<CanvasShadow> {
        let (shadow_color, shadow_space) = &self.state.shadow_color;
        let mut shadow_blur = self.state.shadow_blur;
        if !self.casts_shadow() {
            return None;
        }

        // Per spec, sigma is exactly half the blur radius:
        // https://www.w3.org/TR/css-backgrounds-3/#shadow-blur
        shadow_blur *= 0.5;
        let mut sigma = Point::new(shadow_blur, shadow_blur);
        // Apply scaling from the current transform matrix to blur radius, if
        // there is any of either.
        if self.state.matrix.get_type().contains(TypeMask::SCALE)
            && !almost_zero(shadow_blur)
        {
            // Decompose the matrix to just the scaling factors
            // (matrix.scale_x/y() methods just return M11/M22 values)
            if let Some(scale) = self.state.matrix.decompose_scale(None) {
                if almost_zero(scale.width) {
                    sigma.x = 0.0;
                } else {
                    sigma.x /= scale.width;
                }
                if almost_zero(scale.height) {
                    sigma.y = 0.0;
                } else {
                    sigma.y /= scale.height;
                }
            }
        }
        let mut paint = base_paint.clone();
        // The paint's alpha moves into the shadow's colour, which the filter
        // applies to the shape at the page's own precision, so that the two
        // are one product: on an eight-bit sRGB page rounded once, as a fill
        // under a `globalAlpha` rounds them, the colour placed -- see
        // `Onto::placed`; on float pixels kept in float, where Skia's raster
        // backend brings a filtered layer back in steps of one 255th, so a
        // `globalAlpha` of 0.5 cast a shadow of 128/255 and one of 0.001
        // none at all. Through a colour filter, which sees the alpha before
        // the shadow does, it stays on the paint. A run naming a line colour
        // moves only the `globalAlpha`, which its line colour holds too, and
        // its line is drawn without it: moved whole, the line's shadow took
        // the alpha twice, half of `underline rgb(0 90 200 / 0.6)` under a
        // `globalAlpha` of 0.5.
        let bytes = self.onto().filter(|onto| onto.bytes);
        let mut named = *shadow_color;
        let mut pass = Pass::Draw;
        if (bytes.is_some() || self.faint() == Faint::Kept)
            && paint.color_filter().is_none()
        {
            let global = self.state.global_alpha as f32;
            let moved = match lines {
                true => global,
                false => paint.alpha_f(),
            };
            if moved > 0.0 {
                named.a *= moved;
                paint.set_alpha_f(paint.alpha_f() / moved);
                pass = Pass::AlphaInShadow;
            }
        }
        // A document keeps the colour as named, as a fill's is; the pixels
        // of a page that records them apart are placed as a wide-gamut
        // page places them, or as an sRGB page does.
        let on_bytes = |onto: Onto| onto.placed(named, shadow_space);
        let placed = match recording {
            Recording::Document => None,
            _ if self.composites_in_floats() => None,
            Recording::Pixels => self
                .placed_colour(named, shadow_space)
                .or_else(|| bytes.clone().and_then(on_bytes)),
            Recording::Shared => bytes.clone().and_then(on_bytes),
        };
        let (colour, space) = placed.unwrap_or_else(|| {
            skia_paint_color_onto(named, shadow_space, &self.canvas_color_space)
        });
        Some(CanvasShadow {
            paint,
            pass,
            colour,
            space,
            named,
            named_space: shadow_space.clone(),
            sigma,
        })
    }
}

/// A draw's `shadowColor` shadow as one recording casts it -- see
/// `Context2D::shadow_cast_in`.
struct CanvasShadow {
    /// The paint the draw is drawn in to cast its shadow: its own, its alpha
    /// moved into `colour` where `pass` says so.
    paint: Paint,
    /// How the draw is drawn to cast it.
    pass: Pass,
    /// The shadow's colour, unpremultiplied, in `space`, as the recording
    /// places it for Skia's shadow filter, whose conversion of a colour can
    /// move a product off the level it rounds to -- see
    /// `Context2D::paint_for_shadow_in`.
    colour: Color4f,
    space: ColorSpace,
    /// The colour as named, unpremultiplied, in `named_space`, the alpha the
    /// draw's paint moved into it included: what a chain tints with, whose
    /// own conversion is exact and whose product is rounded once -- see
    /// `chain::tint_colour`. Placed on a level instead, a translucent
    /// shadow's every blurred value was scaled by that level's error.
    named: Color4f,
    named_space: ColorSpace,
    /// Its standard deviation along each axis in the draw's own units, as
    /// Skia's shadow filter takes it: half the `shadowBlur`, undone by the
    /// transform's scale.
    sigma: Point,
}

/// What a run of text hands [`Context2D::render_to_canvas`] beside its draw.
pub struct TextRun<'a> {
    /// Whether the run names a line colour -- see
    /// `Typesetter::names_a_line_colour`.
    pub lines: bool,
    /// Set by the draw to what the run can ink, in the units of the canvas it
    /// draws on, where that is known -- see
    /// [`Context2D::files_through_chain`].
    pub reached: &'a Cell<Option<Rect>>,
    /// The run kept for drawing, `None` the first time it is drawn -- see
    /// `Typesetter::laid_out` -- which a blurred run's coverage is kept
    /// under: see [`run_mask`].
    pub kept: Option<Arc<RunPaint>>,
    /// Where the kept run's runs are drawn from, under the canvas's
    /// transform: the anchor moved by the layout's offset, and squeezed by
    /// its condensation where it has one -- the transform its glyphs'
    /// positions are drawn under, which `run_mask` places them by.
    pub anchor: Matrix,
    /// The kept run's draw in a paint, to be recorded when a target first
    /// plays it, and what it can ink, in the units of the canvas it draws
    /// on -- see `layer::DeferredPicture`. `None`, or answering `None`,
    /// where the run is recorded as it is drawn.
    pub(crate) defer: Option<&'a Defer<'a>>,
    /// The run kept whole where it holds colour glyphs this library draws
    /// itself, to draw them where the page is played back -- see
    /// [`Context2D::files_as_colour_text`].
    pub colour: Option<Replay>,
    /// Whether the run holds a glyph the platform draws in its own colours
    /// -- see `font::platform_colour` -- asked only where the coverage route
    /// could otherwise take the run.
    pub(crate) own_colours: &'a dyn Fn() -> bool,
}

/// The transform a run anchored at `origin` is drawn from -- see
/// [`TextRun::anchor`] -- as `Context2D::draw_text` draws it: at `offset`
/// from the anchor, and under a squeeze by `condensation` about the anchor
/// where that is not 1.
fn drawn_from(origin: Point, offset: Point, condensation: f32) -> Matrix {
    #[cfg(test)]
    if run_mask::RULE.get() == run_mask::Rule::AnchorOnly {
        return Matrix::translate(origin);
    }
    match condensation {
        1.0 => Matrix::translate(origin + offset),
        scale => Matrix::concat(
            &Matrix::translate(origin),
            &Matrix::concat(
                &Matrix::scale((scale, 1.0)),
                &Matrix::translate(offset),
            ),
        ),
    }
}

/// What [`TextRun::defer`] asks: the kept run's draw in the paint it is
/// handed, and what it can ink.
pub(crate) type Defer<'a> =
    dyn Fn(&Paint) -> Option<(DrawnText, Option<Rect>)> + 'a;

/// What a draw handed to [`Context2D::render_to_canvas`] is drawing, which
/// a run naming a line colour needs: the line holds the `globalAlpha` apart
/// from the paint -- see `Context2D::paint_for_shadow_in`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// The draw, or a shadow of it holding the `globalAlpha` where the draw
    /// does.
    Draw,
    /// A shadow whose colour took the `globalAlpha`, which a named line
    /// colour is drawn without.
    AlphaInShadow,
}

/// A draw's gradient as the draw hands it to the page: what it is made
/// from -- see [`Dye::gradient`] -- and whether the draw is a rectangle on
/// whole device pixels.
pub struct GradientDye<'a> {
    pub spec: Arc<GradientSpec>,
    /// The path drawn, where the draw is one, and what it reaches in device
    /// pixels -- see [`drawn_bounds`]: kept, rather than a recording of it,
    /// so that a GPU canvas makes the effect for it where the page is played
    /// -- see `layer::GradientPath`.
    pub path: Option<(&'a Path, Rect)>,
    /// Whether the draw fills a rectangle whose edges lie on whole device
    /// pixels, under no clip -- see [`fills_whole_pixels`]. Drawn aliased,
    /// which covers the same pixels whole, it is the one shape Ganesh hands
    /// the effect each pixel centre's coordinates for exactly, so a GPU
    /// canvas draws it with the effect itself and needs no image of it --
    /// see [`GradientSpec::sampled`].
    pub whole_pixels: bool,
}

/// Whether `path` is a rectangle `matrix` maps onto whole device pixels: a
/// translation by whole pixels and edges on whole units.
fn fills_whole_pixels(path: &Path, matrix: &Matrix) -> bool {
    let Some(rect) = path.is_rect().map(|(rect, ..)| rect) else {
        return false;
    };
    matrix.is_translate()
        && !path.is_inverse_fill_type()
        && [
            rect.left + matrix.translate_x(),
            rect.top + matrix.translate_y(),
            rect.right + matrix.translate_x(),
            rect.bottom + matrix.translate_y(),
        ]
        .iter()
        .all(|edge| edge.fract() == 0.0)
}

//
// Dye abstraction for Color / CanvasGradient / CanvasPattern
//

#[derive(Clone)]
pub enum Dye {
    /// A solid color, unpremultiplied, with the space it is in: the one a CSS
    /// string named, sRGB for a float array, linear-light sRGB for an
    /// `RgbaLinear`. `None` is sRGB, as Skia reads an untagged colour.
    ///
    /// And the colour as it was declared, which the getter serialises and
    /// keeps the read-back of; `None` for one set through the crate as an
    /// `RgbaLinear`, which no getter reads back as CSS.
    Color(Color4f, Option<ColorSpace>, Option<Declaration>),
    Gradient(CanvasGradient),
    Pattern(CanvasPattern),
    Texture(CanvasTexture),
    /// A reusable Skia shader (e.g. fractal noise / turbulence) set as a
    /// fill or stroke style, with what a vector backend has to reckon with:
    /// the gradient factories are paint servers SVG can name, the noise ones
    /// are not. And, for a gradient, what it is made from -- see
    /// [`GradientSpec`].
    Shader(SkShader, VectorFeatures, Option<Arc<GradientSpec>>),
}

impl Dye {
    pub fn new<'a>(
        cx: &mut FunctionContext<'a>,
        value: Handle<'a, JsValue>,
    ) -> Option<Self> {
        if let Ok(gradient) = value.downcast::<BoxedCanvasGradient, _>(cx) {
            Some(Dye::Gradient(gradient.borrow().clone()))
        } else if let Ok(pattern) = value.downcast::<BoxedCanvasPattern, _>(cx)
        {
            Some(Dye::Pattern(pattern.borrow().clone()))
        } else if let Ok(texture) = value.downcast::<BoxedCanvasTexture, _>(cx)
        {
            Some(Dye::Texture(texture.borrow().clone()))
        } else if let Ok(shader) = value.downcast::<BoxedShader, _>(cx) {
            Some(Dye::Shader(
                shader.borrow().inner.clone(),
                shader.borrow().features,
                None,
            ))
        } else {
            // Tagged either way by `color4f_in`: a CSS colour with the space
            // it named, a float array as sRGB.
            declared_in(cx, value).map(|(c, cs, declared)| {
                Dye::Color(c, Some(cs), declared.map(Declaration::new))
            })
        }
    }

    pub fn value<'a>(
        &self,
        cx: &mut FunctionContext<'a>,
    ) -> JsResult<'a, JsValue> {
        match self {
            Dye::Color(_, _, Some(declared)) => {
                Ok(cx.string(declared.css()).upcast())
            }
            Dye::Color(color, cs, None) => {
                color4f_to_css(cx, color, cs.as_ref())
            }
            _ => Ok(cx.null().upcast()), /* flag to the js context that it
                                          * should use its cached
                                          * pattern/gradient ref */
        }
    }

    /// What a vector backend has to reckon with to write a draw painted in
    /// this dye.
    ///
    /// SVG names a solid color, a linear, radial or two-point conical
    /// gradient, and an image shader. A sweep gradient, a procedural shader
    /// or a canvas's pattern leaves it with nothing to write.
    pub fn vector_features(&self) -> VectorFeatures {
        match self {
            // A canvas's pattern is a picture shader to a document, which
            // SVG names no paint server for: a draw dyed by one wrote a bare
            // path, black. Skia's PDF backend rasterizes one at the wrong
            // scale and in the wrong space -- see
            // `VectorFeatures::CANVAS_PATTERN`. A draw whose tiles land on
            // whole device pixels asks for `CANVAS_PATTERN_ALIGNED` in its
            // place -- see `Self::files_through_pattern`. An image's is an
            // image shader, which both write.
            // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
            // Re-check: cargo test a_pdf_pattern_is_embedded_at_the_density
            // with `CANVAS_PATTERN` left out of `PDF_CANNOT`.
            Dye::Pattern(pattern) if pattern.tiling().is_some() => {
                VectorFeatures::EXOTIC_SHADER
                    .with(VectorFeatures::CANVAS_PATTERN)
            }
            Dye::Color(..) | Dye::Pattern(_) | Dye::Texture(_) => {
                VectorFeatures::PLAIN
            }
            Dye::Gradient(gradient) => gradient.vector_features(),
            Dye::Shader(_, features, _) => *features,
        }
    }

    /// What the gradient this dye paints with is made from: `None` for a
    /// dye that is not a gradient, and for one the Canvas standard says
    /// paints nothing.
    pub fn gradient(&self) -> Option<Arc<GradientSpec>> {
        match self {
            Dye::Gradient(gradient) => gradient.spec().map(Arc::new),
            Dye::Shader(_, _, spec) => spec.clone(),
            _ => None,
        }
    }

    pub fn is_opaque(&self) -> bool {
        match self {
            Dye::Color(color, ..) => color.is_opaque(),
            Dye::Gradient(gradient) => gradient.is_opaque(),
            Dye::Pattern(pattern) => pattern.is_opaque(),
            Dye::Texture(_) => false,
            Dye::Shader(..) => false,
        }
    }

    /// Puts the dye on `paint` at `alpha`, for a page in `destination`, a
    /// texture's mark as built for `onto` -- see [`CanvasTexture::to_color4f`].
    pub fn mix_into(
        &self,
        paint: &mut Paint,
        alpha: f32,
        sampling_filter: SamplingFilter,
        destination: &ColorSpace,
        onto: Option<&Onto>,
    ) {
        match self {
            Dye::Color(color, cs, _) => {
                let mut color = *color;
                color.a *= alpha;
                // Kept in the space it was named in, so what `fillStyle`
                // reports back echoes that space as a browser's does, and
                // handed over as the page it is drawn onto needs.
                match cs {
                    Some(space) => {
                        let (color, space) =
                            skia_paint_color_onto(color, space, destination);
                        paint.set_color4f(color, &space)
                    }
                    None => paint.set_color4f(color, None),
                };
            }
            Dye::Gradient(gradient) => {
                paint.set_shader(gradient.shader()).set_alpha_f(alpha);
            }
            Dye::Pattern(pattern) => {
                paint
                    .set_shader(pattern.shader(sampling_filter))
                    .set_alpha_f(alpha);
            }
            Dye::Texture(texture) => {
                let (color, space) = texture.to_color4f(alpha, onto);
                paint.set_color4f(color, &space);
            }
            Dye::Shader(shader, ..) => {
                paint.set_shader(Some(shader.clone())).set_alpha_f(alpha);
            }
        };
    }
}

#[cfg(test)]
thread_local! {
    /// The last reach a run of text drawn on this thread answered, in the
    /// units of the canvas it was drawn on -- see [`Context2D::draw_text`].
    static TEXT_REACHED: Cell<Option<Rect>> = const { Cell::new(None) };
    /// Whether a run's coverage copy may be the picture of its pixels where
    /// the two are drawn in one paint, so that a test can hold the shared
    /// picture to one recorded on its own.
    pub(crate) static SHARES_COVERAGE_PICTURE: Cell<bool> =
        const { Cell::new(true) };
    /// How many runs on this thread took the picture of their pixels as
    /// their coverage copy.
    pub(crate) static SHARED_COVERAGE_PICTURES: Cell<usize> =
        const { Cell::new(0) };
    /// Whether a kept run under a chain may be recorded when first played
    /// -- see `layer::DeferredPicture` -- so that a test can hold it to the
    /// run recorded as it is drawn.
    pub(crate) static DEFERS: Cell<bool> = const { Cell::new(true) };
    /// Whether a run holding colour glyphs is recorded for pixels as it is
    /// drawn rather than when first asked for -- see
    /// `layer::ColourText::recorded` -- so that a test can hold the one to
    /// the other.
    pub(crate) static COLOUR_RECORDED_AT_ONCE: Cell<bool> =
        const { Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// Whether a draw dyed by a gradient is filed as a
    /// [`GradientDraw`](layer::GradientDraw) on this thread: off, it is
    /// recorded as any other draw, the reference what a CPU canvas and a
    /// document make of one is held to.
    pub(crate) static FILES_GRADIENTS: Cell<bool> = const { Cell::new(true) };
}

#[cfg(test)]
thread_local! {
    /// Whether [`Context2D::exact_paint`] takes what it made of a flat colour
    /// kept in [`FILLS`]: off, every draw makes it afresh, the reference the
    /// kept ones are held to; and how many draws took one kept.
    static KEEPS_FILLS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    static FILLS_KEPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Everything that decides what [`Context2D::exact_paint`] makes of a flat
/// colour through no filter on a page keeping a recording for pixels, so
/// that a colour is found again, on any page, only where it would be made
/// the same. Compared by bits.
#[derive(Clone, PartialEq)]
struct FillKey {
    /// The colour's components, unpremultiplied, in `space`: what is
    /// converted into the page's space.
    colour: [u32; 4],
    /// The space the colour is named in: the same components in another
    /// space are another colour.
    space: ColorSpace,
    /// The paint's alpha, which the colour is placed on the bytes with --
    /// see `ExactColour::at` -- and which decides whether Skia lands that
    /// as a plain paint colour -- see `ExactColour::verified_colour`.
    alpha: u32,
    /// The page's space, which the colour is converted into. It names the
    /// space whole: `PixelColorSpace::matching` takes a page's space to
    /// one of these by its gamut and transfer function.
    page: PixelColorSpace,
    /// Whether the page composites in floats, where the colour is painted
    /// by its shader rather than placed on bytes.
    floats: bool,
}

/// 2^64 divided by the golden ratio, rounded to odd: Knuth's multiplier for
/// hashing by multiplication (The Art of Computer Programming, vol. 3,
/// section 6.4), as the verdicts of `layer::ExactColour` are filed by.
const FILL_SLOT_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;

/// How many sets of flat colours [`FILLS`] keeps on a thread, and how many
/// a set holds: 1,024 colours, each 64 bytes with what was made of it,
/// 64 KiB a thread once the first is kept. A page of 200 colours, each set
/// once, finds them all on the next page drawn on the thread, fresh canvas
/// or not -- `a_fresh_canvas_finds_the_colours_the_last_one_converted`;
/// four to a set found 190 of them, the rest filed in sets they overfilled.
/// A colour filed over another is converted again when it is next drawn,
/// which costs what converting it always did.
const FILL_SETS: usize = 128;
const FILL_WAYS: usize = 8;

thread_local! {
    /// The flat colours pixels recordings on this thread painted with and
    /// what [`Context2D::exact_paint`] made of them -- see [`FillKey`] and
    /// [`PlacedFill`].
    static FILLS: RefCell<KeptSets<FillKey, PlacedFill, FILL_SETS, FILL_WAYS>> =
        const { RefCell::new(KeptSets::new()) };
}

impl FillKey {
    /// Where [`FILLS`] files this key: its bits folded together and
    /// multiplied by [`FILL_SLOT_MULTIPLIER`], which carries every bit into
    /// the top ones a set is taken from.
    fn hash(&self) -> u64 {
        let [r, g, b, a] = self.colour.map(u64::from);
        let key =
            (r ^ g.rotate_left(16) ^ b.rotate_left(32) ^ a.rotate_left(48))
                ^ u64::from(self.alpha).rotate_left(8)
                ^ (self.page as u64) << 56
                ^ u64::from(self.floats) << 63;
        key.wrapping_mul(FILL_SLOT_MULTIPLIER)
    }

    fn of(
        colour: Color4f,
        space: &ColorSpace,
        alpha: f32,
        page: PixelColorSpace,
        floats: bool,
    ) -> Self {
        Self {
            colour: [colour.r, colour.g, colour.b, colour.a].map(f32::to_bits),
            space: space.clone(),
            alpha: alpha.to_bits(),
            page,
            floats,
        }
    }
}

/// What [`Context2D::exact_paint`] made of a flat colour at an alpha:
/// nothing, where the colour converts into the page's space inexactly or
/// the page keeps one recording; the colour and space set on the paint,
/// where it lands placed as a paint's own colour; or a shader, which is
/// built again from the colour and kept as [`Context2D::exact_shader`]
/// keeps it.
#[derive(Clone)]
enum PlacedFill {
    Inexact,
    Placed(Color4f, ColorSpace),
    Shaded,
}

/// Whether a float page in `color_type` is drawn on the CPU whichever engine
/// its canvas asks for: where the GPU cannot composite it, a canvas asking
/// for the GPU is drawn on the raster backend -- see `Canvas::engine_kind`.
/// [`Context2D::pixels_onto`] builds its filters for the CPU, where
/// `filter::onto::exact_blend` blends every mode to the bit as Skia's own
/// filter does. On Ganesh Metal it blends at the half precision of a
/// fragment's output, a level off on eight bits where Skia folds its own
/// filter into a flat colour on the CPU, so a float page the GPU draws has to
/// be measured before it takes that filter --
/// `float_pages_draw_on_the_cpu` fails when one does.
pub(crate) fn floats_draw_on_the_cpu(color_type: ColorType) -> bool {
    matches!(
        RenderingEngine::for_canvas(true, color_type),
        RenderingEngine::CPU
    )
}

/// Whether a canvas in `space` keeps a recording for each kind of target at
/// any depth -- see `page::Recorders`: one whose space is one a flat colour
/// can be converted into exactly here, other than sRGB, onto which Skia
/// lands a paint colour exactly itself. An eight-bit sRGB page keeps two as
/// well, for its blit's sake rather than its space's -- see
/// [`records_pixels_apart`], which is what a page asks.
///
/// At any depth. Skia keeps a paint's colour in sRGB and converts it into
/// the page's space at the draw, through skcms's approximate transfer
/// function: at eight bits that moved a channel across a rounding boundary
/// for about one colour in 25, and at every tie between two bytes -- on a
/// Display P3 canvas `color(display-p3 0.5 0.5 0.5)` filled 127 where an
/// sRGB canvas fills the same grey 128 -- as at a float depth it moved the
/// colour itself.
fn keeps_two_recordings(space: &ColorSpace) -> bool {
    PixelColorSpace::matching(space).is_some_and(|space| {
        space != PixelColorSpace::Srgb && space.converts_exactly()
    })
}

/// Whether a canvas in `space` whose pixels are `color_type` keeps a
/// recording for its pixels apart from the one its documents are written
/// from -- see `page::Recorders`: one in a space that keeps two at any depth
/// -- see [`keeps_two_recordings`] -- and an sRGB page that composites in
/// eight bits.
///
/// Skia lands a paint's colour on an sRGB page as itself, but blits a
/// source-over paint with no shader onto eight-bit sRGB pixels through its
/// legacy blitter -- see `SkBlitter::UseLegacyBlitter` -- which rounds the
/// colour and its alpha to bytes before multiplying them. Over 65,025 grey
/// and alpha pairs off the byte grid, `(c + 0.37) / 255` at `(a + 0.61) /
/// 255`, that stored a level off the rounded exact product for 8,260. The
/// pixels recording draws the colour placed on the bytes its exact product
/// rounds to -- see `ExactColour::at` -- which that blit stores as they are.
/// A document is written from the paint as it was set.
fn records_pixels_apart(space: &ColorSpace, color_type: ColorType) -> bool {
    keeps_two_recordings(space)
        || (!composites_in_floats(color_type)
            && PixelColorSpace::matching(space) == Some(PixelColorSpace::Srgb))
}

/// Whether a page of `color_type` composites in floats rather than in eight
/// bits -- see `ExportOptions::compositing_color_type`.
fn composites_in_floats(color_type: ColorType) -> bool {
    matches!(
        color_type,
        ColorType::RGBAF16 | ColorType::RGBAF16Norm | ColorType::RGBAF32
    )
}

/// Whether the CPU's eight-bit pipeline (`lowp`) has a stage for `blend`,
/// so that a draw through it stays on that pipeline: the separable modes and
/// the Porter-Duff ones `SkRasterPipeline_opts.h` writes a `BLEND_MODE` for,
/// and `Src` and `Dst`, which need none. The rest -- the dodge, the burn,
/// the soft light and the four non-separable modes -- take the float
/// pipeline (`highp`).
fn blends_in_eight_bits(blend: BlendMode) -> bool {
    matches!(
        blend,
        BlendMode::Clear
            | BlendMode::Src
            | BlendMode::Dst
            | BlendMode::SrcOver
            | BlendMode::DstOver
            | BlendMode::SrcIn
            | BlendMode::DstIn
            | BlendMode::SrcOut
            | BlendMode::DstOut
            | BlendMode::SrcATop
            | BlendMode::DstATop
            | BlendMode::Xor
            | BlendMode::Plus
            | BlendMode::Modulate
            | BlendMode::Screen
            | BlendMode::Overlay
            | BlendMode::Darken
            | BlendMode::Lighten
            | BlendMode::HardLight
            | BlendMode::Difference
            | BlendMode::Exclusion
            | BlendMode::Multiply
    )
}

/// Whether `blend` changes the canvas outside what a draw covers, so that
/// the draw has to be composited whole, as one picture -- see
/// `Context2D::render_to_canvas`.
fn covers_beyond_the_draw(blend: BlendMode) -> bool {
    matches!(
        blend,
        BlendMode::SrcIn
            | BlendMode::SrcOut
            | BlendMode::DstIn
            | BlendMode::DstOut
            | BlendMode::DstATop
            | BlendMode::Src
    )
}

/// `tile`, a paint `CanvasTexture::mix_into` has made from `paint`, given the
/// mark's colour back where `paint` is the exact one a page that keeps a
/// recording for pixels draws with: the colour `mark` held exactly in the
/// page's space, placed at `alpha`, the mark's -- see [`ExactColour::at`].
/// `mix_into` hands the paint the mark's own colour again, its alpha
/// unplaced, and the GPU engine stored a translucent mark a premultiplied
/// level off a fill of its colour on an eight-bit page.
fn place_mark(
    tile: &mut Paint,
    paint: &Paint,
    mark: Option<&ExactColour>,
    alpha: f32,
) {
    if paint.shader().is_none() {
        return;
    }
    let Some((placed, alpha)) = mark.map(|mark| mark.at(alpha)) else {
        return;
    };
    if let Some(shader) = placed.shader() {
        tile.set_shader(shader).set_alpha_f(alpha);
    }
}

/// Whether a canvas's picture, carrying `source`'s features, is drawn with
/// `paint` rather than without one: where the source composites in a mode of
/// its own, or where the paint says something -- an alpha under one, a blend
/// mode past source-over, an image filter, a colour filter. See
/// `Context2D::draw_picture_costing`.
fn carries(source: VectorFeatures, paint: &Paint) -> bool {
    !matches!(
        (
            source.any(),
            paint.as_blend_mode(),
            paint.alpha(),
            paint.image_filter(),
            paint.color_filter(),
        ),
        (false, Some(BlendMode::SrcOver), 255, None, None)
    )
}

/// Whether `path` holds `rect`, as `Path::conservatively_contains_rect`
/// answers -- asked of the path's bounds first.
///
/// Skia works out a path's convexity before anything else, afresh for every
/// path made, so asking whether a 10x10 fill covers the page was 9% of what
/// recording one cost. A path holds nothing its bounds do not, so a
/// rectangle reaching past them is refused without it. The comparison is
/// written out rather than `Rect::contains`, which refuses an empty
/// rectangle that Skia's own test still answers for.
fn holds(path: &Path, rect: Rect) -> bool {
    let reach = path.bounds();
    reach.left <= rect.left
        && reach.top <= rect.top
        && reach.right >= rect.right
        && reach.bottom >= rect.bottom
        && path.conservatively_contains_rect(rect)
}

/// What `path` covers once `matrix` puts it on the device, for choosing how
/// to blur it: a rectangle or a rounded rectangle only while the matrix
/// keeps it one, since Skia's cheap coverage blur for those shapes works on
/// axis-aligned device rectangles.
pub(crate) fn coverage_of(path: &Path, matrix: &Matrix) -> Coverage {
    coverage_given(path, path.is_rect().is_some(), matrix)
}

/// As [`coverage_of`], for a caller that has already asked whether `path` is
/// a rectangle and has the answer in `rect`.
fn coverage_given(path: &Path, rect: bool, matrix: &Matrix) -> Coverage {
    if !matrix.rect_stays_rect() {
        return Coverage::Path;
    }
    if rect {
        Coverage::Rect
    } else if path.is_oval().is_some() || path.is_rrect().is_some() {
        Coverage::Rounded
    } else {
        Coverage::Path
    }
}

#[cfg(test)]
thread_local! {
    /// Whether [`Context2D::cut_clip`] routes a cut draw at all on this
    /// thread: off, a page records every draw as it did before the route,
    /// the reference the route's vector and GPU playbacks are held to.
    static ROUTES_CUTS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Where a fill or stroke of `path` in `paint` under `matrix` can put
/// coverage, in device pixels: the path's bounds, widened by the stroke --
/// half its width, times the miter limit where joins are mitred and the
/// square root of two where caps are square -- or, where `outline` is set or
/// a path effect makes the path drawn, the bounds of the outline Skia fills;
/// and a pixel either side for anti-aliasing, two for a hairline.
fn drawn_bounds(
    path: &Path,
    paint: &Paint,
    style: PaintStyle,
    matrix: &Matrix,
    outline: bool,
) -> Rect {
    let stroked = style != PaintStyle::Fill;
    let local = match outline || paint.path_effect().is_some() {
        true => {
            let mut filled = PathBuilder::new();
            fill_path_with_paint(path, paint, &mut filled, None, Some(*matrix));
            *filled.detach().bounds()
        }
        false if stroked => {
            let reach = [
                1.0,
                match paint.stroke_join() {
                    PaintJoin::Miter => paint.stroke_miter(),
                    _ => 1.0,
                },
                match paint.stroke_cap() {
                    PaintCap::Square => std::f32::consts::SQRT_2,
                    _ => 1.0,
                },
            ]
            .into_iter()
            .fold(0.0, f32::max);
            let half = paint.stroke_width().max(0.0) * 0.5 * reach;
            path.bounds().with_outset((half, half))
        }
        false => *path.bounds(),
    };
    let hairline = stroked && paint.stroke_width() <= 0.0;
    let margin = if hairline { 2.0 } else { 1.0 };
    matrix.map_rect(local).0.with_outset((margin, margin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canvas::{
            Canvas, CanvasOptions, DEFAULT_TEXT_CONTRAST, DEFAULT_TEXT_GAMMA,
        },
        export::{EncodeOptions, ImageFormat},
        node::{filter::FilterSpec, utils::opt_color_space},
        pixels::PixelDepth,
    };

    /// An sRGB page keeps a recording for pixels only where it composites
    /// in eight bits, while `keeps_two_recordings` -- which places a
    /// shadow's colour, and a chain's fill -- still says no for sRGB at any
    /// depth; a Display P3 page keeps two at every depth.
    #[test]
    fn an_srgb_page_records_pixels_apart_only_in_eight_bits() {
        let srgb = ColorSpace::new_srgb();
        assert!(!keeps_two_recordings(&srgb));
        assert!(records_pixels_apart(&srgb, ColorType::N32));
        assert!(records_pixels_apart(&srgb, ColorType::RGBA8888));
        for floats in [
            ColorType::RGBAF16,
            ColorType::RGBAF16Norm,
            ColorType::RGBAF32,
        ] {
            assert!(!records_pixels_apart(&srgb, floats), "{floats:?}");
        }
        let p3 = opt_color_space("display-p3").expect("a space");
        assert!(records_pixels_apart(&p3, ColorType::RGBAF16));
    }

    /// Builds a context the size of a small canvas, as `Canvas::new` does.
    fn context() -> Context2D {
        Context2D::new(
            ColorSpace::new_srgb(),
            ColorType::N32,
            (DEFAULT_TEXT_CONTRAST, DEFAULT_TEXT_GAMMA),
            (200.0, 100.0),
        )
    }

    /// A run of text casting a shadow answers what it can ink, as one
    /// under a filter does, so that its shadow's chain is bounded by the
    /// run rather than by the clip; a run casting none, under no filter,
    /// does not work it out.
    #[test]
    fn a_shadowed_run_bounds_its_chain() {
        let mut ctx = context();
        assert!(!ctx.chains_text(), "a plain run");
        ctx.state.shadow_color =
            (Color4f::new(0.0, 0.0, 0.0, 0.6), ColorSpace::new_srgb());
        ctx.state.shadow_blur = 6.0;
        assert!(ctx.chains_text(), "a shadowed run");
    }

    /// Every float page is drawn on the CPU, so that its pixels take the
    /// CanvasKit filters [`Context2D::pixels_onto`] builds, which the CPU
    /// alone draws to the bit. When a GPU backend comes to composite a float
    /// page this fails: that GPU's blend of each mode has to be measured
    /// against the exact one before a float page it draws takes the filter.
    #[test]
    fn float_pages_draw_on_the_cpu() {
        for color_type in [ColorType::RGBAF16, ColorType::RGBAF32] {
            assert!(
                floats_draw_on_the_cpu(color_type),
                "{color_type:?} pages are drawn on the GPU here"
            );
        }
        let dual = Context2D::new(
            ColorSpace::new_srgb_linear(),
            ColorType::RGBAF32,
            (DEFAULT_TEXT_CONTRAST, DEFAULT_TEXT_GAMMA),
            (8.0, 8.0),
        );
        assert!(dual.pixels_onto().is_some_and(|onto| onto.exact));
    }

    /// A triangle reaching from x 20 to x 140.5, a third of it right of a
    /// cut at x 82.
    fn cut_triangle(ctx: &mut crate::context2d::Context2D) {
        ctx.begin_path();
        ctx.move_to(20.0, 80.0);
        ctx.line_to(80.0, 15.25);
        ctx.line_to(140.5, 85.0);
        ctx.close_path();
        ctx.fill(crate::path::FillRule::NonZero);
    }

    /// Clips at x 82.
    fn cut(ctx: &mut crate::context2d::Context2D) {
        ctx.rect(-10.0, -10.0, 92.0, 200.0);
    }

    /// Clips to a circle off the pixel grid, which the GPU clips by other
    /// than a rectangle's bounds.
    fn circle(ctx: &mut crate::context2d::Context2D) {
        ctx.arc(90.3, 60.2, 45.3, 0.0, std::f32::consts::TAU, false)
            .expect("a positive radius");
    }

    /// A page clipped by the path `clip` makes, with `draw` on it, the route
    /// on or off.
    fn clipped_canvas(
        routed: bool,
        gpu: bool,
        clip: fn(&mut crate::context2d::Context2D),
        draw: fn(&mut crate::context2d::Context2D),
    ) -> crate::canvas::Canvas {
        ROUTES_CUTS.set(routed);
        let mut canvas = crate::canvas::Canvas::new(200.0, 120.0);
        canvas.set_gpu(gpu);
        {
            let ctx = canvas.context();
            ctx.begin_path();
            clip(ctx);
            ctx.clip(crate::path::FillRule::NonZero);
            ctx.set_line_width(3.3);
            draw(ctx);
        }
        ROUTES_CUTS.set(true);
        canvas
    }

    /// `document` with each clip's id renumbered in the order it first
    /// appears: Skia names an SVG's clips `cl_` and the clip's generation in
    /// hexadecimal (`SkSVGDevice.cpp`), from a count the whole process
    /// shares, so two exports of one page are told apart by nothing else.
    fn numbered(document: &[u8]) -> Vec<u8> {
        const PREFIX: &[u8] = b"cl_";
        let mut seen: Vec<&[u8]> = vec![];
        let mut out = Vec::with_capacity(document.len());
        let mut rest = document;
        while let Some(at) =
            rest.windows(PREFIX.len()).position(|w| w == PREFIX)
        {
            let digits = rest[at + PREFIX.len()..]
                .iter()
                .take_while(|byte| byte.is_ascii_hexdigit())
                .count();
            let (head, tail) = rest.split_at(at + PREFIX.len());
            let (id, tail) = tail.split_at(digits);
            let index = seen
                .iter()
                .position(|known| *known == id)
                .unwrap_or_else(|| {
                    seen.push(id);
                    seen.len() - 1
                });
            out.extend_from_slice(head);
            out.extend_from_slice(index.to_string().as_bytes());
            rest = tail;
        }
        out.extend_from_slice(rest);
        out
    }

    /// The `CutOp`s among `layers`, among the pieces of a run, and among the
    /// entries of every group they hold.
    fn cut_ops(layers: &[layer::Layer]) -> usize {
        layers
            .iter()
            .map(|layer| match layer {
                layer::Layer::Cut(..) => 1,
                layer::Layer::Clipped(piece) => {
                    cut_ops(std::slice::from_ref(piece.entry()))
                }
                layer::Layer::Group(group) => cut_ops(&group.contents.layers),
                _ => 0,
            })
            .sum()
    }

    /// Only a fill or stroke the clip cuts is recorded as a `CutOp`: not one
    /// it leaves whole, not one it misses, and not an axis-aligned rectangle
    /// filled, which Skia draws by a blitter the clip does not move. One
    /// inside a layer is, among the layer's entries.
    #[test]
    fn only_a_draw_the_clip_cuts_records_a_cut_op() {
        let cuts = |draw: fn(&mut crate::context2d::Context2D)| {
            let mut canvas = clipped_canvas(true, false, cut, draw);
            let page = canvas.page(0).expect("a page").inner.page();
            cut_ops(&page.layers)
        };
        assert_eq!(cuts(cut_triangle), 1, "a triangle the clip cuts");
        assert_eq!(
            cuts(|ctx| {
                ctx.begin_path();
                ctx.move_to(80.5, 20.0);
                ctx.line_to(80.5, 90.0);
                ctx.stroke();
            }),
            1,
            "a line the clip misses whose stroke it cuts"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.begin_path();
                ctx.move_to(78.0, 20.0);
                ctx.line_to(78.0, 90.0);
                ctx.stroke();
            }),
            0,
            "a stroke wholly inside"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.begin_path();
                ctx.move_to(10.0, 80.0);
                ctx.line_to(40.0, 15.0);
                ctx.line_to(70.0, 85.0);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
            0,
            "a triangle wholly inside"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.begin_path();
                ctx.move_to(110.0, 80.0);
                ctx.line_to(140.0, 15.0);
                ctx.line_to(170.0, 85.0);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
            0,
            "a triangle wholly outside"
        );
        assert_eq!(
            cuts(|ctx| ctx.fill_rect(40.25, 20.5, 80.0, 50.0)),
            0,
            "an aligned rectangle"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.save_layer();
                cut_triangle(ctx);
                ctx.restore();
            }),
            1,
            "a cut triangle inside a layer"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.set_shadow_color_css("red").expect("a colour");
                ctx.set_shadow_offset(4.0, 4.0);
                cut_triangle(ctx);
            }),
            1,
            "a cut triangle casting a shadow, which files its shadow apart \
             and is drawn as it is casting none"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.set_filter_css("brightness(50%)").expect("a filter");
                cut_triangle(ctx);
            }),
            0,
            "a cut triangle through a filter"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.set_global_composite_operation(
                    crate::paint::BlendMode::DestinationIn,
                );
                cut_triangle(ctx);
            }),
            0,
            "a cut triangle in a blend reaching past it"
        );
        assert_eq!(
            cuts(|ctx| {
                ctx.set_fill_texture(&crate::texture::Texture::new(
                    &crate::texture::TextureOptions::default(),
                ));
                cut_triangle(ctx);
            }),
            0,
            "a cut triangle filled with a texture"
        );
    }

    /// A cut draw exports to SVG and to PDF exactly as it did before the
    /// route, and draws on the GPU exactly as it did: only a raster playback
    /// on the CPU applies its clip as coverage. The GPU half runs where a GPU
    /// does; under this circle the coverage route moves 50 bytes there.
    #[test]
    fn a_cut_draw_exports_to_vectors_and_draws_on_the_gpu_as_it_did() {
        use crate::export::{EncodeOptions, ImageFormat};
        fn draw(ctx: &mut crate::context2d::Context2D) {
            cut_triangle(ctx);
            ctx.begin_path();
            ctx.move_to(10.2, 100.4);
            ctx.line_to(70.7, 20.1);
            ctx.line_to(150.3, 90.9);
            ctx.stroke();
        }
        for format in [ImageFormat::Svg, ImageFormat::Pdf] {
            let export = |routed| {
                clipped_canvas(routed, false, circle, draw)
                    .to_buffer(format, &EncodeOptions::default())
                    .expect("an export")
            };
            assert!(
                numbered(&export(true)) == numbered(&export(false)),
                "{format:?}"
            );
        }
        let gpu = |routed| {
            let mut canvas = clipped_canvas(routed, true, circle, draw);
            (canvas.engine_kind() == crate::canvas::EngineKind::Gpu).then(
                || {
                    canvas
                        .to_image_data(
                            None,
                            &crate::pixels::ImageDataExport::default(),
                        )
                        .expect("pixels")
                        .pixels()
                        .to_vec()
                },
            )
        };
        if let (Some(routed), Some(plain)) = (gpu(true), gpu(false)) {
            assert!(routed == plain, "the GPU");
        }
        // The control: on the CPU the route does change the pixels.
        let cpu = |routed| {
            clipped_canvas(routed, false, circle, draw)
                .to_image_data(None, &crate::pixels::ImageDataExport::default())
                .expect("pixels")
                .pixels()
                .to_vec()
        };
        assert!(cpu(true) != cpu(false), "the CPU");
    }

    /// One step of a program the clip-run pins draw.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Step {
        /// A small triangle at this height, which a clip around the middle
        /// of the page contains.
        Fill(f32),
        /// A run of text, which records the clip's reach beside it.
        Text,
        /// A rectangle filled across the clip's edge, which Skia clips as it
        /// draws: a filled rectangle never takes the cut route.
        Rect,
        /// A blurred fill, which the page keeps as a `BlurOp`.
        Blur,
        /// A triangle blurred by `blur(n px)`: by its coverage at 3, and at
        /// 70, read at density 2, past what a mask filter can draw, by its
        /// result through `chain`.
        BlurPath(f32),
        /// A fill inside a `saveLayer`, which the page keeps as a group.
        Layer,
        /// A `clearRect` short of the page, which the page keeps as an
        /// isolated draw.
        Clear,
        /// The triangle `cut_triangle` draws, which the circle cuts.
        Cut,
        /// `getImageData` of the whole page, kept to compare.
        Read,
        /// An SVG export, kept to compare.
        Export,
        /// `save` and a clip to `circle`.
        Clip,
        /// `restore`.
        Restore,
        /// A translation, which changes the frame and not the clip.
        Move,
    }

    /// What a program drew: the page, what each `Export` step exported and
    /// what each `Read` step read.
    struct Drawn {
        canvas: crate::canvas::Canvas,
        exports: Vec<Vec<u8>>,
        reads: Vec<Vec<u8>>,
    }

    /// The page a program's steps draw, on the GPU or the CPU, the cut route
    /// on or off.
    fn program(steps: &[Step], gpu: bool, routed: bool) -> Drawn {
        use crate::export::{EncodeOptions, ImageFormat};
        ROUTES_CUTS.set(routed);
        let mut canvas = crate::canvas::Canvas::new(200.0, 120.0);
        canvas.set_gpu(gpu);
        let mut exports = vec![];
        let mut reads = vec![];
        for step in steps {
            if *step == Step::Export {
                exports.push(
                    canvas
                        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
                        .expect("an export"),
                );
                continue;
            }
            let ctx = canvas.context();
            match *step {
                Step::Fill(y) => {
                    ctx.begin_path();
                    ctx.move_to(70.0, y);
                    ctx.line_to(95.5, y + 9.25);
                    ctx.line_to(77.0, y + 17.5);
                    ctx.close_path();
                    ctx.set_fill_style_css("rgb(30 60 200 / 0.7)")
                        .expect("a colour");
                    ctx.fill(crate::path::FillRule::NonZero);
                }
                Step::Rect => {
                    ctx.set_fill_style_css("rgb(20 150 60 / 0.8)")
                        .expect("a colour");
                    ctx.fill_rect(100.5, 30.25, 60.0, 40.0);
                }
                Step::Blur => {
                    ctx.set_filter_css("blur(2px)").expect("a filter");
                    ctx.fill_rect(80.0, 50.0, 20.0, 12.0);
                    ctx.set_filter_css("none").expect("no filter");
                }
                Step::BlurPath(radius) => {
                    ctx.set_filter_css(&format!("blur({radius}px)"))
                        .expect("a filter");
                    ctx.begin_path();
                    ctx.move_to(75.0, 30.0);
                    ctx.line_to(105.5, 52.25);
                    ctx.line_to(82.0, 75.5);
                    ctx.close_path();
                    ctx.fill(crate::path::FillRule::NonZero);
                    ctx.set_filter_css("none").expect("no filter");
                }
                Step::Layer => {
                    ctx.save_layer();
                    ctx.fill_rect(60.0, 70.0, 25.0, 10.0);
                    ctx.restore();
                }
                Step::Clear => ctx.clear_rect(85.0, 40.0, 6.0, 6.0),
                Step::Text => {
                    ctx.set_font_css("16px sans-serif");
                    ctx.fill_text("Wg", 75.0, 64.0, None);
                }
                Step::Cut => {
                    ctx.set_fill_style_css("rgb(200 40 90)").expect("a colour");
                    cut_triangle(ctx);
                }
                Step::Read => reads.push(
                    ctx.get_image_data(0.0, 0.0, 200.0, 120.0)
                        .expect("pixels")
                        .pixels()
                        .to_vec(),
                ),
                Step::Clip => {
                    ctx.save();
                    ctx.begin_path();
                    circle(ctx);
                    ctx.clip(crate::path::FillRule::NonZero);
                }
                Step::Restore => ctx.restore(),
                Step::Move => ctx.translate(1.5, 0.75),
                Step::Export => unreachable!("exported above"),
            }
        }
        ROUTES_CUTS.set(true);
        Drawn {
            canvas,
            exports,
            reads,
        }
    }

    /// The page's SVG, its clips renumbered, and its PDF.
    fn documents(canvas: &mut crate::canvas::Canvas) -> (Vec<u8>, Vec<u8>) {
        use crate::export::{EncodeOptions, ImageFormat};
        let mut export = |format| {
            canvas
                .to_buffer(format, &EncodeOptions::default())
                .expect("an export")
        };
        (
            numbered(&export(ImageFormat::Svg)),
            export(ImageFormat::Pdf),
        )
    }

    /// Draws outside the clip and in it, under two frames of one clip, with
    /// text, a blurred rectangle and two blurred triangles, a draw that takes
    /// the cut route, and in each frame a rectangle the clip cuts among them.
    const CLIPPED: [Step; 15] = [
        Step::Fill(5.0),
        Step::Clip,
        Step::Fill(40.0),
        Step::Cut,
        Step::Fill(60.0),
        Step::Blur,
        Step::BlurPath(3.0),
        Step::BlurPath(70.0),
        Step::Rect,
        Step::Text,
        Step::Move,
        Step::Fill(45.0),
        Step::Rect,
        Step::Restore,
        Step::Fill(95.0),
    ];

    /// `CLIPPED` with `step` after each of its steps in turn.
    fn with_each(step: Step) -> Vec<Vec<Step>> {
        (0..=CLIPPED.len())
            .map(|at| {
                let mut steps = CLIPPED.to_vec();
                steps.insert(at, step);
                steps
            })
            .collect()
    }

    /// The SVG and PDF of `CLIPPED` drawn with no cut route and no read: one
    /// clip group a frame.
    fn plain_documents() -> (Vec<u8>, Vec<u8>) {
        documents(&mut program(&CLIPPED, false, false).canvas)
    }

    /// A PDF or SVG of a page is the same document whether or not a draw the
    /// clip cuts took the cut route between ordinary draws under the clip:
    /// the clip is written once for each frame it is drawn under, as it is
    /// for a page drawn straight through.
    #[test]
    fn a_cut_draw_does_not_split_a_document_under_one_clip() {
        let (svg, pdf) = documents(&mut program(&CLIPPED, false, true).canvas);
        let plain = plain_documents();
        assert!(svg == plain.0, "SVG");
        assert!(pdf == plain.1, "PDF");
    }

    /// The same document whether or not the page was read between any two
    /// of its draws.
    #[test]
    fn a_read_does_not_split_a_document_under_one_clip() {
        let plain = plain_documents();
        for steps in with_each(Step::Read) {
            let (svg, pdf) =
                documents(&mut program(&steps, false, true).canvas);
            assert!(svg == plain.0, "SVG of {steps:?}");
            assert!(pdf == plain.1, "PDF of {steps:?}");
        }
    }

    /// The same document whether or not the page was exported between any
    /// two of its draws.
    #[test]
    fn an_export_does_not_split_a_document_under_one_clip() {
        let plain = plain_documents();
        for steps in with_each(Step::Export) {
            let (svg, pdf) =
                documents(&mut program(&steps, false, true).canvas);
            assert!(svg == plain.0, "SVG of {steps:?}");
            assert!(pdf == plain.1, "PDF of {steps:?}");
        }
    }

    /// How many clip groups the picture of a page -- what a canvas drawn
    /// into another, or used as a pattern, is replayed into -- writes when it
    /// is played into an SVG, which writes one for each clip applied.
    fn picture_clips(canvas: &mut crate::canvas::Canvas) -> usize {
        let page = canvas.page(0).expect("a page").inner.page();
        let picture = page.picture(None).expect("a picture");
        // The size `program` draws at.
        let svg =
            skia_safe::svg::Canvas::new(Rect::from_wh(200.0, 120.0), None);
        svg.draw_picture(&picture, None, None);
        let text = svg.end();
        text.as_bytes()
            .windows(b"<clipPath".len())
            .filter(|w| *w == b"<clipPath")
            .count()
    }

    /// The picture a page is replayed into for a canvas drawn into another,
    /// or used as a pattern, applies a frame's clip as often whether or not
    /// the page was read between its draws: once around the draws between
    /// its cut draws, which clip by coverage. A read splits a run into
    /// pieces, and each piece applied the clip again: on a raster that
    /// rebuilds Skia's anti-aliased clip mask for every piece, which a page
    /// of cut draws drawn into another paid for four milliseconds of in each
    /// copy.
    #[test]
    fn a_page_drawn_into_another_applies_a_frames_clip_once() {
        let plain = picture_clips(&mut program(&CLIPPED, false, true).canvas);
        assert!(plain > 0, "the page applies its clips");
        for steps in with_each(Step::Read) {
            let clips = picture_clips(&mut program(&steps, false, true).canvas);
            assert_eq!(clips, plain, "clips in the picture of {steps:?}");
        }
    }

    /// The pixels of `canvas`, read at `density`.
    fn pixels_at(canvas: &mut crate::canvas::Canvas, density: u32) -> Vec<u8> {
        canvas
            .to_image_data(
                None,
                &crate::pixels::ImageDataExport {
                    density,
                    ..Default::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec()
    }

    /// A CPU canvas the size of `source`'s, filled with a pattern of it.
    fn patterned(source: &mut crate::canvas::Canvas) -> crate::canvas::Canvas {
        let mut filled = crate::canvas::Canvas::new(200.0, 120.0);
        filled.set_gpu(false);
        let pattern = filled.context().create_pattern_from_canvas(
            source,
            crate::pattern::PatternRepeat::Repeat,
        );
        filled.context().set_fill_pattern(&pattern);
        filled.context().fill_rect(0.0, 0.0, 200.0, 120.0);
        filled
    }

    /// The pixels, at `density`, of `source` used as a pattern filling
    /// another CPU canvas, and of `source` drawn into one with
    /// `draw_canvas`.
    fn drawn_in(
        source: &mut crate::canvas::Canvas,
        density: u32,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut filled = patterned(source);
        let mut into = crate::canvas::Canvas::new(200.0, 120.0);
        into.set_gpu(false);
        into.context().draw_canvas(source, 0.0, 0.0);
        (
            pixels_at(&mut filled, density),
            pixels_at(&mut into, density),
        )
    }

    /// `steps` without their blurs: a page a document draws as vectors, and
    /// `draw_canvas` draws through a plain paint.
    fn unblurred(steps: Vec<Step>) -> Vec<Step> {
        steps
            .into_iter()
            .filter(|step| !matches!(step, Step::Blur | Step::BlurPath(_)))
            .collect()
    }

    /// A page drawn into another canvas -- with `draw_canvas`, or as a
    /// pattern -- draws there what it draws itself, to the byte, at
    /// densities 1 to 3, read part way or not: a draw its clip cuts is as
    /// exact in the picture another canvas replays as it is on the page, and
    /// a pattern rasterizes the page as the page composites itself. With its
    /// blurs the page carries features that send `draw_canvas` through a
    /// group of its entries whatever the paint; without them a plain paint
    /// takes the route for a page holding a cut draw, and both are held
    /// here.
    #[test]
    fn a_page_drawn_into_another_draws_as_it_does_itself() {
        let mut programs = vec![CLIPPED.to_vec()];
        programs.extend(with_each(Step::Read));
        for steps in programs {
            for density in 1..=3 {
                for (steps, how) in [
                    (steps.clone(), "blurred"),
                    (unblurred(steps.clone()), "unblurred"),
                ] {
                    let mut source = program(&steps, false, true).canvas;
                    let direct = pixels_at(&mut source, density);
                    let (pattern, drawn) = drawn_in(&mut source, density);
                    assert!(
                        drawn == direct,
                        "{how}, drawn in, {density}x: {steps:?}"
                    );
                    assert!(
                        pattern == direct,
                        "{how}, a pattern, {density}x: {steps:?}"
                    );
                }
            }
        }
    }

    /// The pixels of `canvas` read at `density`, premultiplied.
    fn premultiplied_at(
        canvas: &mut crate::canvas::Canvas,
        density: u32,
    ) -> Vec<u8> {
        canvas
            .to_image_data(
                None,
                &crate::pixels::ImageDataExport {
                    density,
                    pixels: crate::pixels::PixelExportOptions {
                        premultiplied: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec()
    }

    /// A page drawn into another through a source rectangle that cuts what
    /// it draws -- `draw_canvas_region`, crop and destination the same --
    /// draws inside the crop what the page drawn in whole draws there, to
    /// the byte, and nothing outside it: as it is, and at an alpha, which
    /// composites it through a layer; on either engine at every density --
    /// see `Group::crop`. Applied as a clip, the crop cut the page's paths
    /// and moved their edges by up to 36 levels on the CPU, and on the GPU
    /// cut a blur off from what it spread into the crop by 63.
    #[test]
    fn a_page_drawn_in_cropped_draws_its_own_pixels_inside_the_crop() {
        // Each on whole pixels, so that a pixel lies inside or outside it.
        let crops = [(60.0, 20.0, 80.0, 80.0), (65.0, 10.0, 60.0, 95.0)];
        // A page holding nothing but a triangle the crops cut, which takes
        // no route of its own until it is cropped, and `CLIPPED`, with its
        // blurs and without, which holds a cut draw.
        let triangle = |gpu: bool| {
            let mut canvas = crate::canvas::Canvas::new(200.0, 120.0);
            canvas.set_gpu(gpu);
            let ctx = canvas.context();
            ctx.begin_path();
            ctx.move_to(20.0, 80.0);
            ctx.line_to(80.0, 15.25);
            ctx.line_to(140.5, 85.0);
            ctx.close_path();
            ctx.fill(crate::path::FillRule::NonZero);
            canvas
        };
        let mut failures = vec![];
        for gpu in [false, true] {
            let sources = [
                ("a triangle", triangle(gpu)),
                ("CLIPPED", program(&CLIPPED, gpu, true).canvas),
                (
                    "CLIPPED unblurred",
                    program(&unblurred(CLIPPED.to_vec()), gpu, true).canvas,
                ),
            ];
            for (name, mut source) in sources {
                for alpha in [1.0, 0.5] {
                    let mut drawn_in = |crop: Option<(f32, f32, f32, f32)>| {
                        let mut into = crate::canvas::Canvas::new(200.0, 120.0);
                        into.set_gpu(gpu);
                        let ctx = into.context();
                        ctx.set_global_alpha(alpha);
                        match crop {
                            Some((x, y, w, h)) => ctx.draw_canvas_region(
                                &mut source,
                                x,
                                y,
                                w,
                                h,
                                x,
                                y,
                                w,
                                h,
                            ),
                            None => ctx.draw_canvas(&mut source, 0.0, 0.0),
                        }
                        into
                    };
                    let mut whole = drawn_in(None);
                    let mut cropped: Vec<_> = crops
                        .iter()
                        .map(|crop| drawn_in(Some(*crop)))
                        .collect();
                    for density in 1..=3 {
                        let page = premultiplied_at(&mut whole, density);
                        for ((x, y, w, h), into) in
                            crops.iter().zip(&mut cropped)
                        {
                            let drawn = premultiplied_at(into, density);
                            let d = density as f32;
                            let width = (200.0 * d) as usize;
                            let (inside, outside) = drawn
                                .chunks(4)
                                .zip(page.chunks(4))
                                .enumerate()
                                .fold(
                                    (0u8, 0u8),
                                    |(inside, outside), (at, (got, own))| {
                                        let (col, row) = (
                                            (at % width) as f32 / d,
                                            (at / width) as f32 / d,
                                        );
                                        let within = (*x..x + w).contains(&col)
                                            && (*y..y + h).contains(&row);
                                        match within {
                                            true => (
                                                inside.max(farthest(got, own)),
                                                outside,
                                            ),
                                            false => (
                                                inside,
                                                outside.max(farthest(
                                                    got, &[0; 4],
                                                )),
                                            ),
                                        }
                                    },
                                );
                            if inside > 0 || outside > 0 {
                                failures.push(format!(
                                    "{name}, gpu {gpu}, alpha {alpha}, {density}x, crop {x},{y}: inside {inside}, outside {outside}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The most two reads part by in any byte.
    fn farthest(a: &[u8], b: &[u8]) -> u8 {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    /// Skia's picture shader rasterizes its tile as RGBA8888 whatever the
    /// surface it fills (`SkPictureShader::CachedImageInfo::Make`), and an
    /// anti-aliased translucent edge blits a premultiplied level apart in
    /// the platform's native colour type and in the other byte order: 58
    /// pixels of this triangle, on macOS, whose native type is RGBA8888,
    /// between it and BGRA8888, and on Linux, whose native type is
    /// BGRA8888, between it and RGBA8888. So a picture-shader fill draws a
    /// translucent edge as a draw onto the native surface does only where
    /// that type is RGBA8888. Fails, as the sign to look again, once the
    /// two agree everywhere, when a canvas pattern could be Skia's picture
    /// shader again -- see `TileSource`.
    #[test]
    fn a_picture_shader_tile_blits_as_rgba() {
        use skia_safe::{FilterMode, PictureRecorder, TileMode};

        let (width, height) = (200, 120);
        let page = Rect::from_wh(width as f32, height as f32);
        let mut builder = PathBuilder::new();
        builder
            .move_to((70.0, 5.0))
            .line_to((95.5, 14.25))
            .line_to((77.0, 22.5))
            .close();
        let triangle = builder.detach();
        // `rgb(30 60 200 / 0.7)`, the fill the clip-run programs draw.
        let rgb = |channel: f32| channel / u8::MAX as f32;
        let mut paint = Paint::default();
        paint.set_anti_alias(true).set_color4f(
            Color4f::new(rgb(30.0), rgb(60.0), rgb(200.0), 0.7),
            None,
        );
        let info = ImageInfo::new_n32_premul((width, height), None);
        let read = |surface: &mut skia_safe::Surface| {
            let mut pixels = vec![0; info.compute_min_byte_size()];
            assert!(surface.read_pixels(
                &info,
                &mut pixels,
                info.min_row_bytes(),
                (0, 0)
            ));
            pixels
        };

        let mut drawn = started::raster(&info, None, None).expect("a page");
        drawn.canvas().draw_path(&triangle, &paint);
        let mut recorder = PictureRecorder::new();
        recorder
            .begin_recording(page, true)
            .draw_path(&triangle, &paint);
        let picture = recorder
            .finish_recording_as_picture(None)
            .expect("a picture");
        let mut fill = Paint::default();
        fill.set_shader(picture.to_shader(
            (TileMode::Repeat, TileMode::Repeat),
            FilterMode::Linear,
            None,
            Some(&page),
        ));
        let mut tiled = started::raster(&info, None, None).expect("a page");
        tiled.canvas().draw_rect(page, &fill);

        let (drawn, tiled) = (read(&mut drawn), read(&mut tiled));
        let parted = drawn
            .chunks(4)
            .zip(tiled.chunks(4))
            .filter(|(a, b)| a != b)
            .count();
        let most = farthest(&drawn, &tiled);
        match ColorType::N32 == ColorType::RGBA8888 {
            true => assert!(parted == 0, "native RGBA: {parted} px, {most}"),
            false => assert!(
                parted > 0 && most == 1,
                "native {:?}: {parted} px, {most}",
                ColorType::N32
            ),
        }
    }

    /// A page that has itself drawn a canvas is drawn into another as
    /// pixels, made from its picture, and a draw its clip cuts is as exact
    /// there as on the page: at density 1, where those pixels are the
    /// page's own, as a pattern and with `draw_canvas` through a plain paint
    /// and through `xor`, which draws through a group over nothing as
    /// source-over does.
    #[test]
    fn a_page_that_drew_a_canvas_draws_its_cut_draws_exact_in_another() {
        let mut source = clipped_canvas(true, false, circle, |ctx| {
            let mut inner = crate::canvas::Canvas::new(4.0, 4.0);
            inner.context().fill_rect(0.0, 0.0, 4.0, 4.0);
            ctx.draw_canvas(&mut inner, 100.0, 60.0);
            cut_triangle(ctx);
        });
        let direct = pixels_at(&mut source, 1);
        let (pattern, drawn) = drawn_in(&mut source, 1);
        assert!(pattern == direct, "as a pattern");
        assert!(drawn == direct, "drawn in");
        let mut into = crate::canvas::Canvas::new(200.0, 120.0);
        into.set_gpu(false);
        into.context()
            .set_global_composite_operation(crate::paint::BlendMode::Xor);
        into.context().draw_canvas(&mut source, 0.0, 0.0);
        assert!(pixels_at(&mut into, 1) == direct, "drawn in through xor");
    }

    /// A cut draw inside a `saveLayer` opened under the clip is as exact in
    /// a page drawn into another as on the page, at densities 1 to 3: the
    /// layer applies the clip by coverage in the picture another canvas
    /// replays, as on the page, rather than being made no larger than it.
    #[test]
    fn a_cut_draw_in_a_layer_is_exact_in_a_page_drawn_into_another() {
        for density in 1..=3 {
            let mut source = clipped_canvas(true, false, circle, |ctx| {
                ctx.save_layer();
                cut_triangle(ctx);
                ctx.restore();
            });
            let direct = pixels_at(&mut source, density);
            let (pattern, drawn) = drawn_in(&mut source, density);
            assert!(pattern == direct, "as a pattern, {density}x");
            assert!(drawn == direct, "drawn in, {density}x");
        }
    }

    /// A region of a page drawn into another in place draws the page's own
    /// pixels inside the region, its cut draws exact, and nothing outside
    /// it. The region holds the whole of the cut path: one that cuts a path
    /// moves its edges as a clip does, a defect of its own.
    #[test]
    fn a_region_of_a_page_drawn_into_another_draws_the_region_alone() {
        let mut source = crate::canvas::Canvas::new(200.0, 120.0);
        source.set_gpu(false);
        {
            let ctx = source.context();
            ctx.fill_rect(165.0, 30.0, 20.0, 20.0);
            ctx.begin_path();
            circle(ctx);
            ctx.clip(crate::path::FillRule::NonZero);
            cut_triangle(ctx);
        }
        let direct = pixels_at(&mut source, 1);
        let mut into = crate::canvas::Canvas::new(200.0, 120.0);
        into.set_gpu(false);
        let (x, y, width, height) = (10.0, 10.0, 140.0, 100.0);
        into.context().draw_canvas_region(
            &mut source,
            x,
            y,
            width,
            height,
            x,
            y,
            width,
            height,
        );
        let drawn = pixels_at(&mut into, 1);
        let inside = |at: usize| {
            let (px, py) = ((at % 200) as f32, (at / 200) as f32);
            (x..x + width).contains(&px) && (y..y + height).contains(&py)
        };
        assert!(
            direct
                .chunks(4)
                .enumerate()
                .any(|(at, px)| { !inside(at) && px[3] > 0 })
        );
        for (at, (drawn, direct)) in
            drawn.chunks(4).zip(direct.chunks(4)).enumerate()
        {
            match inside(at) {
                true => assert_eq!(drawn, direct, "pixel {at}"),
                false => assert_eq!(drawn, [0; 4], "pixel {at}"),
            }
        }
    }

    /// A page drawn into another with `draw_canvas` writes into a PDF or SVG
    /// what it wrote before its cut draws took the route, read part way or
    /// not: a document clips a cut draw by the path, and has no way to write
    /// a clip by coverage at all -- Skia's SVG device writes one as an empty
    /// `clipPath`, which hides what it clips. Without the page's blurs,
    /// which a document draws as pixels, so that both documents hold the
    /// draws as vectors.
    #[test]
    fn a_page_drawn_into_another_exports_as_it_did() {
        let mut programs = vec![unblurred(CLIPPED.to_vec())];
        programs.extend(with_each(Step::Read).into_iter().map(unblurred));
        for steps in &programs {
            let export = |routed| {
                let mut source = program(steps, false, routed).canvas;
                let mut into = crate::canvas::Canvas::new(200.0, 120.0);
                into.set_gpu(false);
                into.context().draw_canvas(&mut source, 0.0, 0.0);
                documents(&mut into)
            };
            let (now, before) = (export(true), export(false));
            assert!(now.0 == before.0, "SVG of {steps:?}");
            assert!(now.1 == before.1, "PDF of {steps:?}");
        }
    }

    /// A page holding no cut draw, drawn into another with `draw_canvas`,
    /// writes the same PDF and SVG with its pieces' clip bounded or not: a
    /// document walks the picture of such a page as vectors and would write
    /// the bound as a clip of its own, so the bound is kept to pages holding
    /// a cut draw. The pieces a read files are each bounded otherwise.
    #[test]
    fn a_page_drawn_into_another_writes_no_bound_into_a_document() {
        let programs = [
            vec![
                Step::Clip,
                Step::Fill(40.0),
                Step::Read,
                Step::Fill(50.0),
                Step::Restore,
            ],
            vec![
                Step::Clip,
                Step::Fill(40.0),
                Step::Read,
                Step::Rect,
                Step::Fill(50.0),
                Step::Restore,
            ],
        ];
        for steps in &programs {
            let export = |bounded| {
                layer::BOUNDS_PIECES.set(bounded);
                let mut source = program(steps, false, true).canvas;
                let mut into = crate::canvas::Canvas::new(200.0, 120.0);
                into.set_gpu(false);
                into.context().draw_canvas(&mut source, 0.0, 0.0);
                let documents = documents(&mut into);
                layer::BOUNDS_PIECES.set(true);
                documents
            };
            let (now, unbounded) = (export(true), export(false));
            assert!(now.0 == unbounded.0, "SVG of {steps:?}");
            assert!(now.1 == unbounded.1, "PDF of {steps:?}");
        }
    }

    /// A page drawn into a GPU canvas with `draw_canvas` draws there what it
    /// draws on the GPU itself: the GPU clips a cut draw by the path, as it
    /// always has. Runs where a GPU does.
    #[test]
    fn a_page_drawn_into_a_gpu_canvas_draws_as_the_gpu_does() {
        let mut source = program(&CLIPPED, true, true).canvas;
        if source.engine_kind() != crate::canvas::EngineKind::Gpu {
            return;
        }
        let direct = pixels_at(&mut source, 2);
        let mut into = crate::canvas::Canvas::new(200.0, 120.0);
        into.set_gpu(true);
        into.context().draw_canvas(&mut source, 0.0, 0.0);
        assert!(pixels_at(&mut into, 2) == direct);
    }

    /// A piece of a clip run is bounded by what it draws, not by the clip:
    /// a small triangle under a large circle, filed as a piece by a read,
    /// reaches the triangle's rows and not the circle's.
    #[test]
    fn a_run_piece_is_bounded_by_what_it_draws() {
        let steps = [Step::Clip, Step::Fill(40.0), Step::Read, Step::Restore];
        let mut canvas = program(&steps, false, true).canvas;
        let page = canvas.page(0).expect("a page").inner.page();
        let piece = page
            .layers
            .iter()
            .find_map(|layer| match layer {
                layer::Layer::Clipped(piece) => Some(piece.clone()),
                _ => None,
            })
            .expect("a piece");
        let drawn = piece.drawn().expect("a bound");
        let clip = piece.clip_bounds();
        assert!(clip.contains(drawn), "{drawn:?} within {clip:?}");
        assert!(
            drawn.height() < clip.height() / 2.0,
            "{drawn:?} against {clip:?}"
        );
    }

    /// A kind of draw `bounded_kinds` lists: its name, whether its stretch
    /// is bounded, and the draw.
    type BoundedKind =
        (&'static str, bool, fn(&mut crate::context2d::Context2D));

    /// One kind of draw a bound must hold whole, placed at fractional
    /// coordinates: where it inks is where its piece's bound is drawn. Each
    /// with whether its stretch of the run is bounded: a draw with a shadow
    /// or a filter is a group or a blur of its own, which has no rectangle
    /// to bound by, and a draw whose ink crosses the clip's edge is left
    /// unbounded -- see `Clipped::play_run`.
    fn bounded_kinds() -> Vec<BoundedKind> {
        use crate::paint::{StrokeCap, StrokeJoin};
        vec![
            ("an anti-aliased fill", true, |ctx| {
                ctx.begin_path();
                ctx.move_to(70.3, 40.6);
                ctx.line_to(98.7, 52.35);
                ctx.line_to(77.15, 71.9);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
            // Joined at 53 degrees, mitred under a limit of 4, which Skia
            // bounds by the stroke's radius times the limit.
            ("a mitred spike", true, |ctx| {
                ctx.set_line_width(3.3);
                ctx.set_line_join(StrokeJoin::Miter);
                ctx.set_miter_limit(4.0);
                ctx.begin_path();
                ctx.move_to(80.35, 67.6);
                ctx.line_to(90.55, 47.15);
                ctx.line_to(100.65, 67.9);
                ctx.stroke();
            }),
            // Joined round: Skia bounds a stroke with mitred joins by its
            // radius times the miter limit, which no clip here contains.
            ("square and round caps", true, |ctx| {
                ctx.set_line_width(5.7);
                ctx.set_line_join(StrokeJoin::Round);
                ctx.set_line_cap(StrokeCap::Square);
                ctx.begin_path();
                ctx.move_to(70.25, 45.5);
                ctx.line_to(104.75, 49.25);
                ctx.stroke();
                ctx.set_line_cap(StrokeCap::Round);
                ctx.begin_path();
                ctx.move_to(72.6, 75.35);
                ctx.line_to(101.4, 69.8);
                ctx.stroke();
            }),
            // A rectangle's shadow is a blurred rectangle of its own --
            // see `Context2D::files_rect_shadow` -- which a piece bounds.
            ("a shadow, offset and blurred", true, |ctx| {
                ctx.set_shadow_color_css("rgb(20 140 60)")
                    .expect("a colour");
                ctx.set_shadow_offset(6.5, 4.25);
                ctx.set_shadow_blur(5.0);
                ctx.fill_rect(70.4, 45.6, 18.3, 12.7);
            }),
            // The draw is filed apart from its shadow's group, and asks
            // for no filter of its own -- see `render_to_canvas`.
            ("a shadow offset, unblurred", true, |ctx| {
                ctx.set_shadow_color_css("rgb(20 140 60)")
                    .expect("a colour");
                ctx.set_shadow_offset(7.5, 5.25);
                ctx.begin_path();
                ctx.move_to(70.3, 44.6);
                ctx.line_to(92.7, 52.35);
                ctx.line_to(77.15, 66.9);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
            ("a drop-shadow filter, unblurred", false, |ctx| {
                ctx.set_filter_css(
                    "drop-shadow(6.5px 4.25px 0 rgb(40 60 200))",
                )
                .expect("a filter");
                ctx.fill_rect(70.4, 45.6, 18.3, 12.7);
            }),
            ("a drop-shadow filter", false, |ctx| {
                ctx.set_filter_css(
                    "drop-shadow(5.5px 3.25px 2px rgb(40 60 200))",
                )
                .expect("a filter");
                ctx.begin_path();
                ctx.move_to(70.3, 44.6);
                ctx.line_to(95.7, 52.35);
                ctx.line_to(77.15, 69.9);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
            ("an image drawn smoothed at a fraction", true, |ctx| {
                let pixels: Vec<u8> = (0..8 * 8)
                    .flat_map(|i| {
                        [(i * 29 % 256) as u8, 90, (i * 7 % 256) as u8, 255]
                    })
                    .collect();
                let image = crate::image::Image::from_pixels(
                    &pixels,
                    8,
                    8,
                    32,
                    Default::default(),
                )
                .expect("an image");
                ctx.set_image_smoothing_enabled(true);
                ctx.draw_image_sized(&image, 72.35, 44.6, 21.3, 17.85);
            }),
            ("a shadowed triangle the clip cuts", false, |ctx| {
                ctx.set_shadow_color_css("rgb(20 140 60)")
                    .expect("a colour");
                ctx.set_shadow_offset(3.5, 2.25);
                ctx.set_shadow_blur(2.0);
                ctx.begin_path();
                ctx.move_to(118.3, 40.6);
                ctx.line_to(146.7, 52.35);
                ctx.line_to(125.15, 71.9);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
            }),
        ]
    }

    /// A page clipped to `circle` with `kind` drawn, a cut triangle, and
    /// `kind` drawn again moved down, so that the run splits on the cut
    /// draw into pieces either side of it -- drawn into another canvas as a
    /// pattern and with `draw_canvas`, read at `density`, its pieces' clip
    /// bounded or not.
    fn bounded_in(
        kind: fn(&mut crate::context2d::Context2D),
        density: u32,
        bounded: bool,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut source = crate::canvas::Canvas::new(200.0, 120.0);
        source.set_gpu(false);
        {
            let ctx = source.context();
            ctx.begin_path();
            circle(ctx);
            ctx.clip(crate::path::FillRule::NonZero);
            ctx.save();
            kind(ctx);
            ctx.restore();
            ctx.set_fill_style_css("rgb(200 40 90)").expect("a colour");
            cut_triangle(ctx);
            ctx.translate(0.5, 21.25);
            kind(ctx);
        }
        layer::BOUNDS_PIECES.set(bounded);
        let drawn = drawn_in(&mut source, density);
        layer::BOUNDS_PIECES.set(true);
        drawn
    }

    /// How many times drawing `kind` in as [`bounded_in`] does bounded a
    /// run's clip.
    fn bounds_applied(kind: fn(&mut crate::context2d::Context2D)) -> usize {
        layer::BOUNDED.take();
        bounded_in(kind, 1, true);
        layer::BOUNDED.take().len()
    }

    /// The source of the timing rows that draw a clipped canvas into
    /// another: 50 small fills in a band 37 pixels tall under a circle of
    /// radius 450.3, the fills at its edge cut draws by their bounds, read
    /// once part way.
    fn row_source() -> crate::canvas::Canvas {
        let mut source = crate::canvas::Canvas::new(1000.0, 1000.0);
        source.set_gpu(false);
        let ctx = source.context();
        ctx.set_fill_style_css("rgba(30 60 200 / 0.7)")
            .expect("a colour");
        ctx.begin_path();
        ctx.arc(500.0, 500.0, 450.3, 0.0, std::f32::consts::TAU, false)
            .expect("a positive radius");
        ctx.clip(crate::path::FillRule::NonZero);
        for i in 0..50 {
            let (x, y) =
                ((i % 40 * 20 + 200) as f32, (i / 40 * 20 + 400) as f32);
            if i == 25 {
                ctx.get_image_data(0.0, 0.0, 1.0, 1.0).expect("pixels");
            }
            ctx.begin_path();
            ctx.move_to(x + 2.0, y + 3.0);
            ctx.line_to(x + 17.5, y + 8.25);
            ctx.line_to(x + 6.0, y + 17.0);
            ctx.close_path();
            ctx.fill(crate::path::FillRule::NonZero);
        }
        source
    }

    /// Used as a pattern, the timing rows' source bounds its clip by the
    /// band its fills occupy, not by the circle's 900 rows: the mask Skia
    /// builds is the band's. Every rectangle the clip is bounded by is at
    /// most the band's 37 rows and a row of rounding either side.
    #[test]
    fn a_drawn_in_run_bounds_its_clip_by_the_band_it_draws() {
        let mut source = row_source();
        let mut into = crate::canvas::Canvas::new(1000.0, 1000.0);
        into.set_gpu(false);
        layer::BOUNDED.take();
        let pattern = into.context().create_pattern_from_canvas(
            &mut source,
            crate::pattern::PatternRepeat::NoRepeat,
        );
        into.context().set_fill_pattern(&pattern);
        into.context().fill_rect(0.0, 0.0, 1000.0, 1000.0);
        pixels_at(&mut into, 1);
        let bounded = layer::BOUNDED.take();
        assert!(!bounded.is_empty(), "the clip is bounded");
        for mask in bounded {
            assert!(mask.height() <= 39, "{mask:?}");
            assert!(mask.top >= 400 && mask.bottom <= 438, "{mask:?}");
        }
    }

    /// A piece whose paint Skia cannot bound -- an image filter that turns
    /// transparent black into a colour, which
    /// `SkImageFilter::canComputeFastBounds` refuses -- reaches the clip's
    /// bounds, which no clip contains, so its stretch is not bounded; the
    /// same draw through a plain paint reaches only what it draws.
    #[test]
    fn a_piece_skia_cannot_bound_reaches_the_clip() {
        let piece = |filtered: bool| {
            let mut recorder = skia_safe::PictureRecorder::new();
            let canvas =
                recorder.begin_recording(Rect::from_wh(200.0, 120.0), false);
            let mut paint = Paint::default();
            if filtered {
                let flood = skia_safe::color_filters::blend(
                    Color::from_rgb(200, 40, 90),
                    BlendMode::SrcOver,
                )
                .expect("a colour filter");
                let filter =
                    skia_safe::image_filters::color_filter(flood, None, None)
                        .expect("an image filter");
                assert!(!filter.can_compute_fast_bounds(), "the control");
                paint.set_image_filter(filter);
            }
            canvas.draw_rect(Rect::from_xywh(80.5, 50.25, 10.0, 8.0), &paint);
            let picture = recorder
                .finish_recording_as_picture(None)
                .expect("a picture");
            let clip =
                Path::rect(Rect::from_xywh(40.0, 20.0, 100.0, 80.0), None);
            layer::Clipped::new(
                7,
                &layer::ClipData::of(&clip),
                layer::Layer::Picture(picture),
            )
        };
        let clip = Rect::from_xywh(40.0, 20.0, 100.0, 80.0);
        assert_eq!(piece(true).drawn(), Some(clip));
        let plain = piece(false).drawn().expect("a bound");
        assert!(plain.height() < 10.0, "{plain:?}");
    }

    /// A piece grown by the next draw of its run forgets what it was found
    /// to draw, and finds it again from what it now draws.
    #[test]
    fn a_grown_piece_finds_again_what_it_draws() {
        let picture = |height: f32| {
            let mut recorder = skia_safe::PictureRecorder::new();
            let canvas =
                recorder.begin_recording(Rect::from_wh(200.0, 120.0), false);
            canvas.draw_rect(
                Rect::from_xywh(60.0, 30.0, 20.0, height),
                &Paint::default(),
            );
            recorder
                .finish_recording_as_picture(None)
                .expect("a picture")
        };
        let clip = Path::rect(Rect::from_xywh(40.0, 20.0, 100.0, 80.0), None);
        let mut piece = layer::Clipped::new(
            7,
            &layer::ClipData::of(&clip),
            layer::Layer::Picture(picture(10.0)),
        );
        assert_eq!(piece.drawn().map(|drawn| drawn.height()), Some(10.0));
        *piece.entry_mut() = layer::Layer::Picture(picture(40.0));
        assert_eq!(piece.drawn().map(|drawn| drawn.height()), Some(40.0));
    }

    /// Every kind of draw a piece holds -- its anti-aliased fringe, a
    /// mitred spike, caps, a shadow's offset and blur, a filter's reach and
    /// an image's smoothed footprint, and a draw the clip cuts that is not
    /// routed -- is drawn byte for byte as it is with the pieces' clip
    /// unbounded, at densities 1 to 3, as a pattern and with `draw_canvas`.
    /// Each is bounded or not as `bounded_kinds` says, so that a kind cannot
    /// pass by never being bounded at all.
    #[test]
    fn a_bounded_piece_draws_what_an_unbounded_one_does() {
        for (name, bounded, kind) in bounded_kinds() {
            assert_eq!(bounds_applied(kind) > 0, bounded, "{name} bounded");
        }
        for (name, _, kind) in bounded_kinds() {
            for density in 1..=3 {
                let (pattern, drawn) = bounded_in(kind, density, true);
                let (pattern_unbounded, drawn_unbounded) =
                    bounded_in(kind, density, false);
                assert!(
                    pattern.iter().any(|&v| v != 0),
                    "{name} draws something"
                );
                assert!(
                    pattern == pattern_unbounded,
                    "{name} as a pattern, {density}x"
                );
                assert!(
                    drawn == drawn_unbounded,
                    "{name} drawn in, {density}x"
                );
            }
        }
    }

    /// A page clipped to `circle` with `kind` drawn, read, and `kind` drawn
    /// again moved down, so that its run is filed in pieces either side of
    /// the read -- read at `density`, its pieces' clip bounded or not.
    fn bounded_on_page(
        kind: fn(&mut crate::context2d::Context2D),
        density: u32,
        bounded: bool,
    ) -> Vec<u8> {
        let mut page = crate::canvas::Canvas::new(200.0, 120.0);
        page.set_gpu(false);
        {
            let ctx = page.context();
            ctx.begin_path();
            circle(ctx);
            ctx.clip(crate::path::FillRule::NonZero);
            ctx.save();
            kind(ctx);
            ctx.restore();
            ctx.get_image_data(0.0, 0.0, 1.0, 1.0).expect("pixels");
            ctx.translate(0.5, 21.25);
            kind(ctx);
        }
        layer::BOUNDS_PAGE.set(bounded);
        let pixels = pixels_at(&mut page, density);
        layer::BOUNDS_PAGE.set(true);
        pixels
    }

    /// The kinds of draw a bounded clip must hold whole, drawn on a CPU page
    /// whose run a read files in pieces: byte for byte what the page draws
    /// with its pieces' clip unbounded, at densities 1 to 3, each bounded or
    /// not as `bounded_kinds` says.
    #[test]
    fn a_bounded_page_piece_draws_what_an_unbounded_one_does() {
        for (name, bounded, kind) in bounded_kinds() {
            layer::BOUNDED.take();
            bounded_on_page(kind, 1, true);
            assert_eq!(
                !layer::BOUNDED.take().is_empty(),
                bounded,
                "{name} bounded"
            );
            for density in 1..=3 {
                assert!(
                    bounded_on_page(kind, density, true)
                        == bounded_on_page(kind, density, false),
                    "{name}, {density}x"
                );
            }
        }
    }

    /// Every program of the clip-run pins, read part way or cut, draws the
    /// same page with its pieces' clip bounded as unbounded, at densities 1
    /// and 2, and reads the same part way.
    #[test]
    fn a_clip_run_draws_the_same_page_bounded() {
        let mut programs = vec![CLIPPED.to_vec()];
        programs.extend(with_each(Step::Read));
        programs.extend(with_each(Step::Cut));
        layer::BOUNDED.take();
        for steps in &programs {
            let drawn = |bounded| {
                layer::BOUNDS_PAGE.set(bounded);
                let mut drawn = program(steps, false, true);
                let pixels =
                    [1, 2].map(|density| pixels_at(&mut drawn.canvas, density));
                layer::BOUNDS_PAGE.set(true);
                (pixels, drawn.reads)
            };
            let (now, unbounded) = (drawn(true), drawn(false));
            assert!(now.0 == unbounded.0, "pixels of {steps:?}");
            assert!(now.1 == unbounded.1, "reads of {steps:?}");
        }
        let bounded = layer::BOUNDED.take().len();
        assert!(bounded > 0, "the programs' pieces are bounded");
    }

    /// A GPU page plays its pieces with their clip unbounded: the mask the
    /// bound keeps small is the one Skia builds on the CPU, and the GPU
    /// clips by other means. Runs where a GPU does.
    #[test]
    fn a_gpu_page_does_not_bound_its_pieces() {
        let steps = [
            Step::Clip,
            Step::Fill(40.0),
            Step::Read,
            Step::Fill(50.0),
            Step::Restore,
        ];
        let mut drawn = program(&steps, true, true);
        if drawn.canvas.engine_kind() != crate::canvas::EngineKind::Gpu {
            return;
        }
        layer::BOUNDED.take();
        pixels_at(&mut drawn.canvas, 1);
        assert!(layer::BOUNDED.take().is_empty());
    }

    /// Drawn in with `draw_canvas`, which plays the page's entries on the
    /// destination's raster, the timing rows' source bounds each piece's
    /// clip by what it draws: every rectangle within the band its fills
    /// occupy, and none with the bound turned off.
    #[test]
    fn a_page_drawn_in_bounds_its_pieces_by_the_band_they_draw() {
        let mut source = row_source();
        let mut into = crate::canvas::Canvas::new(1000.0, 1000.0);
        into.set_gpu(false);
        into.context().draw_canvas(&mut source, 0.0, 0.0);
        layer::BOUNDED.take();
        pixels_at(&mut into, 1);
        let bounded = layer::BOUNDED.take();
        assert!(!bounded.is_empty(), "the pieces' clip is bounded");
        for mask in bounded {
            assert!(mask.height() <= 39, "{mask:?}");
            assert!(mask.top >= 400 && mask.bottom <= 438, "{mask:?}");
        }
    }

    /// A piece of a clip run records its transform and its draws and
    /// nothing else: a run's recording holds no save, so finishing it has
    /// nothing to restore. Read once between two fills, each piece holds the
    /// transform and one fill; with a save it held four operations. On a page
    /// that keeps a recording for pixels, each of the piece's pair does.
    #[test]
    fn a_run_piece_records_its_transform_and_draws_alone() {
        let steps = [
            Step::Clip,
            Step::Fill(40.0),
            Step::Read,
            Step::Fill(50.0),
            Step::Restore,
        ];
        let mut canvas = program(&steps, false, true).canvas;
        let page = canvas.page(0).expect("a page").inner.page();
        let counts: Vec<Vec<usize>> = page
            .layers
            .iter()
            .filter_map(|layer| match layer {
                layer::Layer::Clipped(piece) => Some(
                    recordings(piece.entry())
                        .iter()
                        .map(|picture| picture.approximate_op_count())
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(counts.len(), 2, "two pieces: {counts:?}");
        assert!(
            counts.iter().flatten().all(|&ops| ops == 2),
            "operations in each piece: {counts:?}"
        );
    }

    /// How many clips a PDF's content streams set: the `W n` and `W* n`
    /// operators, in every stream inflated.
    fn pdf_clips(pdf: &[u8]) -> usize {
        use std::io::Read;
        let find = |hay: &[u8], needle: &[u8], from: usize| {
            hay[from..]
                .windows(needle.len())
                .position(|w| w == needle)
                .map(|at| at + from)
        };
        let mut clips = 0;
        let mut at = 0;
        while let Some(start) = find(pdf, b"stream\n", at) {
            let body = start + b"stream\n".len();
            let Some(end) = find(pdf, b"endstream", body) else {
                break;
            };
            let mut text = Vec::new();
            if flate2::read::ZlibDecoder::new(&pdf[body..end])
                .read_to_end(&mut text)
                .is_ok()
            {
                clips += text.windows(3).filter(|w| *w == b"W n").count()
                    + text.windows(4).filter(|w| *w == b"W* n").count();
            }
            // Past `endstream`, whose tail would match `stream` again.
            at = end + b"endstream".len();
        }
        clips
    }

    /// A layer around a small draw under a large clip reaches what the draw
    /// does, not the clip: a run inside a `saveLayer` keeps the bounds of
    /// what it records, which the layer is confined to.
    #[test]
    fn a_layer_of_clipped_draws_reaches_what_they_draw() {
        // A triangle the clip contains, and one it cuts, which is recorded
        // as a cut op.
        for (cx, r) in [(200.0, 180.3), (210.0, 12.3)] {
            let mut canvas = crate::canvas::Canvas::new(400.0, 400.0);
            canvas.set_gpu(false);
            {
                let ctx = canvas.context();
                ctx.begin_path();
                ctx.arc(cx, 200.0, r, 0.0, std::f32::consts::TAU, false)
                    .expect("a positive radius");
                ctx.clip(crate::path::FillRule::NonZero);
                ctx.save_layer_with(0.5, None, None);
                ctx.begin_path();
                ctx.move_to(190.0, 190.0);
                ctx.line_to(215.5, 199.25);
                ctx.line_to(197.0, 207.5);
                ctx.close_path();
                ctx.fill(crate::path::FillRule::NonZero);
                ctx.restore();
            }
            let page = canvas.page(0).expect("a page").inner.page();
            let group = page
                .layers
                .iter()
                .find_map(|layer| match layer {
                    layer::Layer::Group(group) => Some(group.clone()),
                    _ => None,
                })
                .expect("the layer");
            assert_eq!(
                cut_ops(&group.contents.layers),
                usize::from(r < 100.0),
                "radius {r}: cut ops in the layer"
            );
            let reach = group.reach().expect("a reach");
            assert!(
                reach.width() < 40.0 && reach.height() < 40.0,
                "radius {r}: the layer reaches {reach:?}, past the triangle"
            );
        }
    }

    /// The cut draws of one run share one coverage picture of its clip, made
    /// once for the run, rather than one each.
    #[test]
    fn a_runs_cut_draws_share_one_coverage() {
        let steps = [
            Step::Clip,
            Step::Cut,
            Step::Fill(40.0),
            Step::Cut,
            Step::Read,
            Step::Cut,
            Step::Restore,
        ];
        let mut canvas = program(&steps, false, true).canvas;
        let page = canvas.page(0).expect("a page").inner.page();
        let ops: Vec<_> = page
            .layers
            .iter()
            .filter_map(|layer| match layer {
                layer::Layer::Clipped(piece) => match piece.entry() {
                    layer::Layer::Cut(op) => Some(op.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(ops.len(), 3, "a cut op for each cut draw");
        assert!(
            ops.iter().all(|op| op.shares_coverage_with(&ops[0])),
            "one coverage picture for the run"
        );
    }

    /// A blurred draw under a clip is a piece of its frame's run, so a PDF
    /// writes the frame's clip once around it and the draws beside it, as
    /// for the draws alone. Before runs the blur was an entry with its clip
    /// of its own and split the frame in three.
    #[test]
    fn a_blur_joins_its_frame_in_a_document() {
        let clips = |steps: &[Step], runs: bool| {
            page::KEEPS_RUNS.set(runs);
            let (_, pdf) = documents(&mut program(steps, false, true).canvas);
            page::KEEPS_RUNS.set(true);
            pdf_clips(&pdf)
        };
        let with = [
            Step::Clip,
            Step::Fill(40.0),
            Step::Blur,
            Step::Fill(50.0),
            Step::Restore,
        ];
        let without = [
            Step::Clip,
            Step::Fill(40.0),
            Step::Fill(50.0),
            Step::Restore,
        ];
        assert_eq!(clips(&without, true), 1, "the draws alone");
        assert_eq!(clips(&with, true), 1, "with a blur among them");
        assert_eq!(clips(&with, false), 3, "with a blur, before runs");
    }

    /// A layer and an isolated draw under a clip split the page's recording
    /// as they did before runs, and a document writes the frame's clip again
    /// after each, as it did: each is a page entry with its clip of its own,
    /// and the run is filed before it. Joining either would need the group
    /// to play without the clip it sizes the layer by, or each piece of a
    /// run to carry its own features; and an SVG export rasterizes both, so
    /// only a PDF would gain. Pinned so that the documents of such a page
    /// stay what they were.
    #[test]
    fn a_layer_and_an_isolated_draw_keep_their_documents() {
        let steps = [
            Step::Clip,
            Step::Fill(40.0),
            Step::Layer,
            Step::Fill(60.0),
            Step::Clear,
            Step::Fill(45.0),
            Step::Restore,
        ];
        let documents_with = |runs: bool| {
            page::KEEPS_RUNS.set(runs);
            let drawn = documents(&mut program(&steps, false, true).canvas);
            page::KEEPS_RUNS.set(true);
            drawn
        };
        let (with, without) = (documents_with(true), documents_with(false));
        assert!(with.0 == without.0, "SVG");
        assert!(with.1 == without.1, "PDF");
        // The steps make what they are named for.
        let mut canvas = program(&steps, false, true).canvas;
        let page = canvas.page(0).expect("a page").inner.page();
        let kinds: Vec<&str> = page
            .layers
            .iter()
            .zip(page.features.iter())
            .map(|(layer, features)| match layer {
                layer::Layer::Group(_) => "group",
                _ if *features != VectorFeatures::PLAIN => "isolated",
                _ => "plain",
            })
            .collect();
        for kind in ["group", "isolated"] {
            assert!(kinds.contains(&kind), "a {kind} among {kinds:?}");
        }
    }

    /// An export part way through a page is the export of the page drawn up
    /// to there, the frame it was in ended: what the run of that frame holds
    /// is in it.
    #[test]
    fn an_export_part_way_is_the_page_up_to_there() {
        for (at, steps) in with_each(Step::Export).into_iter().enumerate() {
            let exports = program(&steps, false, true).exports;
            let mut before = CLIPPED[..at].to_vec();
            before.push(Step::Restore);
            let (svg, _) = documents(&mut program(&before, false, true).canvas);
            assert!(numbered(&exports[0]) == svg, "an export after {before:?}");
        }
    }

    /// The pixels of a page drawn under clips, and of every read taken part
    /// way, are those it drew with every frame's clip recorded into the
    /// page's own picture, on the CPU and on the GPU.
    #[test]
    fn a_clip_run_draws_the_pixels_a_clip_in_the_picture_does() {
        let drawn = |steps: &[Step], gpu: bool, runs: bool| {
            page::KEEPS_RUNS.set(runs);
            let Drawn {
                mut canvas, reads, ..
            } = program(steps, gpu, true);
            page::KEEPS_RUNS.set(true);
            let engine = canvas.engine_kind() == crate::canvas::EngineKind::Gpu;
            (engine == gpu).then(|| {
                let page = canvas
                    .to_image_data(
                        None,
                        &crate::pixels::ImageDataExport {
                            density: 2,
                            ..Default::default()
                        },
                    )
                    .expect("pixels")
                    .pixels()
                    .to_vec();
                (page, reads)
            })
        };
        let plain: Vec<Step> = CLIPPED
            .iter()
            .copied()
            .filter(|step| *step != Step::Cut)
            .collect();
        let mut programs = vec![plain.clone()];
        programs.extend((0..=plain.len()).map(|at| {
            let mut steps = plain.clone();
            steps.insert(at, Step::Read);
            steps
        }));
        for gpu in [false, true] {
            for steps in &programs {
                let (Some(runs), Some(picture)) =
                    (drawn(steps, gpu, true), drawn(steps, gpu, false))
                else {
                    continue;
                };
                assert!(
                    runs.0 == picture.0,
                    "gpu {gpu}: the page of {steps:?}"
                );
                assert!(
                    runs.1 == picture.1,
                    "gpu {gpu}: the reads of {steps:?}"
                );
            }
        }
    }

    /// The recordings `entry` holds: its picture, or both of the pair a page
    /// that keeps a recording for pixels files -- see `page::Recorders`.
    fn recordings(entry: &layer::Layer) -> Vec<&Picture> {
        match entry {
            layer::Layer::Picture(picture) => vec![picture],
            layer::Layer::Targeted(pair) => vec![&pair.0, &pair.1],
            _ => vec![],
        }
    }

    /// A page drawn under clips that nothing cuts or reads part way keeps
    /// each frame's draws in its own picture, as a page always has: the run
    /// is drawn into the page's recording when its frame ends, and the page
    /// is one picture however many frames it was drawn under -- or one pair
    /// of them, on a page that keeps a recording for pixels.
    #[test]
    fn a_page_no_cut_or_read_splits_is_one_picture() {
        let steps = [
            Step::Fill(5.0),
            Step::Clip,
            Step::Fill(40.0),
            Step::Text,
            Step::Move,
            Step::Fill(50.0),
            Step::Restore,
            Step::Clip,
            Step::Fill(45.0),
            Step::Restore,
            Step::Fill(95.0),
        ];
        let mut canvas = program(&steps, false, true).canvas;
        let page = canvas.page(0).expect("a page").inner.page();
        assert_eq!(page.layers.len(), 1, "the page's entries");
        assert!(!recordings(&page.layers[0]).is_empty(), "a picture");
    }

    #[test]
    fn a_blurred_draw_kept_as_an_op_reads_as_its_painted_draw_would() {
        // `draw_path` builds a `BlurOp`'s paint and features without the
        // paint `paint_for_drawing` makes; they have to be what that paint
        // says, or an export routes the draw differently and the op paints
        // another colour.
        let mut ctx = context();
        ctx.state.global_alpha = 0.6;
        // Away from their defaults, so that a value lost on the way shows.
        ctx.state.dither = true;
        ctx.state.paint.set_anti_alias(false);
        let color = Dye::Color(Color4f::new(0.2, 0.4, 0.8, 0.9), None, None);
        ctx.state.fill_style = color.clone();
        ctx.state.stroke_style = color;
        for sigma in [0.5f32, 12.0, 60.0] {
            ctx.state.filter = Filter::new(
                &format!("blur({sigma}px)"),
                &[FilterSpec::Plain {
                    name: "blur".into(),
                    value: sigma,
                }],
            );
            for coverage in [Coverage::Rect, Coverage::Rounded, Coverage::Path]
            {
                for style in [PaintStyle::Fill, PaintStyle::Stroke] {
                    let full = ctx.paint_for_drawing(style, coverage);
                    let lean = ctx.paint_for_blur(style);
                    assert_eq!(
                        ctx.state.filter.blur_features(coverage),
                        ctx.vector_features(&full, Some(style)),
                        "features, sigma {sigma} {coverage:?} {style:?}"
                    );
                    assert_eq!(full.color4f(), lean.color4f());
                    assert_eq!(full.style(), lean.style());
                    assert_eq!(full.is_anti_alias(), lean.is_anti_alias());
                    assert_eq!(full.is_dither(), lean.is_dither());
                    assert_eq!(full.stroke_width(), lean.stroke_width());
                    assert_eq!(full.stroke_miter(), lean.stroke_miter());
                    assert_eq!(full.stroke_cap(), lean.stroke_cap());
                    assert_eq!(full.stroke_join(), lean.stroke_join());
                }
            }
        }
    }

    /// A document plays a draw dyed by a pattern made at playback as it
    /// plays the draw dyed by the pattern's picture, at the draw's alpha
    /// once: the group the draw is filed in composites at that alpha, and
    /// the draw it writes for a document holds none of its own.
    #[test]
    fn a_document_draws_a_pattern_made_at_playback_at_its_alpha_once() {
        use crate::{
            node::{image::Content, pattern::CanvasPattern},
            pattern::Pattern,
        };
        use skia_safe::TileMode;

        let (width, height) = (60.0, 40.0);
        let mut source = crate::canvas::Canvas::new(width, height);
        source.set_gpu(false);
        source
            .context()
            .set_filter_css("blur(2px)")
            .expect("a filter");
        source.context().fill_rect(10.0, 10.0, 30.0, 20.0);
        let inner = &source.context().inner;
        let repeat = (TileMode::Repeat, TileMode::Repeat);
        let made = CanvasPattern::of_page(inner, repeat);
        let identity = Matrix::new_identity();
        assert!(made.tiling().is_some(), "made at playback");
        let picture = inner.picture().expect("the page's picture");
        let size = inner.bounds.size();
        let recorded = CanvasPattern::from_parts(
            Content::Vector(picture, size),
            size,
            repeat,
            identity,
        );
        assert!(recorded.tiling().is_none(), "recorded");

        let document = |pattern: CanvasPattern| {
            let mut canvas = crate::canvas::Canvas::new(width, height);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            ctx.set_global_alpha(0.5);
            ctx.set_fill_pattern(&Pattern::from_inner(pattern));
            ctx.fill_rect(5.0, 5.0, 50.0, 30.0);
            let page = ctx.inner.page();
            let mut surface =
                started::raster_n32_premul((width as i32, height as i32))
                    .expect("a surface");
            for entry in page.layers.iter() {
                entry.playback(surface.canvas(), layer::Target::Document);
            }
            let info = surface.image_info();
            let mut pixels = vec![0u8; info.compute_min_byte_size()];
            assert!(surface.read_pixels(
                &info,
                &mut pixels,
                info.min_row_bytes(),
                (0, 0)
            ));
            pixels
        };
        let (made, recorded) = (document(made), document(recorded));
        let most = made
            .iter()
            .zip(&recorded)
            .map(|(a, b)| a.abs_diff(*b))
            .max();
        assert!(most.is_some_and(|most| most <= 1), "parted by {most:?}");
    }

    /// [`holds`] answers as `Path::conservatively_contains_rect` does, for
    /// paths and rectangles either side of every edge, touching it, inside,
    /// empty, and paths that are not convex or not closed: the bounds test
    /// it asks first may only refuse what Skia's would.
    #[test]
    fn holding_a_rect_answers_as_skia_does() {
        let page = Rect::from_wh(64.0, 48.0);
        let mut star = PathBuilder::new();
        star.move_to((0.0, 0.0))
            .line_to((70.0, 24.0))
            .line_to((0.0, 50.0))
            .line_to((32.0, 24.0))
            .close();
        let mut open = PathBuilder::new();
        open.move_to((-1.0, -1.0))
            .line_to((70.0, -1.0))
            .line_to((70.0, 60.0));
        let paths = [
            Path::rect(page, None),
            Path::rect(page.with_outset((0.5, 0.5)), None),
            Path::rect(page.with_inset((0.5, 0.0)), None),
            Path::rect(page.with_offset((0.25, 0.0)), None),
            Path::rect(Rect::from_xywh(0.0, 0.0, 10.0, 10.0), None),
            Path::oval(page.with_outset((20.0, 20.0)), None),
            Path::rect(Rect::new_empty(), None),
            star.detach(),
            open.detach(),
        ];
        let rects = [
            page,
            page.with_inset((1.0, 1.0)),
            page.with_outset((1.0, 0.0)),
            Rect::from_xywh(0.0, 0.0, 0.0, 0.0),
            Rect::from_xywh(64.0, 48.0, 0.0, 0.0),
            Rect::from_xywh(5.0, 5.0, 0.0, 10.0),
            Rect::from_xywh(-1.0, 3.0, 2.0, 2.0),
        ];
        let mut agreed = 0;
        for path in &paths {
            for &rect in &rects {
                assert_eq!(
                    holds(path, rect),
                    path.conservatively_contains_rect(rect),
                    "{:?} holding {rect:?}",
                    path.bounds()
                );
                agreed += usize::from(holds(path, rect));
            }
        }
        assert!(
            agreed > 0,
            "some rectangle is held, or this asserts nothing"
        );
    }

    /// Two colours set in turn are each converted once and then taken
    /// kept, on a page that keeps a recording for pixels: a palette, not
    /// only a run in one colour.
    #[test]
    fn colours_set_in_turn_are_each_kept() {
        let mut canvas = Canvas::with_options(
            20.0,
            20.0,
            CanvasOptions {
                gpu: false,
                color_space: PixelColorSpace::DisplayP3,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        let before = FILLS_KEPT.get();
        for i in 0..20 {
            ctx.set_fill_style_css(
                ["#336699", "color(display-p3 0.3 0.6 0.9)"][i % 2],
            )
            .expect("a colour");
            ctx.fill_rect(i as f32, 0.0, 1.0, 1.0);
        }
        assert_eq!(FILLS_KEPT.get() - before, 18, "all but the first of each");
    }

    /// Text in a palette of sixteen colours set in turn, on a Display P3
    /// float page, reads each colour's dye once however many times it comes
    /// round, on one canvas and on a second canvas on the same thread.
    #[test]
    fn a_text_palette_set_in_turn_reads_each_dye_once() {
        let colours: Vec<String> = (0..16)
            .map(|k| {
                format!(
                    "color(display-p3 {:.2} 0.4 0.7)",
                    0.05 + k as f32 * 0.05
                )
            })
            .collect();
        let draw = |rounds: usize| {
            let mut canvas = Canvas::with_options(
                64.0,
                32.0,
                CanvasOptions {
                    gpu: false,
                    color_space: PixelColorSpace::DisplayP3,
                    color_type: PixelDepth::F32,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_font_css("16px sans-serif");
            let before = layer::DYES_READ.get();
            for colour in colours.iter().cycle().take(16 * rounds) {
                ctx.set_fill_style_css(colour).expect("a colour");
                ctx.fill_text("Wg", 4.0, 20.0, None);
            }
            layer::DYES_READ.get() - before
        };
        let first = draw(3);
        assert!(first > 0, "the dye is read at all, or this asserts nothing");
        assert!(first <= 16, "each of 16 colours read once, not {first}");
        assert_eq!(draw(2), 0, "a second canvas finds them all kept");
    }

    /// Text drawn with the dyes kept on this thread writes the pixels text
    /// reading every dye afresh writes, over pages of three kinds drawn in
    /// turn on one thread: Display P3 and Rec. 2020 at full float, and
    /// Display P3 in eight bits. Each page is drawn in a colour named in its
    /// own space with the same components as on the others -- an exact
    /// colour the same but for its space -- and in sRGB colours, so a dye
    /// found for the wrong page or the wrong plain colour shows.
    #[test]
    fn a_kept_dye_draws_what_a_dye_read_afresh_draws() {
        let pages = [
            (PixelColorSpace::DisplayP3, PixelDepth::F32, "display-p3"),
            (PixelColorSpace::Rec2020, PixelDepth::F32, "rec2020"),
            (PixelColorSpace::DisplayP3, PixelDepth::Uint8, "display-p3"),
        ];
        let scene = |keep: bool| {
            layer::KEEPS_DYES.set(keep);
            let shots: Vec<Vec<u8>> = pages
                .iter()
                .map(|&(space, depth, name)| {
                    let mut canvas = Canvas::with_options(
                        80.0,
                        24.0,
                        CanvasOptions {
                            gpu: false,
                            color_space: space,
                            color_type: depth,
                            ..CanvasOptions::default()
                        },
                    )
                    .expect("a canvas");
                    let ctx = canvas.context();
                    ctx.set_font_css("16px sans-serif");
                    let colours = [
                        format!("color({name} 0.3 0.6 0.9)"),
                        "rgb(200 40 120)".to_owned(),
                        "color(srgb 0.1235 0.5 0.2)".to_owned(),
                        format!("color({name} 0.05 0.05 0.05)"),
                    ];
                    for (i, colour) in
                        colours.iter().cycle().take(8).enumerate()
                    {
                        ctx.set_global_alpha([1.0, 0.6][i / 4]);
                        ctx.set_fill_style_css(colour).expect("a colour");
                        ctx.fill_text("Wg", (i * 9) as f32, 18.0, None);
                    }
                    ctx.get_image_data(0.0, 0.0, 80.0, 24.0)
                        .expect("read")
                        .pixels()
                        .to_vec()
                })
                .collect();
            layer::KEEPS_DYES.set(true);
            shots
        };
        let read_before = layer::DYES_READ.get();
        let afresh = scene(false);
        assert!(
            layer::DYES_READ.get() > read_before,
            "the pages read dyes at all, or this asserts nothing"
        );
        // Twice kept: the first fills the table, the second draws from it.
        let _ = scene(true);
        let read_before = layer::DYES_READ.get();
        let kept = scene(true);
        assert_eq!(
            layer::DYES_READ.get(),
            read_before,
            "the second time every dye is found kept"
        );
        for (at, (kept, afresh)) in kept.iter().zip(&afresh).enumerate() {
            assert!(kept == afresh, "page {at}: the same pixels");
        }
    }

    /// A kept run drawn again in the same colour draws the blobs it kept, on
    /// a float page as on an eight-bit one: the paint a float page dyes its
    /// text with, over a colour shader, compares equal from draw to draw.
    #[test]
    fn a_kept_run_draws_its_kept_blobs_on_a_float_page() {
        use crate::node::typography::BLOB_BUILDS;
        for (space, depth) in [
            (PixelColorSpace::DisplayP3, PixelDepth::F32),
            (PixelColorSpace::Srgb, PixelDepth::Uint8),
        ] {
            let mut canvas = Canvas::with_options(
                80.0,
                24.0,
                CanvasOptions {
                    gpu: false,
                    color_space: space,
                    color_type: depth,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_font_css("16px sans-serif");
            ctx.set_fill_style_css("color(display-p3 0.3 0.6 0.9)")
                .expect("a colour");
            // Seen once, kept from the second draw, drawn from the third.
            for _ in 0..3 {
                ctx.fill_text("Wg", 4.0, 18.0, None);
            }
            let built = BLOB_BUILDS.get();
            for _ in 0..4 {
                ctx.fill_text("Wg", 4.0, 18.0, None);
            }
            assert_eq!(
                BLOB_BUILDS.get(),
                built,
                "{space:?} {depth:?}: the kept blobs are drawn"
            );
        }
    }

    /// A page of 200 fills in a colour each, drawn twice on fresh Display
    /// P3 canvases on one thread, converts each colour once: the second
    /// page finds every one kept, as a canvas made for each render does.
    #[test]
    fn a_fresh_canvas_finds_the_colours_the_last_one_converted() {
        let page = || {
            let mut canvas = Canvas::with_options(
                400.0,
                300.0,
                CanvasOptions {
                    gpu: false,
                    color_space: PixelColorSpace::DisplayP3,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            let before = FILLS_KEPT.get();
            for i in 0..200 {
                ctx.set_fill_style_css(&format!("#33{i:02x}99"))
                    .expect("a colour");
                ctx.fill_rect(
                    ((i * 17) % 380) as f32,
                    ((i * 11) % 280) as f32,
                    15.0,
                    12.0,
                );
            }
            FILLS_KEPT.get() - before
        };
        assert_eq!(page(), 0, "the first page converts every colour");
        assert_eq!(page(), 200, "the second finds every one kept");
    }

    /// Each input [`FillKey`] holds keeps two fills apart: two draws that
    /// differ in that input alone, the first on one canvas and the second
    /// on a fresh one on the same thread, draw the second as it is drawn
    /// with nothing kept -- the colour, the space it is named in, the alpha,
    /// the page's space, and whether the page composites in floats.
    #[test]
    fn a_kept_fill_is_kept_apart_by_each_input_that_decides_it() {
        type Draw = (PixelColorSpace, PixelDepth, &'static str, f32);
        let p3 = PixelColorSpace::DisplayP3;
        let srgb = PixelColorSpace::Srgb;
        let u8 = PixelDepth::Uint8;
        let pairs: [(&str, Draw, Draw); 5] = [
            ("colour", (p3, u8, "#336699", 1.0), (p3, u8, "#33669a", 1.0)),
            (
                "space",
                (p3, u8, "color(srgb 0.3 0.6 0.9)", 1.0),
                (p3, u8, "color(display-p3 0.3 0.6 0.9)", 1.0),
            ),
            ("alpha", (p3, u8, "#336699", 1.0), (p3, u8, "#336699", 0.5)),
            // A colour outside sRGB, which the two pages make different
            // fills of: one in sRGB's gamut, the other the colour itself.
            (
                "page",
                (srgb, u8, "color(display-p3 0 1 0)", 1.0),
                (p3, u8, "color(display-p3 0 1 0)", 1.0),
            ),
            (
                "floats",
                (p3, u8, "#336699", 0.5),
                (p3, PixelDepth::F16, "#336699", 0.5),
            ),
        ];
        let draw = |(space, depth, colour, alpha): Draw| {
            let mut canvas = Canvas::with_options(
                8.0,
                8.0,
                CanvasOptions {
                    gpu: false,
                    color_space: space,
                    color_type: depth,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_global_alpha(alpha);
            ctx.set_fill_style_css(colour).expect("a colour");
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
            ctx.get_image_data(0.0, 0.0, 8.0, 8.0)
                .expect("read")
                .pixels()
                .to_vec()
        };
        for (input, first, second) in pairs {
            KEEPS_FILLS.set(false);
            let afresh = draw(second);
            KEEPS_FILLS.set(true);
            draw(first);
            assert!(
                draw(second) == afresh,
                "{input}: the second draw's own fill"
            );
        }
    }

    /// A page drawn with the flat colours kept on its thread writes the
    /// pixels and PDF a page making each afresh writes, over pages drawn in
    /// turn on one thread, two canvases of each kind -- eight-bit sRGB,
    /// eight-bit Display P3, half-float Display P3, and half-float sRGB,
    /// which keeps one recording and asks nothing kept: runs in one colour,
    /// a colour named again in another space, the same components named in
    /// two spaces, an alpha changed under the same colour, a colour that
    /// does not land exactly, and strokes between fills.
    #[test]
    fn a_kept_fill_draws_what_a_fill_made_afresh_draws() {
        let scene = |keep: bool, space: PixelColorSpace, depth: PixelDepth| {
            KEEPS_FILLS.set(keep);
            let mut canvas = Canvas::with_options(
                40.0,
                30.0,
                CanvasOptions {
                    gpu: false,
                    color_space: space,
                    color_type: depth,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            let colours = [
                "rgb(200 40 120)",
                "rgb(200 40 120)",
                "color(display-p3 0.3 0.6 0.9)",
                "color(srgb 0.7843137 0.1568627 0.4705882)",
                "color(a98-rgb 0 1 0)",
                "#fff",
                "color(srgb 0.3 0.6 0.9)",
            ];
            for (i, colour) in colours.iter().cycle().take(42).enumerate() {
                ctx.set_global_alpha([1.0, 0.5, 0.31][i % 3]);
                ctx.set_fill_style_css(colour).expect("a colour");
                ctx.set_stroke_style_css(colour).expect("a colour");
                let at = (i % 7) as f32 * 5.0;
                ctx.fill_rect(at, (i / 7) as f32 * 4.0, 7.0, 5.0);
                if i % 5 == 0 {
                    ctx.stroke_rect(at + 1.0, 2.0, 4.0, 20.0);
                }
            }
            KEEPS_FILLS.set(true);
            let pixels = ctx
                .get_image_data(0.0, 0.0, 40.0, 30.0)
                .expect("read")
                .pixels()
                .to_vec();
            let pdf = canvas
                .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
                .expect("a PDF");
            (pixels, pdf)
        };
        for (space, depth, dual) in [
            (PixelColorSpace::Srgb, PixelDepth::Uint8, true),
            (PixelColorSpace::DisplayP3, PixelDepth::Uint8, true),
            (PixelColorSpace::DisplayP3, PixelDepth::F16, true),
            (PixelColorSpace::Srgb, PixelDepth::F16, false),
        ] {
            let afresh = scene(false, space, depth);
            for canvas in ["first", "second"] {
                let kept_before = FILLS_KEPT.get();
                let kept = scene(true, space, depth);
                assert_eq!(
                    FILLS_KEPT.get() > kept_before,
                    dual,
                    "{space:?} {depth:?} {canvas}: draws took fills kept \
                     only where the page keeps a recording for pixels"
                );
                assert!(
                    kept == afresh,
                    "{space:?} {depth:?} {canvas}: the same pixels and PDF"
                );
            }
        }
    }
}

#[cfg(test)]
mod release_tests {
    //! A page drops its recorders at each read and begins them again
    //! at the next draw -- see `page::PageRecorder::release_recorders`. A
    //! scene of draws and reads is held to what it leaves with the recorders
    //! kept, as a page kept them before, and to what it leaves with no read
    //! between its draws, on either engine.

    use super::page::RELEASES_RECORDERS;
    use crate::prelude::*;

    const SIDE: (f32, f32) = (120.0, 80.0);

    /// Reads the whole page, as a script reading back a frame does.
    fn read(ctx: &mut Context2D) {
        ctx.get_image_data(0.0, 0.0, SIDE.0, SIDE.1)
            .expect("a read");
    }

    /// Draws between reads, `between` called where a read may stand: under
    /// the state the last draw left, inside a clip a run records, after a
    /// transform set with nothing drawn inside the clip and outside it,
    /// around an unclipped
    /// `putImageData` and a shadow, and over three frames of an animation
    /// that clears part of the page each frame.
    fn scene(ctx: &mut Context2D, between: &dyn Fn(&mut Context2D)) {
        let colour = |r, g, b, a| RgbaLinear::from_srgb8(r, g, b, a);
        ctx.set_fill_style(colour(200, 40, 120, 0.7));
        ctx.fill_rect(5.0, 5.0, 40.0, 30.0);
        between(ctx);
        ctx.fill_rect(30.0, 20.0, 40.0, 30.0);
        ctx.save();
        ctx.translate(20.0, 10.0);
        ctx.rotate(0.3);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 50.0, 40.0);
        ctx.clip(FillRule::NonZero);
        ctx.set_global_alpha(0.6);
        ctx.fill_rect(-10.0, -10.0, 80.0, 80.0);
        between(ctx);
        ctx.set_fill_style(colour(30, 160, 60, 0.8));
        ctx.fill_rect(10.0, 10.0, 30.0, 30.0);
        between(ctx);
        ctx.translate(5.0, 5.0);
        between(ctx);
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
        ctx.restore();
        // A transform set with nothing drawn since a read, read, and drawn
        // under with no state set between: outside a clip, so that the
        // page's own recording, not a run, frames the draw.
        between(ctx);
        ctx.translate(7.0, 3.0);
        between(ctx);
        ctx.fill_rect(2.0, 50.0, 14.0, 14.0);
        ctx.reset_transform();
        let block = ctx.get_image_data(0.0, 0.0, 40.0, 40.0).expect("a read");
        ctx.scale(1.5, 1.25);
        ctx.put_image_data(&block, 60.0, 30.0).expect("a write");
        between(ctx);
        ctx.fill_rect(4.0, 40.0, 10.0, 10.0);
        ctx.set_shadow_color(colour(0, 0, 90, 0.9));
        ctx.set_shadow_blur(4.0);
        ctx.fill_rect(50.0, 5.0, 20.0, 20.0);
        between(ctx);
        ctx.set_shadow_blur(0.0);
        for frame in 0..3 {
            let x = 70.0 + frame as f32 * 6.0;
            ctx.clear_rect(60.0, 0.0, 40.0, 30.0);
            ctx.fill_rect(x, 8.0, 12.0, 12.0);
            between(ctx);
        }
    }

    /// What the scene leaves on an eight-bit sRGB page on the engine `gpu`
    /// names -- its pixels, and its PDF -- with the page releasing its
    /// recorders or keeping them, and with a read or nothing where the
    /// scene may read; `None` where no GPU engine starts.
    fn drawn(
        gpu: bool,
        releases: bool,
        reads: bool,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        RELEASES_RECORDERS.set(releases);
        let mut canvas = Canvas::new(SIDE.0, SIDE.1);
        canvas.set_gpu(gpu);
        let started = !gpu || canvas.engine_kind() == EngineKind::Gpu;
        if started {
            let between: &dyn Fn(&mut Context2D) = match reads {
                true => &read,
                false => &|_| {},
            };
            scene(canvas.context(), between);
        }
        let mut bytes = |format| {
            canvas
                .to_buffer(format, &EncodeOptions::default())
                .expect("encodes")
        };
        let kept =
            started.then(|| (bytes(ImageFormat::Raw), bytes(ImageFormat::Pdf)));
        RELEASES_RECORDERS.set(true);
        kept
    }

    #[test]
    fn a_page_drawn_and_read_by_turns_draws_as_one_that_kept_its_recorders() {
        for gpu in [false, true] {
            let Some(released) = drawn(gpu, true, true) else {
                continue;
            };
            let kept = drawn(gpu, false, true).expect("the same engine");
            let unread = drawn(gpu, true, false).expect("the same engine");
            assert!(
                released.0.iter().any(|&byte| byte != 0),
                "gpu {gpu}: nothing was drawn"
            );
            assert!(
                released == kept,
                "gpu {gpu}: released recorders drew or wrote otherwise"
            );
            assert!(
                released.0 == unread.0,
                "gpu {gpu}: a read between draws changed the pixels"
            );
        }
    }
}

#[cfg(test)]
mod fork_tests {
    //! A page that keeps two recordings forks them only at the first draw
    //! that records differently in each -- see `page::Recorders::fork`.
    //! Each scene is drawn with forks taken lazily and again with every
    //! recording forked as it begins, as every draw was recorded twice, and
    //! the two are held to the same pixels on either engine and the same
    //! PDF and SVG.

    use super::page::FORKS_LAZILY;
    use crate::prelude::*;

    /// What a scene leaves: its pixels, its PDF and SVG, and whether its
    /// page holds a draw recorded apart for each kind of target.
    #[derive(Debug, PartialEq)]
    struct Drawn {
        pixels: Vec<u8>,
        pdf: Vec<u8>,
        svg: Vec<u8>,
        forked: bool,
    }

    /// `scene` drawn on an eight-bit sRGB page on the engine `gpu` names,
    /// its recordings forked lazily or as they begin; `None` where no GPU
    /// engine starts.
    fn drawn(
        lazy: bool,
        gpu: bool,
        scene: &dyn Fn(&mut Context2D),
    ) -> Option<Drawn> {
        FORKS_LAZILY.set(lazy);
        let mut canvas = Canvas::new(120.0, 80.0);
        canvas.set_gpu(gpu);
        let started = !gpu || canvas.engine_kind() == EngineKind::Gpu;
        if started {
            scene(canvas.context());
        }
        FORKS_LAZILY.set(true);
        if !started {
            return None;
        }
        let mut bytes = |format| {
            canvas
                .to_buffer(format, &EncodeOptions::default())
                .expect("encodes")
        };
        let (pixels, pdf, svg) = (
            bytes(ImageFormat::Raw),
            bytes(ImageFormat::Pdf),
            bytes(ImageFormat::Svg),
        );
        // A pair is filed as one, or held by a cut draw's op beside its
        // draws -- see `layer::CutOp`.
        let layers = format!("{:?}", canvas.context().inner.page().layers);
        let forked =
            layers.contains("Targeted") || layers.contains("documents: Some");
        Some(Drawn {
            pixels,
            pdf,
            svg,
            forked,
        })
    }

    /// The scene drawn lazily on each engine, held to it drawn with every
    /// recording forked as it begins; what it left lazily on each.
    fn lazily_as_eagerly(scene: &dyn Fn(&mut Context2D)) -> Vec<Drawn> {
        [false, true]
            .into_iter()
            .filter_map(|gpu| {
                let lazy = drawn(true, gpu, scene)?;
                let eager = drawn(false, gpu, scene).expect("started before");
                assert!(eager.forked, "gpu {gpu}: the reference forks");
                assert_eq!(lazy.pixels, eager.pixels, "gpu {gpu}: pixels");
                assert_eq!(lazy.pdf, eager.pdf, "gpu {gpu}: PDF");
                assert_eq!(lazy.svg, eager.svg, "gpu {gpu}: SVG");
                Some(lazy)
            })
            .collect()
    }

    /// Fills in colours on their bytes: whole-number `rgb()`, hex and a
    /// named colour, opaque, under a transform, a clip and a `saveLayer`,
    /// with a stroke and a `clearRect` beside them, and a canvas of them
    /// drawn in under a turn and two translucent layers. Not a draw a clip
    /// cuts, which records into both whatever it paints -- see
    /// `PageRecorder::record_cut`.
    fn on_their_bytes(ctx: &mut Context2D) {
        ctx.set_fill_style_css("rgb(200 40 10)").expect("a colour");
        ctx.fill_rect(5.0, 5.0, 40.0, 30.0);
        ctx.set_fill_style_css("#3366cc").expect("a colour");
        ctx.save();
        ctx.translate(10.0, 4.0);
        ctx.rotate(0.2);
        ctx.fill_rect(30.0, 10.0, 35.0, 25.0);
        ctx.restore();
        ctx.translate(4.0, 3.0);
        ctx.save();
        ctx.rect(20.0, 20.0, 50.0, 40.0);
        ctx.clip(FillRule::NonZero);
        ctx.set_fill_style_css("seagreen").expect("a colour");
        ctx.fill_rect(0.0, 0.0, 120.0, 80.0);
        ctx.restore();
        ctx.save_layer();
        ctx.set_stroke_style_css("rgb(0 0 0)").expect("a colour");
        ctx.stroke_rect(60.0, 30.0, 30.0, 20.0);
        ctx.restore();
        ctx.clear_rect(8.0, 8.0, 6.0, 6.0);
        let mut source = Canvas::new(20.0, 20.0);
        source.context().fill_rect(0.0, 0.0, 20.0, 20.0);
        ctx.rotate(0.2);
        ctx.save_layer_with(0.8, None, None);
        ctx.save_layer_with(0.6, None, None);
        ctx.draw_canvas(&mut source, 10.0, 8.0);
        ctx.restore();
        ctx.restore();
    }

    /// A page whose every draw paints the same in each recording never
    /// forks, and writes the pixels and the documents every draw recorded
    /// twice did.
    #[test]
    fn a_page_of_colours_on_their_bytes_never_forks() {
        for lazy in lazily_as_eagerly(&on_their_bytes) {
            assert!(!lazy.forked, "recorded once");
        }
    }

    /// A translucent fill paints a colour placed on the bytes its product
    /// stores into the pixels and the plain colour into the document, so
    /// the recordings fork there, on the page, inside a `saveLayer` and
    /// under a clip, each after draws recorded alike -- and a shadow forks
    /// them too, its colour placed as the fill's is.
    fn forking(at: &str) -> impl Fn(&mut Context2D) + '_ {
        move |ctx: &mut Context2D| {
            ctx.set_fill_style_css("rgb(200 40 10)").expect("a colour");
            ctx.fill_rect(5.0, 5.0, 40.0, 30.0);
            ctx.translate(3.0, 2.0);
            let translucent = |ctx: &mut Context2D| {
                ctx.set_fill_style_css("rgb(30 90 200 / 0.37)")
                    .expect("a colour");
                ctx.fill_rect(20.0, 15.0, 50.0, 35.0);
                ctx.set_fill_style_css("#3366cc").expect("a colour");
                ctx.fill_rect(60.0, 40.0, 30.0, 20.0);
            };
            match at {
                "page" => translucent(ctx),
                "layer" => {
                    ctx.save_layer();
                    ctx.fill_rect(10.0, 30.0, 30.0, 20.0);
                    translucent(ctx);
                    ctx.restore();
                }
                "clip" => {
                    ctx.save();
                    ctx.rect(15.0, 10.0, 70.0, 50.0);
                    ctx.clip(FillRule::NonZero);
                    ctx.fill_rect(10.0, 30.0, 30.0, 20.0);
                    translucent(ctx);
                    ctx.restore();
                }
                "cut" => {
                    ctx.save();
                    ctx.begin_path();
                    ctx.arc(
                        55.0,
                        38.0,
                        28.0,
                        0.0,
                        std::f32::consts::TAU,
                        false,
                    )
                    .expect("an arc");
                    ctx.clip(FillRule::NonZero);
                    // Turned, so that each fill is a path the clip cuts
                    // rather than a rectangle Skia blits whole.
                    ctx.rotate(0.3);
                    ctx.fill_rect(10.0, 30.0, 60.0, 20.0);
                    translucent(ctx);
                    ctx.restore();
                }
                _ => {
                    ctx.set_shadow_color_css("rgb(10 90 200 / 0.37)")
                        .expect("a colour");
                    ctx.set_shadow_offset(2.0, 1.0);
                    ctx.fill_rect(20.0, 15.0, 50.0, 35.0);
                    ctx.set_shadow_blur(3.0);
                    ctx.fill_rect(40.0, 45.0, 30.0, 20.0);
                }
            }
            ctx.set_fill_style_css("seagreen").expect("a colour");
            ctx.fill_rect(70.0, 5.0, 30.0, 20.0);
        }
    }

    /// A page whose first draw forks its recordings holds, in each, the ops
    /// a page forked from the start does: nothing replayed, and no frame
    /// it never set. A transform set where there was none made Skia's
    /// bounds pass map every draw by a full matrix, half again the cost
    /// of finishing the recording.
    #[test]
    fn a_page_forked_at_its_first_draw_records_what_one_forked_throughout_does()
    {
        let ops = |lazy: bool| {
            FORKS_LAZILY.set(lazy);
            let mut canvas = Canvas::new(120.0, 80.0);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            ctx.set_fill_style_css("rgb(30 90 200 / 0.37)")
                .expect("a colour");
            ctx.fill_rect(5.0, 5.0, 40.0, 30.0);
            ctx.fill_rect(50.0, 20.0, 40.0, 30.0);
            let page = ctx.inner.page();
            FORKS_LAZILY.set(true);
            page.layers
                .iter()
                .map(|layer| match layer {
                    super::layer::Layer::Targeted(pair) => [
                        pair.0.approximate_op_count_nested(true),
                        pair.1.approximate_op_count_nested(true),
                    ],
                    other => panic!("not a pair: {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ops(true), ops(false));
    }

    /// A page that forks part way, on the page, inside a `saveLayer`, under
    /// a clip, under a clip that cuts its draws and for a shadow, writes the
    /// pixels and the documents every draw recorded twice did.
    #[test]
    fn a_page_that_forks_part_way_matches_one_forked_throughout() {
        for at in ["page", "layer", "clip", "cut", "shadow"] {
            for lazy in lazily_as_eagerly(&forking(at)) {
                assert!(lazy.forked, "{at}: forked");
            }
        }
    }
}

#[cfg(test)]
mod text_reach_tests {
    //! A chain's layer bounded by the text it holds draws what one bounded
    //! by the clip draws, over scenes chosen to find a reach that is too
    //! small -- see [`layer::KEEPS_TEXT_REACH`].

    use super::{TEXT_REACHED, layer::KEEPS_TEXT_REACH};
    use crate::{
        node::typography::{
            FONT_BOX_FLOOR, MATCH_IGNORES_STROKE, REACH_BOUND, REACH_INSET,
            REACHED_FAMILIES, ReachBound, forget_laid_out,
        },
        prelude::*,
    };
    use rayon::prelude::*;
    use skia_safe::Rect;
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::Instant,
    };

    /// The family Oswald is registered under for these scenes, so that it
    /// cannot be found as a system font of the same name instead.
    const OSWALD: &str = "ReachOswald";

    /// The family Monoton, which has one weight and so is emboldened by
    /// Skia where bold is asked of it, is registered under.
    const MONOTON: &str = "ReachMonoton";

    thread_local! {
        /// Whether [`OSWALD`] is registered on this thread: the registry a
        /// canvas resolves a family against is the thread's own -- see
        /// `FontLibrary::register_font_from_data` -- and the sweep plays its
        /// scenes on rayon's threads.
        static REGISTERED: Cell<bool> = const { Cell::new(false) };
        /// Which of the scenes' fonts this thread found, by font.
        static RESOLVED: RefCell<HashMap<&'static str, bool>> =
            RefCell::new(HashMap::new());
    }

    /// Registers [`OSWALD`] and [`MONOTON`] on this thread, once, and
    /// measures reaches the way `TEXT_REACH_BOUND` names -- see
    /// [`ReachBound`].
    fn register() {
        if !REGISTERED.replace(true) {
            REACH_INSET.set(
                std::env::var("TEXT_REACH_INSET")
                    .ok()
                    .and_then(|inset| inset.parse().ok())
                    .unwrap_or(0.0),
            );
            REACH_BOUND.set(
                match std::env::var("TEXT_REACH_BOUND").as_deref() {
                    Ok("stroked-unwidened") => ReachBound::StrokedUnwidened,
                    Ok("stroked-half-width") => ReachBound::StrokedHalfWidth,
                    _ => ReachBound::Stroked,
                },
            );
            FontLibrary::new()
                .use_paths(
                    Some(OSWALD),
                    ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
                )
                .expect("registers");
            FontLibrary::new()
                .use_paths(
                    Some(MONOTON),
                    ["tests/assets/fonts/Monoton-Regular.woff2"],
                )
                .expect("registers");
        }
    }

    /// Whether a run asking for `font` on this thread is drawn in it rather
    /// than in a fallback: a family not on this machine is still a valid CSS
    /// font, and quietly resolves to the platform's default.
    fn resolves(font: &'static str) -> bool {
        if let Some(found) =
            RESOLVED.with_borrow(|known| known.get(font).copied())
        {
            return found;
        }
        let family = match font {
            OSWALD => "Oswald",
            MONOTON => "Monoton",
            font => font,
        };
        // A letter and an emoji, so that a text face and an emoji face are
        // each asked for a glyph they hold.
        let found = reached(&Scene {
            font,
            text: "A\u{1F600}",
            ..BASE
        })
        .is_some()
            && REACHED_FAMILIES.with_borrow(|families| {
                families.iter().any(|f| f.eq_ignore_ascii_case(family))
            });
        RESOLVED.with_borrow_mut(|known| known.insert(font, found));
        found
    }

    /// One scene: text in a font, at a size and weight, spaced, under a
    /// transform, filled or stroked with a join and miter limit, through a
    /// filter.
    #[derive(Clone, Copy)]
    struct Scene {
        font: &'static str,
        size: f32,
        text: &'static str,
        transform: Option<Affine>,
        stroke: Option<f32>,
        join: StrokeJoin,
        miter: f32,
        bold: bool,
        /// `letterSpacing` and `wordSpacing`, in pixels.
        spacing: (f32, f32),
        filter: &'static str,
    }

    /// Room for the largest run, its stroke and its shadow cast 90 by 60.
    const SIZE: (f32, f32) = (300.0, 220.0);

    impl Scene {
        fn canvas(&self) -> Option<Canvas> {
            register();
            let mut canvas = Canvas::new(SIZE.0, SIZE.1);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            let weight = if self.bold { "bold " } else { "" };
            if !ctx.set_font_css(&format!(
                "{weight}{}px \"{}\"",
                self.size, self.font
            )) {
                return None;
            }
            ctx.set_letter_spacing(self.spacing.0);
            ctx.set_word_spacing(self.spacing.1);
            ctx.set_line_join(self.join);
            ctx.set_miter_limit(self.miter);
            ctx.set_filter_css(self.filter).expect("a filter");
            ctx.set_fill_style_css("rgb(200 120 40)").expect("a colour");
            ctx.set_stroke_style_css("rgb(30 90 200)")
                .expect("a colour");
            ctx.translate(60.3, 110.6);
            if let Some(transform) = self.transform {
                ctx.transform(transform);
            }
            match self.stroke {
                None => ctx.fill_text(self.text, 0.0, 0.0, None),
                Some(width) => {
                    ctx.set_line_width(width);
                    ctx.stroke_text(self.text, 0.0, 0.0, None);
                }
            }
            Some(canvas)
        }
    }

    /// The playbacks a scene is held at, as `(name, on the GPU, scale)`:
    /// exported at densities 1 to 3, and drawn into another canvas at half
    /// and one and a half times its size, on each engine. A chain on the GPU
    /// clears and reads only the region its reach bounds, as on the CPU, and
    /// draws large glyphs as paths under multisampling, so the reach is held
    /// to what each engine inks.
    const PLAYBACKS: [(&str, bool, f32); 10] = [
        ("cpu density 1", false, 1.0),
        ("cpu density 2", false, 2.0),
        ("cpu density 3", false, 3.0),
        ("cpu drawn at 0.5", false, 0.5),
        ("cpu drawn at 1.5", false, 1.5),
        ("gpu density 1", true, 1.0),
        ("gpu density 2", true, 2.0),
        ("gpu density 3", true, 3.0),
        ("gpu drawn at 0.5", true, 0.5),
        ("gpu drawn at 1.5", true, 1.5),
    ];

    /// Whether a canvas on this machine can be drawn on the GPU.
    fn gpu_available() -> bool {
        let mut canvas = Canvas::new(1.0, 1.0);
        canvas.set_gpu(true);
        canvas.engine_kind() == EngineKind::Gpu
    }

    /// The pixels of `canvas` played back at each of [`PLAYBACKS`],
    /// premultiplied; `None` for a GPU playback where there is no GPU.
    fn played(canvas: &mut Canvas) -> Vec<Option<Vec<u8>>> {
        let premultiplied = PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        };
        let gpu = gpu_available();
        PLAYBACKS
            .iter()
            .map(|&(_, on_gpu, scale)| {
                if on_gpu && !gpu {
                    return None;
                }
                canvas.set_gpu(on_gpu);
                let pixels = match scale.fract() == 0.0 {
                    true => canvas
                        .to_image_data(
                            None,
                            &ImageDataExport {
                                density: scale as u32,
                                pixels: premultiplied,
                                ..ImageDataExport::default()
                            },
                        )
                        .expect("pixels")
                        .pixels()
                        .to_vec(),
                    false => {
                        let mut outer =
                            Canvas::new(SIZE.0 * scale, SIZE.1 * scale);
                        outer.set_gpu(on_gpu);
                        outer.context().draw_canvas_sized(
                            canvas,
                            0.0,
                            0.0,
                            SIZE.0 * scale,
                            SIZE.1 * scale,
                        );
                        outer
                            .to_image_data(
                                None,
                                &ImageDataExport {
                                    pixels: premultiplied,
                                    ..ImageDataExport::default()
                                },
                            )
                            .expect("pixels")
                            .pixels()
                            .to_vec()
                    }
                };
                canvas.set_gpu(false);
                Some(pixels)
            })
            .collect()
    }

    /// What the scene's run reports it can ink, drawn through its filter:
    /// `None` where the scene's font is not on this machine or the run
    /// answers nothing.
    fn reached(scene: &Scene) -> Option<Rect> {
        TEXT_REACHED.set(None);
        scene.canvas()?;
        TEXT_REACHED.get()
    }

    /// [`reached`] in each state a run is drawn in: laid out afresh for the
    /// draw, as the first draw of it lays it out, and kept from draws before
    /// -- see `Typesetter::laid_out` -- which lays it out with no paint:
    /// `(state, reach)`.
    fn reached_in_each_state(scene: &Scene) -> Vec<(&'static str, Rect)> {
        crate::node::typography::forget_laid_out();
        let fresh = reached(scene);
        // Seen once, then kept: the third draw takes the kept run.
        let _ = reached(scene);
        let kept = reached(scene);
        [("laid out afresh", fresh), ("kept", kept)]
            .into_iter()
            .filter_map(|(state, reach)| Some((state, reach?)))
            .collect()
    }

    /// How many device pixels the scene's text, drawn without its filter,
    /// inks outside its reach at each scale, past [`layer::HINTED_INK_DRIFT`]
    /// and rounded out to whole pixels as a layer is: the `(scale, how
    /// far)` of each that does.
    fn inked_past(scene: &Scene, reach: Rect) -> Vec<(&'static str, f32)> {
        inked_past_by(scene, reach, super::layer::HINTED_INK_DRIFT)
    }

    /// [`inked_past`] with the reach widened by `drift` device pixels.
    fn inked_past_by(
        scene: &Scene,
        reach: Rect,
        drift: f32,
    ) -> Vec<(&'static str, f32)> {
        let mut sharp = (Scene {
            filter: "none",
            ..*scene
        })
        .canvas()
        .expect("a canvas");
        played(&mut sharp)
            .iter()
            .zip(PLAYBACKS)
            .filter_map(|(pixels, (at, _, scale))| {
                let pixels = pixels.as_ref()?;
                let width = (SIZE.0 * scale) as usize;
                let held = [
                    (reach.left * scale - drift).floor(),
                    (reach.top * scale - drift).floor(),
                    (reach.right * scale + drift).ceil(),
                    (reach.bottom * scale + drift).ceil(),
                ];
                let past = pixels
                    .chunks(4)
                    .enumerate()
                    .filter(|(_, p)| p[3] != 0)
                    .map(|(i, _)| {
                        let (x, y) = ((i % width) as f32, (i / width) as f32);
                        [
                            held[0] - x,
                            held[1] - y,
                            x + 1.0 - held[2],
                            y + 1.0 - held[3],
                        ]
                        .into_iter()
                        .fold(0.0, f32::max)
                    })
                    .fold(0.0, f32::max);
                (past > 0.0).then_some((at, past))
            })
            .collect()
    }

    /// The box, in device pixels, of what the scene's text inks drawn
    /// without its filter at `at`, one of [`PLAYBACKS`], beside its reach at
    /// that scale: what a reading past the reach is reported with, so that
    /// the edge it passes and by how much can be read from the failure.
    fn ink_beside(scene: &Scene, reach: Rect, at: &str) -> String {
        let mut sharp = (Scene {
            filter: "none",
            ..*scene
        })
        .canvas()
        .expect("a canvas");
        let (index, (_, _, scale)) = PLAYBACKS
            .iter()
            .enumerate()
            .find(|(_, (name, _, _))| *name == at)
            .expect("a playback");
        let width = (SIZE.0 * scale) as usize;
        let ink = played(&mut sharp)[index]
            .as_ref()
            .expect("the playback a reading was taken at")
            .chunks(4)
            .enumerate()
            .filter(|(_, p)| p[3] != 0)
            .fold(
                [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
                |[l, t, r, b], (i, _)| {
                    let (x, y) = ((i % width) as f32, (i / width) as f32);
                    [l.min(x), t.min(y), r.max(x + 1.0), b.max(y + 1.0)]
                },
            );
        format!(
            "ink {ink:?}, reach {:?}",
            [
                reach.left * scale,
                reach.top * scale,
                reach.right * scale,
                reach.bottom * scale
            ]
        )
    }

    /// How far, in levels of an 8-bit alpha, Skia's analytic
    /// anti-aliasing covers a curved edge from its true coverage: 40,
    /// measured, not chosen. The stroked "g" of Times New Roman's 96px
    /// "Ag", 40 wide with round joins at density 3, covers (581, 245) with
    /// an alpha of 98 where 8 by 8 samples of the same outline give 137.6,
    /// 39.6 levels short; moved 54 device pixels it covers it with 116.
    /// Measured on the CPU raster, which is the only anti-aliasing route
    /// Skia 156 has for a path -- see `skia_covers_a_curve_by_where_it_lies`.
    const AAA_COVERAGE_ERROR: u8 = 40;

    /// How far two placements of the same scene may draw any pixel apart:
    /// each covers an edge within [`AAA_COVERAGE_ERROR`] of its true
    /// coverage, so the two lie within twice that of each other. It follows
    /// from that error rather than from the spreads seen, which were 25
    /// levels under a drop shadow and 32 without the filter.
    const AAA_PLACEMENT_SPREAD: u8 = 2 * AAA_COVERAGE_ERROR;

    /// The largest difference, in premultiplied levels, between the scene
    /// drawn in a chain layer bounded by its reach and one bounded by the
    /// clip, at each scale.
    ///
    /// An end-to-end check that confining the layer changes nothing past
    /// Skia's own placement noise -- see [`AAA_PLACEMENT_SPREAD`] -- not the
    /// guard on the reach: a reach that cuts a pixel off an anti-aliased
    /// fringe can lose a few tens of levels, which the spread allows, and
    /// [`inked_past`] is what refuses that. Drawn in by TEXT_REACH_INSET, in
    /// the units the run is laid out in, the reach fails [`inked_past`] from
    /// an inset of 1 and this from one of 11: a chain's layer is its reach
    /// widened by as far as the chain reads, so an inset under that cuts
    /// nothing the chain draws.
    fn parted_from_the_clip(scene: &Scene) -> Vec<(&'static str, u8)> {
        // Each play lays its runs out and blurs them for itself. Canvases
        // with equal settings share their kept runs, and a run kept from
        // the first play would bring into the second what the first left
        // of it -- its blurred results, and on the GPU the text blobs Skia
        // cached for it at the larger playbacks -- so the reference has to
        // be its own.
        KEEPS_TEXT_REACH.set(true);
        forget_laid_out();
        let confined = played(&mut scene.canvas().expect("a canvas"));
        KEEPS_TEXT_REACH.set(false);
        forget_laid_out();
        let clipped = played(&mut scene.canvas().expect("a canvas"));
        KEEPS_TEXT_REACH.set(true);
        confined
            .iter()
            .zip(&clipped)
            .zip(PLAYBACKS)
            .filter_map(|((a, b), (at, _, _))| {
                let (a, b) = (a.as_ref()?, b.as_ref()?);
                Some((
                    at,
                    a.iter()
                        .zip(b)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap_or(0),
                ))
            })
            .collect()
    }

    const ROTATE: Affine = Affine {
        a: 0.955,
        b: 0.296,
        c: -0.296,
        d: 0.955,
        tx: 0.0,
        ty: 0.0,
    };
    const SKEW: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.3,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
    const SCALE: Affine = Affine {
        a: 1.7,
        b: 0.0,
        c: 0.0,
        d: 1.7,
        tx: 0.0,
        ty: 0.0,
    };

    const BASE: Scene = Scene {
        font: OSWALD,
        size: 40.0,
        text: "Ag",
        transform: None,
        stroke: None,
        join: StrokeJoin::Miter,
        miter: 10.0,
        bold: false,
        spacing: (0.0, 0.0),
        filter: "drop-shadow(0 0 3px black)",
    };

    /// The scenes a test run plays: the base scene varied one way at a time,
    /// and the ways found or expected hardest together. `full` plays every
    /// combination, for the sweep [`layer::HINTED_INK_DRIFT`] is measured by.
    fn sweep(full: bool) -> Vec<Scene> {
        let fonts = [
            OSWALD,
            "Helvetica",
            "Times New Roman",
            "Zapfino",
            "Apple Color Emoji",
            "DejaVu Sans",
            "Noto Color Emoji",
        ];
        let sizes = [8.0, 13.0, 40.0, 96.0];
        let texts = ["Ag", "fj Wq", "\u{1F600}x"];
        let transforms = [None, Some(ROTATE), Some(SKEW), Some(SCALE)];
        let strokes = [None, Some(1.0), Some(12.0)];
        let filters = [
            "blur(3px)",
            "drop-shadow(0 0 3px black)",
            "drop-shadow(90px 60px 2px red)",
        ];
        if full {
            let mut all = Vec::new();
            for font in fonts {
                for size in sizes {
                    for text in texts {
                        for transform in transforms {
                            for stroke in strokes {
                                for filter in filters {
                                    all.push(Scene {
                                        font,
                                        size,
                                        text,
                                        transform,
                                        stroke,
                                        filter,
                                        ..BASE
                                    });
                                }
                            }
                        }
                    }
                }
            }
            all.extend(edges());
            return all;
        }
        let mut some: Vec<Scene> = Vec::new();
        some.extend(fonts.map(|font| Scene { font, ..BASE }));
        some.extend(sizes.map(|size| Scene { size, ..BASE }));
        some.extend(texts.map(|text| Scene { text, ..BASE }));
        some.extend(transforms.map(|transform| Scene { transform, ..BASE }));
        some.extend(strokes.map(|stroke| Scene { stroke, ..BASE }));
        some.extend(filters.map(|filter| Scene { filter, ..BASE }));
        some.extend([
            // The one reading the sweep found past the reach rounded out: a
            // colour bitmap resampled at a fractional scale.
            Scene {
                text: "\u{1F600}x",
                transform: Some(SCALE),
                stroke: Some(12.0),
                ..BASE
            },
            Scene {
                font: "Zapfino",
                size: 96.0,
                text: "fj Wq",
                transform: Some(ROTATE),
                ..BASE
            },
            Scene {
                size: 8.0,
                transform: Some(SKEW),
                stroke: Some(12.0),
                filter: "drop-shadow(90px 60px 2px red)",
                ..BASE
            },
            Scene {
                font: "Noto Color Emoji",
                text: "\u{1F600}x",
                transform: Some(ROTATE),
                filter: "drop-shadow(0 0 3px black)",
                ..BASE
            },
        ]);
        some.extend(edges().into_iter().step_by(EDGES_SAMPLED));
        some
    }

    /// One in this many of [`edges`] is played by a test run; the full
    /// sweep plays them all.
    const EDGES_SAMPLED: usize = 7;

    /// The strokes the full sweep adds to its plain ones, as `(width, join,
    /// miter limit)`: a hairline, which is a device pixel wide whatever the
    /// transform, and a 40px stroke under each join, mitred at a limit that
    /// cuts every corner and at one that cuts none.
    const EDGE_STROKES: [(f32, StrokeJoin, f32); 5] = [
        (0.1, StrokeJoin::Miter, 10.0),
        (40.0, StrokeJoin::Miter, 1.0),
        (40.0, StrokeJoin::Miter, 30.0),
        (40.0, StrokeJoin::Round, 10.0),
        (40.0, StrokeJoin::Bevel, 10.0),
    ];

    /// The scenes past the plain sweep's axes: hairline and wide strokes
    /// under each join, bold drawn from a variable font's axis
    /// ([`OSWALD`]), from a face of its own (DejaVu Sans) and emboldened by
    /// Skia from a family with no bold ([`MONOTON`]), and letter and word
    /// spacing, which move the glyphs within a run.
    fn edges() -> Vec<Scene> {
        let fonts = [OSWALD, "Helvetica", "Times New Roman", "DejaVu Sans"];
        let texts = ["Ag", "fj Wq"];
        let transforms = [None, Some(ROTATE), Some(SKEW), Some(SCALE)];
        let mut all = Vec::new();
        for font in fonts {
            for size in [13.0, 96.0] {
                for text in texts {
                    for transform in transforms {
                        for (width, join, miter) in EDGE_STROKES {
                            all.push(Scene {
                                font,
                                size,
                                text,
                                transform,
                                stroke: Some(width),
                                join,
                                miter,
                                ..BASE
                            });
                        }
                    }
                }
            }
        }
        for font in [OSWALD, MONOTON, "DejaVu Sans"] {
            for size in [13.0, 40.0, 96.0] {
                for text in texts {
                    for transform in transforms {
                        for stroke in [None, Some(12.0)] {
                            all.push(Scene {
                                font,
                                size,
                                text,
                                transform,
                                stroke,
                                bold: true,
                                ..BASE
                            });
                        }
                    }
                }
            }
        }
        for font in [OSWALD, "Helvetica", "DejaVu Sans"] {
            for spacing in [(5.0, 20.0), (-2.0, 0.0)] {
                for size in [13.0, 40.0] {
                    for transform in transforms {
                        for stroke in [None, Some(12.0)] {
                            all.push(Scene {
                                font,
                                size,
                                text: "fj Wq",
                                transform,
                                stroke,
                                spacing,
                                ..BASE
                            });
                        }
                    }
                }
            }
        }
        all
    }

    /// A run of text under a filter chain is filtered in a layer the size of
    /// what it inks, and not of the page: under a `drop-shadow()` and a
    /// `blur()`, a 40px word on a 300 by 220 page is given a rectangle the
    /// size of the word, where one bounded by its picture -- which for text
    /// is the clip -- is given the page and as far again as the chain
    /// reaches. That was 1.7 ms a word under a drop shadow.
    #[test]
    fn text_in_a_chain_is_filtered_in_a_layer_the_size_of_its_ink() {
        for filter in ["drop-shadow(0 0 3px black)", "blur(3px)"] {
            let scene = Scene {
                font: "Helvetica",
                filter,
                ..BASE
            };
            let content = |keep: bool| {
                KEEPS_TEXT_REACH.set(keep);
                super::layer::CHAIN_CONTENT.set(None);
                let mut canvas = scene.canvas().expect("a canvas");
                canvas
                    .to_image_data(None, &ImageDataExport::default())
                    .expect("pixels");
                KEEPS_TEXT_REACH.set(true);
                super::layer::CHAIN_CONTENT.get()
            };
            let reach = reached(&scene).expect("a reach");
            let confined = content(true).expect("the chain's content");
            // The margin on each side, and a pixel on each for rounding out.
            const MARGINS: f32 = 2.0 * (super::layer::HINTED_INK_DRIFT + 1.0);
            assert!(
                (confined.left <= reach.left
                    && confined.top <= reach.top
                    && confined.right >= reach.right
                    && confined.bottom >= reach.bottom)
                    && confined.width() <= reach.width() + MARGINS
                    && confined.height() <= reach.height() + MARGINS,
                "{filter}: the layer is the reach, widened by the margin: {confined:?} against {reach:?}"
            );
            let page = SIZE.0 * SIZE.1;
            let clipped =
                content(false).map_or(page, |r| r.width() * r.height());
            assert!(
                clipped >= page,
                "{filter}: the reference covers the page, or this asserts nothing"
            );
        }
    }

    /// The playback the margin is measured by on CoreText: a stroke whose
    /// round join half its width bounds exactly, drawn into another canvas
    /// at half the size, where its anti-aliased fringe inks a device pixel
    /// past the reach rounded out. Held within [`layer::HINTED_INK_DRIFT`],
    /// so that a fringe reaching further shows here, and on macOS to that
    /// pixel with no margin, so that this stays the case the margin is
    /// measured by there.
    #[test]
    fn a_wide_round_stroke_inks_a_pixel_past_its_reach() {
        let scene = Scene {
            size: 13.0,
            stroke: Some(40.0),
            join: StrokeJoin::Round,
            ..BASE
        };
        let reach = reached(&scene).expect("a reach");
        assert_eq!(
            inked_past(&scene, reach),
            Vec::new(),
            "held within the margin"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            inked_past_by(&scene, reach, 0.0)
                .into_iter()
                .filter(|(at, _)| at.starts_with("cpu"))
                .collect::<Vec<_>>(),
            vec![("cpu drawn at 0.5", 1.0)],
            "a pixel past with no margin"
        );
    }

    #[test]
    fn the_font_box_widens_a_reach_only_where_it_is_kept() {
        // A run's reach takes in its font's declared box only where the
        // sweep has not been played without it -- see `keeps_font_box`.
        // Helvetica's box reaches far past "Ag": on macOS the reach is the
        // glyphs' own boxes and lies inside the one with the font's box.
        let scene = Scene {
            font: "Helvetica",
            filter: "blur(3px)",
            ..BASE
        };
        if !resolves(scene.font) {
            return;
        }
        forget_laid_out();
        let own = reached(&scene).expect("a reach");
        FONT_BOX_FLOOR.set(true);
        forget_laid_out();
        let boxed = reached(&scene).expect("a reach");
        FONT_BOX_FLOOR.set(false);
        forget_laid_out();
        eprintln!(
            "own {own:?}, {} wide; with the font's box {boxed:?}, {} wide",
            own.width(),
            boxed.width()
        );
        let mut joined = own;
        joined.join(boxed);
        assert_eq!(joined, boxed, "the font's box holds the glyphs' own");
        match cfg!(target_os = "macos") {
            true => assert!(own.width() < boxed.width() / 2.0, "{own:?}"),
            false => assert_eq!(own, boxed),
        }
    }

    /// The reach of each of `scenes`, drawn in turn on fresh canvases that
    /// share the runs they lay out -- see `Typesetting::typography` -- once
    /// each pass: the first pass lays each run out afresh, the second keeps
    /// it, and the third draws it from what the second kept. Bits, so that
    /// equal means the same answer.
    fn reaches_over_passes(scenes: &[Scene]) -> [Vec<Option<[u32; 4]>>; 3] {
        let bits =
            |r: Rect| [r.left, r.top, r.right, r.bottom].map(f32::to_bits);
        forget_laid_out();
        [0, 1, 2].map(|_| {
            scenes
                .iter()
                .map(|scene| reached(scene).map(bits))
                .collect()
        })
    }

    #[test]
    fn a_kept_runs_reach_is_the_reach_worked_out_afresh() {
        // A kept run keeps what it can ink beside the blobs of each paint
        // it is drawn in -- see `PaintedBlobs::inks` -- and places it at
        // each draw's anchor. Over the scenes a test run plays, and the same
        // run stroked at two widths in turn, the kept answer is the one
        // worked out afresh, bit for bit; a run found by the blobs of
        // another width answers that width's reach, and is caught.
        let mut scenes: Vec<Scene> = sweep(false)
            .into_iter()
            .filter(|scene| resolves(scene.font))
            .collect();
        scenes.extend(
            [Some(1.0), Some(12.0), Some(1.0)]
                .map(|stroke| Scene { stroke, ..BASE }),
        );
        let [fresh, kept, again] = reaches_over_passes(&scenes);
        assert!(fresh.iter().filter(|r| r.is_some()).count() > 10);
        assert_eq!(fresh, kept, "the second pass, which keeps the runs");
        assert_eq!(fresh, again, "the third, drawn from what was kept");
        let strokes = &scenes[scenes.len() - 3..];
        MATCH_IGNORES_STROKE.set(true);
        let [fresh, _, again] = reaches_over_passes(strokes);
        MATCH_IGNORES_STROKE.set(false);
        assert_ne!(fresh, again, "kept for another width, and not caught");
    }

    /// What a run of text in a filter chain reports it can ink holds all it
    /// inks, at every scale and on each engine it is played back on, once
    /// widened by [`layer::HINTED_INK_DRIFT`]: over a glyph whose ink passes
    /// its font's declared box (Zapfino's swashes), colour emoji, a wide
    /// stroke, a turn, a skew and a scale. And the chain bounded by it
    /// draws what one bounded by the clip draws, a drop shadow cast further
    /// than the run is long included, within [`AAA_PLACEMENT_SPREAD`]: Skia
    /// covers a curved edge by where on the device its layer puts it.
    #[test]
    fn text_in_a_chain_is_bounded_by_all_it_inks() {
        let full = std::env::var("TEXT_REACH_SWEEP").is_ok_and(|v| v == "full");
        // Each scene on a thread of its own: the switches it is drawn under
        // are the thread's, and nothing else of one scene reaches another.
        // The full sweep measures the margin, which the ink alone answers;
        // the chain against the clip-bounded one, which costs several times
        // as much a scene, is held over the scenes a test run plays.
        assert!(
            resolves(OSWALD),
            "{OSWALD} is drawn in Oswald, or the scenes asking for it test a fallback"
        );
        // The GPU playbacks are left out where there is no GPU, and CI names
        // the device it has, so that they are not left out there.
        assert!(
            gpu_available()
                || std::env::var_os("MEO_SKIA_CANVAS_REQUIRE_GPU").is_none(),
            "a GPU is required and there is none"
        );
        let (scenes, missing): (Vec<Scene>, Vec<Scene>) = sweep(full)
            .into_iter()
            .partition(|scene| resolves(scene.font));
        if full {
            eprintln!(
                "engines: cpu{}",
                if gpu_available() {
                    ", gpu"
                } else {
                    "; no gpu, its playbacks left out"
                }
            );
            eprintln!(
                "reach bound: {}",
                std::env::var("TEXT_REACH_BOUND")
                    .unwrap_or_else(|_| "stroked".into())
            );
            let mut absent: Vec<&str> =
                missing.iter().map(|s| s.font).collect();
            absent.dedup();
            eprintln!(
                "fonts not on this machine, their scenes left out: {absent:?}"
            );
        }
        let (count, done, start) =
            (scenes.len(), AtomicUsize::new(0), Instant::now());
        let found: Vec<(Vec<String>, Vec<String>, Vec<String>)> = scenes
            .into_par_iter()
            .filter_map(|scene| {
                let seen = done.fetch_add(1, Ordering::Relaxed) + 1;
                if full && seen % 200 == 0 {
                    eprintln!("{seen} of {count} scenes in {:.0?}", start.elapsed());
                }
                assert!(
                    resolves(scene.font),
                    "{} resolves on every thread a scene is drawn on",
                    scene.font
                );
                let reaches = reached_in_each_state(&scene);
                if reaches.is_empty() {
                    return None;
                }
                let label = format!(
                    "{} {}px {:?} {:?} stroke {:?} {:?} miter {} bold {} spacing {:?} {}",
                    scene.font,
                    scene.size,
                    scene.text,
                    scene.transform.map(|t| [t.a, t.b, t.c, t.d]),
                    scene.stroke,
                    scene.join,
                    scene.miter,
                    scene.bold,
                    scene.spacing,
                    scene.filter
                );
                let (mut past, mut unwidened) = (Vec::new(), Vec::new());
                for (state, reach) in reaches {
                    past.extend(inked_past(&scene, reach).into_iter().map(
                        |(at, by)| {
                            format!(
                                "{label} {state} {at}: inks {by} px past; {}",
                                ink_beside(&scene, reach, at)
                            )
                        },
                    ));
                    // What the margin is measured by: the ink past the reach
                    // with none.
                    if full {
                        unwidened.extend(inked_past_by(&scene, reach, 0.0).into_iter().map(
                            |(at, by)| {
                                format!(
                                    "{label} {state} {at}: inks {by} px past with no margin; {}",
                                    ink_beside(&scene, reach, at)
                                )
                            },
                        ));
                    }
                }
                let parted = if full { Vec::new() } else { parted_from_the_clip(&scene) }
                    .into_iter()
                    .filter(|(_, levels)| *levels > AAA_PLACEMENT_SPREAD)
                    .map(|(at, levels)| {
                        format!("{label} {at}: {levels} levels from the clip-bounded chain")
                    })
                    .collect();
                Some((past, parted, unwidened))
            })
            .collect();
        let played = found.len();
        let (mut past, mut parted, mut unwidened) =
            (Vec::new(), Vec::new(), Vec::new());
        for (a, b, c) in found {
            past.extend(a);
            parted.extend(b);
            unwidened.extend(c);
        }
        if full {
            eprintln!("{played} of {count} scenes answered a reach");
            eprintln!(
                "{} playbacks ink past the reach with no margin:",
                unwidened.len()
            );
            for line in &unwidened {
                eprintln!("  {line}");
            }
        }
        assert!(
            played > 0,
            "some scene answered a reach, or this asserts nothing"
        );
        for line in past.iter().chain(&parted).take(20) {
            eprintln!("{line}");
        }
        assert!(
            past.is_empty() && parted.is_empty(),
            "{} inked past, {} parted, of {played} scenes",
            past.len(),
            parted.len()
        );
    }
}

#[cfg(test)]
mod colour_recording_tests {
    //! A run holding colour glyphs is recorded for pixels only when first
    //! asked for, and the recording is the one made as it was drawn -- see
    //! `layer::ColourText::recorded`.

    use super::{
        COLOUR_RECORDED_AT_ONCE,
        layer::{COLOUR_WATCH, ColourText, Layer, drawn_by},
    };
    use crate::{
        export::{EncodeOptions, ImageFormat},
        font::colr::assert_colour_text,
        paint::BlendMode,
        pixels::{ImageDataExport, PixelExportOptions},
        prelude::*,
        started,
    };
    use skia_safe::Color;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// The family the colour test font is registered under.
    const FAMILY: &str = "LazyColr";

    /// The colour test font.
    const FONT: &str =
        "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";

    /// Colour glyphs of the test font, each with colour paint: a radial
    /// gradient, a sweep, a translucent fill and a composite.
    const TEXT: &str = "\u{f0200}\u{f0b06}\u{f0508}\u{f0600}";

    /// The page's size in its own units.
    const SIDE: (f32, f32) = (320.0, 160.0);

    /// The scenes, each a way `render_to_canvas` records a run otherwise.
    const SCENES: [&str; 7] = [
        "plain",
        "shadow",
        "shadow at half alpha",
        "underlined in red at half alpha",
        "copy",
        "condensed",
        "clipped",
    ];

    /// The ways a page is read, and how many recordings each asks for of
    /// the two runs a scene draws: none on its own pixels, nor as an SVG,
    /// which embeds a colour glyph as pixels; the pixels' of each run drawn
    /// into another canvas, which takes the page as a picture -- see
    /// `Context2D::picture` -- and the document's of each as a PDF.
    const READS: [(&str, usize); 4] = [
        ("pixels", 0),
        ("drawn into another canvas", 2),
        ("PDF", 2),
        ("SVG", 0),
    ];

    /// A CPU canvas of [`SIDE`].
    fn canvas() -> Canvas {
        Canvas::with_options(
            SIDE.0,
            SIDE.1,
            CanvasOptions {
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas")
    }

    /// `scene` drawn twice on a CPU canvas, each run recorded as it is
    /// drawn where `at_once`, and where the runs filed count the
    /// recordings made of them later.
    fn scene(scene: &str, at_once: bool) -> (Canvas, Arc<AtomicUsize>) {
        let library = FontLibrary::new();
        library
            .use_paths(Some(FAMILY), [FONT])
            .expect("the font registers");
        let asked = Arc::new(AtomicUsize::new(0));
        COLOUR_RECORDED_AT_ONCE.set(at_once);
        COLOUR_WATCH.with_borrow_mut(|watch| *watch = Some(Arc::clone(&asked)));
        let mut canvas = canvas();
        let ctx = canvas.context();
        ctx.set_fill_style_css("white").expect("a colour");
        ctx.fill_rect(0.0, 0.0, SIDE.0, SIDE.1);
        ctx.rotate(0.05);
        assert!(ctx.set_font_css(&format!("40px {FAMILY}")));
        ctx.set_fill_style_css("rgb(40,90,160)").expect("a colour");
        match scene {
            "shadow" | "shadow at half alpha" => {
                ctx.set_shadow_color_css("rgba(0,0,0,0.6)")
                    .expect("a colour");
                ctx.set_shadow_offset(6.0, 4.0);
                ctx.set_shadow_blur(3.0);
                if scene == "shadow at half alpha" {
                    ctx.set_global_alpha(0.5);
                }
            }
            "underlined in red at half alpha" => {
                ctx.set_text_decoration_css("underline rgb(200,0,0)")
                    .expect("a decoration");
                ctx.set_shadow_color_css("rgba(0,0,0,0.6)")
                    .expect("a colour");
                ctx.set_shadow_offset(6.0, 4.0);
                ctx.set_global_alpha(0.5);
            }
            "copy" => ctx.set_global_composite_operation(BlendMode::Copy),
            "clipped" => {
                ctx.begin_path();
                ctx.rect(30.5, 20.0, 200.0, 90.0);
                ctx.clip(FillRule::NonZero);
            }
            _ => {}
        }
        let width = (scene == "condensed").then_some(120.0);
        assert_colour_text(FONT, TEXT);
        for (x, y) in [(20.3, 60.6), (40.7, 130.2)] {
            ctx.fill_text(TEXT, x, y, width);
        }
        COLOUR_RECORDED_AT_ONCE.set(false);
        COLOUR_WATCH.with_borrow_mut(|watch| *watch = None);
        (canvas, asked)
    }

    /// `canvas` read as `read`.
    fn read(canvas: &mut Canvas, read: &str) -> Vec<u8> {
        let premultiplied = ImageDataExport {
            pixels: PixelExportOptions {
                premultiplied: true,
                ..PixelExportOptions::default()
            },
            ..ImageDataExport::default()
        };
        match read {
            "PDF" => canvas
                .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
                .expect("a PDF"),
            "SVG" => canvas
                .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
                .expect("an SVG"),
            "drawn into another canvas" => {
                let mut outer = self::canvas();
                outer.context().draw_canvas(canvas, 3.0, 2.0);
                outer
                    .to_image_data(None, &premultiplied)
                    .expect("pixels")
                    .pixels()
                    .to_vec()
            }
            _ => canvas
                .to_image_data(None, &premultiplied)
                .expect("pixels")
                .pixels()
                .to_vec(),
        }
    }

    /// The runs holding colour glyphs `canvas`'s page files.
    fn runs(canvas: &mut Canvas) -> Vec<Arc<ColourText>> {
        let page = canvas.context().inner.page();
        page.layers
            .iter()
            .filter_map(|layer| match layer {
                Layer::ColourText(text) => Some(Arc::clone(text)),
                Layer::Clipped(piece) => match piece.entry() {
                    Layer::ColourText(text) => Some(Arc::clone(text)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// Each of `runs`' recordings, for pixels and for a document, played
    /// onto a white raster of the page's size.
    fn played(runs: &[Arc<ColourText>]) -> Vec<Vec<u8>> {
        runs.iter()
            .flat_map(|run| [run.recorded(), run.recorded_for_document()])
            .map(|recorded| {
                let mut surface =
                    started::raster_n32_premul((SIDE.0 as i32, SIDE.1 as i32))
                        .expect("a raster");
                surface.canvas().clear(Color::WHITE);
                recorded.expect("a recording").playback(surface.canvas());
                let info = surface.image_info();
                let mut bytes = vec![0; info.compute_min_byte_size()];
                assert!(surface.read_pixels(
                    &info,
                    &mut bytes,
                    info.min_row_bytes(),
                    (0, 0),
                ));
                bytes
            })
            .collect()
    }

    #[test]
    fn a_colour_run_is_recorded_only_when_asked() {
        for scene in SCENES {
            for (read, asks) in READS {
                let (mut at_once, _) = self::scene(scene, true);
                let (mut later, asked) = self::scene(scene, false);
                let bytes = self::read(&mut later, read);
                assert!(
                    self::read(&mut at_once, read) == bytes,
                    "{scene}, {read}: the bytes differ"
                );
                assert_eq!(
                    asked.load(Ordering::Relaxed),
                    asks,
                    "{scene}, {read}: the recordings asked for"
                );
            }
        }
    }

    #[test]
    fn a_colour_run_recorded_when_asked_is_recorded_as_drawn() {
        for scene in SCENES {
            let (mut at_once, asked) = self::scene(scene, true);
            let at_once = runs(&mut at_once);
            assert_eq!(at_once.len(), 2, "{scene}: the runs filed");
            let at_once = played(&at_once);
            assert_eq!(asked.load(Ordering::Relaxed), 0, "{scene}: asked");
            let (mut later, asked) = self::scene(scene, false);
            let later = runs(&mut later);
            // What each run reaches, known before it is recorded, is what
            // its recording reaches.
            let reaches: Vec<_> = later.iter().map(|run| run.reach()).collect();
            assert_eq!(asked.load(Ordering::Relaxed), 0, "{scene}: asked");
            for (run, reach) in later.iter().zip(reaches) {
                let recorded = run.recorded().expect("a recording");
                assert_eq!(reach, recorded.cull_rect(), "{scene}: the reach");
                assert_eq!(Some(reach), drawn_by(recorded), "{scene}: drawn");
            }
            let later = played(&later);
            assert_eq!(
                asked.load(Ordering::Relaxed),
                4,
                "{scene}: the recordings made when asked"
            );
            for (run, (at_once, later)) in
                at_once.iter().zip(&later).enumerate()
            {
                assert!(
                    at_once.iter().any(|&byte| byte != u8::MAX),
                    "{scene}: recording {run} holds nothing"
                );
                let apart =
                    at_once.iter().zip(later).filter(|(a, b)| a != b).count();
                assert_eq!(apart, 0, "{scene}: recording {run}, bytes apart");
            }
        }
    }
}
