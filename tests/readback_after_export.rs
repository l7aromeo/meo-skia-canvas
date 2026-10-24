//! A read after an export sees the page once.
//!
//! An export files the page it rendered in the page cache, and a read that
//! composites the page draws from that cache rather than replaying every
//! layer. The cache is the page as it stood, so it stands in for what the
//! read's own surface would otherwise draw -- it does not go on top of a
//! surface that already holds it. A translucent fill shows the difference:
//! drawn twice, half alpha reads as three quarters.
//!
//! The grid serves a read that fits one 256-pixel square, and the grid never
//! consults the cache, so only a page wider than that reaches it. The
//! rows run on both engines at that boundary, and through each export form
//! that files a page.

use meo_skia_canvas::prelude::*;

/// Half-alpha red over a whole `side` by `side` page on `engine`.
fn half_red(side: f32, gpu: bool) -> Result<Canvas, Error> {
    let mut canvas = Canvas::with_options(
        side,
        side,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )?;
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgba(255, 0, 0, 0.5)")?;
    ctx.fill_rect(0.0, 0.0, side, side);
    Ok(canvas)
}

/// The alpha of the first pixel of a whole-page read.
fn read_alpha(canvas: &mut Canvas, side: f32) -> Result<u8, Error> {
    let data = canvas.context().get_image_data(0.0, 0.0, side, side)?;
    Ok(data.pixels()[3])
}

#[test]
fn a_read_after_an_export_sees_the_page_once() -> Result<(), Error> {
    for gpu in [false, true] {
        for side in [256.0, 257.0, 300.0] {
            for format in
                [ImageFormat::Png, ImageFormat::Raw, ImageFormat::Webp]
            {
                let mut canvas = half_red(side, gpu)?;
                canvas.to_buffer(format, &EncodeOptions::default())?;
                let reads = [
                    read_alpha(&mut canvas, side)?,
                    read_alpha(&mut canvas, side)?,
                ];
                // A second export files the page again, which compounded:
                // 128, 192 and then 224.
                canvas.to_buffer(format, &EncodeOptions::default())?;
                let after = read_alpha(&mut canvas, side)?;
                assert_eq!(
                    [reads[0], reads[1], after],
                    [128; 3],
                    "gpu {gpu}, {side} pixels, {format:?}",
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_cache_ahead_of_the_read_surface_replaces_what_it_holds()
-> Result<(), Error> {
    // A read leaves the surface at one layer; a second fill and an export
    // file the page at two. The next read starts from the cache, which
    // already holds the first layer, so the surface's own copy of it goes.
    for gpu in [false, true] {
        let mut canvas = half_red(300.0, gpu)?;
        read_alpha(&mut canvas, 300.0)?;
        let ctx = canvas.context();
        ctx.set_fill_style_css("rgba(255, 0, 0, 0.5)")?;
        ctx.fill_rect(0.0, 0.0, 300.0, 300.0);
        let exported =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?[3];
        // Half over half, composited in eight bits: 128 + 128 * 127 / 255.
        assert_eq!(exported, 192, "gpu {gpu}: the export");
        assert_eq!(read_alpha(&mut canvas, 300.0)?, exported, "gpu {gpu}");
        assert_eq!(
            read_alpha(&mut canvas, 300.0)?,
            exported,
            "gpu {gpu}, again"
        );
    }
    Ok(())
}

#[test]
fn a_read_between_exports_does_not_change_what_they_write() -> Result<(), Error>
{
    // The read surface is what the next export files, so a read that drew
    // the cache twice would hand that on.
    for gpu in [false, true] {
        let mut canvas = half_red(300.0, gpu)?;
        let first =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
        read_alpha(&mut canvas, 300.0)?;
        read_alpha(&mut canvas, 300.0)?;
        let second =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
        assert_eq!(first[3], 128, "gpu {gpu}");
        assert!(first == second, "gpu {gpu}: the export changed after reads");
    }
    Ok(())
}

#[test]
fn a_page_read_at_a_density_places_later_drawing_once_scaled()
-> Result<(), Error> {
    // Each read brings the page surface up to date by replaying only what is
    // new, under the read's density. The scale compounded across those
    // replays, so the second read drew new layers at density four.
    for gpu in [false, true] {
        let mut canvas = Canvas::with_options(
            300.0,
            300.0,
            CanvasOptions {
                gpu,
                ..CanvasOptions::default()
            },
        )?;
        let export = ImageDataExport {
            density: 2,
            ..ImageDataExport::default()
        };
        let ctx = canvas.context();
        ctx.set_fill_style_css("red")?;
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        ctx.get_image_data_with(0.0, 0.0, 300.0, 300.0, &export)?;
        ctx.set_fill_style_css("blue")?;
        ctx.fill_rect(100.0, 100.0, 10.0, 10.0);
        let data = ctx.get_image_data_with(0.0, 0.0, 300.0, 300.0, &export)?;
        let at = |x: usize, y: usize| {
            let i = (y * 600 + x) * 4;
            [data.pixels()[i], data.pixels()[i + 1], data.pixels()[i + 2]]
        };
        assert_eq!(at(205, 205), [0, 0, 255], "gpu {gpu}: where it was drawn");
        assert_eq!(at(405, 405), [0, 0, 0], "gpu {gpu}: twice as far out");
        assert_eq!(at(5, 5), [255, 0, 0], "gpu {gpu}: the first layer");
    }
    Ok(())
}
