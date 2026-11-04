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
    AlphaType, Canvas as SkCanvas, Color, ColorType, IRect, ImageInfo, Matrix,
    Paint, Rect, Surface,
};

use super::{
    Depth, Outcome, Step, layer_info, layer_surface, placed, region, split,
};
use crate::{
    context::{blur_mask::baked_image, exact_blur},
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
}

/// Writes the alpha of `surface`'s pixels in `read` into `rows`, one row of
/// `read` each, as each byte's value. A raster surface's pixels are read
/// where they lie where each holds its alpha as a byte -- an A8 plane, or a
/// premultiplied or unpremultiplied RGBA or BGRA one -- which takes the byte
/// `read_pixels` to A8 would copy, without its conversion or a copy of the
/// plane. Any other surface goes through `read_pixels`. `false` where the
/// pixels cannot be read.
fn alpha_into<'a>(
    surface: &mut Surface,
    read: IRect,
    rows: impl Iterator<Item = &'a mut [f32]>,
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
/// `stride` bytes apart, into `rows` as its value. Each a constant, so that
/// a row is read as fixed-size pixels and the compiler can take several at
/// once.
fn alpha_rows<'a, const STEP: usize, const AT: usize>(
    rows: impl Iterator<Item = &'a mut [f32]>,
    bytes: &[u8],
    stride: usize,
) {
    rows.zip(bytes.chunks(stride)).for_each(|(row, pixels)| {
        let (pixels, _) = pixels.as_chunks::<STEP>();
        row.iter_mut()
            .zip(pixels)
            .for_each(|(v, pixel)| *v = f32::from(pixel[AT]));
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

/// A draw's coverage before it is blurred: one float a pixel of `region`,
/// a byte's value, and the device sigmas the blur takes along x and y.
pub(crate) struct Coverage {
    values: Vec<f32>,
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
        let base = canvas.local_to_device_as_3x3();
        let (layer, rest) = split(&base)?;
        let steps = [Step::Blur(sigma)];
        let (to_draw, worked) = region(canvas, &steps, content, &layer, &rest)?;
        let (region, drawn) =
            placed(canvas, to_draw, rest.is_identity(), false);
        let worked = IRect::join(&worked, &region);
        let surface = |drawn: IRect| {
            let surface = match plane {
                Plane::Drawn => {
                    let info = ImageInfo::new(
                        drawn.size(),
                        ColorType::Alpha8,
                        AlphaType::Premul,
                        None,
                    );
                    canvas
                        .new_surface(&info, None)
                        .or_else(|| started::raster(&info, None, None))
                }
                Plane::Alpha => layer_surface(canvas, Depth::Bytes, drawn),
            };
            surface.map(|surface| (surface, drawn))
        };
        // The region alone where a plane from the device's origin cannot be
        // made, as `run` falls back for its layer.
        let (mut surface, drawn) =
            surface(drawn).or_else(|| surface(region))?;
        let offscreen = surface.canvas();
        offscreen.clear(Color::TRANSPARENT);
        let origin = Matrix::translate((-drawn.left as f32, -drawn.top as f32));
        offscreen.set_matrix(&Matrix::concat(&origin, &layer).into());
        draw(offscreen);
        // The region's coverage, as floats, on a plane of what the blur works
        // on: +0.0 around it, where nothing is painted.
        let (w, h) = (worked.width() as usize, worked.height() as usize);
        let read = region.with_offset((-drawn.left, -drawn.top));
        let mut values = vec![0.0f32; w * h];
        let (dx, dy) = (
            (region.left - worked.left) as usize,
            (region.top - worked.top) as usize,
        );
        let line = read.width() as usize;
        let rows = values[dy * w..]
            .chunks_mut(w)
            .take(read.height() as usize)
            .map(|row| &mut row[dx..][..line]);
        if !alpha_into(&mut surface, read, rows) {
            return None;
        }
        let scale = (layer.scale_x().abs(), layer.scale_y().abs());
        Some(Coverage {
            values,
            region: worked,
            rest,
            sigma: (sigma * scale.0, sigma * scale.1),
        })
    }

    /// Each pixel's coverage, a byte's value, row by row across
    /// [`Self::region`]: the read rectangle's and +0.0 around it, out to
    /// what the blur works on. Everything the blur reads but the sigmas.
    pub(crate) fn values(&self) -> &[f32] {
        &self.values
    }

    /// The device pixels [`Self::values`] cover: the read rectangle widened
    /// by what the blur works on.
    pub(crate) fn region(&self) -> IRect {
        self.region
    }

    /// This coverage blurred exactly, unrounded.
    pub(crate) fn blurred(self) -> Blurred {
        let (w, h) =
            (self.region.width() as usize, self.region.height() as usize);
        let values = exact_blur::blur_alpha(
            self.values,
            w,
            h,
            self.sigma.0,
            self.sigma.1,
        );
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
    let image = baked_image(&info, |out, stride| {
        for (row, values) in
            out.chunks_mut(stride).zip(blurred.values.chunks(w))
        {
            for (px, &b) in row.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                let alpha = (b * gain + 0.5) as u8;
                let scaled = f64::from(b) * f64::from(gain);
                let [r, g, bl] =
                    rgb.map(|c| (f64::from(c) * scaled + 0.5) as u8);
                *px = match bgra {
                    true => [bl, g, r, alpha],
                    false => [r, g, bl, alpha],
                };
            }
        }
    })?;
    Some(Outcome {
        image,
        region: blurred.region,
        rest: blurred.rest,
    })
}

/// `blurred` coloured -- see [`coloured`] -- and composited onto `canvas`
/// through `paint`, as the chain's result is. `None` where no image could be
/// made.
pub(crate) fn composite(
    canvas: &SkCanvas,
    blurred: &Blurred,
    rgb: [f32; 3],
    gain: f32,
    paint: &Paint,
) -> Option<()> {
    coloured(canvas, blurred, rgb, gain)?.composite(canvas, paint);
    Some(())
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, ColorType, IRect, ImageInfo, Matrix, Paint, PaintStyle,
        Path, PathBuilder, Rect,
    };

    use super::{Coverage, READ_IN_PLACE, alpha_into, composite};
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
            let mut got = vec![0.5f32; want.len()];
            let before = READ_IN_PLACE.with(|n| n.get());
            assert!(alpha_into(&mut surface, read, got.chunks_mut(line)));
            let got: Vec<u8> = got
                .iter()
                .map(|&v| {
                    assert_eq!(v.fract(), 0.0, "{colour:?}: a byte's value");
                    v as u8
                })
                .collect();
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
                            .values
                            .iter()
                            .flat_map(|&v| [0, 0, 0, v as u8])
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
                        composite(
                            canvas,
                            &blurred,
                            rgb,
                            gain,
                            &Paint::default(),
                        )
                        .expect("composited");
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
}
