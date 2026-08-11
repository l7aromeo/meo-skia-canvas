#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    Color, Color4f, ColorSpace, Matrix, Paint, PaintCap, PaintStyle, Path,
    Point, line_2d_path_effect, path_2d_path_effect,
};
use std::{cell::RefCell, f32::consts::PI, rc::Rc};

use crate::utils::*;

struct Texture {
    path: Option<Path>,
    color: Color,
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
            color: Color::BLACK,
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
        color: Color,
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
    pub fn mix_into(&self, paint: &mut Paint, alpha: f32, magnify: f32) {
        let tile = self.texture.borrow();

        let mut matrix = Matrix::new_identity();
        matrix
            .pre_translate(tile.shift)
            .pre_rotate(180.0 * tile.angle / PI, None);

        // Scaling the mark in step with the grid is what holds the coverage
        // fixed. Widening the grid on its own would thin the pattern out.
        let line = tile.line * magnify;
        let scale = (tile.scale.0 * magnify, tile.scale.1 * magnify);

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

        let mut color: Color4f = tile.color.into();
        color.a *= alpha;
        paint.set_color4f(color, &ColorSpace::new_srgb());
    }

    pub fn use_clip(&self) -> bool {
        !self.outline
    }

    pub fn spacing(&self) -> Point {
        let tile = self.texture.borrow();
        tile.scale.into()
    }

    pub fn to_color4f(&self, alpha: f32) -> (Color4f, Option<ColorSpace>) {
        let tile = self.texture.borrow();
        let mut color: Color4f = tile.color.into();
        color.a *= alpha;
        // Texture colors come from CSS parsing -- they're sRGB.
        (color, Some(ColorSpace::new_srgb()))
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedCanvasTexture> {
    let path = opt_skpath_arg(&mut cx, 1);
    let color = opt_color_arg(&mut cx, 2).unwrap_or(Color::BLACK);

    let line = match opt_float_arg(&mut cx, 3) {
        Some(weight) => weight,
        None => cx.throw_type_error("Expected a number for `line`")?,
    };

    let cap = match to_stroke_cap(&string_arg(&mut cx, 4, "cap")?) {
        Some(style) => style,
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
