#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{FilterMode, Matrix, Rect, Shader, Size, TileMode};
use std::{cell::RefCell, rc::Rc, sync::Arc};

use crate::{
    context::{
        BoxedContext2D, Context2D, layer::TileSource, page::ExportOptions,
    },
    node::{
        filter::{SamplingFilter, ScalingOperation},
        image::{BoxedImage, Content},
    },
    utils::*,
};

pub type BoxedCanvasPattern = JsBox<RefCell<CanvasPattern>>;
impl Finalize for CanvasPattern {}

pub struct Stamp {
    content: Content,
    dims: Size,
    repeat: (TileMode, TileMode),
    matrix: Matrix,
    /// The page a canvas pattern was made from, which a draw dyed by the
    /// pattern rasterizes where it is played -- see [`TileSource`]. `None`
    /// for an image's pattern. `content` holds the page as a document
    /// writes it.
    source: Option<Arc<TileSource>>,
}

#[derive(Clone)]
pub struct CanvasPattern {
    pub stamp: Rc<RefCell<Stamp>>,
}

impl CanvasPattern {
    /// Builds a pattern from an already-resolved tile.
    ///
    /// The Neon entry points below and the Rust `Pattern` wrapper both go
    /// through here, so there is one place that knows how a `Stamp` is put
    /// together.
    pub fn from_parts(
        content: Content,
        dims: Size,
        repeat: (TileMode, TileMode),
        matrix: Matrix,
    ) -> Self {
        Self {
            stamp: Rc::new(RefCell::new(Stamp {
                content,
                dims,
                repeat,
                matrix,
                source: None,
            })),
        }
    }

    /// A pattern of the page `ctx` holds now, repeated as `repeat` says.
    ///
    /// The page is kept, for a draw dyed by the pattern to rasterize it
    /// where the draw is played, as the page composites itself -- see
    /// [`TileSource`]. For a document it is kept as a picture too, so that
    /// a vector backend can see through it -- but a page that already
    /// carries someone else's picture pays for its pixels here instead,
    /// because a picture reached by two paths is replayed twice while being
    /// recorded once. A page turned into a pattern, painted onto another
    /// page, and that page turned into a pattern again doubles the eventual
    /// rasterization each round -- 47, 122, 378, 1409 milliseconds at eight,
    /// twelve, fourteen and sixteen rounds. One that has only been drawn on
    /// does not pay, which is what keeps an ordinary pattern cheap and
    /// leaves it vector. The same rule `node::image::Source::of` follows.
    pub fn of_page(ctx: &Context2D, repeat: (TileMode, TileMode)) -> Self {
        let dims = ctx.bounds.size();
        let content = match ctx.replay_cost() > 0 {
            true => ctx
                .source_image(true)
                .map(Content::Bitmap)
                .unwrap_or_default(),
            false => ctx
                .picture()
                .map(|picture| Content::Vector(picture, dims))
                .unwrap_or_default(),
        };
        let (text_contrast, text_gamma) = ctx.canvas_text;
        let options = ExportOptions {
            color_space: ctx.canvas_color_space.clone(),
            color_type: ctx.canvas_color_type,
            surface_color_space: ctx.canvas_color_space.clone(),
            surface_color_type: ctx.canvas_color_type,
            text_contrast,
            text_gamma,
            ..ExportOptions::default()
        };
        let pattern =
            Self::from_parts(content, dims, repeat, Matrix::new_identity());
        pattern.stamp.borrow_mut().source =
            Some(Arc::new(TileSource::new(ctx.page(), options)));
        pattern
    }

    /// What a draw dyed by this pattern rasterizes it from where it is
    /// played -- the canvas's page, the pattern's own transform, and how it
    /// repeats -- for a canvas's pattern; `None` for an image's, which its
    /// [`shader`](Self::shader) paints.
    pub fn tiling(
        &self,
    ) -> Option<(Arc<TileSource>, Matrix, (TileMode, TileMode))> {
        let stamp = self.stamp.borrow();
        Some((
            Arc::clone(stamp.source.as_ref()?),
            stamp.matrix,
            stamp.repeat,
        ))
    }

    /// Replaces the tile's local transform.
    pub fn set_matrix(&self, matrix: Matrix) {
        self.stamp.borrow_mut().matrix = matrix;
    }

    /// The tile's local transform.
    pub fn matrix(&self) -> Matrix {
        self.stamp.borrow().matrix
    }

    /// The tile's size in pixels.
    pub fn dims(&self) -> Size {
        self.stamp.borrow().dims
    }

    pub fn shader(&self, sampling_filter: SamplingFilter) -> Option<Shader> {
        let stamp = self.stamp.borrow();

        match &stamp.content {
            // Unknown, not Default: a pattern's scale is not known here -- it
            // arrives with the CTM when the shader is painted. Chrome takes the
            // mipmapped branch in that case rather than a cubic one.
            Content::Bitmap(image) => image
                .to_shader(
                    stamp.repeat,
                    sampling_filter.sampling_for(ScalingOperation::Unknown),
                    None,
                )
                .map(|shader| shader.with_local_matrix(&stamp.matrix)),
            Content::Vector(pict, ..) => {
                let tile_rect = Rect::from_size(stamp.dims);
                let shader = pict.to_shader(
                    stamp.repeat,
                    FilterMode::Linear,
                    None,
                    Some(&tile_rect),
                );
                Some(shader.with_local_matrix(&stamp.matrix))
            }
            _ => None,
        }
    }

    pub fn is_opaque(&self) -> bool {
        let stamp = self.stamp.borrow();

        match &stamp.content {
            Content::Bitmap(image) => image.is_opaque(),
            _ => false,
        }
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn from_image(mut cx: FunctionContext) -> JsResult<BoxedCanvasPattern> {
    let src = cx.argument::<BoxedImage>(1)?;
    let canvas_width = float_arg_or_bail(&mut cx, 2, "width")?;
    let canvas_height = float_arg_or_bail(&mut cx, 3, "height")?;
    let repeat = repetition_arg(&mut cx, 4)?;

    let src = src.borrow();
    let content = src.content.clone();
    let dims = src.content.size();
    let mut matrix = Matrix::new_identity();

    if src.autosized && !dims.is_empty() {
        // If this flag is set (for SVG images with no intrinsic size) then we
        // need to scale the image to the canvas' smallest dimension.
        // This preserves compatibility with how Chromium browsers behave.
        let min_size = f32::min(canvas_width, canvas_height);
        let factor = (min_size / dims.width, min_size / dims.height);
        matrix.set_scale(factor, None);
    }

    let canvas_pattern =
        CanvasPattern::from_parts(content, dims, repeat, matrix);
    Ok(cx.boxed(RefCell::new(canvas_pattern)))
}

pub fn from_image_data(
    mut cx: FunctionContext,
) -> JsResult<BoxedCanvasPattern> {
    let src = image_data_arg(&mut cx, 1)?;
    let repeat = repetition_arg(&mut cx, 2)?;
    let content = Content::from_image_data(src);
    let dims: Size = content.size();
    let matrix = Matrix::new_identity();

    let canvas_pattern =
        CanvasPattern::from_parts(content, dims, repeat, matrix);
    Ok(cx.boxed(RefCell::new(canvas_pattern)))
}

pub fn from_canvas(mut cx: FunctionContext) -> JsResult<BoxedCanvasPattern> {
    let src = cx.argument::<BoxedContext2D>(1)?;
    let repeat = repetition_arg(&mut cx, 2)?;

    let canvas_pattern = CanvasPattern::of_page(&src.borrow(), repeat);
    Ok(cx.boxed(RefCell::new(canvas_pattern)))
}

pub fn setTransform(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasPattern>(0)?;
    let matrix = matrix_arg(&mut cx, 1)?;
    let this = this.borrow();

    this.set_matrix(matrix);
    Ok(cx.undefined())
}

pub fn repr(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasPattern>(0)?;
    let this = this.borrow();

    let stamp = this.stamp.borrow();
    let style = match stamp.content {
        Content::Bitmap(..) => "Bitmap",
        _ => "Canvas",
    };

    Ok(cx.string(format!(
        "{} {}×{}",
        style, stamp.dims.width, stamp.dims.height
    )))
}
