//! A blurred draw in one flat colour, made from its coverage alone: on an
//! eight-bit page on the CPU, a shape under a sole `blur()` is drawn into an
//! A8 plane where [`run`](super::run) would draw its layer, that plane is
//! blurred exactly -- see [`exact_blur::blur_alpha`] -- and each pixel's
//! colour is the placed colour times the blurred coverage times the gain,
//! rounded once, then composited as the chain's result is.
//!
//! The chain draws the shape into an RGBA layer in its colour, so each
//! channel of an edge pixel is rounded to a byte before the blur and again
//! after it. Here a channel is rounded once, from the blurred coverage: the
//! exact product the blur's definition asks for. The alpha is the chain's
//! bit for bit -- the layer's alpha is the same coverage, blurred by the
//! same sums and rounded by the same rule -- and so is an interior, where
//! the coverage is whole and the product lands on the bytes a fill stores.
//! It costs a quarter of the chain's sums and none of its conversions.

use skia_safe::{
    AlphaType, Canvas as SkCanvas, ClipOp, ColorType, IRect, ISize, ImageInfo,
    Matrix, Rect, Surface, image::RequiredProperties,
    surface::ContentChangeMode,
};

use super::{
    Depth, Outcome, Step, clear_unless_zeroed, ink, layer_info, layer_surface,
    placed, pool, region, split, trims_composite,
};
use crate::{
    context::{blur_mask::baked_image, exact_blur},
    gpu::{RenderingEngine, engine_of},
    started,
};

/// A draw's coverage, blurred, before it is coloured: one float a pixel of
/// `region`, from 0 to 255, under the part `rest` of the canvas's
/// transform the plane was not drawn in.
pub(crate) struct Blurred {
    values: Vec<f32>,
    region: IRect,
    rest: Matrix,
}

/// The byte an eight-bit RGBA or BGRA pixel holds its alpha in: the last of
/// its four in either order (`kRGBA_8888_SkColorType` and
/// `kBGRA_8888_SkColorType`, `include/core/SkColorType.h`).
const ALPHA_BYTE_OF_8888: usize = 3;

#[cfg(test)]
thread_local! {
    /// How many planes [`alpha_into`] has read where they lie on this
    /// thread, for a test to tell that read from `read_pixels`.
    static READ_IN_PLACE: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// Whether [`Placement::of`] on this thread takes the whole region as
    /// inked, for a test to hold the inked rectangle to what reading all of
    /// it finds.
    static INKED_IS_REGION: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// How many placements on this thread had an inked rectangle smaller
    /// than their region, for a test to tell that it held one to anything.
    static INKED_WITHIN: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Writes the alpha of `surface`'s pixels in `read` into `rows`, one row of
/// `read` each. A raster surface's pixels are read where they lie where each
/// holds its alpha as a byte -- an A8 plane, or a premultiplied or
/// unpremultiplied RGBA or BGRA one -- which takes the byte `read_pixels` to
/// A8 would copy, without its conversion or a copy of the plane. Any other
/// surface goes through `read_pixels`. `false` where the pixels cannot be
/// read.
fn alpha_into<'a>(
    surface: &mut Surface,
    read: IRect,
    rows: impl Iterator<Item = &'a mut [u8]>,
) -> bool {
    let line = read.width() as usize;
    if let Some(pixmap) = surface.peek_pixels()
        && let Some(bytes) = pixmap.bytes()
    {
        let info = pixmap.info();
        let stride = pixmap.row_bytes();
        let start = |step: usize| {
            read.top as usize * stride + read.left as usize * step
        };
        #[cfg(test)]
        let counted = || READ_IN_PLACE.with(|n| n.set(n.get() + 1));
        match (info.color_type(), info.alpha_type()) {
            (ColorType::Alpha8, _) => {
                alpha_rows::<1, 0>(rows, &bytes[start(1)..], stride);
                #[cfg(test)]
                counted();
                return true;
            }
            (
                ColorType::RGBA8888 | ColorType::BGRA8888,
                AlphaType::Premul | AlphaType::Unpremul,
            ) => {
                alpha_rows::<4, ALPHA_BYTE_OF_8888>(
                    rows,
                    &bytes[start(4)..],
                    stride,
                );
                #[cfg(test)]
                counted();
                return true;
            }
            _ => {}
        }
    }
    let info =
        ImageInfo::new(read.size(), ColorType::Alpha8, AlphaType::Premul, None);
    let mut bytes = vec![0u8; line * read.height() as usize];
    if !surface.read_pixels(&info, &mut bytes, line, (read.left, read.top)) {
        return false;
    }
    alpha_rows::<1, 0>(rows, &bytes, line);
    true
}

/// Writes byte `AT` of each `STEP`-byte pixel of `bytes`, whose rows start
/// `stride` bytes apart, into `rows`. Each a constant, so that a row is read
/// as fixed-size pixels and the compiler can take several at once.
fn alpha_rows<'a, const STEP: usize, const AT: usize>(
    rows: impl Iterator<Item = &'a mut [u8]>,
    bytes: &[u8],
    stride: usize,
) {
    rows.zip(bytes.chunks(stride)).for_each(|(row, pixels)| {
        let (pixels, _) = pixels.as_chunks::<STEP>();
        row.iter_mut()
            .zip(pixels)
            .for_each(|(v, pixel)| *v = pixel[AT]);
    });
}

/// What a draw's coverage is read from. `draw` paints through the draw's own
/// paint either way, never a stand-in. Skia pre-blends an A8 glyph mask's
/// coverage through one of eight tables, and picks the table from the
/// paint's luminance colour (Skia M156, `SkPaintPriv::ComputeLuminanceColor`
/// at `SkPaintPriv.cpp:135-159`): its colour, through its colour filter where
/// it has one, or grey for a shader that names no colour, as a run painted
/// through [`ExactColour::shader`](crate::context::layer::ExactColour::shader)
/// is -- see [`CoverageRun`](crate::context::layer::CoverageRun). That
/// colour has each channel cut to its top three bits
/// (`SkScalerContext.cpp:1104-1107`, `SkMaskGamma.h:129-134`), then the
/// luminance `(54r + 183g + 19b) >> 8` of that (`SkScalerContext.cpp:77-87`),
/// whose top three bits name the table (`SkMaskGamma.h:231-238`). Two paints
/// that pick one table cover a run alike and two that pick different tables
/// can differ. A run whose text size on the device passes 256 pixels
/// is drawn as paths, with no pre-blend (`SkStrikeSpec::ShouldDrawAsPath`),
/// so every colour covers it alike.
///
/// Where FreeType pre-blends its A8 masks (`SK_GAMMA_APPLY_TO_A8`,
/// `BUILD.gn:92`; `SkFontHost_FreeType.cpp:858-863`) blue picks black's table
/// and red does not, whatever the hinting. CoreText does the same for an
/// unhinted A8 mask, and first scales the colour of a mask it hints
/// (`SkTypeface_mac_ct.cpp:960-982`): by a half where CoreGraphics smooths
/// without subpixel coverage, which puts red on black's table too, and by
/// three quarters where it smooths with it, which does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plane {
    /// An A8 surface, which holds the coverage alone. A path's coverage is
    /// the chain's layer's alpha here, bit for bit, in every colour tried.
    Drawn,
    /// The alpha of a premultiplied surface of the page's own type and
    /// space, the layer the chain draws into on an eight-bit page: for text,
    /// whose glyph masks on the A8 surface land a level off that alpha on a
    /// few pixels of small or turned runs.
    Alpha,
}

/// The coverage of what `draw` draws onto `canvas`, read from `plane` and
/// blurred by `sigma` canvas pixels -- see [`Coverage::blurred`]. `None`
/// where nothing would show or no plane could be made.
pub(crate) fn blurred(
    canvas: &SkCanvas,
    sigma: f32,
    content: Option<Rect>,
    plane: Plane,
    draw: impl FnOnce(&SkCanvas),
) -> Option<Blurred> {
    Some(sharp(canvas, sigma, content, plane, draw)?.blurred())
}

/// The coverage of what `draw` draws onto `canvas`, read from `plane` for a
/// blur of `sigma` canvas pixels, before it is blurred -- see
/// [`Coverage::of`]. `None` where nothing would show or no plane could be
/// made.
pub(crate) fn sharp(
    canvas: &SkCanvas,
    sigma: f32,
    content: Option<Rect>,
    plane: Plane,
    draw: impl FnOnce(&SkCanvas),
) -> Option<Coverage> {
    Coverage::of(canvas, sigma, content, plane, draw)
}

/// What a plane of kind `plane` for a draw on `canvas` holds over `drawn`:
/// coverage alone, or a layer of the page's own type and space.
fn plane_info(canvas: &SkCanvas, plane: Plane, drawn: IRect) -> ImageInfo {
    match plane {
        Plane::Drawn => ImageInfo::new(
            drawn.size(),
            ColorType::Alpha8,
            AlphaType::Premul,
            None,
        ),
        Plane::Alpha => layer_info(canvas, Depth::Bytes, drawn),
    }
}

/// A plane of kind `plane` for a draw on `canvas` over `drawn`, kept for the
/// rest of the playback -- see [`pool`] -- and whether a draw used it before:
/// one kept from an earlier draw where one fits, and otherwise one made as
/// the plane made for this draw alone would be, but as large as the page, so
/// that the playback's later draws fit in it. Made by the canvas's own
/// device, as that plane is, so in main memory and made with the canvas's
/// props. `None` off the CPU, where planes are not kept, where the canvas
/// reports no props, or where the canvas makes no plane.
fn kept_plane(
    canvas: &SkCanvas,
    plane: Plane,
    drawn: IRect,
) -> Option<(Surface, bool)> {
    match engine_of(canvas) {
        RenderingEngine::CPU if pool::keeps_planes() => {}
        RenderingEngine::CPU
        | RenderingEngine::GPU
        | RenderingEngine::Graphite => return None,
    }
    let info = plane_info(canvas, plane, drawn);
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test skia_safe_reads_base_props_as_props_does on Windows
    // `Canvas::base_props` declares the C++ method `SkCanvas::getBaseProps`,
    // which returns a struct by value, as a C function. MSVC hands such a
    // method its return slot after `this`, where a C function takes it before
    // `this` on x64 and takes none on arm64, so on Windows Skia writes the
    // props over the canvas, or through whatever the register holds.
    // `clippy.toml` refuses the call. The canvas's base props, which
    // `new_surface` below gives a plane, are read through `SkCanvas::getProps`
    // instead, which writes through a pointer: in Skia M156 it and
    // `getBaseProps` both copy them through `onGetProps(props, false)`
    // (`SkCanvas.cpp:1232`).
    let props = canvas.props()?;
    if let Some(kept) = pool::take_plane(plane, &info, &props, drawn.size()) {
        return Some((kept, true));
    }
    let page = canvas.base_layer_size();
    let size = ISize::new(
        drawn.width().max(page.width),
        drawn.height().max(page.height),
    );
    let mut surface = canvas.new_surface(&info.with_dimensions(size), None)?;
    surface.peek_pixels()?;
    Some((surface, false))
}

/// Sets every byte of `surface`'s pixels within `within` to zero:
/// transparent black, as a plane made afresh holds, filled row by row where
/// the rows lie. `None` where the pixels are not in main memory.
fn zero_within(surface: &mut Surface, within: IRect) -> Option<()> {
    surface.notify_content_will_change(ContentChangeMode::Retain);
    let mut pixmap = surface.peek_pixels()?;
    let step = pixmap.info().bytes_per_pixel();
    let stride = pixmap.row_bytes();
    let (left, right) =
        (within.left as usize * step, within.right as usize * step);
    pixmap
        .bytes_mut()?
        .chunks_mut(stride)
        .skip(within.top as usize)
        .take(within.height() as usize)
        .for_each(|row| row[left..right].fill(0));
    Some(())
}

impl Blurred {
    /// This blur, composited `by` whole device pixels from where it was
    /// made. A draw moved by whole pixels does not always cover the same --
    /// see [`Coverage::of`] -- so the caller holds that its draw's coverage
    /// is the same at the new place.
    pub(crate) fn at(&self, by: (i32, i32)) -> Blurred {
        Blurred {
            values: self.values.clone(),
            region: self.region.with_offset(by),
            rest: self.rest,
        }
    }
}

/// Where [`Coverage::of`] places a draw's coverage for a blur on `canvas`,
/// decided before anything is drawn: by the canvas's transform and clip, the
/// blur's sigma and what the draw paints, and by nothing about the plane it
/// is drawn on.
pub(crate) struct Placement {
    /// What the blur works on, in layer pixels: [`Self::region`] widened by
    /// the blur's reach, and the rectangle [`Coverage::region`] reports.
    pub(crate) worked: IRect,
    /// The rectangle of the plane a draw's coverage is taken over, in layer
    /// pixels: what it paints within the blur's reach of the clip, and the
    /// clip within reach of what it paints.
    pub(crate) region: IRect,
    /// The part of [`Self::region`] the draw can cover, in layer pixels:
    /// what it paints, a pixel either side -- see [`ink`] -- and the region
    /// itself where nothing bounds what it paints. Coverage outside it is
    /// zero, so only it is cleared and read.
    pub(crate) inked: IRect,
    /// The plane the draw is made on, in layer pixels: on the CPU it starts
    /// at the device's origin, as the chain's layer does.
    pub(crate) drawn: IRect,
    /// The part of the canvas's transform the draw is made under, on the
    /// plane.
    pub(crate) layer: Matrix,
    /// The part of the transform left to place the blurred result on the
    /// device.
    pub(crate) rest: Matrix,
}

impl Placement {
    /// The placement of a draw painting within `content`, a rectangle in the
    /// canvas's local coordinates, blurred by `sigma` on `canvas`. `None` where
    /// the transform does not split into a layer's part and the rest, or the
    /// draw lies outside the clip.
    pub(crate) fn of(
        canvas: &SkCanvas,
        sigma: f32,
        content: Option<Rect>,
    ) -> Option<Placement> {
        let base = canvas.local_to_device_as_3x3();
        let (layer, rest) = split(&base)?;
        let steps = [Step::Blur(sigma)];
        let (to_draw, worked) = region(canvas, &steps, content, &layer, &rest)?;
        let (region, drawn) =
            placed(canvas, to_draw, rest.is_identity(), false);
        let inked = content
            .and_then(|content| {
                IRect::intersect(&ink(&layer, content), &region)
            })
            .unwrap_or(region);
        #[cfg(test)]
        let inked = match INKED_IS_REGION.with(std::cell::Cell::get) {
            true => region,
            false => inked,
        };
        #[cfg(test)]
        if inked != region {
            INKED_WITHIN.with(|n| n.set(n.get() + 1));
        }
        Some(Placement {
            worked: IRect::join(&worked, &region),
            region,
            inked,
            drawn,
            layer,
            rest,
        })
    }
}

/// A draw's coverage before it is blurred: one byte a pixel of `inked`, zero
/// everywhere else in `region`, what the blur works on, and the device
/// sigmas the blur takes along x and y.
///
/// Bytes rather than the floats the blur takes, because a kept result is
/// found by comparing a draw's coverage with the coverage it was made from
/// -- see `run_mask::drawn` and `kept::CoverageInput` -- and a draw that finds
/// one is never blurred: comparing and keeping bytes costs a quarter of the
/// memory and none of the conversion, and [`Self::blurred`] widens them to
/// the same floats. Only the inked rectangle's, because the region around it
/// is the blur's reach on every side: on #506's 15 by 12 ellipse under
/// blur(3px), 270 bytes of 2,754.
pub(crate) struct Coverage {
    ink: Vec<u8>,
    inked: IRect,
    region: IRect,
    rest: Matrix,
    sigma: (f32, f32),
}

impl Coverage {
    /// The coverage of what `draw` draws onto `canvas`, on the plane
    /// [`run`](super::run) would make its layer for a blur of `sigma`: placed
    /// alike and drawn under the same transform, so that Skia's
    /// anti-aliasing, which depends on where a path lies in the plane it
    /// fills, gives the coverage the layer's alpha would hold.
    ///
    /// The plane starts where the chain's layer does: on the CPU at the
    /// device's origin, so a draw moved by whole pixels lands elsewhere in
    /// it, and Skia's coverage of a curve can change with where it lands.
    fn of(
        canvas: &SkCanvas,
        sigma: f32,
        content: Option<Rect>,
        plane: Plane,
        draw: impl FnOnce(&SkCanvas),
    ) -> Option<Coverage> {
        let Placement {
            worked,
            region,
            inked,
            drawn,
            layer,
            rest,
        } = Placement::of(canvas, sigma, content)?;
        let surface = |drawn: IRect| {
            let surface = match plane {
                Plane::Drawn => {
                    let info = plane_info(canvas, plane, drawn);
                    canvas
                        .new_surface(&info, None)
                        .or_else(|| started::raster(&info, None, None))
                }
                // On the CPU, where no layer is kept: see `pool::key`.
                Plane::Alpha => layer_surface(canvas, Depth::Bytes, drawn)
                    .map(|(surface, _)| surface),
            };
            surface.map(|surface| (surface, drawn))
        };
        // A plane kept from an earlier draw of the playback where there is
        // one, and otherwise one made for this draw: the region alone where
        // a plane from the device's origin cannot be made, as `run` falls
        // back for its layer.
        let (mut surface, drawn, kept) = match kept_plane(canvas, plane, drawn)
        {
            Some((surface, used)) => (surface, drawn, Some(used)),
            None => {
                let (surface, drawn) =
                    surface(drawn).or_else(|| surface(region))?;
                (surface, drawn, None)
            }
        };
        // The plane is zero wherever a draw reads it: one made for it is
        // made zero, and a kept one is cleared where this draw is read.
        let read = inked.with_offset((-drawn.left, -drawn.top));
        match kept {
            // What an earlier draw left where this one is read.
            Some(true) => zero_within(&mut surface, read)?,
            Some(false) | None => clear_unless_zeroed(&mut surface),
        }
        let offscreen = surface.canvas();
        offscreen.save();
        // A kept plane is larger than the one made for this draw would be,
        // and Skia covers a path cut by a plane's edge otherwise than one it
        // draws whole -- see `region` -- so the draw is cut where that plane
        // would end.
        offscreen.clip_irect(IRect::from_size(drawn.size()), ClipOp::Intersect);
        let origin = Matrix::translate((-drawn.left as f32, -drawn.top as f32));
        offscreen.set_matrix(&Matrix::concat(&origin, &layer).into());
        draw(offscreen);
        offscreen.restore_to_count(1);
        offscreen.reset_matrix();
        let line = read.width() as usize;
        let mut ink = vec![0u8; line * read.height() as usize];
        if !alpha_into(&mut surface, read, ink.chunks_mut(line.max(1))) {
            return None;
        }
        if kept.is_some() {
            pool::keep_plane(plane, surface);
        }
        let scale = (layer.scale_x().abs(), layer.scale_y().abs());
        Some(Coverage {
            ink,
            inked,
            region: worked,
            rest,
            sigma: (sigma * scale.0, sigma * scale.1),
        })
    }

    /// Each pixel's coverage, row by row across [`Self::region`]: the inked
    /// rectangle's and zero around it, out to what the blur works on.
    /// Everything the blur reads but the sigmas, made on each call.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; self.region_len()];
        self.widen_into(&mut bytes, |byte| byte);
        bytes
    }

    /// The coverage of the inked rectangle, row by row, and the rectangle in
    /// device pixels. Every pixel of [`Self::region`] outside it is zero.
    pub(crate) fn ink(&self) -> (&[u8], IRect) {
        (&self.ink, self.inked)
    }

    /// The device pixels the blur works on: the region the coverage is
    /// taken over -- see [`Placement`] -- widened by the blur's reach.
    pub(crate) fn region(&self) -> IRect {
        self.region
    }

    /// How many pixels [`Self::region`] holds.
    fn region_len(&self) -> usize {
        self.region.width() as usize * self.region.height() as usize
    }

    /// Writes the inked rectangle's bytes into `out`, one value a pixel of
    /// [`Self::region`] row by row, each through `value`; the rest of `out`
    /// is left as it is.
    fn widen_into<T>(&self, out: &mut [T], value: impl Fn(u8) -> T) {
        let w = self.region.width() as usize;
        let line = self.inked.width() as usize;
        if line == 0 {
            return;
        }
        let (dx, dy) = (
            (self.inked.left - self.region.left) as usize,
            (self.inked.top - self.region.top) as usize,
        );
        out[dy * w..]
            .chunks_mut(w)
            .zip(self.ink.chunks(line))
            .for_each(|(row, ink)| {
                row[dx..][..line]
                    .iter_mut()
                    .zip(ink)
                    .for_each(|(out, &byte)| *out = value(byte));
            });
    }

    /// The device sigmas the blur takes along x and y.
    pub(crate) fn sigma(&self) -> (f32, f32) {
        self.sigma
    }

    /// The part of the canvas's transform the plane was not drawn in.
    pub(crate) fn rest(&self) -> Matrix {
        self.rest
    }

    /// This coverage blurred exactly, unrounded.
    pub(crate) fn blurred(self) -> Blurred {
        let (w, h) =
            (self.region.width() as usize, self.region.height() as usize);
        let mut values = vec![0.0f32; self.region_len()];
        self.widen_into(&mut values, f32::from);
        let values =
            exact_blur::blur_alpha(values, w, h, self.sigma.0, self.sigma.1);
        Blurred {
            values,
            region: self.region,
            rest: self.rest,
        }
    }
}

/// `blurred` coloured for `canvas` as the chain's result is, ready to
/// composite -- see [`Outcome::composite`] -- in `rgb`, the colour placed as
/// a fill of it is on an eight-bit page, at `gain`, the alpha on its byte:
/// each pixel's alpha `B * gain` rounded as [`exact_blur::blur_bytes_into`]
/// rounds, and each channel the exact product `rgb * B * gain` rounded once.
/// The image takes `canvas`'s colour type and space, so it composites only
/// onto a canvas of the same. `None` where no image could be made.
pub(crate) fn coloured(
    canvas: &SkCanvas,
    blurred: &Blurred,
    rgb: [f32; 3],
    gain: f32,
) -> Option<Outcome> {
    let info = layer_info(canvas, Depth::Bytes, blurred.region);
    let bgra = info.color_type() == ColorType::BGRA8888;
    let w = blurred.region.width() as usize;
    // The pixels that are not transparent black, as each is written: the
    // blur's tails round to zero well inside the region it works on.
    let mut inked = IRect::new_empty();
    let image = baked_image(&info, |out, stride| {
        for (y, (row, values)) in out
            .chunks_mut(stride)
            .zip(blurred.values.chunks(w))
            .enumerate()
        {
            let (mut first, mut last) = (usize::MAX, 0);
            for (x, (px, &b)) in row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(values)
                .enumerate()
            {
                let alpha = (b * gain + 0.5) as u8;
                let scaled = f64::from(b) * f64::from(gain);
                let [r, g, bl] =
                    rgb.map(|c| (f64::from(c) * scaled + 0.5) as u8);
                *px = match bgra {
                    true => [bl, g, r, alpha],
                    false => [r, g, bl, alpha],
                };
                if *px != [0; 4] {
                    first = first.min(x);
                    last = x;
                }
            }
            if first <= last {
                let row = IRect::new(
                    first as i32,
                    y as i32,
                    last as i32 + 1,
                    y as i32 + 1,
                );
                inked = IRect::join(&inked, &row);
            }
        }
    })?;
    // The inked part alone is kept, copied out of the region: it is what a
    // composite draws -- see `Outcome::composite`.
    if !trims_composite() {
        return Some(Outcome::whole(image, blurred.region, blurred.rest));
    }
    let image = match inked.is_empty() {
        true => image,
        false => image.make_subset(
            None,
            inked,
            RequiredProperties { mipmapped: false },
        )?,
    };
    Some(Outcome::with_inked(
        image,
        blurred.region,
        blurred.rest,
        inked,
    ))
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, Color, ColorType, IRect, ImageInfo, Matrix, Paint,
        PaintStyle, Path, PathBuilder, PixelGeometry, Rect, SurfaceProps,
        SurfacePropsFlags,
    };

    use super::{
        Coverage, Plane, READ_IN_PLACE, alpha_into, coloured, kept_plane,
        pool::{self, PlaneScope},
    };
    use crate::{
        canvas::{Canvas, CanvasOptions},
        context::{
            exact_blur,
            layer::{COVERAGE_ROUTE, COVERAGE_RUNS},
        },
        path::FillRule,
        pixels::{ImageDataExport, PixelColorSpace, PixelExportOptions},
        started,
    };

    /// A blurred shape on an eight-bit page, through the coverage route or
    /// the chain's layer, as premultiplied pixels at `density`.
    fn drawn(
        space: PixelColorSpace,
        shape: usize,
        colour: &str,
        sigma: f32,
        density: u32,
        coverage: bool,
    ) -> Vec<u8> {
        COVERAGE_ROUTE.with(|route| route.set(coverage));
        let mut canvas = Canvas::with_options(
            120.0,
            90.0,
            CanvasOptions {
                color_space: space,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_filter_css(&format!("blur({sigma}px)"))
            .expect("a filter");
        ctx.set_fill_style_css(colour).expect("a colour");
        ctx.set_stroke_style_css(colour).expect("a colour");
        ctx.begin_path();
        match shape {
            0 => {
                ctx.move_to(30.3, 20.6);
                ctx.line_to(82.7, 31.2);
                ctx.line_to(44.1, 70.9);
                ctx.close_path();
                ctx.fill(FillRule::NonZero);
            }
            1 => {
                ctx.ellipse(
                    60.3,
                    45.6,
                    7.5,
                    6.0,
                    0.0,
                    0.0,
                    std::f32::consts::TAU,
                    false,
                )
                .expect("an ellipse");
                ctx.fill(FillRule::NonZero);
            }
            2 => {
                ctx.move_to(20.5, 70.2);
                ctx.bezier_curve_to(40.0, 5.0, 80.0, 95.0, 100.3, 22.7);
                ctx.set_line_width(3.0);
                ctx.stroke();
            }
            _ => {
                for i in 0..11 {
                    let a = i as f32 * std::f32::consts::PI * 4.0 / 5.0;
                    let (x, y) = (60.2 + 30.0 * a.cos(), 45.4 + 30.0 * a.sin());
                    match i {
                        0 => ctx.move_to(x, y),
                        _ => ctx.line_to(x, y),
                    }
                }
                ctx.close_path();
                ctx.fill(FillRule::EvenOdd);
            }
        }
        let pixels = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    density,
                    // In the page's own space, so that nothing is converted.
                    pixels: PixelExportOptions {
                        color_space: space,
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        COVERAGE_ROUTE.with(|route| route.set(true));
        pixels
    }

    /// Further from a half-level than float noise moves a sum: a byte that
    /// differs from the exact product nearer a tie than this is a tie.
    const TIE: f64 = 1e-3;

    #[test]
    fn a_plane_read_where_it_lies_is_its_alpha_read_to_a8() {
        // Every layout a plane is read in place from, and one that is read
        // through `read_pixels`, each holding every alpha level; the plane's
        // rows are longer than its pixels, and the rectangle read starts
        // and stops inside them, so that neither a row's length nor its
        // first pixel is taken for the plane's.
        let (size, read) = ((37, 23), IRect::from_ltrb(3, 2, 31, 19));
        for (colour, alpha, in_place) in [
            (ColorType::Alpha8, AlphaType::Premul, true),
            (ColorType::RGBA8888, AlphaType::Premul, true),
            (ColorType::BGRA8888, AlphaType::Premul, true),
            (ColorType::RGBA8888, AlphaType::Unpremul, true),
            (ColorType::RGBAF16, AlphaType::Premul, false),
        ] {
            let info = ImageInfo::new(size, colour, alpha, None);
            let mut surface =
                started::raster(&info, info.min_row_bytes() + 24, None)
                    .expect("a plane");
            // Premultiplied RGBA whose alpha steps through every level
            // across the plane, its colour half of it.
            let pixels: Vec<u8> = (0..size.0 * size.1)
                .flat_map(|i| {
                    let a = (i * 7 % 256) as u8;
                    [a / 2, a / 3, a / 4, a]
                })
                .collect();
            let source = ImageInfo::new(
                size,
                ColorType::RGBA8888,
                AlphaType::Premul,
                None,
            );
            assert!(surface.canvas().write_pixels(
                &source,
                &pixels,
                source.min_row_bytes(),
                (0, 0)
            ));
            let line = read.width() as usize;
            let mut want = vec![0u8; line * read.height() as usize];
            let a8 = ImageInfo::new(
                read.size(),
                ColorType::Alpha8,
                AlphaType::Premul,
                None,
            );
            assert!(surface.read_pixels(
                &a8,
                &mut want,
                line,
                (read.left, read.top)
            ));
            // Not zero, so that a row the read misses shows.
            let mut got = vec![0x5au8; want.len()];
            let before = READ_IN_PLACE.with(|n| n.get());
            assert!(alpha_into(&mut surface, read, got.chunks_mut(line)));
            assert_eq!(got, want, "{colour:?} {alpha:?}");
            assert_eq!(
                READ_IN_PLACE.with(|n| n.get()) - before,
                usize::from(in_place),
                "{colour:?} {alpha:?}: read where it lies"
            );
        }
    }

    #[test]
    fn a_blurred_coverage_rounds_the_exact_product_once() {
        // Each pixel the route composites onto a transparent page, against
        // the exact Gaussian of the same coverage in f64 times the placed
        // colour and the gain, rounded once: the alpha and every channel,
        // in each eight-bit page space, at gains of 1 and on alphas'
        // bytes, every sigma summed tap by tap, a curve and a polygon.
        let shapes: [Path; 2] =
            [Path::oval(Rect::new(20.3, 15.6, 63.1, 49.8), None), {
                let mut b = PathBuilder::new();
                b.move_to((12.3, 40.6))
                    .line_to((70.7, 8.2))
                    .line_to((55.1, 66.9));
                b.close();
                b.detach()
            }];
        let (mut checked, mut ties) = (0usize, 0usize);
        for space in [
            PixelColorSpace::Srgb,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::Rec2020,
        ] {
            let skia = space.to_skia_color_space().expect("a space");
            let info = ImageInfo::new(
                (90, 80),
                ColorType::RGBA8888,
                AlphaType::Premul,
                skia,
            );
            for (shape, plane_kind) in shapes.iter().flat_map(|s| {
                [(s, super::Plane::Drawn), (s, super::Plane::Alpha)]
            }) {
                for sigma in [0.5f32, 1.5, 3.0, 4.9] {
                    for (rgb, stored) in [
                        ([200.0 / 255.0, 120.0 / 255.0, 40.0 / 255.0], 255u8),
                        ([21.0 / 179.0, 42.0 / 179.0, 140.0 / 179.0], 179),
                        ([137.0 / 140.0, 10.0 / 140.0, 77.0 / 140.0], 140),
                    ] {
                        let gain = f32::from(stored) / 255.0;
                        let mut surface =
                            crate::started::raster(&info, None, None)
                                .expect("a page");
                        let canvas = surface.canvas();
                        let mut paint = Paint::default();
                        paint.set_anti_alias(true).set_style(PaintStyle::Fill);
                        let coverage = Coverage::of(
                            canvas,
                            sigma,
                            Some(*shape.bounds()),
                            plane_kind,
                            |plane: &skia_safe::Canvas| {
                                plane.draw_path(shape, &paint);
                            },
                        )
                        .expect("a coverage");
                        assert_eq!(coverage.rest, Matrix::new_identity());
                        let (w, h) = (
                            coverage.region.width() as usize,
                            coverage.region.height() as usize,
                        );
                        let plane: Vec<u8> = coverage
                            .bytes()
                            .into_iter()
                            .flat_map(|v| [0, 0, 0, v])
                            .collect();
                        let exact = exact_blur::reference(
                            &plane,
                            w,
                            h,
                            coverage.sigma.0,
                            coverage.sigma.1,
                        );
                        let region = coverage.region;
                        let blurred = coverage.blurred();
                        coloured(canvas, &blurred, rgb, gain)
                            .expect("coloured")
                            .composite(canvas, &Paint::default());
                        let mut px = vec![0u8; 90 * 80 * 4];
                        assert!(surface.read_pixels(
                            &info,
                            &mut px,
                            90 * 4,
                            (0, 0)
                        ));
                        for y in 0..h {
                            for x in 0..w {
                                let (dx, dy) = (
                                    region.left + x as i32,
                                    region.top + y as i32,
                                );
                                if !(0..90).contains(&dx)
                                    || !(0..80).contains(&dy)
                                {
                                    continue;
                                }
                                let b = exact[(y * w + x) * 4 + 3]
                                    * f64::from(gain);
                                let at = (dy as usize * 90 + dx as usize) * 4;
                                let want = [
                                    f64::from(rgb[0]) * b,
                                    f64::from(rgb[1]) * b,
                                    f64::from(rgb[2]) * b,
                                    b,
                                ];
                                for c in 0..4 {
                                    let v = want[c];
                                    if (v - v.floor() - 0.5).abs() < TIE {
                                        ties += 1;
                                        continue;
                                    }
                                    assert_eq!(
                                        f64::from(px[at + c]),
                                        (v + 0.5).floor(),
                                        "{space:?}, sigma {sigma}, stored {stored}, ({dx}, {dy}) channel {c}"
                                    );
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 100_000, "{checked} values checked");
        assert!(ties * 1000 < checked, "{ties} ties of {checked}");
    }

    #[test]
    fn the_coverage_route_moves_only_an_edge_colour_byte_by_a_level() {
        // Against the chain's layer, on every eight-bit page space, with the
        // pixels read in the page's own space: the alpha everywhere and the
        // colour inside a shape bit for bit, an edge's colour by at most a
        // level, and some edge moved -- the route is not the chain. Taken on
        // every draw whose device sigma is summed tap by tap: 3 and 4.9 at
        // density 2 go through the FFT and keep the chain.
        let colours = [
            "rgb(200 120 40)",
            "rgb(30 60 200 / 0.7)",
            "black",
            "rgb(128 128 128 / 0.3)",
            "rgb(250 20 140 / 0.55)",
        ];
        for space in [
            PixelColorSpace::Srgb,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::Rec2020,
        ] {
            let (mut px, mut moved, mut worst, mut alpha, mut runs) =
                (0usize, 0usize, 0u8, 0usize, 0usize);
            for shape in 0..4 {
                for colour in colours {
                    for sigma in [0.5f32, 1.5, 3.0, 4.9] {
                        for density in [1, 2] {
                            let before = COVERAGE_RUNS.with(|r| r.get());
                            let ours = drawn(
                                space, shape, colour, sigma, density, true,
                            );
                            runs += COVERAGE_RUNS.with(|r| r.get()) - before;
                            let chain = drawn(
                                space, shape, colour, sigma, density, false,
                            );
                            for (a, b) in ours.chunks(4).zip(chain.chunks(4)) {
                                px += 1;
                                if a[3] != b[3] {
                                    alpha += 1;
                                }
                                let d = (0..3)
                                    .map(|c| a[c].abs_diff(b[c]))
                                    .max()
                                    .unwrap_or(0);
                                if d > 0 {
                                    moved += 1;
                                }
                                worst = worst.max(d);
                            }
                        }
                    }
                }
            }
            assert_eq!(alpha, 0, "{space:?}: alphas moved");
            assert!(worst <= 1, "{space:?}: a colour byte moved by {worst}");
            assert!(moved > 0, "{space:?}: nothing moved of {px} pixels");
            assert_eq!(runs, 4 * 5 * 4 * 2 - 4 * 5 * 2, "{space:?}");
            // Deep inside the triangle, past a reach of the blur at density 1.
            for colour in colours {
                for sigma in [0.5f32, 1.5] {
                    let ours = drawn(space, 0, colour, sigma, 1, true);
                    let chain = drawn(space, 0, colour, sigma, 1, false);
                    for (x, y) in [(51, 40), (52, 41), (53, 42), (50, 42)] {
                        let at = (y * 120 + x) * 4;
                        assert_eq!(
                            ours[at..at + 4],
                            chain[at..at + 4],
                            "{space:?} {colour} sigma {sigma}: interior ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_composite_drawn_only_where_inked_lands_every_byte() {
        // Blurred shapes and text over a page already painted, through the
        // coverage route, composited only where inked and as the whole
        // region: the same bytes under every blend, at either density and
        // under a turn. Counted, so that the trim is shown to have been
        // taken where a transparent pixel changes nothing and only there.
        use crate::{
            context::{
                chain::{TRIMMED, TRIMS_COMPOSITE},
                kept::release_kept_results,
                run_mask::release_cached_runs,
            },
            paint::BlendMode,
        };
        let page = |blend: BlendMode, density: u32, turn: f32, trims: bool| {
            // Nothing kept from the other run: each page makes its results.
            release_cached_runs();
            release_kept_results();
            TRIMS_COMPOSITE.set(trims);
            let mut canvas = Canvas::with_options(
                160.0,
                120.0,
                CanvasOptions {
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            for (i, colour) in
                ["rgb(30 140 200)", "rgb(240 200 20 / 0.6)", "rgb(90 20 60)"]
                    .iter()
                    .enumerate()
            {
                ctx.set_fill_style_css(colour).expect("a colour");
                ctx.fill_rect(i as f32 * 41.3, 10.0, 70.0, 100.0);
            }
            ctx.set_global_composite_operation(blend);
            ctx.rotate(turn);
            for (k, (sigma, colour, alpha)) in [
                (1.0, "rgb(200 120 40)", 1.0),
                (2.5, "black", 0.6),
                (4.0, "rgb(20 200 90 / 0.7)", 1.0),
            ]
            .into_iter()
            .enumerate()
            {
                let k = k as f32;
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                ctx.set_fill_style_css(colour).expect("a colour");
                ctx.set_global_alpha(alpha);
                ctx.begin_path();
                ctx.ellipse(
                    30.3 + 45.0 * k,
                    35.6,
                    9.5,
                    6.0,
                    0.3,
                    0.0,
                    std::f32::consts::TAU,
                    false,
                )
                .expect("an ellipse");
                ctx.fill(FillRule::NonZero);
                assert!(ctx.set_font_css("28px sans-serif"));
                ctx.fill_text("Ag", 12.4 + 48.0 * k, 92.7, None);
            }
            let before = TRIMMED.get();
            let pixels = canvas
                .to_image_data(
                    None,
                    &ImageDataExport {
                        density,
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels")
                .pixels()
                .to_vec();
            TRIMS_COMPOSITE.set(true);
            (pixels, TRIMMED.get() - before)
        };
        let leaves = [BlendMode::SourceOver];
        let clears = [
            BlendMode::Copy,
            BlendMode::DestinationIn,
            BlendMode::SourceIn,
            BlendMode::Lighter,
            BlendMode::Multiply,
        ];
        for (blend, density, turn) in leaves
            .into_iter()
            .chain(clears)
            .flat_map(|b| [(b, 1, 0.0), (b, 2, 0.0), (b, 1, 0.05)])
        {
            let what = format!("{blend:?} at density {density}, turned {turn}");
            let (whole, untrimmed) = page(blend, density, turn, false);
            let (trimmed, trims) = page(blend, density, turn, true);
            assert_eq!(untrimmed, 0, "{what}: the control trims nothing");
            assert!(whole == trimmed, "{what}: a byte moved");
            // A turn is drawn in the plane, so `rest` is the identity.
            let expected = leaves.contains(&blend);
            assert_eq!(trims > 0, expected, "{what}: trimmed {trims}");
        }
    }

    #[test]
    fn a_kept_plane_covers_as_a_plane_made_afresh() {
        // One playback of blurred shapes and text that overlap, so that a
        // draw reads where an earlier one drew on a kept plane; some cut by a
        // clip, so that a draw is cut short of the page, one of them a path
        // the plane's edge cuts; turned, scaled, at densities 1 and 2: the same
        // bytes with planes kept and made afresh for every draw.
        // Counted, so that planes are shown to be kept.
        use crate::context::{
            chain::pool::{KEEP_PLANES, PLANES_TAKEN},
            kept::release_kept_results,
            run_mask::release_cached_runs,
        };
        let page = |density: u32, keep: bool| {
            release_cached_runs();
            release_kept_results();
            KEEP_PLANES.set(keep);
            let mut canvas = Canvas::with_options(
                220.0,
                160.0,
                CanvasOptions {
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_fill_style_css("rgb(240 230 200)")
                .expect("a colour");
            ctx.fill_rect(0.0, 0.0, 220.0, 160.0);
            // A shape and a glyph covering most of the page first, so that
            // each kind of plane is inked nearly everywhere the later draws
            // read: an edge of what they read left as it was shows.
            ctx.set_filter_css("blur(1px)").expect("a filter");
            ctx.set_fill_style_css("rgb(60 60 160 / 0.5)")
                .expect("a colour");
            ctx.begin_path();
            ctx.ellipse(
                110.0,
                80.0,
                112.0,
                82.0,
                0.0,
                0.0,
                std::f32::consts::TAU,
                false,
            )
            .expect("an ellipse");
            ctx.fill(FillRule::NonZero);
            assert!(ctx.set_font_css("190px sans-serif"));
            ctx.fill_text("W", -6.0, 150.0, None);
            for k in 0..14 {
                let f = k as f32;
                let (x, y) =
                    (17.3 + 13.1 * f, 21.6 + 7.7 * (f * 1.3).sin().abs() * 9.0);
                ctx.save();
                if k % 3 == 1 {
                    ctx.begin_path();
                    ctx.rect(x - 6.0, y - 9.0, 21.0, 17.0);
                    ctx.clip(FillRule::NonZero);
                }
                if k % 4 == 2 {
                    ctx.translate(x, y);
                    ctx.rotate(0.3 * f);
                    ctx.scale(1.25, 0.8);
                    ctx.translate(-x, -y);
                }
                let sigma = [1.0, 2.5, 4.0][k % 3];
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                let colour =
                    ["rgb(200 120 40)", "black", "rgb(20 90 200 / 0.7)"][k % 3];
                ctx.set_fill_style_css(colour).expect("a colour");
                ctx.set_stroke_style_css(colour).expect("a colour");
                match k % 3 {
                    0 => {
                        ctx.begin_path();
                        ctx.ellipse(
                            x,
                            y,
                            9.5 + f * 0.4,
                            6.0,
                            0.2 * f,
                            0.0,
                            std::f32::consts::TAU,
                            false,
                        )
                        .expect("an ellipse");
                        ctx.fill(FillRule::NonZero);
                    }
                    1 => {
                        ctx.begin_path();
                        ctx.move_to(x - 8.4, y + 9.1);
                        ctx.bezier_curve_to(
                            x,
                            y - 20.0,
                            x + 9.0,
                            y + 18.0,
                            x + 16.3,
                            y - 3.7,
                        );
                        ctx.set_line_width(2.5);
                        ctx.stroke();
                    }
                    _ => {
                        assert!(ctx.set_font_css(&format!(
                            "{}px sans-serif",
                            14 + 5 * (k % 5)
                        )));
                        ctx.fill_text("Ag&", x - 9.6, y + 6.2, None);
                    }
                }
                ctx.restore();
            }
            // A path the plane made for its draw cuts: a clip at x 82 ends
            // that plane short of the triangle's right corner, which a kept
            // plane as large as the page would draw whole, and Skia covers
            // the triangle's left edge otherwise when its corner is cut --
            // see `chain::skia_rasterises_a_path_its_clip_cuts_another_way`.
            ctx.save();
            ctx.begin_path();
            ctx.rect(0.0, 0.0, 82.0, 160.0);
            ctx.clip(FillRule::NonZero);
            ctx.set_filter_css("blur(1px)").expect("a filter");
            ctx.set_fill_style_css("black").expect("a colour");
            ctx.begin_path();
            ctx.move_to(20.0, 80.0);
            ctx.line_to(80.0, 15.25);
            ctx.line_to(140.5, 85.0);
            ctx.close_path();
            ctx.fill(FillRule::NonZero);
            ctx.restore();
            let before = PLANES_TAKEN.get();
            let pixels = canvas
                .to_image_data(
                    None,
                    &ImageDataExport {
                        density,
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels")
                .pixels()
                .to_vec();
            KEEP_PLANES.set(true);
            (pixels, PLANES_TAKEN.get() - before)
        };
        for density in [1, 2] {
            let (afresh, none) = page(density, false);
            let (kept, taken) = page(density, true);
            assert_eq!(none, 0, "density {density}: the control keeps none");
            assert!(afresh == kept, "density {density}: a byte moved");
            assert!(taken > 0, "density {density}: no plane was kept");
        }
    }

    #[test]
    fn a_draw_the_route_cannot_take_keeps_the_chain() {
        // A float page, and a sigma through the FFT, take the chain's layer;
        // the same draw on an eight-bit page at a tapped sigma takes the
        // route. Counted, since the bytes alone cannot tell the two apart.
        let runs = |depth: crate::pixels::PixelDepth, sigma: f32| {
            let mut canvas = Canvas::with_options(
                60.0,
                50.0,
                CanvasOptions {
                    color_type: depth,
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_filter_css(&format!("blur({sigma}px)"))
                .expect("a filter");
            ctx.set_fill_style_css("rgb(200 120 40)").expect("a colour");
            ctx.begin_path();
            ctx.move_to(10.3, 8.6);
            ctx.line_to(48.7, 15.2);
            ctx.line_to(20.1, 40.9);
            ctx.close_path();
            ctx.fill(FillRule::NonZero);
            let before = COVERAGE_RUNS.with(|r| r.get());
            canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels");
            COVERAGE_RUNS.with(|r| r.get()) - before
        };
        use crate::pixels::PixelDepth;
        assert_eq!(runs(PixelDepth::Uint8, 3.0), 1, "an eight-bit page");
        assert_eq!(runs(PixelDepth::F16, 3.0), 0, "a half-float page");
        assert_eq!(runs(PixelDepth::F32, 3.0), 0, "a float page");
        assert_eq!(runs(PixelDepth::Uint8, 6.0), 0, "a sigma through the FFT");
    }

    /// Props no canvas has by default, told apart by their pixel geometry.
    fn props(geometry: PixelGeometry) -> SurfaceProps {
        SurfaceProps::new(SurfacePropsFlags::default(), geometry)
    }

    #[test]
    fn a_kept_plane_is_taken_only_for_a_canvas_of_its_props() {
        // A plane takes its canvas's props, which decide how text is drawn
        // on it, so a plane kept for one canvas is not taken for a canvas
        // of other props.
        let info = ImageInfo::new_n32_premul((40, 30), None);
        let mut own =
            started::raster(&info, None, Some(&props(PixelGeometry::RGBH)))
                .expect("a surface");
        let mut other =
            started::raster(&info, None, Some(&props(PixelGeometry::BGRV)))
                .expect("a surface");
        let drawn = IRect::from_wh(20, 10);
        let _planes = PlaneScope::open();
        let (plane, used) =
            kept_plane(own.canvas(), Plane::Drawn, drawn).expect("a plane");
        assert!(!used, "a plane before any was kept");
        assert_eq!(plane.props(), &props(PixelGeometry::RGBH));
        pool::keep_plane(Plane::Drawn, plane);
        let (_, used) =
            kept_plane(other.canvas(), Plane::Drawn, drawn).expect("a plane");
        assert!(!used, "a plane kept for other props was taken");
        let (_, used) =
            kept_plane(own.canvas(), Plane::Drawn, drawn).expect("a plane");
        assert!(used, "the plane kept for these props was not taken");
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "skia-safe 0.156.0 calls getBaseProps with the wrong ABI on Windows"
    )]
    #[allow(
        clippy::disallowed_methods,
        reason = "the call `clippy.toml` refuses, tested"
    )]
    fn skia_safe_reads_base_props_as_props_does() {
        // `kept_plane` reads a canvas's props through `Canvas::props`; this
        // is the call it does not make. Where `base_props` passes its
        // arguments as Skia expects, both read the same props and the canvas
        // still draws.
        let info = ImageInfo::new_n32_premul((4, 4), None);
        let mut surface =
            started::raster(&info, None, Some(&props(PixelGeometry::RGBH)))
                .expect("a surface");
        let canvas = surface.canvas();
        assert_eq!(canvas.base_props(), props(PixelGeometry::RGBH));
        assert_eq!(canvas.props(), Some(props(PixelGeometry::RGBH)));
        canvas.clear(Color::RED);
        let mut pixel = [0u8; 4];
        let rgba = ImageInfo::new(
            (1, 1),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        assert!(surface.read_pixels(&rgba, &mut pixel, 4, (0, 0)));
        assert_eq!(pixel, [255, 0, 0, 255], "a red pixel");
    }
}

#[cfg(test)]
mod inked_tests {
    //! That the coverage route's plane holds what the shortcuts it takes
    //! leave out: the inked rectangle [`super::Placement::of`] gives holds
    //! every pixel a draw covers, so a page drawn reading only it is the page
    //! drawn reading the whole region, across what decides how far a draw
    //! reaches past its path; and a fill's plane drawn without the exact
    //! colour's shader is the plane drawn with it.

    use std::f32::consts::{PI, TAU};

    use super::{INKED_IS_REGION, INKED_WITHIN};
    use crate::{
        context::layer::{COVERAGE_RUNS, SHADED_PLANE},
        pixels::PixelColorSpace,
        prelude::*,
    };

    /// The family Oswald is registered under here, so that it cannot be
    /// found as a system font of the same name instead.
    const OSWALD: &str = "InkedOswald";

    /// A five-pointed star of radius 30 about the canvas's centre.
    fn star(ctx: &mut Context2D) {
        ctx.begin_path();
        for i in 0..5 {
            let a = i as f32 * PI * 4.0 / 5.0 - PI / 2.0;
            let (x, y) = (60.0 + 30.0 * a.cos(), 45.0 + 30.0 * a.sin());
            match i {
                0 => ctx.move_to(x, y),
                _ => ctx.line_to(x, y),
            }
        }
        ctx.close_path();
    }

    /// An open zigzag whose turns are sharp enough to reach far past its
    /// points with a miter join.
    fn zigzag(ctx: &mut Context2D) {
        ctx.begin_path();
        ctx.move_to(25.0, 60.0);
        ctx.line_to(45.0, 25.0);
        ctx.line_to(55.0, 62.0);
        ctx.line_to(75.0, 28.0);
        ctx.line_to(95.0, 58.0);
    }

    /// A draw onto a context, its paint and transform set.
    type Draw = fn(&mut Context2D);

    /// The draws swept: each a way a draw reaches past its path's bounds,
    /// or a shape the bounds fit tightly. A stroke's bounds reach half its
    /// width times the miter limit past its path, so the strokes without a
    /// miter join set the limit to 1, where the bounds are tightest: half the
    /// width times the square root of 2, which a square cap's corner
    /// reaches. A dashed stroke takes the chain's layer and not this route.
    const DRAWS: [(&str, Draw); 13] = [
        ("filled star", |ctx| {
            star(ctx);
            ctx.fill(FillRule::EvenOdd);
        }),
        ("filled ellipse", |ctx| {
            ctx.begin_path();
            ctx.ellipse(60.0, 45.0, 7.5, 6.0, 0.3, 0.0, TAU, false)
                .expect("an ellipse");
            ctx.fill(FillRule::NonZero);
        }),
        ("mitred star, limit 10", |ctx| {
            ctx.set_line_width(4.0);
            ctx.set_line_join(StrokeJoin::Miter);
            ctx.set_miter_limit(10.0);
            star(ctx);
            ctx.stroke();
        }),
        ("mitred zigzag, limit 4", |ctx| {
            ctx.set_line_width(5.0);
            ctx.set_line_join(StrokeJoin::Miter);
            ctx.set_miter_limit(4.0);
            zigzag(ctx);
            ctx.stroke();
        }),
        ("square caps", |ctx| {
            ctx.set_line_width(6.0);
            ctx.set_miter_limit(1.0);
            ctx.set_line_cap(StrokeCap::Square);
            ctx.begin_path();
            ctx.move_to(30.0, 30.0);
            ctx.line_to(90.0, 60.0);
            ctx.stroke();
        }),
        ("round caps and joins", |ctx| {
            ctx.set_line_width(7.0);
            ctx.set_miter_limit(1.0);
            ctx.set_line_cap(StrokeCap::Round);
            ctx.set_line_join(StrokeJoin::Round);
            zigzag(ctx);
            ctx.stroke();
        }),
        ("thin stroke", |ctx| {
            ctx.set_line_width(0.1);
            ctx.set_miter_limit(1.0);
            zigzag(ctx);
            ctx.stroke();
        }),
        ("stroked curve", |ctx| {
            ctx.set_line_width(3.0);
            ctx.set_miter_limit(1.0);
            ctx.begin_path();
            ctx.move_to(20.5, 70.2);
            ctx.bezier_curve_to(40.0, 5.0, 80.0, 95.0, 100.3, 22.7);
            ctx.stroke();
        }),
        ("text filled", |ctx| {
            assert!(ctx.set_font_css(&format!("32px {OSWALD}")));
            ctx.fill_text("Ajgy", 30.0, 58.0, None);
        }),
        ("text stroked", |ctx| {
            assert!(ctx.set_font_css(&format!("32px {OSWALD}")));
            ctx.set_line_width(3.0);
            ctx.set_line_join(StrokeJoin::Miter);
            ctx.stroke_text("AVW", 30.0, 58.0, None);
        }),
        ("small hinted text", |ctx| {
            assert!(ctx.set_font_css("11px sans-serif"));
            ctx.fill_text("Hamburgefonstiv", 18.0, 50.0, None);
        }),
        // The size whose glyph masks land a level off an A8 plane's on a
        // page of the page's own type -- see `layer::CoverageRun`.
        ("16px text", |ctx| {
            assert!(ctx.set_font_css(&format!("16px {OSWALD}")));
            ctx.fill_text("Ajgy", 30.0, 50.0, None);
        }),
        ("text drawn at a half", |ctx| {
            ctx.scale(0.5, 0.5);
            assert!(ctx.set_font_css(&format!("40px {OSWALD}")));
            ctx.fill_text("Ajgy", 60.0, 100.0, None);
        }),
    ];

    /// Transforms the draws are swept under: none, a turn, a skew and an
    /// uneven scale, each about the canvas's centre.
    const FRAMES: [Affine; 4] = [
        Affine::IDENTITY,
        Affine {
            a: 0.866,
            b: 0.5,
            c: -0.5,
            d: 0.866,
            tx: 30.5,
            ty: -24.0,
        },
        Affine {
            a: 1.0,
            b: 0.0,
            c: 0.4,
            d: 1.0,
            tx: -18.0,
            ty: 0.0,
        },
        Affine {
            a: 1.7,
            b: 0.0,
            c: 0.0,
            d: 0.6,
            tx: -42.0,
            ty: 18.0,
        },
    ];

    /// Sub-pixel places a draw is moved to, so that its edges fall at each
    /// quarter of a pixel along each axis.
    const PHASES: [(f32, f32); 4] =
        [(0.0, 0.0), (0.25, 0.5), (0.5, 0.75), (0.75, 0.25)];

    /// How a page is drawn: what it is held to, and its colour and space.
    #[derive(Clone, Copy)]
    struct Way {
        /// The inked rectangle taken as the whole region.
        whole: bool,
        /// The coverage plane drawn through the chain's shaded paint.
        shaded: bool,
        /// The fill and stroke colour, and the page's colour space.
        colour: &'static str,
        space: PixelColorSpace,
    }

    /// As the route draws: the inked rectangle, the plain paint, on an sRGB
    /// page.
    const ROUTE: Way = Way {
        whole: false,
        shaded: false,
        colour: "rgb(200 120 40)",
        space: PixelColorSpace::Srgb,
    };

    /// `draw` under `frame`, moved by `phase`, blurred by `sigma` on a
    /// 120x90 eight-bit CPU page at `density`, drawn `way`, as premultiplied
    /// pixels.
    fn page(
        draw: Draw,
        frame: Affine,
        phase: (f32, f32),
        sigma: f32,
        density: u32,
        way: Way,
    ) -> Vec<u8> {
        INKED_IS_REGION.with(|w| w.set(way.whole));
        SHADED_PLANE.with(|s| s.set(way.shaded));
        let mut canvas = Canvas::with_options(
            120.0,
            90.0,
            CanvasOptions {
                gpu: false,
                color_space: way.space,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_filter_css(&format!("blur({sigma}px)"))
            .expect("a filter");
        ctx.set_fill_style_css(way.colour).expect("a colour");
        ctx.set_stroke_style_css(way.colour).expect("a colour");
        ctx.translate(phase.0, phase.1);
        ctx.transform(frame);
        draw(ctx);
        let pixels = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    density,
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        INKED_IS_REGION.with(|w| w.set(false));
        SHADED_PLANE.with(|s| s.set(false));
        pixels
    }

    #[test]
    fn a_draw_covers_nothing_outside_its_inked_rectangle() {
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let mut differ = Vec::new();
        let (mut cases, mut routed) = (0, 0);
        for (name, draw) in DRAWS {
            let mut within = 0;
            for (f, frame) in FRAMES.into_iter().enumerate() {
                for phase in PHASES {
                    for (sigma, density) in [(0.6, 1), (1.5, 1), (1.5, 2)] {
                        let (runs, inked) = (
                            COVERAGE_RUNS.with(|r| r.get()),
                            INKED_WITHIN.with(|n| n.get()),
                        );
                        let ours =
                            page(draw, frame, phase, sigma, density, ROUTE);
                        routed +=
                            usize::from(COVERAGE_RUNS.with(|r| r.get()) > runs);
                        within +=
                            usize::from(INKED_WITHIN.with(|n| n.get()) > inked);
                        let whole = page(
                            draw,
                            frame,
                            phase,
                            sigma,
                            density,
                            Way {
                                whole: true,
                                ..ROUTE
                            },
                        );
                        cases += 1;
                        if ours != whole {
                            differ.push(format!(
                                "{name}, frame {f}, phase {phase:?}, sigma {sigma}, density {density}"
                            ));
                        }
                    }
                }
            }
            // The control: somewhere its inked rectangle was smaller than
            // its region, or the draw held the rectangle to nothing.
            assert!(within > 0, "{name}: inked across its whole region");
        }
        assert!(
            differ.is_empty(),
            "{} of {cases} differ: {differ:#?}",
            differ.len()
        );
        assert_eq!(routed, cases, "draws that missed the coverage route");
    }

    #[test]
    fn a_fill_drawn_plain_holds_what_the_shaded_paint_covers() {
        // The route draws a fill's plane without the exact colour's shader.
        // Each fill, in colours opaque and not, on every eight-bit page
        // space, under each transform, is the same page drawn through the
        // chain's shaded paint. A stroke keeps the shader -- see
        // `BlurOp::playback` -- so it is not swept.
        let fills: [(&str, Draw); 4] = [
            DRAWS[0],
            DRAWS[1],
            ("filled zigzag", |ctx| {
                zigzag(ctx);
                ctx.fill(FillRule::NonZero);
            }),
            ("sliver a third of a pixel wide", |ctx| {
                ctx.begin_path();
                ctx.rect(30.2, 20.1, 0.33, 50.0);
                ctx.rect(40.6, 45.3, 60.0, 0.33);
                ctx.fill(FillRule::NonZero);
            }),
        ];
        let colours = [
            "rgb(200 120 40)",
            "rgb(30 60 200 / 0.7)",
            "black",
            "rgb(250 20 140 / 0.55)",
        ];
        let spaces = [
            PixelColorSpace::Srgb,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::Rec2020,
        ];
        let mut differ = Vec::new();
        let (mut cases, mut routed) = (0, 0);
        for (name, draw) in fills {
            for (f, frame) in FRAMES.into_iter().enumerate() {
                for phase in [PHASES[0], PHASES[2]] {
                    for (sigma, density) in [(0.6, 1), (1.5, 1), (1.5, 2)] {
                        for colour in colours {
                            for space in spaces {
                                let way = Way {
                                    colour,
                                    space,
                                    ..ROUTE
                                };
                                let runs = COVERAGE_RUNS.with(|r| r.get());
                                let plain = page(
                                    draw, frame, phase, sigma, density, way,
                                );
                                routed += usize::from(
                                    COVERAGE_RUNS.with(|r| r.get()) > runs,
                                );
                                let shaded = page(
                                    draw,
                                    frame,
                                    phase,
                                    sigma,
                                    density,
                                    Way {
                                        shaded: true,
                                        ..way
                                    },
                                );
                                cases += 1;
                                if plain != shaded {
                                    differ.push(format!(
                                        "{name}, frame {f}, phase {phase:?}, sigma {sigma}, density {density}, {colour}, {space:?}"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} of {cases} differ: {differ:#?}",
            differ.len()
        );
        // The control: every draw took the route, or the paint it draws its
        // plane through decided nothing.
        assert_eq!(routed, cases, "draws that missed the coverage route");
    }

    #[test]
    fn a_stroke_draws_its_plane_through_the_shaded_paint() {
        // A stroke thinner than a pixel is a hairline with its paint's alpha
        // cut to a byte by its width, which a shaded paint and a plain one
        // round apart, so the route keeps the shaded paint for every stroke
        // -- see `BlurOp::playback`. Each stroke here, drawn as the route
        // draws it, is the page drawn through the shaded paint; were strokes
        // drawn plain, the 0.1-wide one would differ.
        let strokes: Vec<(&str, Draw)> = DRAWS
            .into_iter()
            // Text takes its own route, which this paint does not reach.
            .filter(|(name, _)| {
                !name.contains("text")
                    && ["stroke", "caps", "mitred"]
                        .iter()
                        .any(|k| name.contains(k))
            })
            .collect();
        assert!(
            strokes.iter().any(|(name, _)| *name == "thin stroke"),
            "the control: the sweep holds a stroke thinner than a pixel"
        );
        let mut differ = Vec::new();
        let (mut cases, mut routed) = (0, 0);
        for (name, draw) in strokes {
            for (f, frame) in FRAMES.into_iter().enumerate() {
                for phase in [PHASES[0], PHASES[2]] {
                    for (sigma, density) in [(0.6, 1), (1.5, 1), (1.5, 2)] {
                        for colour in
                            ["rgb(200 120 40)", "rgb(30 60 200 / 0.7)"]
                        {
                            let way = Way { colour, ..ROUTE };
                            let runs = COVERAGE_RUNS.with(|r| r.get());
                            let route =
                                page(draw, frame, phase, sigma, density, way);
                            routed += usize::from(
                                COVERAGE_RUNS.with(|r| r.get()) > runs,
                            );
                            let shaded = page(
                                draw,
                                frame,
                                phase,
                                sigma,
                                density,
                                Way {
                                    shaded: true,
                                    ..way
                                },
                            );
                            cases += 1;
                            if route != shaded {
                                differ.push(format!(
                                    "{name}, frame {f}, phase {phase:?}, sigma {sigma}, density {density}, {colour}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{} of {cases} differ: {differ:#?}",
            differ.len()
        );
        assert_eq!(routed, cases, "draws that missed the coverage route");
    }
}
