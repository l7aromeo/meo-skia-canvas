#![allow(clippy::upper_case_acronyms)]
#[cfg(feature = "metal")]
use objc2::rc::autoreleasepool;

use crate::{
    context::page::{ExportOptions, Page},
    start_skia, started,
};
use serde_json::{Value, json};
use skia_safe::{
    AlphaType, Canvas, Color, Color4f, ColorSpace, ColorType, Image, ImageInfo,
    Matrix, Paint, Rect, Surface, gpu::DirectContext,
};
use std::sync::OnceLock;

/// Confirming the GPU ran a page's draws before they are read back.
pub(crate) mod completion;
/// Graphite, the engine for pages Ganesh cannot composite.
pub(crate) mod graphite;
/// The one thread that owns a GPU context for exporting.
pub mod owner;
/// Flushing a GPU playback every so many layers it renders offscreen.
pub(crate) mod pacing;
/// An eight-bit GPU page unpremultiplied on the GPU, to the CPU table's byte.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) mod unpremultiply;

/// A marker making its holder `!Send`, for state that belongs to one thread.
///
/// Skia's rule is that a `DirectContext` is active on the thread that built it,
/// and everything reached through it -- surfaces, texture-backed images --
/// belongs to that thread too. Nothing in the types said so. Both backends keep
/// their context in a `thread_local`, which happens to enforce it, and
/// [`owner`] now keeps the exporting one on a single thread, which also happens
/// to enforce it: two arrangements, no statement.
///
/// An arrangement is correct only while it holds. A second device, a window
/// thread beside the export thread, or a refactor that looks harmless brings
/// the mistake back with no diagnostic -- it returns as a wrong image or a
/// crash inside Skia rather than as an error.
///
/// One zero-sized field turns that into a compile error. It costs nothing at
/// runtime: `PhantomData<*const ()>` occupies no bytes and only removes an auto
/// trait.
///
/// Only the two GPU engines carry one, so with neither compiled in there is
/// no state to bind and the marker is not built at all -- which the
/// features-off lint reports as dead code rather than as an unused feature.
#[cfg(any(feature = "metal", feature = "vulkan"))]
#[derive(Debug, Default)]
pub struct ThreadBound(std::marker::PhantomData<*const ()>);

#[cfg(any(feature = "metal", feature = "vulkan"))]
impl ThreadBound {
    /// A marker for state that must not leave the thread that made it.
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

#[cfg(feature = "metal")]
mod metal;
#[cfg(feature = "metal")]
use crate::gpu::metal::MetalEngine as Engine;
#[cfg(all(feature = "metal", feature = "window"))]
pub use crate::gpu::metal::MetalRenderer as Renderer;

// Metal takes precedence where both backends are enabled. Cargo features are
// additive, so a binary depending on two crates that ask for different backends
// gets both, and the overlap has to resolve to one engine rather than bind
// `Engine` twice inside a dependency the consumer never named and cannot edit.
// Metal wins because the overlap is only reachable on macOS, where it is the
// native API and Vulkan reaches the device through MoltenVK.
//
// `all(feature = "vulkan", not(feature = "metal"))` is what "Vulkan is the
// backend" means across the crate, `gui::window` included. A bare
// `feature = "vulkan"` states something else -- that the feature is on -- and
// under this rule the two are no longer the same question.
#[cfg(all(feature = "vulkan", not(feature = "metal")))]
mod vulkan;
#[cfg(all(feature = "vulkan", not(feature = "metal")))]
use crate::gpu::vulkan::engine::VulkanEngine as Engine;
#[cfg(all(
    feature = "vulkan",
    not(feature = "metal"),
    feature = "window"
))]
pub use crate::gpu::vulkan::renderer::VulkanRenderer as Renderer;

#[cfg(not(any(feature = "vulkan", feature = "metal")))]
struct Engine {}
#[cfg(not(any(feature = "vulkan", feature = "metal")))]
impl Engine {
    pub fn api() -> Option<String> {
        None
    }

    pub fn supported() -> bool {
        false
    }

    pub fn status() -> Value {
        serde_json::json!({
            "renderer": "CPU",
            "api": Value::Null,
            "device": "CPU-based renderer (compiled without GPU support)",
            "error": Value::Null,
        })
    }

    // placeholders that match the GPU signatures (for the type-checker) but
    // will never be called (see the RenderingEngine methods for their
    // inline implementation when in CPU mode)
    pub fn make_surface(
        _info: &ImageInfo,
        _opts: &ExportOptions,
    ) -> Result<Surface, String> {
        unreachable!(
            "no GPU backend is compiled in, so `RenderingEngine` answers \
             every surface request itself and never reaches this signature"
        )
    }

    pub fn with_direct_context(_f: impl FnOnce(Option<&mut DirectContext>)) {
        unreachable!(
            "no GPU backend is compiled in, so there is no direct context to \
             hand out and `RenderingEngine` does not ask for one"
        )
    }

    // Unlike the two above this one is reached for real, by an idle export
    // owner, and there is no context to release when no backend is compiled
    // in.
    pub fn release_idle_context() {}
}

// Metal's `objc` allocations are autoreleased, so they need a pool on whatever
// thread makes them or they accumulate until that thread exits. `rayon`'s
// workers have none of their own.
//
// Neither does node's main thread, which is the part that cost something. This
// comment used to say it got one from the event loop; it does not. Node runs no
// `NSRunLoop` -- it is not a Cocoa app -- so nothing drains a main-thread pool
// between ticks, and returning to the loop does not help. Measured on the
// synchronous export path before it was wrapped: 100 GPU canvases of 1200x900
// exported per pass grew RSS 512 -> 886 -> 1257 -> 1633 -> 2004 -> 2376 MB,
// about 3.9 MB a canvas and near enough the whole surface, across six passes
// that each awaited and forced two collections. The asynchronous path, wrapped
// for the rayon worker, was flat over the same run.
//
// So every entry point that touches Metal wraps its work in this, synchronous
// ones included.
#[cfg(feature = "metal")]
pub fn autorelease<T>(f: impl FnOnce() -> T) -> T {
    // The pool token is discarded: nothing here borrows from the pool, so the
    // callback has no use for it.
    autoreleasepool(|_| f())
}

#[cfg(not(feature = "metal"))]
pub fn autorelease<T>(f: impl FnOnce() -> T) -> T {
    f()
}

/// The alpha sixty layers at 0.006 composite to: 1 - 0.994^60, as arithmetic
/// without rounding gives it, to five places (0.3030798 to seven).
const SIXTY_LAYERS: f32 = 0.303_08;

/// How far from [`SIXTY_LAYERS`] an `F32` page may land and still count as
/// compositing in float.
///
/// Chosen to separate the two outcomes the probe sees, not tuned: an engine
/// that rounds each draw's source to eight bits lands at 0.36078 or beyond,
/// and one compositing in float, half floats included, within 0.002. It
/// measures blending, not whether one draw keeps its colour -- that is
/// [`HELD_BY_F32_ONLY`]'s.
const FLOAT_COMPOSITING: f32 = 0.01;

/// A colour channel an `F32` page holds and an `F16` one cannot: 0.1, which
/// as an `f32` is 0.100000001490116, and whose nearest half float is
/// 0.0999755859375. One opaque draw of it, read back from an `F32` page, has
/// these bits exactly where the draw kept its colour in float, and the half's
/// where any step of the draw narrowed it to 16 bits.
const HELD_BY_F32_ONLY: f32 = 0.1;

/// The side of the page [`HELD_BY_F32_ONLY`] is drawn on: one pixel of it is
/// drawn, so the draw does not cover the page. Graphite turns an unclipped
/// opaque fill that covers its whole target into a clear of it, and "nothing
/// has to be rendered at all" (`Device::drawGeometry`,
/// `src/gpu/graphite/Device.cpp`, lines 1844 to 1853, under the skia-bindings
/// crate), so the colour reaches the attachment without a shader: on a
/// one-pixel page it reads back 0.1 exactly, and the probe would see nothing.
const CLEAR_FREE: i32 = 2;

/// The rasterizer a page is drawn on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RenderingEngine {
    /// Skia's raster backend.
    CPU,
    /// Ganesh, Skia's GPU backend for everything it can composite: Metal on
    /// macOS, Vulkan elsewhere.
    GPU,
    /// Graphite on Metal, the engine for the float pages Ganesh cannot
    /// composite, once it keeps them in float. [`Self::for_canvas`] never
    /// chooses it, and on this Skia [`Self::can_composite`] refuses it every
    /// float format as well.
    ///
    /// Present on every build, so that a match over the engines reads the
    /// same with and without it; built only where `meo_graphite` is set on
    /// Metal, and never constructed anywhere else -- see [`graphite`].
    #[cfg_attr(not(all(meo_graphite, feature = "metal")), allow(dead_code))]
    Graphite,
}

impl Default for RenderingEngine {
    fn default() -> Self {
        if Engine::supported() {
            Self::GPU
        } else {
            Self::CPU
        }
    }
}

/// The engine `canvas` draws on, read from the canvas itself: Ganesh where it
/// has a recording context, Graphite where it has a recorder, and the CPU
/// otherwise. A surface made from a canvas's own (`Canvas::new_surface`)
/// answers as that canvas does.
pub(crate) fn engine_of(canvas: &Canvas) -> RenderingEngine {
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    if canvas.recording_context().is_some() {
        return RenderingEngine::GPU;
    }
    #[cfg(all(meo_graphite, feature = "metal"))]
    if canvas.recorder().is_some() {
        return RenderingEngine::Graphite;
    }
    #[cfg(not(any(feature = "metal", feature = "vulkan")))]
    let _ = canvas;
    RenderingEngine::CPU
}

#[allow(dead_code)]
impl RenderingEngine {
    /// The engine a canvas of `color_type` rasterizes on, where `gpu` says
    /// whether it asks for the GPU: the CPU where it does not, where no GPU
    /// backend is reachable, and where the GPU cannot composite `color_type`.
    /// Every engine choice goes through this -- the Rust `Canvas`, the
    /// readback of its pages, and the binding's canvas, whether the engine is
    /// asked for at construction or set afterwards -- so that none of them
    /// can make another.
    ///
    /// A float canvas that the GPU cannot composite goes to the raster
    /// backend rather than quietly dropping to eight bits: `colorType` means
    /// the same thing on both engines, and the canvas reports which one
    /// answered. Skia's Ganesh Metal and Vulkan backends carry no 32-bit
    /// float format today, and round a draw's colour to eight bits before
    /// compositing it in an `F16` one, so this is where an `F16` or `F32`
    /// canvas changes hands -- and [`Self::can_composite`] probes rather than
    /// assumes, so a Skia that keeps either in float keeps the canvas
    /// on the GPU.
    pub fn for_canvas(gpu: bool, color_type: ColorType) -> Self {
        if !gpu {
            return Self::CPU;
        }
        let engine = Self::default();
        match engine.can_composite(color_type) {
            true => engine,
            false => Self::CPU,
        }
    }

    pub fn selectable(&self) -> bool {
        match self {
            Self::GPU => Engine::supported(),
            Self::Graphite => graphite::supported(),
            Self::CPU => true,
        }
    }

    /// Whether this engine can composite a page in `color_type`.
    ///
    /// Every engine composites eight-bit formats. A float format is asked
    /// once per engine and remembered, the first time a canvas of that format
    /// asks for the GPU: sixty layers at alpha 0.006 are drawn on a one-pixel
    /// page in `color_type` and read back -- see [`Self::composited`] -- and
    /// the answer is judged by the format.
    ///
    /// - `F32` is taken where the page blends in float, landing within
    ///   [`FLOAT_COMPOSITING`] of the arithmetic, and where one draw keeps its
    ///   colour in float -- see [`Self::keeps_f32_colour`], asked once per
    ///   engine with the first `F32` question and remembered. The first alone
    ///   measures blending: an engine that rounds the source to eight bits
    ///   lands at least 0.05 away, and Graphite lands 0.00002 away, against the
    ///   CPU's 0.0000002. Yet Graphite on Metal narrows the colour of every
    ///   draw it shades to a half float, which blending sixty constant layers
    ///   cannot show and one draw does, so on this Skia it is refused `F32`.
    /// - `F16` is taken only where the page lands no further from the
    ///   arithmetic than the CPU's own `F16` page does, since the CPU holds the
    ///   same format and is what the canvas gets otherwise. Graphite's
    ///   half-float blend lands 0.00112 away, against the CPU's 0.00010, so an
    ///   `F16` page stays on the CPU.
    ///
    /// A probe rather than a table because the answer is Skia's to give and
    /// it changes between releases: as of Skia m150 its Ganesh Metal and
    /// Vulkan backends carry no 32-bit float format at all -- the Metal one's
    /// list of pixel formats stops at `RGBA16Float`, and neither declares
    /// `kRGBA_F32` -- while its GL, raster and Graphite paths do. When that
    /// changes, or when a caller builds against a Skia where it already has,
    /// this starts answering yes on its own. A hardcoded rule would still be
    /// refusing years later.
    pub fn can_composite(&self, color_type: ColorType) -> bool {
        if !matches!(
            color_type,
            ColorType::RGBAF16 | ColorType::RGBAF16Norm | ColorType::RGBAF32
        ) {
            return true;
        }
        if matches!(self, Self::CPU) {
            return true;
        }
        let Some(alpha) = self.composited(color_type) else {
            return false;
        };
        let error = (alpha - SIXTY_LAYERS).abs();
        match color_type {
            ColorType::RGBAF32 => {
                error < FLOAT_COMPOSITING && self.keeps_f32_colour()
            }
            _ => Self::CPU
                .composited(color_type)
                .is_some_and(|cpu| error <= (cpu - SIXTY_LAYERS).abs()),
        }
    }

    /// The alpha sixty layers at 0.006 composite to on a one-pixel page of
    /// this engine in the float format `color_type`, asked once per engine
    /// and format; `None` where the engine cannot make or read the page.
    ///
    /// The case float exists for. Ganesh rounds each draw's source colour to
    /// eight bits before blending, whatever the surface is made of, so sixty
    /// layers at alpha 0.006 accumulate in F16 from a source that has already
    /// become 2/255: 0.37695 against the 0.30308 the arithmetic gives, and
    /// further from it than eight-bit storage manages on either backend.
    /// Feeding 2/255 itself, an alpha eight bits hold exactly, brings the GPU
    /// to within 0.00024 of the CPU's F16 (0.37695 against 0.37671), where
    /// 0.006 leaves them 0.074 apart -- which is what says the loss is the
    /// source and not the accumulation. It is not the paint slot -- a colour
    /// arriving through a shader rounds identically -- so no shader or
    /// runtime effect escapes it. Graphite blends F32 to 0.30310 against the
    /// CPU's 0.30308, and F16 to 0.30420 against the CPU's 0.30298 -- what
    /// this measures is blending, which happens at the attachment's own
    /// precision, and not whether one draw keeps its colour.
    fn composited(&self, color_type: ColorType) -> Option<f32> {
        static ALPHAS: [[OnceLock<Option<f32>>; 2]; 3] =
            [const { [const { OnceLock::new() }; 2] }; 3];
        let format = match color_type {
            ColorType::RGBAF32 => 1,
            _ => 0,
        };
        *ALPHAS[self.index()][format].get_or_init(|| {
            let pixel = self.drawn_pixel(color_type, 1, |canvas| {
                let mut paint = Paint::default();
                paint.set_anti_alias(false);
                paint.set_color4f(
                    Color4f::new(0.006, 0.006, 0.006, 0.006),
                    &ColorSpace::new_srgb(),
                );
                canvas.clear(Color::TRANSPARENT);
                for _ in 0..60 {
                    canvas
                        .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);
                }
            })?;
            Some(pixel[3])
        })
    }

    /// Whether one opaque draw of [`HELD_BY_F32_ONLY`] on one pixel of an
    /// `F32` page [`CLEAR_FREE`] pixels square reads back with that `f32`'s
    /// bits exactly; asked once per engine, with the first `F32` question
    /// [`Self::can_composite`] is asked, and remembered. False where the
    /// engine cannot make or read the page.
    ///
    /// The CPU keeps it. Graphite on Metal does not: the pixel reads back
    /// 0.0999755859375, the half float nearest 0.1, because Skia's SkSL
    /// generator for Metal writes every half as an MSL `half` -- the fragment
    /// output is `half4 sk_FragColor` and every texture is sampled as
    /// `texture2d<half>` -- and it reads no setting that would widen them.
    /// Blending happens at the attachment's precision, so the sixty-layer
    /// probe cannot see it.
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test graphite_is_refused_f32_while_metal_draws_half
    // with the patched Graphite bindings; it fails once a Graphite F32 draw
    // keeps its colour
    //
    // `SkSLMetalCodeGenerator.cpp` declares `half4 sk_FragColor
    // [[color(0)]]` (line 3291) and reads no `fForceHighPrecision`, which
    // the GLSL, SPIR-V and WGSL generators honour. Worked around by refusing
    // `F32` on Graphite here, so no canvas reaches it; keeping F32 on Graphite
    // needs that generator patched, which #558 tracks.
    fn keeps_f32_colour(&self) -> bool {
        static KEPT: [OnceLock<bool>; 3] = [const { OnceLock::new() }; 3];
        *KEPT[self.index()].get_or_init(|| {
            let pixel =
                self.drawn_pixel(ColorType::RGBAF32, CLEAR_FREE, |canvas| {
                    let mut paint = Paint::default();
                    paint.set_anti_alias(false);
                    paint.set_color4f(
                        Color4f::new(
                            HELD_BY_F32_ONLY,
                            HELD_BY_F32_ONLY,
                            HELD_BY_F32_ONLY,
                            1.0,
                        ),
                        &ColorSpace::new_srgb(),
                    );
                    canvas.clear(Color::TRANSPARENT);
                    canvas
                        .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);
                });
            pixel.is_some_and(|pixel| {
                pixel[..3].iter().all(|channel| {
                    channel.to_bits() == HELD_BY_F32_ONLY.to_bits()
                })
            })
        })
    }

    /// The premultiplied `F32` channels of the top-left pixel `draw` leaves on
    /// a `side` by `side` sRGB page of this engine in `color_type`, or `None`
    /// where the engine cannot make or read the page.
    fn drawn_pixel(
        &self,
        color_type: ColorType,
        side: i32,
        draw: impl FnOnce(&Canvas),
    ) -> Option<[f32; 4]> {
        let info = ImageInfo::new(
            (side, side),
            color_type,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        );
        let read = ImageInfo::new(
            (1, 1),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        );
        let mut pixels = [0u8; 16];
        match self {
            // `Surface::read_pixels` does not read a Graphite page.
            Self::Graphite => {
                graphite::probe(&info, draw, &read, &mut pixels).ok()?
            }
            Self::CPU | Self::GPU => {
                let mut surface =
                    self.make_surface(&info, &ExportOptions::default()).ok()?;
                draw(surface.canvas());
                let stride = read.min_row_bytes();
                surface
                    .read_pixels(&read, &mut pixels, stride, (0, 0))
                    .then_some(())?
            }
        }
        let channel = |i: usize| {
            f32::from_le_bytes([
                pixels[4 * i],
                pixels[4 * i + 1],
                pixels[4 * i + 2],
                pixels[4 * i + 3],
            ])
        };
        Some([channel(0), channel(1), channel(2), channel(3)])
    }

    /// This engine's slot in the probes' per-engine caches.
    fn index(&self) -> usize {
        match self {
            Self::CPU => 0,
            Self::GPU => 1,
            Self::Graphite => 2,
        }
    }

    pub fn make_surface(
        &self,
        image_info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        match self {
            Self::GPU => {
                start_skia();
                Engine::make_surface(image_info, opts)
            }
            Self::Graphite => {
                start_skia();
                graphite::make_surface(image_info, opts)
            }
            Self::CPU => started::raster(
                image_info,
                None,
                Some(&opts.surface_props()),
            )
            .ok_or(format!(
                "Could not allocate new {}×{} bitmap (color type: {:?})",
                image_info.width(),
                image_info.height(),
                image_info.color_type()
            )),
        }
    }

    pub fn with_direct_context(
        &self,
        f: impl FnOnce(Option<&mut DirectContext>),
    ) {
        match self {
            Self::GPU => Engine::with_direct_context(f),
            Self::Graphite | Self::CPU => f(None),
        }
    }

    /// What this engine is, for `canvas.engine`.
    ///
    /// Reports `self`, not the machine. Asking the device instead answered
    /// "GPU" for every canvas on a machine that has one -- including canvases
    /// rendering on the raster backend because their pixel format needed it,
    /// which is the one case where a caller most wants to know.
    pub fn status(&self, is_manually_disabled: bool) -> serde_json::Value {
        match (self, is_manually_disabled) {
            (Self::GPU, _) => Engine::status(),
            (Self::Graphite, _) => graphite::status(),
            (Self::CPU, true) => json!({
                "renderer":"CPU",
                "api": Engine::api(),
                "device": "CPU-based renderer (GPU manually disabled)",
                "driver": "N/A",
                "threads": rayon::current_num_threads()
            }),
            // Either there is no GPU to use, or this canvas asked for
            // something it cannot composite -- a float pixel format, today.
            (Self::CPU, false) => match Engine::supported() {
                false => Engine::status(),
                true => json!({
                    "renderer": "CPU",
                    "api": Engine::api(),
                    "device": "CPU-based renderer (pixel format needs it)",
                    "driver": "N/A",
                    "threads": rayon::current_num_threads()
                }),
            },
        }
    }

    pub fn lacks_gpu_support(&self) -> Option<String> {
        match Engine::supported() {
            true => None,
            false => {
                let mut msg = vec!["No windowing support".to_string()];
                if let Some(Value::String(error)) =
                    Engine::status().get("error")
                {
                    msg.push(error.to_string());
                }
                Some(msg.join(": "))
            }
        }
    }
}

#[allow(dead_code)]
pub struct RenderCache {
    image: Option<Image>,
    content: Rect,
    page: Page,
    matte: Color,
    dpr: f32,
    state: RenderState,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self {
            image: None,
            content: Rect::new_empty(),
            page: Page::default(),
            dpr: 0.0,
            matte: Color::TRANSPARENT,
            state: RenderState::Clean,
        }
    }
}

#[allow(dead_code)]
impl RenderCache {
    pub fn validate(
        &mut self,
        page: &Page,
        matte: Color,
        dpr: f32,
        clip: Rect,
    ) -> Option<(&Image, &Rect, Rect)> {
        if self.state == RenderState::Dirty
            || self.page.id != page.id
            || self.matte != matte
            || self.dpr != dpr
        {
            *self = Self::default();
        }

        self.image.as_ref().map(|img| {
            let (dst, _) = Matrix::scale((dpr, dpr)).map_rect(clip);
            (img, &self.content, dst)
        })
    }

    pub fn depth(&self) -> usize {
        self.page.layers.len()
    }

    pub fn update(
        &mut self,
        image: Image,
        page: &Page,
        matte: Color,
        dpr: f32,
        content: Rect,
    ) {
        if self.state == RenderState::Resizing {
            // mark the framebuffer as needing a full redraw and skip updating
            // cached image during resize
            self.state = RenderState::Dirty;
        } else {
            let state = RenderState::Clean;
            let (content, _) =
                skia_safe::Matrix::scale((dpr, dpr)).map_rect(content);
            *self = Self {
                image: Some(image),
                page: page.clone(),
                matte,
                dpr,
                content,
                state,
            };
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RenderState {
    Clean,
    Dirty,
    Resizing,
}

#[cfg(test)]
mod tests {
    use skia_safe::{AlphaType, ColorSpace, ColorType, ImageInfo};

    use super::*;

    const FLOATS: [ColorType; 3] = [
        ColorType::RGBAF16,
        ColorType::RGBAF16Norm,
        ColorType::RGBAF32,
    ];

    fn info(color_type: ColorType) -> ImageInfo {
        ImageInfo::new(
            (4, 4),
            color_type,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        )
    }

    /// Every engine composites the eight-bit format everything else is
    /// drawn in, and the CPU every format.
    #[test]
    fn every_engine_composites_eight_bits_and_the_cpu_every_format() {
        for engine in [
            RenderingEngine::CPU,
            RenderingEngine::GPU,
            RenderingEngine::Graphite,
        ] {
            assert!(engine.can_composite(ColorType::N32), "{engine:?}");
        }
        for color_type in FLOATS {
            assert!(RenderingEngine::CPU.can_composite(color_type));
        }
    }

    /// Ganesh takes neither float format: it has no 32-bit one, and rounds
    /// a draw's colour to eight bits before compositing it in 16.
    #[cfg(feature = "metal")]
    #[test]
    fn ganesh_takes_no_float_format() {
        assert!(RenderingEngine::GPU.selectable());
        for color_type in FLOATS {
            assert!(
                !RenderingEngine::GPU.can_composite(color_type),
                "{color_type:?}"
            );
        }
    }

    /// The CPU keeps one draw's `F32` colour to the bit, so the probe that
    /// refuses Graphite `F32` passes the engine that does keep it.
    #[test]
    fn the_cpu_keeps_a_drawn_f32_colour_to_the_bit() {
        assert!(RenderingEngine::CPU.keeps_f32_colour());
        assert_eq!(
            RenderingEngine::CPU
                .drawn_pixel(ColorType::RGBAF32, CLEAR_FREE, |canvas| {
                    let mut paint = Paint::default();
                    paint.set_color4f(
                        Color4f::new(HELD_BY_F32_ONLY, 0.0, 0.0, 1.0),
                        &ColorSpace::new_srgb(),
                    );
                    canvas
                        .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);
                })
                .map(|pixel| pixel[0].to_bits()),
            Some(HELD_BY_F32_ONLY.to_bits())
        );
    }

    /// Graphite takes neither float format on Metal. It blends `F32` in float,
    /// within [`FLOAT_COMPOSITING`] of the arithmetic, yet narrows one draw's
    /// colour to a half float, so it is refused `F32`; and its `F16` lands
    /// further from the arithmetic than the CPU's `F16`.
    #[cfg(all(meo_graphite, feature = "metal"))]
    #[test]
    fn graphite_is_refused_f32_while_metal_draws_half() {
        assert!(RenderingEngine::Graphite.selectable());
        let error = |engine: RenderingEngine, color_type| {
            (engine.composited(color_type).expect("a reading") - SIXTY_LAYERS)
                .abs()
        };
        // Blending alone would take it: within the tolerance, if further from
        // the arithmetic than the CPU.
        assert!(
            error(RenderingEngine::Graphite, ColorType::RGBAF32)
                < FLOAT_COMPOSITING
        );
        assert!(
            error(RenderingEngine::Graphite, ColorType::RGBAF32)
                > error(RenderingEngine::CPU, ColorType::RGBAF32)
        );
        // One draw does not keep its colour: it comes back as the half float
        // nearest 0.1. 0.1 is 1.6 times 2^-4, and a half's ten-bit mantissa
        // holds 1.6 to 1638/1024, so the half is 1638 / 2^14.
        let nearest_half = 1638.0 / f32::from(1u16 << 14);
        assert_eq!(
            RenderingEngine::Graphite
                .drawn_pixel(ColorType::RGBAF32, CLEAR_FREE, |canvas| {
                    let mut paint = Paint::default();
                    paint.set_color4f(
                        Color4f::new(HELD_BY_F32_ONLY, 0.0, 0.0, 1.0),
                        &ColorSpace::new_srgb(),
                    );
                    canvas
                        .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);
                })
                .map(|pixel| pixel[0]),
            Some(nearest_half)
        );
        assert!(!RenderingEngine::Graphite.keeps_f32_colour());
        assert!(!RenderingEngine::Graphite.can_composite(ColorType::RGBAF32));
        for color_type in [ColorType::RGBAF16, ColorType::RGBAF16Norm] {
            // In float, so a tolerance alone would take it.
            assert!(
                error(RenderingEngine::Graphite, color_type)
                    < FLOAT_COMPOSITING
            );
            assert!(
                error(RenderingEngine::Graphite, color_type)
                    > error(RenderingEngine::CPU, color_type)
            );
            assert!(
                !RenderingEngine::Graphite.can_composite(color_type),
                "{color_type:?}"
            );
        }
    }

    /// Without the engine Graphite is never selectable and takes no float
    /// format.
    #[cfg(not(all(meo_graphite, feature = "metal")))]
    #[test]
    fn a_build_without_graphite_gives_it_nothing() {
        assert!(!RenderingEngine::Graphite.selectable());
        for color_type in FLOATS {
            assert!(!RenderingEngine::Graphite.can_composite(color_type));
        }
    }

    /// Each kind of page answers with the engine that made it, and a page
    /// made from its canvas answers as it does.
    #[test]
    fn a_canvas_answers_with_the_engine_that_made_it() {
        let mut engines = vec![RenderingEngine::CPU];
        if RenderingEngine::GPU.selectable() {
            engines.push(RenderingEngine::GPU);
        }
        if RenderingEngine::Graphite.selectable() {
            engines.push(RenderingEngine::Graphite);
        }
        #[cfg(all(meo_graphite, feature = "metal"))]
        assert!(engines.contains(&RenderingEngine::Graphite));
        for engine in engines {
            let color_type = match engine {
                RenderingEngine::Graphite => ColorType::RGBAF32,
                RenderingEngine::CPU | RenderingEngine::GPU => ColorType::N32,
            };
            let info = info(color_type);
            let mut page = engine
                .make_surface(&info, &ExportOptions::default())
                .expect("a page");
            assert_eq!(engine_of(page.canvas()), engine);
            let mut layer = page.new_surface(&info).expect("a layer");
            assert_eq!(engine_of(layer.canvas()), engine, "a layer");
        }
    }
}
