//! The canvas document: pages in, encoded bytes out.
//!
//! Mirrors the Canvas API's `Canvas` object. A canvas owns one or more pages,
//! each drawn through a [`Context2D`], and stays
//! resolution-independent until
//! export -- [`EncodeOptions::density`](crate::export::EncodeOptions::density)
//! scales at encode time rather than at construction, so the same drawing
//! yields any resolution.
//!
//! ```no_run
//! use meo_skia_canvas::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut canvas = Canvas::new(800.0, 400.0);
//! {
//!     let ctx = canvas.context();
//!     ctx.set_fill_style(RgbaLinear::opaque(0.1, 0.1, 0.12));
//!     ctx.fill_rect(0.0, 0.0, 800.0, 400.0);
//! }
//! let png = canvas.to_buffer(ImageFormat::Png, &EncodeOptions::default())?;
//! # Ok(())
//! # }
//! ```

use std::path::Path;

use skia_safe::ColorSpace;

use crate::{
    backend::EngineKind,
    context::{Context2D as Inner, page::PageSequence},
    context2d::Context2D,
    error::Error,
    export::{EncodeOptions, ImageFormat},
    gpu::RenderingEngine,
    pixels::{PixelColorSpace, PixelDepth},
};

/// What a canvas is built with, beyond its size.
///
/// The Rust counterpart of the JavaScript
/// `new Canvas(width, height, { colorSpace, colorType, gpu })`, and the same
/// fields.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasOptions {
    /// The space every page composites in.
    ///
    /// Fixed here, not at export: a colour outside this space's gamut is
    /// clipped as it is drawn, and an export converts out of it. Defaults to
    /// [`PixelColorSpace::Srgb`].
    pub color_space: PixelColorSpace,
    /// The pixel format exports and readbacks default to.
    ///
    /// Compositing is eight bits per channel whatever this says -- it selects
    /// the format pixels are *handed back* in. Defaults to
    /// [`PixelDepth::Uint8`].
    pub color_type: PixelDepth,
    /// Whether rendering may use the GPU. Defaults to `true`.
    pub gpu: bool,
}

impl Default for CanvasOptions {
    fn default() -> Self {
        Self {
            color_space: PixelColorSpace::Srgb,
            color_type: PixelDepth::Uint8,
            gpu: true,
        }
    }
}

/// A canvas document, holding one page per [`Context2D`].
pub struct Canvas {
    width: f32,
    height: f32,
    /// Never empty: [`Canvas::new`] seeds one page and nothing removes pages.
    contexts: Vec<Context2D>,
    gpu: bool,
    options: CanvasOptions,
}

impl Canvas {
    /// Creates a canvas `width` by `height` with a single blank page.
    ///
    /// Composites in sRGB. Use [`Canvas::with_options`] for a wider space.
    pub fn new(width: f32, height: f32) -> Self {
        // SAFETY: sRGB is the one space every Skia build can construct.
        Self::with_options(width, height, CanvasOptions::default())
            .expect("sRGB is always available")
    }

    /// Creates a canvas with an explicit color space and pixel format.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedPixelColorSpace`] when the requested space
    /// cannot be built by this Skia build.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::with_options(
    ///     400.0,
    ///     300.0,
    ///     CanvasOptions {
    ///         color_space: PixelColorSpace::DisplayP3,
    ///         ..CanvasOptions::default()
    ///     },
    /// )?;
    /// assert_eq!(canvas.color_space(), PixelColorSpace::DisplayP3);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_options(
        width: f32,
        height: f32,
        options: CanvasOptions,
    ) -> Result<Self, Error> {
        let space = options.color_space.to_skia_color_space()?;
        Ok(Self {
            width,
            height,
            contexts: vec![Self::make_context(
                width,
                height,
                options.gpu,
                space,
                options.color_type,
                options.color_space,
            )],
            gpu: options.gpu,
            options,
        })
    }

    /// The Skia space the pages are built in.
    ///
    /// Falls back to sRGB rather than failing: the constructor already
    /// rejected a space this build cannot make, so this cannot be reached
    /// with one.
    fn surface_space(&self) -> ColorSpace {
        self.options
            .color_space
            .to_skia_color_space()
            .unwrap_or_else(|_| ColorSpace::new_srgb())
    }

    /// The space this canvas composites in.
    pub fn color_space(&self) -> PixelColorSpace {
        self.options.color_space
    }

    /// The pixel format exports and readbacks default to.
    pub fn color_type(&self) -> PixelDepth {
        self.options.color_type
    }

    fn make_context(
        width: f32,
        height: f32,
        gpu: bool,
        space: ColorSpace,
        readback_depth: PixelDepth,
        readback_space: PixelColorSpace,
    ) -> Context2D {
        let mut inner = Inner::new(space);
        inner.reset_size((width, height));
        Context2D::from_inner(inner, gpu, readback_depth, readback_space)
    }

    /// The canvas width in points.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// The canvas height in points.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// How many pages the canvas holds.
    pub fn page_count(&self) -> usize {
        self.contexts.len()
    }

    /// Resizes the canvas and clears the current page.
    ///
    /// Clearing is the HTML Canvas behaviour: assigning `canvas.width`
    /// discards the drawing rather than rescaling or cropping it. Pages
    /// added earlier keep their own size.
    pub fn set_size(&mut self, width: f32, height: f32) {
        self.width = width;
        self.height = height;
        self.context().inner.reset_size((width, height));
    }

    /// Borrows an earlier page's context, `0` being the first added.
    ///
    /// [`Canvas::context`] only ever reaches the newest page, so without
    /// this an earlier page becomes unreachable the moment
    /// [`Canvas::new_page`] is called.
    ///
    /// Returns `None` when `index` is past the last page.
    pub fn page(&mut self, index: usize) -> Option<&mut Context2D> {
        self.contexts.get_mut(index)
    }

    /// Whether rendering may use the GPU. Defaults to `true`, falling back to
    /// the CPU rasterizer when no GPU backend is available.
    ///
    /// Applies to every page, including the readback
    /// [`Context2D::get_image_data`](crate::context2d::Context2D::get_image_data)
    /// performs, and to pages added afterwards.
    pub fn set_gpu(&mut self, enabled: bool) {
        self.gpu = enabled;
        for context in &mut self.contexts {
            context.gpu = enabled;
        }
    }

    /// Borrows the current page's drawing context.
    ///
    /// The borrow lasts as long as the returned reference, so unlike the
    /// JavaScript API the context is used in a scope rather than held for the
    /// canvas's lifetime.
    pub fn context(&mut self) -> &mut Context2D {
        // SAFETY: `new` seeds one context and no method removes one, so the
        // vector is never empty.
        self.contexts
            .last_mut()
            .expect("a canvas always has at least one page")
    }

    /// Appends a blank page at the canvas's own size and borrows its context.
    pub fn new_page(&mut self) -> &mut Context2D {
        self.new_page_with(self.width, self.height)
    }

    /// Appends a blank page at an explicit size and borrows its context.
    ///
    /// **Resizes the canvas.** The new size applies to this page and to
    /// every page added after it, so a later [`Canvas::new_page`] inherits
    /// it. Pages already added keep the size they were created at, which is
    /// how a multi-page PDF ends up with pages of differing dimensions.
    ///
    /// This mirrors the JavaScript `newPage(width, height)`, which assigns
    /// the pair to the canvas.
    pub fn new_page_with(&mut self, width: f32, height: f32) -> &mut Context2D {
        self.width = width;
        self.height = height;
        self.contexts.push(Self::make_context(
            width,
            height,
            self.gpu,
            self.surface_space(),
            self.options.color_type,
            self.options.color_space,
        ));
        self.context()
    }

    /// Whether the GPU has been asked for.
    ///
    /// The request, not the outcome: on a machine with no reachable GPU
    /// backend this still reports what was asked while
    /// [`Canvas::engine_kind`] reports the CPU it fell back to.
    pub fn gpu(&self) -> bool {
        self.gpu
    }

    /// The rasterizer this canvas will actually use.
    ///
    /// [`Canvas::set_gpu`] asks for the GPU; this reports what asking got,
    /// which is [`EngineKind::Cpu`] on a machine with no reachable GPU
    /// backend however the flag is set. The JavaScript `canvas.gpu` getter
    /// answers the same question.
    pub fn engine_kind(&self) -> EngineKind {
        match self.engine() {
            RenderingEngine::GPU => EngineKind::Gpu,
            RenderingEngine::CPU => EngineKind::Cpu,
        }
    }

    fn engine(&self) -> RenderingEngine {
        if self.gpu {
            RenderingEngine::default()
        } else {
            RenderingEngine::CPU
        }
    }

    /// Encodes the canvas and returns the bytes.
    ///
    /// [`ImageFormat::Pdf`] emits every page as one document. The raster
    /// formats encode the **current** page -- the one
    /// [`Canvas::context`] hands back, which is the page just added by
    /// [`Canvas::new_page`] rather than the one the canvas started with.
    /// That is what the Canvas API does; its `pages.slice(-1)` picks the
    /// same page.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] when the encoder rejects the drawing, such
    /// as a page with a zero dimension or an
    /// [`EncodeOptions::page`] past the last one, and propagates the
    /// color-space error from [`EncodeOptions`] when the requested space is
    /// unavailable.
    pub fn to_buffer(
        &mut self,
        format: ImageFormat,
        options: &EncodeOptions,
    ) -> Result<Vec<u8>, Error> {
        let mut internal = options.to_internal(format)?;
        // The canvas decides what its pages composite in and what a readback
        // defaults to; the call decides only what it converts into.
        internal.surface_color_space = self.surface_space();
        internal.color_type = self.options.color_type.to_skia_color_type();
        let engine = self.engine();
        let pages = self
            .contexts
            .iter()
            .map(|context| context.inner.get_page())
            .collect();

        let mut sequence = PageSequence::from(pages, engine);
        sequence.materialize(&engine, &internal);

        let bytes = if format == ImageFormat::Pdf && sequence.len() > 1 {
            sequence.as_pdf(internal)
        } else {
            // `last`, not `first`: pages are appended, so the newest is the
            // one `context()` returns and the one the caller has been
            // drawing into. Matched against the binding rather than
            // assumed -- `PageSequence::first` exists and reads naturally,
            // which is exactly how this shipped encoding a blank page.
            let selected = match options.page {
                Some(index) => sequence.pages.get(index),
                None => sequence.pages.last(),
            };
            match selected {
                Some(page) => page.encoded_as(internal, engine),
                None => Err(format!(
                    "page {} is out of range; the canvas has {}",
                    options.page.unwrap_or(0),
                    sequence.len()
                )),
            }
        };

        bytes.map_err(|reason| Error::Encode { reason })
    }

    /// Encodes the canvas and writes it to `path`.
    ///
    /// The format is taken from the file extension. An unrecognized or absent
    /// extension is an error rather than a silent default, so a typo does not
    /// quietly produce a PNG named `.wepb`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] when the extension names no known format,
    /// when encoding fails, or when the file cannot be written, and
    /// propagates everything [`Canvas::to_buffer`] can return -- including
    /// the color-space error from [`EncodeOptions`].
    pub fn to_file(
        &mut self,
        path: impl AsRef<Path>,
        options: &EncodeOptions,
    ) -> Result<(), Error> {
        let path = path.as_ref();
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(ImageFormat::from_extension)
            .ok_or_else(|| Error::Encode {
                reason: format!(
                    "cannot infer a format from {}; expected one of \
                     png, jpg, jpeg, webp, pdf, svg",
                    path.display()
                ),
            })?;

        let bytes = self.to_buffer(format, options)?;
        std::fs::write(path, bytes).map_err(|e| Error::Encode {
            reason: format!("could not write {}: {e}", path.display()),
        })
    }
}
