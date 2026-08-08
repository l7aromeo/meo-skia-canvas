//! Node.js / Neon binding surface.
//!
//! These modules implement the `meo-skia-canvas` npm package's
//! `CanvasRenderingContext2D` and friends. They intentionally expose
//! `skia_safe` and `neon` types and are **not** a supported Rust API --
//! Rust consumers use the crate-root modules (and [`crate::prelude`]).
//! The whole subtree is `pub(crate)`; nothing here is part of the public
//! crate API.

pub mod canvas;
pub mod color_filter;
pub mod filter;
pub mod font_library;
pub mod gradient;
pub mod image;
pub mod image_filter;
pub mod mask_filter;
pub mod paragraph;
pub mod path;
pub mod pattern;
pub mod shader;
pub mod texture;
pub mod typography;
pub mod utils;
