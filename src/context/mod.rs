#![allow(dead_code)]
use neon::prelude::*;
use skia_safe::{
    AlphaType, BlendMode, Canvas as SkCanvas, ClipOp, Color, Color4f,
    ColorFilter as SkColorFilter, ColorSpace, ColorType, Contains, Data, IRect,
    Image, ImageFilter as SkImageFilter, ImageInfo, MaskFilter as SkMaskFilter,
    Paint, PaintStyle, Path, PathBuilder, PathFillType, PathOp, Picture,
    PictureRecorder, Point, Rect, RoundOut, Shader as SkShader, Size, Typeface,
    canvas::SrcRectConstraint::Strict,
    color_filters, dash_path_effect,
    font_style::{FontStyle, Width},
    image_filter::MapDirection,
    image_filters,
    matrix::{Matrix, TypeMask},
    path_1d_path_effect,
    path_utils::fill_path_with_paint,
    shaders,
    textlayout::{StrutStyle, TextStyle},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub mod api;
pub(crate) mod blur_mask;
pub(crate) mod faint;
pub mod layer;
pub mod page;
pub mod typesetting;

#[cfg(test)]
use crate::node::typography::Painter;
use crate::{
    context2d::{FontKerning, TextRendering, font_spec_from_css},
    css::{FontUnits, ROOT_FONT_PX},
    export::VectorFeatures,
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
    typography::{
        Baseline, DecorationStyle, DrawnRun, FontSpec, PendingInk, Spacing,
        TextExtents, TypesetInputs, Typesetter, paragraph_style,
    },
    utils::*,
};
use faint::Faint;
use layer::{BlurDraw, ExactColour, Layer, LayerRec, reach_the_clip};
use page::{ExportOptions, Page, PageRecorder, Recording};
use typesetting::{TextSettings, Typesetting, Typography};

const BLACK: Color = Color::BLACK;
const TRANSPARENT: Color = Color::TRANSPARENT;

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
    /// Releases what this context told the engine its page holds.
    fn finalize<'a, C: Context<'a>>(mut self, cx: &mut C) {
        self.external.release(cx);
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
    /// What the surface `canvas_color_type` composites in does with a
    /// paint too faint for eight bits, fixed with it.
    faint: Faint,
    recorder: RefCell<PageRecorder>,
    /// The last exact colour a pixels recording painted with and its shader,
    /// so that a run of draws in one colour holds one image between them
    /// rather than one each -- see [`Self::exact_paint`].
    exact_shader: RefCell<Option<(ExactColour, SkShader)>>,
    /// The same for text: the last exact colour, the plain colour beside
    /// it and their dye -- see [`Self::exact_text_paint`].
    exact_dye: RefCell<Option<(ExactColour, Color4f, SkColorFilter)>>,
    pub state: State,
    stack: Vec<State>,
    /// Parallel to `stack`: whether each saved frame opened a Skia layer
    /// (via `save_layer`). On `pop`, a layer-owning frame triggers a
    /// matching `canvas.restore()` to composite the layer.
    layers: Vec<bool>,
    pub path: PathBuilder,
    /// What the JavaScript engine has been told this context's page holds.
    /// See [`Context2D::report_resident`].
    external: External,
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
    pub skia_color_filter: Option<SkColorFilter>,
    pub skia_image_filter: Option<SkImageFilter>,
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
        dims: impl Into<Size>,
    ) -> Self {
        let bounds = Rect::from_size(dims);
        let dual = keeps_two_recordings(&canvas_color_space, canvas_color_type);

        Context2D {
            bounds,
            canvas_color_space,
            canvas_color_type,
            faint: Faint::of(canvas_color_type),
            recorder: RefCell::new(PageRecorder::recording(bounds, dual)),
            exact_shader: RefCell::default(),
            exact_dye: RefCell::default(),
            path: PathBuilder::new(),
            stack: vec![],
            layers: vec![],
            state: State::default(),
            external: External::default(),
        }
    }

    /// Tells the JavaScript engine what this context's page holds in pixels
    /// now, so that a collection is scheduled by the native memory a
    /// canvas holds and not only by its small JavaScript object.
    ///
    /// Called by the binding after a read, which builds the page surface or
    /// its tiles, and after a resize, which drops them, and released in the
    /// finalizer. A drawing that clears the whole page drops them too and is
    /// counted at the next of those, so the engine is told too much in
    /// between and never too little.
    pub(crate) fn report_resident<'a, C: Context<'a>>(&mut self, cx: &mut C) {
        let bytes = self.recorder.borrow().resident_bytes();
        self.external.hold(cx, bytes);
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
    ///
    /// [`PageRecorder::append_isolated`]: page::PageRecorder::append_isolated
    fn append_draw<F>(&self, features: VectorFeatures, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        match features.any() {
            false => self.with_canvas(f),
            true => self.with_recorder(|mut recorder| {
                recorder.append_isolated(features, f)
            }),
        }
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

        if self.state.global_composite_operation != BlendMode::SrcOver {
            features = features.with(VectorFeatures::BLEND_MODE);
        }
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

    /// Records the draw `f` makes with `paint`, its shadow beneath it, into
    /// every recording the page keeps. Where the page keeps one for pixels,
    /// `exact` is the paint that recording draws with instead -- see
    /// [`Self::exact_paint`].
    /// The paint a page's pixels recording draws `paint` with, where the
    /// page keeps one: the flat colour `style` dyes with, held exactly in the
    /// page's own space by [`ExactColour::shader`] where Skia's conversion of a
    /// paint colour would land it off -- see `page::Recorders`. Its alpha
    /// stays on the paint, which scales the shader's opaque colour.
    ///
    /// `None` on a page that keeps one recording, for a dye that is not a
    /// flat colour, and for a colour this crate cannot convert into the
    /// page's space exactly.
    pub fn exact_paint(
        &self,
        paint: &Paint,
        style: PaintStyle,
    ) -> Option<Paint> {
        if paint.shader().is_some() {
            return None;
        }
        let exact = self.exact_colour(style)?;
        let mut held = self.exact_shader.borrow_mut();
        let shader = match &*held {
            Some((colour, shader)) if *colour == exact => shader.clone(),
            _ => {
                let shader = exact.shader()?;
                *held = Some((exact, shader.clone()));
                shader
            }
        };
        let mut paint = paint.clone();
        paint.set_shader(shader);
        Some(paint)
    }

    /// The paint a page's pixels recording draws text in `paint` with, where
    /// the page keeps one: `paint` over a colour shader of its own colour,
    /// opaque, with [`ExactColour::dye`] replacing that colour before any
    /// colour filter `paint` already carries. Without a shader Skia folds a
    /// colour filter into the paint's colour, through the conversion the
    /// dye exists to avoid; and a colour shader answers the luminance Skia
    /// sets a glyph mask's contrast from, which an image shader does not.
    pub fn exact_text_paint(
        &self,
        paint: &Paint,
        style: PaintStyle,
    ) -> Option<Paint> {
        if paint.shader().is_some() {
            return None;
        }
        let exact = self.exact_colour(style)?;
        let mut opaque = paint.color4f();
        opaque.a = 1.0;
        let mut held = self.exact_dye.borrow_mut();
        let dye = match &*held {
            Some((colour, plain, dye))
                if *colour == exact && *plain == opaque =>
            {
                dye.clone()
            }
            _ => {
                let dye = exact.dye(opaque)?;
                *held = Some((exact, opaque, dye.clone()));
                dye
            }
        };
        let dye = match paint.color_filter() {
            Some(theirs) => color_filters::compose(&theirs, &dye)?,
            None => dye,
        };
        let mut paint = paint.clone();
        paint.set_shader(shaders::color_in_space(
            opaque,
            ColorSpace::new_srgb(),
        ));
        paint.set_color_filter(dye);
        Some(paint)
    }

    /// The flat colour `style` dyes with, held exactly in the page's own
    /// space, on a page that keeps a recording for pixels -- see
    /// [`Self::exact_paint`].
    pub fn exact_colour(&self, style: PaintStyle) -> Option<ExactColour> {
        if !self.recorder.borrow().dual() {
            return None;
        }
        let Dye::Color(color, Some(space), _) = self.state.dye(style) else {
            return None;
        };
        let canvas = PixelColorSpace::matching(&self.canvas_color_space)?;
        let rgb = color_in_canvas_space(*color, space, canvas)?;
        Some(ExactColour {
            rgb: rgb.map(|c| c as f32),
            space: self.canvas_color_space.clone(),
        })
    }

    pub fn render_to_canvas<F>(
        &self,
        paint: &Paint,
        exact: Option<&Paint>,
        features: VectorFeatures,
        f: F,
    ) where
        F: Fn(&SkCanvas, &Paint),
    {
        let paint_for = |recording: Recording| match (recording, exact) {
            (Recording::Pixels, Some(exact)) => exact,
            _ => paint,
        };
        let render_shadow = |canvas: &SkCanvas, paint: &Paint| {
            if let Some(shadow_paint) = self.paint_for_shadow(paint) {
                canvas.save();
                canvas.set_matrix(
                    &Matrix::translate(self.state.shadow_offset).into(),
                );
                canvas.concat(&self.state.matrix);
                f(canvas, &shadow_paint);
                canvas.restore();
            }
        };

        match covers_beyond_the_draw(self.state.global_composite_operation) {
            true => {
                // for blend modes that affect regions of the canvas outside of
                // the bounds of the object being drawn, create
                // an intermediate picture before drawing to the canvas, one
                // for each recording, each with that recording's paint
                self.append_draw(features, |canvas, recording| {
                    let mut layer_paint = paint_for(recording).clone();
                    layer_paint.set_blend_mode(BlendMode::SrcOver);
                    let mut layer_recorder = PictureRecorder::new();
                    layer_recorder.begin_recording(self.bounds, true);
                    if let Some(layer) = layer_recorder.recording_canvas() {
                        // draw the dropshadow (if applicable)
                        render_shadow(layer, &layer_paint);
                        // draw normally
                        layer.set_matrix(&self.state.matrix.into());
                        f(layer, &layer_paint);
                    }

                    // transfer the picture contents to the canvas in a
                    // single operation, applying the blend mode to the
                    // whole canvas (regardless of the bounds of the
                    // text/path being drawn)
                    if let Some(pict) =
                        layer_recorder.finish_recording_as_picture(None)
                    {
                        canvas.save();
                        canvas.set_matrix(&Matrix::new_identity().into());
                        let mut blend_paint = Paint::default();
                        blend_paint.set_anti_alias(true);
                        blend_paint.set_blend_mode(
                            self.state.global_composite_operation,
                        );
                        canvas.draw_picture(&pict, None, Some(&blend_paint));
                        canvas.restore();
                    }
                });
            }
            false => {
                self.append_draw(features, |canvas, recording| {
                    // draw the dropshadow (if applicable)
                    render_shadow(canvas, paint);
                    // draw with the normal paint
                    f(canvas, paint_for(recording));
                });
            }
        };
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
    pub fn with_composited_canvas<F>(&self, f: F)
    where
        F: Fn(&SkCanvas),
    {
        let alpha = self.state.global_alpha as f32;
        let blend = self.state.global_composite_operation;

        if alpha == 1.0 && blend == BlendMode::SrcOver {
            self.with_canvas(|canvas, _| f(canvas));
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
        let record = || {
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.state.matrix.into());
            f(canvas);
            recorder.finish_recording_as_picture(None)
        };
        // Once for each recording the page keeps, as every draw is.
        let entry = match self.recorder.borrow().dual() {
            false => record().map(Layer::Picture),
            true => record()
                .zip(record())
                .map(|pictures| Layer::Targeted(Arc::new(pictures))),
        };
        let contents = Arc::new(entry.into_iter().collect());
        self.with_recorder(|mut recorder| {
            recorder.append_group(
                LayerRec {
                    paint: Some(paint),
                    ..LayerRec::default()
                },
                None,
                Matrix::new_identity(),
                contents,
                VectorFeatures::PLAIN,
                Point::default(),
            );
        });
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
        backdrop: Option<SkImageFilter>,
    ) {
        let paint =
            paint.map(|paint| self.faint().drawable(&paint).into_owned());
        // open_layer, not with_canvas: the recorder draws the layer around
        // its contents once it closes -- see `PageRecorder::open_layer`.
        self.with_recorder(|mut recorder| {
            recorder.open_layer(LayerRec {
                paint,
                bounds,
                backdrop,
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
            && path.conservatively_contains_rect(self.bounds)
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
        self.render_to_canvas(
            &paint,
            exact.as_ref(),
            features,
            |canvas, paint| {
                if let Some(tile) = self.state.texture(style) {
                    // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
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
                    fill_path_with_paint(
                        &path,
                        paint,
                        &mut stencil_builder,
                        None,
                        None,
                    );
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
                    );

                    let enclosing_frame = Path::rect(expanded_bounds, None);

                    if tile.use_clip() {
                        // apply the user path as a clipping mask and fill the
                        // whole enclosing rect with
                        // tile pattern
                        canvas.save();
                        canvas.clip_path(
                            &stencil,
                            Some(ClipOp::Intersect),
                            Some(true),
                        );
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
                            fill_paint
                                .set_alpha_f(fill_paint.alpha_f() * thinned);
                        }
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
                    canvas.draw_path(&path, &self.faint().drawable(paint));
                }
            },
        );
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
            .and_then(|path| {
                match path.conservatively_contains_rect(self.bounds) {
                    true => None,
                    false => Some(path),
                }
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
            false => {
                self.append_draw(VectorFeatures::BLEND_MODE, |canvas, _| {
                    let mut paint = Paint::default();
                    paint
                        .set_anti_alias(true)
                        .set_style(PaintStyle::Fill)
                        .set_blend_mode(BlendMode::Clear);
                    canvas.draw_rect(rect, &paint);
                })
            }
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
    /// canvas's, so that a draw through its paint holds the page as it was
    /// recorded and each target composites its groups as it needs -- see
    /// `LayerRec::open_on`. `None` for a picture with no page behind it.
    pub fn draw_picture_costing(
        &mut self,
        picture: &Picture,
        entries: Option<Arc<Vec<Layer>>>,
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
        // `destination-out` inside it is still a blend mode. And where it is
        // drawn through its paint, it is drawn through a layer -- see
        // `carries` below -- which a backend may not have.
        let carried = match carries(source, &paint) {
            true => VectorFeatures::PICTURE_PAINT,
            false => VectorFeatures::PLAIN,
        };
        let features = self
            .vector_features(&paint, None)
            .with(source)
            .with(carried);

        // Drawn through its paint as a group of our own, which composites
        // the alpha exactly on pixels and as an opacity in a document -- see
        // `LayerRec::open_on` -- wherever the blend composites only where
        // the source lands, which `render_to_canvas` draws as it is: its
        // shadow first, as a group of its own offset from it, then the
        // source.
        let drawn = self.faint().drawable(&paint).into_owned();
        if carries(source, &drawn)
            && !covers_beyond_the_draw(self.state.global_composite_operation)
        {
            let contents_matrix = Matrix::concat(&self.state.matrix, &matrix);
            let contents = entries.unwrap_or_else(|| {
                Arc::new(vec![Layer::Picture(picture.clone())])
            });
            let shadow = self
                .paint_for_shadow(&paint)
                .map(|shadow| self.faint().drawable(&shadow).into_owned());
            let offset = self.state.shadow_offset;
            self.with_recorder(|mut recorder| {
                let mut group = |paint: Paint, shift: Point| {
                    recorder.append_group(
                        LayerRec {
                            paint: Some(paint),
                            ..LayerRec::default()
                        },
                        Some(*dst_rect),
                        contents_matrix,
                        Arc::clone(&contents),
                        features,
                        shift,
                    );
                };
                if let Some(shadow) = shadow {
                    group(shadow, offset);
                }
                group(drawn, Point::default());
            });
            return;
        }

        self.render_to_canvas(&paint, None, features, |canvas, paint| {
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
            // A faint paint drawn as its opaque stand-in is carried as well:
            // its alpha is in a colour filter -- see `Faint::drawable`.
            let paint = self.faint().drawable(paint);
            let paint = carries(source, &paint).then_some(&*paint);
            canvas.save();
            canvas.clip_rect(dst_rect, ClipOp::Intersect, true);
            canvas.draw_picture(picture, Some(&matrix), paint);
            canvas.restore();
        });
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
        self.render_to_canvas(&paint, None, features, |canvas, paint| {
            let sampling = self.state.sampling_filter.sampling_for(scaling);
            canvas.draw_image_rect_with_sampling_options(
                image,
                Some((src_rect, Strict)),
                dst_rect,
                sampling,
                &self.faint().drawable(paint),
            );
        });
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
    pub fn entries(&self) -> Arc<Vec<Layer>> {
        self.recorder.borrow_mut().page().layers
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
            self.with_canvas(|canvas, _| {
                let paint = Paint::default();
                let mut eraser = Paint::default();
                canvas.restore_to_count(1); // discard current matrix & clip
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
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
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
            let features = self.vector_features(&paint, Some(style));
            let kept = typesetter.laid_out();
            let global_alpha = self.state.global_alpha as f32;
            let faint = self.faint();
            // On a page that keeps a recording for pixels, text there takes
            // its colour exactly through a dye -- see `exact_text_paint` --
            // rather than the image shader a fill takes: Skia sets a glyph
            // mask's contrast from the luminance of the paint's colour,
            // which an image shader does not report, so text through one is
            // read as mid-grey and drawn with heavier edges -- 360 pixels of
            // a bold 40px run on a Display P3 F32 page.
            let dyed = self.exact_text_paint(&paint, style);
            let dyed = dyed.as_ref();
            self.render_to_canvas(&paint, dyed, features, |canvas, paint| {
                // The kept run, or, the first time a run is drawn, the run
                // laid out for this paint -- see `Typesetter::laid_out`.
                let run = match &kept {
                    Some(kept) => DrawnRun::Kept(kept.clone()),
                    None => typesetter.drawn_once(&faint.drawable(paint)),
                };
                let (offset, condensation) = (run.offset(), run.condensation());
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
                    run.paint(canvas, at, paint, global_alpha, faint);
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
            });
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
            paint.set_color_filter(cf.clone());
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
        let has_shader = !matches!(self.state.dye(style), Dye::Color(..));
        let rendered =
            has_shader || !self.state.filter.blurs_coverage(coverage);
        self.state
            .filter
            .mix_into(&mut paint, self.state.matrix, rendered);

        // 3. Compose Skia imageFilter with CSS imageFilter (if both present)
        if let Some(skia_imgf) = &self.state.skia_image_filter {
            let final_image_filter = match paint.image_filter() {
                Some(css_imgf) => {
                    image_filters::compose(skia_imgf.clone(), css_imgf)
                }
                None => Some(skia_imgf.clone()),
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
            paint.set_color_filter(cf.clone());
        }

        // 2. Apply CSS filter (image_filter + mask_filter)
        self.state
            .filter
            .mix_into(&mut paint, self.state.matrix, true)
            .set_alpha_f(self.state.global_alpha as f32);

        // 3. Compose Skia imageFilter with CSS imageFilter (if both present)
        if let Some(skia_imgf) = &self.state.skia_image_filter {
            let final_image_filter = match paint.image_filter() {
                Some(css_imgf) => {
                    image_filters::compose(skia_imgf.clone(), css_imgf)
                }
                None => Some(skia_imgf.clone()),
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
        );
        paint.set_style(style);
        paint
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
        let (mut shadow_color, shadow_space) = skia_paint_color_onto(
            *shadow_color,
            shadow_space,
            &self.canvas_color_space,
        );
        // On float pixels the paint's own alpha moves into the shadow's
        // colour, which the filter applies to the shape in float: the shape
        // is drawn into the filter's layer, and on Skia's raster backend a
        // filtered layer on an F16 or F32 page comes back in steps of one
        // 255th, so a `globalAlpha` of 0.5 cast a shadow of 128/255 and one
        // of 0.001 none at all. The two are one product whatever order they
        // are taken in -- the shadow is the shape's alpha times its colour
        // -- except through a colour filter, which sees the alpha before
        // the shadow does and is left to have it there.
        if self.faint() == Faint::Kept && paint.color_filter().is_none() {
            shadow_color.a *= paint.alpha_f();
            paint.set_alpha_f(1.0);
        }
        paint.set_image_filter(image_filters::drop_shadow_only(
            (0.0, 0.0),
            (sigma.x, sigma.y),
            shadow_color,
            shadow_space,
            None,
            None,
        ));
        Some(paint)
    }
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
    /// are not.
    Shader(SkShader, VectorFeatures),
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
    /// gradient, and an image shader. A sweep gradient or a procedural
    /// shader leaves it with nothing to write.
    pub fn vector_features(&self) -> VectorFeatures {
        match self {
            Dye::Color(..) | Dye::Pattern(_) | Dye::Texture(_) => {
                VectorFeatures::PLAIN
            }
            Dye::Gradient(gradient) => gradient.vector_features(),
            Dye::Shader(_, features) => *features,
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

    /// Puts the dye on `paint` at `alpha`, for a page in `destination`.
    pub fn mix_into(
        &self,
        paint: &mut Paint,
        alpha: f32,
        sampling_filter: SamplingFilter,
        destination: &ColorSpace,
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
                let (color, cs) = texture.to_color4f(alpha);
                paint.set_color4f(color, cs.as_ref());
            }
            Dye::Shader(shader, _) => {
                paint.set_shader(Some(shader.clone())).set_alpha_f(alpha);
            }
        };
    }
}

/// Whether a canvas's picture, carrying `source`'s features, is drawn with
/// `paint` rather than without one: where the source composites in a mode of
/// its own, or where the paint says something -- an alpha under one, a blend
/// mode past source-over, an image filter, a colour filter. See
/// `Context2D::draw_picture_costing`.
/// Whether a canvas in `space` and `color_type` keeps a recording for each
/// kind of target -- see `page::Recorders`: one whose pixels hold a float
/// and whose space is one a flat colour can be converted into exactly here,
/// other than sRGB, onto which Skia lands a paint colour exactly itself.
fn keeps_two_recordings(space: &ColorSpace, color_type: ColorType) -> bool {
    Faint::of(color_type) == Faint::Kept
        && matches!(
            PixelColorSpace::matching(space),
            Some(
                PixelColorSpace::SrgbLinear
                    | PixelColorSpace::DisplayP3
                    | PixelColorSpace::DisplayP3Linear
                    | PixelColorSpace::Rec2020
                    | PixelColorSpace::Rec2020Linear
            )
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
mod tests {
    use super::*;
    use crate::node::filter::FilterSpec;

    /// Builds a context the size of a small canvas, as `Canvas::new` does.
    fn context() -> Context2D {
        Context2D::new(ColorSpace::new_srgb(), ColorType::N32, (200.0, 100.0))
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
}
