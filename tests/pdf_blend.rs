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
/// by `operation`, as a repeating pattern over most of a page in `space`,
/// under `transform` of its own where that is `Some`.
fn patterned(
    space: PixelColorSpace,
    operation: BlendMode,
    transform: Option<Affine>,
) -> Canvas {
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
    let mut pattern =
        ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    if let Some(transform) = transform {
        pattern.set_transform(transform);
    }
    ctx.set_fill_pattern(&pattern);
    ctx.fill_rect(20.0, 20.0, 160.0, 80.0);
    canvas
}

/// Half a pixel along: a pattern moved by it lands between the device's
/// pixels at every density, and is embedded.
const HALF_A_PIXEL: f32 = 0.5;

/// A shear small enough that the scale a tile is rasterized at rounds back
/// to 1 (`SCALE_SLACK` in `context::layer` is 1/1024), which a sheared tile
/// still does not land whole at.
const SLIGHT_SHEAR: f32 = 1.0 / 4096.0;

/// The colour images in `pdf` -- not their soft masks -- each as its width
/// and its pixels, RGB, inflated from the stream Skia's PDF backend writes.
fn colour_images(pdf: &[u8]) -> Vec<(usize, Vec<[u8; 3]>)> {
    inflated_images(pdf)
        .into_iter()
        .filter(|(gray, ..)| !gray)
        .map(|(_, width, raw)| (width, raw.as_chunks::<3>().0.to_vec()))
        .collect()
}

/// The soft masks in `pdf`, each as its width and its alpha bytes.
fn soft_masks(pdf: &[u8]) -> Vec<(usize, Vec<u8>)> {
    inflated_images(pdf)
        .into_iter()
        .filter(|(gray, ..)| *gray)
        .map(|(_, width, raw)| (width, raw))
        .collect()
}

/// Every image in `pdf`, each as whether it is gray -- a soft mask -- its
/// width and its bytes, inflated from the stream Skia's PDF backend writes.
fn inflated_images(pdf: &[u8]) -> Vec<(bool, usize, Vec<u8>)> {
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
        images.push((dict.contains("/DeviceGray"), width, raw));
    }
    images
}

/// Whether `pdf` paints with PDF tiling: a pattern of type 1 (ISO 32000-1,
/// 8.7.3).
fn tiles(pdf: &[u8]) -> bool {
    contains(pdf, b"/PatternType 1")
}

/// A draw dyed by a canvas's pattern whose tiles fall between the device's
/// pixels is embedded in a PDF as the page composites it, at the export's
/// density: one image of the draw, 160 by 80 pixels at a density of 1 and
/// that times the density beyond -- not Skia's PDF backend's raster of a
/// picture shader, made at the unscaled page's size whatever the density.
#[test]
fn a_pdf_pattern_is_embedded_at_the_density() {
    let moved = Some(Affine::translation(HALF_A_PIXEL, 0.0));
    for density in [1.0f32, 2.0, 3.0] {
        let pdf =
            patterned(PixelColorSpace::Srgb, BlendMode::SourceOver, moved)
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
        assert!(!tiles(&pdf), "tiled at a density of {density}");
    }
}

/// A draw dyed by a canvas's pattern whose tiles land on the device's
/// pixels is written as PDF tiling, from one tile rasterized at the export's
/// density -- 40 pixels a side at 1 and that times the density beyond --
/// however far the draw repeats it. At a density that is not whole its
/// steps fall between pixels, and it is embedded.
#[test]
fn an_aligned_pdf_pattern_is_one_tile_at_the_density() {
    for (density, tiled, width) in [
        (1.0f32, true, 40.0),
        (2.0, true, 80.0),
        (3.0, true, 120.0),
        (2.5, false, 400.0),
    ] {
        let pdf = patterned(PixelColorSpace::Srgb, BlendMode::SourceOver, None)
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
            (tiles(&pdf), widths),
            (tiled, vec![width as usize]),
            "at a density of {density}"
        );
    }
}

/// A PDF of a pattern written as tiling holds one tile however far the
/// draw repeats it, where an embedded draw is an image of all of it: a
/// 600 by 400 page filled with a 10-pixel tile, at a density of 4, is a
/// fraction of the same page whose pattern is moved half a pixel and
/// embedded.
#[test]
fn a_tiled_pdf_pattern_is_smaller_than_the_draw_embedded() {
    let size = |transform: Option<Affine>| {
        let mut tile = Canvas::new(10.0, 10.0);
        tile.context()
            .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
        tile.context().fill_rect(0.0, 0.0, 5.0, 5.0);
        let mut canvas = Canvas::new(600.0, 400.0);
        let ctx = canvas.context();
        let mut pattern =
            ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
        if let Some(transform) = transform {
            pattern.set_transform(transform);
        }
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, 600.0, 400.0);
        canvas
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    density: 4.0,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF")
            .len()
    };
    let tiled = size(None);
    let embedded = size(Some(Affine::translation(HALF_A_PIXEL, 0.0)));
    assert!(
        4 * tiled < embedded,
        "{tiled} bytes tiled against {embedded} embedded"
    );
}

/// The content streams in `pdf` -- every Flate stream that is not an image
/// or a colour profile -- inflated.
fn content_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    use std::io::Read;
    let mut streams = Vec::new();
    let mut from = 0;
    while let Some(at) = pdf[from..]
        .windows(7)
        .position(|w| w == b"stream\n")
        .map(|i| i + from)
    {
        from = at + 7;
        if pdf[..at].ends_with(b"end") {
            continue;
        }
        let start = pdf[..at].windows(2).rposition(|w| w == b"<<").expect("<<");
        let dict = String::from_utf8_lossy(&pdf[start..at]).into_owned();
        if !dict.contains("/FlateDecode")
            || dict.contains("/Subtype /Image")
            || dict.contains("/N 3")
        {
            continue;
        }
        let at_length = dict.find("/Length ").expect("a length") + 8;
        let digits: String = dict[at_length..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let length: usize = digits.parse().expect("a length");
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&pdf[from..from + length])
            .read_to_end(&mut raw)
            .expect("a Flate stream");
        streams.push(raw);
    }
    streams
}

/// Text dyed by a canvas's pattern whose tiles land on whole pixels is
/// written as text, filled with the pattern as PDF tiling -- selectable,
/// and drawn by the renderer as it draws text of any other colour -- rather
/// than as an image of its glyphs.
#[test]
fn a_pdf_writes_text_dyed_by_a_tiled_pattern_as_text() {
    FontLibrary::new()
        .use_paths(Some(TEXT_FAMILY), [OSWALD])
        .expect("registers");
    let mut tile = Canvas::new(10.0, 10.0);
    tile.context()
        .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
    tile.context().fill_rect(0.0, 0.0, 5.0, 5.0);
    let mut canvas = backdrop(PixelColorSpace::Srgb);
    let ctx = canvas.context();
    let pattern =
        ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    assert!(ctx.set_font_css(&format!("60px {TEXT_FAMILY}")), "a font");
    ctx.fill_text("Hgw", 10.0, 80.0, None);
    let pdf = pdf_of(&mut canvas);
    let widths: Vec<usize> =
        colour_images(&pdf).iter().map(|(w, _)| *w).collect();
    let shows = content_streams(&pdf)
        .iter()
        .any(|stream| contains(stream, b"Tj") || contains(stream, b"TJ"));
    assert_eq!((tiles(&pdf), widths, shows), (true, vec![10], true));
}

/// A font a test can rely on wherever it runs.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

/// The family [`OSWALD`] is registered under here.
const TEXT_FAMILY: &str = "PdfBlendOswald";

/// Which draws dyed by a canvas's pattern a PDF writes as tiling at a
/// density of 2: those whose 40-pixel tiles land on whole device pixels at
/// every whole density, composited only where they land, through nothing
/// but their alpha and blend mode. Each other row differs from one that
/// tiles in one of those.
#[test]
fn a_pdf_tiles_a_pattern_only_where_each_tile_lands_on_whole_pixels() {
    type Setup = fn(&mut Context2D, &mut Pattern);
    let rows: [(&str, bool, Setup); 20] = [
        ("as drawn", true, |_, _| {}),
        ("scaled by 2", true, |ctx, _| ctx.scale(2.0, 2.0)),
        ("moved 3, 5", true, |ctx, _| ctx.translate(3.0, 5.0)),
        ("at an alpha", true, |ctx, _| ctx.set_global_alpha(0.6)),
        ("multiplied", true, |ctx, _| {
            ctx.set_global_composite_operation(BlendMode::Multiply)
        }),
        ("its own transform moved 40", true, |_, pattern| {
            pattern.set_transform(Affine::translation(40.0, 0.0))
        }),
        ("moved half a pixel", false, |ctx, _| {
            ctx.translate(HALF_A_PIXEL, 0.0)
        }),
        (
            "its own transform moved half a pixel",
            false,
            |_, pattern| {
                pattern.set_transform(Affine::translation(HALF_A_PIXEL, 0.0))
            },
        ),
        ("scaled by 1.5", false, |ctx, _| ctx.scale(1.5, 1.5)),
        ("scaled by 2 and 3", false, |ctx, _| ctx.scale(2.0, 3.0)),
        ("scaled by 3 and 2", false, |ctx, _| ctx.scale(3.0, 2.0)),
        ("sheared too little to change its scale", false, |ctx, _| {
            ctx.set_transform(Affine {
                a: 1.0,
                b: 0.0,
                c: SLIGHT_SHEAR,
                d: 1.0,
                tx: 0.0,
                ty: 0.0,
            })
        }),
        ("rotated", false, |ctx, _| ctx.rotate(0.3)),
        ("mirrored", false, |ctx, _| {
            ctx.translate(200.0, 0.0);
            ctx.scale(-1.0, 1.0);
        }),
        ("turned half round", false, |ctx, _| {
            ctx.translate(200.0, 120.0);
            ctx.scale(-1.0, -1.0);
        }),
        ("sheared", false, |ctx, _| {
            ctx.set_transform(Affine {
                a: 1.0,
                b: 0.0,
                c: 1.0,
                d: 1.0,
                tx: 0.0,
                ty: 0.0,
            })
        }),
        ("copied", false, |ctx, _| {
            ctx.set_global_composite_operation(BlendMode::Copy)
        }),
        ("colour-filtered", false, |ctx, _| {
            ctx.set_color_filter(Some(&ColorFilter::luma()))
        }),
        ("casting a shadow", false, |ctx, _| {
            ctx.set_shadow_color(RgbaLinear::from_srgb8(0, 0, 0, 0.5));
            ctx.set_shadow_offset(4.0, 4.0);
        }),
        ("blurred", false, |ctx, _| {
            ctx.set_filter_css("blur(1px)").expect("a filter")
        }),
    ];
    let mut wrong = Vec::new();
    for (name, tiled, setup) in rows {
        let mut tile = Canvas::new(40.0, 40.0);
        tile.context()
            .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
        tile.context().fill_rect(0.0, 0.0, 30.0, 30.0);
        let mut canvas = backdrop(PixelColorSpace::Srgb);
        let ctx = canvas.context();
        let mut pattern =
            ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
        setup(ctx, &mut pattern);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(20.0, 20.0, 80.0, 40.0);
        let pdf = canvas
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    density: 2.0,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF");
        if tiles(&pdf) != tiled {
            wrong.push(format!("{name}: tiled {}", tiles(&pdf)));
        }
    }
    for (name, repeat, side) in [
        ("repeated across", PatternRepeat::RepeatX, 40.0),
        ("repeated down", PatternRepeat::RepeatY, 40.0),
        ("7.5 pixels a side", PatternRepeat::Repeat, 7.5),
    ] {
        let mut tile = Canvas::new(side, side);
        tile.context().fill_rect(0.0, 0.0, 30.0, 30.0);
        let mut canvas = backdrop(PixelColorSpace::Srgb);
        let ctx = canvas.context();
        let pattern = ctx.create_pattern_from_canvas(&mut tile, repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(20.0, 20.0, 80.0, 40.0);
        if tiles(&pdf_of(&mut canvas)) {
            wrong.push(format!("{name}: tiled"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A pattern in a canvas drawn into another lands where that canvas's
/// transform and the export's density put it, which the draw cannot know
/// when it is made: a 10-pixel tile drawn in at a scale of 1.5 steps 15
/// pixels at a density of 1, 30 at 2 and 45 at 3. A PDF asks again where the
/// pattern is played, and tiles it where the step is whole.
#[test]
fn a_pdf_tiles_a_pattern_in_a_canvas_drawn_in_only_where_it_lands_whole() {
    let mut wrong = Vec::new();
    for (scale, density, tiled) in [
        (1.0f32, 1.0f32, true),
        (2.0, 1.0, true),
        (1.5, 1.0, false),
        (1.5, 2.0, true),
        (1.5, 3.0, false),
    ] {
        let mut tile = Canvas::new(10.0, 10.0);
        tile.context()
            .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
        tile.context().fill_rect(0.0, 0.0, 5.0, 5.0);
        let mut source = Canvas::new(80.0, 40.0);
        let sctx = source.context();
        let pattern =
            sctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
        sctx.set_fill_pattern(&pattern);
        sctx.fill_rect(0.0, 0.0, 80.0, 40.0);
        let mut canvas = backdrop(PixelColorSpace::Srgb);
        let ctx = canvas.context();
        ctx.scale(scale, scale);
        ctx.draw_canvas(&mut source, 10.0, 10.0);
        let pdf = canvas
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    density,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF");
        if tiles(&pdf) != tiled {
            wrong.push(format!(
                "scale {scale} at a density of {density}: tiled {}",
                tiles(&pdf)
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The side, in pixels, of a pattern's canvas too large to rasterize whole
/// at a density of 4: 520 by 520 at 4 is 2,080 pixels a side, past the
/// 2,048 a tile is held to (`TILE_AREA_MAX` in `context::layer`), and fits
/// at 3.
const OVERSIZED_TILE: f32 = 520.0;

/// A pattern whose canvas is too large to rasterize at the density it is
/// drawn at is rasterized at a lower one and resampled, which a renderer
/// drawing it as PDF tiling would do with a filter of its own: such a draw
/// is embedded as the page composites it, as the canvas resamples it, and
/// tiled at a density where it fits.
#[test]
fn a_pdf_embeds_a_pattern_whose_tile_is_too_large_to_rasterize_whole() {
    let mut tile = Canvas::new(OVERSIZED_TILE, OVERSIZED_TILE);
    tile.context()
        .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
    tile.context().fill_rect(0.0, 0.0, 100.0, 100.0);
    let mut canvas = backdrop(PixelColorSpace::Srgb);
    let ctx = canvas.context();
    let pattern =
        ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    ctx.fill_rect(20.0, 20.0, 160.0, 80.0);
    let routes: Vec<(f32, bool)> = [3.0f32, 4.0]
        .into_iter()
        .map(|density| {
            let pdf = canvas
                .to_buffer(
                    ImageFormat::Pdf,
                    &EncodeOptions {
                        density,
                        ..EncodeOptions::default()
                    },
                )
                .expect("a PDF");
            (density, tiles(&pdf))
        })
        .collect();
    assert_eq!(routes, [(3.0, true), (4.0, false)]);
}

/// A tile written as PDF tiling holds the draw's alpha, and the pattern is
/// painted at full alpha: PDFium paints a pattern fill at the alpha of the
/// pattern cell's own graphics state, which Skia writes as 1, and ignored a
/// `globalAlpha` of 0.6. The tile is opaque, so its soft mask is the
/// alpha's byte throughout, and no graphics state in the file sets 0.6.
#[test]
fn a_pdf_tile_holds_the_draw_s_alpha() {
    let alpha = 0.6f32;
    let mut tile = Canvas::new(40.0, 40.0);
    tile.context()
        .set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0));
    tile.context().fill_rect(0.0, 0.0, 40.0, 40.0);
    let mut canvas = backdrop(PixelColorSpace::Srgb);
    let ctx = canvas.context();
    let pattern =
        ctx.create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    ctx.set_global_alpha(alpha);
    ctx.fill_rect(20.0, 20.0, 160.0, 80.0);
    let pdf = pdf_of(&mut canvas);
    assert!(tiles(&pdf), "tiled");
    let byte = (alpha * 255.0).round() as u8;
    let masks: Vec<bool> = soft_masks(&pdf)
        .iter()
        .map(|(_, mask)| mask.iter().all(|a| *a == byte))
        .collect();
    assert_eq!(masks, [true], "one soft mask, the alpha's byte throughout");
    assert!(
        !contains(&pdf, b"/ca .6"),
        "an alpha of 0.6 set on the fill"
    );
}

/// A pattern tile on a Display P3 page is composited in the page's space,
/// as the canvas composites it -- `multiply` of two colours there is not
/// `multiply` of their sRGB bytes -- and written in eight-bit sRGB, as PDF
/// tiling and embedded alike: the tile's pixels, and the embedded draw's,
/// are the canvas's own read as sRGB, to a level, in a tile's base, its
/// overlap and its second square.
#[test]
fn a_pattern_tile_on_a_display_p3_page_blends_in_its_space_in_a_pdf() {
    let moved = Some(Affine::translation(HALF_A_PIXEL, 0.0));
    let mut wrong = Vec::new();
    for (route, transform) in [("tiled", None), ("embedded", moved)] {
        let mut canvas = patterned(
            PixelColorSpace::DisplayP3,
            BlendMode::Multiply,
            transform,
        );
        let pdf = pdf_of(&mut canvas);
        let images = colour_images(&pdf);
        let [(width, pixels)] = images.as_slice() else {
            panic!("{} colour images for one draw {route}", images.len());
        };
        // Page positions. The draw, and so the embedded image, starts at
        // (20, 20); the tile at the page's origin, 40 pixels a side.
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
            let got = match transform {
                None => pixels[(y % 40) * width + x % 40],
                Some(_) => pixels[(y - 20) * width + (x - 20)],
            };
            if (0..3).any(|k| want[k].abs_diff(got[k]) > 1) {
                wrong.push(format!("{route} ({x}, {y}): {got:?} for {want:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Each colour image in `pdf` -- not a soft mask -- as its filter's name and
/// its stream's raw bytes.
fn image_streams(pdf: &[u8]) -> Vec<(String, Vec<u8>)> {
    let text = |range: &[u8]| String::from_utf8_lossy(range).into_owned();
    let field = |dict: &str, name: &str| -> Option<String> {
        let at = dict.find(name)? + name.len();
        Some(dict[at..].split_whitespace().next()?.to_string())
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
        let filter = field(&dict, "/Filter").expect("a filter");
        let length: usize = field(&dict, "/Length")
            .and_then(|l| l.parse().ok())
            .expect("a length");
        let body = end
            + pdf[end..]
                .windows(6)
                .position(|w| w == b"stream")
                .expect("a stream")
            + 6;
        let body = body + usize::from(pdf[body] == b'\r') + 1;
        images.push((filter, pdf[body..body + length].to_vec()));
    }
    images
}

/// The horizontal and vertical sampling factors of each component of the
/// JPEG `bytes`, from its baseline frame header (ITU-T T.81, B.2.2): 1 and
/// 1 for each at full chroma, 2 and 2 for luma at 4:2:0.
fn sampling(bytes: &[u8]) -> Vec<(u8, u8)> {
    let sof = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("a baseline frame header");
    let count = usize::from(bytes[sof + 9]);
    (0..count)
        .map(|c| {
            let factors = bytes[sof + 10 + c * 3 + 1];
            (factors >> 4, factors & 0x0F)
        })
        .collect()
}

/// A page whose blend a PDF cannot write over an opaque backdrop, so that
/// the PDF embeds it, opaque, with what lies beneath it.
fn opaque_run() -> Canvas {
    blended(PixelColorSpace::Srgb, BlendMode::Lighter, 1.0)
}

/// A PDF exported with no quality given writes the pixels it embeds
/// losslessly: one image, Flate-compressed, holding the canvas's own pixels
/// to the byte -- where it wrote them as a JPEG at 0.92 before, 65 levels
/// off along colour edges in PDFium.
#[test]
fn a_pdf_s_embedded_pixels_are_lossless_by_default() {
    let mut canvas = opaque_run();
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a PDF");
    let streams = image_streams(&pdf);
    let filters: Vec<&str> = streams.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(filters, ["/FlateDecode"]);
    let images = colour_images(&pdf);
    let [(width, pixels)] = images.as_slice() else {
        panic!("{} colour images", images.len());
    };
    assert_eq!(*width, 200, "the whole page, embedded");
    let page = canvas
        .context()
        .get_image_data(0.0, 0.0, 200.0, 120.0)
        .expect("the page");
    let want: Vec<[u8; 3]> = page
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    let parted = want.iter().zip(pixels).filter(|(a, b)| a != b).count();
    assert_eq!(parted, 0, "pixels parted from the canvas");
}

/// A quality below 1 asks for JPEG: the run is embedded as one, at full
/// chroma as the JPEG exporter writes, or at 4:2:0 with `jpeg_downsample`.
/// A quality of 1 is lossless.
#[test]
fn a_pdf_at_a_quality_below_1_embeds_a_jpeg_at_the_jpeg_exporter_s_chroma() {
    let pdf = |quality: f32, jpeg_downsample: bool| {
        opaque_run()
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    quality: Some(quality),
                    jpeg_downsample,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF")
    };
    let full = image_streams(&pdf(0.8, false));
    let [(filter, bytes)] = full.as_slice() else {
        panic!("{} colour images", full.len());
    };
    assert_eq!(filter, "/DCTDecode");
    assert_eq!(sampling(bytes), [(1, 1), (1, 1), (1, 1)], "full chroma");
    let halved = image_streams(&pdf(0.8, true));
    assert_eq!(halved[0].0, "/DCTDecode");
    assert_eq!(sampling(&halved[0].1), [(2, 2), (1, 1), (1, 1)], "4:2:0");
    let lossless = image_streams(&pdf(1.0, false));
    assert_eq!(lossless[0].0, "/FlateDecode", "a quality of 1");
}

/// A JPEG file drawn onto a page is written into its PDF as its own bytes,
/// whatever the quality: nothing is decoded and encoded again.
#[test]
fn a_jpeg_drawn_onto_a_page_keeps_its_bytes_in_a_pdf() {
    let file = std::fs::read("tests/assets/images/globe.jpg").expect("a JPEG");
    let image = Image::from_encoded(&file).expect("decoded");
    for quality in [None, Some(0.8)] {
        let mut canvas = backdrop(PixelColorSpace::Srgb);
        canvas
            .context()
            .draw_image_sized(&image, 20.0, 10.0, 100.0, 100.0);
        let pdf = canvas
            .to_buffer(
                ImageFormat::Pdf,
                &EncodeOptions {
                    quality,
                    ..EncodeOptions::default()
                },
            )
            .expect("a PDF");
        assert!(
            contains(&pdf, &file),
            "the file's bytes, at a quality of {quality:?}"
        );
    }
}
