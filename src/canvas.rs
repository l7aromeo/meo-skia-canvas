//! The canvas document: pages in, encoded bytes out.
//!
//! Mirrors the Canvas API's `Canvas` object. A canvas owns one or more pages,
//! each drawn through a [`Context2D`], and stays resolution-independent until
//! export -- [`EncodeOptions::density`] scales at encode time rather than at
//! construction, so the same drawing yields any resolution.
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

use skia_safe::{ColorSpace, ColorType, SurfaceProps};

use crate::{
    context::{
        Context2D as Inner,
        page::{ExportOptions, PageSequence},
    },
    context2d::Context2D,
    error::Error,
    export::{EncodeOptions, ImageFormat, Pages},
    gpu::RenderingEngine,
    pixels::{ImageData, ImageDataExport, PixelColorSpace, PixelDepth},
    start_skia,
};

/// What this build renders through, and what it found to render with.
///
/// The same facts the Node binding's `backend()` reports. Read it once at
/// startup to log what a machine actually has, or to decide whether a
/// workload is worth sending to the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// Which renderer a canvas gets by default.
    pub renderer: EngineKind,
    /// The graphics API in use -- `"Metal"`, `"Vulkan"` -- or `None` on a
    /// build with no GPU support compiled in.
    pub api: Option<String>,
    /// The adapter, as the driver names it, or a sentence saying why the
    /// CPU is being used instead.
    pub device: Option<String>,
    /// The driver version, where the API reports one.
    pub driver: Option<String>,
    /// Why the GPU is unavailable, when it is.
    ///
    /// `None` on a working GPU *and* on a build compiled without GPU
    /// support -- there is no fault to report in either case.
    pub error: Option<String>,
    /// How many threads the rasterizing pool has.
    pub threads: usize,
    /// Whether a canvas may choose the GPU at all.
    ///
    /// False on a build without GPU support and on a machine whose driver
    /// declined, which [`error`](Self::error) tells apart.
    pub gpu_available: bool,
}

impl BackendInfo {
    /// What this build and this machine offer.
    ///
    /// The strings come from the driver and are for logs, not for matching
    /// on: their wording is the platform's and changes with it.
    pub fn query() -> Self {
        let engine = RenderingEngine::default();
        let status = engine.status(false);
        // `status` is a JSON object because the Node binding hands it
        // straight across the boundary. Read back into typed fields here,
        // so a Rust caller is not asked to parse what this crate produced.
        let text = |key: &str| {
            status
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        Self {
            renderer: match engine {
                RenderingEngine::GPU => EngineKind::Gpu,
                RenderingEngine::CPU => EngineKind::Cpu,
            },
            api: text("api"),
            device: text("device"),
            driver: text("driver"),
            error: text("error"),
            threads: rayon::current_num_threads(),
            gpu_available: RenderingEngine::GPU.selectable(),
        }
    }
}

/// The alphabet RFC 4648 assigns to standard base64, in index order.
///
/// Not the URL-safe variant: a `data:` URL carries base64 in its body,
/// where `+` and `/` are legal.
/// The text contrast a canvas rasterizes glyph coverage with unless asked
/// for another: the value Chromium compiles into its Skia build for the
/// platform, `SK_GAMMA_CONTRAST` in `skia/BUILD.gn`'s `config("skia_config")`
/// on the chromium/chromium main branch, which that file says is chosen to
/// approximate the system browser. With [`DEFAULT_TEXT_GAMMA`] and hinting
/// on, Chrome 156 on macOS draws 13px Helvetica and 48px Helvetica and Oswald
/// pixel for pixel as a canvas does. The Linux value, Chromium's for Linux
/// and ChromeOS, has not been measured against a Linux Chrome; it stands for
/// every platform that is neither macOS nor Windows.
#[cfg(target_os = "macos")]
pub(crate) const DEFAULT_TEXT_CONTRAST: f32 = 0.0;
/// See the macOS value.
#[cfg(target_os = "windows")]
pub(crate) const DEFAULT_TEXT_CONTRAST: f32 = 1.0;
/// See the macOS value.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) const DEFAULT_TEXT_CONTRAST: f32 = 0.2;

/// The gamma glyph coverage is corrected against unless asked for another,
/// Chromium's `SK_GAMMA_EXPONENT` for the platform -- see
/// [`DEFAULT_TEXT_CONTRAST`]. Chromium sets `SK_GAMMA_SRGB` instead on macOS
/// and Windows, which this Skia no longer reads: an exponent of 0 is its sRGB
/// curve either way.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) const DEFAULT_TEXT_GAMMA: f32 = 0.0;
/// See the macOS value.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) const DEFAULT_TEXT_GAMMA: f32 = 1.2;

/// Whether `contrast` is a text contrast Skia's `SkSurfaceProps` takes:
/// `MIN_CONTRAST_INCLUSIVE` to `MAX_CONTRAST_INCLUSIVE`, 0 to 1. NaN is not.
pub(crate) fn text_contrast_in_range(contrast: f32) -> bool {
    (SurfaceProps::MIN_CONTRAST_INCLUSIVE
        ..=SurfaceProps::MAX_CONTRAST_INCLUSIVE)
        .contains(&contrast)
}

/// Whether `gamma` is a text gamma Skia's `SkSurfaceProps` takes:
/// `MIN_GAMMA_INCLUSIVE` up to, not including, `MAX_GAMMA_EXCLUSIVE` -- 0 up
/// to 4. NaN is not.
pub(crate) fn text_gamma_in_range(gamma: f32) -> bool {
    (SurfaceProps::MIN_GAMMA_INCLUSIVE..SurfaceProps::MAX_GAMMA_EXCLUSIVE)
        .contains(&gamma)
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The character RFC 4648 pads a final partial group with.
const BASE64_PAD: u8 = b'=';

/// `bytes` as standard base64.
///
/// Written here rather than taken as a dependency: it is three lines of
/// arithmetic over a named alphabet, and RFC 4648 publishes the test
/// vectors that prove it, which is a better guarantee than a crate this
/// small usually comes with.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        // Three bytes become four six-bit indices. A short final group is
        // zero-filled and the missing indices become padding.
        let packed =
            group.iter().enumerate().fold(0u32, |packed, (i, byte)| {
                packed | u32::from(*byte) << (16 - 8 * i)
            });
        for i in 0..4 {
            let character = match i <= group.len() {
                true => {
                    BASE64_ALPHABET
                        [(packed >> (18 - 6 * i) & 0b11_1111) as usize]
                }
                false => BASE64_PAD,
            };
            out.push(character as char);
        }
    }
    out
}

/// The size a canvas has when nobody names one.
///
/// 300 by 150, which is the HTML specification's default for a `<canvas>`
/// element and therefore what a caller porting browser code expects. Not a
/// choice this crate gets to make: a different default would silently
/// change the output of any drawing that relied on it.
pub const DEFAULT_WIDTH: f32 = 300.0;
/// The height half of [`DEFAULT_WIDTH`].
pub const DEFAULT_HEIGHT: f32 = 150.0;

/// The rasterizer a canvas ended up using.
///
/// Reported by [`Canvas::engine_kind`]: [`Canvas::set_gpu`] asks, and on a
/// machine with no reachable GPU backend the answer is [`EngineKind::Cpu`]
/// however the flag is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// Skia's raster backend.
    Cpu,
    /// A hardware backend -- Metal on macOS, Vulkan elsewhere.
    Gpu,
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        })
    }
}

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
    /// The pixel format this canvas composites, exports and reads back in.
    ///
    /// A float format composites in float: an `RGBAF32` canvas holds an alpha
    /// of 0.002, where eight bits round it to 1/255. It also fixes the depth
    /// this canvas carries when another canvas draws it as a source, which is
    /// as deep as the deferred-image API allows -- F16 for either float
    /// format. Defaults to [`PixelDepth::Uint8`].
    pub color_type: PixelDepth,
    /// Whether rendering may use the GPU. Defaults to `true`.
    pub gpu: bool,
    /// How much the rasterizer thickens small text, from `0.0` to `1.0`.
    ///
    /// Glyph stems below a pixel wide antialias to something lighter than
    /// the same shape at a larger size, and this compensates. Defaults to
    /// the value Chrome's text is drawn with on the platform: `0.0` on
    /// macOS, `1.0` on Windows and `0.2` elsewhere. The value the Node
    /// binding takes under the name `textContrast`. Outside `0.0` to `1.0`,
    /// Skia's range, [`Canvas::with_options`] refuses it.
    ///
    /// On macOS it changes nothing while
    /// [`Context2D::set_font_hinting`](crate::Context2D::set_font_hinting)
    /// is on, as it is by default: Skia sets hinted glyphs' contrast to `0`
    /// there, leaving their weight to CoreGraphics' own dilation of them. It
    /// reaches unhinted text only.
    pub text_contrast: f32,
    /// The gamma the rasterizer corrects glyph coverage against, `0.0`
    /// meaning the sRGB curve.
    ///
    /// Works with [`text_contrast`](Self::text_contrast): coverage is a
    /// linear quantity and the display is not, so blending glyph edges
    /// without accounting for that renders light text on dark thinner than
    /// dark on light. Defaults to the value Chrome's text is drawn with on
    /// the platform: `0.0` on macOS and Windows and `1.2` elsewhere. From
    /// `0.0` up to, not including, `4.0`, Skia's range; outside it
    /// [`Canvas::with_options`] refuses it.
    pub text_gamma: f32,
}

impl Default for CanvasOptions {
    fn default() -> Self {
        Self {
            color_space: PixelColorSpace::Srgb,
            color_type: PixelDepth::Uint8,
            gpu: true,
            text_contrast: DEFAULT_TEXT_CONTRAST,
            text_gamma: DEFAULT_TEXT_GAMMA,
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
        // SAFETY: sRGB is the one space every Skia build can construct, and
        // the default text contrast and gamma are within Skia's range.
        Self::with_options(width, height, CanvasOptions::default())
            .expect("the default options are always available")
    }

    /// Creates a canvas with an explicit color space and pixel format.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedPixelColorSpace`] when the requested space
    /// cannot be built by this Skia build, and [`Error::InvalidCanvasOption`]
    /// for a [`text_contrast`](CanvasOptions::text_contrast) or
    /// [`text_gamma`](CanvasOptions::text_gamma) outside the range Skia
    /// takes.
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
        start_skia();
        if !text_contrast_in_range(options.text_contrast) {
            return Err(Error::InvalidCanvasOption {
                option: "text_contrast",
                reason: format!(
                    "{} is not from {} to {}",
                    options.text_contrast,
                    SurfaceProps::MIN_CONTRAST_INCLUSIVE,
                    SurfaceProps::MAX_CONTRAST_INCLUSIVE
                ),
            });
        }
        if !text_gamma_in_range(options.text_gamma) {
            return Err(Error::InvalidCanvasOption {
                option: "text_gamma",
                reason: format!(
                    "{} is not from {} up to, not including, {}",
                    options.text_gamma,
                    SurfaceProps::MIN_GAMMA_INCLUSIVE,
                    SurfaceProps::MAX_GAMMA_EXCLUSIVE
                ),
            });
        }
        let space = options.color_space.to_skia_color_space()?;
        Ok(Self {
            width,
            height,
            contexts: vec![Self::make_context(
                width,
                height,
                options.gpu,
                space,
                &options,
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

    /// The pixel format this canvas draws into, which is not always the one
    /// [`color_type`](Self::color_type) names.
    ///
    /// [`color_type`](Self::color_type) is an *output* format: it decides
    /// what a readback or an export converts into. The surface underneath is
    /// [`PixelDepth::N32`] unless a float format was asked for, because
    /// compositing in anything narrower costs either transparency or colour
    /// -- see `ExportOptions::compositing_color_type`, which records what
    /// each of them loses.
    ///
    /// So a canvas asking for [`PixelDepth::Gray8`] holds four bytes a pixel
    /// and hands back one. Choosing a narrow format changes the pixels a
    /// canvas returns, not the memory it occupies, and this is how to ask
    /// which of the two a given canvas is doing.
    pub fn compositing_color_type(&self) -> PixelDepth {
        // Derived from the one place the rule lives rather than restated, so
        // the two cannot drift. Only three formats can come back, which is
        // what makes naming them here cheap.
        let asked = ExportOptions {
            surface_color_type: self.options.color_type.to_skia_color_type(),
            ..ExportOptions::default()
        };
        match asked.compositing_color_type() {
            ColorType::RGBAF16 | ColorType::RGBAF16Norm => PixelDepth::F16,
            ColorType::RGBAF32 => PixelDepth::F32,
            _ => PixelDepth::N32,
        }
    }

    fn make_context(
        width: f32,
        height: f32,
        gpu: bool,
        space: ColorSpace,
        options: &CanvasOptions,
    ) -> Context2D {
        let inner = Inner::new(
            space,
            options.color_type.to_skia_color_type(),
            (options.text_contrast, options.text_gamma),
            (width, height),
        );
        Context2D::from_inner(inner, gpu, options)
    }

    /// The options the canvas was made with, which a window shows its text
    /// with.
    #[cfg(feature = "window")]
    pub(crate) fn options(&self) -> &CanvasOptions {
        &self.options
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

    /// Takes page `index`'s size as the canvas's, without clearing anything,
    /// as a window does on showing that page.
    ///
    /// `width` and `height` report the newest page's size until a window
    /// shows another, which is what `window.page = n` does to the canvas in
    /// JavaScript. Does nothing for an index past the last page. Only a
    /// window shows a page, so it exists only where windows do.
    #[cfg(feature = "window")]
    pub(crate) fn adopt_page_size(&mut self, index: usize) {
        if let Some(context) = self.contexts.get(index) {
            let size = context.inner.bounds.size();
            self.width = size.width;
            self.height = size.height;
        }
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

    /// Reads a whole page's pixels back, `page` counting from `0` and `None`
    /// meaning the current page.
    ///
    /// The Rust counterpart of JavaScript's `toBuffer("raw", ..)`: the
    /// page's bytes at a `density`, over a `matte`, in the layout
    /// `export.pixels` names -- premultiplied when it says so, which is what
    /// the `alphaType` option asks for there -- returned with that layout
    /// attached rather than as bare bytes. The same readback
    /// [`Context2D::get_image_data_with`] performs, over the page's whole
    /// area.
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(4.0, 2.0);
    /// canvas
    ///     .context()
    ///     .set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    /// canvas.context().set_global_alpha(0.5);
    /// canvas.context().fill_rect(0.0, 0.0, 4.0, 2.0);
    /// let options = ImageDataExport {
    ///     pixels: PixelExportOptions {
    ///         premultiplied: true,
    ///         ..PixelExportOptions::default()
    ///     },
    ///     ..ImageDataExport::default()
    /// };
    /// let page = canvas.to_image_data(None, &options)?;
    /// assert_eq!((page.width(), page.height()), (4, 2));
    /// assert_eq!(&page.pixels()[..4], [128, 0, 0, 128]);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::PixelReadback`] for a `page` past the last one, and
    /// everything [`Context2D::get_image_data_with`] returns.
    pub fn to_image_data(
        &mut self,
        page: Option<usize>,
        export: &ImageDataExport,
    ) -> Result<ImageData, Error> {
        let pages = self.contexts.len();
        let context = match page {
            None => self.context(),
            Some(index) => {
                self.page(index).ok_or_else(|| Error::PixelReadback {
                    reason: format!(
                        "page {index} is past the last of {pages}, which is \
                         {}",
                        pages - 1
                    ),
                })?
            }
        };
        let (width, height) =
            (context.inner.bounds.width(), context.inner.bounds.height());
        context.get_image_data_with(0.0, 0.0, width, height, export)
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
            &self.options,
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
        if !self.gpu {
            return RenderingEngine::CPU;
        }
        let engine = RenderingEngine::default();
        // A float canvas that the GPU cannot composite goes to the raster
        // backend rather than quietly dropping to eight bits: `colorType`
        // means the same thing on both engines, and `engine_kind` reports
        // which one answered. Skia's Ganesh Metal and Vulkan backends carry
        // no 32-bit float format today, and round a draw's colour to eight
        // bits before compositing it in an `F16` one, so this is where an
        // `F16` or `F32` canvas changes hands -- and `can_composite` probes
        // rather than assumes, so a Skia that composites either in float
        // keeps the canvas on the GPU.
        match engine.can_composite(self.options.color_type.to_skia_color_type())
        {
            true => engine,
            false => RenderingEngine::CPU,
        }
    }

    /// The cheap half of an export, handed back so the expensive half can
    /// run elsewhere.
    ///
    /// Resolves which pages the call names, folds the canvas's own colour
    /// and text settings into the export options, and snapshots each page's
    /// recorded drawing. The returned [`Pages`] is [`Send`] where a
    /// `Canvas` is not, so [`Pages::encode`] can be called on a worker
    /// thread while this canvas goes on being drawn into.
    ///
    /// The handle is bound to `format` and `options`; take another for
    /// another format rather than reusing this one. [`Pages`] says why.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] for an [`EncodeOptions::page_range`] the
    /// canvas cannot satisfy or a frame-delay list that does not match the
    /// pages selected, and propagates the color-space error from
    /// [`EncodeOptions`] when the requested space is unavailable. Errors
    /// that belong to encoding itself are raised by [`Pages::encode`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut canvas = Canvas::new(800.0, 400.0);
    /// canvas.context().fill_rect(0.0, 0.0, 800.0, 400.0);
    /// let pages =
    ///     canvas.prepare_export(ImageFormat::Png, &EncodeOptions::default())?;
    /// let png = std::thread::spawn(move || pages.encode())
    ///     .join()
    ///     .expect("encoding thread")?;
    /// # let _ = png;
    /// # Ok(())
    /// # }
    /// ```
    pub fn prepare_export(
        &mut self,
        format: ImageFormat,
        options: &EncodeOptions,
    ) -> Result<Pages, Error> {
        self.prepare_pages(format, options, false)
    }

    /// As [`Canvas::prepare_export`], for a call that writes each page to a
    /// file of its own when `sequence` is set.
    fn prepare_pages(
        &mut self,
        format: ImageFormat,
        options: &EncodeOptions,
        sequence: bool,
    ) -> Result<Pages, Error> {
        // The page count `frame_delays` is checked against is the number of
        // frames this call will write, not the number the canvas holds.
        // Naming a page writes one -- so once `page` was honoured, the list
        // that matched the output was refused for being the wrong length
        // and the list that passed was then ignored at encode time, falling
        // back to `fps` and retiming the frame it did write.
        let selected =
            options.resolved_pages(format, self.contexts.len(), sequence)?;
        let frames = match options.page {
            Some(_) => 1,
            None => selected.len(),
        };
        let mut internal =
            options.to_internal(format, self.options.color_space, frames)?;
        // The canvas decides what its pages composite in and what a readback
        // defaults to; the call decides only what it converts into.
        internal.surface_color_space = self.surface_space();
        internal.surface_color_type =
            self.options.color_type.to_skia_color_type();
        // Glyph rasterization follows the canvas, as the compositing
        // format does: it is a property of the surface being drawn into,
        // not of the call that reads it back.
        internal.text_contrast = self.options.text_contrast;
        internal.text_gamma = self.options.text_gamma;
        // The call may name a readback format; the canvas decides what its
        // pages composite in either way.
        internal.color_type = options
            .color_type
            .unwrap_or(self.options.color_type)
            .to_skia_color_type();
        let engine = self.engine();
        // Sliced here, before the sequence exists, rather than skipped as it
        // encodes. An animated format codes each frame against the one
        // before it, so a range that left its predecessor in place would
        // open with a frame diffed against a page the file does not carry.
        // `page` and `page_range` cannot both be set, so the indices
        // `to_buffer` resolves `page` against are still the canvas's own.
        let pages = self.contexts[selected]
            .iter()
            .map(|context| context.inner.page())
            .collect();

        Ok(Pages::new(
            internal,
            PageSequence::from(pages, engine),
            options.page,
        ))
    }

    /// Encodes the canvas and returns the bytes.
    ///
    /// A format that spans pages emits all of them as one file -- PDF, the
    /// two animations, TIFF and ICO. The rest encode the **current** page --
    /// the one [`Canvas::context`] hands back, which is the page just added
    /// by [`Canvas::new_page`] rather than the one the canvas started with.
    /// That is what the Canvas API does; its `pages.slice(-1)` picks the
    /// same page.
    ///
    /// [`ImageFormat::Svg`] is vector where SVG can describe the drawing and
    /// pixels where it cannot: a sweep gradient, procedural noise, a blend
    /// mode, a filter or a shadow is rendered at `density` and embedded as an
    /// image, because Skia's SVG writer omits all of them and the file would
    /// otherwise show a flat black shape where a conic gradient was. The rest
    /// of the document, text included, stays vector.
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
        self.prepare_export(format, options)?.encode()
    }

    /// Encodes the canvas as a `data:` URL.
    ///
    /// The same bytes [`to_buffer`](Self::to_buffer) returns, base64-encoded
    /// behind the format's media type -- what an `<img src>` or a CSS
    /// `url()` takes directly.
    ///
    /// Base64 costs a third more bytes than the buffer it wraps, so this is
    /// for embedding rather than for writing files.
    /// [`ImageFormat::Raw`] has no media type worth embedding and is
    /// refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] for [`ImageFormat::Raw`], and everything
    /// [`to_buffer`](Self::to_buffer) can return.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(8.0, 8.0);
    /// canvas.context().fill_rect(0.0, 0.0, 8.0, 8.0);
    /// let url =
    ///     canvas.to_data_url(ImageFormat::Png, &EncodeOptions::default())?;
    /// assert!(url.starts_with("data:image/png;base64,"));
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn to_data_url(
        &mut self,
        format: ImageFormat,
        options: &EncodeOptions,
    ) -> Result<String, Error> {
        if format == ImageFormat::Raw {
            return Err(Error::Encode {
                reason: "raw pixel bytes have no media type to embed in a \
                         data URL"
                    .to_string(),
            });
        }
        let bytes = self.to_buffer(format, options)?;
        Ok(format!(
            "data:{};base64,{}",
            format.mime_type(),
            base64(&bytes)
        ))
    }

    /// Encodes the canvas and writes it to `path`.
    ///
    /// The format is taken from the file extension. An unrecognized or absent
    /// extension is an error rather than a silent default, so a typo does not
    /// quietly produce a PNG named `.wepb`; [`Canvas::to_file_as`] names the
    /// format where the path cannot.
    ///
    /// A path carrying `{}` writes one numbered file per page, as
    /// JavaScript's `toFile` does: `"frame-{}.png"` writes `frame-1.png`
    /// upward, with as many digits as the page count needs, and a number in
    /// the braces fixes the width, so `"frame-{4}.png"` writes
    /// `frame-0001.png`. Every pair of braces takes the number. The pages are
    /// [`EncodeOptions::page_range`] when one is set -- for any format, since
    /// each page is its own file -- the one [`EncodeOptions::page`] names, or
    /// all of them, and they are numbered from one within that selection. A
    /// format that gathers pages, such as PDF, writes one file each under a
    /// pattern rather than one file of all of them.
    ///
    /// Without braces it is one file: every page of a format that gathers
    /// them, and the current page of the rest.
    ///
    /// An `@` and a whole number before an `x` at the end of the name's stem
    /// is a density, as in JavaScript: `"chart@2x.png"` writes at
    /// [`EncodeOptions::density`] 2, and `"frame-{}@2x.png"` writes every
    /// page at it. Only when the options leave the density at its default of
    /// 1 -- a density they name wins. `@1.5x` is not a suffix, and neither is
    /// one with more of the name after it, as in `"a@2x-b.png"`. The stem is
    /// the name less its own extension whatever the format, so
    /// [`Canvas::to_file_as`] reads `"chart@2x.dat"` as 2x too.
    ///
    /// One difference from JavaScript: there, an explicit `{ density: 1 }`
    /// wins over the suffix, and here it cannot, because
    /// [`EncodeOptions::density`] has no unset state to tell the default from
    /// a 1 the caller wrote.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] when the extension names no known format,
    /// when encoding fails, or when a file cannot be written, and propagates
    /// everything [`Canvas::to_buffer`] can return -- including the
    /// color-space error from [`EncodeOptions`].
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
                    "cannot infer a format from {}; expected one of {}",
                    path.display(),
                    ImageFormat::inferable_names().join(", ")
                ),
            })?;
        self.to_file_as(path, format, options)
    }

    /// As [`Canvas::to_file`], in `format` whatever the path says -- which
    /// is JavaScript's `toFile(path, { format })`, and what a path with no
    /// extension, or the wrong one, needs.
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut canvas = Canvas::new(8.0, 8.0);
    /// canvas.new_page();
    /// // `frames/1` and `frames/2`, both JPEG.
    /// canvas.to_file_as(
    ///     "frames/{}",
    ///     ImageFormat::Jpeg,
    ///     &EncodeOptions::default(),
    /// )?;
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Canvas::to_file`], [`Error::Encode`] for an
    /// [`EncodeOptions::page`] past the last page, and
    /// [`Error::InvalidExportOption`] naming `density` for an `@0x` suffix.
    pub fn to_file_as(
        &mut self,
        path: impl AsRef<Path>,
        format: ImageFormat,
        options: &EncodeOptions,
    ) -> Result<(), Error> {
        let path = path.as_ref();
        let suffixed;
        let options = match density_suffix(path) {
            Some(digits)
                if options.density == EncodeOptions::default().density =>
            {
                let density = digits
                    .parse::<u32>()
                    .ok()
                    .filter(|&density| density > 0)
                    .ok_or_else(|| Error::InvalidExportOption {
                        option: "density",
                        reason: format!(
                            "the file name's @{digits}x asks for a density \
                             of {digits}; it must be a whole number from 1 \
                             to {}",
                            u32::MAX
                        ),
                    })?;
                suffixed = EncodeOptions {
                    density: density as f32,
                    ..options.clone()
                };
                &suffixed
            }
            _ => options,
        };
        let Some((pattern, digits)) = path.to_str().and_then(folio_pattern)
        else {
            return self.prepare_export(format, options)?.write(path);
        };

        // A named page is a sequence of one, numbered `1`, as in JavaScript.
        let options = match options.page {
            Some(index) if index >= self.contexts.len() => {
                return Err(Error::Encode {
                    reason: format!(
                        "page {index} is past the last of {} pages",
                        self.contexts.len()
                    ),
                });
            }
            Some(index) => EncodeOptions {
                page: None,
                page_range: Some(index..index + 1),
                ..options.clone()
            },
            None => options.clone(),
        };
        self.prepare_pages(format, &options, true)?
            .write_each(&pattern, digits)
    }
}

/// The digits of an `@2x` density suffix ending `path`'s stem -- the name
/// without its last extension, or the whole name when it has none -- or
/// `None` when the stem does not end in one. Case-insensitive in the `x`, as
/// JavaScript's `/@(\d+)x$/i` is.
fn density_suffix(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let body = stem.strip_suffix(['x', 'X'])?;
    let digits = &body[body.rfind('@')? + 1..];
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some(digits)
}

/// A file name with every `{}` or `{N}` in it rewritten as `{}`, and the
/// page-number width the braces asked for -- `None` for as many digits as
/// the page count needs -- or `None` when there are no braces and the name
/// is one file's.
///
/// The width is the last number given, and a later `{}` keeps it, as the
/// JavaScript `toFile` reads a name. A width too large to hold is kept as
/// the largest one, so it is refused by name when the files are written
/// rather than wrapped into a small one here.
fn folio_pattern(name: &str) -> Option<(String, Option<usize>)> {
    let mut pattern = String::with_capacity(name.len());
    let mut digits = None;
    let mut found = false;
    let mut rest = name;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let run = after.bytes().take_while(u8::is_ascii_digit).count();
        if after[run..].starts_with('}') {
            found = true;
            pattern.push_str(&rest[..open]);
            pattern.push_str("{}");
            if run > 0 {
                digits = Some(after[..run].parse().unwrap_or(usize::MAX));
            }
            rest = &after[run + 1..];
        } else {
            pattern.push_str(&rest[..=open]);
            rest = after;
        }
    }
    pattern.push_str(rest);
    found.then_some((pattern, digits))
}

#[cfg(test)]
mod data_url_tests {
    use super::*;
    use crate::prelude::*;

    #[test]
    fn base64_matches_the_vectors_the_rfc_publishes() {
        // RFC 4648 section 10. Every padding case is here -- no padding,
        // one `=`, two -- which is the part of base64 worth checking, and
        // the part a hand-rolled one gets wrong.
        for (input, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), encoded, "{input:?}");
        }
    }

    #[test]
    fn base64_covers_every_byte_value() {
        // The alphabet is 64 characters and a byte is 256 values, so a
        // wrong shift shows up as a character that never appears or one
        // that appears where it should not. Encoding 0..=255 exercises
        // every six-bit index.
        let all: Vec<u8> = (0..=255u8).collect();
        let encoded = base64(&all);
        assert_eq!(encoded.len(), 344, "256 bytes is 344 base64 characters");
        assert!(
            encoded
                .bytes()
                .all(|b| BASE64_ALPHABET.contains(&b) || b == BASE64_PAD)
        );
        // Every alphabet character is reachable from some input, so none of
        // the 64 indices is unreachable through an off-by-one.
        let used: std::collections::HashSet<u8> = encoded.bytes().collect();
        assert!(
            BASE64_ALPHABET.iter().all(|c| used.contains(c)),
            "some alphabet characters never appear"
        );
    }

    #[test]
    fn a_data_url_carries_the_format_it_names() {
        let mut canvas = Canvas::new(4.0, 4.0);
        canvas.context().fill_rect(0.0, 0.0, 4.0, 4.0);

        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp] {
            let url = canvas
                .to_data_url(format, &EncodeOptions::default())
                .expect("a data url");
            let prefix = format!("data:{};base64,", format.mime_type());
            assert!(url.starts_with(&prefix), "{format:?}: {}", &url[..40]);

            // And the body is the same bytes `to_buffer` gives, so the URL
            // is an encoding rather than a second rendering.
            let bytes = canvas
                .to_buffer(format, &EncodeOptions::default())
                .expect("a buffer");
            assert_eq!(&url[prefix.len()..], base64(&bytes), "{format:?}");
        }
    }

    #[test]
    fn raw_pixels_are_refused_rather_than_given_a_made_up_type() {
        let mut canvas = Canvas::new(4.0, 4.0);
        canvas.context().fill_rect(0.0, 0.0, 4.0, 4.0);
        let refused = canvas
            .to_data_url(ImageFormat::Raw, &EncodeOptions::default())
            .expect_err("raw has no media type worth embedding");
        assert!(format!("{refused}").contains("no media type"));
    }
}

#[cfg(test)]
mod backend_info_tests {
    use super::*;

    #[test]
    fn the_backend_reports_something_consistent_with_itself() {
        let info = BackendInfo::query();

        // The pool is never empty, whatever the machine.
        assert!(info.threads >= 1);

        // A GPU renderer implies the GPU was selectable, and a fault
        // implies it was not. The other two combinations are legitimate: a
        // build without GPU support reports no error and no availability,
        // and a canvas can fall back to the CPU on a working GPU when the
        // pixel format needs it.
        if info.renderer == EngineKind::Gpu {
            assert!(info.gpu_available, "a GPU renderer that is unavailable");
            assert_eq!(info.error, None, "a working GPU reporting a fault");
        }
        if info.error.is_some() {
            assert!(!info.gpu_available, "a fault that did not stop it");
        }

        // Whatever it says about the device, it says something.
        assert!(
            info.device.is_some(),
            "no device description at all: {info:?}"
        );
    }

    #[test]
    fn it_agrees_with_what_a_canvas_reports() {
        // Two ways to the same fact, and they have to agree. `engine_kind`
        // reads it off a canvas and was once the only way to ask;
        // `BackendInfo::query` answers at module level without one. A
        // default canvas takes the default engine, so the two must match.
        let canvas = Canvas::new(4.0, 4.0);
        assert_eq!(canvas.engine_kind(), BackendInfo::query().renderer);
    }
}

#[cfg(test)]
mod pages_tests {
    use crate::{prelude::*, started};
    use skia_safe::{
        AlphaType, ColorType, Data, ImageInfo, image::CachingHint,
    };

    /// An export decoded to its size and unpremultiplied RGBA pixels, or
    /// `None` when it does not decode.
    type Decoded = Option<(i32, i32, Vec<[u8; 4]>)>;

    fn decoded(encoded: &[u8]) -> Decoded {
        let image =
            started::deferred_from_encoded_data(Data::new_copy(encoded), None)?;
        let info = ImageInfo::new(
            image.dimensions(),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut bytes = vec![0; info.compute_min_byte_size()];
        image
            .read_pixels(
                &info,
                &mut bytes,
                info.min_row_bytes(),
                (0, 0),
                CachingHint::Disallow,
            )
            .then(|| {
                let pixels = bytes.as_chunks::<4>().0.to_vec();
                (image.width(), image.height(), pixels)
            })
    }

    fn summary(image: &Decoded) -> String {
        match image {
            None => "an image that does not decode".to_string(),
            Some((width, height, pixels)) => {
                match pixels.iter().all(|pixel| pixel[3] == 0) {
                    true => format!("{width}x{height}, fully transparent"),
                    false => format!("{width}x{height}, not blank"),
                }
            }
        }
    }

    /// Asserts two exports are byte for byte the same, and says how they
    /// differ when they are not.
    ///
    /// A bare `assert_eq!` on encoded bytes prints two arrays for a reader to
    /// decode by hand. How the images differ is what points at a cause: a
    /// fully transparent export means nothing reached the surface it was read
    /// from, where a partly different one means something was drawn wrong.
    /// The engine narrows it again -- a failure on the CPU raster backend
    /// rules out every explanation that needs a GPU.
    #[track_caller]
    fn assert_same_export(
        canvas: &Canvas,
        what: &str,
        got: &[u8],
        want: &[u8],
    ) {
        if got == want {
            return;
        }
        let (got, want) = (decoded(got), decoded(want));
        let differing = match (&got, &want) {
            (Some((gw, gh, gp)), Some((ww, wh, wp)))
                if (gw, gh) == (ww, wh) =>
            {
                let differ = gp.iter().zip(wp).filter(|(a, b)| a != b).count();
                format!("{differ} of {} pixels differ", gp.len())
            }
            _ => "the two cannot be compared pixel for pixel".to_string(),
        };
        panic!(
            "{what} differs from its reference on the {} engine: got {}, \
             want {}; {differing}",
            canvas.engine_kind(),
            summary(&got),
            summary(&want),
        );
    }

    #[test]
    fn a_handle_writes_the_same_bytes_it_encodes() {
        // `write` and `encode` take different paths for a spanning format --
        // one streams, the other buffers -- so a single-page format is where
        // they must agree byte for byte.
        let options = EncodeOptions::default();
        let mut canvas = drawn(48.0, 32.0);
        let dir = std::env::temp_dir()
            .join(format!("meo-pages-write-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.png");

        canvas
            .prepare_export(ImageFormat::Png, &options)
            .unwrap()
            .write(&path)
            .unwrap();

        let written = std::fs::read(&path).unwrap();
        let encoded = canvas
            .prepare_export(ImageFormat::Png, &options)
            .unwrap()
            .encode()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_same_export(&canvas, "the written PNG", &written, &encoded);
    }

    #[test]
    fn a_handle_writes_every_page_of_a_spanning_format() {
        // The path `encode` cannot offer: a format that gathers pages goes
        // straight to the file rather than through a buffer.
        let options = EncodeOptions::default();
        let mut canvas = drawn(32.0, 32.0);
        canvas.new_page();
        {
            let ctx = canvas.context();
            ctx.fill_rect(0.0, 0.0, 16.0, 16.0);
        }
        let dir = std::env::temp_dir()
            .join(format!("meo-pages-write-gif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spanning.gif");

        let pages = canvas.prepare_export(ImageFormat::Gif, &options).unwrap();
        assert_eq!(pages.len(), 2);
        pages.write(&path).unwrap();

        let written = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_dir_all(&dir).ok();
        assert!(written > 0, "a spanning write produced an empty file");
    }

    fn drawn(width: f32, height: f32) -> Canvas {
        let mut canvas = Canvas::new(width, height);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::opaque(0.1, 0.4, 0.8));
            ctx.fill_rect(0.0, 0.0, width * 0.75, height * 0.5);
            ctx.set_fill_style(RgbaLinear::opaque(0.9, 0.2, 0.1));
            ctx.fill_rect(width * 0.25, height * 0.5, width * 0.5, height);
        }
        canvas
    }

    #[test]
    fn one_canvas_yields_a_handle_per_format() {
        // The handle is bound to its format, so two of them off the same
        // canvas have to encode independently. The page cache is keyed by
        // the export options, and a key that ignored the format would show
        // up here as the second format returning the first one's bytes.
        let options = EncodeOptions::default();
        let mut canvas = drawn(64.0, 48.0);
        let png = canvas.prepare_export(ImageFormat::Png, &options).unwrap();
        let jpeg = canvas.prepare_export(ImageFormat::Jpeg, &options).unwrap();

        let (png, jpeg) = (png.encode().unwrap(), jpeg.encode().unwrap());

        let mut reference = drawn(64.0, 48.0);
        let want_png = reference.to_buffer(ImageFormat::Png, &options).unwrap();
        let want_jpeg =
            reference.to_buffer(ImageFormat::Jpeg, &options).unwrap();
        assert_same_export(&canvas, "the PNG handle", &png, &want_png);
        assert_same_export(&canvas, "the JPEG handle", &jpeg, &want_jpeg);
        assert_ne!(png, jpeg);
    }

    #[test]
    fn a_handle_encodes_on_another_thread() {
        // The reason the type exists. `Canvas` is `!Send`, so this is the
        // whole of the check: that it compiles at all is half of it, and
        // that the bytes match a same-thread export is the other half.
        let options = EncodeOptions::default();
        let mut canvas = drawn(64.0, 48.0);
        let pages = canvas.prepare_export(ImageFormat::Png, &options).unwrap();

        let elsewhere = std::thread::spawn(move || pages.encode())
            .join()
            .unwrap()
            .unwrap();

        let here = canvas.to_buffer(ImageFormat::Png, &options).unwrap();
        assert_same_export(
            &canvas,
            "the handle encoded elsewhere",
            &elsewhere,
            &here,
        );
    }

    #[test]
    #[should_panic(
        expected = "got 64x48, fully transparent, want 64x48, not blank"
    )]
    fn a_blank_export_is_reported_as_blank() {
        // The message is the instrument, so it has to be shown naming the
        // case it exists for: an export with nothing on it, set against one
        // that was drawn.
        let options = EncodeOptions::default();
        let mut blank = Canvas::new(64.0, 48.0);
        let got = blank.to_buffer(ImageFormat::Png, &options).unwrap();
        let want = drawn(64.0, 48.0)
            .to_buffer(ImageFormat::Png, &options)
            .unwrap();
        assert_same_export(&blank, "an undrawn canvas", &got, &want);
    }

    #[test]
    fn a_handle_reports_the_pages_the_call_selected() {
        let mut canvas = drawn(32.0, 32.0);
        canvas.new_page();
        canvas.new_page();

        let all = canvas
            .prepare_export(ImageFormat::Png, &EncodeOptions::default())
            .unwrap();
        assert_eq!(all.len(), 3);
        assert!(!all.is_empty());

        // `page_range` slices; `page` does not, because that index is
        // resolved against the sequence when the bytes are produced. Asked
        // of an animation because `page_range` is refused outright by a
        // format that encodes one page.
        let sliced = canvas
            .prepare_export(
                ImageFormat::Gif,
                &EncodeOptions {
                    page_range: Some(1..3),
                    ..EncodeOptions::default()
                },
            )
            .unwrap();
        assert_eq!(sliced.len(), 2);

        let named = canvas
            .prepare_export(
                ImageFormat::Png,
                &EncodeOptions {
                    page: Some(2),
                    ..EncodeOptions::default()
                },
            )
            .unwrap();
        assert_eq!(named.len(), 3);
    }

    #[test]
    fn a_narrow_canvas_reports_the_format_it_actually_composites_in() {
        // The whole point of the accessor: `color_type` answers what the
        // canvas hands back, and this answers what it draws into. They
        // differ for every format below four bytes a pixel.
        for narrow in [
            PixelDepth::Gray8,
            PixelDepth::Alpha8,
            PixelDepth::Rgb565,
            PixelDepth::Argb4444,
            PixelDepth::R8UNorm,
            PixelDepth::A16UNorm,
        ] {
            let canvas = Canvas::with_options(
                8.0,
                8.0,
                CanvasOptions {
                    color_type: narrow,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(canvas.color_type(), narrow);
            assert_eq!(
                canvas.compositing_color_type(),
                PixelDepth::N32,
                "{narrow:?} composites in N32"
            );
        }

        // A float canvas is the case where following `color_type` is worth
        // what it costs, so the two agree.
        for (asked, composited) in [
            (PixelDepth::F16, PixelDepth::F16),
            (PixelDepth::F32, PixelDepth::F32),
            // The 8-bit formats are already N32 or convert to it freely.
            (PixelDepth::Uint8, PixelDepth::N32),
            (PixelDepth::N32, PixelDepth::N32),
        ] {
            let canvas = Canvas::with_options(
                8.0,
                8.0,
                CanvasOptions {
                    color_type: asked,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                canvas.compositing_color_type(),
                composited,
                "{asked:?}"
            );
        }
    }

    #[test]
    fn each_page_goes_to_its_own_numbered_file() {
        let dir = std::env::temp_dir()
            .join(format!("msc-write-each-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut canvas = drawn(16.0, 16.0);
        canvas.new_page();
        canvas.new_page();

        let pattern = dir.join("frame-{}.png");
        canvas
            .prepare_export(ImageFormat::Png, &EncodeOptions::default())
            .unwrap()
            .write_each(pattern.to_str().unwrap(), None)
            .unwrap();

        // Three pages need one digit, so no padding is added.
        for name in ["frame-1.png", "frame-2.png", "frame-3.png"] {
            assert!(dir.join(name).is_file(), "{name} was not written");
        }

        // A fixed width pads to it.
        canvas
            .prepare_export(ImageFormat::Png, &EncodeOptions::default())
            .unwrap()
            .write_each(pattern.to_str().unwrap(), Some(4))
            .unwrap();
        assert!(dir.join("frame-0001.png").is_file());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_width_no_filename_could_hold_is_refused() {
        let mut canvas = drawn(8.0, 8.0);
        let refused = canvas
            .prepare_export(ImageFormat::Png, &EncodeOptions::default())
            .unwrap()
            // 255 is the bound; a digit past it cannot name a file, and the
            // point of refusing is that building the string is what the
            // process cannot survive.
            .write_each("/nonexistent/{}.png", Some(256));

        let message = refused.unwrap_err().to_string();
        assert!(
            message.contains("256") && message.contains("255"),
            "the error should name both the width and the bound: {message}"
        );
    }

    #[test]
    fn writing_each_page_refuses_a_handle_that_names_one() {
        let mut canvas = drawn(8.0, 8.0);
        canvas.new_page();

        let refused = canvas
            .prepare_export(
                ImageFormat::Png,
                &EncodeOptions {
                    page: Some(0),
                    ..EncodeOptions::default()
                },
            )
            .unwrap()
            .write_each("/nonexistent/{}.png", None);

        assert!(
            refused.unwrap_err().to_string().contains("page 0"),
            "the contradiction should name the page that was asked for"
        );
    }
}

#[cfg(test)]
mod readback_text_tests {
    use super::*;
    use crate::FontLibrary;

    const OSWALD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
    );

    /// A canvas's text reads back as it exports, at a text gamma other
    /// than the platform's default: the readback rasterizes glyphs as the
    /// canvas does, not at the defaults.
    #[test]
    fn a_readback_draws_text_at_the_canvas_s_gamma() {
        FontLibrary::new()
            .register_font_from_path("ReadbackGammaOswald", OSWALD)
            .expect("a bundled face registers");
        let drawn = |text_gamma: f32| {
            let options = CanvasOptions {
                text_gamma,
                gpu: false,
                ..CanvasOptions::default()
            };
            let mut canvas = Canvas::with_options(420.0, 120.0, options)
                .expect("a gamma in range");
            let ctx = canvas.context();
            // Opaque, so that a raw export and a readback agree whether or
            // not either is premultiplied.
            ctx.set_fill_style_css("white").expect("a colour");
            ctx.fill_rect(0.0, 0.0, 420.0, 120.0);
            ctx.set_fill_style_css("#203040").expect("a colour");
            assert!(ctx.set_font_css("28px ReadbackGammaOswald"));
            ctx.fill_text("Hamburgefonstiv fi AV", 10.0, 70.0, None);
            let read = ctx
                .get_image_data(0.0, 0.0, 420.0, 120.0)
                .expect("the canvas reads back")
                .pixels()
                .to_vec();
            let exported = canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("a raw export");
            (read, exported)
        };
        let (read, exported) = drawn(2.2);
        assert!(read == exported, "read back as exported");
        let (default_read, _) = drawn(CanvasOptions::default().text_gamma);
        assert!(read != default_read, "and unlike the default gamma");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod text_contrast_tests {
    use super::*;
    use crate::FontLibrary;

    const OSWALD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
    );

    /// A line of text on a CPU canvas made with `text_contrast`, hinted or
    /// not.
    fn drawn(text_contrast: f32, hinting: bool) -> Vec<u8> {
        let options = CanvasOptions {
            text_contrast,
            gpu: false,
            ..CanvasOptions::default()
        };
        let mut canvas = Canvas::with_options(420.0, 120.0, options)
            .expect("a contrast in range");
        let ctx = canvas.context();
        ctx.set_font_hinting(hinting);
        assert!(ctx.set_font_css("28px TextContrastOswald"));
        ctx.fill_text("Hamburgefonstiv fi AV", 10.0, 70.0, None);
        ctx.get_image_data(0.0, 0.0, 420.0, 120.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec()
    }

    /// Skia's CoreText scaler leaves a hinted glyph's contrast to
    /// CoreGraphics, so the option reaches unhinted text alone -- see
    /// `ExportOptions::surface_props`. Fails, as the sign to look again,
    /// once a hinted run takes it too.
    #[test]
    fn hinted_macos_text_ignores_contrast() {
        FontLibrary::new()
            .register_font_from_path("TextContrastOswald", OSWALD)
            .expect("a bundled face registers");
        assert!(drawn(0.0, true) == drawn(1.0, true), "hinted: the same");
        assert!(drawn(0.0, false) != drawn(1.0, false), "unhinted: not");
    }
}
