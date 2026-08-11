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
    context::{Context2D as Inner, page::PageSequence},
    context2d::Context2D,
    error::Error,
    export::{EncodeOptions, ImageFormat},
    gpu::RenderingEngine,
};

/// A canvas document, holding one page per [`Context2D`].
pub struct Canvas {
    width: f32,
    height: f32,
    /// Never empty: [`Canvas::new`] seeds one page and nothing removes pages.
    contexts: Vec<Context2D>,
    gpu: bool,
}

impl Canvas {
    /// Creates a canvas `width` by `height` with a single blank page.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            contexts: vec![Self::make_context(width, height, true)],
            gpu: true,
        }
    }

    fn make_context(width: f32, height: f32, gpu: bool) -> Context2D {
        let mut inner = Inner::new(ColorSpace::new_srgb());
        inner.reset_size((width, height));
        Context2D::from_inner(inner, gpu)
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
        self.contexts
            .push(Self::make_context(width, height, self.gpu));
        self.context()
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
        let internal = options.to_internal(format)?;
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
    /// when encoding fails, or when the file cannot be written.
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
