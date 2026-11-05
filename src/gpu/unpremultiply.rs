//! An eight-bit GPU page unpremultiplied on the GPU, to the byte the CPU's
//! table gives.
//!
//! A read that wants unpremultiplied pixels of a page held premultiplied is
//! converted somewhere. Skia's own GPU conversion rounds down -- see
//! `read_converted_on_cpu` in `context::page` -- so the conversion is not
//! handed to it. Instead a pass of our own draws the read's area of the page
//! into a target of its own, looking each channel up in
//! `context::page::unpremultiply_table`, the table the CPU path converts with,
//! uploaded as an `Alpha8` texture. The index is the source byte; the texel is
//! fetched at its centre with no filtering; the result is written as a byte.
//! Every value is a whole number of 255ths, and nothing of the conversion is
//! worked out on the GPU, so the bytes are the table's on any GPU whose 8-bit
//! targets store k/255 as k. `every_pair_through_the_pass_is_the_tables`
//! checks that on the device it runs on. The target's bytes are then read as
//! stored. A read smaller than [`MIN_PASS_PIXELS`] is not given the pass.
//!
//! This is the shape Skia's own conversion has -- a draw into a temporary
//! target and a copy of that (`SurfaceContext::asyncRescaleAndReadPixels`,
//! src/gpu/ganesh/SurfaceContext.cpp under the skia-bindings crate) -- with the
//! table in place of its divide.

use std::sync::LazyLock;

use skia_safe::{
    AlphaType, BlendMode, ColorType, Data, IRect, Image, ImageInfo, Paint,
    Rect, RuntimeEffect, SamplingOptions, Surface, TileMode,
    gpu::{Budgeted, SurfaceOrigin, surfaces},
    runtime_effect::ChildPtr,
};

use crate::{context::page::unpremultiply_table, started};

/// The pass. Each byte of the source is recovered from the half the GPU
/// samples it as -- a half holds k/255 to within 1.2e-4, so `floor(v * 255 +
/// 0.5)` is k exactly -- and used as a texel index, row by alpha, column by
/// channel. Alpha is passed through.
const LOOKUP_SKSL: &str = "
uniform shader page;
uniform shader table;

half lookup(float channel, float alpha) {
    return table.eval(float2(channel + 0.5, alpha + 0.5)).a;
}

half4 main(float2 p) {
    half4 s = page.eval(p);
    float alpha = floor(s.a * 255.0 + 0.5);
    float3 c = floor(s.rgb * 255.0 + 0.5);
    return half4(lookup(c.r, alpha), lookup(c.g, alpha), lookup(c.b, alpha), s.a);
}
";

/// The fewest pixels a read converts through the pass. A smaller read is
/// converted on the CPU, one thread, where the table costs less than the
/// pass's draw and target.
///
/// Measured in release on Apple silicon (M4 Pro), a whole `getImageData` of a
/// GPU page, median of 20 rounds of 200 reads, the pass against the CPU route,
/// after a 1-pixel draw and after a blurred rect: 206 and 204 microseconds
/// against 198 and 198 at 64x48, 205 and 204 against 200 and 203 at 96x72,
/// 206 and 202 against 204 and 205 at 128x96, 208 and 206 against 208 and 214
/// at 160x120, and 214 and 212 against 232 and 244 at 256x192. The pass was
/// faster in 0 to 5 rounds of 20 up to 96x72, 6 and 11 at 128x96, 14 and 17
/// at 160x120 and 19 and 20 at 256x192, so the floor is 160x120. A loop this
/// light can leave the GPU at its lowest clock, where a pass costs several
/// times what it does under load, so a busier GPU may cross over lower.
pub(crate) const MIN_PASS_PIXELS: usize = 160 * 120;

thread_local! {
    /// [`LOOKUP_SKSL`], compiled once per thread: a `RuntimeEffect` is neither
    /// `Send` nor `Sync`.
    static LOOKUP: Option<RuntimeEffect> =
        RuntimeEffect::make_for_shader(LOOKUP_SKSL, None).ok();
}

/// The table as a raster `Alpha8` image, 256 rows of 256: row `alpha`, column
/// `channel`. Rows 0 and 255 are the identity, because the CPU path keeps a
/// pixel of either alpha as it is rather than looking it up.
///
/// A raster image rather than a texture of one context: the GPU uploads it to
/// each context that draws with it the first time, keeps the upload in that
/// context's own cache by the image's id, and drops it with the context.
static TABLE: LazyLock<Option<Image>> = LazyLock::new(|| {
    let table = unpremultiply_table();
    let mut bytes = Vec::with_capacity(256 * 256);
    for (alpha, row) in table.iter().enumerate() {
        match alpha {
            0 | 255 => bytes.extend(0..=u8::MAX),
            _ => bytes.extend_from_slice(row),
        }
    }
    let info =
        ImageInfo::new((256, 256), ColorType::Alpha8, AlphaType::Premul, None);
    started::raster_from_data(&info, Data::new_copy(&bytes), 256)
});

/// The `info`-sized area at `at` of `surface`, a GPU page, unpremultiplied
/// into a target of its own, `info`'s colour type and space, premultiplied
/// in name only: its bytes are the unpremultiplied pixels, to be read as
/// stored. `None` where the surface is not a GPU one, or the pass cannot be
/// drawn, and the caller converts on the CPU instead. It draws an area of any
/// size; holding a smaller read to the CPU under [`MIN_PASS_PIXELS`] is the
/// caller's.
///
/// The pass is drawn, not submitted: whatever reads the target commits it with
/// the page's draws, and confirms it with them.
pub(crate) fn unpremultiplied(
    surface: &mut Surface,
    info: &ImageInfo,
    at: (i32, i32),
) -> Option<Surface> {
    let mut context = surface.direct_context()?;
    let page = surface.image_snapshot();
    let table = TABLE.as_ref()?;
    let nearest = SamplingOptions::default();
    let clamp = (TileMode::Clamp, TileMode::Clamp);
    let children = [
        ChildPtr::Shader(page.to_raw_shader(clamp, nearest, None)?),
        ChildPtr::Shader(table.to_raw_shader(clamp, nearest, None)?),
    ];
    let shader = LOOKUP.with(|effect| {
        effect
            .as_ref()
            .and_then(|e| e.make_shader(Data::new_empty(), &children, None))
    })?;
    let target_info = info.with_alpha_type(AlphaType::Premul);
    let mut target = surfaces::render_target(
        &mut context,
        Budgeted::Yes,
        &target_info,
        None,
        SurfaceOrigin::TopLeft,
        None,
        false,
        None,
    )?;
    let mut paint = Paint::default();
    paint.set_shader(shader);
    paint.set_blend_mode(BlendMode::Src);
    let area = IRect::from_xywh(at.0, at.1, info.width(), info.height());
    let canvas = target.canvas();
    canvas.translate((-at.0 as f32, -at.1 as f32));
    canvas.draw_rect(Rect::from(area), &paint);
    Some(target)
}

#[cfg(test)]
mod tests {
    use skia_safe::{ColorSpace, Pixmap, named_primaries, named_transfer_fn};

    use super::*;
    use crate::{
        Canvas, CanvasOptions, PixelColorSpace, PixelExportOptions, RgbaLinear,
        canvas::EngineKind,
        context::page::{ExportOptions, read_converted},
        gpu::RenderingEngine,
    };

    /// What the CPU route gives a channel, worked from the pipeline's
    /// arithmetic rather than read from the table the pass uploads: kept at
    /// alpha 0 and 255, divided otherwise.
    fn want(channel: u8, alpha: u8) -> u8 {
        if alpha == 0 || alpha == u8::MAX {
            return channel;
        }
        let scale = 1.0 / f32::from(u8::MAX);
        let reciprocal = 1.0 / (f32::from(alpha) * scale);
        ((f32::from(channel) * scale * reciprocal).min(1.0)
            * f32::from(u8::MAX))
        .round_ties_even() as u8
    }

    /// A GPU page holding every (channel, alpha) pair, red, green and blue
    /// each a different function of the channel, so that some sit above
    /// their alpha, or `None` with no GPU engine.
    fn every_pair(space: ColorSpace) -> Option<(Surface, Vec<u8>, ImageInfo)> {
        if !RenderingEngine::GPU.selectable() {
            return None;
        }
        let info = ImageInfo::new(
            (256, 256),
            ColorType::RGBA8888,
            AlphaType::Premul,
            Some(space),
        );
        let mut source: Vec<u8> = (0..256 * 256)
            .flat_map(|i| {
                let (alpha, c) = ((i / 256) as u8, (i % 256) as u8);
                [c, u8::MAX - c, c / 2, alpha]
            })
            .collect();
        let mut surface = RenderingEngine::GPU
            .make_surface(
                &info,
                &ExportOptions {
                    msaa: Some(0),
                    ..ExportOptions::default()
                },
            )
            .expect("a GPU page");
        let pixmap =
            Pixmap::new(&info, &mut source, 256 * 4).expect("a pixmap");
        surface.write_pixels_from_pixmap(&pixmap, (0, 0));
        Some((surface, source, info))
    }

    fn read_through_the_pass(
        surface: &mut Surface,
        info: &ImageInfo,
        at: (i32, i32),
    ) -> Vec<u8> {
        let mut target = unpremultiplied(surface, info, at).expect("the pass");
        let mut got = vec![0u8; info.compute_min_byte_size()];
        assert!(target.read_pixels(
            info,
            &mut got,
            info.min_row_bytes(),
            (0, 0)
        ));
        got
    }

    /// Every pair, read back through the pass, is the CPU route's byte, on an
    /// sRGB page and a Display P3 one: the lookup, the identity rows and the
    /// channels above their alpha included.
    #[test]
    fn every_pair_through_the_pass_is_the_tables() {
        let p3 = ColorSpace::new_cicp(
            named_primaries::CicpId::SMPTE_EG_432_1,
            named_transfer_fn::CicpId::IEC61966_2_1,
        )
        .expect("Display P3");
        for space in [ColorSpace::new_srgb(), p3] {
            let Some((mut surface, source, info)) = every_pair(space) else {
                return;
            };
            let got = read_through_the_pass(&mut surface, &info, (0, 0));
            let wrong: Vec<usize> = (0..256 * 256)
                .filter(|&i| {
                    let p = &source[4 * i..4 * i + 4];
                    let w = [
                        want(p[0], p[3]),
                        want(p[1], p[3]),
                        want(p[2], p[3]),
                        p[3],
                    ];
                    got[4 * i..4 * i + 4] != w
                })
                .collect();
            assert!(
                wrong.is_empty(),
                "{} of 65,536 pixels off, first at {:?}",
                wrong.len(),
                wrong.first()
            );
            assert!(got != source, "nothing was unpremultiplied");
        }
    }

    /// A read of part of the page converts that part, not the page's corner.
    #[test]
    fn a_read_at_an_offset_converts_the_area_it_names() {
        let Some((mut surface, source, info)) =
            every_pair(ColorSpace::new_srgb())
        else {
            return;
        };
        let (x, y) = (37, 101);
        let part = info.with_dimensions((50, 20));
        let got = read_through_the_pass(&mut surface, &part, (x, y));
        for row in 0..20 {
            for col in 0..50 {
                let i = (y + row) as usize * 256 + (x + col) as usize;
                let p = &source[4 * i..4 * i + 4];
                let w = [
                    want(p[0], p[3]),
                    want(p[1], p[3]),
                    want(p[2], p[3]),
                    p[3],
                ];
                let j = (row as usize * 50 + col as usize) * 4;
                assert_eq!(
                    &got[j..j + 4],
                    &w,
                    "pixel ({}, {})",
                    x + col,
                    y + row
                );
            }
        }
    }

    /// How `read_converted` reads an unpremultiplied `height` rows of
    /// `surface`: whether from the page itself, the alpha type it asks for,
    /// whether it hands over rows to convert, and where.
    fn route(
        surface: &mut Surface,
        info: &ImageInfo,
        height: i32,
    ) -> (bool, AlphaType, bool, (i32, i32)) {
        let page: *const Surface = &*surface;
        let wanted = info
            .with_dimensions((info.width(), height))
            .with_alpha_type(AlphaType::Unpremul);
        let mut pixels = vec![0u8; wanted.compute_min_byte_size()];
        let mut handed = None;
        assert!(read_converted(
            surface,
            &wanted,
            &mut pixels,
            wanted.min_row_bytes(),
            (0, 0),
            |s, i, p, r, a, rows| {
                let read: *const Surface = &*s;
                handed = Some((
                    std::ptr::eq(read, page),
                    i.alpha_type(),
                    rows.is_none(),
                    a,
                ));
                s.read_pixels(i, p, r, a)
            },
        ));
        handed.expect("a read")
    }

    /// An unpremultiplying read of a GPU page reads the pass's target, as it
    /// stands, rather than the page with rows to convert.
    #[test]
    fn an_unpremultiplying_read_reads_the_pass() {
        let Some((mut surface, _, info)) = every_pair(ColorSpace::new_srgb())
        else {
            return;
        };
        assert_eq!(
            route(&mut surface, &info, info.height()),
            (false, AlphaType::Premul, true, (0, 0)),
            "not the pass's target, read as stored"
        );
    }

    /// A read one row short of [`MIN_PASS_PIXELS`] is read from the page and
    /// converted on the CPU; one reaching it takes the pass.
    #[test]
    fn a_read_below_the_floor_is_converted_on_the_cpu() {
        let Some((mut surface, _, info)) = every_pair(ColorSpace::new_srgb())
        else {
            return;
        };
        let width = info.width() as usize;
        let reaching = MIN_PASS_PIXELS.div_ceil(width);
        assert!(reaching <= info.height() as usize, "the page is too small");
        let reaching = reaching as i32;
        assert_eq!(
            route(&mut surface, &info, reaching - 1),
            (true, AlphaType::Premul, false, (0, 0)),
            "below the floor"
        );
        assert_eq!(
            route(&mut surface, &info, reaching),
            (false, AlphaType::Premul, true, (0, 0)),
            "at the floor"
        );
    }

    /// A whole `getImageData` of a translucent GPU canvas large enough for the
    /// pass, on sRGB and Display P3, gives the bytes its premultiplied read
    /// gives once unpremultiplied by the CPU route's rule.
    #[test]
    fn a_whole_read_is_its_premultiplied_read_unpremultiplied() {
        let (width, height) = (256.0, 256.0);
        assert!((width * height) as usize >= MIN_PASS_PIXELS);
        for space in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
            let mut canvas = Canvas::with_options(
                width,
                height,
                CanvasOptions {
                    color_space: space,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            canvas.set_gpu(true);
            if canvas.engine_kind() != EngineKind::Gpu {
                return;
            }
            let ctx = canvas.context();
            for (i, alpha) in [0.37, 0.5, 0.81, 0.06].into_iter().enumerate() {
                let f = i as f32;
                ctx.set_fill_style(RgbaLinear::from_srgb(
                    0.9 - 0.2 * f,
                    0.3 + 0.1 * f,
                    0.6,
                    alpha,
                ));
                ctx.fill_rect(3.5 + 9.0 * f, 2.25 + 7.0 * f, 30.0, 25.0);
            }
            let plain =
                ctx.get_image_data(0.0, 0.0, width, height).expect("a read");
            let premultiplied = ctx
                .get_image_data_as(
                    0.0,
                    0.0,
                    width,
                    height,
                    PixelExportOptions {
                        color_space: space,
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                )
                .expect("a premultiplied read");
            let want: Vec<u8> = premultiplied
                .pixels()
                .chunks(4)
                .flat_map(|p| {
                    [want(p[0], p[3]), want(p[1], p[3]), want(p[2], p[3]), p[3]]
                })
                .collect();
            assert!(
                plain.pixels() == want.as_slice(),
                "{space:?}: the read differs"
            );
            assert!(
                premultiplied.pixels() != want.as_slice(),
                "{space:?}: nothing translucent"
            );
        }
    }

    /// A raster page has no GPU to draw the pass on, and is left to the CPU.
    #[test]
    fn a_raster_page_is_not_offered_the_pass() {
        let info = ImageInfo::new(
            (4, 4),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let mut raster = RenderingEngine::CPU
            .make_surface(&info, &ExportOptions::default())
            .expect("a raster page");
        assert!(unpremultiplied(&mut raster, &info, (0, 0)).is_none());
    }
}
