//! A filter chain run by this crate on pixels: a draw rasterised on its own,
//! each step applied to what the step before it left, and the result
//! composited onto the page.
//!
//! Skia evaluates an image filter inside its paint pipeline, and its blur is
//! not the exact Gaussian. There is no hook for running ours inside one, so
//! a draw whose blur has to be exact is played back here instead: drawn
//! unfiltered into a layer of its own, blurred by [`exact_blur`] on the CPU
//! or [`exact_blur_gpu`] on the GPU, and drawn back.
//!
//! Lengths in a chain are in canvas pixels, as a filter's are whatever the
//! canvas transform: the space the canvas's own pixels are in, which the
//! playback may map to the device by a density, a tile's origin, or the
//! size a canvas is drawn into another at. The layer is that space scaled
//! as the playback scales it, so that a blur is made at the resolution it
//! lands at. Where the playback only scales and translates -- an export at
//! a density, a tile -- the layer is the device's own pixels and the result
//! lands on them unresampled.

use skia_safe::{
    AlphaType, BlendMode, Canvas as SkCanvas, Color, Color4f, ColorFilter,
    ColorType, Data, FilterMode, IRect, Image, ImageInfo, Matrix, MipmapMode,
    Paint, Rect, RoundOut, SamplingOptions, Surface, Vector, color_filters,
    image::{CachingHint, RequiredProperties},
    image_filters, images, surfaces,
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::gpu;

use crate::context::exact_blur::{self, REACH_SIGMAS};
#[cfg(any(feature = "metal", feature = "vulkan"))]
use crate::context::exact_blur_gpu;

/// One step of a chain, applied in order to what the step before it left.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// The exact Gaussian blur of this standard deviation, in canvas pixels.
    Blur(f32),
    /// A colour filter, which is the same at any scale.
    Colour(ColorFilter),
}

/// How the chain's pixels are kept where the canvas it is played onto has
/// none of its own to copy -- a picture being recorded, which a pattern or
/// an image draws as pixels later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Depth {
    /// Eight bits a channel, as an N32 page composites.
    Bytes,
    /// Floats, as an F16 or F32 page composites; F32 holds either.
    Floats,
}

/// Plays `draw` onto `canvas` through `steps`, composited through `paint`.
///
/// `paint`'s alpha is taken into the last step rather than composited
/// after it, so that the result is rounded once: an 8-bit blur rounded,
/// scaled by an alpha of a half and rounded again lands on an exact tie for
/// every odd byte.
///
/// `canvas`'s transform maps canvas pixels to its device, and its clip
/// confines what the result paints, not what the chain reads: a blur
/// spreads what lies outside the clip into it. `draw` is handed the layer
/// with a transform mapping canvas pixels to the layer's, and draws what is
/// to be filtered under it. `content` bounds what `draw` paints, in canvas
/// pixels, where it is known; the layer is sized from it and the clip, so a
/// small draw does not cost a page-sized layer.
pub(crate) fn apply(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &Paint,
    draw: impl FnOnce(&SkCanvas),
) {
    let base = canvas.local_to_device_as_3x3();
    let Some((layer, rest)) = split(&base) else {
        return;
    };
    let Some(region) = region(canvas, steps, content, &layer, &rest) else {
        return;
    };
    let on_gpu = is_on_gpu(canvas);
    let (region, drawn) = placed(canvas, region, rest.is_identity(), on_gpu);
    let info = layer_info(canvas, depth, drawn);
    // A surface the canvas's own device makes, so that the draw lands as
    // it would on the page: on the GPU at the page's sample count. A layer
    // without the page's samples drew edges another way, and left a path's
    // blur by 2 up to 3.7 levels off the blur of what the page draws. A
    // canvas recording a picture has no device to ask, and gets memory.
    let Some(mut surface) = canvas
        .new_surface(&info, None)
        .or_else(|| surfaces::raster(&info, None, None))
    else {
        return;
    };
    let mut context = gpu_of(&mut surface);
    let offscreen = surface.canvas();
    // Only what the chain reads is cleared: the columns the CPU's layer
    // starts at the page's origin with lie before it and are never read.
    let read = region.with_offset((-drawn.left, -drawn.top));
    offscreen.save();
    offscreen.clip_irect(read, None);
    offscreen.clear(Color::TRANSPARENT);
    offscreen.restore();
    let origin = Matrix::translate((-drawn.left as f32, -drawn.top as f32));
    offscreen.set_matrix(&Matrix::concat(&origin, &layer).into());
    draw(offscreen);
    let snapshot = surface.image_snapshot();
    let Some(mut image) = (match read == IRect::from_size(drawn.size()) {
        true => Some(snapshot),
        false => snapshot.make_subset(
            None,
            read,
            RequiredProperties { mipmapped: false },
        ),
    }) else {
        return;
    };

    let scale = (layer.scale_x().abs(), layer.scale_y().abs());
    let last = steps.len().saturating_sub(1);
    for (i, step) in steps.iter().enumerate() {
        let gain = match i == last {
            true => paint.alpha_f(),
            false => 1.0,
        };
        let next = match step {
            Step::Blur(sigma) => blurred(
                context.as_mut(),
                &image,
                sigma * scale.0,
                sigma * scale.1,
                gain,
            ),
            Step::Colour(filter) => {
                filtered(context.as_mut(), &image, filter, gain)
            }
        };
        let Some(next) = next else {
            return;
        };
        image = next;
    }

    // Back onto the page at the layer's place: unresampled where the rest
    // of the transform is the identity, which every scale-and-translate
    // playback leaves it.
    let sampling = match rest.is_identity() {
        true => SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
        false => SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
    };
    let mut paint = paint.clone();
    paint.set_alpha_f(1.0);
    canvas.save();
    canvas.set_matrix(&rest.into());
    canvas.draw_image_with_sampling_options(
        &image,
        (region.left as f32, region.top as f32),
        sampling,
        Some(&paint),
    );
    canvas.restore();
}

/// `base` as `rest * layer`: `layer` the scale from canvas pixels to the
/// layer's, and `rest` what maps the layer onto the device. A transform
/// that only scales and translates is all layer, so the layer is the
/// device's pixels. Any other leaves the layer the canvas's pixels scaled
/// by as much as `base` stretches each axis, and the rotation, the skew or
/// the perspective to `rest`, as Skia's own filters do. `None` for a
/// transform that maps the canvas to nothing.
fn split(base: &Matrix) -> Option<(Matrix, Matrix)> {
    if base.is_scale_translate() {
        let invertible = base.scale_x() != 0.0 && base.scale_y() != 0.0;
        return invertible.then(|| (*base, Matrix::new_identity()));
    }
    let mut rest = Matrix::new_identity();
    match base.decompose_scale(Some(&mut rest)) {
        Some(scale) => Some((Matrix::scale((scale.width, scale.height)), rest)),
        // Perspective: blurred at the canvas's own resolution.
        None => base.invert().map(|_| (Matrix::new_identity(), *base)),
    }
}

/// Where the layer lies, given the `region` a chain needs: the region it
/// reads and writes, and the larger one it is drawn into. A layer that is
/// the device's own pixels is placed so that Skia draws into it as it draws
/// onto the page, which depends on where the layer's origin falls.
///
/// - On the CPU the anti-aliasing of an edge moves with the surface's x origin,
///   with no period: a rounded rectangle's corners came out alike with its left
///   edge 48, 42, 40, 38, 33 or 13 pixels into the layer and up to 8 levels
///   apart with it 56, 49, 44, 41, 39, 36, 32, 24 or 8 in. Its y origin and its
///   size moved nothing. So the layer is drawn from the page's own left edge,
///   or the region's where that lies further left, and the chain runs on the
///   region alone.
/// - On the GPU, Metal with multisampling and its surfaces' origin at the
///   bottom, an edge comes out as the page draws it wherever the layer's left
///   edge and its bottom are an even number of pixels from the page's; an odd
///   one moved a rounded rectangle's corners by up to 7 levels. So the region
///   is widened by a pixel on either side that needs it.
///
/// A layer the rest of the transform maps onto the device is not in its
/// pixels, and is left where the region puts it.
fn placed(
    canvas: &SkCanvas,
    region: IRect,
    on_device: bool,
    on_gpu: bool,
) -> (IRect, IRect) {
    if !on_device {
        return (region, region);
    }
    if on_gpu {
        let height = canvas.base_layer_size().height;
        let mut region = region;
        region.left -= region.left.rem_euclid(2);
        region.bottom += (region.bottom - height).rem_euclid(2);
        return (region, region);
    }
    let mut drawn = region;
    drawn.left = region.left.min(0);
    (region, drawn)
}

/// The layer's bounds, in its own pixels: the pixels a visible result is
/// made from, and the visible result itself. With the clip mapped back onto
/// the layer and `content` the bounds of what is painted, that is what is
/// painted within the chain's reach of the clip, and the clip within reach
/// of what is painted -- and so what lies between the two, which a chain
/// of several blurs passes through. Where nothing bounds what is painted,
/// the clip widened by the reach. `None` where nothing would show.
///
/// Bounded this tightly rather than by the clip widened by the reach,
/// which held the same result, for where the layer then starts as well as
/// for its size: Skia's anti-aliasing of an edge moves by a level or more
/// with where the surface's origin falls, and a layer starting where the
/// page does, as it does for a draw on the page within reach of its edge,
/// draws the edge as the page does.
fn region(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    layer: &Matrix,
    rest: &Matrix,
) -> Option<IRect> {
    // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
    // Re-check: cargo test skia_rasterises_a_path_its_clip_cuts_another_way
    //
    // Skia's analytic anti-aliasing takes another route for a path its clip
    // does not contain -- `SkScan::AAAFillPath`'s `containedInClip` -- and
    // the edges it draws then differ by up to 15 levels, however far they
    // are from the cut. The layer is cut where Skia's own filter layers
    // are, at the clip widened by the reach, so a draw this cuts is
    // rasterised as any draw the page's clip cuts is; drawing it whole
    // instead would cost a layer the size of the draw.
    let reach = reach(steps, layer);
    let clip = Rect::from(canvas.device_clip_bounds()?);
    let clip = match rest.is_identity() {
        true => clip,
        false => rest.invert()?.map_rect(clip).0,
    };
    // A filter that paints where its input is transparent -- a colour
    // matrix adding to the alpha -- spreads past any content's bounds, so
    // only the clip bounds the layer then.
    let bounded = steps.iter().all(|step| match step {
        Step::Blur(_) => true,
        Step::Colour(filter) => {
            image_filters::color_filter(filter.clone(), None, None)
                .is_some_and(|filter| filter.can_compute_fast_bounds())
        }
    });
    let region = match content.filter(|_| bounded) {
        Some(content) => {
            // A pixel either side for anti-aliasing.
            let painted = layer.map_rect(content).0.with_outset((1.0, 1.0));
            let meet = |mut a: Rect, b: Rect| a.intersect(b).then_some(a);
            let read = meet(painted, clip.with_outset(reach));
            let shown = meet(clip, painted.with_outset(reach));
            match (read, shown) {
                (Some(mut read), Some(shown)) => {
                    read.join(shown);
                    read
                }
                _ => return None,
            }
        }
        None => clip.with_outset(reach),
    };
    let region: IRect = region.round_out();
    (!region.is_empty()).then_some(region)
}

/// How far, in layer pixels along each axis, the chain's blurs together
/// carry a pixel.
fn reach(steps: &[Step], layer: &Matrix) -> Vector {
    let (sx, sy) = (layer.scale_x().abs(), layer.scale_y().abs());
    let sigma = steps
        .iter()
        .filter_map(|step| match step {
            Step::Blur(sigma) => Some(*sigma),
            Step::Colour(_) => None,
        })
        .filter(|sigma| sigma.is_finite() && *sigma > 0.0)
        .fold(0.0, |sum, sigma| sum + sigma);
    let taps = |sigma: f32| (REACH_SIGMAS * f64::from(sigma)).ceil() as f32;
    Vector::new(taps(sigma * sx), taps(sigma * sy))
}

/// The layer's pixels: the canvas's own colour type and space, or for a
/// canvas with none, `depth`'s type in no space at all, which is what a
/// recorded picture's own draws are in until it is played.
fn layer_info(canvas: &SkCanvas, depth: Depth, region: IRect) -> ImageInfo {
    let own = canvas.image_info();
    let color_type = match (own.color_type(), depth) {
        (ColorType::Unknown, Depth::Bytes) => ColorType::N32,
        (ColorType::Unknown, Depth::Floats) => ColorType::RGBAF32,
        (color_type, _) => color_type,
    };
    ImageInfo::new(
        region.size(),
        color_type,
        AlphaType::Premul,
        own.color_space(),
    )
}

/// The GPU a chain's layers live on.
#[cfg(any(feature = "metal", feature = "vulkan"))]
type Gpu = gpu::RecordingContext;

/// No GPU: a build without a GPU backend has none to be on.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
enum Gpu {}

/// Whether `canvas` draws on the GPU.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn is_on_gpu(canvas: &SkCanvas) -> bool {
    canvas.recording_context().is_some()
}

/// Whether `canvas` draws on the GPU: never, in a build without a backend.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn is_on_gpu(_: &SkCanvas) -> bool {
    false
}

/// The GPU `surface` is on, if it is on one.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn gpu_of(surface: &mut Surface) -> Option<Gpu> {
    surface.recording_context()
}

/// The GPU `surface` is on: none, in a build without a backend.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn gpu_of(_: &mut Surface) -> Option<Gpu> {
    None
}

/// A surface of `info` on `context`'s GPU where there is one, in memory
/// otherwise.
fn make_surface(
    context: Option<&mut Gpu>,
    info: &ImageInfo,
) -> Option<Surface> {
    match context {
        #[cfg(any(feature = "metal", feature = "vulkan"))]
        Some(context) => gpu::surfaces::render_target(
            context,
            gpu::Budgeted::Yes,
            info,
            None,
            gpu::SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        ),
        #[cfg(not(any(feature = "metal", feature = "vulkan")))]
        Some(never) => match *never {},
        None => surfaces::raster(info, None, None),
    }
}

/// `image` blurred by `sigma_x` along rows and `sigma_y` along columns and
/// scaled by `gain`, in its own pixel type: on the GPU it lives on where
/// that can, on the CPU otherwise.
fn blurred(
    context: Option<&mut Gpu>,
    image: &Image,
    sigma_x: f32,
    sigma_y: f32,
    gain: f32,
) -> Option<Image> {
    let raster = match context {
        #[cfg(any(feature = "metal", feature = "vulkan"))]
        Some(context) => {
            let (w, h) = (image.width() as usize, image.height() as usize);
            let on_gpu = exact_blur_gpu::blur(
                context,
                image,
                w,
                h,
                sigma_x,
                sigma_y,
                image.color_type(),
                gain,
            );
            if on_gpu.is_some() {
                return on_gpu;
            }
            // A target the GPU cannot make -- Metal has no F32 one -- is
            // blurred on the CPU, which every type reaches.
            let mut direct = context.as_direct_context();
            image.make_raster_image(direct.as_mut(), CachingHint::Disallow)?
        }
        #[cfg(not(any(feature = "metal", feature = "vulkan")))]
        Some(never) => match *never {},
        None => image.clone(),
    };
    blurred_on_cpu(&raster, sigma_x, sigma_y, gain)
}

/// `image`, a raster image, blurred on the CPU and scaled by `gain`: its
/// bytes rounded once where it is eight bits a channel, its floats
/// unrounded otherwise.
fn blurred_on_cpu(
    image: &Image,
    sigma_x: f32,
    sigma_y: f32,
    gain: f32,
) -> Option<Image> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let space = image.color_space();
    let bytes = matches!(
        image.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    // Read in the image's own space, so nothing is converted; an 8-bit
    // image in its own channel order, which a blur treats alike.
    let (info, pixels) = match bytes {
        true => {
            let info = image.image_info().with_alpha_type(AlphaType::Premul);
            let mut pixels = vec![0u8; w * h * 4];
            if !image.read_pixels(
                &info,
                &mut pixels,
                w * 4,
                (0, 0),
                CachingHint::Disallow,
            ) {
                return None;
            }
            (
                info,
                exact_blur::blur_bytes(&pixels, w, h, sigma_x, sigma_y, gain),
            )
        }
        false => {
            let info = ImageInfo::new(
                (w as i32, h as i32),
                ColorType::RGBAF32,
                AlphaType::Premul,
                space,
            );
            let mut pixels = vec![0f32; w * h * 4];
            let row_bytes = w * 4 * size_of::<f32>();
            if !image.read_pixels(
                &info,
                &mut pixels,
                row_bytes,
                (0, 0),
                CachingHint::Disallow,
            ) {
                return None;
            }
            let blurred = exact_blur::blur(&pixels, w, h, sigma_x, sigma_y);
            let bytes = blurred
                .iter()
                .flat_map(|v| (v * gain).to_ne_bytes())
                .collect::<Vec<u8>>();
            (info, bytes)
        }
    };
    let row_bytes = info.min_row_bytes();
    images::raster_from_data(&info, Data::new_copy(&pixels), row_bytes)
}

/// `image` drawn through `filter` and then scaled by `gain`, as one colour
/// filter, onto a layer of its size and type: written once, so rounded
/// once.
fn filtered(
    context: Option<&mut Gpu>,
    image: &Image,
    filter: &ColorFilter,
    gain: f32,
) -> Option<Image> {
    let filter = match gain < 1.0 {
        true => color_filters::blend_with_color_space(
            Color4f::new(0.0, 0.0, 0.0, gain),
            None,
            BlendMode::DstIn,
        )?
        .composed(filter.clone())?,
        false => filter.clone(),
    };
    let info = image.image_info();
    let mut surface = make_surface(context, info)?;
    let mut paint = Paint::default();
    paint.set_color_filter(filter);
    let canvas = surface.canvas();
    canvas.clear(Color::TRANSPARENT);
    canvas.draw_image(image, (0, 0), Some(&paint));
    Some(surface.image_snapshot())
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        Color, ImageInfo, Paint, PathBuilder, Rect, Surface, surfaces,
    };

    /// The surface's width, and where [`cut`] clips the triangle: through
    /// it, 60 pixels right of the edge measured.
    const WIDTH: i32 = 200;
    const CUT: f32 = 82.0;

    /// The alpha of a triangle filled anti-aliased on a raster surface,
    /// clipped at `clip` from the left where there is one.
    fn triangle(clip: Option<f32>) -> Vec<u8> {
        let info = ImageInfo::new_n32_premul((WIDTH, 120), None);
        let mut surface: Surface =
            surfaces::raster(&info, None, None).expect("a surface");
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        if let Some(x) = clip {
            canvas.clip_rect(Rect::from_ltrb(0.0, 0.0, x, 120.0), None, true);
        }
        let mut path = PathBuilder::new();
        path.move_to((20.0, 80.0))
            .line_to((80.0, 15.25))
            .line_to((140.5, 85.0))
            .close();
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        canvas.draw_path(&path.detach(), &paint);
        let mut pixels = vec![0u8; (WIDTH * 120 * 4) as usize];
        assert!(surface.read_pixels(
            &info,
            &mut pixels,
            (WIDTH * 4) as usize,
            (0, 0)
        ));
        pixels.chunks(4).map(|p| p[3]).collect()
    }

    /// Skia's anti-aliasing of an edge depends on whether the clip cuts the
    /// path elsewhere: the triangle's left edge, 60 pixels from the cut,
    /// moves by levels when the clip takes its right corner off. If this
    /// fails, Skia rasterises a path alike whatever its clip, and the note
    /// in `region` -- and the narrow shape `tests/blur_result.rs` keeps
    /// within its blur's layer -- can go.
    #[test]
    fn skia_rasterises_a_path_its_clip_cuts_another_way() {
        let (whole, cut) = (triangle(None), triangle(Some(CUT)));
        let worst = whole
            .iter()
            .zip(&cut)
            .enumerate()
            .filter(|(i, _)| (*i as i32 % WIDTH) < 60)
            .map(|(_, (a, b))| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(worst > 1, "the left edge moved by {worst} levels");
    }
}
