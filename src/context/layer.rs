//! One entry of a page's recording: a Skia picture, or a blurred draw kept as
//! ours so that its blur is made where the page is played back.
//!
//! A picture fixes everything about a draw when it is recorded, and a page is
//! played back under transforms the draw never saw: an export at a density,
//! a tile's origin, a canvas drawn into another at a size. Most of a draw
//! scales with them. How a blur is made does not have to: Skia's coverage
//! blur stops at a sigma of 128 device pixels, so a choice of it made at
//! density 1 can ask it for more than it draws at density 4. A [`BlurOp`]
//! checks that choice when it is played back, against the sigma it has
//! there.

use skia_safe::{
    BlurStyle, Canvas as SkCanvas, ClipOp, Color4f, ColorSpace, Data,
    MaskFilter, Matrix, Paint, PaintCap, PaintJoin, PaintStyle, Path, Picture,
    TileMode, image_filters,
};

use crate::{context::coverage_of, node::filter::MASK_BLUR_MAX_SIGMA};

/// One entry of a page's recording, played back in order.
#[derive(Clone, Debug)]
pub enum Layer {
    /// Draws Skia recorded, played back as recorded.
    Picture(Picture),
    /// A blurred flat-colour draw, its blur made at playback.
    Blur(BlurOp),
}

impl Layer {
    /// Draws this entry onto `canvas`, under whatever transform and clip
    /// the canvas already carries.
    pub fn playback(&self, canvas: &SkCanvas) {
        match self {
            Layer::Picture(picture) => picture.playback(canvas),
            Layer::Blur(op) => op.playback(canvas),
        }
    }
}

/// A fill or stroke in one flat colour, blurred by a `blur()` filter.
///
/// Holds what the draw needs and nothing Skia would have to fix early: the
/// geometry and the frame it was drawn in -- the transform and the clip --
/// and the blur's standard deviation in canvas pixels. The path and clip are
/// kept serialized, since a page is read from several threads at once and
/// Skia's `Path` is not `Sync`; `Data` is.
#[derive(Clone, Debug)]
pub struct BlurOp {
    /// The path, in the coordinates it was drawn in.
    path: Data,
    /// The transform it was drawn under.
    matrix: Matrix,
    /// The clip it was drawn under, in device pixels of the recording.
    clip: Option<Data>,
    /// The draw's colour, global alpha included, unpremultiplied.
    color: Color4f,
    /// The space `color` is in.
    space: ColorSpace,
    /// How the path is painted, and its stroke where it has one.
    style: PaintStyle,
    stroke: Stroke,
    anti_alias: bool,
    dither: bool,
    /// The blur's standard deviation in canvas pixels: in pixels of the
    /// canvas whatever its transform, as a filter's length is.
    sigma: f32,
}

/// How a stroke is drawn, read whether the draw strokes or not.
#[derive(Clone, Copy, Debug)]
struct Stroke {
    width: f32,
    miter: f32,
    cap: PaintCap,
    join: PaintJoin,
}

impl BlurOp {
    /// The draw of `path` under `matrix` and `clip` with `paint`, blurred by
    /// `sigma` canvas pixels. `paint` gives the colour and how the path is
    /// painted -- a paint with a shader or a path effect is not one this
    /// keeps -- and its filters, which this replaces, are not read.
    pub fn new(
        path: &Path,
        matrix: Matrix,
        clip: Option<&Path>,
        paint: &Paint,
        space: ColorSpace,
        sigma: f32,
    ) -> Self {
        let stroke = Stroke {
            width: paint.stroke_width(),
            miter: paint.stroke_miter(),
            cap: paint.stroke_cap(),
            join: paint.stroke_join(),
        };
        BlurOp {
            path: path.serialize(),
            matrix,
            clip: clip.map(Path::serialize),
            color: paint.color4f(),
            space,
            style: paint.style(),
            stroke,
            anti_alias: paint.is_anti_alias(),
            dither: paint.is_dither(),
            sigma,
        }
    }

    /// Draws the blurred path onto `canvas`, with the coverage blur the
    /// recording chose wherever it can draw the sigma this comes to in
    /// `canvas`'s device pixels, and the image blur where it cannot.
    pub fn playback(&self, canvas: &SkCanvas) {
        let Some(path) = Path::deserialize(&self.path) else {
            return;
        };
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        // How far the playback scales a canvas pixel: the density, a nested
        // canvas's size. The recording's own transform is not part of it,
        // since a filter's length does not follow the canvas transform.
        let replay = canvas
            .local_to_device_as_3x3()
            .map_radius(1.0)
            .filter(|s| s.is_normal() && *s > 0.0)
            .unwrap_or(1.0);
        canvas.concat(&self.matrix);

        let mut paint = Paint::new(self.color, Some(&self.space));
        paint
            .set_anti_alias(self.anti_alias)
            .set_dither(self.dither);
        paint
            .set_style(self.style)
            .set_stroke_width(self.stroke.width)
            .set_stroke_miter(self.stroke.miter)
            .set_stroke_cap(self.stroke.cap)
            .set_stroke_join(self.stroke.join);

        // The coverage blur where the recording chose it, as long as the
        // sigma it comes to here is one it can draw: past
        // `MASK_BLUR_MAX_SIGMA` device pixels it draws the cap instead, so
        // `blur(40px)` exported at density 4 came out a blur of 128 pixels
        // where 160 were asked for. Below the cap the recording's choice
        // stands, since on the GPU the coverage blur of a rectangle is
        // nearer the exact Gaussian than the image blur is, 1.5 levels
        // against 2.6 at a sigma of 80.
        let coverage = coverage_of(&path, &self.matrix);
        let coverage_blur = coverage.blurs_alike(self.sigma)
            && self.sigma * replay <= MASK_BLUR_MAX_SIGMA;
        // The sigma in the path's own units, which Skia maps back through
        // the canvas: `respect_ctm` for the mask filter, and an image
        // filter's lengths are local whatever it is given.
        let local = self
            .matrix
            .map_radius(1.0)
            .filter(|s| s.is_normal() && *s > 0.0);
        match (coverage_blur, local) {
            (true, Some(scale)) => {
                paint.set_mask_filter(MaskFilter::blur(
                    BlurStyle::Normal,
                    self.sigma / scale,
                    true,
                ));
            }
            // Divided as `Filter::filters_for` divides a rendered draw's.
            _ => {
                let sigma = (
                    self.sigma / self.matrix.scale_x(),
                    self.sigma / self.matrix.scale_y(),
                );
                paint.set_image_filter(image_filters::blur(
                    sigma,
                    TileMode::Decal,
                    None,
                    None,
                ));
            }
        }
        canvas.draw_path(&path, &paint);
        canvas.restore();
    }
}
