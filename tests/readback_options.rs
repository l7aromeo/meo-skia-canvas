//! Reading pixels back at a density, over a matte, with an MSAA count --
//! what JavaScript's `getImageData` takes after its rectangle and what
//! `toBuffer("raw", ..)` takes for a whole page.
//!
//! Two kinds of evidence. Pixels away from any edge are pinned to what the
//! Node binding reads for the same scene and options. The rest is held to
//! `Canvas::to_buffer(ImageFormat::Raw, ..)` with the same settings, which
//! has taken them all along: same bytes, whatever engine draws them, so an
//! antialiased edge counts without its value being guessed.
//!
//! MSAA is a GPU setting, so on a build without `metal` or `vulkan` every
//! sample count draws alike and its rows agree without testing anything.
//! `just test-rust` builds with the platform's GPU feature, where dropping
//! the count fails `each_setting_reads_what_the_raw_export_reads`.

use meo_skia_canvas::prelude::*;

const SIDE: f32 = 4.0;

/// Red at half alpha over the middle 2x2 of a 4x4 page.
fn half_red_square() -> Canvas {
    let mut canvas = Canvas::new(SIDE, SIDE);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    ctx.set_global_alpha(0.5);
    ctx.fill_rect(1.0, 1.0, 2.0, 2.0);
    canvas
}

/// A diagonal stroke, so antialiasing has edges to show up on.
fn diagonal() -> Canvas {
    let mut canvas = Canvas::new(SIDE, SIDE);
    let ctx = canvas.context();
    ctx.set_stroke_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.move_to(0.0, 0.3);
    ctx.line_to(4.0, 3.1);
    ctx.stroke();
    canvas
}

fn px(data: &ImageData, x: u32, y: u32) -> [u8; 4] {
    let at = (y * data.width() + x) as usize * 4;
    data.pixels()[at..at + 4].try_into().expect("four bytes")
}

/// The raw export of the current page with the same settings.
fn raw(
    canvas: &mut Canvas,
    export: &ImageDataExport,
) -> Result<Vec<u8>, Error> {
    canvas.to_buffer(
        ImageFormat::Raw,
        &EncodeOptions {
            density: export.density as f32,
            matte: export.matte,
            msaa: export.msaa,
            ..EncodeOptions::default()
        },
    )
}

#[test]
fn a_density_reads_more_pixels_of_the_same_region() {
    let export = ImageDataExport {
        density: 2,
        ..ImageDataExport::default()
    };
    let data = half_red_square()
        .context()
        .get_image_data_with(0.0, 0.0, SIDE, SIDE, &export)
        .expect("a readback");
    // The binding's `getImageData(0, 0, 4, 4, {density: 2})`.
    assert_eq!((data.width(), data.height()), (8, 8));
    assert_eq!(
        [
            px(&data, 2, 2),
            px(&data, 5, 5),
            px(&data, 1, 1),
            px(&data, 6, 6)
        ],
        [[255, 0, 0, 128], [255, 0, 0, 128], [0; 4], [0; 4]]
    );
}

#[test]
fn a_matte_is_composited_beneath_the_readback() {
    let export = ImageDataExport {
        matte: Some(RgbaLinear::opaque(1.0, 1.0, 1.0)),
        ..ImageDataExport::default()
    };
    let data = half_red_square()
        .context()
        .get_image_data_with(0.0, 0.0, SIDE, SIDE, &export)
        .expect("a readback");
    // The binding's `getImageData(0, 0, 4, 4, {matte: "white"})`.
    assert_eq!(
        [px(&data, 1, 1), px(&data, 0, 0)],
        [[255, 127, 127, 255], [255; 4]]
    );
}

#[test]
fn each_setting_reads_what_the_raw_export_reads() {
    let settings = [
        ImageDataExport::default(),
        ImageDataExport {
            density: 3,
            ..ImageDataExport::default()
        },
        ImageDataExport {
            matte: Some(RgbaLinear::opaque(0.0, 1.0, 0.0)),
            ..ImageDataExport::default()
        },
        ImageDataExport {
            msaa: Some(0),
            ..ImageDataExport::default()
        },
        ImageDataExport {
            msaa: Some(4),
            density: 2,
            ..ImageDataExport::default()
        },
    ];
    for export in settings {
        let mut canvas = diagonal();
        let want = raw(&mut canvas, &export);
        let got = canvas
            .context()
            .get_image_data_with(0.0, 0.0, SIDE, SIDE, &export)
            .map(ImageData::into_pixels);
        let page = canvas
            .to_image_data(None, &export)
            .map(ImageData::into_pixels);
        // A count the device does not offer is refused by both, or by
        // neither; what it says is the export's business.
        match want {
            Ok(want) => {
                assert_eq!(got.as_ref().ok(), Some(&want), "{export:?}");
                assert_eq!(page.as_ref().ok(), Some(&want), "{export:?}");
            }
            Err(_) => {
                assert!(got.is_err() && page.is_err(), "{export:?}");
            }
        }
    }
}

#[test]
fn a_whole_page_can_be_read_premultiplied() {
    let export = ImageDataExport {
        pixels: PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        ..ImageDataExport::default()
    };
    let page = half_red_square()
        .to_image_data(None, &export)
        .expect("a readback");
    assert!(page.premultiplied());
    assert_eq!(px(&page, 1, 1), [128, 0, 0, 128]);
}

#[test]
fn an_earlier_page_is_read_by_its_index() {
    let mut canvas = Canvas::new(2.0, 2.0);
    canvas
        .context()
        .set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    canvas.context().fill_rect(0.0, 0.0, 2.0, 2.0);
    canvas.new_page_with(3.0, 1.0);
    canvas
        .context()
        .set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
    canvas.context().fill_rect(0.0, 0.0, 3.0, 1.0);

    let read = |canvas: &mut Canvas, page| {
        canvas
            .to_image_data(page, &ImageDataExport::default())
            .map(|data| (data.width(), data.height(), px(&data, 0, 0)))
    };
    assert_eq!(
        read(&mut canvas, Some(0)).ok(),
        Some((2, 2, [255, 0, 0, 255]))
    );
    assert_eq!(
        read(&mut canvas, Some(1)).ok(),
        Some((3, 1, [0, 0, 255, 255]))
    );
    assert_eq!(read(&mut canvas, None).ok(), Some((3, 1, [0, 0, 255, 255])));
    assert!(read(&mut canvas, Some(2)).is_err(), "past the last page");
}

#[test]
fn a_density_of_zero_is_refused() {
    let export = ImageDataExport {
        density: 0,
        ..ImageDataExport::default()
    };
    let mut canvas = half_red_square();
    // Named for what is wrong, rather than the empty region a zero scale
    // would otherwise produce and report as zero dimensions.
    let refused = |result: Result<ImageData, Error>| match result {
        Err(Error::PixelReadback { reason }) => reason.contains("density"),
        _ => false,
    };
    assert!(refused(
        canvas
            .context()
            .get_image_data_with(0.0, 0.0, 4.0, 4.0, &export)
    ));
    assert!(refused(canvas.to_image_data(None, &export)));
}
