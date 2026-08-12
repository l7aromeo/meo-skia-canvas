#![allow(clippy::upper_case_acronyms)]
#[cfg(feature = "metal")]
use objc2::rc::autoreleasepool;

use crate::context::page::{ExportOptions, Page};
use serde_json::{Value, json};
use skia_safe::{
    AlphaType, Color, Color4f, ColorSpace, ColorType, Image, ImageInfo, Matrix,
    Paint, Rect, Surface, gpu::DirectContext, surfaces,
};
use std::sync::OnceLock;

#[cfg(feature = "metal")]
mod metal;
#[cfg(feature = "metal")]
use crate::gpu::metal::MetalEngine as Engine;
#[cfg(all(feature = "metal", feature = "window"))]
pub use crate::gpu::metal::MetalRenderer as Renderer;

#[cfg(feature = "vulkan")]
mod vulkan;
#[cfg(feature = "vulkan")]
use crate::gpu::vulkan::engine::VulkanEngine as Engine;
#[cfg(all(feature = "vulkan", feature = "window"))]
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
        panic!()
    }

    pub fn with_direct_context(_f: impl FnOnce(Option<&mut DirectContext>)) {
        panic!()
    }
}

// `rayon`'s workers have no autorelease pool of their own, so Metal's `objc`
// allocations would accumulate until the thread exits. The main thread gets one
// from node's event loop.
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

#[derive(Copy, Clone, Debug)]
pub enum RenderingEngine {
    CPU,
    GPU,
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

#[allow(dead_code)]
impl RenderingEngine {
    pub fn selectable(&self) -> bool {
        match self {
            Self::GPU => Engine::supported(),
            Self::CPU => true,
        }
    }

    /// Whether this engine can composite a page in `color_type`.
    ///
    /// Asked once per format and remembered, by building a one-pixel surface
    /// and seeing whether the backend takes it. A probe rather than a table
    /// because the answer is Skia's to give and it changes between releases:
    /// as of Skia m150 its Ganesh Metal and Vulkan backends carry no 32-bit
    /// float format at all -- the Metal one's list of pixel formats stops at
    /// `RGBA16Float`, and neither declares `kRGBA_F32` -- while its GL,
    /// raster and Graphite paths do. When that changes, or when a caller
    /// builds against a Skia where it already has, this starts answering yes
    /// on its own and the canvas stays on the GPU. A hardcoded rule would
    /// still be refusing years later.
    pub fn can_composite(&self, color_type: ColorType) -> bool {
        // Every backend rasterises N32, which is what everything else
        // composites in.
        if !matches!(
            color_type,
            ColorType::RGBAF16 | ColorType::RGBAF16Norm | ColorType::RGBAF32
        ) {
            return true;
        }
        if matches!(self, Self::CPU) {
            return true;
        }

        static F16: OnceLock<bool> = OnceLock::new();
        static F32: OnceLock<bool> = OnceLock::new();
        let cell = match color_type {
            ColorType::RGBAF32 => &F32,
            _ => &F16,
        };
        *cell.get_or_init(|| {
            let info = ImageInfo::new(
                (1, 1),
                color_type,
                AlphaType::Premul,
                Some(ColorSpace::new_srgb()),
            );
            let Ok(mut surface) =
                self.make_surface(&info, &ExportOptions::default())
            else {
                return false;
            };

            // Allocating the surface is not the question -- compositing in it
            // is. A GPU quantises the paint colour to eight bits before it
            // reaches the surface, so sixty faint layers land on 60/255
            // however wide the pixels are: an answer further from the truth
            // than eight-bit storage manages. So this draws the case float
            // exists for and checks the arithmetic.
            let mut paint = Paint::default();
            paint.set_anti_alias(false);
            paint.set_color4f(
                Color4f::new(0.006, 0.006, 0.006, 0.006),
                &ColorSpace::new_srgb(),
            );
            let canvas = surface.canvas();
            canvas.clear(Color::TRANSPARENT);
            for _ in 0..60 {
                canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);
            }

            let read_info = ImageInfo::new(
                (1, 1),
                ColorType::RGBAF32,
                AlphaType::Premul,
                Some(ColorSpace::new_srgb()),
            );
            let mut pixels = [0u8; 16];
            let stride = read_info.min_row_bytes();
            if !surface.read_pixels(&read_info, &mut pixels, stride, (0, 0)) {
                return false;
            }
            let alpha = f32::from_le_bytes([
                pixels[12], pixels[13], pixels[14], pixels[15],
            ]);

            // 1 - 0.994^60, the answer arithmetic without rounding gives.
            const IDEAL: f32 = 0.303_08;
            (alpha - IDEAL).abs() < 0.01
        })
    }

    pub fn make_surface(
        &self,
        image_info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        match self {
            Self::GPU => Engine::make_surface(image_info, opts),
            Self::CPU => surfaces::raster(
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
            Self::CPU => f(None),
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
