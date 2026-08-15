//! GPU-accelerated, multi-threaded HTML Canvas-compatible 2D rendering for
//! Rust and Node, powered by [Skia].
//!
//! # Rust consumers: start at the crate root
//!
//! Every public type is reachable as `meo_skia_canvas::Thing`, so nothing has
//! to be looked up by module first. The modules group them by subject for
//! reading; the [`prelude`] globs the same set for anyone who prefers one.
//! No signature anywhere in the crate exposes a `skia_safe` or `neon` type,
//! [`gui`] included, and the Node/Neon binding lives under the internal
//! `node` module. `scripts/check-public-api.mjs` reads rustdoc's JSON on
//! every push and fails on a leak, with no module exempted -- the claim is
//! checked rather than maintained by hand.
//!
//! # The shape of it
//!
//! [`Canvas`] + [`Context2D`] are the API. Method names and argument order
//! match `CanvasRenderingContext2D`, so knowledge carries over from
//! JavaScript: a graphics state you mutate -- fill style, transform, clip --
//! and an encode straight to PNG, JPEG, WebP, GIF, APNG, TIFF, ICO, BMP,
//! AVIF, PDF or SVG.
//!
//! Everything else is the vocabulary those two speak -- [`RgbaLinear`],
//! [`Path2D`], [`Shader`], [`Image`], the filters, the text types -- and one
//! draw usually reaches across several, which is why they are at the root
//! rather than behind their modules.
//!
//! Drawing something and saving it:
//!
//! ```no_run
//! use meo_skia_canvas::prelude::*;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut canvas = Canvas::new(800.0, 400.0);
//! {
//!     let ctx = canvas.context();
//!     ctx.set_fill_style(RgbaLinear::opaque(0.05, 0.05, 0.08));
//!     ctx.fill_rect(0.0, 0.0, 800.0, 400.0);
//!
//!     ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.35, 0.2));
//!     ctx.set_font(&Font::new("Helvetica", 48.0));
//!     ctx.fill_text("Hello", 40.0, 120.0, None);
//! }
//!
//! canvas.to_file("hello.png", &EncodeOptions::default())?;
//! # Ok(())
//! # }
//! ```
//!
//! A canvas composites in the color space it was built with, and an export
//! converts out of it:
//!
//! ```no_run
//! use meo_skia_canvas::prelude::*;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut canvas = Canvas::with_options(
//!     1920.0,
//!     1080.0,
//!     CanvasOptions {
//!         color_space: PixelColorSpace::DisplayP3,
//!         ..CanvasOptions::default()
//!     },
//! )?;
//!
//! let ctx = canvas.context();
//! // Wider than sRGB can name, and the canvas is built to hold it.
//! ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
//! ctx.fill_rect(100.0, 100.0, 200.0, 100.0);
//!
//! let pixels = ctx.get_image_data(0.0, 0.0, 200.0, 100.0)?;
//! # let _ = pixels;
//! # Ok(())
//! # }
//! ```
//!
//! # Colors are premultiplied and linear
//!
//! [`RgbaLinear`] is **not** the 0-255 sRGB triple a CSS
//! color parses to. Components are linear-light and premultiplied by alpha,
//! so `RgbaLinear::opaque(0.5, 0.5, 0.5)` is not mid-gray on screen -- it
//! encodes to sRGB byte 188.
//!
//! When porting a CSS color, use the sRGB constructors and the conversion
//! happens for you:
//!
//! ```
//! use meo_skia_canvas::prelude::*;
//!
//! # fn main() -> Result<(), meo_skia_canvas::error::Error> {
//! let grey = RgbaLinear::from_srgb8(0x80, 0x80, 0x80, 1.0); // "#808080"
//! let same = RgbaLinear::from_hex("#808080")?;
//! let translucent = RgbaLinear::from_srgb8(255, 0, 0, 0.5); // rgba(255,0,0,.5)
//! # let _ = (grey, same, translucent);
//! # Ok(())
//! # }
//! ```
//!
//! See [`docs/api/native-rust.md`][api-doc] in the repository for a longer
//! reference (color spaces, alpha semantics, surfaces, paint, paths, shaders,
//! filters, images, text, fonts).
//!
//! # Cargo features
//!
//! - `vulkan` -- enable the Vulkan backend (Linux / Windows).
//! - `metal` -- enable the Metal backend (macOS).
//! - `window` -- enable the [`winit`]-backed GUI window/event loop.
//! - `freetype` -- bundle FreeType + WOFF2 support for font registration on
//!   Linux containers / minimal images.
//! - `node-addon` -- register the `#[neon::main]` entry point so the resulting
//!   cdylib loads as a Node.js addon. Pure-Rust consumers should leave this
//!   off.
//!
//! Pure-Rust consumers typically depend with `default-features = false` and
//! pick the backend they need:
//!
//! ```toml
//! [dependencies]
//! meo-skia-canvas = { git = "https://github.com/l7aromeo/meo-skia-canvas", default-features = false, features = ["vulkan", "freetype"] }
//! ```
//!
//! [Skia]: https://skia.org
//! [api-doc]: https://github.com/l7aromeo/meo-skia-canvas/blob/main/docs/api/native-rust.md
//! [`winit`]: https://docs.rs/winit

#![allow(unused_braces)]
#![allow(clippy::unnecessary_wraps)]
// This crate publishes to docs.rs, so an undocumented public item is a gap a
// user sees. The lint only reaches the public API: the Neon binding is
// `pub(crate)`, so if it ever fires there, the module's visibility is what
// needs fixing, not the docs.
#![warn(missing_docs)]

#[cfg(feature = "node-addon")]
use neon::prelude::*;

// The public Rust API. Each module sits at the crate root; `prelude`
// re-exports their contents so `use meo_skia_canvas::prelude::*;` needs no
// module prefixes. Public signatures never expose `skia_safe` or `neon`
// types -- the Node/Neon binding lives under the internal `node` module.
/// Entry point: the canvas, its pages, and the engine that draws them.
pub mod canvas;
/// Colors and color spaces.
pub mod color;
pub mod context2d;
// The CSS-string parsers behind the `_css` setters. Crate-private: what a
// caller wants is the setter, not the grammar behind it.
pub(crate) mod css;
// The decoders for the formats Skia cannot read, which is APNG and nothing
// else. Crate-private, as `encode` is.
pub(crate) mod decode;
// The encoders for formats Skia has none for. Crate-private: a caller names a
// format, and which crate writes its bytes is not part of the promise.
pub(crate) mod encode;
/// The crate's error type.
pub mod error;
pub mod export;
/// Image, color, and mask filters.
pub mod filter;
/// Font registration and variable-font axes.
pub mod font;
pub mod geometry;
/// Decoded raster images.
pub mod image;
/// Fill and stroke styling.
pub mod paint;
/// Vector paths.
pub mod path;

pub mod pattern;
/// Pixel layouts for reading surfaces back and writing them.
pub mod pixels;
/// Gradients and procedural shaders.
pub mod shader;
/// Text layout and styling.
pub mod text;

pub mod texture;

// The whole public API, re-exported at the crate root.
//
// The modules below group the types by subject, which is how they are
// documented -- but a caller should not have to know that `FillRule` lives in
// `path` and `BlendMode` in `paint` to write one draw. `Canvas::to_buffer`
// alone speaks types from four of them. So every public item is reachable as
// `meo_skia_canvas::Thing`, and the modules remain for anyone who wants the
// narrower import.
#[doc(inline)]
pub use crate::{
    canvas::*, color::*, context2d::*, error::*, export::*, filter::*, font::*,
    geometry::*, image::*, paint::*, path::*, pattern::*, pixels::*, shader::*,
    text::*, texture::*,
};

/// Glob-importable re-export of the whole public API:
/// `use meo_skia_canvas::prelude::*;`.
///
/// The same set the crate root exposes, for callers who prefer a prelude to a
/// glob of the crate itself.
pub mod prelude {
    pub use crate::{
        canvas::*, color::*, context2d::*, error::*, export::*, filter::*,
        font::*, geometry::*, image::*, paint::*, path::*, pattern::*,
        pixels::*, shader::*, text::*, texture::*,
    };

    // Windowing, which is feature-gated and so cannot be globbed with the
    // rest. Named rather than starred: `gui` also holds the loop's own
    // vocabulary, and a prelude that dragged in `Frame` or `LoopMode`
    // alongside `Window` would be importing the machinery to use the API.
    #[cfg(feature = "window")]
    pub use crate::gui::{
        app::App,
        event::{ModifierKeys, UiEvent},
        key::Key,
        session::Window,
        window::Fit,
    };
}

/// The seven types the two surfaces still spell differently.
///
/// Most of the crate answers to one name on both sides: [`Path2D`],
/// [`Paragraph`], [`ParagraphBuilder`], [`FontLibrary`], [`ImageData`],
/// [`ColorMatrix`], [`Canvas`], [`Image`], `ColorFilter`, `ImageFilter`,
/// `MaskFilter`, `Shader`, `TextMetrics`, `TextDecoration`, `App` and
/// `Window` are written the same in Rust as in JavaScript.
///
/// What is left is the prefixes JavaScript carries only because it has one
/// flat global namespace to keep tidy. Rust has modules, so
/// `texture::Texture` says what `CanvasTexture` says with less of it, and
/// `DOM` names a document object model this crate does not have. Those
/// seven are shortened -- and re-exported here under the long names anyway, so
/// a file being ported from the Node binding still compiles halfway through
/// the translation.
///
/// Every item here is a re-export, not a new type: a `CanvasTexture` *is* a
/// [`Texture`] and the two are interchangeable everywhere.
pub mod js_names {
    pub use crate::{
        context2d::Context2D as CanvasRenderingContext2D,
        geometry::{Affine as DOMMatrix, Point as DOMPoint, Rect as DOMRect},
        pattern::Pattern as CanvasPattern,
        shader::Shader as CanvasGradient,
        texture::Texture as CanvasTexture,
    };
}

// Shared internal infrastructure (not part of the public API).
pub(crate) mod context;
pub(crate) mod gpu;

/// winit-backed windowing, behind the `window` feature.
///
/// Backs the Node addon's window support and is drivable from Rust over the
/// same loop. [`gui::session::Window`] configures one and attaches the draw
/// and event handlers, and [`gui::app::App::run`] opens every window queued
/// that way and blocks until the last closes. Both are in the [`prelude`].
///
/// The live winit-backed window is not this type and is not public: windows
/// exist only inside a running event loop, so what a caller holds beforehand
/// is what one gets made from.
#[cfg(feature = "window")]
pub mod gui;

// Node.js / Neon binding -- internal; intentionally leaks `skia_safe` /
// `neon` types, so it is `pub(crate)` and never part of the public API.
// `dead_code` is allowed across the subtree: many binding methods exist
// for JS-surface symmetry and aren't all reachable from Rust (and `pub`
// no longer keeps them "live" now that the module is crate-private).
#[allow(dead_code)]
pub(crate) mod node;

// Surface the non-colliding binding modules at the crate root so the
// binding code keeps using `crate::gradient`, `crate::utils`, etc. The
// names that collide with a public root module (canvas/filter/image/path/
// pattern/shader/texture) belong to the public API; the binding refers to
// those via `crate::node`.
#[allow(unused_imports)]
pub(crate) use node::{
    color_filter, font_library, gradient, image_filter, mask_filter, paragraph,
    typography, utils,
};

#[cfg(feature = "node-addon")]
use context::api as ctx;

/// Module-level function to get backend status without creating a canvas.
///
/// Returns JSON string with renderer, api, device, driver, threads, and
/// gpuAvailable fields.
#[cfg(feature = "node-addon")]
fn backend(mut cx: FunctionContext) -> JsResult<JsString> {
    // `RenderingEngine::status` rather than the free `get_backend_status`
    // that used to live in `gpu`: that one was deleted with the render-target
    // layer, and nothing noticed because a stale `lib/skia.node` kept the
    // JavaScript suite green -- `just test` only builds the addon when the
    // file is missing.
    let mut status = gpu::RenderingEngine::default().status(false);
    // `threads` and `gpuAvailable` are part of what `backend()` returns and
    // are asserted by the JavaScript suite; the engine's own status carries
    // neither, since a canvas reports them separately.
    if let serde_json::Value::Object(ref mut map) = status {
        map.insert(
            "threads".to_string(),
            serde_json::json!(rayon::current_num_threads()),
        );
        map.insert(
            "gpuAvailable".to_string(),
            serde_json::json!(gpu::RenderingEngine::GPU.selectable()),
        );
    }
    Ok(cx.string(status.to_string()))
}

/// Module-level function describing every format the addon can encode.
///
/// Returns a JSON array of
/// `{name, mime, extension, aliases, spansPages, animated, inferable,
/// bitDepths}`, read once when the JavaScript side loads.
///
/// It is here so there is one table rather than two. The binding used to
/// keep its own copy of the extension and media-type maps, the list of names
/// its error message offered, and -- the one that would have bitten -- a
/// bare `format == "pdf"` deciding which exports gather every page. Adding a
/// multi-page raster format with that line in place would have quietly
/// encoded the last page alone and reported nothing wrong. The compiler
/// cannot reach across the boundary to catch that, so the boundary asks
/// instead of remembering.
#[cfg(feature = "node-addon")]
fn formats(mut cx: FunctionContext) -> JsResult<JsString> {
    let described: Vec<_> = export::ImageFormat::all()
        .map(|format| {
            let traits = format.traits();
            serde_json::json!({
                "name": traits.name,
                "mime": traits.mime,
                "extension": traits.extension,
                "aliases": traits.aliases,
                "spansPages": format.spans_pages(),
                "animated": traits.animated,
                "inferable": traits.inferable,
                // Empty for every format that reads its depth from the
                // canvas, which is every one but AVIF.
                "bitDepths": traits.depths,
            })
        })
        .collect();
    Ok(cx.string(serde_json::json!(described).to_string()))
}

/// Module-level function describing every `colorType` the addon accepts.
///
/// Returns a JSON array of `{name, bytes}`, read once when the JavaScript
/// side loads. Here for the reason [`formats`] is: the binding kept its own
/// copy of these names in `pixelSize`, and the two drifted -- the addon took
/// `"N32"` and `pixelSize` threw on it, while both listed `"RGBA8888"`
/// twice. The bytes come from Skia rather than from a hand-written table,
/// so a type whose width this crate has never thought about still reports
/// the right one.
#[cfg(feature = "node-addon")]
fn color_types(mut cx: FunctionContext) -> JsResult<JsString> {
    let described: Vec<_> = node::utils::COLOR_TYPES
        .iter()
        .map(|(name, color_type)| {
            serde_json::json!({
                "name": name,
                "bytes": color_type.bytes_per_pixel(),
            })
        })
        .collect();
    Ok(cx.string(serde_json::json!(described).to_string()))
}

#[cfg(feature = "node-addon")]
#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    // initialize thread pool w/ non-default size if requested
    if let Ok(value) = std::env::var("SKIA_CANVAS_THREADS")
        && let Ok(num) = value.parse::<usize>()
        && let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(num)
            .build_global()
    {
        eprintln!("Warning: failed to set custom thread pool size: {}", e);
    }

    // -- Image -------------------------------------------------------------------------------------

    cx.export_function("Image_new", node::image::new)?;
    cx.export_function("Image_get_src", node::image::get_src)?;
    cx.export_function("Image_set_src", node::image::set_src)?;
    cx.export_function("Image_set_data", node::image::set_data)?;
    cx.export_function("Image_get_width", node::image::get_width)?;
    cx.export_function("Image_get_height", node::image::get_height)?;
    cx.export_function("Image_get_complete", node::image::get_complete)?;
    cx.export_function("Image_get_frames", node::image::get_frames)?;
    cx.export_function("Image_get_delays", node::image::get_delays)?;
    cx.export_function("Image_takeFrame", node::image::take_frame)?;
    cx.export_function("Image_pixels", node::image::pixels)?;

    // -- Path2D ------------------------------------------------------------------------------------

    cx.export_function("Path2D_new", node::path::new)?;
    cx.export_function("Path2D_from_path", node::path::from_path)?;
    cx.export_function("Path2D_from_svg", node::path::from_svg)?;
    cx.export_function("Path2D_addPath", node::path::addPath)?;
    cx.export_function("Path2D_closePath", node::path::closePath)?;
    cx.export_function("Path2D_moveTo", node::path::moveTo)?;
    cx.export_function("Path2D_lineTo", node::path::lineTo)?;
    cx.export_function("Path2D_bezierCurveTo", node::path::bezierCurveTo)?;
    cx.export_function(
        "Path2D_quadraticCurveTo",
        node::path::quadraticCurveTo,
    )?;
    cx.export_function("Path2D_conicCurveTo", node::path::conicCurveTo)?;
    cx.export_function("Path2D_arc", node::path::arc)?;
    cx.export_function("Path2D_arcTo", node::path::arcTo)?;
    cx.export_function("Path2D_ellipse", node::path::ellipse)?;
    cx.export_function("Path2D_rect", node::path::rect)?;
    cx.export_function("Path2D_roundRect", node::path::roundRect)?;
    cx.export_function("Path2D_op", node::path::op)?;
    cx.export_function("Path2D_interpolate", node::path::interpolate)?;
    cx.export_function("Path2D_simplify", node::path::simplify)?;
    cx.export_function("Path2D_unwind", node::path::unwind)?;
    cx.export_function("Path2D_round", node::path::round)?;
    cx.export_function("Path2D_trim", node::path::trim)?;
    cx.export_function("Path2D_jitter", node::path::jitter)?;
    cx.export_function("Path2D_offset", node::path::offset)?;
    cx.export_function("Path2D_transform", node::path::transform)?;
    cx.export_function("Path2D_bounds", node::path::bounds)?;
    cx.export_function("Path2D_contains", node::path::contains)?;
    cx.export_function("Path2D_edges", node::path::edges)?;
    cx.export_function("Path2D_get_d", node::path::get_d)?;
    cx.export_function("Path2D_set_d", node::path::set_d)?;

    // -- CanvasGradient
    // ----------------------------------------------------------------------------

    cx.export_function("CanvasGradient_linear", gradient::linear)?;
    cx.export_function("CanvasGradient_radial", gradient::radial)?;
    cx.export_function("CanvasGradient_conic", gradient::conic)?;
    cx.export_function("CanvasGradient_addColorStop", gradient::addColorStop)?;
    cx.export_function("CanvasGradient_repr", gradient::repr)?;
    cx.export_function(
        "CanvasGradient_get_interpolation",
        gradient::get_interpolation,
    )?;
    cx.export_function(
        "CanvasGradient_set_interpolation",
        gradient::set_interpolation,
    )?;
    cx.export_function(
        "CanvasGradient_get_hueInterpolation",
        gradient::get_hueInterpolation,
    )?;
    cx.export_function(
        "CanvasGradient_set_hueInterpolation",
        gradient::set_hueInterpolation,
    )?;

    // -- CanvasPattern
    // -----------------------------------------------------------------------------

    cx.export_function("CanvasPattern_from_image", node::pattern::from_image)?;
    cx.export_function(
        "CanvasPattern_from_image_data",
        node::pattern::from_image_data,
    )?;
    cx.export_function(
        "CanvasPattern_from_canvas",
        node::pattern::from_canvas,
    )?;
    cx.export_function(
        "CanvasPattern_setTransform",
        node::pattern::setTransform,
    )?;
    cx.export_function("CanvasPattern_repr", node::pattern::repr)?;

    // -- CanvasTexture
    // -----------------------------------------------------------------------------

    cx.export_function("CanvasTexture_new", node::texture::new)?;
    cx.export_function("CanvasTexture_repr", node::texture::repr)?;

    // -- ColorFilter
    // -------------------------------------------------------------------------------

    cx.export_function("ColorFilter_makeMatrix", color_filter::makeMatrix)?;
    cx.export_function(
        "ColorFilter_makeSRGBToLinearGamma",
        color_filter::makeSRGBToLinearGamma,
    )?;
    cx.export_function(
        "ColorFilter_makeLinearToSRGBGamma",
        color_filter::makeLinearToSRGBGamma,
    )?;
    cx.export_function("ColorFilter_makeBlend", color_filter::makeBlend)?;
    cx.export_function("ColorFilter_makeCompose", color_filter::makeCompose)?;
    cx.export_function("ColorFilter_makeLerp", color_filter::makeLerp)?;
    cx.export_function(
        "ColorFilter_makeHSLAMatrix",
        color_filter::makeHSLAMatrix,
    )?;
    cx.export_function("ColorFilter_makeLighting", color_filter::makeLighting)?;
    cx.export_function(
        "ColorFilter_makeLumaColorFilter",
        color_filter::makeLumaColorFilter,
    )?;
    cx.export_function("ColorFilter_makeTable", color_filter::makeTable)?;
    cx.export_function(
        "ColorFilter_makeTableARGB",
        color_filter::makeTableARGB,
    )?;
    cx.export_function("ColorFilter_repr", color_filter::repr)?;
    cx.export_function("ColorFilter_delete", color_filter::delete)?;

    // -- ImageFilter
    // -------------------------------------------------------------------------------

    cx.export_function(
        "ImageFilter_makeColorFilter",
        image_filter::makeColorFilter,
    )?;
    cx.export_function("ImageFilter_makeCompose", image_filter::makeCompose)?;
    cx.export_function("ImageFilter_makeBlur", image_filter::makeBlur)?;
    cx.export_function(
        "ImageFilter_makeDropShadow",
        image_filter::makeDropShadow,
    )?;
    cx.export_function(
        "ImageFilter_makeDropShadowOnly",
        image_filter::makeDropShadowOnly,
    )?;
    cx.export_function("ImageFilter_makeOffset", image_filter::makeOffset)?;
    cx.export_function("ImageFilter_makeDilate", image_filter::makeDilate)?;
    cx.export_function("ImageFilter_makeErode", image_filter::makeErode)?;
    cx.export_function("ImageFilter_makeMerge", image_filter::makeMerge)?;
    cx.export_function("ImageFilter_makeEmpty", image_filter::makeEmpty)?;
    cx.export_function("ImageFilter_makeTile", image_filter::makeTile)?;
    // Advanced ImageFilter methods
    cx.export_function("ImageFilter_makeBlend", image_filter::makeBlend)?;
    cx.export_function(
        "ImageFilter_makeArithmetic",
        image_filter::makeArithmetic,
    )?;
    cx.export_function(
        "ImageFilter_makeDisplacementMap",
        image_filter::makeDisplacementMap,
    )?;
    cx.export_function(
        "ImageFilter_makeMatrixConvolution",
        image_filter::makeMatrixConvolution,
    )?;
    cx.export_function(
        "ImageFilter_makeMatrixTransform",
        image_filter::makeMatrixTransform,
    )?;
    cx.export_function(
        "ImageFilter_makeMagnifier",
        image_filter::makeMagnifier,
    )?;
    cx.export_function("ImageFilter_makeCrop", image_filter::makeCrop)?;
    // Lighting ImageFilter methods
    cx.export_function(
        "ImageFilter_makeDistantLitDiffuse",
        image_filter::makeDistantLitDiffuse,
    )?;
    cx.export_function(
        "ImageFilter_makePointLitDiffuse",
        image_filter::makePointLitDiffuse,
    )?;
    cx.export_function(
        "ImageFilter_makeSpotLitDiffuse",
        image_filter::makeSpotLitDiffuse,
    )?;
    cx.export_function(
        "ImageFilter_makeDistantLitSpecular",
        image_filter::makeDistantLitSpecular,
    )?;
    cx.export_function(
        "ImageFilter_makePointLitSpecular",
        image_filter::makePointLitSpecular,
    )?;
    cx.export_function(
        "ImageFilter_makeSpotLitSpecular",
        image_filter::makeSpotLitSpecular,
    )?;
    cx.export_function("ImageFilter_repr", image_filter::repr)?;
    cx.export_function("ImageFilter_delete", image_filter::delete)?;

    // -- MaskFilter
    // --------------------------------------------------------------------

    cx.export_function("MaskFilter_makeBlur", mask_filter::makeBlur)?;
    cx.export_function("MaskFilter_delete", mask_filter::delete)?;

    // -- Shader
    // --------------------------------------------------------------------

    cx.export_function(
        "Shader_makeFractalNoise",
        node::shader::makeFractalNoise,
    )?;
    cx.export_function("Shader_makeTurbulence", node::shader::makeTurbulence)?;
    cx.export_function("Shader_delete", node::shader::delete)?;

    // -- FontLibrary
    // -------------------------------------------------------------------------------

    cx.export_function("FontLibrary_get_families", font_library::get_families)?;
    cx.export_function("FontLibrary_has", font_library::has)?;
    cx.export_function("FontLibrary_family", font_library::family)?;
    cx.export_function("FontLibrary_addFamily", font_library::addFamily)?;
    cx.export_function(
        "FontLibrary_addFamilyFromData",
        font_library::addFamilyFromData,
    )?;
    cx.export_function("FontLibrary_reset", font_library::reset)?;

    // -- ParagraphBuilder
    // --------------------------------------------------------------------------

    cx.export_function("ParagraphBuilder_new", paragraph::new)?;
    cx.export_function("ParagraphBuilder_pushStyle", paragraph::pushStyle)?;
    cx.export_function("ParagraphBuilder_pop", paragraph::pop)?;
    cx.export_function("ParagraphBuilder_addText", paragraph::addText)?;
    cx.export_function(
        "ParagraphBuilder_addPlaceholder",
        paragraph::addPlaceholder,
    )?;
    cx.export_function("ParagraphBuilder_build", paragraph::build)?;

    // -- Paragraph
    // ------------------------------------------------------------------------------

    cx.export_function("Paragraph_layout", paragraph::layout)?;
    cx.export_function("Paragraph_paint", paragraph::paint)?;
    cx.export_function("Paragraph_getHeight", paragraph::getHeight)?;
    cx.export_function("Paragraph_getLongestLine", paragraph::getLongestLine)?;
    cx.export_function("Paragraph_getMaxWidth", paragraph::getMaxWidth)?;
    cx.export_function(
        "Paragraph_getMaxIntrinsicWidth",
        paragraph::getMaxIntrinsicWidth,
    )?;
    cx.export_function(
        "Paragraph_getMinIntrinsicWidth",
        paragraph::getMinIntrinsicWidth,
    )?;
    cx.export_function(
        "Paragraph_getAlphabeticBaseline",
        paragraph::getAlphabeticBaseline,
    )?;
    cx.export_function(
        "Paragraph_getIdeographicBaseline",
        paragraph::getIdeographicBaseline,
    )?;
    cx.export_function("Paragraph_getLineMetrics", paragraph::getLineMetrics)?;
    cx.export_function(
        "Paragraph_getGlyphPositionAtCoordinate",
        paragraph::getGlyphPositionAtCoordinate,
    )?;
    cx.export_function(
        "Paragraph_getRectsForRange",
        paragraph::getRectsForRange,
    )?;
    cx.export_function(
        "Paragraph_didExceedMaxLines",
        paragraph::didExceedMaxLines,
    )?;
    cx.export_function(
        "Paragraph_getNumberOfLines",
        paragraph::getNumberOfLines,
    )?;
    cx.export_function(
        "Paragraph_getRectsForPlaceholders",
        paragraph::getRectsForPlaceholders,
    )?;
    cx.export_function(
        "Paragraph_getUnresolvedCodepoints",
        paragraph::getUnresolvedCodepoints,
    )?;

    // -- Backend (module-level)
    // --------------------------------------------------------------------

    cx.export_function("backend", backend)?;
    cx.export_function("formats", formats)?;
    cx.export_function("colorTypes", color_types)?;

    // -- Canvas ------------------------------------------------------------------------------------

    cx.export_function("Canvas_new", node::canvas::new)?;

    cx.export_function("Canvas_get_colorType", node::canvas::get_colorType)?;
    cx.export_function("Canvas_get_colorSpace", node::canvas::get_colorSpace)?;
    cx.export_function("Canvas_get_engine", node::canvas::get_engine)?;
    cx.export_function("Canvas_set_engine", node::canvas::set_engine)?;
    cx.export_function(
        "Canvas_get_engine_status",
        node::canvas::get_engine_status,
    )?;

    cx.export_function("Canvas_get_width", node::canvas::get_width)?;
    cx.export_function("Canvas_set_width", node::canvas::set_width)?;
    cx.export_function("Canvas_get_height", node::canvas::get_height)?;
    cx.export_function("Canvas_set_height", node::canvas::set_height)?;

    cx.export_function("Canvas_save", node::canvas::save)?;
    cx.export_function("Canvas_saveSync", node::canvas::saveSync)?;
    cx.export_function("Canvas_toBuffer", node::canvas::toBuffer)?;
    cx.export_function("Canvas_toBufferSync", node::canvas::toBufferSync)?;

    // -- Context
    // -----------------------------------------------------------------------------------

    cx.export_function("CanvasRenderingContext2D_new", ctx::new)?;
    cx.export_function("CanvasRenderingContext2D_resetSize", ctx::resetSize)?;
    cx.export_function("CanvasRenderingContext2D_get_size", ctx::get_size)?;
    cx.export_function("CanvasRenderingContext2D_set_size", ctx::set_size)?;
    cx.export_function("CanvasRenderingContext2D_reset", ctx::reset)?;

    // grid state
    cx.export_function("CanvasRenderingContext2D_save", ctx::save)?;
    cx.export_function("CanvasRenderingContext2D_restore", ctx::restore)?;
    cx.export_function("CanvasRenderingContext2D_saveLayer", ctx::saveLayer)?;
    cx.export_function("CanvasRenderingContext2D_transform", ctx::transform)?;
    cx.export_function("CanvasRenderingContext2D_translate", ctx::translate)?;
    cx.export_function("CanvasRenderingContext2D_scale", ctx::scale)?;
    cx.export_function("CanvasRenderingContext2D_rotate", ctx::rotate)?;
    cx.export_function(
        "CanvasRenderingContext2D_resetTransform",
        ctx::resetTransform,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_currentTransform",
        ctx::get_currentTransform,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_currentTransform",
        ctx::set_currentTransform,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_createProjection",
        ctx::createProjection,
    )?;

    // bézier paths
    cx.export_function("CanvasRenderingContext2D_beginPath", ctx::beginPath)?;
    cx.export_function("CanvasRenderingContext2D_rect", ctx::rect)?;
    cx.export_function("CanvasRenderingContext2D_roundRect", ctx::roundRect)?;
    cx.export_function("CanvasRenderingContext2D_arc", ctx::arc)?;
    cx.export_function("CanvasRenderingContext2D_ellipse", ctx::ellipse)?;
    cx.export_function("CanvasRenderingContext2D_moveTo", ctx::moveTo)?;
    cx.export_function("CanvasRenderingContext2D_lineTo", ctx::lineTo)?;
    cx.export_function("CanvasRenderingContext2D_arcTo", ctx::arcTo)?;
    cx.export_function(
        "CanvasRenderingContext2D_bezierCurveTo",
        ctx::bezierCurveTo,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_quadraticCurveTo",
        ctx::quadraticCurveTo,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_conicCurveTo",
        ctx::conicCurveTo,
    )?;
    cx.export_function("CanvasRenderingContext2D_closePath", ctx::closePath)?;
    cx.export_function(
        "CanvasRenderingContext2D_isPointInPath",
        ctx::isPointInPath,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_isPointInStroke",
        ctx::isPointInStroke,
    )?;
    cx.export_function("CanvasRenderingContext2D_clip", ctx::clip)?;

    // fill & stroke
    cx.export_function("CanvasRenderingContext2D_fill", ctx::fill)?;
    cx.export_function("CanvasRenderingContext2D_stroke", ctx::stroke)?;
    cx.export_function("CanvasRenderingContext2D_fillRect", ctx::fillRect)?;
    cx.export_function("CanvasRenderingContext2D_strokeRect", ctx::strokeRect)?;
    cx.export_function("CanvasRenderingContext2D_clearRect", ctx::clearRect)?;
    cx.export_function(
        "CanvasRenderingContext2D_get_fillStyle",
        ctx::get_fillStyle,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_fillStyle",
        ctx::set_fillStyle,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_strokeStyle",
        ctx::get_strokeStyle,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_strokeStyle",
        ctx::set_strokeStyle,
    )?;

    // line style
    cx.export_function(
        "CanvasRenderingContext2D_getLineDash",
        ctx::getLineDash,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_setLineDash",
        ctx::setLineDash,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineCap",
        ctx::get_lineCap,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineCap",
        ctx::set_lineCap,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineDashFit",
        ctx::get_lineDashFit,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineDashFit",
        ctx::set_lineDashFit,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineDashMarker",
        ctx::get_lineDashMarker,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineDashMarker",
        ctx::set_lineDashMarker,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineDashOffset",
        ctx::get_lineDashOffset,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineDashOffset",
        ctx::set_lineDashOffset,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineJoin",
        ctx::get_lineJoin,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineJoin",
        ctx::set_lineJoin,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_lineWidth",
        ctx::get_lineWidth,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_lineWidth",
        ctx::set_lineWidth,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_miterLimit",
        ctx::get_miterLimit,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_miterLimit",
        ctx::set_miterLimit,
    )?;

    // imagery
    cx.export_function("CanvasRenderingContext2D_drawImage", ctx::drawImage)?;
    cx.export_function("CanvasRenderingContext2D_drawCanvas", ctx::drawCanvas)?;
    cx.export_function(
        "CanvasRenderingContext2D_getImageData",
        ctx::getImageData,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_putImageData",
        ctx::putImageData,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_imageSmoothingEnabled",
        ctx::get_imageSmoothingEnabled,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_imageSmoothingEnabled",
        ctx::set_imageSmoothingEnabled,
    )?;
    cx.export_function("CanvasRenderingContext2D_get_dither", ctx::get_dither)?;
    cx.export_function("CanvasRenderingContext2D_set_dither", ctx::set_dither)?;
    cx.export_function(
        "CanvasRenderingContext2D_get_imageSmoothingQuality",
        ctx::get_imageSmoothingQuality,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_imageSmoothingQuality",
        ctx::set_imageSmoothingQuality,
    )?;

    // typography
    cx.export_function("CanvasRenderingContext2D_fillText", ctx::fillText)?;
    cx.export_function("CanvasRenderingContext2D_strokeText", ctx::strokeText)?;
    cx.export_function(
        "CanvasRenderingContext2D_measureText",
        ctx::measureText,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_outlineText",
        ctx::outlineText,
    )?;
    cx.export_function("CanvasRenderingContext2D_get_font", ctx::get_font)?;
    cx.export_function("CanvasRenderingContext2D_set_font", ctx::set_font)?;
    cx.export_function(
        "CanvasRenderingContext2D_get_textAlign",
        ctx::get_textAlign,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_textAlign",
        ctx::set_textAlign,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_textBaseline",
        ctx::get_textBaseline,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_textBaseline",
        ctx::set_textBaseline,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_direction",
        ctx::get_direction,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_direction",
        ctx::set_direction,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_letterSpacing",
        ctx::get_letterSpacing,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_letterSpacing",
        ctx::set_letterSpacing,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_wordSpacing",
        ctx::get_wordSpacing,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_wordSpacing",
        ctx::set_wordSpacing,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_fontHinting",
        ctx::get_fontHinting,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_fontHinting",
        ctx::set_fontHinting,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_fontVariant",
        ctx::get_fontVariant,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_fontVariant",
        ctx::set_fontVariant,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_fontStretch",
        ctx::get_fontStretch,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_fontStretch",
        ctx::set_fontStretch,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_textWrap",
        ctx::get_textWrap,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_textWrap",
        ctx::set_textWrap,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_textDecoration",
        ctx::get_textDecoration,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_textDecoration",
        ctx::set_textDecoration,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_fontVariationSettings",
        ctx::get_fontVariationSettings,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_fontVariationSettings",
        ctx::set_fontVariationSettings,
    )?;

    // effects
    cx.export_function(
        "CanvasRenderingContext2D_get_globalAlpha",
        ctx::get_globalAlpha,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_globalAlpha",
        ctx::set_globalAlpha,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_globalCompositeOperation",
        ctx::get_globalCompositeOperation,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_globalCompositeOperation",
        ctx::set_globalCompositeOperation,
    )?;
    cx.export_function("CanvasRenderingContext2D_get_filter", ctx::get_filter)?;
    cx.export_function("CanvasRenderingContext2D_set_filter", ctx::set_filter)?;
    cx.export_function(
        "CanvasRenderingContext2D_get_shadowBlur",
        ctx::get_shadowBlur,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_shadowBlur",
        ctx::set_shadowBlur,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_shadowColor",
        ctx::get_shadowColor,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_shadowColor",
        ctx::set_shadowColor,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_shadowOffsetX",
        ctx::get_shadowOffsetX,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_shadowOffsetY",
        ctx::get_shadowOffsetY,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_shadowOffsetX",
        ctx::set_shadowOffsetX,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_shadowOffsetY",
        ctx::set_shadowOffsetY,
    )?;

    // Skia filter properties (CanvasKit parity)
    cx.export_function(
        "CanvasRenderingContext2D_get_colorFilter",
        ctx::get_colorFilter,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_colorFilter",
        ctx::set_colorFilter,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_skiaImageFilter",
        ctx::get_skiaImageFilter,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_skiaImageFilter",
        ctx::set_skiaImageFilter,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_get_skiaMaskFilter",
        ctx::get_skiaMaskFilter,
    )?;
    cx.export_function(
        "CanvasRenderingContext2D_set_skiaMaskFilter",
        ctx::set_skiaMaskFilter,
    )?;

    // drawParagraph
    cx.export_function(
        "CanvasRenderingContext2D_drawParagraph",
        paragraph::drawParagraph,
    )?;

    // -- Window -----------------------------------------------------------------------------------

    #[cfg(feature = "window")]
    {
        cx.export_function("App_register", gui::register)?;
        cx.export_function("App_activate", gui::activate)?;
        cx.export_function("App_quit", gui::quit)?;
        cx.export_function("App_closeWindow", gui::close)?;
        cx.export_function("App_openWindow", gui::open)?;
        cx.export_function("App_setRate", gui::set_rate)?;
        cx.export_function("App_setMode", gui::set_mode)?;
    }

    Ok(())
}
