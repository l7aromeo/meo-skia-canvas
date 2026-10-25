#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    Color4f, ColorSpace, Matrix, Paint, PaintCap, PaintStyle, Path, Point,
    line_2d_path_effect, path_2d_path_effect,
};
use std::{cell::RefCell, rc::Rc};

use crate::utils::*;

struct Texture {
    path: Option<Path>,
    /// Unpremultiplied, in the space it is tagged with: the one a CSS string
    /// named, or linear-light sRGB for an `RgbaLinear`.
    color: (Color4f, ColorSpace),
    line: f32,
    cap: PaintCap,
    angle: f32,
    scale: (f32, f32),
    shift: (f32, f32),
}

pub type BoxedCanvasTexture = JsBox<RefCell<CanvasTexture>>;
impl Finalize for CanvasTexture {}

impl Default for Texture {
    fn default() -> Self {
        Texture {
            path: None,
            color: (Color4f::new(0.0, 0.0, 0.0, 1.0), ColorSpace::new_srgb()),
            line: 1.0,
            cap: PaintCap::Butt,
            angle: 0.0,
            scale: (1.0, 1.0),
            shift: (0.0, 0.0),
        }
    }
}

#[derive(Clone)]
pub struct CanvasTexture {
    texture: Rc<RefCell<Texture>>,
    outline: bool,
}

impl CanvasTexture {
    /// Builds a texture from resolved tile settings.
    ///
    /// The Neon constructor and the Rust `Texture` wrapper both go through
    /// here, so there is one place that knows how a `Texture` tile is put
    /// together.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        path: Option<Path>,
        color: (Color4f, ColorSpace),
        line: f32,
        cap: PaintCap,
        angle: f32,
        outline: bool,
        scale: (f32, f32),
        shift: (f32, f32),
    ) -> Self {
        Self {
            texture: Rc::new(RefCell::new(Texture {
                path,
                color,
                line,
                cap,
                angle,
                scale,
                shift,
            })),
            outline,
        }
    }

    /// Installs the tile's path effect, magnified by `magnify`.
    ///
    /// `magnify` is `1.0` for every grid the draw site can lay out as asked,
    /// which is every grid at a resolvable size. Above that it is a
    /// similarity transform of the whole pattern -- grid period, tile and
    /// stroke width scale by the same factor -- which leaves the mean
    /// coverage untouched and only enlarges the structure carrying it.
    /// `Context2D::texture_lattice` picks the factor, and documents both why
    /// one is needed and where the equality stops being exact.
    ///
    /// `device_pixel` is the width of one device pixel in the same units, and
    /// is what a line mark is held to. `line_2d_path_effect` loses a thinner
    /// mark rather than antialiasing it: at a period of 8, a mark of 0.5
    /// painted at a third of the coverage it should, 0.25 at a quarter, and
    /// 0.125 vanished. Nothing else does this -- an ordinary stroke holds its
    /// coverage to within 6% at 0.125, and `path_2d_path_effect` is exact --
    /// so the mark is widened to a pixel and its alpha cut by the same ratio,
    /// which is the usual way to draw a hairline and puts the coverage back
    /// where it belongs.
    /// Returns the factor the mark's alpha was cut by, which is `1.0` unless
    /// the widening above took effect. The outline branch of the draw takes
    /// its geometry from this paint but its colour from another, so it has to
    /// apply the same cut itself.
    #[must_use]
    pub fn mix_into(
        &self,
        paint: &mut Paint,
        alpha: f32,
        magnify: f32,
        device_pixel: f32,
    ) -> f32 {
        let tile = self.texture.borrow();

        let mut matrix = Matrix::new_identity();
        matrix
            .pre_translate(tile.shift)
            .pre_rotate(tile.angle.to_degrees(), None);

        // Scaling the mark in step with the grid is what holds the coverage
        // fixed. Widening the grid on its own would thin the pattern out.
        let line = tile.line * magnify;
        let scale = (tile.scale.0 * magnify, tile.scale.1 * magnify);

        // Only the line grid needs this, and only when it is drawn at all:
        // a zero width means the tile is filled, not stroked.
        let thinned = if tile.path.is_none()
            && line > 0.0
            && device_pixel.is_finite()
            && line < device_pixel
        {
            line / device_pixel
        } else {
            1.0
        };
        let line = if thinned < 1.0 { device_pixel } else { line };
        let alpha = alpha * thinned;

        match &tile.path {
            Some(path) => {
                // A uniform scale commutes with the rotation, so this is the
                // tile magnified about its own origin however it is turned.
                let shape = Matrix::scale((magnify, magnify))
                    * Matrix::rotate_rad(tile.angle);
                let path = path.with_transform(&shape);
                matrix.pre_scale(scale, None);
                paint.set_path_effect(path_2d_path_effect::new(&matrix, &path));
            }
            None => {
                // Parallel lines have a single meaningful period, so the
                // wider of the two wins.
                let scale = scale.0.max(scale.1);
                matrix.pre_scale((scale, scale), None);
                paint.set_path_effect(line_2d_path_effect::new(line, &matrix));
            }
        };

        // Tested against the tile's own width rather than the magnified one:
        // a positive factor cannot move it across zero, and the unmagnified
        // value is the one the caller chose fill or stroke with.
        if tile.line > 0.0 {
            paint.set_stroke_width(line);
            paint.set_stroke_cap(tile.cap);
            paint.set_style(PaintStyle::Stroke);
        } else {
            paint.set_style(PaintStyle::Fill);
        }

        let (mut color, space) = skia_paint_color(tile.color.0, &tile.color.1);
        color.a *= alpha;
        paint.set_color4f(color, &space);

        thinned
    }

    pub fn use_clip(&self) -> bool {
        !self.outline
    }

    /// The grid period the texture actually draws at.
    ///
    /// For a line tile that is the wider of the two components on both axes,
    /// because `mix_into` collapses them: parallel lines have one period, and
    /// reporting the pair as given describes a grid nothing draws.
    pub fn spacing(&self) -> Point {
        let tile = self.texture.borrow();
        match tile.path {
            Some(_) => tile.scale.into(),
            None => {
                let period = tile.scale.0.max(tile.scale.1);
                (period, period).into()
            }
        }
    }

    /// The narrowest extent of the mark the grid repeats, unmagnified.
    ///
    /// `None` for the line variety, whose mark is its stroke width and is
    /// already held to a device pixel by [`CanvasTexture::mix_into`]. For a
    /// tile path it is the shorter side of the path's bounds: the extent that
    /// has to survive rasterization for the tile to appear at all.
    /// `Context2D::texture_lattice` magnifies until it does.
    pub fn mark(&self) -> Option<f32> {
        let tile = self.texture.borrow();
        tile.path.as_ref().map(|path| {
            let bounds = path.bounds();
            bounds.width().abs().min(bounds.height().abs())
        })
    }

    pub fn to_color4f(&self, alpha: f32) -> (Color4f, Option<ColorSpace>) {
        let tile = self.texture.borrow();
        let (mut color, space) = skia_paint_color(tile.color.0, &tile.color.1);
        color.a *= alpha;
        (color, Some(space))
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedCanvasTexture> {
    let path = opt_skpath_arg(&mut cx, 1);
    let color = cx
        .argument_opt(2)
        .and_then(|value| color4f_string_in(&mut cx, value))
        .unwrap_or_else(|| {
            (Color4f::new(0.0, 0.0, 0.0, 1.0), ColorSpace::new_srgb())
        });

    let line = match opt_float_arg(&mut cx, 3) {
        Some(weight) => weight,
        None => cx.throw_type_error("Expected a number for `line`")?,
    };

    let cap = match to_stroke_cap(&string_arg(&mut cx, 4, "cap")?) {
        Some(style) => style.to_skia(),
        None => cx.throw_type_error(
            "Expected \"butt\", \"square\", or \"round\" for `cap`",
        )?,
    };

    let angle = match opt_float_arg(&mut cx, 5) {
        Some(theta) => theta,
        None => cx.throw_type_error("Expected a number for `angle`")?,
    };

    let outline = bool_arg(&mut cx, 6, "outline")?;

    let scale = match opt_float_args(&mut cx, 7..9).as_slice() {
        [h, v] => (*h, *v),
        _ => cx.throw_type_error(
            "Expected a number or array with 2 numbers for `spacing`",
        )?,
    };

    let shift = match opt_float_args(&mut cx, 9..11).as_slice() {
        [h, v] => (*h, *v),
        _ => cx.throw_type_error(
            "Expected a number or array with 2 numbers for `offset`",
        )?,
    };

    let canvas_texture = CanvasTexture::from_parts(
        path, color, line, cap, angle, outline, scale, shift,
    );
    Ok(cx.boxed(RefCell::new(canvas_texture)))
}

pub fn repr(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasTexture>(0)?;
    let this = this.borrow();

    let tile = this.texture.borrow();
    let style = if tile.path.is_some() { "Path" } else { "Lines" };
    Ok(cx.string(style))
}
