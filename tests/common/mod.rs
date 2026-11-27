//! What the integration tests share. Each file under `tests/` is a crate of
//! its own and takes this as `mod common;`.

#![allow(
    dead_code,
    reason = "each test crate compiles this whole and uses what it needs"
)]

use std::env;

use meo_skia_canvas::prelude::*;

/// Text the GPU's device name has to contain where it is set, as on CI's
/// Linux runner, which names lavapipe's `llvmpipe`: a run that has to show
/// its GPU legs drew on that device, rather than skipped for want of one.
pub const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// Whether `canvas`, asked for the GPU, is drawing on one: the gate every
/// GPU leg passes through. `set_gpu(true)` falls back to the CPU in silence
/// where no GPU engine starts -- a build without a GPU feature, or a machine
/// without a device -- and a leg that went on would compare the CPU with
/// itself and pass.
///
/// Where the canvas is on the CPU, [`has_gpu`] decides what that means: a
/// machine with no GPU, which fails the test where one is required and
/// otherwise skips the leg, or a canvas the engine draws on the CPU by
/// design while a plain one is on the GPU -- a float canvas on Metal --
/// which skips. Either skip says so on stderr, which `cargo test` shows
/// with `--nocapture` or a failure.
pub fn on_gpu(canvas: &Canvas) -> bool {
    let engine = canvas.engine_kind();
    if engine == EngineKind::Gpu {
        require(engine);
        return true;
    }
    if has_gpu() {
        eprintln!(
            "skipped: this canvas is drawn on {engine:?} by design, a plain one on the GPU"
        );
    }
    false
}

/// Whether a plain canvas asking for the GPU is drawing on one: the gate
/// for a test that has only a GPU leg. Where it is not, a GPU that is
/// required fails the test, naming the engine it got: one [`REQUIRE_GPU`]
/// names, which the engine has to be on, and Metal where it has a device,
/// so that a Mac whose GPU engine broke does not skip. Otherwise it says
/// `skipped: no GPU` on stderr.
pub fn has_gpu() -> bool {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    let engine = probe.engine_kind();
    require(engine);
    if engine != EngineKind::Gpu {
        eprintln!("skipped: no GPU, the engine is {engine:?}");
    }
    engine == EngineKind::Gpu
}

/// Fails where a GPU is required and `engine` is not on it, or not on the
/// device [`REQUIRE_GPU`] names -- see [`has_gpu`].
fn require(engine: EngineKind) {
    if let Ok(required) = env::var(REQUIRE_GPU) {
        let backend = BackendInfo::query();
        assert!(
            engine == EngineKind::Gpu
                && backend
                    .device
                    .as_deref()
                    .is_some_and(|device| device.contains(&required)),
            "{REQUIRE_GPU} asks for a GPU named {required:?}; the engine is \
             {engine:?} on {:?}, error {:?}",
            backend.device,
            backend.error,
        );
    }
    assert!(
        engine == EngineKind::Gpu || !has_metal_device(),
        "Metal has a device, but the GPU engine did not start on it: {engine:?}"
    );
}

/// Whether Metal has a device to draw on, which a build with `metal` then
/// has to use.
#[cfg(feature = "metal")]
fn has_metal_device() -> bool {
    objc2_metal::MTLCreateSystemDefaultDevice().is_some()
}

/// Whether Metal has a device to draw on: never, without `metal`.
#[cfg(not(feature = "metal"))]
fn has_metal_device() -> bool {
    false
}

/// The engines a test draws each case on, as `gpu` flags: the CPU always,
/// and the GPU where [`has_gpu`] says there is one.
pub fn engines() -> Vec<bool> {
    let gpu = has_gpu();
    [false, true].into_iter().filter(|&g| !g || gpu).collect()
}

/// `svg` rendered by Skia's SVG DOM at `scale` device pixels to a unit,
/// onto a transparent eight-bit sRGB surface of `width` by `height`: its
/// pixels, premultiplied RGBA.
///
/// Composited in sRGB, as a browser composites an SVG and as the canvas
/// composites what the SVG was exported from. [`Svg::rasterize`] draws
/// into a linear-light surface instead, where half-alpha red over blue
/// lands 188, 0, 187 rather than 128, 0, 127, so it cannot tell an SVG
/// that composites as the canvas does from one that does not.
pub fn svg_in_srgb(svg: &str, width: i32, height: i32, scale: f32) -> Vec<u8> {
    use skia_safe::{
        AlphaType, ColorSpace, ColorType, FontMgr, ImageInfo, surfaces, svg,
    };
    let mut dom =
        svg::Dom::from_str(svg, FontMgr::empty()).expect("the SVG parses");
    dom.set_container_size((width as f32, height as f32));
    let info = ImageInfo::new(
        (width, height),
        ColorType::RGBA8888,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
    let mut surface = surfaces::raster(&info, None, None).expect("a surface");
    surface.canvas().scale((scale, scale));
    dom.render(surface.canvas());
    let mut pixels = vec![0; info.compute_min_byte_size()];
    assert!(
        surface.read_pixels(&info, &mut pixels, info.min_row_bytes(), (0, 0)),
        "the render reads back"
    );
    pixels
}
