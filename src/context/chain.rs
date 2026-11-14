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

use std::sync::Arc;

use skia_safe::{
    AlphaType, BlendMode, Canvas as SkCanvas, Color, Color4f, ColorFilter,
    ColorSpace, ColorType, FilterMode, IPoint, IRect, Image, ImageFilter,
    ImageInfo, Matrix, MipmapMode, Paint, Rect, RoundOut, SamplingOptions,
    Shader, Surface, Vector,
    canvas::SrcRectConstraint,
    color_filters,
    image::{CachingHint, RequiredProperties},
    image_filter::MapDirection,
    image_filters,
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::{Recorder, gpu};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use crate::context::exact_blur_gpu;
use crate::{
    context::{
        blur_mask::baked_image,
        exact_blur::{self, REACH_SIGMAS},
        keeps_two_recordings,
        kept::ChainInput,
        layer::ExactColour,
    },
    gpu::{RenderingEngine, engine_of},
    node::utils::color_in_canvas_space,
    pixels::PixelColorSpace,
    started,
};

pub(crate) mod coverage;
pub(crate) mod pool;

/// One step of a chain, applied in order to what the step before it left.
#[derive(Clone, Debug)]
pub enum Step {
    /// The exact Gaussian blur of this standard deviation, in canvas pixels.
    Blur(f32),
    /// A colour filter, which is the same at any scale.
    Colour(ColorFilter),
    /// A `drop-shadow()`: what the step before left, over its own alpha in
    /// `colour` blurred by the exact Gaussian of `sigma` and moved by
    /// `offset`, both in canvas pixels. `colour` is unpremultiplied, in
    /// `space`.
    ///
    /// `shadow_only` leaves out what the step before left, so the step makes
    /// the shadow by itself: a `shadowColor` shadow, which the canvas
    /// composites apart from the draw that casts it, and which the HTML
    /// Standard blurs, offsets and colours as a `drop-shadow()` is.
    Shadow {
        offset: Vector,
        sigma: f32,
        colour: Color4f,
        space: ColorSpace,
        shadow_only: bool,
    },
    /// A Skia image filter, its lengths in the local units of the draw it
    /// filters, as on the layer Skia opens for it: run by drawing the step
    /// before's image through it, which Skia filters in that image's own
    /// colour type. On the CPU Skia's layer for it would be eight bits a
    /// channel whatever the page -- `SkBitmapDevice::createDevice` makes any
    /// layer whose paint carries an image filter `kN32`. A chain holding one
    /// is run in the space of the transform the draw was made under -- see
    /// [`Step::runs_in_local_space`].
    ///
    /// `pixels` is the filter a page's pixels run in its place, where the
    /// page keeps a recording for them and the filter holds a colour built
    /// for them -- see `Context2D::pixels_onto`; a document's embedded
    /// pixels run `filter`, as its vectors do.
    Image {
        filter: ImageFilter,
        pixels: Option<ImageFilter>,
    },
}

impl Step {
    /// Whether a chain holding this step is run in the space of the
    /// transform its draw was made under, rather than in canvas pixels: a
    /// Skia image filter's lengths turn and stretch with that transform,
    /// and only a layer in its space keeps them so.
    pub(crate) fn runs_in_local_space(&self) -> bool {
        matches!(self, Step::Image { .. })
    }

    /// `steps` as a page's pixels run them: each Skia image filter replaced
    /// by the one built for those pixels, where there is one. `None` where
    /// none is, and the pixels run `steps` as they are.
    pub(crate) fn for_pixels(steps: &[Step]) -> Option<Arc<[Step]>> {
        steps
            .iter()
            .any(|step| {
                matches!(
                    step,
                    Step::Image {
                        pixels: Some(_),
                        ..
                    }
                )
            })
            .then(|| {
                steps
                    .iter()
                    .map(|step| match step {
                        Step::Image {
                            pixels: Some(pixels),
                            ..
                        } => Step::Image {
                            filter: pixels.clone(),
                            pixels: None,
                        },
                        step => step.clone(),
                    })
                    .collect()
            })
    }
}

/// How the chain's pixels are kept where the canvas it is played onto has
/// none of its own to copy -- a picture being recorded, which a pattern or
/// an image draws as pixels later -- and, for [`Depth::Halves`], where the
/// canvas's own are bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Depth {
    /// Eight bits a channel, as an N32 page composites.
    Bytes,
    /// Floats, as an F16 or F32 page composites; F32 holds either.
    Floats,
    /// Half floats on a canvas of bytes, the canvas's own on one of
    /// floats: what holds every translucent colour placed on its bytes
    /// exactly where the GPU blurs it -- see `BlurOp::result_chain`.
    ///
    /// What it costs: 256 blurred circles on an eight-bit Display P3 page,
    /// read back, take 1.15 to 1.21 times as long on the GPU as through a
    /// layer of bytes, which does not hold them exactly -- 17 to 24
    /// microseconds a draw. The CPU takes no such layer. Half floats rather
    /// than floats because they are the cheaper: an F32 layer took 1.05 to
    /// 1.35 times as long as this one.
    Halves,
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
/// to be filtered under it -- more than once where a CPU layer starts before
/// the device's origin, see [`draw_from_the_origin`]. `content` bounds what
/// `draw` paints, in canvas pixels, where it is known; the layer is sized from
/// it and the clip, so a small draw does not cost a page-sized layer.
pub(crate) fn apply(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &Paint,
    draw: impl Fn(&SkCanvas),
) {
    if let Some(outcome) = run(canvas, steps, content, depth, paint, draw) {
        outcome.composite(canvas, paint);
    }
}

/// What a chain leaves, before it is composited: the image its last step
/// made, the device region it covers under `rest`, and `rest`, the part of
/// the canvas's transform the layer was not drawn in.
///
/// The image holds the part `inked` of the region, in the region's own
/// pixels, and every pixel of the region outside it is transparent black:
/// the whole region, unless whoever made the result counted -- see
/// [`Self::with_inked`]. The inked part lies at `source` in the image: at its
/// origin, but for results a batch of chains made in one shared target --
/// see [`Self::in_shared`]. A blur works on its reach of [`REACH_SIGMAS`]
/// sigmas past what it reads, and what it leaves rounds to zero long before
/// that: of the 124 by 89 pixels a 40px sans-serif "Ag" under `blur(3px)`
/// works on with CoreText, 62 by 54 are inked.
#[derive(Clone)]
pub(crate) struct Outcome {
    image: Image,
    region: IRect,
    rest: Matrix,
    inked: IRect,
    source: IPoint,
}

impl Outcome {
    /// The result of a chain whose `image` covers all of `region` under
    /// `rest`.
    pub(crate) fn whole(image: Image, region: IRect, rest: Matrix) -> Self {
        let inked = IRect::from_wh(image.width(), image.height());
        Self {
            image,
            region,
            rest,
            inked,
            source: IPoint::new(0, 0),
        }
    }

    /// The result of a chain covering all of `region` under `rest` that a
    /// batch made in `shared`, one target holding every chain's result, the
    /// region's pixel `(0, 0)` at `at` in it.
    pub(crate) fn in_shared(
        shared: Image,
        at: (i32, i32),
        region: IRect,
        rest: Matrix,
    ) -> Self {
        Self {
            image: shared,
            inked: IRect::from_size(region.size()),
            region,
            rest,
            source: IPoint::new(at.0, at.1),
        }
    }

    /// The result of a chain covering `region` under `rest` whose pixels
    /// are transparent black outside `inked`, in the region's own pixels,
    /// and `image` inside it. Where `inked` is empty `image` is the whole
    /// region, transparent.
    pub(crate) fn with_inked(
        image: Image,
        region: IRect,
        rest: Matrix,
        inked: IRect,
    ) -> Self {
        debug_assert!(
            match inked.is_empty() {
                true => image.dimensions() == region.size(),
                false => {
                    image.dimensions() == inked.size()
                        && IRect::intersect(
                            &IRect::from_size(region.size()),
                            &inked,
                        ) == Some(inked)
                }
            },
            "an image that is not what is inked"
        );
        Self {
            image,
            region,
            rest,
            inked,
            source: IPoint::new(0, 0),
        }
    }

    /// Draws the result back onto `canvas` at its place, through `paint`
    /// less its alpha, which the last step already took: unresampled where
    /// the rest of the transform is the identity, which every
    /// scale-and-translate playback leaves it. `canvas`'s clip, not the
    /// chain's, decides what of it shows.
    ///
    /// Unresampled and through a paint under which a transparent pixel
    /// leaves the canvas as it is -- see [`leaves_transparent_alone`] -- only
    /// what is inked is drawn, since the rest would change no byte. Otherwise
    /// the whole region is: resampled, an edge pixel is filtered with the
    /// transparent pixels beside it, and a blend such as `lighter` is not
    /// held to the plain source-over the trim is shown exact for.
    pub(crate) fn composite(&self, canvas: &SkCanvas, paint: &Paint) {
        let mut paint = paint.clone();
        paint.set_alpha_f(1.0);
        let nearest =
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None);
        let trimmed = self.rest.is_identity()
            && trims_composite()
            && leaves_transparent_alone(&paint);
        // A result a batch made in a shared target is drawn from its own
        // part of it, held to that part as an image of its own is held to
        // its edges.
        if !self.inked.is_empty()
            && (self.source != IPoint::new(0, 0)
                || self.image.dimensions() != self.inked.size())
        {
            #[cfg(test)]
            if trimmed {
                TRIMMED.set(TRIMMED.get() + 1);
            }
            let from = IRect::from_xywh(
                self.source.x,
                self.source.y,
                self.inked.width(),
                self.inked.height(),
            );
            let to =
                self.inked.with_offset((self.region.left, self.region.top));
            let sampling = match self.rest.is_identity() {
                true => nearest,
                false => {
                    SamplingOptions::new(FilterMode::Linear, MipmapMode::None)
                }
            };
            canvas.save();
            canvas.set_matrix(&self.rest.into());
            canvas.draw_image_rect_with_sampling_options(
                &self.image,
                Some((&Rect::from(from), SrcRectConstraint::Strict)),
                Rect::from(to),
                sampling,
                &paint,
            );
            canvas.restore();
            return;
        }
        let (image, at, sampling) = match trimmed {
            true => {
                #[cfg(test)]
                TRIMMED.set(TRIMMED.get() + 1);
                if self.inked.is_empty() {
                    return;
                }
                let at =
                    self.inked.with_offset((self.region.left, self.region.top));
                (self.image.clone(), at, nearest)
            }
            false => {
                let Some(image) = self.whole_image() else {
                    return;
                };
                let sampling = match self.rest.is_identity() {
                    true => nearest,
                    false => SamplingOptions::new(
                        FilterMode::Linear,
                        MipmapMode::None,
                    ),
                };
                (image, self.region, sampling)
            }
        };
        canvas.save();
        canvas.set_matrix(&self.rest.into());
        canvas.draw_image_with_sampling_options(
            &image,
            (at.left as f32, at.top as f32),
            sampling,
            Some(&paint),
        );
        canvas.restore();
    }

    /// The image of the whole region: the one held where it is that, and
    /// otherwise the inked part with transparent black around it. `None`
    /// where no surface could be made for it.
    fn whole_image(&self) -> Option<Image> {
        match self.image.dimensions() == self.region.size() {
            true => Some(self.image.clone()),
            false => padded(
                None,
                &self.image,
                self.inked,
                IRect::from_size(self.region.size()),
            ),
        }
    }

    /// The bytes the image holds.
    pub(crate) fn bytes(&self) -> usize {
        self.image.image_info().compute_min_byte_size()
    }

    /// This result composited `by` whole device pixels from where it was
    /// made, sharing its image. `None` unless the layer was the device's own
    /// pixels, `rest` the identity: otherwise a move on the device is not a
    /// move of the region. The caller holds that what it composites covers
    /// the same at the new place.
    pub(crate) fn at(&self, by: (i32, i32)) -> Option<Outcome> {
        self.rest.is_identity().then(|| Outcome {
            image: self.image.clone(),
            region: self.region.with_offset(by),
            rest: self.rest,
            inked: self.inked,
            source: self.source,
        })
    }
}

/// Whether drawing a transparent black pixel through `paint` leaves the
/// pixel under it as it is: a plain source-over, with no colour filter,
/// shader or mask filter to turn nothing into something. Source-over adds
/// the destination scaled by one less the source's alpha, which for an
/// alpha of zero is the destination itself, bit for bit, in Skia's raster
/// blitters (Skia M156, `src/opts/SkBlitRow_opts.h`): the NEON row scales a
/// byte by 255 less the alpha and rounds its division by 255, and the x86
/// rows by 256 less it, shifted down by 8. A blend such as
/// `copy` or `destination-in` clears what a transparent pixel covers, so
/// every pixel is drawn there.
fn leaves_transparent_alone(paint: &Paint) -> bool {
    paint.as_blend_mode() == Some(BlendMode::SrcOver)
        && paint.color_filter().is_none()
        && paint.shader().is_none()
        && paint.mask_filter().is_none()
        && paint.image_filter().is_none()
}

#[cfg(test)]
thread_local! {
    /// Whether a result is kept and composited as what is inked where it
    /// may be: off, the coverage route keeps its whole image and
    /// [`Outcome::composite`] draws the whole region, the reference the
    /// trimmed composite is held to.
    pub(crate) static TRIMS_COMPOSITE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(true) };
    /// How many composites on this thread drew only what is inked.
    pub(crate) static TRIMMED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Whether a result is kept and composited as what is inked where it may
/// be: [`TRIMS_COMPOSITE`]; always outside a test.
#[cfg(test)]
pub(crate) fn trims_composite() -> bool {
    TRIMS_COMPOSITE.get()
}

/// Whether a result is kept and composited as what is inked where it may
/// be: always outside a test.
#[cfg(not(test))]
pub(crate) fn trims_composite() -> bool {
    true
}

/// Runs the chain [`apply`] does, up to the composite. `None` where nothing
/// would show or no layer could be made.
///
/// The three stages a chain runs in, here for one draw: [`plan`], then
/// [`Drawn::new`] for the layer holding the draw, then [`Drawn::finish`].
pub(crate) fn run(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &Paint,
    draw: impl Fn(&SkCanvas),
) -> Option<Outcome> {
    let plan = plan(canvas, steps, content, depth)?;
    let mut drawn =
        Drawn::new(canvas, &[&plan], |_, offscreen| draw(offscreen))?;
    drawn.finish(&plan, steps, paint)
}

/// [`run`] for a chain whose one step casts a shadow from something other
/// than what it draws: `draw` draws what the shadow lies under, and `cast`
/// what the shadow is cast from, whose alpha the shadow takes at `alpha` --
/// see [`Cast`]. Each is drawn into a layer of its own.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_cast(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &Paint,
    alpha: f32,
    draw: impl Fn(&SkCanvas),
    cast: impl Fn(&SkCanvas),
) -> Option<Outcome> {
    let plan = plan(canvas, steps, content, depth)?;
    let mut drawn =
        Drawn::new(canvas, &[&plan], |_, offscreen| draw(offscreen))?;
    let from = Drawn::new(canvas, &[&plan], |_, offscreen| cast(offscreen))?;
    drawn.finish_cast(
        &plan,
        steps,
        paint,
        Some(Cast {
            drawn: &from,
            alpha,
        }),
    )
}

/// What a chain's shadow is cast from where that is not what the chain
/// draws: `drawn`, a layer holding a run of text drawn opaque, the shadow
/// taking its alpha at `alpha`, the alpha the run is drawn at -- see
/// `layer::CoverageRun`.
///
/// The shadow of a run drawn translucent is the blur of the run's coverage
/// times its alpha. Cast from the run as an eight-bit layer holds it, that
/// product is rounded to a byte before the blur and again after; cast from
/// the run drawn opaque, whose alpha is its coverage, and taken at the
/// alpha in floats, it is rounded once. The run the shadow lies under is
/// still the one drawn translucent, as Skia draws it: the shadow is not
/// linear in the alpha, since the run is composited over it, so the alpha
/// cannot be taken as one gain on the whole, as `blur()` takes it.
pub(crate) struct Cast<'a> {
    drawn: &'a Drawn,
    alpha: f32,
}

/// Where a chain's layer lies, settled before anything is drawn into it.
pub(crate) struct Plan {
    /// The scale from canvas pixels to the layer's -- see [`split`].
    layer: Matrix,
    /// What maps the layer onto the device -- see [`split`].
    rest: Matrix,
    /// What the chain reads of the layer, in the layer's pixels before the
    /// layer is placed -- see [`placed`].
    region: IRect,
    /// The pixels the chain works on, which hold `region` -- see [`region`].
    worked: IRect,
    /// The layer `placed` asks for first, in the same pixels as `region`.
    drawn: IRect,
    /// Every pixel of the layer the chain's draw can touch, its
    /// anti-aliasing included, where what it paints is known.
    ink: Option<IRect>,
    /// Whether the layer is on a GPU that draws a chain's contents into a
    /// shared layer as into one of its own -- see [`shares_layers`].
    shares: bool,
    /// What the layer holds a channel in.
    depth: Depth,
}

/// The most chains [`Plan::joins`] lets share one layer. Chosen, not
/// measured: it bounds the pairwise check a batch costs, and decides only
/// how chains are grouped, never what any of them draws.
pub(crate) const MAX_SHARED: usize = 64;

impl Plan {
    /// Whether this chain can draw into the layer `batch`'s chains share,
    /// and each of them still read what it would from a layer of its own.
    ///
    /// The layer has to be the one each would have: on a GPU where
    /// [`shares_layers`] holds, of the same [`Depth`], in the device's
    /// pixels, placed at the same bounds -- the page's, which an edge rounds
    /// against (see [`placed`]). And no chain's draw may touch a pixel
    /// another reads: a pixel a chain reads then holds the same samples,
    /// cleared and drawn by the same commands, as in a layer of its own.
    /// Where what a draw paints is not known, nothing can be shown of it,
    /// and the chain shares nothing.
    pub(crate) fn joins(&self, batch: &[&Plan]) -> bool {
        let Some(ink) = self.ink else {
            return false;
        };
        let apart = |read: IRect, ink: IRect| !IRect::intersects(&read, &ink);
        self.shares
            && self.rest.is_identity()
            && batch.len() < MAX_SHARED
            && batch.iter().all(|other| {
                other.drawn == self.drawn
                    && other.depth == self.depth
                    && other.shares
                    && other.rest.is_identity()
                    && other.ink.is_some_and(|other_ink| {
                        apart(self.region, other_ink)
                            && apart(other.region, ink)
                    })
            })
    }
}

/// The plan of a chain drawn under `canvas`'s transform and clip into a
/// layer of `depth`, with `content` bounding what it paints where that is
/// known. `None` where nothing would show.
pub(crate) fn plan(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    depth: Depth,
) -> Option<Plan> {
    let base = canvas.local_to_device_as_3x3();
    let (layer, rest) = split(&base)?;
    let (to_draw, worked) = region(canvas, steps, content, &layer, &rest)?;
    let (region, drawn) =
        placed(canvas, to_draw, rest.is_identity(), is_on_gpu(canvas));
    let worked = IRect::join(&worked, &region);
    let ink = content.map(|content| ink(&layer, content));
    #[cfg(test)]
    RUNS.with(|runs| runs.set(runs.get() + 1));
    #[cfg(test)]
    LARGEST_REGION.with(|largest| {
        let area = i64::from(worked.width()) * i64::from(worked.height());
        largest.set(largest.get().max(area));
    });
    Some(Plan {
        layer,
        rest,
        region,
        worked,
        drawn,
        ink,
        shares: shares_layers(canvas),
        depth,
    })
}

/// A layer holding what one or more chains draw, each under its own
/// [`Plan`], and the GPU it is on. A layer [`pool`] keeps goes back to it
/// when this is dropped, after the run that drew into it.
pub(crate) struct Drawn {
    snapshot: Image,
    /// The layer's bounds, in the pixels its plans' regions are in.
    at: IRect,
    context: Option<Gpu>,
    /// The layer itself and what it is kept under, where it is kept.
    kept: Option<(pool::Key, Surface)>,
}

impl Drop for Drawn {
    // The snapshot is let go when the fields are, just after this: by the
    // next draw onto the layer only the surface holds it, so Skia drops the
    // snapshot rather than copying the layer to keep it.
    fn drop(&mut self) {
        if let Some((key, surface)) = self.kept.take() {
            pool::keep(key, surface);
        }
    }
}

impl Drawn {
    /// One layer at `plans`' layer, which they share, of their depth,
    /// holding what `draw` draws for each of them: called with each plan's
    /// index in turn, onto a canvas under that plan's transform, in a save
    /// of its own, after every plan's region has been cleared -- and, for a
    /// layer in main memory that starts before the device's origin, once
    /// more for each onto a layer that starts at it, see
    /// [`draw_from_the_origin`]. `None` where no layer could be made.
    ///
    /// A single plan's layer falls back to its region alone where Skia
    /// refuses the one [`placed`] asks for, as `placed` says. Plans sharing a
    /// layer have no such fallback, since a region-sized layer is one plan's.
    pub(crate) fn new(
        canvas: &SkCanvas,
        plans: &[&Plan],
        mut draw: impl FnMut(usize, &SkCanvas),
    ) -> Option<Self> {
        let first = plans.first()?;
        let alone = (plans.len() == 1).then_some(first.region);
        let ((mut surface, key), at) = std::iter::once(first.drawn)
            .chain(alone.filter(|region| *region != first.drawn))
            .find_map(|at| {
                #[cfg(test)]
                if at != first.region
                    && REFUSE_PLACED.with(std::cell::Cell::get)
                {
                    return None;
                }
                layer_surface(canvas, first.depth, at)
                    .map(|surface| (surface, at))
            })?;
        #[cfg(test)]
        SHARED_LAYERS.with(|layers| layers.set(layers.get() + 1));
        let context = gpu_of(&mut surface);
        // In main memory it was made just before, so it is zeros already --
        // see [`clear_unless_zeroed`]; a layer taken from [`pool`] is a GPU
        // target, never in main memory.
        let zeroed = surface.peek_pixels().is_some();
        let offscreen = surface.canvas();
        // Only what the chains read is cleared: the rows and columns the
        // CPU's layer starts at the device's origin with lie before it, are
        // never read, and are left untouched.
        for plan in plans {
            offscreen.save();
            offscreen
                .clip_irect(plan.region.with_offset((-at.left, -at.top)), None);
            if !zeroed {
                offscreen.clear(Color::TRANSPARENT);
            }
            offscreen.restore();
        }
        let origin = Matrix::translate((-at.left as f32, -at.top as f32));
        for (i, plan) in plans.iter().enumerate() {
            offscreen.save();
            offscreen.set_matrix(&Matrix::concat(&origin, &plan.layer).into());
            draw(i, offscreen);
            offscreen.restore();
        }
        if zeroed {
            draw_from_the_origin(canvas, plans, at, offscreen, &mut draw);
        }
        Some(Self {
            snapshot: surface.image_snapshot(),
            at,
            context,
            kept: key.map(|key| (key, surface)),
        })
    }

    /// Runs `plan`'s chain of `steps` on what the layer holds of its region,
    /// up to the composite, with `paint`'s alpha taken by the last step.
    pub(crate) fn finish(
        &mut self,
        plan: &Plan,
        steps: &[Step],
        paint: &Paint,
    ) -> Option<Outcome> {
        self.finish_cast(plan, steps, paint, None)
    }

    /// [`Self::finish`], a shadow cast by the chain's first step cast from
    /// `cast` where it is `Some` -- see [`Cast`] -- and from what the layer
    /// holds otherwise.
    pub(crate) fn finish_cast(
        &mut self,
        plan: &Plan,
        steps: &[Step],
        paint: &Paint,
        cast: Option<Cast<'_>>,
    ) -> Option<Outcome> {
        let Plan {
            layer,
            rest,
            region,
            worked,
            ..
        } = *plan;
        let read = self.read(region);
        let context = &mut self.context;
        let scale = (layer.scale_x().abs(), layer.scale_y().abs());
        // On the CPU a chain handed an input it has run on before takes the
        // result it left, once the input is that one to the byte -- see
        // `kept`. The input is judged by the region where it lies in the
        // layer, before it is copied out and padded, which a result found
        // never needs; a shadow cast from another layer, by that layer's
        // region as well.
        let input = match context.is_none() {
            true => ChainInput::of_region(
                &self.snapshot,
                read,
                worked.size(),
                region.with_offset((-worked.left, -worked.top)),
                steps,
                Vector::new(scale.0, scale.1),
                paint.alpha_f(),
            )
            .and_then(|input| match &cast {
                Some(cast) => input.cast_from(
                    &cast.drawn.snapshot,
                    cast.drawn.read(region),
                    cast.alpha,
                ),
                None => Some(input),
            }),
            false => None,
        };
        if let Some(kept) = input.as_ref().and_then(ChainInput::kept) {
            return Some(Outcome::whole(kept, worked, rest));
        }
        let mut image =
            region_of(&self.snapshot, self.at, context.as_mut(), plan)?;
        let cast = match cast {
            Some(Cast { drawn, alpha }) => Some((
                region_of(&drawn.snapshot, drawn.at, context.as_mut(), plan)?,
                alpha,
            )),
            None => None,
        };

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
                Step::Shadow {
                    offset,
                    sigma,
                    colour,
                    space,
                    shadow_only,
                } => shadowed(
                    context.as_mut(),
                    &image,
                    cast.as_ref()
                        .filter(|_| i == 0)
                        .map(|(cast, alpha)| (cast, *alpha)),
                    Vector::new(offset.x * scale.0, offset.y * scale.1),
                    (sigma * scale.0, sigma * scale.1),
                    (*colour, space),
                    gain,
                    *shadow_only,
                ),
                Step::Image { filter, .. } => {
                    imaged(context.as_mut(), &image, filter, &layer, gain)
                }
            };
            image = next?;
        }
        if let Some(input) = input {
            input.keep(image.clone());
        }
        Some(Outcome::whole(image, worked, rest))
    }

    /// Where `region`, in the pixels a plan's regions are in, lies in the
    /// layer.
    fn read(&self, region: IRect) -> IRect {
        region.with_offset((-self.at.left, -self.at.top))
    }
}

/// What `layer`, a [`Drawn`]'s snapshot lying at `at`, holds of `plan`'s
/// region, copied out and padded to the rectangle its chain works on.
fn region_of(
    layer: &Image,
    at: IRect,
    mut context: Option<&mut Gpu>,
    plan: &Plan,
) -> Option<Image> {
    let read = plan.region.with_offset((-at.left, -at.top));
    let image = match read == IRect::from_size(at.size()) {
        true => Some(layer.clone()),
        false => subset(layer, context.as_deref_mut(), read),
    }?;
    match plan.worked == plan.region {
        true => Some(image),
        false => padded(context, &image, plan.region, plan.worked),
    }
}

/// The shadows of a run's chains, made in passes they share -- see
/// [`exact_blur_gpu::shadows`]: each chain whose one step casts a blurred
/// shadow is copied, as its layer is drawn, into a lane of one target, the
/// atlas, and once every layer is drawn the shadows cast by the same sigma
/// are made together. A run then pays its layers, the atlas and the passes
/// of one shadow for each sigma among them, however many layers its chains
/// were drawn in and whether or not any of them shares one, every pass a
/// render pass of its own. Each
/// chain reads only its own region of its lane, as a chain alone reads its
/// region copied out of its layer, so its bytes are those it makes alone.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) struct ShadowAtlas {
    /// Where each of the run's chains lies in the atlas, its worked
    /// rectangle's origin: `None` for a chain the atlas leaves alone.
    places: Vec<Option<(i32, i32)>>,
    /// The atlas's size, which holds every place.
    size: (usize, usize),
    /// The atlas, made with the first chain copied into it, of that chain's
    /// layer's type and colour space.
    surface: Option<Surface>,
    /// Each chain copied in.
    jobs: Vec<Staged>,
}

/// A chain [`ShadowAtlas`] holds.
#[cfg(any(feature = "metal", feature = "vulkan"))]
struct Staged {
    /// The chain's index in the run.
    index: usize,
    /// The bits of its sigma, which the chains made together share.
    sigma: (u32, u32),
    /// Its shadow, with its lane in the atlas.
    job: exact_blur_gpu::ShadowJob,
    /// The rectangle its result composites over.
    worked: IRect,
    /// The transform its result composites through.
    rest: Matrix,
}

#[cfg(any(feature = "metal", feature = "vulkan"))]
impl ShadowAtlas {
    /// An atlas for those of `chains` that cast a blurred shadow and
    /// nothing else, as many of them, in order, as one target of at most
    /// `max` pixels a side holds. `None` where fewer than two would share it,
    /// which gains nothing.
    pub(crate) fn plan(
        chains: &[(&Plan, &[Step])],
        max: usize,
    ) -> Option<Self> {
        let candidates: Vec<(usize, (usize, usize))> = chains
            .iter()
            .enumerate()
            .filter(|(_, (plan, steps))| shadow_sigma(plan, steps).is_some())
            .map(|(i, (plan, _))| {
                let worked = plan.worked;
                (i, (worked.width() as usize, worked.height() as usize))
            })
            .collect();
        let sizes: Vec<(usize, usize)> =
            candidates.iter().map(|&(_, size)| size).collect();
        // The longest leading run of candidates one target holds: what fits
        // shrinks with what is left out, so the lengths that fit are those
        // up to some length, found by halving.
        let fits = |n: usize| exact_blur_gpu::shelves(&sizes[..n], max);
        let (mut lo, mut hi) = (0, sizes.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            match fits(mid) {
                Some(_) => lo = mid,
                None => hi = mid - 1,
            }
        }
        if lo < 2 {
            return None;
        }
        let (size, at) = fits(lo)?;
        let mut places = vec![None; chains.len()];
        for (&(i, _), place) in candidates.iter().zip(at) {
            places[i] = Some(place);
        }
        Some(Self {
            places,
            size,
            surface: None,
            jobs: Vec::new(),
        })
    }

    /// Each copied chain's result, by its index in the run, made from the
    /// atlas on `context`; and the chains whose shared passes could not be
    /// made, or that have no context to be made on, which then finish
    /// alone.
    pub(crate) fn finish(
        self,
        context: Option<&mut Gpu>,
    ) -> (Vec<(usize, Outcome)>, Vec<usize>) {
        let (Some(mut surface), Some(context)) = (self.surface, context) else {
            return (
                Vec::new(),
                self.jobs.iter().map(|job| job.index).collect(),
            );
        };
        let atlas = surface.image_snapshot();
        let mut sigmas: Vec<(u32, u32)> =
            self.jobs.iter().map(|job| job.sigma).collect();
        sigmas.sort_unstable();
        sigmas.dedup();
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        for sigma in sigmas {
            let batch: Vec<_> =
                self.jobs.iter().filter(|job| job.sigma == sigma).collect();
            let jobs: Vec<exact_blur_gpu::ShadowJob> =
                batch.iter().map(|job| job.job).collect();
            match exact_blur_gpu::shadows(
                context,
                &atlas,
                (f32::from_bits(sigma.0), f32::from_bits(sigma.1)),
                &jobs,
            ) {
                Some((shared, places)) => {
                    for (job, at) in batch.iter().zip(places) {
                        done.push((
                            job.index,
                            Outcome::in_shared(
                                shared.clone(),
                                at,
                                job.worked,
                                job.rest,
                            ),
                        ));
                    }
                }
                None => failed.extend(batch.iter().map(|job| job.index)),
            }
        }
        (done, failed)
    }
}

/// The sigma a chain of `steps` planned as `plan` blurs its shadow by, in
/// its layer's pixels, where its one step casts a blurred shadow -- what
/// [`ShadowAtlas`] takes. `None` for any other chain.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn shadow_sigma(plan: &Plan, steps: &[Step]) -> Option<(f32, f32)> {
    let [Step::Shadow { sigma, .. }] = steps else {
        return None;
    };
    let scale = (plan.layer.scale_x().abs(), plan.layer.scale_y().abs());
    let sigma = (sigma * scale.0, sigma * scale.1);
    (sigma.0 > 0.0 || sigma.1 > 0.0).then_some(sigma)
}

#[cfg(any(feature = "metal", feature = "vulkan"))]
impl Drawn {
    /// Copies the chain the run knows as `index`, planned into this layer
    /// as `plan`, with its steps and the paint its last step takes the
    /// alpha of, into `atlas`, to be finished there -- see [`ShadowAtlas`].
    /// Whether it did: a chain the atlas has no place for, one on a layer
    /// that is not a GPU layer of bytes in the atlas's type and colour
    /// space, and one whose shadow colour has no tint, are finished alone.
    pub(crate) fn stage(
        &mut self,
        atlas: &mut ShadowAtlas,
        index: usize,
        (plan, steps, paint): (&Plan, &[Step], &Paint),
    ) -> bool {
        let Some(place) = atlas.places.get(index).copied().flatten() else {
            return false;
        };
        let (
            Some(sigma),
            [
                Step::Shadow {
                    offset,
                    colour,
                    space,
                    shadow_only,
                    ..
                },
            ],
        ) = (shadow_sigma(plan, steps), steps)
        else {
            return false;
        };
        let Some(context) = self.context.as_mut() else {
            return false;
        };
        let info = self.snapshot.image_info();
        if !matches!(
            info.color_type(),
            ColorType::RGBA8888 | ColorType::BGRA8888
        ) {
            return false;
        }
        let Some(tint) = tint_colour(*colour, space, info) else {
            return false;
        };
        let surface = match &mut atlas.surface {
            Some(surface) => {
                let held = surface.image_info();
                let alike = held.color_type() == info.color_type()
                    && held.color_space() == info.color_space();
                if !alike {
                    return false;
                }
                surface
            }
            none => {
                let info = info.with_dimensions((
                    atlas.size.0 as i32,
                    atlas.size.1 as i32,
                ));
                let Some(mut surface) = make_surface(Some(context), &info)
                else {
                    return false;
                };
                clear_unless_zeroed(&mut surface);
                none.insert(surface)
            }
        };
        let (region, worked) = (plan.region, plan.worked);
        // The region, byte for byte, where it lies in the chain's worked
        // rectangle at its place: what a chain alone copies out of its layer.
        let from = IRect::from_xywh(
            region.left - self.at.left,
            region.top - self.at.top,
            region.width(),
            region.height(),
        );
        let to = IRect::from_xywh(
            place.0 + region.left - worked.left,
            place.1 + region.top - worked.top,
            region.width(),
            region.height(),
        );
        let mut copy = Paint::default();
        copy.set_blend_mode(BlendMode::Src);
        surface.canvas().draw_image_rect_with_sampling_options(
            &self.snapshot,
            Some((&Rect::from(from), SrcRectConstraint::Strict)),
            Rect::from(to),
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
            &copy,
        );
        let scale = (plan.layer.scale_x().abs(), plan.layer.scale_y().abs());
        let job = exact_blur_gpu::ShadowJob {
            from: place,
            reads: IRect::from_xywh(
                region.left - worked.left,
                region.top - worked.top,
                region.width(),
                region.height(),
            ),
            size: (worked.width() as usize, worked.height() as usize),
            offset: (offset.x * scale.0, offset.y * scale.1),
            tint,
            gain: paint.alpha_f(),
            shadow_only: *shadow_only,
        };
        atlas.jobs.push(Staged {
            index,
            sigma: (sigma.0.to_bits(), sigma.1.to_bits()),
            job,
            worked,
            rest: plan.rest,
        });
        true
    }
}

/// `image`, which holds `region` of the layer, with nothing around it out
/// to `worked`: the pixels the chain works on that nothing is painted in.
fn padded(
    context: Option<&mut Gpu>,
    image: &Image,
    region: IRect,
    worked: IRect,
) -> Option<Image> {
    let info = image.image_info().with_dimensions(worked.size());
    let mut surface = make_surface(context, &info)?;
    clear_unless_zeroed(&mut surface);
    let canvas = surface.canvas();
    let at = (region.left - worked.left, region.top - worked.top);
    canvas.draw_image(image, (at.0 as f32, at.1 as f32), None);
    Some(surface.image_snapshot())
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
        Self::whole(image, IRect::from_wh(side, side), Matrix::new_identity())
    }
}

#[cfg(test)]
thread_local! {
    /// How many chains [`plan`] has planned on this thread that had a region
    /// to work on, which the tests of a replayed group count by.
    pub(crate) static RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Whether [`Drawn::new`] refuses the layer [`placed`] asks for first, as
    /// Skia refuses one too large, so that a test can reach the region-sized
    /// fallback on a page small enough to compare against.
    pub(crate) static REFUSE_PLACED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// Whether [`shares_layers`] says no, as it does on Vulkan, so that a
    /// test on Metal can hold a page whose chains each take a layer of
    /// their own to what it lands and the passes it pays.
    pub(crate) static UNSHARED_LAYERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// How many layers [`Drawn::new`] has made on this thread, which the
    /// tests of chains sharing a layer count by.
    pub(crate) static SHARED_LAYERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// How many layer surfaces [`layer_surface`] has made on this thread
    /// rather than taken from [`pool`], which the tests of kept layers count
    /// by.
    pub(crate) static MADE_LAYERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// The most pixels a chain [`plan`] on this thread has worked on, which
    /// the tests of a chain's layer size count by.
    pub(crate) static LARGEST_REGION: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };

    /// How many GPU targets a chain has opened on this thread after its
    /// layer -- a region copied out of it, padded, a run's atlas, a blur's
    /// pass, a shadow's composition -- each one a render pass of its own,
    /// which the tests of how many passes a page of chains costs count by.
    pub(crate) static OFFSCREEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts one GPU target a chain opens -- see [`OFFSCREEN`].
#[cfg(test)]
pub(crate) fn opened_offscreen() {
    OFFSCREEN.with(|n| n.set(n.get() + 1));
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
///   alone. One that starts before the origin draws what lies from the origin
///   on again in a layer that starts at it, which the chain takes that part
///   from -- see [`draw_from_the_origin`]. Only the region is written, and
///   memory nothing writes is never mapped, so the layer costs the pages its
///   rows touch: a page or more a row, which for a narrow region far into a
///   wide page is most of its cost -- 8.5 MB for 518 rows of 16 KB pages, where
///   the region alone is 0.3 MB.
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
/// - On the GPU the region is widened to an even left edge and a bottom an even
///   number of pixels from the page's, whichever layer it is drawn in, so that
///   where Skia refuses the page-sized one the layer can be the region alone:
///   Metal's surfaces have their origin at the bottom, and an odd offset either
///   way moved a rounded rectangle's corners by up to 7 levels. Blurred by 2px
///   it moved 80 to 132 pixels of one; the region-sized layer at even edges
///   moved none. In the page-sized layer the widening reads and writes at most
///   one more column and one more row of the blur.
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

/// Clears `surface`, made just before by Skia -- never one wrapped around
/// pixels of our own -- to transparent where its pixels may hold anything
/// else. One whose pixels are in main memory holds nothing but zero bits
/// already, by Skia's guarantee (Skia M156): `SkSurfaces::Raster`, which a
/// raster canvas's `new_surface` also ends in (`SkBitmapDevice.cpp:651-652`),
/// allocates them through `SkMallocPixelRef::MakeAllocate`
/// (`SkSurface_Raster.cpp:214-219`), which callocs them
/// (`SkMallocPixelRef.cpp:47`), and documents them as "zeroed before use"
/// (`include/core/SkSurface.h:61-62`). Zero bits are transparent in every
/// colour type -- +0.0 in a float one -- which is what a clear writes, so
/// clearing such a surface again only writes the same bytes; on a float
/// layer through Skia's raster pipeline, 12% of an 8-bit drop shadow's draw.
/// A GPU target's texture may be a scratch one used before, and is cleared.
pub(crate) fn clear_unless_zeroed(surface: &mut Surface) {
    if surface.peek_pixels().is_none() {
        surface.canvas().clear(Color::TRANSPARENT);
    }
}

/// Draws again, in a layer in main memory starting at the device's origin,
/// what `plans`' chains draw onto `offscreen`, one at `at` that starts
/// before it, and copies the rows of each chain's region from the origin on
/// from it over `offscreen`'s: so that a chain whose region crosses the page's
/// top or left edge draws its input there as a chain inside the page does.
/// Skia's anti-aliasing covers a curve by where on the raster it lies, whole
/// pixels included -- see [`placed`] -- and Times New Roman's 96px "Ag",
/// stroked 40 wide, drew 25 levels apart in a layer starting 54 pixels before
/// the origin. The layer's size does not decide it on the CPU: one from the
/// origin to the region's far corner draws as the page does. What lies before
/// the origin has no page to match, and keeps `offscreen`'s pixels. Nothing
/// is done for a layer starting at the origin or past it, or one the rest of
/// the transform maps onto the device, which is not in the device's pixels.
fn draw_from_the_origin(
    canvas: &SkCanvas,
    plans: &[&Plan],
    at: IRect,
    offscreen: &SkCanvas,
    draw: &mut impl FnMut(usize, &SkCanvas),
) {
    let Some(first) = plans.first() else {
        return;
    };
    if (at.left >= 0 && at.top >= 0)
        || !plans.iter().all(|plan| plan.rest.is_identity())
    {
        return;
    }
    let from_origin = IRect::from_ltrb(0, 0, at.right, at.bottom);
    let inside: Vec<IRect> = plans
        .iter()
        .filter_map(|plan| IRect::intersect(&plan.region, &from_origin))
        .collect();
    if inside.is_empty() {
        return;
    }
    let Some((mut layer, _)) = layer_surface(canvas, first.depth, from_origin)
    else {
        return;
    };
    // Made just before in main memory, so zeros already -- see
    // [`clear_unless_zeroed`].
    for (i, plan) in plans.iter().enumerate() {
        layer.canvas().save();
        layer.canvas().set_matrix(&plan.layer.into());
        draw(i, layer.canvas());
        layer.canvas().restore();
    }
    // The rows are copied as they lie: the two layers share a colour type
    // and space, so a write is a copy of bytes. A draw in `Src` does the same
    // through Skia's raster pipeline, and took most of this function's time.
    let Some(drawn) = layer.peek_pixels() else {
        return;
    };
    for area in &inside {
        let written = drawn.extract_subset(area).is_some_and(|rows| {
            rows.bytes().is_some_and(|bytes| {
                offscreen.write_pixels(
                    rows.info(),
                    bytes,
                    rows.row_bytes(),
                    (area.left - at.left, area.top - at.top),
                )
            })
        });
        debug_assert!(written, "a layer in main memory takes its own rows");
    }
}

/// A layer of `drawn`'s size for `canvas`, made by the canvas's own device
/// so that a draw lands on it as it would on the page: on the GPU at the
/// page's sample count. A layer without the page's samples drew edges
/// another way, and left a path's blur by 2 up to 3.7 levels off the blur of
/// what the page draws. A canvas recording a picture has no device to ask,
/// and gets memory. `None` where neither makes one.
///
/// A GPU layer for the canvas of an open [`pool::LayerScope`] is taken from
/// the layers [`pool`] keeps where one fits, cleared whole, and comes with
/// the key it goes back under once its run is done; [`Drawn::new`] clears
/// every region a chain reads before drawing besides, so what a kept layer
/// held before is never read. Any other layer is let go after its run, to
/// Skia's resource cache, which keeps it only while it fits the cache's budget
/// -- see [`pool`].
///
/// A Graphite page's layer is in memory, made as a CPU page's is, and never
/// kept: its chains take the CPU's route, whose steps read the layer where it
/// lies. A layer the page made would be a Graphite texture, which nothing on
/// that route reads.
fn layer_surface(
    canvas: &SkCanvas,
    depth: Depth,
    drawn: IRect,
) -> Option<(Surface, Option<pool::Key>)> {
    let info = layer_info(canvas, depth, drawn);
    match engine_of(canvas) {
        RenderingEngine::Graphite => {
            return started::raster(&info, None, None)
                .map(|raster| (raster, None));
        }
        RenderingEngine::GPU | RenderingEngine::CPU => {}
    }
    let key = pool::key(canvas, &info);
    if let Some(mut kept) = key.as_ref().and_then(pool::take) {
        // Cleared whole, not only where the chains read: a pass whose target
        // is cleared first starts from nothing, where one that keeps the
        // target's contents loads every sample of the page-sized layer first.
        kept.canvas().clear(Color::TRANSPARENT);
        return Some((kept, key));
    }
    #[cfg(test)]
    MADE_LAYERS.with(|made| made.set(made.get() + 1));
    match canvas.new_surface(&info, None) {
        Some(surface) => Some((surface, key)),
        None => started::raster(&info, None, None).map(|raster| (raster, None)),
    }
}

/// The rectangle `region` takes as painted, in the layer's pixels:
/// `content` under `layer`, with a pixel either side for anti-aliasing,
/// rounded out. A rounded rectangle's coverage stays inside its bounds
/// rounded out, so for one the pixel changes nothing; it keeps what one
/// chain reads and what another is taken to paint the same rectangle.
pub(super) fn ink(layer: &Matrix, content: Rect) -> IRect {
    layer
        .map_rect(content)
        .0
        .with_outset((1.0, 1.0))
        .round_out()
}

/// The layer's bounds, in its own pixels: the region drawn into, and the
/// region the chain works on, which holds it. With the clip mapped back
/// onto the layer and `content` the bounds of what is painted, what is
/// drawn is what is painted within the chain's reach of the clip, and the
/// clip within reach of what is painted -- and so what lies between the
/// two. The chain works on that and what [`worked_on`] adds past both:
/// nothing is painted there, but a step's output there can reach what is
/// shown -- a drop shadow's offset brings in the blur made past the page's
/// edge. Cut at what is drawn, a shadow 2 pixels down blurred by 8 lost up
/// to 1.56 levels of its tail along the top of the page. Where nothing
/// bounds what is painted, both are the clip widened by the reach. `None`
/// where nothing would show.
///
/// What is drawn is not widened to what the chain works on, because where
/// the layer starts decides how Skia draws into it -- see [`placed`]: a
/// rounded rectangle 30 pixels from the top of the page, drawn into a layer
/// starting above the page for a blur of 8, rounded 28 pixels of the blur
/// to the other side of a half-level from the blur of what the page draws.
///
/// Bounded by what is painted as well as the clip, for the layer's size:
/// a 15 by 12 rect under a drop shadow works on 30 by 27 pixels rather
/// than a page's worth.
fn region(
    canvas: &SkCanvas,
    steps: &[Step],
    content: Option<Rect>,
    layer: &Matrix,
    rest: &Matrix,
) -> Option<(IRect, IRect)> {
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
    // Re-check: cargo test skia_rasterises_a_path_its_clip_cuts_another_way
    //
    // Skia's analytic anti-aliasing takes another route for a path its clip
    // does not contain -- `SkScan::AAAFillPath`'s `containedInClip` -- and
    // the edges it draws then differ by up to 15 levels, however far they
    // are from the cut. The layer is cut where Skia's own filter layers
    // are, at the clip widened by the reach, so a draw this cuts is
    // rasterised as any draw the page's clip cuts is; drawing it whole
    // instead would cost a layer the size of the draw.
    //
    // On the CPU engine that includes a draw running past the page's edge
    // by more than the reach: an 80 by 40 rounded rectangle 40 pixels past
    // the right edge of a 300 by 200 page reads 15 pixels off the exact
    // blur of the whole draw blurred by 2px, by at most 0.85 of a level,
    // and none off blurred by 8px; 25 pixels past the bottom edge it is
    // exact at both. The page's edge cuts an unblurred draw the same way,
    // and docs/browser-differences.md lists that as "a fill or stroke the
    // page's edge cuts". The GPU engine's coverage is not moved by its
    // clip, and there the same draws are exact.
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
        Step::Shadow { .. } => true,
        Step::Image { filter, .. } => filter.can_compute_fast_bounds(),
    });
    let (drawn, worked) = match content.filter(|_| bounded) {
        Some(content) => {
            // A pixel either side for anti-aliasing.
            let painted = layer.map_rect(content).0.with_outset((1.0, 1.0));
            let meet = |mut a: Rect, b: Rect| a.intersect(b).then_some(a);
            let read = meet(painted, clip.with_outset(reach))?;
            let shown = meet(clip, painted.with_outset(reach))?;
            let mut drawn = read;
            drawn.join(shown);
            let worked =
                worked_on(steps, layer, painted, clip).unwrap_or(drawn);
            (drawn, worked)
        }
        None => (clip.with_outset(reach), clip.with_outset(reach)),
    };
    let (drawn, worked): (IRect, IRect) =
        (drawn.round_out(), worked.round_out());
    (!drawn.is_empty()).then_some((drawn, worked))
}

/// What one step does to where pixels lie, in layer pixels.
#[derive(Clone, Copy)]
enum Spread {
    /// Each pixel reaches as far as this along each axis, either way.
    Grow(Vector),
    /// What came in stays, and a copy of it grown by `grow` is moved by
    /// `by`: a drop shadow.
    Cast { grow: Vector, by: Vector },
}

impl Spread {
    /// How `step` moves pixels on a layer scaled by `layer`: a blur out to
    /// [`REACH_SIGMAS`] of its sigmas, as [`reach`] counts it.
    fn of(step: &Step, layer: &Matrix) -> Self {
        let (sx, sy) = (layer.scale_x().abs(), layer.scale_y().abs());
        let taps =
            |sigma: f32, scale: f32| match sigma.is_finite() && sigma > 0.0 {
                true => (REACH_SIGMAS * f64::from(sigma * scale)).ceil() as f32,
                false => 0.0,
            };
        match step {
            Step::Blur(sigma) => {
                Spread::Grow(Vector::new(taps(*sigma, sx), taps(*sigma, sy)))
            }
            Step::Colour(_) => Spread::Grow(Vector::default()),
            // A pixel either side for an offset off the pixel grid, which
            // the shadow is moved by with linear sampling.
            Step::Shadow { offset, sigma, .. } => Spread::Cast {
                grow: Vector::new(
                    taps(*sigma, sx) + 1.0,
                    taps(*sigma, sy) + 1.0,
                ),
                by: Vector::new(offset.x * sx, offset.y * sy),
            },
            Step::Image { filter, .. } => {
                Spread::Grow(image_reach(filter, layer))
            }
        }
    }

    /// `rect` after the step: where what lay in it can lie.
    fn forward(self, rect: Rect) -> Rect {
        match self {
            Spread::Grow(grow) => rect.with_outset(grow),
            Spread::Cast { grow, by } => {
                Rect::join2(rect, rect.with_outset(grow).with_offset(by))
            }
        }
    }

    /// `rect` before the step: where what lands in it can come from.
    fn backward(self, rect: Rect) -> Rect {
        match self {
            Spread::Grow(grow) => rect.with_outset(grow),
            Spread::Cast { grow, by } => {
                Rect::join2(rect, rect.with_offset(-by).with_outset(grow))
            }
        }
    }
}

/// The pixels a chain of `steps` works on, given what is `painted` and the
/// `clip`: at every stage, where what is painted can lie by then and where
/// what is shown can still come from, met and joined. That holds the
/// pixels a drop shadow brings onto the page from past it, where nothing
/// is painted and nothing is shown, and no farther: a shadow moved 1e38
/// pixels away meets nothing shown, where the reach would be 1e38 either
/// way.
///
/// `None` where nothing meets, or where the region is past what a layer's
/// size can hold, as only steps on either side of a stage that each reach
/// that far ask for -- two blurs of 1e38, say, which leave nothing a level
/// could show; the chain then works on what is drawn.
fn worked_on(
    steps: &[Step],
    layer: &Matrix,
    painted: Rect,
    clip: Rect,
) -> Option<Rect> {
    let spreads: Vec<Spread> =
        steps.iter().map(|step| Spread::of(step, layer)).collect();
    /// `start`, then where it lies after each of `order` in turn.
    fn stages(
        start: Rect,
        step: fn(Spread, Rect) -> Rect,
        order: impl Iterator<Item = Spread>,
    ) -> Vec<Rect> {
        std::iter::once(start)
            .chain(order.scan(start, move |at, spread| {
                *at = step(spread, *at);
                Some(*at)
            }))
            .collect()
    }
    let lies = stages(painted, Spread::forward, spreads.iter().copied());
    let mut comes =
        stages(clip, Spread::backward, spreads.iter().rev().copied());
    comes.reverse();
    let meet = |mut a: Rect, b: Rect| a.intersect(b).then_some(a);
    // Within a drop shadow, its blur before the offset moves it: what is
    // painted grown, and what is shown moved back.
    let shadows = spreads.iter().enumerate().filter_map(|(i, spread)| {
        let Spread::Cast { grow, by } = *spread else {
            return None;
        };
        meet(lies[i].with_outset(grow), comes[i + 1].with_offset(-by))
    });
    let worked = lies
        .iter()
        .zip(&comes)
        .filter_map(|(lies, comes)| meet(*lies, *comes))
        .chain(shadows)
        .reduce(|mut all, rect| {
            all.join(rect);
            all
        })?;
    // Half of `i32::MAX`, so that a width across the origin still fits.
    let holds = |at: f32| at.is_finite() && at.abs() < (i32::MAX / 2) as f32;
    [worked.left, worked.top, worked.right, worked.bottom]
        .into_iter()
        .all(holds)
        .then_some(worked)
}

/// How far, in canvas pixels along each axis, the chain's steps together
/// carry a pixel of a draw made under `drawn_under`: what lies farther than
/// this outside what is shown cannot reach it. A chain run in the draw's
/// own space -- see [`Step::runs_in_local_space`] -- reaches as far as its
/// lengths are stretched by the larger of the scales `drawn_under` makes,
/// which bounds them along any axis it turns them to.
pub(crate) fn canvas_reach(steps: &[Step], drawn_under: &Matrix) -> Vector {
    let local = steps.iter().any(Step::runs_in_local_space);
    let stretch = drawn_under
        .decompose_scale(None)
        .filter(|_| local)
        .map_or(1.0, |scale| scale.width.max(scale.height));
    reach(steps, &Matrix::scale((stretch, stretch)))
}

/// How far, in layer pixels along each axis, the chain's blurs and
/// shadows together carry a pixel: its blurs' reach, and as far again as
/// each shadow moves along that axis.
fn reach(steps: &[Step], layer: &Matrix) -> Vector {
    let (sx, sy) = (layer.scale_x().abs(), layer.scale_y().abs());
    let sigma = steps
        .iter()
        .filter_map(|step| match step {
            Step::Blur(sigma) | Step::Shadow { sigma, .. } => Some(*sigma),
            Step::Colour(_) | Step::Image { .. } => None,
        })
        .filter(|sigma| sigma.is_finite() && *sigma > 0.0)
        .fold(0.0, |sum, sigma| sum + sigma);
    let (moved_x, moved_y) = steps
        .iter()
        .filter_map(|step| match step {
            Step::Shadow { offset, .. } => Some(*offset),
            Step::Blur(_) | Step::Colour(_) | Step::Image { .. } => None,
        })
        .fold((0.0, 0.0), |(x, y), offset| {
            (x + offset.x.abs(), y + offset.y.abs())
        });
    let imaged = steps
        .iter()
        .filter_map(|step| match step {
            Step::Image { filter, .. } => Some(image_reach(filter, layer)),
            Step::Blur(_) | Step::Colour(_) | Step::Shadow { .. } => None,
        })
        .fold(Vector::default(), |sum, reach| sum + reach);
    let taps = |sigma: f32| (REACH_SIGMAS * f64::from(sigma)).ceil() as f32;
    Vector::new(
        taps(sigma * sx) + (moved_x * sx).ceil() + imaged.x,
        taps(sigma * sy) + (moved_y * sy).ceil() + imaged.y,
    )
}

/// How far, in layer pixels along each axis, `filter` carries a pixel at
/// the layer's scale: how far past a square of [`PROBE_SIDE`] pixels its
/// output reaches, as Skia bounds it.
fn image_reach(filter: &ImageFilter, layer: &Matrix) -> Vector {
    let square = IRect::from_wh(PROBE_SIDE, PROBE_SIDE);
    let scale = Matrix::scale((layer.scale_x().abs(), layer.scale_y().abs()));
    let out = filter.filter_bounds(square, &scale, MapDirection::Forward, None);
    let x = (-out.left).max(out.right - PROBE_SIDE).max(0);
    let y = (-out.top).max(out.bottom - PROBE_SIDE).max(0);
    Vector::new(x as f32, y as f32)
}

/// The side of the square [`image_reach`] asks Skia to bound: any square
/// does for a filter whose reach does not depend on what it is handed, and
/// one larger than the reach keeps an offset from reading as a shrink.
const PROBE_SIDE: i32 = 1024;

/// The layer's pixels: the canvas's own colour type and space, or half
/// floats in its space where `depth` is [`Depth::Halves`] and the canvas
/// holds no floats; or for a canvas with none, `depth`'s type in no space
/// at all, which is what a recorded picture's own draws are in until it is
/// played.
fn layer_info(canvas: &SkCanvas, depth: Depth, region: IRect) -> ImageInfo {
    let own = canvas.image_info();
    let color_type = match (own.color_type(), depth) {
        (ColorType::Unknown, Depth::Bytes) => ColorType::N32,
        (ColorType::Unknown, Depth::Floats) => ColorType::RGBAF32,
        (ColorType::RGBAF16 | ColorType::RGBAF32, _) => own.color_type(),
        (_, Depth::Halves) => ColorType::RGBAF16,
        (color_type, Depth::Bytes | Depth::Floats) => color_type,
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
    #[cfg(test)]
    if context.is_some() {
        opened_offscreen();
    }
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

/// Whether `canvas` draws on Ganesh, whose layers and placement a chain on
/// it takes. A Graphite page's chains take the CPU's: their layers are in
/// memory -- see [`layer_surface`] -- so they are placed as the CPU's are and
/// kept as the CPU's are.
pub(crate) fn is_on_gpu(canvas: &SkCanvas) -> bool {
    match engine_of(canvas) {
        RenderingEngine::GPU => true,
        RenderingEngine::CPU | RenderingEngine::Graphite => false,
    }
}

/// Whether chains on `canvas` may share a layer: on Ganesh's Metal backend,
/// and on no other. A Graphite page has no recording context, and its chains,
/// on the CPU's route, share no more than the CPU's do.
///
/// Ganesh merges the draws of chains sharing a layer into one op where it
/// can -- `FillRRectOp` combines any two compatible rounded rectangles into
/// one instanced draw, writing each instance's data as it would alone. So a
/// shared layer is bytes-for-bytes a layer of its own only where a merged
/// draw rasterises as the same draw made alone. On Metal it does:
/// `a_rounded_rect_drawn_among_others_draws_as_it_does_alone` finds no
/// pixel apart in any of 12 rounded rectangles, and `shared_tests` none in
/// any chain. On Mesa's llvmpipe it does not: 4 of the same 12 land up to a
/// level apart at their corners, 4 to 8 pixels each, and 3 still do moved a
/// hundredth of a pixel off the grid; shared, 181 to 243 pixels of a page of
/// 12 blurred chains moved. A draw over the whole layer after each chain,
/// which stops the merge and changes no pixel, brings both to none. On an
/// NVIDIA GTX 1050 Ti, driver 580.178.04, it does not either: the same 4 of
/// 12 moved, and 7 of the sharing tests fail with sharing turned on. No
/// other Vulkan device has been measured, so none shares; #524 tracks
/// measuring them. Chains that share no layer still share their shadows'
/// passes -- see [`ShadowAtlas`].
// UPSTREAM: Mesa llvmpipe 25.0.7 -- unfiled -- worked around
// Re-check: cargo test a_rounded_rect_drawn_among_others_draws_as_it_does_alone
// run with `--ignored` in a `vulkan` build on llvmpipe, where it fails while
// the defect stands. Worked around rather than not: nothing corrects the merged
// draw, but the code takes another path because of it, and passing there is
// what would let Vulkan share.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn shares_layers(canvas: &SkCanvas) -> bool {
    #[cfg(test)]
    if UNSHARED_LAYERS.with(std::cell::Cell::get) {
        return false;
    }
    canvas.recording_context().is_some_and(|context| {
        context.backend() == gpu::ganesh::BackendApi::Metal
    })
}

/// Whether chains on `canvas` may share a layer: never, in a build without
/// a backend.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn shares_layers(_: &SkCanvas) -> bool {
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
    #[cfg(test)]
    if context.is_some() {
        opened_offscreen();
    }
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
            // A blur the GPU's passes cannot make -- onto a target Metal
            // has none of, F32, or with a pass longer than their loop -- is
            // blurred on the CPU, which every type and sigma reaches.
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
    // image in its own channel order, which a blur treats alike. The result
    // is written into the image's own pixels -- see [`baked_image`] -- with
    // no buffer between: two buffers its size alive at once, never three;
    // an 8-bit one is rounded into them as its columns are summed.
    match bytes {
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
            baked_image(&info, |out, stride| {
                exact_blur::blur_bytes_into(
                    pixels,
                    w,
                    h,
                    (sigma_x, sigma_y),
                    gain,
                    out,
                    stride,
                );
            })
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
            let blurred = exact_blur::blur(pixels, w, h, sigma_x, sigma_y);
            baked_image(&info, |out, stride| {
                out.chunks_mut(stride)
                    .zip(blurred.chunks(w * 4))
                    .for_each(|(row, line)| write_floats(row, line, gain));
            })
        }
    }
}

/// Writes `line`, floats, each scaled by `gain`, into the start of `row`, as
/// the floats' bytes in the machine's order.
fn write_floats(row: &mut [u8], line: &[f32], gain: f32) {
    row.as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(line)
        .for_each(|(out, v)| *out = (v * gain).to_ne_bytes());
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
    clear_unless_zeroed(&mut surface);
    let mut paint = Paint::default();
    paint.set_color_filter(filter);
    let canvas = surface.canvas();
    canvas.draw_image(image, (0, 0), Some(&paint));
    Some(surface.image_snapshot())
}

/// `image` over its shadow, scaled by `gain`, in `image`'s own pixel type:
/// its alpha in `colour`, blurred by the exact Gaussian of `sigma` along
/// rows and columns and moved by `offset`, all in layer pixels. With
/// `shadow_only`, the shadow by itself, without `image` over it -- see
/// [`Step::Shadow`].
///
/// Made in floats on the CPU -- the tint, the blur and the two composited
/// -- and rounded once into `image`'s type, by [`rounded_once`]: an
/// eight-bit tint rounded before the blur, or a blurred shadow rounded
/// before the image is drawn over it, would be rounded again on the way.
/// The shadow is moved unresampled where the offset is whole pixels, and
/// bilinearly otherwise.
///
/// Where `cast` is `Some`, the shadow is cast from its image instead, at its
/// alpha -- see [`Cast`] -- as a shadow in `colour` with that alpha as a
/// factor of its own, which is the same product: the shadow is the tint's
/// alpha times the blurred alpha it is cast from, and the run's alpha is a
/// third factor of it.
///
/// On a GPU an eight-bit layer's blurred shadow is made there instead, by
/// [`shadowed_on_gpu`], to the same bytes but on exact ties: made on the
/// CPU it costs a read of the layer back from the GPU each draw, which
/// waits for the GPU to finish everything before it. A shadow cast from
/// another image is made on the CPU.
#[allow(clippy::too_many_arguments)]
fn shadowed(
    mut context: Option<&mut Gpu>,
    image: &Image,
    cast: Option<(&Image, f32)>,
    offset: Vector,
    sigma: (f32, f32),
    colour: (Color4f, &ColorSpace),
    gain: f32,
    shadow_only: bool,
) -> Option<Image> {
    if let Some((from, alpha)) = cast {
        let (tint, space) = colour;
        let composed = shadow_composed(
            context.as_deref(),
            image,
            Some(from),
            offset,
            sigma,
            (Color4f::new(tint.r, tint.g, tint.b, tint.a * alpha), space),
            shadow_only,
        )?;
        return rounded_once(&composed, image.image_info(), gain);
    }
    if let Some(context) = context.as_deref_mut()
        && let Some(on_gpu) = shadowed_on_gpu(
            context,
            image,
            offset,
            sigma,
            colour,
            gain,
            shadow_only,
        )
    {
        return Some(on_gpu);
    }
    if shadow_only
        && offset == Vector::default()
        && let Some(alone) =
            shadow_of_alpha(context.as_deref(), image, sigma, colour, gain)
    {
        return Some(alone);
    }
    let composed = shadow_composed(
        context.as_deref(),
        image,
        None,
        offset,
        sigma,
        colour,
        shadow_only,
    )?;
    rounded_once(&composed, image.image_info(), gain)
}

/// [`shadowed`] by itself on an eight-bit layer, unmoved: a canvas's own
/// shadow, whose group is moved by its offset instead -- see
/// `Context2D::shadow_steps` -- made from the layer's alpha bytes. The
/// same sums [`shadow_composed`] makes, without what they are composed
/// with: an eight-bit alpha is read as the float the source drawn into
/// floats holds, the tint is [`tinted_alpha`]'s, and a shadow drawn onto a
/// cleared float surface comes back as itself, so the result is the same to
/// the bit. It is two fewer float copies of the layer and one fewer surface.
///
/// `None` for anything else, for [`shadow_composed`] to make.
fn shadow_of_alpha(
    context: Option<&Gpu>,
    image: &Image,
    sigma: (f32, f32),
    (colour, space): (Color4f, &ColorSpace),
    gain: f32,
) -> Option<Image> {
    let bytes = matches!(
        image.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    let blurred = sigma.0 > 0.0 || sigma.1 > 0.0;
    if !(bytes && blurred) {
        return None;
    }
    let raster = to_raster(context, image)?;
    let named = tint_colour(colour, space, raster.image_info());
    let pixmap = raster.peek_pixels()?;
    let (w, h) = (raster.width() as usize, raster.height() as usize);
    let row = pixmap.row_bytes();
    let pixels = pixmap.bytes()?;
    // The alpha is the fourth byte of a pixel in either byte order, made a
    // float as Skia's raster pipeline loads a byte, by a multiply.
    let alpha: Vec<f32> = pixels
        .chunks(row)
        .take(h)
        .flat_map(|line| line[..w * 4].as_chunks::<4>().0.iter())
        .map(|px| f32::from(px[3]) * (1.0 / 255.0))
        .collect();
    let alpha = exact_blur::blur_alpha(alpha, w, h, sigma.0, sigma.1);
    let info = raster
        .image_info()
        .with_color_type(ColorType::RGBAF32)
        .with_alpha_type(AlphaType::Premul);
    // Tinted as `shadow_composed` tints: by `tinted_alpha` where the tint
    // is named, and otherwise the alpha alone through Skia's blend.
    let pixel = |a: f32| match named {
        Some((rgb, tint_alpha)) => tinted_alpha(rgb, tint_alpha, a),
        None => [0.0, 0.0, 0.0, a],
    };
    let shadow = baked_image(&info, |out, stride| {
        out.chunks_mut(stride).zip(alpha.chunks(w)).for_each(
            |(line, values)| {
                line.as_chunks_mut::<16>()
                    .0
                    .iter_mut()
                    .zip(values)
                    .for_each(|(px, &a)| {
                        px.as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(pixel(a))
                            .for_each(|(out, v)| *out = v.to_ne_bytes());
                    });
            },
        );
    })?;
    let shadow = match named {
        Some(_) => shadow,
        None => filtered(None, &shadow, &tint(named, colour, space)?, 1.0)?,
    };
    rounded_once(&shadow, image.image_info(), gain)
}

/// [`shadowed`] made on the GPU `image` lives on, through
/// [`exact_blur_gpu::shadow`], or `None` for the CPU to make as before:
///
/// - an unblurred shadow, which is a tint and a composite alone;
/// - a layer not of eight bits a channel, whose floats the GPU's targets do not
///   hold;
/// - a tint [`tint_colour`] cannot name, in a space this crate does not
///   convert, which [`tint`] leaves to Skia's blend;
/// - and wherever [`exact_blur_gpu::shadow`] gives `None`: a blur reaching past
///   four times its line, a pass of more taps than its loop takes, an effect
///   that does not compile, or a target that cannot be made.
///
/// The offset and sigma come in layer pixels, the chain's scale already
/// applied, so no transform reaches the pass.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn shadowed_on_gpu(
    context: &mut Gpu,
    image: &Image,
    offset: Vector,
    sigma: (f32, f32),
    (colour, space): (Color4f, &ColorSpace),
    gain: f32,
    shadow_only: bool,
) -> Option<Image> {
    let blurred = sigma.0 > 0.0 || sigma.1 > 0.0;
    let bytes = matches!(
        image.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    if !(blurred && bytes) {
        return None;
    }
    let tint = tint_colour(colour, space, image.image_info())?;
    let size = (image.width() as usize, image.height() as usize);
    exact_blur_gpu::shadow(
        context,
        image,
        size,
        sigma,
        (offset.x, offset.y),
        tint,
        gain,
        shadow_only,
    )
}

/// [`shadowed`] on a build with no GPU backend: never.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn shadowed_on_gpu(
    context: &mut Gpu,
    _: &Image,
    _: Vector,
    _: (f32, f32),
    _: (Color4f, &ColorSpace),
    _: f32,
    _: bool,
) -> Option<Image> {
    match *context {}
}

/// [`shadowed`]'s image over its shadow, or the shadow by itself where
/// `shadow_only`, in floats, before it is rounded. The shadow is cast from
/// `cast` where it is `Some` -- see [`Cast`] -- an image of the same size,
/// type and space as `image`, and from `image` otherwise.
///
/// On an eight-bit layer the shadow is the alpha it is cast from alone,
/// blurred in f32 and then tinted -- by [`tinted_alpha`] where [`tint_colour`]
/// names the tint, and through Skia's blend otherwise. The tint is the
/// colour scaled by the alpha, and a blur is linear, so the two orders
/// agree but for float rounding, which the bytes the result is rounded to
/// show only where a value lies on a tie. It is a quarter of the sums of
/// blurring the tinted source's four channels, each in f32 rather than f64.
/// A layer of floats keeps that blur, so that its values stay the floats
/// nearest the exact sums -- see [`exact_blur::blur`].
fn shadow_composed(
    context: Option<&Gpu>,
    image: &Image,
    cast: Option<&Image>,
    offset: Vector,
    sigma: (f32, f32),
    (colour, space): (Color4f, &ColorSpace),
    shadow_only: bool,
) -> Option<Image> {
    let raster = to_raster(context, image)?;
    let floats = raster.image_info().with_color_type(ColorType::RGBAF32);
    let on_floats = |draw: &dyn Fn(&SkCanvas)| {
        let mut surface = started::raster(&floats, None, None)?;
        clear_unless_zeroed(&mut surface);
        draw(surface.canvas());
        Some(surface.image_snapshot())
    };
    // An image in floats: the raster itself where it already is them,
    // premultiplied, as an F32 page's layer is -- drawn onto a cleared
    // surface of its own kind it would come back as itself, each value
    // plus nothing -- and drawn into floats otherwise.
    let in_floats = |raster: &Image| match raster.color_type()
        == ColorType::RGBAF32
        && raster.alpha_type() == AlphaType::Premul
    {
        true => Some(raster.clone()),
        false => on_floats(&|canvas| {
            canvas.draw_image(raster, (0, 0), None);
        }),
    };
    let source = in_floats(&raster)?;
    let from = match cast {
        Some(cast) => in_floats(&to_raster(context, cast)?)?,
        None => source.clone(),
    };
    let named = tint_colour(colour, space, raster.image_info());
    let tint = || tint(named, colour, space);
    let blurred = sigma.0 > 0.0 || sigma.1 > 0.0;
    let bytes = matches!(
        image.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    let shadow = match (blurred, bytes) {
        (true, true) => match named {
            Some((rgb, alpha)) => {
                blurred_alpha(&from, sigma, |a| tinted_alpha(rgb, alpha, a))?
            }
            None => {
                let alpha =
                    blurred_alpha(&from, sigma, |a| [0.0, 0.0, 0.0, a])?;
                filtered(None, &alpha, &tint()?, 1.0)?
            }
        },
        (true, false) => {
            let tinted = filtered(None, &from, &tint()?, 1.0)?;
            blurred_on_cpu(&tinted, sigma.0, sigma.1, 1.0)?
        }
        (false, _) => filtered(None, &from, &tint()?, 1.0)?,
    };
    let sampling = match offset.x.fract() == 0.0 && offset.y.fract() == 0.0 {
        true => SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
        false => SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
    };
    // Composed by Skia, even at a whole-pixel offset where src-over is one
    // multiply-add a channel: done here, it has to round as Skia's raster
    // pipeline does on every machine this ships to, and it does not. On
    // arm64 `d.mul_add(1 - sa, s)` matched Skia's F32 src-over in all of
    // 49,152 values and `s + d * (1 - sa)` missed 4,966; on x86 Skia picks
    // its stages at run time, fused or not as the processor allows, so no
    // one formula matches it there.
    let composed = on_floats(&|canvas| {
        canvas.draw_image_with_sampling_options(
            &shadow,
            (offset.x, offset.y),
            sampling,
            None,
        );
        if !shadow_only {
            canvas.draw_image(&source, (0, 0), None);
        }
    })?;
    Some(composed)
}

/// `source`'s alpha, blurred by `sigma` along rows and columns -- see
/// [`exact_blur::blur_alpha`] -- as a premultiplied F32 image of the
/// source's size and space, each pixel `pixel` of its blurred alpha: that
/// alpha alone, for a tint to colour, or the tint itself, by
/// [`tinted_alpha`].
fn blurred_alpha(
    source: &Image,
    sigma: (f32, f32),
    pixel: impl Fn(f32) -> [f32; 4],
) -> Option<Image> {
    let (w, h) = (source.width() as usize, source.height() as usize);
    let info = source
        .image_info()
        .with_color_type(ColorType::RGBAF32)
        .with_alpha_type(AlphaType::Premul);
    let row_bytes = w * 4 * size_of::<f32>();
    let mut pixels = vec![0f32; w * h * 4];
    if !source.read_pixels(
        &info,
        &mut pixels,
        row_bytes,
        (0, 0),
        CachingHint::Disallow,
    ) {
        return None;
    }
    let alpha: Vec<f32> =
        pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    drop(pixels);
    let alpha = exact_blur::blur_alpha(alpha, w, h, sigma.0, sigma.1);
    baked_image(&info, |out, stride| {
        out.chunks_mut(stride)
            .zip(alpha.chunks(w))
            .for_each(|(row, line)| {
                row.as_chunks_mut::<16>().0.iter_mut().zip(line).for_each(
                    |(px, &a)| {
                        px.as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(pixel(a))
                            .for_each(|(out, v)| *out = v.to_ne_bytes());
                    },
                );
            });
    })
}

/// The pixel [`offset_tint`] of `rgb` at `tint_alpha` makes of a
/// premultiplied F32 pixel holding nothing but `alpha`, drawn onto a
/// cleared F32 surface by Skia's raster pipeline, as [`filtered`] draws
/// it -- computed here, to the bit, rather than drawn:
///
/// - the tint's alpha times `alpha`, held to `[0, 1]`, as Skia's draw holds the
///   alpha its colour matrix makes: [`exact_blur::blur_alpha`] leaves its
///   values to the rounding to bytes to hold, so through the FFT some lie up to
///   about 1e-7 below zero, and some blurs reach 1.0000002;
/// - each colour times that product, one rounding a channel -- not the colour
///   times the tint's alpha first, which rounds differently in two of five
///   pixels that test sweeps;
/// - and `+ 0.0`, the source-over onto zeros the draw ends with, which leaves
///   every value as it is but a negative zero, which it makes positive: a
///   colour below zero, outside the layer's gamut, over an alpha of zero.
///
/// Held to Skia's draw by `a_tint_into_floats_is_one_product_a_channel`,
/// which CI runs on arm64 and x86-64 alike, since Skia picks its stages at
/// run time on x86. An alpha of -0.0 is the one input the two part on:
/// Skia's draw makes NaN of every channel of it, and this the zeros any
/// other zero alpha tints to.
fn tinted_alpha([r, g, b]: [f32; 3], tint_alpha: f32, alpha: f32) -> [f32; 4] {
    let a = (tint_alpha * alpha).clamp(0.0, 1.0);
    [r * a, g * a, b * a, a].map(|c| c + 0.0)
}

/// The colour filter a shadow in `colour`, unpremultiplied in `space`, is
/// tinted through onto a layer `named` is the [`tint_colour`] of: the
/// colour, in the alpha of what it is tinting.
///
/// A layer the page places its colours on -- eight bits a channel in a
/// space other than sRGB, see [`exact_fill`] -- takes the colour a fill of
/// it paints there, read back from a pixel filled with it, so that a
/// shadow's bytes are the placed fill's. Any other layer takes the colour
/// as named, converted exactly by [`exact_tint`] where the layer is in a
/// space this crate converts, and through Skia's blend otherwise: on an
/// eight-bit sRGB layer [`rounded_once`] then rounds the product of the
/// colour and its alpha once, on either engine, where a pixel filled on the
/// raster backend rounds the colour to bytes and premultiplies the bytes --
/// the shadow of `color(srgb-linear 0.25 0.25 0.25 / 0.5)` stored 69 that
/// way, where the product is 68.
///
/// The placed colour and the exactly converted one reach Skia through
/// [`offset_tint`], which it does not convert. Held by Skia's blend filter
/// instead, the read-back colour was converted from the layer's space into
/// that same space through skcms's approximate transfer function: on an
/// eight-bit Display P3 layer, read-back white tinted 1.54e-5 above its
/// alpha, and a white shadow stored a colour byte one above its alpha
/// byte, `[42, 42, 41, 41]`.
fn tint(
    named: Option<([f32; 3], f32)>,
    colour: Color4f,
    space: &ColorSpace,
) -> Option<ColorFilter> {
    match named {
        Some((rgb, alpha)) => Some(offset_tint(rgb, alpha)),
        None => color_filters::blend_with_color_space(
            colour,
            Some(space.clone()),
            BlendMode::SrcIn,
        ),
    }
}

/// The colour [`tint`] paints through [`offset_tint`], unpremultiplied in
/// `layer`'s space, and the alpha it paints at: the colour a fill places,
/// on a layer [`exact_fill`] places colours on, and the colour converted
/// by [`exact_tint`] otherwise. `None` where neither applies, which [`tint`]
/// blends through Skia instead.
fn tint_colour(
    colour: Color4f,
    space: &ColorSpace,
    layer: &ImageInfo,
) -> Option<([f32; 3], f32)> {
    let bytes = matches!(
        layer.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    let placed = layer
        .color_space()
        .is_some_and(|working| keeps_two_recordings(&working));
    if !(bytes && placed) {
        return exact_tint(colour, space, layer);
    }
    let pixel = layer.with_dimensions((1, 1));
    let mut surface = started::raster(&pixel, None, None)?;
    surface.canvas().clear(Color::TRANSPARENT);
    let mut fill = Paint::new(colour, Some(space));
    if let Some((shader, alpha)) = exact_fill(colour, space, layer) {
        fill.set_shader(shader).set_alpha_f(alpha);
    }
    surface.canvas().draw_paint(&fill);
    // Read back unpremultiplied, in floats, in the layer's own space: the
    // filled bytes divided out exactly enough that the tint multiplies them
    // back to the same bytes.
    let read = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF32,
        AlphaType::Unpremul,
        layer.color_space(),
    );
    let mut filled = [0f32; 4];
    if !surface.image_snapshot().read_pixels(
        &read,
        &mut filled,
        4 * size_of::<f32>(),
        (0, 0),
        CachingHint::Disallow,
    ) {
        return None;
    }
    let [r, g, b, a] = filled;
    Some(([r, g, b], a))
}

/// A shader painting `colour`, unpremultiplied in `space`, exactly on an
/// eight-bit page in `layer`'s space, where that page keeps a recording for
/// pixels and so paints a flat colour exactly -- see [`ExactColour`] -- and
/// the alpha it paints at: placed as a fill's colour is, so that it stores
/// a fill's bytes.
fn exact_fill(
    colour: Color4f,
    space: &ColorSpace,
    layer: &ImageInfo,
) -> Option<(Shader, f32)> {
    let working = layer.color_space()?;
    if !keeps_two_recordings(&working) {
        return None;
    }
    let canvas = PixelColorSpace::matching(&working)?;
    let rgb = color_in_canvas_space(colour, space, canvas)?;
    let (exact, alpha) =
        ExactColour::new(rgb, working, canvas, true).at(colour.a);
    Some((exact.shader()?, alpha))
}

/// [`tint_colour`] on a layer it does not place colours on, in a space this
/// crate converts: the colour converted to the layer's space in `f64` on
/// this side, for [`offset_tint`] to hand to Skia, and its alpha. `None`
/// where the layer has no space or one this crate does not convert.
fn exact_tint(
    colour: Color4f,
    space: &ColorSpace,
    layer: &ImageInfo,
) -> Option<([f32; 3], f32)> {
    let working = layer.color_space()?;
    let canvas = PixelColorSpace::matching(&working)?;
    let rgb = color_in_canvas_space(colour, space, canvas)?.map(|c| c as f32);
    Some((rgb, colour.a))
}

/// A tint painting `rgb`, unpremultiplied in the space it is drawn in, at
/// `alpha` of the alpha of what it tints: the colour handed to Skia as the
/// offsets of a colour matrix, which it does not convert. Skia converts a
/// colour a blend filter holds through skcms's approximate transfer
/// function, whatever space it is tagged with and even into that same
/// space: a shadow in `color(srgb-linear 0.5 0.5 0.5)` landed 6.29e-5 from
/// its encoding on an sRGB F32 page, and pre-encoded to `color(srgb ...)`
/// it landed the same.
fn offset_tint([r, g, b]: [f32; 3], alpha: f32) -> ColorFilter {
    #[rustfmt::skip]
    let matrix = [
        0.0, 0.0, 0.0, 0.0, r,
        0.0, 0.0, 0.0, 0.0, g,
        0.0, 0.0, 0.0, 0.0, b,
        0.0, 0.0, 0.0, alpha, 0.0,
    ];
    color_filters::matrix_row_major(&matrix, color_filters::Clamp::No)
}

/// `floats`, an F32 image, scaled by `gain` into `info`'s type in memory:
/// rounded once, by the rule [`exact_blur::blur_bytes`] rounds by, where
/// that is eight bits a channel, and kept as floats otherwise.
///
/// Converted here rather than drawn: Skia draws a float image onto an
/// eight-bit surface through fewer bits than a float, and a shadow at an
/// alpha of 0.3 -- 76.500003 levels -- came out 76 where a fill of the
/// colour is 77. Onto a GPU target the floats went through a narrower
/// texture first, and an image at half alpha read a level high at 807
/// pixels of a shadow.
///
/// The floats are read where they lie when `floats` holds them in memory as
/// premultiplied F32, as a snapshot of a raster surface does, and copied
/// out otherwise; the result is written into the image's own pixels -- see
/// [`baked_image`].
fn rounded_once(floats: &Image, info: &ImageInfo, gain: f32) -> Option<Image> {
    let (w, h) = (floats.width() as usize, floats.height() as usize);
    // As premultiplied F32, which is what a chain hands it and what the
    // copy below reads any other kind as.
    let float_info = floats
        .image_info()
        .with_alpha_type(AlphaType::Premul)
        .with_color_type(ColorType::RGBAF32);
    let peeked = floats.peek_pixels().filter(|pixmap| {
        pixmap.color_type() == ColorType::RGBAF32
            && pixmap.alpha_type() == AlphaType::Premul
    });
    let copied;
    let (source, stride): (&[u8], usize) = match &peeked {
        Some(pixmap) => (pixmap.bytes()?, pixmap.row_bytes()),
        None => {
            let row_bytes = w * 4 * size_of::<f32>();
            let mut pixels = vec![0u8; row_bytes * h];
            if !floats.read_pixels(
                &float_info,
                &mut pixels,
                row_bytes,
                (0, 0),
                CachingHint::Disallow,
            ) {
                return None;
            }
            copied = pixels;
            (&copied, row_bytes)
        }
    };
    // Each row of the source, as its pixels' four floats.
    let float_pixel = 4 * size_of::<f32>();
    let rows = source.chunks(stride).take(h).map(|row| {
        row[..w * float_pixel].as_chunks::<16>().0.iter().map(|p| {
            let channel = |c: usize| {
                f32::from_ne_bytes([
                    p[c * 4],
                    p[c * 4 + 1],
                    p[c * 4 + 2],
                    p[c * 4 + 3],
                ])
            };
            [channel(0), channel(1), channel(2), channel(3)]
        })
    });
    match info.color_type() {
        ColorType::RGBA8888 | ColorType::BGRA8888 => {
            let swap = info.color_type() == ColorType::BGRA8888;
            let level = |v: f32| (v * 255.0 * gain + 0.5) as u8;
            let info = info.with_alpha_type(AlphaType::Premul);
            baked_image(&info, |out, out_stride| {
                for (row, line) in out.chunks_mut(out_stride).zip(rows) {
                    for (px, [r, g, b, a]) in
                        row.as_chunks_mut::<4>().0.iter_mut().zip(line)
                    {
                        *px = match swap {
                            true => [level(b), level(g), level(r), level(a)],
                            false => [level(r), level(g), level(b), level(a)],
                        };
                    }
                }
            })
        }
        _ => baked_image(&float_info, |out, out_stride| {
            for (row, line) in out.chunks_mut(out_stride).zip(rows) {
                for (px, floats) in
                    row.as_chunks_mut::<16>().0.iter_mut().zip(line)
                {
                    for (channel, v) in
                        px.as_chunks_mut::<4>().0.iter_mut().zip(floats)
                    {
                        *channel = (v * gain).to_ne_bytes();
                    }
                }
            }
        }),
    }
}

/// `image` drawn through `filter`, given the layer's scale as its local
/// matrix so its lengths are the draw's own units, and scaled by `gain`,
/// onto a layer of its size and type: on its GPU where it lives on one, in
/// its own colour type, which keeps a float image float.
fn imaged(
    context: Option<&mut Gpu>,
    image: &Image,
    filter: &ImageFilter,
    layer: &Matrix,
    gain: f32,
) -> Option<Image> {
    let scale = Matrix::scale((layer.scale_x().abs(), layer.scale_y().abs()));
    let filter = filter.with_local_matrix(&scale)?;
    let mut surface = make_surface(context, image.image_info())?;
    clear_unless_zeroed(&mut surface);
    let mut paint = Paint::default();
    paint.set_image_filter(filter).set_alpha_f(gain);
    let canvas = surface.canvas();
    canvas.draw_image(image, (0, 0), Some(&paint));
    Some(surface.image_snapshot())
}

/// `image` in memory: read back from the GPU where it lives on one.
fn to_raster(context: Option<&Gpu>, image: &Image) -> Option<Image> {
    match context {
        #[cfg(any(feature = "metal", feature = "vulkan"))]
        Some(context) => {
            let mut direct = context.clone().as_direct_context();
            image.make_raster_image(direct.as_mut(), CachingHint::Disallow)
        }
        #[cfg(not(any(feature = "metal", feature = "vulkan")))]
        Some(never) => match *never {},
        None => Some(image.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::env::consts::ARCH;

    use skia_safe::{
        AlphaType, BlendMode as SkBlendMode, Canvas as SkCanvas, Color,
        Color4f, ColorSpace, ColorType, Data, FilterMode, IRect, Image,
        ImageInfo, Matrix, MipmapMode, Paint, PathBuilder, PictureRecorder,
        Rect, SamplingOptions, Surface, Vector, color_filters,
        image::{CachingHint, RequiredProperties},
        named_primaries, named_transfer_fn,
    };

    use super::{
        Depth, Outcome, REFUSE_PLACED, Step, TRIMMED, TRIMS_COMPOSITE, apply,
        blurred_alpha, blurred_on_cpu, exact_blur, filtered, offset_tint,
        rounded_once, run_cast, shadow_composed, shadowed, tint, tint_colour,
        tinted_alpha, worked_on,
    };
    #[cfg(all(meo_graphite, feature = "metal"))]
    use crate::{context::page::ExportOptions, gpu::graphite};
    use crate::{prelude::*, started};

    #[test]
    fn a_result_kept_as_its_inked_part_composites_as_the_whole() {
        // A result inked only at its centre, kept as that part and as the
        // whole image, composited onto a painted page: the same bytes under
        // a blend a transparent pixel leaves alone, drawn from the inked
        // part, and under ones where it clears what it covers, `copy` and
        // `destination-in`, drawn from the whole region padded back. The
        // canvas reaches those two through a layer of their own, so only a
        // result composited directly can tell.
        let info = ImageInfo::new_n32_premul((12, 10), None);
        let mut source = started::raster(&info, None, None).expect("a surface");
        let inked = IRect::new(4, 3, 8, 7);
        source.canvas().draw_rect(
            Rect::from(inked),
            &Paint::new(Color4f::new(0.8, 0.4, 0.2, 0.75), None),
        );
        let image = source.image_snapshot();
        let region = IRect::new(5, 6, 17, 16);
        let identity = Matrix::new_identity();
        let whole = Outcome::whole(image.clone(), region, identity);
        let part = image
            .make_subset(None, inked, RequiredProperties { mipmapped: false })
            .expect("a subset");
        let kept = Outcome::with_inked(part, region, identity, inked);
        let page = |outcome: &Outcome, blend: SkBlendMode, trims: bool| {
            TRIMS_COMPOSITE.set(trims);
            let info = ImageInfo::new_n32_premul((24, 20), None);
            let mut page = started::raster(&info, None, None).expect("a page");
            page.canvas().clear(Color::from_argb(255, 30, 140, 200));
            let mut paint = Paint::default();
            paint.set_blend_mode(blend);
            let before = TRIMMED.get();
            outcome.composite(page.canvas(), &paint);
            TRIMS_COMPOSITE.set(true);
            let mut bytes = vec![0u8; 24 * 20 * 4];
            assert!(page.read_pixels(&info, &mut bytes, 24 * 4, (0, 0)));
            (bytes, TRIMMED.get() - before)
        };
        for (blend, trimmed) in [
            (SkBlendMode::SrcOver, 1),
            (SkBlendMode::Src, 0),
            (SkBlendMode::DstIn, 0),
        ] {
            let (want, _) = page(&whole, blend, false);
            let (got, trims) = page(&kept, blend, true);
            assert!(got == want, "{blend:?}: a byte moved");
            assert_eq!(trims, trimmed, "{blend:?}");
            let (padded, _) = page(&kept, blend, false);
            assert!(padded == want, "{blend:?}: padded back, a byte moved");
        }
    }

    #[cfg(all(meo_graphite, feature = "metal"))]
    #[test]
    fn a_graphite_page_s_chains_work_on_layers_in_memory() {
        // A chain on a Graphite page takes the CPU's route: its layer is in
        // main memory, where the route's steps read it, and it is placed as
        // the CPU's layers are. One the page made would be a Graphite
        // texture, which none of those steps reads.
        if !graphite::supported() {
            eprintln!("skipped: no Graphite context on this machine");
            return;
        }
        let info = ImageInfo::new(
            (16, 16),
            ColorType::RGBAF32,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut page = graphite::make_surface(&info, &ExportOptions::default())
            .expect("a Graphite page");
        let canvas = page.canvas();
        assert!(!super::is_on_gpu(canvas), "placed as the CPU's");
        let (mut layer, key) =
            super::layer_surface(canvas, Depth::Floats, IRect::from_wh(8, 8))
                .expect("a layer");
        assert!(layer.peek_pixels().is_some(), "a layer in main memory");
        assert!(key.is_none(), "a layer in main memory is never kept");
    }

    #[test]
    fn a_raster_surface_is_made_of_zeros() {
        // What `clear_unless_zeroed` leaves a raster surface on: every one
        // made holds nothing but zero bits, in each colour type a chain
        // makes, small and past the allocator's large sizes, and where it
        // takes the memory a surface filled with white just let go -- which
        // a surface allocated but not zeroed would show.
        let (mut made, mut reused) = (0, 0);
        for colour in [
            ColorType::RGBA8888,
            ColorType::BGRA8888,
            ColorType::RGBAF16,
            ColorType::RGBAF32,
            ColorType::Alpha8,
        ] {
            for size in [(3, 2), (59, 56), (600, 400)] {
                let info =
                    ImageInfo::new(size, colour, AlphaType::Premul, None);
                let mut white =
                    started::raster(&info, None, None).expect("a surface");
                white.canvas().clear(Color::WHITE);
                let was = white.peek_pixels().expect("pixels").addr() as usize;
                drop(white);
                let mut fresh =
                    started::raster(&info, None, None).expect("a surface");
                let pixels = fresh.peek_pixels().expect("pixels");
                reused += usize::from(pixels.addr() as usize == was);
                let bytes = pixels.bytes().expect("bytes");
                assert!(
                    bytes.iter().all(|&b| b == 0),
                    "{colour:?} {size:?} made holding something"
                );
                made += 1;
            }
        }
        assert_eq!(made, 5 * 3);
        assert!(reused > 0, "no surface took the memory one let go");
    }

    #[test]
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn a_gpu_target_is_cleared_whatever_it_holds() {
        // A GPU target can hold what Skia's scratch texture held before it,
        // so `clear_unless_zeroed` clears it, whatever is there: a target
        // filled with white reads nothing but zeros after it. Without a GPU
        // there is no target to ask.
        use crate::{context::page::ExportOptions, gpu::RenderingEngine};
        if !RenderingEngine::GPU.selectable() {
            return;
        }
        let info = ImageInfo::new(
            (64, 48),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut page = RenderingEngine::GPU
            .make_surface(&info, &ExportOptions::default())
            .expect("a GPU page");
        let mut context = super::gpu_of(&mut page).expect("its context");
        let mut target =
            super::make_surface(Some(&mut context), &info).expect("a target");
        assert!(target.peek_pixels().is_none(), "a target in main memory");
        target.canvas().clear(Color::WHITE);
        super::clear_unless_zeroed(&mut target);
        let mut bytes = vec![0u8; info.compute_min_byte_size()];
        assert!(target.read_pixels(
            &info,
            &mut bytes,
            info.min_row_bytes(),
            (0, 0)
        ));
        assert!(bytes.iter().all(|&b| b == 0), "left holding white");
    }

    /// A drop shadow in black, moved by `(dx, dy)` and blurred by `sigma`.
    fn shadow(dx: f32, dy: f32, sigma: f32) -> Step {
        Step::Shadow {
            offset: Vector::new(dx, dy),
            sigma,
            colour: Color4f::new(0.0, 0.0, 0.0, 1.0),
            space: ColorSpace::new_srgb(),
            shadow_only: false,
        }
    }

    /// A triangle's bounds 15 pixels from the top of a 160 by 100 page.
    const NEAR_THE_TOP: Rect = Rect {
        left: 19.0,
        top: 14.0,
        right: 142.0,
        bottom: 86.0,
    };

    /// The 160 by 100 page [`NEAR_THE_TOP`] lies on.
    const PAGE: Rect = Rect {
        left: 0.0,
        top: 0.0,
        right: 160.0,
        bottom: 100.0,
    };

    /// How many alphas [`a_tint_into_floats_is_one_product_a_channel`]
    /// tints at a time: a 256 by 256 image of them.
    const SWEPT_SIDE: i32 = 256;

    #[test]
    fn a_float_layer_blurred_through_the_fft_colours_without_nan() {
        // A float layer blurred through the FFT, then coloured and then
        // given a drop shadow by Skia, as a chain of `blur()` and a colour
        // step or `drop-shadow()` runs them. This layer's blur leaves a few
        // alphas at zero where the FFT's round-off lands, and through Skia's
        // colour matrix a pixel of alpha -0.0 came out NaN in every channel,
        // which a page then draws in place of what it held. Where the
        // round-off signs a zero depends on the machine: on arm64 this
        // layer's alphas at two pixels, and on an x86-64 with AVX2 and FMA
        // no alpha of the 828 layers `the_fft_writes_no_negative_zero`'s two
        // were found among, so this fails without the fix on arm64 alone.
        // A -0.0 in a colour value, which is all x86-64 showed, came out of
        // the matrix as +0.0, NaN nowhere.
        let (w, h) = (144, 108);
        let info = ImageInfo::new(
            (w, h),
            ColorType::RGBAF32,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let pixels: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let inside = (72..120).contains(&x) && (10..46).contains(&y);
                let p = match inside {
                    true => [0.4f32, 0.4, 0.4, 1.0],
                    false => [0.0; 4],
                };
                p.into_iter().flat_map(f32::to_ne_bytes)
            })
            .collect();
        let layer = started::raster_from_data(
            &info,
            Data::new_copy(&pixels),
            info.min_row_bytes(),
        )
        .expect("a layer");
        let blurred = blurred_on_cpu(&layer, 6.0, 6.0, 1.0).expect("a blur");
        #[rustfmt::skip]
        let brighter = color_filters::matrix_row_major(
            &[
                1.2, 0.0, 0.0, 0.0, 0.0,
                0.0, 1.2, 0.0, 0.0, 0.0,
                0.0, 0.0, 1.2, 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ],
            color_filters::Clamp::Yes,
        );
        let coloured =
            filtered(None, &blurred, &brighter, 1.0).expect("coloured");
        let shadowed = shadow_composed(
            None,
            &blurred,
            None,
            Vector::new(2.0, 2.0),
            (1.0, 1.0),
            (Color4f::new(1.0, 0.0, 0.0, 1.0), &ColorSpace::new_srgb()),
            false,
        )
        .expect("a shadow");
        for (what, image) in [("coloured", &coloured), ("shadowed", &shadowed)]
        {
            let nan: Vec<usize> = bytes_of(image, image.image_info())
                .as_chunks::<16>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.as_chunks::<4>()
                        .0
                        .iter()
                        .any(|&c| f32::from_ne_bytes(c).is_nan())
                })
                .map(|(i, _)| i)
                .collect();
            assert!(nan.is_empty(), "{what}: NaN at pixels {nan:?}");
        }
    }

    #[test]
    fn a_tint_into_floats_is_one_product_a_channel() {
        // What Skia's raster pipeline makes of `offset_tint` over a
        // premultiplied F32 image holding nothing but an alpha, against
        // `tinted_alpha`, which an eight-bit shadow is tinted by in its
        // place: one product a channel, the tint's alpha times the image's
        // and then each colour times that. The other order is counted
        // beside it, as the control a sweep that cannot tell them apart
        // would fail. Run on every machine CI builds on: Skia picks its
        // stages at run time on x86.
        let n = (SWEPT_SIDE * SWEPT_SIDE) as usize;
        // Every byte's alpha, a dense spread of the floats a blur leaves,
        // and the ends: either zero, the smallest normal and subnormal, one
        // and the float under it.
        let mut alphas: Vec<f32> =
            (0..=255).map(|v| v as f32 / 255.0).collect();
        // What a blur's float rounding can leave either side of the alphas
        // it was handed: a little under zero and a little over one.
        alphas.extend((1..=32u8).flat_map(|k| {
            let k = f32::from(k);
            [-k * 1e-7, 1.0 + k * f32::EPSILON]
        }));
        let mut x = 0x9E37_79B9u32;
        while alphas.len() < n - 7 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            alphas.push((x >> 8) as f32 / (1u32 << 24) as f32);
        }
        alphas.extend([
            0.0,
            -0.0,
            f32::MIN_POSITIVE,
            f32::from_bits(1),
            1.0,
            1.0f32.next_down(),
            0.5,
        ]);
        let pixels: Vec<f32> =
            alphas.iter().flat_map(|&a| [0.0, 0.0, 0.0, a]).collect();
        let bytes: Vec<u8> =
            pixels.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let (mut compared, mut missed, mut missed_other) = (0usize, 0, 0);
        for space in [
            ColorSpace::new_srgb(),
            ColorSpace::new_srgb_linear(),
            ColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::IEC61966_2_1,
            )
            .expect("Display P3"),
        ] {
            let info = ImageInfo::new(
                (SWEPT_SIDE, SWEPT_SIDE),
                ColorType::RGBAF32,
                AlphaType::Premul,
                Some(space),
            );
            let image = started::raster_from_data(
                &info,
                Data::new_copy(&bytes),
                info.min_row_bytes(),
            )
            .expect("an image");
            // Placed colours, a byte over 255, and some that are not.
            let colours = (0..=255u8)
                .step_by(17)
                .map(|v| {
                    let c = f32::from(v) / 255.0;
                    [c, 1.0 - c, c / 3.0]
                })
                .chain([[0.1, 0.7, 0.999_999], [1e-30, 0.5, 2.0 / 3.0]])
                // Out of the layer's gamut, as a colour converted from a
                // wider space lands.
                .chain([[-0.3, 1.2, 0.5], [1.5, -0.01, 2.0]]);
            for rgb in colours {
                for tint_alpha in
                    [1.0, 0.5, 0.3, 1.0 / 255.0, 254.0 / 255.0, 0.7]
                {
                    let tinted = filtered(
                        None,
                        &image,
                        &offset_tint(rgb, tint_alpha),
                        1.0,
                    )
                    .expect("tinted");
                    let mut got = vec![0f32; n * 4];
                    assert!(tinted.read_pixels(
                        &info,
                        &mut got,
                        info.min_row_bytes(),
                        (0, 0),
                        CachingHint::Disallow,
                    ));
                    for (px, &a) in got.as_chunks::<4>().0.iter().zip(&alphas) {
                        let bits = |p: &[f32; 4]| p.map(f32::to_bits);
                        let want = tinted_alpha(rgb, tint_alpha, a);
                        // Skia makes NaN of a negative zero's pixel, and
                        // `tinted_alpha` the zeros a zero alpha tints to.
                        if a == 0.0 && a.is_sign_negative() {
                            assert_eq!(bits(&want), [0; 4], "-0.0 to zeros");
                            continue;
                        }
                        let held = (tint_alpha * a).clamp(0.0, 1.0);
                        let other = [
                            rgb[0] * tint_alpha * a,
                            rgb[1] * tint_alpha * a,
                            rgb[2] * tint_alpha * a,
                            held,
                        ]
                        .map(|c| c + 0.0);
                        missed += usize::from(bits(px) != bits(&want));
                        missed_other += usize::from(bits(px) != bits(&other));
                        compared += 1;
                    }
                }
            }
        }
        eprintln!(
            "tint sweep on {}: {compared} pixels, {missed} off one product a \
             channel, {missed_other} off the other order",
            ARCH
        );
        assert!(missed_other > 0, "the control: the orders differ somewhere");
        assert_eq!(missed, 0, "{missed} of {compared} off");
    }

    #[test]
    fn a_shadow_s_chain_works_on_its_blur_past_the_page() {
        // The shadow's rows 0 and 1 are its blur's rows -2 and -1.
        let steps = [shadow(3.0, 2.0, 8.0)];
        let id = Matrix::new_identity();
        let worked =
            worked_on(&steps, &id, NEAR_THE_TOP, PAGE).expect("a region");
        assert!(worked.top <= -2.0, "{worked:?}");
        assert!(worked.left <= -3.0, "{worked:?}");
    }

    #[test]
    fn a_shadow_moved_far_away_adds_nothing_to_what_a_chain_works_on() {
        let steps = [shadow(1e38, 2.0, 8.0)];
        let id = Matrix::new_identity();
        let worked =
            worked_on(&steps, &id, NEAR_THE_TOP, PAGE).expect("a region");
        assert!(worked.right < 200.0, "{worked:?}");
    }

    #[test]
    fn a_chain_past_any_layer_works_on_what_is_drawn() {
        // Two blurs each reaching past what an `i32` holds: the region
        // between them is that wide.
        let steps = [Step::Blur(1e38), Step::Blur(1e38)];
        let id = Matrix::new_identity();
        assert_eq!(worked_on(&steps, &id, NEAR_THE_TOP, PAGE), None);
    }

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

    #[test]
    fn a_shadow_cast_from_another_layer_takes_no_result_kept_for_another() {
        // One translucent square, its shadow cast first from one opaque
        // square and then from another, moved: the second is made, not
        // taken from what the first left -- see `ChainInput::cast_from` --
        // though the layer the chain draws is the same to the byte.
        use crate::context::kept::{FOUND, KEPT};
        let info = ImageInfo::new_n32_premul((120, 80), None);
        let cast_at = |x: f32| {
            let mut surface: Surface =
                started::raster(&info, None, None).expect("a surface");
            let canvas = surface.canvas();
            let mut translucent = Paint::default();
            translucent.set_color4f(Color4f::new(0.8, 0.3, 0.1, 0.6), None);
            let mut opaque = Paint::default();
            opaque.set_color4f(Color4f::new(0.8, 0.3, 0.1, 1.0), None);
            let shadow = Step::Shadow {
                offset: Vector::new(3.0, 2.0),
                sigma: 2.0,
                colour: Color4f::new(0.0, 0.0, 0.0, 1.0),
                space: ColorSpace::new_srgb(),
                shadow_only: false,
            };
            if let Some(outcome) = run_cast(
                canvas,
                &[shadow],
                Some(Rect::from_xywh(10.0, 10.0, 80.0, 50.0)),
                Depth::Floats,
                &Paint::default(),
                0.6,
                |layer| {
                    layer.draw_rect(
                        Rect::from_xywh(20.0, 20.0, 30.0, 30.0),
                        &translucent,
                    );
                },
                |layer| {
                    layer.draw_rect(
                        Rect::from_xywh(x, 20.0, 30.0, 30.0),
                        &opaque,
                    );
                },
            ) {
                outcome.composite(surface.canvas(), &Paint::default());
            }
            let mut pixels = vec![0u8; 120 * 80 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 120 * 4, (0, 0)));
            pixels
        };
        // Made first, with nothing taken: a result made later is filed
        // under its key whether or not results are taken.
        KEPT.set(false);
        let made = cast_at(24.0);
        KEPT.set(true);
        let first = cast_at(20.0);
        cast_at(20.0);
        let taken = FOUND.get().taken;
        assert_eq!(cast_at(20.0), first, "the result kept");
        assert!(FOUND.get().taken > taken, "the control: nothing was kept");
        assert_ne!(made, first, "the control: the casts made one shadow");
        assert_eq!(cast_at(24.0), made, "taken from the other cast");
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

    /// The pages a group's chain is held on: every float space that keeps a
    /// recording for pixels, and an sRGB float page and two eight-bit pages
    /// beside them, which record a draw as one picture.
    const CHAIN_PAGES: [(PixelDepth, PixelColorSpace); 7] = [
        (PixelDepth::F16, PixelColorSpace::DisplayP3),
        (PixelDepth::F32, PixelColorSpace::DisplayP3),
        (PixelDepth::F32, PixelColorSpace::Rec2020),
        (PixelDepth::F32, PixelColorSpace::SrgbLinear),
        (PixelDepth::F32, PixelColorSpace::Srgb),
        (PixelDepth::Uint8, PixelColorSpace::DisplayP3),
        (PixelDepth::Uint8, PixelColorSpace::Srgb),
    ];

    /// The side of the pages a chain is held on.
    const CHAIN_SIDE: (f32, f32) = (160.0, 120.0);

    const OSWALD: &str =
        "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

    /// A filter a draw's chain runs, set on the context.
    fn chain_filter(ctx: &mut Context2D, which: usize) {
        let colour = RgbaLinear::new_premultiplied(0.12, 0.2, 0.28, 0.4);
        match which {
            0 => ctx.set_image_filter(Some(
                &ImageFilter::drop_shadow(
                    5.0, 4.0, 3.0, 3.0, colour, None, None,
                )
                .expect("a shadow"),
            )),
            1 => ctx.set_image_filter(Some(
                &ImageFilter::blur(2.5, 2.5, None, None, None).expect("a blur"),
            )),
            _ => {
                ctx.set_filter_css(
                    "drop-shadow(4px 3px 2px rgb(0 40 200 / 0.6)) blur(1px)",
                )
                .expect("a CSS filter");
            }
        }
    }

    /// What is drawn through the chain: a fill off the pixel grid, a wide
    /// stroke, text whose ink passes its box, a fill a clip cuts, a run of
    /// draws in a `saveLayer`, and a canvas drawn in.
    fn chain_draws(ctx: &mut Context2D, which: usize) {
        ctx.set_fill_style_css("rgb(200 120 40 / 0.9)")
            .expect("a colour");
        ctx.set_stroke_style_css("rgb(20 160 90 / 0.8)")
            .expect("a colour");
        match which {
            0 => ctx.fill_rect(20.3, 17.6, 15.0, 12.0),
            1 => {
                ctx.set_line_width(9.0);
                ctx.stroke_rect(30.5, 30.5, 40.0, 30.0);
            }
            2 => {
                FontLibrary::new()
                    .use_paths(Some("ChainOswald"), [OSWALD])
                    .expect("Oswald");
                assert!(ctx.set_font_css("italic 40px ChainOswald"));
                ctx.fill_text("Wfg", 20.0, 70.0, None);
            }
            3 => {
                ctx.begin_path();
                ctx.rect(25.0, 20.0, 50.0, 40.0);
                ctx.clip(FillRule::NonZero);
                ctx.begin_path();
                ctx.move_to(10.0, 10.0);
                ctx.line_to(110.0, 35.0);
                ctx.line_to(40.0, 90.0);
                ctx.close_path();
                ctx.fill(FillRule::NonZero);
            }
            4 => {
                ctx.save_layer();
                ctx.fill_rect(15.0, 15.0, 20.0, 20.0);
                ctx.set_line_width(4.0);
                ctx.stroke_rect(60.0, 50.0, 30.0, 25.0);
                ctx.restore();
            }
            _ => {
                let mut source = Canvas::new(40.0, 30.0);
                let inner = source.context();
                inner
                    .set_fill_style_css("rgb(40 90 220 / 0.85)")
                    .expect("a colour");
                inner.fill_rect(2.0, 3.0, 30.0, 20.0);
                ctx.draw_canvas(&mut source, 50.5, 40.25);
            }
        }
    }

    /// The pixels of `page` with `draw` drawn through `filter`, read as F32
    /// premultiplied in the page's space, with the chain's layer bounded by
    /// what its group reaches, or by the clip alone where `unbounded`.
    fn chained(
        (depth, space): (PixelDepth, PixelColorSpace),
        gpu: bool,
        filter: usize,
        draw: usize,
        unbounded: bool,
    ) -> Vec<u8> {
        crate::context::layer::UNBOUNDED_CHAIN.with(|flag| flag.set(unbounded));
        let (width, height) = CHAIN_SIDE;
        let mut canvas = Canvas::with_options(
            width,
            height,
            CanvasOptions {
                color_type: depth,
                color_space: space,
                gpu,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        chain_filter(canvas.context(), filter);
        chain_draws(canvas.context(), draw);
        let pixels = canvas
            .context()
            .get_image_data_as(
                0.0,
                0.0,
                width,
                height,
                PixelExportOptions {
                    depth: PixelDepth::F32,
                    color_space: space,
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        crate::context::layer::UNBOUNDED_CHAIN.with(|flag| flag.set(false));
        pixels
    }

    /// The alpha of `canvas` at `(x, y)`, premultiplied in F32.
    fn alpha_at(
        canvas: &mut Canvas,
        space: PixelColorSpace,
        x: f32,
        y: f32,
    ) -> f32 {
        let data = canvas
            .context()
            .get_image_data_as(
                x,
                y,
                1.0,
                1.0,
                PixelExportOptions {
                    depth: PixelDepth::F32,
                    color_space: space,
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
            )
            .expect("a pixel");
        f32::from_le_bytes(data.pixels()[12..16].try_into().expect("an alpha"))
    }

    /// The alphas a draw through a chain lands where they can be worked out:
    /// a fill of `rgb(200 120 40 / 0.9)` over 20.3 to 35.3 by 17.6 to 29.6,
    /// through an unblurred CSS shadow at 0.6 moved by (4, 3), and through a
    /// CanvasKit `source-in` blend at 0.8. Each is the fill's own alpha
    /// there, as the page draws the fill through no filter, over the
    /// shadow's: inside the fill, on row 29 it covers in part -- 0.6 of it on
    /// the CPU, and what Ganesh's coverage makes of that on the GPU -- where
    /// the shadow alone lies, and where the two overlap. Held to the clip
    /// alone, the chain moved the edge's 0.54 to 0.5365 on an F32 sRGB page.
    #[test]
    fn a_chain_lands_the_alphas_its_draw_and_filter_make() {
        let shadow = 0.6 * 0.9;
        let over = |top: f32, beneath: f32| top + beneath * (1.0 - top);
        // (x, y, the shadow beneath the fill there.)
        let points = [
            (21.0, 29.0, 0.0),
            (22.0, 20.0, 0.0),
            (36.0, 25.0, shadow),
            (25.0, 22.0, shadow),
        ];
        let mut missed = vec![];
        for (depth, space) in CHAIN_PAGES {
            // An eight-bit page stores a level, and F16 about a thousandth.
            let tolerance = match depth {
                PixelDepth::Uint8 => 1.0 / 255.0,
                PixelDepth::F16 => 1e-3,
                _ => 1e-5,
            };
            for gpu in [false, true] {
                let page = |canvas: &mut Canvas| {
                    let ctx = canvas.context();
                    ctx.set_fill_style_css("rgb(200 120 40 / 0.9)")
                        .expect("a colour");
                    ctx.fill_rect(20.3, 17.6, 15.0, 12.0);
                };
                let (width, height) = CHAIN_SIDE;
                let options = CanvasOptions {
                    color_type: depth,
                    color_space: space,
                    gpu,
                    ..CanvasOptions::default()
                };
                let mut plain =
                    Canvas::with_options(width, height, options.clone())
                        .expect("a canvas");
                page(&mut plain);
                for blend in [false, true] {
                    let mut canvas =
                        Canvas::with_options(width, height, options.clone())
                            .expect("a canvas");
                    let ctx = canvas.context();
                    match blend {
                        false => {
                            ctx.set_filter_css(
                                "drop-shadow(4px 3px 0px rgb(0 40 200 / 0.6))",
                            )
                            .expect("a CSS filter");
                        }
                        true => ctx.set_image_filter(Some(
                            &ColorFilter::blend(
                                RgbaLinear::new_premultiplied(
                                    0.08, 0.16, 0.4, 0.8,
                                ),
                                BlendMode::SourceIn,
                            )
                            .and_then(|cf| {
                                ImageFilter::from_color_filter(cf, None, None)
                            })
                            .expect("a filter"),
                        )),
                    }
                    page(&mut canvas);
                    for (x, y, beneath) in points {
                        let drawn = alpha_at(&mut plain, space, x, y);
                        let want = match blend {
                            true => 0.8 * drawn,
                            false => over(drawn, beneath),
                        };
                        let got = alpha_at(&mut canvas, space, x, y);
                        if (got - want).abs() > tolerance {
                            missed.push(format!(
                                "{depth:?} {space:?} gpu {gpu} blend {blend} at \
                                 ({x}, {y}): {got} for {want}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            missed.is_empty(),
            "{} missed:\n{}",
            missed.len(),
            missed.join("\n")
        );
    }

    /// Where `alphas` of a `CHAIN_SIDE` page are not zero, as the first and
    /// last column and row.
    fn inked(alphas: &[f32]) -> Option<(usize, usize, usize, usize)> {
        let width = CHAIN_SIDE.0 as usize;
        alphas.iter().enumerate().filter(|(_, a)| **a != 0.0).fold(
            None,
            |found, (i, _)| {
                let (x, y) = (i % width, i / width);
                Some(match found {
                    None => (x, x, y, y),
                    Some((l, r, t, b)) => {
                        (l.min(x), r.max(x), t.min(y), b.max(y))
                    }
                })
            },
        )
    }

    /// A level of eight bits, and the float error a level read back from
    /// one carries.
    const LEVEL: f32 = 1.0 / 255.0 + 1e-6;

    /// A group's chain bounded by what its entries reach takes in all that
    /// it filters and all that it shows, on every page and both engines,
    /// through a CanvasKit drop shadow and blur and a CSS chain, for every
    /// kind of draw a group holds -- a stroke's width and a glyph's overhang
    /// included: it inks the pixels a chain the clip alone bounds inks, and
    /// lands within a level of it. Not to the bit: Skia's own blur makes
    /// values up to 0.9 of a level apart on a smaller layer, each as far
    /// from the exact Gaussian -- 0.48 of a level, through a blur of 2.5 on
    /// an F32 sRGB page -- and the clip-bounded chain moves an anti-aliased
    /// edge -- see `a_chain_lands_the_alphas_its_draw_and_filter_make`.
    #[test]
    fn a_chain_bounded_by_its_group_s_reach_takes_in_what_it_filters() {
        let alphas = |pixels: Vec<u8>| -> Vec<f32> {
            pixels
                .chunks(16)
                .map(|p| {
                    f32::from_le_bytes(p[12..16].try_into().expect("an alpha"))
                })
                .collect()
        };
        let mut missed = vec![];
        for page in CHAIN_PAGES {
            for gpu in [false, true] {
                for filter in 0..3 {
                    for draw in 0..6 {
                        let bounded =
                            alphas(chained(page, gpu, filter, draw, false));
                        let clip_bounded =
                            alphas(chained(page, gpu, filter, draw, true));
                        let apart = bounded
                            .iter()
                            .zip(&clip_bounded)
                            .map(|(a, b)| (a - b).abs())
                            .fold(0.0, f32::max);
                        if inked(&bounded) != inked(&clip_bounded)
                            || apart > LEVEL
                        {
                            missed.push(format!(
                                "{page:?} gpu {gpu} filter {filter} draw {draw}: inked \
                                 {:?} for {:?}, {apart} apart",
                                inked(&bounded),
                                inked(&clip_bounded)
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            missed.is_empty(),
            "{} missed:\n{}",
            missed.len(),
            missed.join("\n")
        );
    }

    /// A shadowed rect's chain on a page that keeps a recording for pixels
    /// works on as many pixels as on the sRGB page of its depth, which keeps
    /// one, and not on the whole page: the draw is filed as a pair, which a
    /// bound taken from a single picture alone did not know, and 50 of them
    /// took 1.75 s on F16 Display P3 against 26 ms on F16 sRGB.
    #[test]
    fn a_shadowed_draw_s_chain_on_a_dual_page_works_on_the_draw_alone() {
        let worked_on = |page: (PixelDepth, PixelColorSpace)| {
            super::LARGEST_REGION.with(|largest| largest.set(0));
            chained(page, false, 0, 0, false);
            super::LARGEST_REGION.with(std::cell::Cell::get)
        };
        for (dual, single) in [
            (
                (PixelDepth::F16, PixelColorSpace::DisplayP3),
                (PixelDepth::F16, PixelColorSpace::Srgb),
            ),
            (
                (PixelDepth::F32, PixelColorSpace::Rec2020),
                (PixelDepth::F32, PixelColorSpace::Srgb),
            ),
            (
                (PixelDepth::F32, PixelColorSpace::SrgbLinear),
                (PixelDepth::F32, PixelColorSpace::Srgb),
            ),
        ] {
            let (dual_pixels, single_pixels) =
                (worked_on(dual), worked_on(single));
            assert!(single_pixels > 0, "{single:?}: no chain ran");
            assert_eq!(
                dual_pixels, single_pixels,
                "{dual:?} against {single:?}"
            );
        }
    }
    /// A raster image of `kind` holding premultiplied noise, its pixels
    /// written through floats so that every kind holds the same scene.
    fn noise(w: i32, h: i32, kind: ColorType) -> Image {
        let floats = ImageInfo::new(
            (w, h),
            ColorType::RGBAF32,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut seed: u32 = 17;
        let mut unit = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let pixels: Vec<u8> = (0..w * h)
            .flat_map(|_| {
                let a = unit();
                [unit() * a, unit() * a, unit() * a, a]
            })
            .flat_map(f32::to_ne_bytes)
            .collect();
        let image = started::raster_from_data(
            &floats,
            Data::new_copy(&pixels),
            floats.min_row_bytes(),
        )
        .expect("an image");
        let info = floats.with_color_type(kind);
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        surface.canvas().draw_image(&image, (0, 0), None);
        surface.image_snapshot()
    }

    /// Every byte of `image` as `info` holds it.
    fn bytes_of(image: &Image, info: &ImageInfo) -> Vec<u8> {
        let row = info.min_row_bytes();
        let mut out = vec![0u8; row * image.height() as usize];
        assert!(image.read_pixels(
            info,
            &mut out,
            row,
            (0, 0),
            CachingHint::Disallow
        ));
        out
    }

    /// `rounded_once` as it was written through a copied buffer, for the
    /// in-place one to be held to.
    fn rounded_through_a_copy(
        floats: &Image,
        info: &ImageInfo,
        gain: f32,
    ) -> Vec<u8> {
        let float_info = floats
            .image_info()
            .with_alpha_type(AlphaType::Premul)
            .with_color_type(ColorType::RGBAF32);
        let pixels: Vec<f32> = bytes_of(floats, &float_info)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b))
            .collect();
        match info.color_type() {
            ColorType::RGBA8888 | ColorType::BGRA8888 => {
                let swap = info.color_type() == ColorType::BGRA8888;
                let level = |v: f32| (v * 255.0 * gain + 0.5) as u8;
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|&[r, g, b, a]| match swap {
                        true => [level(b), level(g), level(r), level(a)],
                        false => [level(r), level(g), level(b), level(a)],
                    })
                    .collect()
            }
            _ => pixels
                .iter()
                .flat_map(|v| (v * gain).to_ne_bytes())
                .collect(),
        }
    }

    #[test]
    fn a_result_written_in_place_holds_the_bits_a_copied_one_did() {
        let mut compared = 0;
        for (w, h) in [(1, 1), (7, 3), (59, 56)] {
            // F32, which is read where it lies, and F16, which is not and
            // is copied out as floats.
            for source in [ColorType::RGBAF32, ColorType::RGBAF16] {
                let floats = noise(w, h, source);
                for kind in [
                    ColorType::RGBA8888,
                    ColorType::BGRA8888,
                    ColorType::RGBAF32,
                ] {
                    for gain in [1.0f32, 0.6] {
                        let info = ImageInfo::new(
                            (w, h),
                            kind,
                            AlphaType::Premul,
                            ColorSpace::new_srgb(),
                        );
                        let ours = rounded_once(&floats, &info, gain)
                            .expect("an image");
                        let read = match kind {
                            ColorType::RGBAF32 => ours.image_info().clone(),
                            _ => info.clone(),
                        };
                        assert_eq!(
                            bytes_of(&ours, &read),
                            rounded_through_a_copy(&floats, &info, gain),
                            "{w}x{h} from {source:?} into {kind:?} at {gain}"
                        );
                        compared += 1;
                    }
                }
            }
            // Blurred, in floats and in bytes: the bits of the copying route,
            // its floats scaled by the gain and its bytes as they were.
            for kind in [ColorType::RGBAF32, ColorType::RGBA8888] {
                let image = noise(w, h, kind);
                let ours =
                    blurred_on_cpu(&image, 1.5, 2.5, 0.6).expect("an image");
                let info = ours.image_info();
                let theirs = match kind {
                    ColorType::RGBAF32 => {
                        let floats: Vec<f32> = bytes_of(&image, info)
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_ne_bytes(*b))
                            .collect();
                        exact_blur::blur(
                            floats, w as usize, h as usize, 1.5, 2.5,
                        )
                        .iter()
                        .flat_map(|v| (v * 0.6).to_ne_bytes())
                        .collect::<Vec<u8>>()
                    }
                    _ => exact_blur::blur_bytes(
                        bytes_of(&image, info),
                        w as usize,
                        h as usize,
                        1.5,
                        2.5,
                        0.6,
                    ),
                };
                assert_eq!(
                    bytes_of(&ours, info),
                    theirs,
                    "blurred {w}x{h} {kind:?}"
                );
                compared += 1;
            }
        }
        assert_eq!(compared, 3 * (2 * 3 * 2 + 2));
    }
    /// A layer of `kind` in `space` holding `scene`: 0 a shape with
    /// anti-aliased edges, 1 premultiplied noise -- written through floats
    /// and drawn into the layer's own kind, as a chain's layer holds a draw.
    fn layer(
        w: i32,
        h: i32,
        kind: ColorType,
        space: &ColorSpace,
        scene: u32,
    ) -> Image {
        let info =
            ImageInfo::new((w, h), kind, AlphaType::Premul, space.clone());
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        match scene {
            0 => {
                let mut paint = Paint::default();
                paint
                    .set_anti_alias(true)
                    .set_color4f(Color4f::new(0.8, 0.45, 0.15, 0.9), space);
                canvas.draw_round_rect(
                    Rect::from_xywh(
                        w as f32 * 0.3 + 0.4,
                        h as f32 * 0.3 + 0.7,
                        w as f32 * 0.4,
                        h as f32 * 0.35,
                    ),
                    2.5,
                    2.5,
                    &paint,
                );
            }
            _ => {
                canvas.draw_image(
                    noise(w, h, ColorType::RGBAF32),
                    (0, 0),
                    None,
                );
            }
        }
        surface.image_snapshot()
    }

    /// `shadowed` as it was drawn before its cuts: the source drawn into
    /// floats, tinted, blurred four channels to a pixel in f64, composed by
    /// Skia and rounded once -- every part of it a function it still calls.
    fn shadowed_as_drawn(
        image: &Image,
        offset: Vector,
        sigma: (f32, f32),
        colour: (Color4f, &ColorSpace),
        gain: f32,
    ) -> Option<Image> {
        let composed = composed_as_drawn(image, offset, sigma, colour)?;
        rounded_once(&composed, image.image_info(), gain)
    }

    /// [`shadowed_as_drawn`]'s floats, before they are rounded.
    fn composed_as_drawn(
        image: &Image,
        offset: Vector,
        sigma: (f32, f32),
        (colour, space): (Color4f, &ColorSpace),
    ) -> Option<Image> {
        let floats = image.image_info().with_color_type(ColorType::RGBAF32);
        let on_floats = |draw: &dyn Fn(&SkCanvas)| {
            let mut surface = started::raster(&floats, None, None)?;
            surface.canvas().clear(Color::TRANSPARENT);
            draw(surface.canvas());
            Some(surface.image_snapshot())
        };
        let source = on_floats(&|canvas| {
            canvas.draw_image(image, (0, 0), None);
        })?;
        let named = tint_colour(colour, space, image.image_info());
        let tint = tint(named, colour, space)?;
        let tinted = filtered(None, &source, &tint, 1.0)?;
        let shadow = match sigma.0 > 0.0 || sigma.1 > 0.0 {
            true => blurred_on_cpu(&tinted, sigma.0, sigma.1, 1.0)?,
            false => tinted,
        };
        let sampling = match offset.x.fract() == 0.0 && offset.y.fract() == 0.0
        {
            true => SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
            false => SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        };
        let composed = on_floats(&|canvas| {
            canvas.draw_image_with_sampling_options(
                &shadow,
                (offset.x, offset.y),
                sampling,
                None,
            );
            canvas.draw_image(&source, (0, 0), None);
        })?;
        Some(composed)
    }

    /// The layers, sigmas, offsets, colours and gains a shadow's sweep runs
    /// over: each float layer kind a chain plays on -- an F32 sRGB page's
    /// and an F16 Display P3 page's -- with a shape and with noise, at
    /// sigmas either side of the FFT's, whole, zero and fractional offsets,
    /// opaque and translucent colours, and two gains. An eight-bit layer's
    /// shadow is made another way, and held to this one to within a tie by
    /// `an_eight_bit_shadow_differs_from_its_tinted_blur_only_on_ties`.
    fn shadow_sweep(
        mut each: impl FnMut(
            &str,
            &Image,
            Vector,
            (f32, f32),
            (Color4f, &ColorSpace),
            f32,
        ),
    ) -> usize {
        let srgb = ColorSpace::new_srgb();
        let p3 = PixelColorSpace::DisplayP3
            .to_skia_color_space()
            .expect("Display P3");
        let kinds = [
            (ColorType::RGBAF32, &srgb, "F32 sRGB"),
            (ColorType::RGBAF16, &p3, "F16 P3"),
        ];
        let sigmas = [
            (0.5, 0.5),
            (1.0, 1.0),
            (2.0, 2.0),
            (3.0, 3.0),
            (5.0, 5.0),
            (8.0, 8.0),
            (1.5, 4.0),
        ];
        let offsets = [
            Vector::new(2.0, 2.0),
            Vector::new(0.0, 0.0),
            Vector::new(1.5, -2.25),
        ];
        let colours = [
            Color4f::new(0.0, 0.0, 0.0, 1.0),
            Color4f::new(0.78, 0.16, 0.35, 0.6),
            Color4f::new(0.1, 0.6, 0.9, 0.85),
        ];
        let mut runs = 0;
        for (kind, space, name) in kinds {
            for scene in 0..2 {
                let image = layer(40, 36, kind, space, scene);
                for sigma in sigmas {
                    for offset in offsets {
                        for colour in colours {
                            for gain in [1.0, 0.7] {
                                each(
                                    name,
                                    &image,
                                    offset,
                                    sigma,
                                    (colour, &srgb),
                                    gain,
                                );
                                runs += 1;
                            }
                        }
                    }
                }
            }
        }
        runs
    }

    #[test]
    fn a_shadow_keeps_every_bit_it_was_drawn_with() {
        let mut differing = Vec::new();
        let runs = shadow_sweep(|name, image, offset, sigma, colour, gain| {
            let ours =
                shadowed(None, image, None, offset, sigma, colour, gain, false)
                    .expect("a shadow");
            let theirs = shadowed_as_drawn(image, offset, sigma, colour, gain)
                .expect("a shadow");
            let info = ours.image_info();
            assert_eq!(
                info.color_type(),
                theirs.image_info().color_type(),
                "{name}"
            );
            if bytes_of(&ours, info) != bytes_of(&theirs, info) {
                differing.push(format!(
                    "{name} sigma {sigma:?} offset {offset:?} gain {gain}"
                ));
            }
        });
        assert_eq!(runs, 2 * 2 * 7 * 3 * 3 * 2);
        assert!(
            differing.is_empty(),
            "{} of {runs} differ, first {:?}",
            differing.len(),
            differing.first()
        );
    }

    /// The most an eight-bit layer's shadow, its alpha blurred in f32 and
    /// then tinted, may lie from the four channels of the tinted source
    /// blurred in f64, in levels of the byte it is rounded to: the 1e-4 of
    /// a level f32 holds a byte's blur to -- see [`exact_blur::blur_bytes`]
    /// -- a few ulps at full scale, where 1 ulp of 1.0 is 255 * 2^-23,
    /// about 3e-5 levels.
    const TIE_NOISE_LEVELS: f32 = 1e-4;

    #[test]
    fn an_eight_bit_shadow_differs_from_its_tinted_blur_only_on_ties() {
        let srgb = ColorSpace::new_srgb();
        let p3 = PixelColorSpace::DisplayP3
            .to_skia_color_space()
            .expect("Display P3");
        let kinds = [
            (ColorType::RGBA8888, &srgb, "8-bit sRGB"),
            (ColorType::BGRA8888, &srgb, "8-bit BGRA sRGB"),
            (ColorType::RGBA8888, &p3, "8-bit Display P3"),
        ];
        let sigmas = [
            (0.5, 0.5),
            (1.0, 1.0),
            (2.0, 2.0),
            (3.0, 3.0),
            (4.75, 4.75),
            (5.0, 5.0),
            (8.0, 8.0),
            (1.5, 4.0),
            (0.0, 3.0),
            (6.0, 0.0),
        ];
        let offsets = [
            Vector::new(2.0, 2.0),
            Vector::new(0.0, 0.0),
            Vector::new(1.5, -2.25),
        ];
        let colours = [
            Color4f::new(0.0, 0.0, 0.0, 1.0),
            Color4f::new(0.0, 0.0, 0.0, 0.5),
            Color4f::new(0.78, 0.16, 0.35, 0.6),
            Color4f::new(0.1, 0.6, 0.9, 0.85),
            Color4f::new(1.0, 1.0, 1.0, 0.3),
            Color4f::new(0.25, 0.25, 0.25, 0.5),
        ];
        let float_info = |image: &Image| {
            image
                .image_info()
                .with_color_type(ColorType::RGBAF32)
                .with_alpha_type(AlphaType::Premul)
        };
        let floats_of = |image: &Image| -> Vec<f32> {
            bytes_of(image, &float_info(image))
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_ne_bytes(b))
                .collect()
        };
        let (mut runs, mut values, mut flips, mut farthest) = (0, 0, 0, 0f32);
        let mut beyond = Vec::new();
        for (kind, space, name) in kinds {
            for (scene, (w, h)) in [(0, (40, 36)), (1, (40, 36)), (0, (7, 61))]
            {
                let image = layer(w, h, kind, space, scene);
                for sigma in sigmas {
                    for offset in offsets {
                        for colour in colours {
                            let ours = shadow_composed(
                                None,
                                &image,
                                None,
                                offset,
                                sigma,
                                (colour, &srgb),
                                false,
                            )
                            .expect("a shadow");
                            let theirs = composed_as_drawn(
                                &image,
                                offset,
                                sigma,
                                (colour, &srgb),
                            )
                            .expect("a shadow");
                            let (ours, theirs) =
                                (floats_of(&ours), floats_of(&theirs));
                            assert_eq!(ours.len(), theirs.len(), "{name}");
                            for gain in [1.0, 0.7] {
                                let level = |v: f32| v * 255.0 * gain + 0.5;
                                for (&a, &b) in ours.iter().zip(&theirs) {
                                    let gap = (level(a) - level(b)).abs();
                                    farthest = farthest.max(gap);
                                    if gap > TIE_NOISE_LEVELS {
                                        beyond.push(format!(
                                            "{name} sigma {sigma:?} offset \
                                             {offset:?} {a} against {b}"
                                        ));
                                    }
                                    if level(a) as u8 != level(b) as u8 {
                                        flips += 1;
                                    }
                                    values += 1;
                                }
                                runs += 1;
                            }
                        }
                    }
                }
            }
        }
        println!(
            "{flips} of {values} values in {runs} shadows round to another \
             byte; farthest apart {farthest} levels"
        );
        assert_eq!(runs, 3 * 3 * 10 * 3 * 6 * 2);
        assert!(
            beyond.is_empty(),
            "{} values lie further apart than a tie, first {:?}",
            beyond.len(),
            beyond.first()
        );
    }

    #[test]
    fn an_eight_bit_shadow_is_tinted_to_the_bit_skia_tints_it() {
        // An eight-bit layer's blurred alpha tinted by `tinted_alpha`,
        // against the same plane drawn by Skia through `offset_tint`: the
        // layers a page places its colours on and those it does not, a
        // colour outside the layer's gamut, and sigmas summed tap by tap and
        // through the FFT, so that the alphas a blur leaves under zero and
        // over one are among those tinted.
        let srgb = ColorSpace::new_srgb();
        let p3 = PixelColorSpace::DisplayP3
            .to_skia_color_space()
            .expect("Display P3");
        let kinds = [
            (ColorType::RGBA8888, &srgb, "8-bit sRGB"),
            (ColorType::BGRA8888, &srgb, "8-bit BGRA sRGB"),
            (ColorType::RGBA8888, &p3, "8-bit Display P3"),
        ];
        let colours = [
            (Color4f::new(0.0, 0.0, 0.0, 1.0), &srgb),
            (Color4f::new(0.78, 0.16, 0.35, 0.6), &srgb),
            (Color4f::new(1.0, 1.0, 1.0, 0.3), &srgb),
            // Outside sRGB's gamut: below zero in red and blue on an sRGB
            // layer.
            (Color4f::new(0.0, 1.0, 0.0, 0.8), &p3),
        ];
        let sigmas =
            [(1.0, 1.0), (3.0, 0.0), (5.0, 5.0), (8.0, 2.0), (0.0, 6.0)];
        // Layers of rects of grey, at random sizes and levels and some
        // opaque, beside the shared ones: an opaque rect blurs to alphas over
        // one, and the FFT's round-off leaves some under zero.
        let mut x = 0x1234_5679u32;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as usize
        };
        let mut blocks = |kind, space: &ColorSpace, layout: usize| {
            let (w, h) = (20 + next() % 140, 10 + next() % 120);
            let mut levels = vec![0u8; w * h];
            for _ in 0..=layout % 4 {
                let (x0, y0) = (next() % w, next() % h);
                let (x1, y1) = (
                    (x0 + 1 + next() % w).min(w),
                    (y0 + 1 + next() % h).min(h),
                );
                let level = match layout % 3 {
                    0 => 255,
                    _ => (next() % 256) as u8,
                };
                (y0..y1)
                    .for_each(|y| levels[y * w + x0..y * w + x1].fill(level));
            }
            let bytes: Vec<u8> = levels.iter().flat_map(|&v| [v; 4]).collect();
            let info = ImageInfo::new(
                (w as i32, h as i32),
                kind,
                AlphaType::Premul,
                space.clone(),
            );
            started::raster_from_data(
                &info,
                Data::new_copy(&bytes),
                info.min_row_bytes(),
            )
            .expect("an image")
        };
        let (mut tinted, mut under, mut over, mut outside) = (0, 0, 0, 0);
        for (kind, space, name) in kinds {
            for scene in 0..8 {
                let image = match scene {
                    0 | 1 => layer(40, 36, kind, space, scene as u32),
                    _ => blocks(kind, space, scene),
                };
                for sigma in sigmas {
                    let alpha =
                        blurred_alpha(&image, sigma, |a| [0.0, 0.0, 0.0, a])
                            .expect("an alpha");
                    let plane = floats_of_image(&alpha);
                    under += plane.iter().skip(3).step_by(4).any(|&a| a < 0.0)
                        as usize;
                    over += plane.iter().skip(3).step_by(4).any(|&a| a > 1.0)
                        as usize;
                    for (colour, colour_space) in colours {
                        let (rgb, tint_alpha) = tint_colour(
                            colour,
                            colour_space,
                            image.image_info(),
                        )
                        .expect("a tint this crate names");
                        outside += rgb.iter().any(|&c| c < 0.0) as usize;
                        let ours = blurred_alpha(&image, sigma, |a| {
                            tinted_alpha(rgb, tint_alpha, a)
                        })
                        .expect("a tint");
                        let theirs = filtered(
                            None,
                            &alpha,
                            &offset_tint(rgb, tint_alpha),
                            1.0,
                        )
                        .expect("a tint");
                        let bits = |image: &Image| -> Vec<u32> {
                            floats_of_image(image)
                                .iter()
                                .map(|v| v.to_bits())
                                .collect()
                        };
                        assert_eq!(
                            bits(&ours),
                            bits(&theirs),
                            "{name} scene {scene} sigma {sigma:?} {colour:?}"
                        );
                        tinted += 1;
                    }
                }
            }
        }
        assert_eq!(tinted, 3 * 8 * 5 * 4);
        assert!(under > 0, "no blur left an alpha under zero");
        assert!(over > 0, "no blur left an alpha over one");
        assert!(outside > 0, "no tint outside the layer's gamut");
    }

    /// Every float of `image`, a premultiplied F32 image, as it holds them.
    fn floats_of_image(image: &Image) -> Vec<f32> {
        bytes_of(image, image.image_info())
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_ne_bytes(b))
            .collect()
    }

    /// A layer of 8-bit floats, recovered in floats as the CPU composes it.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn floats_of(image: &Image) -> Vec<f32> {
        let info = image
            .image_info()
            .with_color_type(ColorType::RGBAF32)
            .with_alpha_type(AlphaType::Premul);
        bytes_of(image, &info)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_ne_bytes(b))
            .collect()
    }

    /// How far from a tie, in levels, a byte the GPU and the CPU round
    /// apart may lie: the GPU's passes carry about 22 bits and float
    /// arithmetic, the CPU's f32, so a value nearer a boundary than this
    /// may round either way -- `exact_blur_gpu`'s tests hold its blur to
    /// the same.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    const GPU_TIE_LEVELS: f64 = 1e-3;

    /// A shadow made by itself from a layer's alpha bytes lands the bytes
    /// the shadow the general route composes does, to the byte: on each
    /// eight-bit kind, through a named tint and through Skia's blend, at
    /// every sigma and gain.
    #[test]
    fn a_shadow_made_from_alpha_bytes_is_the_composed_one() {
        use super::shadow_of_alpha;
        let srgb = ColorSpace::new_srgb();
        let p3 = PixelColorSpace::DisplayP3
            .to_skia_color_space()
            .expect("Display P3");
        let kinds = [
            (ColorType::RGBA8888, &srgb),
            (ColorType::BGRA8888, &srgb),
            (ColorType::RGBA8888, &p3),
        ];
        let colours = [
            Color4f::new(0.0, 0.0, 0.0, 1.0),
            Color4f::new(0.78, 0.16, 0.35, 0.6),
            Color4f::new(1.0, 1.0, 1.0, 0.3),
        ];
        let mut compared = 0;
        for (kind, space) in kinds {
            for scene in [0, 1] {
                let image = layer(40, 36, kind, space, scene);
                let info = image.image_info();
                for sigma in [(0.5, 0.5), (2.0, 2.0), (6.0, 6.0), (1.5, 4.0)] {
                    for colour in colours {
                        for gain in [1.0, 0.7] {
                            let ours = shadow_of_alpha(
                                None,
                                &image,
                                sigma,
                                (colour, &srgb),
                                gain,
                            )
                            .expect("a shadow");
                            let composed = shadow_composed(
                                None,
                                &image,
                                None,
                                Vector::default(),
                                sigma,
                                (colour, &srgb),
                                true,
                            )
                            .expect("a shadow");
                            let theirs = rounded_once(&composed, info, gain)
                                .expect("rounded");
                            assert_eq!(
                                bytes_of(&ours, info),
                                bytes_of(&theirs, info),
                                "{kind:?} {scene} {sigma:?} {colour:?} {gain}"
                            );
                            compared += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(compared, 3 * 2 * 4 * 3 * 2);
    }

    #[cfg(any(feature = "metal", feature = "vulkan"))]
    #[test]
    fn an_eight_bit_gpu_shadow_is_the_cpus_but_on_ties() {
        use super::shadowed_on_gpu;
        use crate::{context::page::ExportOptions, gpu::RenderingEngine};
        use skia_safe::BlendMode as SkBlendMode;
        if !RenderingEngine::GPU.selectable() {
            // A Mac always has Metal, so there a missing GPU is a failure
            // and not a reason to pass; elsewhere -- CI's Linux runners --
            // the test says it did not run.
            if cfg!(target_os = "macos") {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        let srgb = ColorSpace::new_srgb();
        let p3 = PixelColorSpace::DisplayP3
            .to_skia_color_space()
            .expect("Display P3");
        let rec2020 = PixelColorSpace::Rec2020
            .to_skia_color_space()
            .expect("Rec. 2020");
        let kinds = [
            (ColorType::RGBA8888, &srgb, "8-bit sRGB"),
            (ColorType::BGRA8888, &srgb, "8-bit BGRA sRGB"),
            (ColorType::RGBA8888, &p3, "8-bit Display P3"),
            (ColorType::RGBA8888, &rec2020, "8-bit Rec. 2020"),
        ];
        let sigmas = [
            (0.5, 0.5),
            (2.0, 2.0),
            (3.0, 3.0),
            (4.75, 4.75),
            (6.0, 6.0),
            (8.0, 8.0),
            (1.5, 4.0),
            (0.0, 3.0),
        ];
        let offsets = [
            Vector::new(2.0, 2.0),
            Vector::new(0.0, 0.0),
            Vector::new(1.5, -2.25),
            // A first covered pixel whose taps reach before the image's
            // edge, which a draw holds to its edge pixels.
            Vector::new(0.25, 0.75),
        ];
        let colours = [
            Color4f::new(0.0, 0.0, 0.0, 1.0),
            Color4f::new(0.0, 0.0, 0.0, 0.5),
            Color4f::new(0.5, 0.5, 0.5, 1.0),
            Color4f::new(0.78, 0.16, 0.35, 0.6),
            Color4f::new(1.0, 1.0, 1.0, 0.3),
        ];
        let (mut runs, mut values, mut flips, mut nearest) = (0, 0, 0, 0f64);
        let mut to_cpu = 0;
        let mut wrong = Vec::new();
        for (kind, space, name) in kinds {
            for (scene, (w, h)) in [(0, (40, 36)), (1, (40, 36)), (0, (7, 61))]
            {
                let raster = layer(w, h, kind, space, scene);
                let info = raster.image_info();
                let mut up = RenderingEngine::GPU
                    .make_surface(info, &ExportOptions::default())
                    .expect("a GPU surface");
                let mut paint = Paint::default();
                paint.set_blend_mode(SkBlendMode::Src);
                up.canvas().draw_image(&raster, (0, 0), Some(&paint));
                let texture = up.image_snapshot();
                let mut context = up.recording_context().expect("a context");
                for sigma in sigmas {
                    for offset in offsets {
                        for colour in colours {
                            // The shadow under its image, and alone.
                            for (gain, shadow_only) in [
                                (1.0, false),
                                (0.7, false),
                                (1.0, true),
                                (0.7, true),
                            ] {
                                let Some(gpu) = shadowed_on_gpu(
                                    &mut context,
                                    &texture,
                                    offset,
                                    sigma,
                                    (colour, &srgb),
                                    gain,
                                    shadow_only,
                                ) else {
                                    // A blur reaching past four times its
                                    // line is the CPU's: the 7-wide layer
                                    // from a sigma of 4.75.
                                    assert!(sigma.0 * 6.0 > 28.0, "{name}");
                                    to_cpu += 1;
                                    continue;
                                };
                                let mut direct = context.as_direct_context();
                                let got = bytes_of(
                                    &gpu.make_raster_image(
                                        direct.as_mut(),
                                        CachingHint::Disallow,
                                    )
                                    .expect("read back"),
                                    info,
                                );
                                let composed = shadow_composed(
                                    None,
                                    &raster,
                                    None,
                                    offset,
                                    sigma,
                                    (colour, &srgb),
                                    shadow_only,
                                )
                                .expect("a shadow");
                                let floats = floats_of(&composed);
                                let want = bytes_of(
                                    &rounded_once(&composed, info, gain)
                                        .expect("rounded"),
                                    info,
                                );
                                let swap = kind == ColorType::BGRA8888;
                                for (i, (&g, &c)) in
                                    got.iter().zip(&want).enumerate()
                                {
                                    values += 1;
                                    if g == c {
                                        continue;
                                    }
                                    flips += 1;
                                    // The float the CPU rounded, in levels.
                                    let channel = match (swap, i % 4) {
                                        (true, 0) => 2,
                                        (true, 2) => 0,
                                        (_, c) => c,
                                    };
                                    let v =
                                        f64::from(floats[i / 4 * 4 + channel])
                                            * 255.0
                                            * f64::from(gain)
                                            + 0.5;
                                    let from_tie = (v - v.round()).abs();
                                    nearest = nearest.max(from_tie);
                                    if from_tie > GPU_TIE_LEVELS
                                        || g.abs_diff(c) > 1
                                    {
                                        wrong.push(format!(
                                            "{name} scene {scene} sigma \
                                             {sigma:?} offset {offset:?} \
                                             gain {gain}: {g} against {c}, \
                                             {from_tie} from a tie"
                                        ));
                                    }
                                }
                                runs += 1;
                            }
                        }
                    }
                }
            }
        }
        println!(
            "{flips} of {values} bytes in {runs} shadows round apart, the \
             farthest {nearest} levels from a tie"
        );
        // Four kinds, three scenes, eight sigmas, four offsets, five colours,
        // and two gains for the shadow under its image and two alone.
        assert_eq!(runs + to_cpu, 4 * 3 * 8 * 4 * 5 * 4);
        // The GPU made every shadow but those on the 7-wide layer at sigmas
        // 4.75, 6 and 8, on each of four kinds, which its reach refuses: a
        // sweep that fell back to the CPU throughout would compare the CPU
        // with itself.
        assert_eq!(to_cpu, 4 * 3 * 4 * 5 * 4);
        assert_eq!(runs, 4 * 3 * 8 * 4 * 5 * 4 - to_cpu);
        assert!(
            wrong.is_empty(),
            "{} bytes apart off a tie, first {:?}",
            wrong.len(),
            wrong.first()
        );
    }
}

/// A chain on the CPU whose region crosses the page's top or left edge,
/// held to drawing its input within the page as a chain inside it does.
#[cfg(test)]
mod origin_tests {
    use skia_safe::{
        AlphaType, Canvas as SkCanvas, Color, ColorType, IRect, Image,
        ImageInfo, Paint, PaintStyle, RRect, Rect, image::CachingHint,
    };

    use super::{Depth, Drawn, Step, layer_info, plan};
    use crate::started;

    /// The page every scene is drawn on.
    const PAGE: (i32, i32) = (600, 400);

    /// The sigma whose reach takes a shape's region past the page's edge
    /// where the shape only nears it.
    const BLUR: f32 = 6.0;

    /// The `k`th shape of a sweep that crosses the page's left edge, its
    /// top, or both, by 5 to 59 pixels and a fraction, at a place along the
    /// edge that varies.
    fn crossing(k: usize) -> Rect {
        let fraction = (k as f32 * 0.137) % 1.0;
        let (w, h) =
            (120.0 + (k % 3) as f32 * 17.3, 70.0 + (k % 4) as f32 * 9.1);
        let along = ((k * 7919) % 1000) as f32 / 1000.0;
        let over = 5.0 + (k % 7) as f32 * 9.0 + fraction;
        let (pw, ph) = (PAGE.0 as f32, PAGE.1 as f32);
        match k % 3 {
            0 => Rect::from_xywh(-over, along * (ph - h), w, h),
            1 => Rect::from_xywh(along * (pw - w), -over, w, h),
            _ => Rect::from_xywh(-over, -over * 0.7, w, h),
        }
    }

    /// The `k`th shape's draw: a filled rounded rectangle, a filled oval, or
    /// a stroked oval, anti-aliased.
    fn draw(canvas: &SkCanvas, rect: Rect, k: usize) {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(Color::from_argb(255, 20, 60, 200));
        match k % 4 {
            0 => {
                canvas.draw_rrect(RRect::new_rect_xy(rect, 10.0, 10.0), &paint);
            }
            1 => {
                canvas.draw_oval(rect, &paint);
            }
            _ => {
                paint.set_style(PaintStyle::Stroke).set_stroke_width(9.3);
                canvas.draw_oval(rect, &paint);
            }
        }
    }

    /// The pixels of `image` within `area`, as premultiplied 32-bit floats.
    fn pixels(image: &Image, area: IRect) -> Vec<u8> {
        let info = ImageInfo::new(
            area.size(),
            ColorType::RGBAF32,
            AlphaType::Premul,
            image.color_space(),
        );
        let mut pixels = vec![0u8; info.compute_min_byte_size()];
        assert!(image.read_pixels(
            &info,
            &mut pixels,
            info.min_row_bytes(),
            (area.left, area.top),
            CachingHint::Disallow,
        ));
        pixels
    }

    /// A chain on the CPU whose region crosses the page's top or left edge
    /// draws its input, within the page, in the bytes a layer starting at
    /// the device's origin holds -- the layer a chain inside the page draws
    /// into: Skia's anti-aliasing covers a curve by where on the raster it
    /// lies, so a layer starting before the origin draws it otherwise. In
    /// bytes and in half floats, over curves, corners and a stroke.
    #[test]
    fn a_chain_past_the_top_left_draws_its_input_as_the_page_does() {
        let mut page =
            started::raster(&ImageInfo::new_n32_premul(PAGE, None), None, None)
                .expect("a page");
        let canvas = page.canvas();
        let (mut crossed, mut apart) = (0, Vec::new());
        for depth in [Depth::Bytes, Depth::Halves] {
            for k in 0..84 {
                let rect = crossing(k);
                let steps = [Step::Blur(BLUR)];
                let Some(plan) = plan(canvas, &steps, Some(rect), depth) else {
                    continue;
                };
                if plan.drawn.left >= 0 && plan.drawn.top >= 0 {
                    continue;
                }
                crossed += 1;
                let drawn = Drawn::new(canvas, &[&plan], |_, layer| {
                    draw(layer, rect, k)
                })
                .expect("a layer");
                let from_origin =
                    IRect::from_ltrb(0, 0, plan.drawn.right, plan.drawn.bottom);
                let Some(inside) = IRect::intersect(&plan.region, &from_origin)
                else {
                    continue;
                };
                let got = pixels(
                    &drawn.snapshot,
                    inside.with_offset((-drawn.at.left, -drawn.at.top)),
                );
                let mut origin = started::raster(
                    &layer_info(canvas, depth, from_origin),
                    None,
                    None,
                )
                .expect("a layer at the origin");
                origin.canvas().set_matrix(&plan.layer.into());
                draw(origin.canvas(), rect, k);
                let want = pixels(&origin.image_snapshot(), inside);
                // The reference's size does not decide it: a layer the
                // page's size, from the same origin, holds the same bytes.
                let mut whole = started::raster(
                    &layer_info(canvas, depth, IRect::from_wh(PAGE.0, PAGE.1)),
                    None,
                    None,
                )
                .expect("a page-sized layer");
                whole.canvas().set_matrix(&plan.layer.into());
                draw(whole.canvas(), rect, k);
                let within =
                    IRect::intersect(&inside, &IRect::from_wh(PAGE.0, PAGE.1))
                        .expect("within the page");
                assert_eq!(
                    pixels(&whole.image_snapshot(), within),
                    pixels(&origin.image_snapshot(), within),
                    "{depth:?} #{k}: the reference moved with its size"
                );
                let differ =
                    got.iter().zip(&want).filter(|(a, b)| a != b).count();
                if differ > 0 {
                    apart.push(format!(
                        "{depth:?} #{k} at {:?}: {differ} bytes",
                        plan.drawn
                    ));
                }
            }
        }
        assert!(crossed > 100, "only {crossed} plans crossed an edge");
        assert!(apart.is_empty(), "apart from the page: {apart:#?}");
    }
}

/// Chains sharing one layer, held to the bytes each draws in a layer of its
/// own -- on Metal; where chains do not share, held to sharing none.
#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
mod shared_tests {
    use skia_safe::{
        AlphaType, Canvas as SkCanvas, Color, Color4f, ColorSpace, ColorType,
        ImageInfo, Paint, RRect, Rect, Surface, Vector,
    };

    use super::{
        Depth, Drawn, MAX_SHARED, Plan, SHARED_LAYERS, Step, apply, plan,
        shares_layers,
    };
    use crate::{
        context::page::ExportOptions, gpu::RenderingEngine,
        pixels::PixelColorSpace,
    };

    /// A rounded rectangle a chain draws, in the canvas's coordinates moved
    /// by `at`, the paint it is drawn with, and the steps it runs where they
    /// are not the page's.
    struct Member {
        rect: Rect,
        at: Vector,
        paint: Paint,
        steps: Option<Vec<Step>>,
    }

    impl Member {
        fn draw(&self, canvas: &SkCanvas) {
            let mut paint = self.paint.clone();
            paint.set_alpha_f(1.0);
            paint.set_anti_alias(true);
            canvas
                .draw_rrect(RRect::new_rect_xy(self.rect, 12.0, 12.0), &paint);
        }
    }

    /// A GPU surface of `info` at the page's sample count, or `None` where
    /// there is no GPU.
    fn page(info: &ImageInfo) -> Option<Surface> {
        RenderingEngine::GPU
            .make_surface(info, &ExportOptions::default())
            .ok()
    }

    /// `surface`'s pixels, as unpremultiplied 32-bit floats, so that a float
    /// page is compared bit for bit.
    fn read(surface: &mut Surface) -> Vec<u8> {
        let info = surface
            .image_info()
            .with_color_type(ColorType::RGBAF32)
            .with_alpha_type(AlphaType::Premul);
        let mut pixels = vec![0u8; info.compute_min_byte_size()];
        assert!(surface.read_pixels(
            &info,
            &mut pixels,
            info.min_row_bytes(),
            (0, 0)
        ));
        pixels
    }

    /// `members` each run through `steps` on a layer of its own, as a page
    /// plays them today.
    fn alone(canvas: &SkCanvas, steps: &[Step], members: &[Member]) {
        members.iter().for_each(|member| {
            canvas.save();
            canvas.translate(member.at);
            apply(
                canvas,
                member.steps.as_deref().unwrap_or(steps),
                Some(member.rect),
                Depth::Floats,
                &member.paint,
                |layer| member.draw(layer),
            );
            canvas.restore();
        });
    }

    /// `members` run in order, consecutive ones sharing a layer wherever
    /// [`Plan::joins`] allows. Returns how many layers they were drawn in.
    fn shared(canvas: &SkCanvas, steps: &[Step], members: &[Member]) -> usize {
        let plans: Vec<Option<Plan>> = members
            .iter()
            .map(|member| {
                canvas.save();
                canvas.translate(member.at);
                let steps = member.steps.as_deref().unwrap_or(steps);
                let plan =
                    plan(canvas, steps, Some(member.rect), Depth::Floats);
                canvas.restore();
                plan
            })
            .collect();
        let before = SHARED_LAYERS.with(std::cell::Cell::get);
        let mut start = 0;
        while start < members.len() {
            let mut batch: Vec<&Plan> = Vec::new();
            let mut end = start;
            while let Some(Some(next)) = plans.get(end) {
                if !batch.is_empty() && !next.joins(&batch) {
                    break;
                }
                batch.push(next);
                end += 1;
            }
            if batch.is_empty() {
                start += 1;
                continue;
            }
            let mut drawn = Drawn::new(canvas, &batch, |i, layer| {
                members[start + i].draw(layer)
            })
            .expect("a layer");
            for (plan, member) in batch.iter().zip(&members[start..end]) {
                let steps = member.steps.as_deref().unwrap_or(steps);
                if let Some(outcome) = drawn.finish(plan, steps, &member.paint)
                {
                    outcome.composite(canvas, &member.paint);
                }
            }
            start = end;
        }
        SHARED_LAYERS.with(std::cell::Cell::get) - before
    }

    /// `count` rounded rectangles 80 by 40, `gap` pixels apart in rows
    /// across a page `width` wide, at fractions of a pixel that vary: each
    /// drawn at the origin and moved there by its transform, so that every
    /// chain's layer is drawn under a transform of its own.
    fn grid(count: usize, gap: f32, width: f32, paint: &Paint) -> Vec<Member> {
        let per_row = ((width - 40.0) / (80.0 + gap)).floor().max(1.0) as usize;
        (0..count)
            .map(|i| {
                let (column, row) =
                    ((i % per_row) as f32, (i / per_row) as f32);
                let x = 20.0 + column * (80.0 + gap) + (i % 7) as f32 * 0.13;
                let y = 20.0 + row * (40.0 + gap) + (i % 5) as f32 * 0.21;
                Member {
                    rect: Rect::from_xywh(0.0, 0.0, 80.0, 40.0),
                    at: Vector::new(x, y),
                    paint: paint.clone(),
                    steps: None,
                }
            })
            .collect()
    }

    fn blur(sigma: f32) -> Vec<Step> {
        vec![Step::Blur(sigma)]
    }

    fn drop_shadow(dx: f32, dy: f32, sigma: f32) -> Vec<Step> {
        vec![Step::Shadow {
            offset: Vector::new(dx, dy),
            sigma,
            colour: Color4f::new(0.0, 0.0, 0.3, 0.6),
            space: ColorSpace::new_srgb(),
            shadow_only: false,
        }]
    }

    /// The bytes of `members` through `steps` on a page of `info`, played
    /// alone and shared, and how many layers sharing took. `None` where
    /// there is no GPU, and where chains do not share -- see
    /// [`shares_layers`] -- once every chain is seen to take a layer of its
    /// own.
    fn both(
        info: &ImageInfo,
        steps: &[Step],
        members: &[Member],
    ) -> Option<(Vec<u8>, Vec<u8>, usize)> {
        let mut one = page(info)?;
        let mut two = page(info)?;
        for surface in [&mut one, &mut two] {
            surface.canvas().clear(Color::WHITE);
        }
        alone(one.canvas(), steps, members);
        let layers = shared(two.canvas(), steps, members);
        if !shares_layers(two.canvas()) {
            assert_eq!(
                layers,
                members.len(),
                "chains shared a layer off Metal"
            );
            eprintln!("skipped: chains share a layer on Metal alone");
            return None;
        }
        Some((read(&mut one), read(&mut two), layers))
    }

    fn paints() -> [Paint; 2] {
        let opaque = Paint::new(Color4f::new(0.1, 0.2, 0.6, 1.0), None);
        let translucent = Paint::new(Color4f::new(0.8, 0.3, 0.1, 0.55), None);
        [opaque, translucent]
    }

    /// Disjoint chains share one layer, and every one of them lands the
    /// bytes it lands alone: blurs whose axes are blurred tap by tap and
    /// decimated, and a drop shadow moved toward its neighbour, on eight-bit
    /// sRGB and Display P3 pages and a half-float one.
    #[test]
    fn disjoint_chains_share_a_layer_and_draw_as_they_do_alone() {
        let infos = [
            ImageInfo::new_n32_premul((1200, 900), ColorSpace::new_srgb()),
            ImageInfo::new_n32_premul(
                (1200, 900),
                PixelColorSpace::DisplayP3
                    .to_skia_color_space()
                    .expect("Display P3"),
            ),
            ImageInfo::new(
                (1200, 900),
                ColorType::RGBAF16,
                AlphaType::Premul,
                ColorSpace::new_srgb(),
            ),
        ];
        let chains =
            [blur(2.0), blur(8.0), blur(30.0), drop_shadow(9.0, 4.0, 5.0)];
        let mut checked = 0;
        for info in &infos {
            for steps in &chains {
                for paint in paints() {
                    // Far enough apart that the widest reach, 6 sigma of 30
                    // plus the layer's even-edge widening, meets nothing.
                    let members = grid(12, 200.0, 1200.0, &paint);
                    let Some((alone, shared, layers)) =
                        both(info, steps, &members)
                    else {
                        return;
                    };
                    let off = alone
                        .chunks(16)
                        .zip(shared.chunks(16))
                        .filter(|(a, b)| a != b)
                        .count();
                    assert_eq!(
                        off,
                        0,
                        "{off} pixels differ sharing a layer: {:?} {steps:?}",
                        info.color_type()
                    );
                    assert_eq!(
                        layers, 1,
                        "{steps:?}: the chains did not share"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, infos.len() * chains.len() * 2);
    }

    /// 12 rounded rectangles drawn into one surface, which Ganesh merges
    /// into one instanced op, land the bytes each lands drawn alone -- what
    /// [`shares_layers`] rests on. Ignored where Metal is not the backend:
    /// on llvmpipe 4 of the 12 land apart, and this is the re-check for it.
    #[test]
    #[cfg_attr(
        not(feature = "metal"),
        ignore = "fails on Mesa llvmpipe 25.0.7, where chains do not share"
    )]
    fn a_rounded_rect_drawn_among_others_draws_as_it_does_alone() {
        let info =
            ImageInfo::new_n32_premul((1200, 900), ColorSpace::new_srgb());
        let paint = Paint::new(Color4f::new(0.1, 0.2, 0.6, 1.0), None);
        let members = grid(12, 200.0, 1200.0, &paint);
        let drawn = |only: Option<usize>| {
            let mut surface = page(&info)?;
            let canvas = surface.canvas();
            canvas.clear(Color::TRANSPARENT);
            for (i, member) in members.iter().enumerate() {
                if only.is_none_or(|only| only == i) {
                    canvas.save();
                    canvas.translate(member.at);
                    member.draw(canvas);
                    canvas.restore();
                }
            }
            Some(read(&mut surface))
        };
        let Some(together) = drawn(None) else {
            eprintln!("skipped: no GPU");
            return;
        };
        let apart: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|&(i, member)| {
                let alone = drawn(Some(i)).expect("a GPU surface");
                let (x, y) =
                    (member.at.x as usize - 2, member.at.y as usize - 2);
                let rows = (y..y + 46).map(|row| {
                    (row * 1200 + x) * 16..(row * 1200 + x + 86) * 16
                });
                let inked =
                    rows.clone().any(|row| alone[row].iter().any(|&b| b != 0));
                assert!(inked, "member {i} drew nothing alone");
                rows.into_iter()
                    .any(|row| together[row.clone()] != alone[row])
            })
            .map(|(i, _)| i)
            .collect();
        assert!(apart.is_empty(), "drawn among the others, {apart:?} moved");
    }

    /// The same on a 6000 by 4000 page, where the layer each chain would
    /// otherwise make is past the resource cache's budget.
    #[test]
    fn chains_share_a_layer_on_a_large_page() {
        let info =
            ImageInfo::new_n32_premul((6000, 4000), ColorSpace::new_srgb());
        let members = grid(40, 60.0, 6000.0, &paints()[1]);
        let Some((alone, shared, layers)) = both(&info, &blur(8.0), &members)
        else {
            return;
        };
        assert!(alone == shared, "sharing a layer changed the page");
        assert_eq!(layers, 1);
    }

    /// Many identical small rounded rectangles in one layer, the case where
    /// Ganesh can merge one chain's draw into another's op.
    #[test]
    fn identical_small_draws_sharing_a_layer_draw_as_they_do_alone() {
        let info =
            ImageInfo::new_n32_premul((1200, 900), ColorSpace::new_srgb());
        let paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 1.0), None);
        let members: Vec<Member> = (0..MAX_SHARED)
            .map(|i| Member {
                rect: Rect::from_xywh(
                    20.0 + (i % 8) as f32 * 140.0 + 0.37,
                    20.0 + (i / 8) as f32 * 100.0 + 0.25,
                    24.0,
                    16.0,
                ),
                at: Vector::new(0.0, 0.0),
                paint: paint.clone(),
                steps: None,
            })
            .collect();
        let Some((alone, shared, layers)) = both(&info, &blur(3.0), &members)
        else {
            return;
        };
        assert!(alone == shared, "merged draws changed the page");
        assert_eq!(layers, 1);
    }

    /// A chain whose draw lies in what another reads, or which reads what
    /// another draws, gets a layer of its own -- and a page with such pairs
    /// still lands what it lands alone.
    #[test]
    fn a_chain_reaching_another_does_not_share_its_layer() {
        let info =
            ImageInfo::new_n32_premul((1200, 900), ColorSpace::new_srgb());
        let paint = &paints()[1];
        // 8 sigma reaches 48 pixels: 30 apart in one row, each reads its
        // neighbours' draws.
        let members = grid(6, 30.0, 1200.0, paint);
        let Some((alone, shared, layers)) = both(&info, &blur(8.0), &members)
        else {
            return;
        };
        assert!(alone == shared, "a refused share changed the page");
        assert_eq!(layers, members.len(), "chains that meet shared a layer");
    }

    /// Two chains at every gap across the edge of sharing land the bytes
    /// they land alone, whether they share or not: the gap where the first's
    /// read stops meeting the second's draw, anti-aliasing included, is
    /// where they start to share.
    #[test]
    fn chains_either_side_of_the_edge_of_sharing_draw_as_they_do_alone() {
        let info =
            ImageInfo::new_n32_premul((600, 300), ColorSpace::new_srgb());
        let paint = &paints()[1];
        let mut shared_at = Vec::new();
        let mut compared = true;
        for steps in [blur(2.0), drop_shadow(5.0, 0.0, 2.0)] {
            for tenths in 0..400 {
                let gap = tenths as f32 / 10.0;
                let members = [
                    Member {
                        rect: Rect::from_xywh(0.0, 0.0, 80.0, 40.0),
                        at: Vector::new(100.3, 120.6),
                        paint: paint.clone(),
                        steps: None,
                    },
                    Member {
                        rect: Rect::from_xywh(0.0, 0.0, 80.0, 40.0),
                        at: Vector::new(180.3 + gap, 120.6),
                        paint: paint.clone(),
                        steps: None,
                    },
                ];
                // Off Metal every gap is still played, so that `both` sees
                // each refused.
                let Some((alone, shared, layers)) =
                    both(&info, &steps, &members)
                else {
                    compared = false;
                    continue;
                };
                assert!(
                    alone == shared,
                    "{steps:?} {gap} apart: sharing changed the page"
                );
                if layers == 1 {
                    shared_at.push(gap);
                }
            }
            if !compared {
                return;
            }
            let first = shared_at.first().copied();
            assert!(
                first.is_some_and(|gap| gap > 0.0),
                "{steps:?}: shared from {first:?}"
            );
            // Once apart, farther is apart too.
            let at = shared_at.len();
            assert!(
                shared_at
                    .windows(2)
                    .all(|w| (w[1] - w[0] - 0.1).abs() < 1e-3),
                "{steps:?}: sharing came and went over {at} gaps"
            );
            shared_at.clear();
        }
    }

    /// A chain whose wide reach takes in a later chain's draw does not share
    /// with it, though the later chain's narrow reach meets nothing of the
    /// first: sharing is refused either way round.
    #[test]
    fn a_wide_reach_into_a_later_draw_is_not_shared() {
        let info =
            ImageInfo::new_n32_premul((600, 300), ColorSpace::new_srgb());
        let paint = &paints()[1];
        let at = |x: f32, steps: Vec<Step>| Member {
            rect: Rect::from_xywh(0.0, 0.0, 80.0, 40.0),
            at: Vector::new(x, 120.6),
            paint: paint.clone(),
            steps: Some(steps),
        };
        // 8 sigma reaches 48 pixels and 2 sigma 12: 30 apart, the first
        // reads the second's draw and the second reads nothing of the first.
        for members in [
            [at(100.3, blur(8.0)), at(210.3, blur(2.0))],
            [at(100.3, blur(2.0)), at(210.3, blur(8.0))],
        ] {
            let Some((alone, shared, layers)) = both(&info, &[], &members)
            else {
                return;
            };
            assert!(alone == shared, "sharing changed the page");
            assert_eq!(layers, 2, "chains a reach joins shared a layer");
        }
    }

    /// A batch stops at [`MAX_SHARED`] chains.
    #[test]
    fn a_layer_is_shared_by_at_most_max_shared_chains() {
        let info =
            ImageInfo::new_n32_premul((6000, 4000), ColorSpace::new_srgb());
        let members = grid(MAX_SHARED + 1, 60.0, 6000.0, &paints()[0]);
        let Some((alone, shared, layers)) = both(&info, &blur(2.0), &members)
        else {
            return;
        };
        assert!(alone == shared);
        assert_eq!(layers, 2);
    }
}
