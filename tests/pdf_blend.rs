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
    ctx.set_global_composite_operation(operation);
    ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, alpha));
    ctx.fill_rect(50.0, 30.0, 100.0, 60.0);
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
