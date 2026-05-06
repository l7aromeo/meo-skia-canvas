use std::fmt;

use crate::native::backend::RenderEngine;
use crate::native::color::{LinearColorSpace, OutputColorSpace};
use crate::native::geometry::Rect;
use crate::native::pixels::{PixelColorSpace, PixelDepth, PixelFormat};

#[derive(Debug, Clone, PartialEq)]
pub enum NativeError {
    InvalidDimensions { width: f32, height: f32 },
    InvalidRect { rect: Rect },
    UnsupportedColorSpace { color_space: LinearColorSpace },
    UnsupportedOutputColorSpace { color_space: OutputColorSpace },
    UnsupportedPixelColorSpace { color_space: PixelColorSpace },
    UnsupportedPixelFormat { pixel_format: PixelFormat },
    UnsupportedPixelDepth { depth: PixelDepth },
    InvalidStride { expected: usize, actual: usize },
    InvalidByteLength { expected: usize, actual: usize },
    SurfaceCreate { reason: String },
    DecodeImage { reason: String },
    InvalidSvgPath { reason: String },
    InvalidGradient { reason: String },
    FontRegister { reason: String },
    FilterCreate { reason: String },
    Render { reason: String },
    PixelReadback { reason: String },
    PixelWrite { reason: String },
    /// Caller pinned [`RenderEngine::Gpu`] but no GPU backend is
    /// compiled in or the runtime cannot reach a device.
    EngineUnavailable { engine: RenderEngine, reason: String },
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid dimensions: {width}x{height}")
            }
            Self::InvalidRect { rect } => write!(f, "invalid rect: {rect:?}"),
            Self::UnsupportedColorSpace { color_space } => {
                write!(f, "unsupported linear color space: {color_space:?}")
            }
            Self::UnsupportedOutputColorSpace { color_space } => {
                write!(f, "unsupported output color space: {color_space:?}")
            }
            Self::UnsupportedPixelColorSpace { color_space } => {
                write!(f, "unsupported pixel color space: {color_space:?}")
            }
            Self::UnsupportedPixelFormat { pixel_format } => {
                write!(f, "unsupported pixel format: {pixel_format:?}")
            }
            Self::UnsupportedPixelDepth { depth } => {
                write!(f, "unsupported pixel depth: {depth:?}")
            }
            Self::InvalidStride { expected, actual } => {
                write!(f, "invalid stride: expected {expected}, got {actual}")
            }
            Self::InvalidByteLength { expected, actual } => {
                write!(f, "invalid byte length: expected {expected}, got {actual}")
            }
            Self::SurfaceCreate { reason } => write!(f, "surface create failed: {reason}"),
            Self::DecodeImage { reason } => write!(f, "decode image failed: {reason}"),
            Self::InvalidSvgPath { reason } => write!(f, "invalid SVG path: {reason}"),
            Self::InvalidGradient { reason } => write!(f, "invalid gradient: {reason}"),
            Self::FontRegister { reason } => write!(f, "font register failed: {reason}"),
            Self::FilterCreate { reason } => write!(f, "filter create failed: {reason}"),
            Self::Render { reason } => write!(f, "render failed: {reason}"),
            Self::PixelReadback { reason } => write!(f, "pixel readback failed: {reason}"),
            Self::PixelWrite { reason } => write!(f, "pixel write failed: {reason}"),
            Self::EngineUnavailable { engine, reason } => {
                write!(f, "render engine {engine:?} unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for NativeError {}
