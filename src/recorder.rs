use skia_safe::{
    BlendMode as SkBlendMode, Canvas as SkCanvas, ColorSpace as SkColorSpace,
    ColorType, ImageInfo, Matrix, Paint as SkPaint, Point as SkPoint, RRect,
    Rect as SkRect,
    canvas::{SaveLayerRec, SrcRectConstraint},
};

use crate::{
    backend::resolve_engine,
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_unpremul_color4f,
    },
    context::page::{ExportOptions, PageRecorder},
    error::Error,
    filter::ImageFilter,
    geometry::{Affine, Point, Rect},
    image::Image,
    paint::Paint,
    path::Path,
    pixels::{RawFrame, RawFrameOptions, SamplingMode, SurfaceOptions},
    surface::Surface,
    text::{TextAlign, TextBoxOptions, TextLayout, VerticalAlign},
};

/// Records drawing commands once and replays them into surfaces later.
///
/// Nothing is rasterized while recording, so the same [`Recorder`] can be
/// rendered at several sizes or color spaces without re-issuing the draws.
pub struct Recorder {
    recorder: PageRecorder,
    bounds: Rect,
}

/// The drawing surface handed to [`Recorder::record`] and
/// [`Surface::with_canvas`](crate::surface::Surface::with_canvas).
///
/// Borrows its target, so it cannot outlive the call it was handed to.
pub struct Canvas<'a> {
    canvas: &'a SkCanvas,
    /// The destination surface's working color space. `RgbaLinear`
    /// values handed to canvas methods are interpreted in this space:
    /// drawing onto an `LinearColorSpace::Rec2020` surface treats
    /// `RgbaLinear::opaque(1.0, 0.0, 0.0)` as full red in linear
    /// Rec.2020 primaries, not linear sRGB.
    working_color_space: SkColorSpace,
}

/// Options for [`Canvas::save_layer_with`], mirroring CanvasKit's
/// `Canvas.saveLayer(paint?, bounds?, backdrop?, flags?)`.
#[derive(Default)]
pub struct SaveLayerOptions<'a> {
    /// Paint whose alpha, blend mode, and filters composite the layer
    /// onto the destination on `restore()`. `None` is a straight copy.
    pub paint: Option<&'a Paint>,
    /// Layer bounds hint. `None` uses the current clip bounds.
    pub bounds: Option<Rect>,
    /// Image filter applied to the existing backdrop before the layer
    /// draws over it (blur-behind / frosted glass). `None` = no backdrop
    /// filter.
    pub backdrop: Option<&'a ImageFilter>,
}

impl Recorder {
    /// Creates a recorder covering `bounds`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when `bounds` is empty or has a
    /// non-finite extent.
    pub fn new(bounds: Rect) -> Result<Self, Error> {
        if bounds.is_empty()
            || !bounds.width().is_finite()
            || !bounds.height().is_finite()
        {
            return Err(Error::InvalidDimensions {
                width: bounds.width(),
                height: bounds.height(),
            });
        }
        let sk_bounds = to_sk_rect(bounds);
        let recorder = PageRecorder::new(sk_bounds);
        Ok(Self { recorder, bounds })
    }

    /// Appends the draws issued by `f` to the recording.
    ///
    /// Can be called repeatedly; each call adds to what is already there.
    pub fn record(&mut self, f: impl FnOnce(&mut Canvas<'_>)) {
        // Recorder records into a picture whose working space is fixed
        // at render time; default the canvas to linear sRGB for color
        // tagging. Surface-driven callers (`Surface::with_canvas`)
        // carry the surface's working space through.
        let working_cs = linear_srgb_color_space();
        self.recorder.append(|skia_canvas| {
            let mut canvas = Canvas::new(skia_canvas, working_cs.clone());
            f(&mut canvas);
        });
    }

    /// Replays the recording into a fresh surface and reads it straight
    /// back as raw pixels.
    ///
    /// The frame is scaled by `surface_options.density`, so its dimensions
    /// are the recorder's bounds times that factor, floored -- not the
    /// bounds themselves. A density that is not finite and positive is
    /// silently treated as `1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedColorSpace`],
    /// [`Error::UnsupportedOutputColorSpace`] or
    /// [`Error::UnsupportedPixelFormat`] for a layout this build cannot
    /// produce, [`Error::InvalidDimensions`] if the scaled size is empty,
    /// [`Error::EngineUnavailable`] if the options pin a GPU that cannot be
    /// reached, and [`Error::Render`] if the render or the readback fails.
    pub fn render_raw(
        &mut self,
        surface_options: SurfaceOptions,
        frame_options: RawFrameOptions,
    ) -> Result<RawFrame, Error> {
        let surface_color_space =
            surface_options.color_space.to_skia_color_space()?;
        let dst_color_type = frame_options.pixel_format.to_skia_color_type()?;
        let dst_alpha_type = frame_options.pixel_format.to_skia_alpha_type();
        let dst_color_space =
            frame_options.color_space.to_skia_color_space()?;

        let density = if surface_options.density.is_finite()
            && surface_options.density > 0.0
        {
            surface_options.density
        } else {
            1.0
        };
        let scaled_w = (self.bounds.width() * density).floor().max(0.0) as i32;
        let scaled_h = (self.bounds.height() * density).floor().max(0.0) as i32;
        if scaled_w <= 0 || scaled_h <= 0 {
            return Err(Error::InvalidDimensions {
                width: self.bounds.width(),
                height: self.bounds.height(),
            });
        }

        let dst_info = ImageInfo::new(
            (scaled_w, scaled_h),
            dst_color_type,
            dst_alpha_type,
            dst_color_space,
        );

        let export_options = ExportOptions {
            density,
            color_type: ColorType::RGBAF16,
            color_space: surface_color_space,
            msaa: surface_options.msaa,
            ..ExportOptions::default()
        };

        let internal_engine = resolve_engine(surface_options.engine)?;
        let page = self.recorder.get_page();
        let pixels = page
            .render_raw(export_options, dst_info, internal_engine)
            .map_err(|reason| Error::Render { reason })?;

        let stride =
            (scaled_w as usize) * frame_options.pixel_format.bytes_per_pixel();
        Ok(RawFrame::new(
            scaled_w as u32,
            scaled_h as u32,
            stride,
            frame_options.pixel_format,
            frame_options.color_space,
            pixels,
        ))
    }

    /// Returns the bounds this recorder was created with.
    ///
    /// Not the size of the frame [`Recorder::render_raw`] produces: that is
    /// scaled by the density in its surface options.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
}

impl Canvas<'_> {
    pub(crate) fn new(
        canvas: &SkCanvas,
        working_color_space: SkColorSpace,
    ) -> Canvas<'_> {
        Canvas {
            canvas,
            working_color_space,
        }
    }

    /// Fills the entire clip with `color`, replacing what is there rather
    /// than blending over it.
    pub fn clear(&mut self, color: RgbaLinear) {
        // `Canvas::clear(Color4f)` builds an SkPaint internally with no
        // color space, so it would treat our linear value as
        // sRGB-encoded and gamma-decode it. Build the paint ourselves
        // with the destination's working color space tag and
        // `BlendMode::Src` (what `clear` does internally).
        let mut paint = SkPaint::default();
        paint.set_color4f(
            rgba_linear_to_unpremul_color4f(color),
            Some(&self.working_color_space),
        );
        paint.set_blend_mode(SkBlendMode::Src);
        self.canvas.draw_paint(&paint);
    }

    /// Pushes the current matrix and clip onto the state stack.
    pub fn save(&mut self) {
        self.canvas.save();
    }

    /// Pops the matrix and clip saved by the matching [`Canvas::save`].
    pub fn restore(&mut self) {
        self.canvas.restore();
    }

    /// Shifts the coordinate system by `point`.
    pub fn translate(&mut self, point: Point) {
        self.canvas.translate(SkPoint::new(point.x, point.y));
    }

    /// Rotates the coordinate system about `pivot`, or about the origin
    /// when `pivot` is `None`.
    pub fn rotate_degrees(&mut self, degrees: f32, pivot: Option<Point>) {
        let pivot = pivot.map(|p| SkPoint::new(p.x, p.y));
        self.canvas.rotate(degrees, pivot);
    }

    /// Scales the coordinate system about the origin.
    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.canvas.scale((sx, sy));
    }

    /// Concatenates an affine transform onto the current canvas matrix.
    /// `transform` is in `[a, b, c, d, tx, ty]` form (CSS DOMMatrix2DInit).
    pub fn concat_transform(&mut self, transform: Affine) {
        let matrix = Matrix::from_affine(&[
            transform.a,
            transform.b,
            transform.c,
            transform.d,
            transform.tx,
            transform.ty,
        ]);
        self.canvas.concat(&matrix);
    }

    /// Pushes an isolated drawing layer. Subsequent draws accumulate into the
    /// layer until `restore()`; on restore the layer is composited onto the
    /// destination using `paint`'s alpha, blend mode, and filters. Pass
    /// `None` for a transparent isolation buffer with default composition.
    pub fn save_layer(&mut self, paint: Option<&Paint>) {
        if let Some(p) = paint {
            let sk_paint = p.to_skia_paint(&self.working_color_space);
            let rec = SaveLayerRec::default().paint(&sk_paint);
            self.canvas.save_layer(&rec);
        } else {
            let rec = SaveLayerRec::default();
            self.canvas.save_layer(&rec);
        }
    }

    /// Pushes an isolated layer with full control over bounds and a
    /// backdrop filter, mirroring CanvasKit's
    /// `Canvas.saveLayer(paint?, bounds?, backdrop?)`. The `backdrop`
    /// image filter is applied to the *existing* destination content
    /// before the layer draws over it -- the only route to blur-behind /
    /// frosted-glass effects, which the temp-surface + `draw_canvas`
    /// emulation of grouped opacity cannot produce.
    pub fn save_layer_with(&mut self, options: SaveLayerOptions) {
        let sk_paint = options
            .paint
            .map(|p| p.to_skia_paint(&self.working_color_space));
        let sk_bounds = options.bounds.map(to_sk_rect);
        let mut rec = SaveLayerRec::default();
        if let Some(p) = sk_paint.as_ref() {
            rec = rec.paint(p);
        }
        if let Some(b) = sk_bounds.as_ref() {
            rec = rec.bounds(b);
        }
        if let Some(backdrop) = options.backdrop {
            rec = rec.backdrop(&backdrop.inner);
        }
        self.canvas.save_layer(&rec);
    }

    /// Intersects the current clip with `rect`. Subsequent draws outside the
    /// clip are discarded. Pair with `save()`/`restore()` to scope the clip.
    pub fn clip_rect(&mut self, rect: Rect) {
        self.canvas.clip_rect(to_sk_rect(rect), None, true);
    }

    /// Intersects the current clip with the rounded rect formed by `rect`
    /// and
    /// the given corner `radius`.
    pub fn clip_rrect(&mut self, rect: Rect, radius: f32) {
        let rrect = RRect::new_rect_xy(to_sk_rect(rect), radius, radius);
        self.canvas.clip_rrect(rrect, None, true);
    }

    /// Intersects the current clip with `path`. The path's fill rule decides
    /// which interior regions are kept.
    pub fn clip_path(&mut self, path: &Path) {
        self.canvas.clip_path(&path.inner, None, true);
    }

    /// Fills or strokes `path` according to `paint`. The path's fill rule
    /// (`NonZero` / `EvenOdd`) decides interior coverage on fills.
    pub fn draw_path(&mut self, path: &Path, paint: &Paint) {
        self.canvas.draw_path(
            &path.inner,
            &paint.to_skia_paint(&self.working_color_space),
        );
    }

    /// Strokes a line segment from `p1` to `p2` using the paint's stroke
    /// width, cap, dash, and anti-alias state. The paint should be a
    /// stroke-style paint; fill style produces no output.
    pub fn draw_line(&mut self, p1: Point, p2: Point, paint: &Paint) {
        self.canvas.draw_line(
            SkPoint::new(p1.x, p1.y),
            SkPoint::new(p2.x, p2.y),
            &paint.to_skia_paint(&self.working_color_space),
        );
    }

    /// Draws the `src` rect of `image` into the `dst` rect on this canvas
    /// using the given sampling mode. Optional `paint` controls alpha and
    /// blend mode of the composite. Pixels outside `src` are not sampled
    /// (strict source rect constraint).
    pub fn draw_image_src(
        &mut self,
        image: &Image,
        src: Rect,
        dst: Rect,
        paint: Option<&Paint>,
        sampling: SamplingMode,
    ) {
        let src_rect = to_sk_rect(src);
        let dst_rect = to_sk_rect(dst);
        let sk_paint =
            paint.map(|p| p.to_skia_paint(&self.working_color_space));
        let default_paint = SkPaint::default();
        let p_ref = sk_paint.as_ref().unwrap_or(&default_paint);
        self.canvas.draw_image_rect_with_sampling_options(
            &image.inner,
            Some((&src_rect, SrcRectConstraint::Strict)),
            dst_rect,
            sampling.to_skia(),
            p_ref,
        );
    }

    /// Composites `source`'s current contents onto this canvas at `(x, y)`.
    /// Optional `paint` controls alpha and blend mode of the composite. The
    /// source is snapshotted internally; the source is borrowed mutably
    /// because Skia requires mut access for snapshotting.
    pub fn draw_surface(
        &mut self,
        source: &mut Surface,
        x: f32,
        y: f32,
        paint: Option<&Paint>,
    ) {
        let image = source.snapshot();
        let sk_paint =
            paint.map(|p| p.to_skia_paint(&self.working_color_space));
        self.canvas.draw_image(
            &image.inner,
            SkPoint::new(x, y),
            sk_paint.as_ref(),
        );
    }

    /// Fills or strokes a rectangle according to `paint`.
    pub fn draw_rect(&mut self, rect: Rect, paint: &Paint) {
        self.canvas.draw_rect(
            to_sk_rect(rect),
            &paint.to_skia_paint(&self.working_color_space),
        );
    }

    /// Draws a rectangle with all four corners rounded by `radius`.
    pub fn draw_rounded_rect(
        &mut self,
        rect: Rect,
        radius: f32,
        paint: &Paint,
    ) {
        let rrect = RRect::new_rect_xy(to_sk_rect(rect), radius, radius);
        self.canvas
            .draw_rrect(rrect, &paint.to_skia_paint(&self.working_color_space));
    }

    /// Draws the ellipse inscribed in `rect`.
    pub fn draw_oval(&mut self, rect: Rect, paint: &Paint) {
        self.canvas.draw_oval(
            to_sk_rect(rect),
            &paint.to_skia_paint(&self.working_color_space),
        );
    }

    /// Draws `image` scaled to fill `dst`, at `opacity` clamped to
    /// `0.0..=1.0`.
    pub fn draw_image_rect(&mut self, image: &Image, dst: Rect, opacity: f32) {
        let dst_rect = to_sk_rect(dst);
        let mut paint = SkPaint::default();
        paint.set_anti_alias(true);
        paint.set_alpha_f(opacity.clamp(0.0, 1.0));
        self.canvas
            .draw_image_rect(&image.inner, None, dst_rect, &paint);
    }

    /// Paints a [`TextLayout`] produced by
    /// [`TextEngine`](crate::text::TextEngine) at
    /// `(x, y)` (the paragraph's top-left). Layout-time alignment from
    /// the `TextStyle` controls horizontal positioning within the
    /// paragraph's max width.
    pub fn draw_text_layout(&mut self, layout: &TextLayout, x: f32, y: f32) {
        layout.paragraph.paint(self.canvas, (x, y));
    }

    /// Lays out `text` inside `rect` and paints it in one call.
    ///
    /// Not equivalent to building a [`TextLayout`]: this path constructs a
    /// throwaway font collection from system fonts on every call, so
    /// families registered through
    /// [`FontManager`](crate::font::FontManager) are invisible to it and
    /// font fallback is off. Use it for system-font text that is not reused
    /// across frames.
    pub fn draw_text_box(
        &mut self,
        text: &str,
        rect: Rect,
        options: &TextBoxOptions,
    ) {
        use skia_safe::{
            FontMgr, FontStyle,
            font_style::{Slant, Weight, Width},
            textlayout::{
                FontCollection, ParagraphBuilder, ParagraphStyle,
                TextAlign as SkTextAlign, TextStyle,
            },
        };

        let mut paint = SkPaint::default();
        let modulated = options.color.with_opacity(options.opacity);
        paint.set_color4f(
            rgba_linear_to_unpremul_color4f(modulated),
            Some(&self.working_color_space),
        );
        paint.set_anti_alias(true);

        let font_mgr = FontMgr::new();
        let mut font_collection = FontCollection::new();
        font_collection.set_default_font_manager(font_mgr, None);

        let mut text_style = TextStyle::new();
        text_style.set_foreground_paint(&paint);
        text_style.set_font_size(options.font_size);
        if let Some(family) = &options.font_family {
            text_style.set_font_families(&[family.as_str()]);
        }
        text_style.set_font_style(FontStyle::new(
            Weight::from(options.font_weight),
            Width::NORMAL,
            Slant::Upright,
        ));

        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_align(match options.horizontal_align {
            TextAlign::Left => SkTextAlign::Left,
            TextAlign::Center => SkTextAlign::Center,
            TextAlign::Right => SkTextAlign::Right,
        });
        paragraph_style.set_text_style(&text_style);

        let mut builder =
            ParagraphBuilder::new(&paragraph_style, font_collection);
        builder.add_text(text);
        let mut paragraph = builder.build();
        paragraph.layout(rect.width());

        let y_offset = match options.vertical_align {
            VerticalAlign::Top => 0.0,
            VerticalAlign::Center => {
                (rect.height() - paragraph.height()).max(0.0) / 2.0
            }
            VerticalAlign::Bottom => {
                (rect.height() - paragraph.height()).max(0.0)
            }
        };

        paragraph.paint(self.canvas, (rect.left, rect.top + y_offset));
    }
}

fn to_sk_rect(rect: Rect) -> SkRect {
    SkRect::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}
