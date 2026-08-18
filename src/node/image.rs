#![allow(unused_imports)]
#![allow(dead_code)]
use crate::{
    context::{BoxedContext2D, Context2D},
    font_library::FontLibrary,
    image::{decode_frame, frame_delays},
    utils::*,
};
use neon::{prelude::*, types::buffer::TypedArray};
use skia_safe::{
    AlphaType, ColorSpace, ColorType, Data, FontMgr, ISize, Image as SkImage,
    ImageInfo, Picture, PictureRecorder, Rect, Size,
    image::images,
    svg::{self, Length, LengthUnit},
};
use std::cell::RefCell;

pub type BoxedImage = JsBox<RefCell<Image>>;
impl Finalize for Image {}

pub struct Image {
    src: String,
    pub autosized: bool,
    pub content: Content,
    /// How long each frame is shown, in milliseconds, one entry per frame.
    ///
    /// A still image has a single zero, so `delays.len()` is the frame count
    /// and the two can never disagree.
    delays: Vec<u32>,
    /// The encoded bytes, kept only while there is more than one frame in
    /// them, so `frame()` can decode the rest on demand.
    encoded: Option<Data>,
    /// A decoder held part-way through an animation, as on the Rust
    /// [`Image`](crate::image::Image).
    ///
    /// Frames of a coded sequence are stored as differences, so reaching
    /// frame `n` means decoding everything before it. Without this, the
    /// documented way to play an animation -- `img.frame(n)` once per output
    /// frame -- restarts from zero every time and costs the square of the
    /// frame count in sample decodes.
    playback: Option<crate::decode::Playback>,
}

impl Default for Image {
    fn default() -> Self {
        Image {
            content: Content::Loading,
            autosized: false,
            src: "".to_string(),
            delays: vec![0],
            encoded: None,
            playback: None,
        }
    }
}

#[derive(Default)]
pub enum Content {
    Bitmap(SkImage),
    Vector(Picture, Size),
    #[default]
    Loading,
    Broken,
}

/// Names what it holds and how big it is, without saying whose pixels.
impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Bitmap(_) => write!(f, "Bitmap({:?})", self.size()),
            Content::Vector(_, size) => write!(f, "Vector({size:?})"),
            Content::Loading => f.write_str("Loading"),
            Content::Broken => f.write_str("Broken"),
        }
    }
}

/// An image a drawing verb was given, resolved to what it will paint.
///
/// `autosized` says the source is an SVG with no intrinsic size, which
/// changes how a destination rect is scaled. It lives on the `Image` rather
/// than in the picture, so it travels beside the content rather than in it.
#[derive(Clone, Debug, Default)]
pub struct Source {
    /// What will be painted.
    pub content: Content,
    /// Whether the source had no size of its own to be laid out by.
    pub autosized: bool,
}

impl Clone for Content {
    fn clone(&self) -> Self {
        match self {
            Content::Bitmap(img) => Content::Bitmap(img.clone()),
            Content::Vector(pict, size) => Content::Vector(pict.clone(), *size),
            _ => Content::default(),
        }
    }
}

impl Source {
    /// What a drawing verb was handed, if it was handed something to paint.
    ///
    /// An `Image`, or the context of a `Canvas` -- and a canvas resolves to
    /// its pixels, which is what `drawImage` has always taken from one.
    /// Anything else is `None`: an `ImageData` is read straight off the call
    /// instead, and everything else is not an image at all. The caller
    /// decides whether that is an error or a call that paints nothing.
    pub fn of<'a>(
        cx: &mut FunctionContext<'a>,
        value: Handle<'a, JsValue>,
    ) -> Option<Self> {
        if let Ok(image) = value.downcast::<BoxedImage, _>(cx) {
            let image = image.borrow();
            return Some(Self {
                content: image.content.clone(),
                autosized: image.autosized,
            });
        }
        if let Ok(context) = value.downcast::<BoxedContext2D, _>(cx) {
            return Some(Self {
                content: Content::from_context(
                    &mut context.borrow_mut(),
                    false,
                ),
                autosized: false,
            });
        }
        None
    }
}

impl Content {
    pub fn from_context(ctx: &mut Context2D, use_vector: bool) -> Self {
        match use_vector {
            true => ctx
                .get_picture()
                .map(|p| Content::Vector(p, ctx.bounds.size())),
            false => ctx.get_image().map(Content::Bitmap),
        }
        .unwrap_or_default()
    }

    pub fn from_image_data(image_data: ImageData) -> Self {
        let info = image_data.image_info();
        images::raster_from_data(
            &info,
            &image_data.buffer,
            info.min_row_bytes(),
        )
        .map(Content::Bitmap)
        .unwrap_or_default()
    }

    pub fn size(&self) -> Size {
        match &self {
            Content::Bitmap(img) => img.dimensions().into(),
            Content::Vector(_, size) => *size,
            _ => Size::new_empty(),
        }
    }

    pub fn is_complete(&self) -> bool {
        !matches!(self, Content::Loading)
    }

    pub fn is_drawable(&self) -> bool {
        !matches!(self, Content::Loading | Content::Broken)
    }

    pub fn snap_rects_to_bounds(
        &self,
        mut src: Rect,
        mut dst: Rect,
    ) -> (Rect, Rect) {
        // Handle 'overdraw' of the src image where the crop coordinates are
        // outside of its bounds Snap the src rect to its actual bounds
        // and shift/pad the dst rect to account for the whitespace
        // included in the crop.
        let scale_x = dst.width() / src.width();
        let scale_y = dst.height() / src.height();
        let size = self.size();

        if src.left < 0.0 {
            dst.left += -src.left * scale_x;
            src.left = 0.0;
        }

        if src.top < 0.0 {
            dst.top += -src.top * scale_y;
            src.top = 0.0;
        }

        if src.right > size.width {
            dst.right -= (src.right - size.width) * scale_x;
            src.right = size.width;
        }

        if src.bottom > size.height {
            dst.bottom -= (src.bottom - size.height) * scale_y;
            src.bottom = size.height;
        }

        (src, dst)
    }
}

#[derive(Debug)]
pub struct ImageData {
    pub width: f32,
    pub height: f32,
    pub buffer: Data,
    color_type: ColorType,
    color_space: ColorSpace,
}

impl ImageData {
    pub fn new(
        buffer: Data,
        width: f32,
        height: f32,
        color_type: String,
        color_space: String,
    ) -> Self {
        // Both named rather than parsed here: this constructor has no `cx` to
        // throw from, and an unrecognised name must not quietly become the
        // default. Every path in reaches it from an `ImageData` that was
        // already checked -- `pixelSize` on the JavaScript side, or
        // `color_type_or_throw` and `color_space_or_throw` on this one -- so
        // the fallbacks are unreachable rather than lenient.
        let color_type =
            opt_color_type(&color_type).unwrap_or(ColorType::RGBA8888);
        let color_space =
            opt_color_space(&color_space).unwrap_or_else(ColorSpace::new_srgb);
        Self {
            buffer,
            width,
            height,
            color_type,
            color_space,
        }
    }

    pub fn image_info(&self) -> ImageInfo {
        ImageInfo::new(
            (self.width as _, self.height as _),
            self.color_type,
            AlphaType::Unpremul,
            self.color_space.clone(),
        )
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedImage> {
    let this = RefCell::new(Image::default());
    Ok(cx.boxed(this))
}

pub fn get_src(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow();

    Ok(cx.string(&this.src))
}

pub fn set_src(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedImage>(0)?;
    let mut this = this.borrow_mut();

    let src = cx.argument::<JsString>(1)?.value(&mut cx);
    this.src = src;
    Ok(cx.undefined())
}

pub fn set_data<'a>(
    mut cx: FunctionContext<'a>,
) -> NeonResult<Handle<'a, JsBoolean>> {
    let this = cx.argument::<BoxedImage>(0)?;
    let mut this = this.borrow_mut();
    let buffer = cx.argument::<JsBuffer>(1)?;
    let data = Data::new_copy(buffer.as_slice(&cx));

    if let Some(raw_info) = opt_image_info_arg(&mut cx, 2)? {
        // First, check for an optional dims argument and interpret the buffer
        // as raw rgba if present
        this.content = match images::raster_from_data(
            &raw_info,
            data,
            raw_info.min_row_bytes(),
        ) {
            Some(image) => Content::Bitmap(image),
            None => Content::Broken,
        }
    } else if let Some(image) = images::deferred_from_encoded_data(&data, None)
        .or_else(|| {
            // Skia decodes every format here but AVIF, of which it decodes
            // none -- so without this an `.avif` reaches the SVG branch
            // below and comes back as a broken image.
            crate::decode::avif::is_avif(data.as_bytes())
                .then(|| decode_frame(&data, 0, None).ok())
                .flatten()
        })
    {
        // Next, try interpreting the data as an encoded bitmap
        this.content = Content::Bitmap(image);
        // A second pass over the same bytes, opening its own codec: the
        // cost is one extra header parse at construction, and the return is
        // that `frames` and `delays` are property reads on the JavaScript
        // side rather than calls that could fail.
        this.delays = frame_delays(&data);
        this.encoded = (this.delays.len() > 1).then_some(data);
    } else if let Ok(mut dom) = svg::Dom::from_bytes(
        &data,
        FontLibrary::with_shared(|lib| lib.font_mgr()),
    ) {
        // Finally, try parsing as SVG
        let root = dom.root();

        let mut size = root.intrinsic_size();
        if size.is_empty() {
            // flag that image lacks an intrinsic size so it will be drawn to
            // match the canvas size if dimensions aren't provided
            // in the drawImage() call
            this.autosized = true;

            // If width or height attributes aren't defined on the root `<svg>`
            // element, they will be reported as "100%". If only one
            // is defined, use it for both dimensions, and if both are missing
            // use the aspect ratio to scale the width vs a fixed
            // height of 150 (i.e., Chrome's behavior)
            let Length {
                value: width,
                unit: w_unit,
            } = root.width();
            let Length {
                value: height,
                unit: h_unit,
            } = root.height();
            size = match ((width, w_unit), (height, h_unit)) {
                // NB: only unitless numeric lengths are currently being
                // handled; values in em, cm, in, etc. are ignored,
                // but perhaps they should be converted to px?
                (
                    (100.0, LengthUnit::Percentage),
                    (height, LengthUnit::Number),
                ) => (*height, *height).into(),
                (
                    (width, LengthUnit::Number),
                    (100.0, LengthUnit::Percentage),
                ) => (*width, *width).into(),
                _ => {
                    let aspect = root
                        .view_box()
                        .map(|vb| vb.width() / vb.height())
                        .unwrap_or(1.0);
                    (150.0 * aspect, 150.0).into()
                }
            };
        };

        // Save the SVG contents as a Picture (to be drawn later)
        let bounds = Rect::from_size(size);
        let mut compositor = PictureRecorder::new();
        dom.set_container_size(bounds.size());
        dom.render(compositor.begin_recording(bounds, true));
        this.content = match compositor.finish_recording_as_picture(None) {
            Some(picture) => Content::Vector(picture, size),
            None => Content::Broken,
        };
    } else {
        this.content = Content::Broken
    }

    Ok(cx.boolean(this.content.is_drawable()))
}

pub fn get_width(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.content.size().width).upcast())
}

pub fn get_height(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.content.size().height).upcast())
}

pub fn get_complete(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow();
    Ok(cx.boolean(this.content.is_complete()))
}

pub fn get_frames(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.delays.len() as f64))
}

pub fn get_delays(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedImage>(0)?;
    let delays = this.borrow().delays.clone();
    let array = JsArray::new(&mut cx, delays.len());
    for (index, delay) in delays.iter().enumerate() {
        let value = cx.number(*delay as f64);
        array.set(&mut cx, index as u32, value)?;
    }
    Ok(array)
}

/// Replaces this image's contents with frame `index` of `source`.
///
/// Two images rather than a return value because the JavaScript `Image` has
/// private fields its constructor installs, so a wrapper built around a
/// boxed struct from here would throw on `decode()` or `onload`. The caller
/// constructs an ordinary `Image` and this fills it in.
pub fn take_frame(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedImage>(0)?;
    let source = cx.argument::<BoxedImage>(1)?;
    let asked = float_arg(&mut cx, 2, "index")?;

    let (content, delays, encoded) = {
        // Mutable because the decode lends `source.playback`: playing an
        // animation forward keeps the decoder rather than rebuilding it for
        // every frame.
        let mut source = source.borrow_mut();
        let count = source.delays.len();
        // Counted from the end when negative, as `page` is in the export
        // options and as `Array.prototype.at` is. Resolved here rather than
        // only in JavaScript because `as usize` saturates: a negative index
        // used to arrive as frame 0 and be handed back without a word.
        //
        // Truncated toward zero *before* the end is counted from, which is
        // the order `at` uses: `at(-1.5)` is the last element, not the one
        // before it. Resolving first would have made `frame(-1.5)` the
        // second to last, which is the same argument reading two ways
        // depending on whether the count is even.
        let whole = asked.trunc() as f64;
        let resolved = match whole < 0.0 {
            true => count as f64 + whole,
            false => whole,
        };
        if resolved < 0.0 || resolved >= count as f64 {
            return cx.throw_range_error(format!(
                "frame {asked} is out of range; the image has {count}"
            ));
        }
        let index = resolved as usize;
        match source.encoded.as_ref() {
            // A still image is its own frame 0, and the range check above
            // has already refused anything else.
            None => (
                source.content.clone(),
                source.delays.clone(),
                source.encoded.clone(),
            ),
            Some(data) => {
                let data = data.clone();
                match decode_frame(&data, index, Some(&mut source.playback)) {
                    Ok(image) => (Content::Bitmap(image), vec![0], None),
                    Err(error) => {
                        return cx.throw_error(error.to_string());
                    }
                }
            }
        }
    };

    let mut this = this.borrow_mut();
    this.content = content;
    this.delays = delays;
    this.encoded = encoded;
    Ok(cx.undefined())
}

pub fn pixels(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedImage>(0)?;
    let this = this.borrow_mut();
    let (color_type, color_space) = image_data_settings_arg(&mut cx, 1)?;

    let info = ImageInfo::new(
        this.content.size().to_floor(),
        color_type,
        AlphaType::Unpremul,
        color_space,
    );
    let mut pixels = cx.buffer(
        info.bytes_per_pixel() * (info.width() * info.height()) as usize,
    )?;

    match &this.content {
        Content::Bitmap(image) => {
            match image.read_pixels(
                &info,
                pixels.as_mut_slice(&mut cx),
                info.min_row_bytes(),
                (0, 0),
                skia_safe::image::CachingHint::Allow,
            ) {
                true => Ok(pixels.upcast()),
                false => Ok(cx.undefined().upcast()),
            }
        }
        _ => Ok(cx.undefined().upcast()),
    }
}
