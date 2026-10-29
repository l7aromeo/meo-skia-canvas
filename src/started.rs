//! Skia's surface, image and codec constructors, each after [`start_skia`].
//!
//! `SkGraphics::Init` picks the raster pipeline's CPU-specific stages, and
//! it is process-wide. A public entry starts it, but code inside the crate --
//! a unit test above all -- can reach Skia's pixels without going through
//! one, and whether it then ran on an x86 machine's AVX2 stages or its SSE2
//! baseline depended on whether something else had started Skia first. So
//! every raster surface, raster image and codec in the crate is made here,
//! and `tests/static/skia-start.test.js` refuses the raw constructors
//! anywhere else in `src`.
//!
//! Each takes what its skia-safe namesake takes and returns what it returns.

use crate::start_skia;
#[cfg(test)]
use skia_safe::ISize;
use skia_safe::{
    AlphaType, Codec, Data, Image, ImageInfo, Surface, SurfaceProps, images,
    surfaces,
};

/// [`surfaces::raster`], once Skia is started.
pub(crate) fn raster(
    info: &ImageInfo,
    row_bytes: impl Into<Option<usize>>,
    props: Option<&SurfaceProps>,
) -> Option<Surface> {
    start_skia();
    surfaces::raster(info, row_bytes, props)
}

/// [`surfaces::raster_n32_premul`], once Skia is started. Only tests make
/// one.
#[cfg(test)]
pub(crate) fn raster_n32_premul(size: impl Into<ISize>) -> Option<Surface> {
    start_skia();
    surfaces::raster_n32_premul(size)
}

/// [`images::raster_from_data`], once Skia is started.
pub(crate) fn raster_from_data(
    info: &ImageInfo,
    pixels: impl Into<Data>,
    row_bytes: usize,
) -> Option<Image> {
    start_skia();
    images::raster_from_data(info, pixels, row_bytes)
}

/// [`images::deferred_from_encoded_data`], once Skia is started.
pub(crate) fn deferred_from_encoded_data(
    data: impl Into<Data>,
    alpha_type: impl Into<Option<AlphaType>>,
) -> Option<Image> {
    start_skia();
    images::deferred_from_encoded_data(data, alpha_type)
}

/// [`Codec::from_data`], once Skia is started.
pub(crate) fn codec(data: impl Into<Data>) -> Option<Codec<'static>> {
    start_skia();
    Codec::from_data(data)
}
