//! A PDF writes each composite operation as the blend it is, where PDF has
//! one and Skia's PDF backend writes it right, on a page whose transparency
//! group names the space the blends are computed in; and embeds the rest as
//! pixels with what lies beneath them.

use meo_skia_canvas::prelude::*;

/// Every composite operation the Canvas API names, with the PDF blend mode
/// written for it where PDF has one by that name (ISO 32000-1, tables 136
/// and 137) -- the Porter-Duff operators have none and are written by other
/// means.
const OPERATIONS: [(&str, BlendMode, Option<&str>); 26] = [
    ("source-over", BlendMode::SourceOver, None),
    ("source-in", BlendMode::SourceIn, None),
    ("source-out", BlendMode::SourceOut, None),
    ("source-atop", BlendMode::SourceAtop, None),
    ("destination-over", BlendMode::DestinationOver, None),
    ("destination-in", BlendMode::DestinationIn, None),
    ("destination-out", BlendMode::DestinationOut, None),
    ("destination-atop", BlendMode::DestinationAtop, None),
    ("lighter", BlendMode::Lighter, None),
    ("copy", BlendMode::Copy, None),
    ("xor", BlendMode::Xor, None),
    ("multiply", BlendMode::Multiply, Some("Multiply")),
    ("screen", BlendMode::Screen, Some("Screen")),
    ("overlay", BlendMode::Overlay, Some("Overlay")),
    ("darken", BlendMode::Darken, Some("Darken")),
    ("lighten", BlendMode::Lighten, Some("Lighten")),
    ("color-dodge", BlendMode::ColorDodge, Some("ColorDodge")),
    ("color-burn", BlendMode::ColorBurn, Some("ColorBurn")),
    ("hard-light", BlendMode::HardLight, Some("HardLight")),
    ("soft-light", BlendMode::SoftLight, Some("SoftLight")),
    ("difference", BlendMode::Difference, Some("Difference")),
    ("exclusion", BlendMode::Exclusion, Some("Exclusion")),
    ("hue", BlendMode::Hue, Some("Hue")),
    ("saturation", BlendMode::Saturation, Some("Saturation")),
    ("color", BlendMode::Color, Some("Color")),
    ("luminosity", BlendMode::Luminosity, Some("Luminosity")),
];

/// What each page is given -- see `export::pdf`.
const PAGE_GROUP: &[u8] = b"/Group <</S /Transparency /CS /DeviceRGB>>";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Whether `pdf` embeds an image, which is how it holds what it rasterizes.
fn embeds_an_image(pdf: &[u8]) -> bool {
    contains(pdf, b"/Subtype /Image")
}

/// A rectangle in `rgb(200 40 120)` at `alpha`, composited by `operation`
/// over two opaque colours, on a canvas in `space`.
fn blended(space: PixelColorSpace, operation: BlendMode, alpha: f32) -> Canvas {
    let mut canvas = backdrop(space);
    let ctx = canvas.context();
    ctx.set_global_composite_operation(operation);
    ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, alpha));
    ctx.fill_rect(50.0, 30.0, 100.0, 60.0);
    canvas
}

/// Two opaque colours side by side, on a canvas in `space`.
fn backdrop(space: PixelColorSpace) -> Canvas {
    let mut canvas = Canvas::with_options(
        200.0,
        120.0,
        CanvasOptions {
            color_space: space,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::from_srgb8(230, 180, 40, 1.0));
    ctx.fill_rect(0.0, 0.0, 100.0, 120.0);
    ctx.set_fill_style(RgbaLinear::from_srgb8(40, 90, 200, 1.0));
    ctx.fill_rect(100.0, 0.0, 100.0, 120.0);
    canvas
}

fn pdf_of(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a PDF")
}

/// On an sRGB canvas every operation is written as the blend it is, opaque
/// and translucent, but `lighter`, an addition PDF has no blend mode for,
/// `xor`, a Porter-Duff operator it has no counterpart for, and
/// `destination-atop` from a translucent source, which Skia's PDF backend
/// writes 35 levels off: those three are embedded.
#[test]
fn a_pdf_writes_every_blend_it_can_and_embeds_the_rest() {
    let mut wrong = Vec::new();
    for alpha in [1.0, 0.8] {
        for (name, operation, mode) in OPERATIONS {
            let pdf =
                pdf_of(&mut blended(PixelColorSpace::Srgb, operation, alpha));
            let embedded =
                matches!(operation, BlendMode::Lighter | BlendMode::Xor)
                    || (operation == BlendMode::DestinationAtop && alpha < 1.0);
            if embeds_an_image(&pdf) != embedded {
                wrong
                    .push(format!("{name} at {alpha}: embedded {}", !embedded));
            }
            if let Some(mode) = mode
                && !contains(&pdf, format!("/BM /{mode}").as_bytes())
            {
                wrong.push(format!("{name} at {alpha}: no /BM /{mode}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `clearRect` short of the whole canvas is `Clear` under the hood, which a
/// PDF writes as it is.
#[test]
fn a_pdf_writes_a_clear_rect_as_it_is() {
    let mut canvas = blended(PixelColorSpace::Srgb, BlendMode::SourceOver, 1.0);
    canvas.context().clear_rect(60.5, 30.5, 80.0, 60.0);
    assert!(!embeds_an_image(&pdf_of(&mut canvas)));
}

/// A canvas in any colour space but sRGB blends in that space, and a PDF in
/// its page group's DeviceRGB, so every blend on it is embedded, as the
/// canvas composited it.
#[test]
fn a_pdf_of_a_canvas_outside_srgb_embeds_every_blend() {
    for space in [PixelColorSpace::SrgbLinear, PixelColorSpace::DisplayP3] {
        let pdf = pdf_of(&mut blended(space, BlendMode::Multiply, 1.0));
        assert!(embeds_an_image(&pdf), "{space:?}");
        let plain = pdf_of(&mut blended(space, BlendMode::SourceOver, 1.0));
        assert!(!embeds_an_image(&plain), "{space:?}: nothing to embed");
    }
}

/// The composite operations Skia's PDF backend writes through a soft mask
/// made from the draw's shape -- `needShape` in its `SkPDFDevice.cpp` --
/// with `clearRect`, which is `Clear`, beside them.
const MASKED: [(&str, Option<BlendMode>); 8] = [
    ("source-in", Some(BlendMode::SourceIn)),
    ("source-out", Some(BlendMode::SourceOut)),
    ("source-atop", Some(BlendMode::SourceAtop)),
    ("destination-in", Some(BlendMode::DestinationIn)),
    ("destination-out", Some(BlendMode::DestinationOut)),
    ("destination-atop", Some(BlendMode::DestinationAtop)),
    ("copy", Some(BlendMode::Copy)),
    ("clearRect", None),
];

/// Where a masked draw lies: inside a canvas drawn in whole, inside one
/// drawn in cropped -- a group of its own -- inside a `saveLayer`, inside
/// one still open when the page is exported, or on the page itself.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    Drawn,
    Cropped,
    Layer,
    Open,
    Page,
}

/// Over two opaque colours, a square in `rgb(200 40 120)` with a smaller
/// one composited onto it by `operation` -- or cleared, for `None` -- placed
/// as `placement` says.
fn masked(operation: Option<BlendMode>, placement: Placement) -> Canvas {
    let mut canvas = backdrop(PixelColorSpace::Srgb);
    let draw = |ctx: &mut Context2D| {
        ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
        ctx.fill_rect(0.0, 0.0, 80.0, 80.0);
        match operation {
            Some(operation) => {
                ctx.set_global_composite_operation(operation);
                ctx.set_fill_style(RgbaLinear::from_srgb8(20, 160, 90, 1.0));
                ctx.fill_rect(20.0, 20.0, 40.0, 40.0);
            }
            None => ctx.clear_rect(20.0, 20.0, 40.0, 40.0),
        }
    };
    let mut logo = Canvas::with_options(
        80.0,
        80.0,
        CanvasOptions {
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    draw(logo.context());
    let ctx = canvas.context();
    match placement {
        Placement::Drawn => ctx.draw_canvas(&mut logo, 60.0, 20.0),
        Placement::Cropped => ctx.draw_canvas_region(
            &mut logo, 10.0, 10.0, 60.0, 60.0, 70.0, 30.0, 60.0, 60.0,
        ),
        Placement::Layer => {
            ctx.save();
            ctx.translate(60.0, 20.0);
            ctx.save_layer();
            draw(ctx);
            ctx.restore();
            ctx.restore();
        }
        Placement::Open => {
            ctx.translate(60.0, 20.0);
            ctx.save_layer();
            draw(ctx);
        }
        Placement::Page => {
            ctx.translate(60.0, 20.0);
            draw(ctx);
        }
    }
    canvas
}

/// A blend written through a soft mask inside a group -- a canvas drawn in
/// through a layer, or a `saveLayer` -- is embedded as pixels in a PDF
/// exported at a density other than 1, which CoreGraphics renders otherwise
/// under the page's scale: see docs/browser-differences.md. At a density of
/// 1, and on the page itself at any density, it stays vector, as a blend
/// written without a mask does inside a group at any density.
#[test]
fn a_masked_blend_in_a_group_is_embedded_under_a_density() {
    let placements = [
        Placement::Drawn,
        Placement::Cropped,
        Placement::Layer,
        Placement::Open,
        Placement::Page,
    ];
    let mut wrong = Vec::new();
    for density in [1.0, 2.5] {
        let options = EncodeOptions {
            density,
            ..EncodeOptions::default()
        };
        let pdf = |canvas: &mut Canvas| {
            canvas.to_buffer(ImageFormat::Pdf, &options).expect("a PDF")
        };
        for placement in placements {
            for (name, operation) in MASKED {
                let embedded =
                    embeds_an_image(&pdf(&mut masked(operation, placement)));
                let want = density != 1.0 && placement != Placement::Page;
                if embedded != want {
                    wrong.push(format!(
                        "{name} {placement:?} at {density}: embedded {embedded}"
                    ));
                }
            }
            let multiply = Some(BlendMode::Multiply);
            if embeds_an_image(&pdf(&mut masked(multiply, placement))) {
                wrong.push(format!(
                    "multiply {placement:?} at {density}: embedded"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Over two opaque colours, a `saveLayer` composited by `operation`
/// holding two overlapping rectangles drawn `source-over`, on a canvas in
/// `space`; left open at the export where `open`.
fn layer_composited(
    space: PixelColorSpace,
    operation: BlendMode,
    open: bool,
) -> Canvas {
    let mut canvas = backdrop(space);
    let ctx = canvas.context();
    ctx.set_global_composite_operation(operation);
    ctx.save_layer();
    ctx.set_global_composite_operation(BlendMode::SourceOver);
    ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
    ctx.fill_rect(50.0, 30.0, 60.0, 60.0);
    ctx.fill_rect(90.0, 40.0, 60.0, 40.0);
    if !open {
        ctx.restore();
    }
    canvas
}

/// A `saveLayer` composited by an operation a PDF cannot write is embedded
/// with what lies beneath it, as a draw composited by it is: `lighter`,
/// `xor`, and `destination-atop`, since what a layer holds may be
/// translucent -- and on a canvas outside sRGB, every blend. The rest are
/// written as the blends they are. Closed, and still open at the export.
///
/// `source-out` over the opaque page leaves nothing anywhere, so outside
/// sRGB the raster it is embedded as holds no ink and nothing is written.
#[test]
fn a_layer_composited_by_a_blend_a_pdf_cannot_write_is_embedded() {
    let mut wrong = Vec::new();
    for space in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
        for (name, operation, _) in OPERATIONS {
            for open in [false, true] {
                let pdf = pdf_of(&mut layer_composited(space, operation, open));
                let embedded = match space {
                    PixelColorSpace::Srgb => matches!(
                        operation,
                        BlendMode::Lighter
                            | BlendMode::Xor
                            | BlendMode::DestinationAtop
                    ),
                    _ => !matches!(
                        operation,
                        BlendMode::SourceOver | BlendMode::SourceOut
                    ),
                };
                if embeds_an_image(&pdf) != embedded {
                    wrong.push(format!(
                        "{name} on {space:?}, open {open}: embedded {}",
                        !embedded
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A `saveLayer` composited by a blend written through a soft mask is a
/// masked blend at the top of the page, kept vector at any density; inside
/// another `saveLayer` it is one inside a group, embedded at a density
/// other than 1 -- see `a_masked_blend_in_a_group_is_embedded_under_a_density`.
/// Closed, and both left open at the export.
#[test]
fn a_layer_composited_by_a_masked_blend_inside_another_is_nested() {
    let mut wrong = Vec::new();
    for density in [1.0, 2.5] {
        let options = EncodeOptions {
            density,
            ..EncodeOptions::default()
        };
        for (inside, open) in [(false, false), (true, false), (true, true)] {
            let mut canvas = backdrop(PixelColorSpace::Srgb);
            let ctx = canvas.context();
            if inside {
                ctx.save_layer();
            }
            ctx.set_global_composite_operation(BlendMode::DestinationOut);
            ctx.save_layer();
            ctx.set_global_composite_operation(BlendMode::SourceOver);
            ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
            ctx.fill_rect(50.0, 30.0, 60.0, 60.0);
            if !open {
                ctx.restore();
                if inside {
                    ctx.restore();
                }
            }
            let pdf =
                canvas.to_buffer(ImageFormat::Pdf, &options).expect("a PDF");
            let want = inside && density != 1.0;
            if embeds_an_image(&pdf) != want {
                wrong.push(format!(
                    "inside {inside}, open {open} at {density}: embedded {}",
                    !want
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every page of a PDF has a transparency group naming DeviceRGB, through
/// `to_buffer` and through a file, once.
#[test]
fn every_page_has_a_transparency_group() {
    let mut canvas = blended(PixelColorSpace::Srgb, BlendMode::Multiply, 1.0);
    for _ in 0..2 {
        let ctx = canvas.new_page();
        ctx.set_global_composite_operation(BlendMode::Screen);
        ctx.fill_rect(10.0, 10.0, 50.0, 50.0);
    }
    let buffer = pdf_of(&mut canvas);
    assert_eq!(count(&buffer, b"/Type /Page\n"), 3);
    assert_eq!(count(&buffer, PAGE_GROUP), 3, "one group a page");
    let dir =
        std::env::temp_dir().join(format!("pdf-blend-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let path = dir.join("pages.pdf");
    canvas
        .to_file(&path, &EncodeOptions::default())
        .expect("a file");
    let file = std::fs::read(&path).expect("the file");
    std::fs::remove_dir_all(&dir).expect("cleaned up");
    assert_eq!(count(&file, PAGE_GROUP), 3, "through a file as well");
}

/// The scale both the canvas and the PDF are rendered at for comparison:
/// 288 dots per inch, four times the PDF's 72.
const SCALE: f32 = 4.0;

/// How far poppler's rendering of a blend the PDF writes may land from the
/// canvas's own, inside what the source covers whole: within a level of
/// rounding either way, as measured for every such mode, opaque and at 0.8.
const RENDERED: u8 = 2;

/// `png`, drawn over white at `SCALE` times the 200 by 120 page, as RGB.
fn over_white(png: &[u8]) -> Vec<[u8; 3]> {
    let image = Image::from_encoded(png).expect("a decodable render");
    let (w, h) = (200.0 * SCALE, 120.0 * SCALE);
    let mut canvas = Canvas::new(w, h);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 1.0));
    ctx.fill_rect(0.0, 0.0, w, h);
    ctx.draw_image_sized(&image, 0.0, 0.0, w, h);
    let data = ctx.get_image_data(0.0, 0.0, w, h).expect("pixels");
    data.pixels()
        .chunks(4)
        .map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// `pdf` as poppler renders it at `SCALE`, or `None` where `pdftoppm` is not
/// installed.
fn poppler(pdf: &[u8], name: &str) -> Option<Vec<[u8; 3]>> {
    let dir = std::env::temp_dir()
        .join(format!("pdf-blend-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let path = dir.join(format!("{name}.pdf"));
    std::fs::write(&path, pdf).expect("the PDF written");
    let out = dir.join(name);
    let dpi = (72.0 * SCALE).to_string();
    let ran = std::process::Command::new("pdftoppm")
        .args(["-r", &dpi, "-png", "-singlefile"])
        .arg(&path)
        .arg(&out)
        .status();
    let rendered = match ran {
        Ok(status) => {
            assert!(status.success(), "pdftoppm failed on {name}");
            let png =
                std::fs::read(out.with_extension("png")).expect("the render");
            Some(over_white(&png))
        }
        Err(_) => None,
    };
    std::fs::remove_dir_all(&dir).expect("cleaned up");
    rendered
}

/// Whether the pixel at `i` of a render at `SCALE` lies inside the source
/// rectangle, at least a pixel and a half of the page from its edges and
/// from the seam between the two backdrop colours, where renderers' own
/// antialiasing differs.
fn inside(i: usize) -> bool {
    let w = (200.0 * SCALE) as usize;
    let (x, y) = ((i % w) as f32 / SCALE, (i / w) as f32 / SCALE);
    let band = 1.5;
    (50.0 + band..150.0 - band).contains(&x)
        && (30.0 + band..90.0 - band).contains(&y)
        && (x - 100.0).abs() >= band
}

/// Every blend a PDF writes, opaque and at 0.8, renders in poppler as the
/// canvas draws it, to within [`RENDERED`] levels wherever the source covers
/// a pixel whole; and the ring a canvas drawn in punches out with
/// `destination-out` within 3 everywhere. Runs wherever `pdftoppm` is
/// installed, and fails rather than skips on CI, which installs it; a
/// developer machine without it is told how to get it.
#[test]
fn every_blend_a_pdf_writes_renders_as_the_canvas_in_poppler() {
    let Some(_) = poppler(
        &pdf_of(&mut blended(
            PixelColorSpace::Srgb,
            BlendMode::SourceOver,
            1.0,
        )),
        "probe",
    ) else {
        assert!(
            std::env::var_os("CI").is_none(),
            "pdftoppm is missing on CI: the workflow installs poppler-utils"
        );
        eprintln!(
            "skipped: pdftoppm not found -- `brew install poppler` or `apt install poppler-utils`"
        );
        return;
    };
    let density = EncodeOptions {
        density: SCALE,
        ..EncodeOptions::default()
    };
    let mut wrong = Vec::new();
    for alpha in [1.0, 0.8] {
        for (name, operation, _) in OPERATIONS {
            let embedded =
                matches!(operation, BlendMode::Lighter | BlendMode::Xor)
                    || (operation == BlendMode::DestinationAtop && alpha < 1.0);
            if embedded {
                continue;
            }
            let mut canvas = blended(PixelColorSpace::Srgb, operation, alpha);
            let png =
                canvas.to_buffer(ImageFormat::Png, &density).expect("a PNG");
            let want = over_white(&png);
            let got = poppler(&pdf_of(&mut canvas), name).expect("pdftoppm");
            let off = want
                .iter()
                .zip(&got)
                .enumerate()
                .filter(|(i, _)| inside(*i))
                .map(|(_, (a, b))| {
                    (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            if off > RENDERED {
                wrong.push(format!("{name} at {alpha}: {off} levels"));
            }
        }
    }
    let mut logo = Canvas::new(80.0, 80.0);
    {
        let ctx = logo.context();
        ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
        ctx.fill_rect(0.0, 0.0, 80.0, 80.0);
        ctx.set_global_composite_operation(BlendMode::DestinationOut);
        ctx.fill_rect(20.0, 20.0, 40.0, 40.0);
    }
    let mut page = blended(PixelColorSpace::Srgb, BlendMode::SourceOver, 0.0);
    page.context().draw_canvas(&mut logo, 60.0, 20.0);
    let want =
        over_white(&page.to_buffer(ImageFormat::Png, &density).expect("a PNG"));
    let got = poppler(&pdf_of(&mut page), "logo").expect("pdftoppm");
    let off = want
        .iter()
        .zip(&got)
        .map(|(a, b)| (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0))
        .max()
        .unwrap_or(0);
    if off > 3 {
        wrong.push(format!("the punched-out ring: {off} levels"));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A 40-pixel tile holding a square with a smaller one composited onto it
/// by `operation`, as a repeating pattern over most of a page in `space`.
fn patterned(space: PixelColorSpace, operation: BlendMode) -> Canvas {
    let mut tile = Canvas::with_options(
        40.0,
        40.0,
        CanvasOptions {
            color_space: space,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let t = tile.context();
    t.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
    t.fill_rect(0.0, 0.0, 30.0, 30.0);
    t.set_global_composite_operation(operation);
    t.set_fill_style(RgbaLinear::from_srgb8(20, 160, 90, 1.0));
    t.fill_rect(10.0, 10.0, 25.0, 25.0);
    let mut canvas = backdrop(space);
    let ctx = canvas.context();
    let pattern =
        ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    ctx.fill_rect(20.0, 20.0, 160.0, 80.0);
    canvas
}

/// The colour images in `pdf` -- not their soft masks -- each as its width
/// and its pixels, RGB, inflated from the stream Skia's PDF backend writes.
fn colour_images(pdf: &[u8]) -> Vec<(usize, Vec<[u8; 3]>)> {
    use std::io::Read;
    let text = |range: &[u8]| String::from_utf8_lossy(range).into_owned();
    let field = |dict: &str, name: &str| -> Option<usize> {
        let at = dict.find(name)? + name.len();
        dict[at..].split_whitespace().next()?.parse().ok()
    };
    let mut images = Vec::new();
    let mut from = 0;
    while let Some(at) = pdf[from..]
        .windows(15)
        .position(|w| w == b"/Subtype /Image")
        .map(|i| i + from)
    {
        from = at + 15;
        let start = pdf[..at].windows(2).rposition(|w| w == b"<<").expect("<<");
        let end =
            at + pdf[at..].windows(2).position(|w| w == b">>").expect(">>");
        let dict = text(&pdf[start..end]);
        if dict.contains("/DeviceGray") {
            continue;
        }
        let (width, length) = (
            field(&dict, "/Width").expect("a width"),
            field(&dict, "/Length").expect("a length"),
        );
        let body = end
            + pdf[end..]
                .windows(6)
                .position(|w| w == b"stream")
                .expect("a stream")
            + 6;
        let body = body + usize::from(pdf[body] == b'\r') + 1;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&pdf[body..body + length])
            .read_to_end(&mut raw)
            .expect("a Flate stream");
        images.push((width, raw.as_chunks::<3>().0.to_vec()));
    }
    images
}

/// A draw dyed by a canvas's pattern is embedded in a PDF as the page
/// composites it, at the export's density: one image of the draw, 160 by 80
/// pixels at a density of 1 and that times the density beyond -- not
/// Skia's PDF backend's raster of a picture shader, made at the unscaled
/// page's size whatever the density.
#[test]
fn a_pdf_pattern_is_embedded_at_the_density() {
    for density in [1.0f32, 2.0, 3.0] {
        let pdf = patterned(PixelColorSpace::Srgb, BlendMode::SourceOver)
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    density,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF");
        let widths: Vec<usize> =
            colour_images(&pdf).iter().map(|(w, _)| *w).collect();
        assert_eq!(
            widths,
            [(160.0 * density) as usize],
            "at a density of {density}"
        );
    }
}

/// A pattern tile on a Display P3 page is composited in the page's space,
/// as the canvas composites it -- `multiply` of two colours there is not
/// `multiply` of their sRGB bytes -- and embedded in eight-bit sRGB: the
/// embedded draw's pixels are the canvas's own read as sRGB, to a level,
/// in a tile's base, its overlap and its second square.
#[test]
fn a_pattern_tile_on_a_display_p3_page_blends_in_its_space_in_a_pdf() {
    let mut canvas = patterned(PixelColorSpace::DisplayP3, BlendMode::Multiply);
    let pdf = pdf_of(&mut canvas);
    let images = colour_images(&pdf);
    let [(width, pixels)] = images.as_slice() else {
        panic!("{} colour images for one draw", images.len());
    };
    let mut wrong = Vec::new();
    // Page positions; the draw, and so the image, starts at (20, 20).
    for (x, y) in [(45usize, 45usize), (25, 25), (72, 72)] {
        let data = canvas
            .context()
            .get_image_data_as(
                x as f32,
                y as f32,
                1.0,
                1.0,
                PixelExportOptions {
                    color_space: PixelColorSpace::Srgb,
                    ..PixelExportOptions::default()
                },
            )
            .expect("a pixel");
        let want = &data.pixels()[..3];
        let got = pixels[(y - 20) * width + (x - 20)];
        if (0..3).any(|k| want[k].abs_diff(got[k]) > 1) {
            wrong.push(format!("({x}, {y}): {got:?} for {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
