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
    image_filters,
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::{Recorder, gpu};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use crate::context::exact_blur_gpu;
use crate::{
    context::exact_blur::{self, REACH_SIGMAS},
    started,
};

/// One step of a chain, applied in order to what the step before it left.
#[derive(Clone, Debug)]
pub enum Step {
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
    if let Some(outcome) = run(canvas, steps, content, depth, paint, draw) {
        outcome.composite(canvas, paint);
    }
}

/// What a chain leaves, before it is composited: the image its last step
/// made, the device region the image covers under `rest`, and `rest`, the
/// part of the canvas's transform the layer was not drawn in.
#[derive(Clone)]
pub(crate) struct Outcome {
    image: Image,
    region: IRect,
    rest: Matrix,
}

impl Outcome {
    /// Draws the image back onto `canvas` at its place, through `paint` less
    /// its alpha, which the last step already took: unresampled where the
    /// rest of the transform is the identity, which every
    /// scale-and-translate playback leaves it. `canvas`'s clip, not the
    /// chain's, decides what of it shows.
    pub(crate) fn composite(&self, canvas: &SkCanvas, paint: &Paint) {
        let sampling = match self.rest.is_identity() {
            true => SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
            false => SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        };
        let mut paint = paint.clone();
        paint.set_alpha_f(1.0);
        canvas.save();
        canvas.set_matrix(&self.rest.into());
        canvas.draw_image_with_sampling_options(
            &self.image,
            (self.region.left as f32, self.region.top as f32),
            sampling,
            Some(&paint),
        );
        canvas.restore();
    }

    /// The bytes the image holds.
    pub(crate) fn bytes(&self) -> usize {
        self.image.image_info().compute_min_byte_size()
    }
}

/// Runs the chain [`apply`] does, up to the composite. `None` where nothing
/// would show or no layer could be made.
pub(crate) fn run(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &Paint,
    draw: impl FnOnce(&SkCanvas),
) -> Option<Outcome> {
    let base = canvas.local_to_device_as_3x3();
    let (layer, rest) = split(&base)?;
    let region = region(canvas, steps, content, &layer, &rest)?;
    #[cfg(test)]
    RUNS.with(|runs| runs.set(runs.get() + 1));
    let on_gpu = is_on_gpu(canvas);
    let (region, drawn) = placed(canvas, region, rest.is_identity(), on_gpu);
    // The region alone where Skia refuses a layer from the device's origin,
    // as `placed` says.
    let (mut surface, drawn) = std::iter::once(drawn)
        .chain((drawn != region).then_some(region))
        .find_map(|drawn| {
            #[cfg(test)]
            if drawn != region && REFUSE_PLACED.with(std::cell::Cell::get) {
                return None;
            }
            layer_surface(canvas, depth, drawn).map(|surface| (surface, drawn))
        })?;
    let mut context = gpu_of(&mut surface);
    let offscreen = surface.canvas();
    // Only what the chain reads is cleared: the rows and columns the CPU's
    // layer starts at the device's origin with lie before it, are never
    // read, and are left untouched.
    let read = region.with_offset((-drawn.left, -drawn.top));
    offscreen.save();
    offscreen.clip_irect(read, None);
    offscreen.clear(Color::TRANSPARENT);
    offscreen.restore();
    let origin = Matrix::translate((-drawn.left as f32, -drawn.top as f32));
    offscreen.set_matrix(&Matrix::concat(&origin, &layer).into());
    draw(offscreen);
    let snapshot = surface.image_snapshot();
    let mut image = match read == IRect::from_size(drawn.size()) {
        true => Some(snapshot),
        false => subset(&snapshot, context.as_mut(), read),
    }?;

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
        image = next?;
    }
    Some(Outcome {
        image,
        region,
        rest,
    })
}

#[cfg(test)]
impl Outcome {
    /// A transparent result `side` pixels square at the origin, for the
    /// tests of what keeps results.
    pub(crate) fn blank(side: i32) -> Self {
        let info = ImageInfo::new_n32_premul((side, side), None);
        let image = started::raster(&info, None, None)
            .expect("a surface")
            .image_snapshot();
        Self {
            image,
            region: IRect::from_wh(side, side),
            rest: Matrix::new_identity(),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How many chains [`run`] has run on this thread that had a region to
    /// work on, which the tests of a replayed group count by.
    pub(crate) static RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Whether [`run`] refuses the layer [`placed`] asks for first, as Skia
    /// refuses one too large, so that a test can reach the region-sized
    /// fallback on a page small enough to compare against.
    pub(crate) static REFUSE_PLACED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
/// - On the CPU Skia splits curves in floats at the device's own coordinates --
///   a conic into quadratics, a curve at its turning points -- and how a float
///   rounds depends on its magnitude, so a path translated by whole pixels can
///   be drawn otherwise. A rounded rectangle 7900 pixels in came out as the
///   page draws it at 46 of 203 integer origins and up to 8 levels apart at the
///   rest, along either axis; lines and rectangles came out alike at every one.
///   So the layer starts at the device's origin, as the page does, or at the
///   region's where that lies before it, and the chain runs on the region
///   alone. Only the region is written, and memory nothing writes is never
///   mapped, so the layer costs the pages its rows touch: a page or more a row,
///   which for a narrow region far into a wide page is most of its cost -- 8.5
///   MB for 518 rows of 16 KB pages, where the region alone is 0.3 MB.
/// - Where Skia refuses a layer that large -- one past the `i32::MAX` bytes it
///   allows a raster surface, which an F32 layer for a picture being recorded
///   can pass when the picture's own pixels would not -- the layer is the
///   region alone, and a curve in it can be drawn up to 8 levels from how the
///   page draws it. No shift short of the whole keeps the rounding: shifting
///   that rounded rectangle by multiples of 2^4 drew it as the page does at 106
///   of 493 shifts, and by multiples of 2^11 at 1 of 3.
/// - On the GPU an edge depends on the size of what it is drawn onto as well as
///   on where: the vertex shader maps device coordinates to clip space by 2
///   over the target's width and height (`GetRTAdjustVector`), so the same
///   position rounds otherwise on another size of target. A layer the size of
///   the region drew a rounded rectangle as a 2000-pixel page does at some
///   positions and not at others past x 1022, and as a page 4000 to 8193 wide
///   or 3000 to 8193 tall at fewer still -- up to 108 pixels over 20 positions,
///   each up to 0.13 of a level past a tie -- with no size below which it
///   always matched: 1408 and 1536 wide missed where 1472 and 1600 did not. So
///   the layer is the page's own size, or the page and the region together
///   where the region runs past it, and it drew as the page does at all 520
///   positions and sizes tried from 500 to 8193 on either axis. A layer that
///   runs past the page draws what lies past its edge as well, up to the
///   chain's reach, which the blur of the whole draw is made from and the page
///   itself never holds: past every edge of a 300 by 200 page that is the exact
///   blur of the whole draw. Past the reach the layer cuts it, as [`region`]
///   notes.
/// - That layer costs a second target the page's size, at the page's sample
///   count, for as long as [`layer_surface`] says Skia keeps it.
/// - Where Skia refuses one, the layer is the region alone, widened to an even
///   left edge and a bottom an even number of pixels from the page's: Metal's
///   surfaces have their origin at the bottom, and an odd offset either way
///   moved a rounded rectangle's corners by up to 7 levels. Blurred by 2px it
///   moved 80 to 132 pixels of one; the region-sized layer at even edges moved
///   none.
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
        let page = canvas.base_layer_size();
        let mut region = region;
        region.left -= region.left.rem_euclid(2);
        region.bottom += (region.bottom - page.height).rem_euclid(2);
        let drawn = IRect::from_ltrb(
            region.left.min(0),
            region.top.min(0),
            region.right.max(page.width),
            region.bottom.max(page.height),
        );
        return (region, drawn);
    }
    let drawn = IRect::from_ltrb(
        region.left.min(0),
        region.top.min(0),
        region.right,
        region.bottom,
    );
    (region, drawn)
}

/// A layer of `drawn`'s size for `canvas`, made by the canvas's own device
/// so that a draw lands on it as it would on the page: on the GPU at the
/// page's sample count. A layer without the page's samples drew edges
/// another way, and left a path's blur by 2 up to 3.7 levels off the blur of
/// what the page draws. A canvas recording a picture has no device to ask,
/// and gets memory. `None` where neither makes one.
///
/// A GPU layer the page's size is Skia's scratch target once let go. While
/// the GPU still runs the commands that read it, the command buffer holds
/// its four-sample attachment (`GrMtlGpu::resolve` files it there); once
/// they have run, Skia drops it at once where its cache is past its budget
/// -- a 6000 by 4000 page alone is -- and otherwise keeps it for the next
/// layer of that size until a purge or the idle release frees it. On a
/// 1200 by 900 page at four samples the cache held 55.5 MB against 35.4 MB
/// for region-sized layers, and both came back to the page's 21.6 MB when
/// purged.
fn layer_surface(
    canvas: &SkCanvas,
    depth: Depth,
    drawn: IRect,
) -> Option<Surface> {
    let info = layer_info(canvas, depth, drawn);
    canvas
        .new_surface(&info, None)
        .or_else(|| started::raster(&info, None, None))
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
/// which held the same result, for the layer's size, and on the GPU for
/// where it starts: there [`placed`] keeps the layer where the region puts
/// it, and a layer starting where the page does, as it does for a draw
/// within reach of the page's edge, draws the edge as the page does.
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

/// The part `read` of `image`, copied: on `context`'s GPU where it is on one,
/// which a texture's subset needs.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn subset(
    image: &Image,
    context: Option<&mut Gpu>,
    read: IRect,
) -> Option<Image> {
    let recorder =
        context.map(|context| context.as_recorder() as &mut dyn Recorder);
    image.make_subset(recorder, read, RequiredProperties { mipmapped: false })
}

/// The part `read` of `image`, copied.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn subset(image: &Image, _: Option<&mut Gpu>, read: IRect) -> Option<Image> {
    image.make_subset(None, read, RequiredProperties { mipmapped: false })
}

/// The GPU a chain's layers live on.
#[cfg(any(feature = "metal", feature = "vulkan"))]
type Gpu = gpu::RecordingContext;

/// No GPU: a build without a GPU backend has none to be on.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
enum Gpu {}

/// Whether `canvas` draws on the GPU.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn is_on_gpu(canvas: &SkCanvas) -> bool {
    canvas.recording_context().is_some()
}

/// Whether `canvas` draws on the GPU: never, in a build without a backend.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn is_on_gpu(_: &SkCanvas) -> bool {
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
        None => started::raster(info, None, None),
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
                (w, h),
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
                exact_blur::blur_bytes(pixels, w, h, sigma_x, sigma_y, gain),
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
            // The floats are let go once they are bytes, before Skia copies
            // the bytes: two buffers this size alive at once, never three.
            let bytes = exact_blur::blur(pixels, w, h, sigma_x, sigma_y)
                .iter()
                .flat_map(|v| (v * gain).to_ne_bytes())
                .collect::<Vec<u8>>();
            (info, bytes)
        }
    };
    let row_bytes = info.min_row_bytes();
    started::raster_from_data(&info, Data::new_copy(&pixels), row_bytes)
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
        Color, ImageInfo, Matrix, Paint, PathBuilder, PictureRecorder, Rect,
        Surface,
    };

    use super::{Depth, REFUSE_PLACED, Step, apply};
    use crate::{prelude::*, started};

    /// A side whose square of F32 pixels passes the `i32::MAX` bytes Skia
    /// allows a raster surface, so that no layer from the origin of a
    /// picture this large can be made: 12000 squared is 2.3 GB at 16 bytes
    /// a pixel.
    const PAST_RASTER_LIMIT: f32 = 12000.0;

    /// The bytes an F32 pixel takes.
    const F32_PIXEL: f64 = 16.0;

    /// The alpha of a 40-pixel square `at` from a picture's origin, blurred
    /// through a chain into the picture recorded over a square `bounds`
    /// wide, and played 40 pixels round.
    fn blurred_square(bounds: f32, at: f32) -> Vec<u8> {
        let mut recorder = PictureRecorder::new();
        let canvas =
            recorder.begin_recording(Rect::from_wh(bounds, bounds), false);
        let square = Rect::from_xywh(at, at, 40.0, 40.0);
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        apply(
            canvas,
            &[Step::Blur(4.0)],
            Some(square),
            Depth::Floats,
            &Paint::default(),
            |canvas| {
                canvas.draw_rect(square, &paint);
            },
        );
        let picture = recorder
            .finish_recording_as_picture(None)
            .expect("a picture");
        let info = ImageInfo::new_n32_premul((120, 120), None);
        let mut surface: Surface =
            started::raster(&info, None, None).expect("a surface");
        let origin = Matrix::translate((40.0 - at, 40.0 - at));
        surface.canvas().draw_picture(&picture, Some(&origin), None);
        let mut pixels = vec![0u8; 120 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 120 * 4, (0, 0)));
        pixels.chunks(4).map(|p| p[3]).collect()
    }

    /// A layer Skia will not make from the origin is the region alone, and
    /// a draw of lines in it -- which Skia draws alike wherever its origin
    /// falls -- blurs as it does from the origin.
    #[test]
    fn a_layer_too_large_from_the_origin_is_drawn_as_the_region_alone() {
        let side = f64::from(PAST_RASTER_LIMIT);
        assert!(side * side * F32_PIXEL > f64::from(i32::MAX));
        let near = blurred_square(300.0, 200.0);
        let far = blurred_square(PAST_RASTER_LIMIT, PAST_RASTER_LIMIT - 100.0);
        assert!(near.iter().any(|a| *a > 0), "the blur drew nothing");
        assert_eq!(near, far);
    }

    /// The surface's width, and where [`cut`] clips the triangle: through
    /// it, 60 pixels right of the edge measured.
    const WIDTH: i32 = 200;
    const CUT: f32 = 82.0;

    /// The alpha of a triangle filled anti-aliased on a raster surface,
    /// clipped at `clip` from the left where there is one.
    fn triangle(clip: Option<f32>) -> Vec<u8> {
        let info = ImageInfo::new_n32_premul((WIDTH, 120), None);
        let mut surface: Surface =
            started::raster(&info, None, None).expect("a surface");
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

    /// The alpha of a rounded rectangle blurred by 2px `at` on a GPU page
    /// `size` large, with the page-sized layer refused where `refuse` says
    /// so, or `None` where there is no GPU.
    fn on_the_gpu(
        size: (f32, f32),
        at: (f32, f32),
        refuse: bool,
    ) -> Option<Vec<u8>> {
        let mut canvas = Canvas::new(size.0, size.1);
        canvas.set_gpu(true);
        if canvas.engine_kind() != EngineKind::Gpu {
            return None;
        }
        let ctx = canvas.context();
        ctx.set_fill_style_css("black").expect("a colour");
        ctx.set_filter_css("blur(2px)").expect("a filter");
        ctx.begin_path();
        ctx.round_rect(at.0, at.1, 80.0, 40.0, [12.0; 4])
            .expect("a rounded rectangle");
        ctx.fill(FillRule::NonZero);
        REFUSE_PLACED.with(|refused| refused.set(refuse));
        let data = ctx.get_image_data(0.0, 0.0, size.0, size.1);
        REFUSE_PLACED.with(|refused| refused.set(false));
        Some(
            data.expect("image data")
                .pixels()
                .chunks(4)
                .map(|p| p[3])
                .collect(),
        )
    }

    /// Where the GPU refuses the page-sized layer, the region alone is drawn
    /// as the page draws it -- at an odd left edge and an odd distance from
    /// the page's bottom, which the fallback widens to even ones. Without
    /// that, these differed from the page in 132, 102 and 80 pixels.
    #[test]
    fn a_refused_gpu_layer_is_the_region_at_even_edges() {
        for (size, at) in [
            ((300.0, 200.0), (42.0, 30.25)),
            ((300.0, 201.0), (41.0, 30.25)),
            ((301.0, 200.0), (40.25, 30.75)),
        ] {
            let Some(page) = on_the_gpu(size, at, false) else {
                return;
            };
            let fallback = on_the_gpu(size, at, true).expect("a GPU");
            let off =
                page.iter().zip(&fallback).filter(|(a, b)| a != b).count();
            assert_eq!(
                off, 0,
                "{off} pixels off the page at {at:?} on {size:?}"
            );
            assert!(page.iter().any(|a| *a > 0), "the blur drew nothing");
        }
    }
}
