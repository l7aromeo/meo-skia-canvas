//! Stable Rust-only facade for `skia-canvas`.
//!
//! Rust library consumers should use the types in this module; the Neon/JS
//! modules at the crate root are kept for Node addon compatibility and
//! intentionally leak `skia_safe` and `neon` types in their public
//! signatures.
//!
//! Public signatures in this module never expose `skia_safe` or `neon`
//! types -- a compile-time pin in
//! `tests/native_studio_renderer_adapter.rs` verifies this.
//!
//! See the [crate-level docs](crate) for a worked example. The repository
//! has a longer reference at [`docs/api/native-rust.md`][api-doc].
//!
//! [api-doc]: https://github.com/phyrondev/phyron-skia-canvas/blob/main/docs/api/native-rust.md

pub mod backend;
pub mod color;
pub mod error;
pub mod filter;
pub mod font;
pub mod geometry;
pub mod image;
pub mod paint;
pub mod path;
pub mod pixels;
pub mod recorder;
pub mod shader;
pub mod surface;
pub mod text;

pub use backend::NativeBackend;
pub use color::{LinearColorSpace, OutputColorSpace, RgbaLinear};
pub use error::NativeError;
pub use filter::{NativeColorFilter, NativeImageFilter};
pub use font::NativeFontManager;
pub use geometry::{NativeAffine, Point, Rect, Size};
pub use image::NativeImage;
pub use paint::{BlendMode, DashPattern, NativePaint, PaintStyle, StrokeCap};
pub use path::{FillRule, NativePath};
pub use pixels::{
    AlphaMode, ExportedPixels, PixelColorSpace, PixelDepth, PixelExportOptions, PixelFormat,
    RawFrame, RawFrameOptions, SamplingMode, SurfaceOptions,
};
pub use recorder::{NativeCanvas, NativeRecorder};
pub use shader::{GradientInterpolation, GradientStop, NativeShader};
pub use surface::NativeSurface;
pub use text::{
    NativeLineMetrics, NativeTextEngine, NativeTextLayout, RichTextSpan, TextAlign, TextBoxOptions,
    TextDecoration, TextDecorationStyle, TextShadow, TextSlant, TextStyle, VerticalAlign,
};
