//! Text set in a font with a `COLR` table, of version 0 or 1, drawn in its
//! own colours -- and on macOS, where this crate draws such a glyph from its
//! paint graph rather than through CoreText, drawn as Chrome draws it. The
//! references are Chrome 158's, described in `tests/assets/images/colr`.

use meo_skia_canvas::prelude::*;

const COLR_V1: &str =
    "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";
const COLR_V0: &str = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";
const REFERENCES: &str = "tests/assets/images/colr";

/// How far a channel may stand from Chrome's where neither image changes
/// within a pixel of it. Measured at 1 over 764,234 such pixels of the 221
/// test glyphs: a gradient's rounding.
#[cfg(target_os = "macos")]
const INTERIOR_TOLERANCE: u8 = 1;

/// How far an edge pixel may stand from Chrome's before it is counted: a
/// circle and a triangle filled on a 2D canvas, no font involved, come out of
/// Chrome 158 and this crate up to 81 apart at their edges.
#[cfg(target_os = "macos")]
const EDGE_ENVELOPE: u8 = 81;

/// The reach of a colour's spread across its channels past which a pixel
/// counts as coloured rather than grey.
const COLOURED_SPREAD: u8 = 25;

fn register() {
    let library = FontLibrary::new();
    library
        .use_paths(Some("ColrGlyphs"), [COLR_V1])
        .expect("the COLRv1 font registers");
    library
        .use_paths(Some("ColrV0"), [COLR_V0])
        .expect("the COLRv0 font registers");
}

fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels")
        .pixels()
        .to_vec()
}

fn coloured(pixels: &[u8]) -> usize {
    pixels
        .chunks(4)
        .filter(|p| {
            let spread = p[0].abs_diff(p[1]).max(p[1].abs_diff(p[2]));
            p[3] > 0 && spread > COLOURED_SPREAD
        })
        .count()
}

#[test]
fn a_colr_version_0_font_inks_in_its_palettes_colours() {
    register();
    let mut canvas = Canvas::new(160.0, 80.0);
    let ctx = canvas.context();
    ctx.set_fill_style_css("#fff").expect("a colour");
    ctx.fill_rect(0.0, 0.0, 160.0, 80.0);
    assert!(ctx.set_font_css("48px ColrV0"));
    ctx.set_fill_style_css("#000").expect("a colour");
    ctx.fill_text("A", 20.0, 60.0, None);
    // Chrome 158 inks 1,929 pixels of this, every one of them coloured.
    let inked = coloured(&pixels(&mut canvas));
    assert!(
        inked > 1500,
        "the COLRv0 glyph inks {inked} coloured pixels"
    );
}

#[cfg(target_os = "macos")]
mod against_chrome {
    use super::*;

    /// A reference's layout, from the `.json` beside its image.
    fn layout(name: &str) -> serde_json::Value {
        let text = std::fs::read_to_string(format!("{REFERENCES}/{name}.json"))
            .expect("the layout");
        serde_json::from_str(&text).expect("JSON")
    }

    fn number(spec: &serde_json::Value, key: &str) -> f32 {
        spec[key].as_f64().expect(key) as f32
    }

    /// A reference's pixels, read as RGBA.
    fn reference(spec: &serde_json::Value) -> Vec<u8> {
        let file = spec["reference"].as_str().expect("a reference");
        let bytes = std::fs::read(format!("{REFERENCES}/{file}")).expect("PNG");
        let image = Image::from_encoded(&bytes).expect("decodes");
        let mut canvas =
            Canvas::new(number(spec, "width"), number(spec, "height"));
        canvas.context().draw_image(&image, 0.0, 0.0);
        pixels(&mut canvas)
    }

    /// The grid of test glyphs as `spec` lays it out, drawn here, moved
    /// across by `shift` pixels.
    fn glyph_sheet(spec: &serde_json::Value, shift: f32) -> Vec<u8> {
        let (width, height) = (number(spec, "width"), number(spec, "height"));
        let (columns, cell) =
            (number(spec, "columns") as usize, number(spec, "cell"));
        let mut canvas = Canvas::new(width, height);
        let ctx = canvas.context();
        backed(ctx, spec);
        assert!(
            ctx.set_font_css(&format!("{}px ColrGlyphs", number(spec, "size")))
        );
        if let Some(settings) = spec["fontVariationSettings"].as_str() {
            let variations: Vec<FontVariation> = settings
                .split(',')
                .map(|setting| {
                    let (tag, value) = setting
                        .trim()
                        .rsplit_once(' ')
                        .expect("a tag and a value");
                    let tag = tag.trim_matches('"');
                    FontVariation::new(
                        tag.parse().expect("an axis tag"),
                        value.parse().expect("a number"),
                    )
                })
                .collect();
            ctx.set_font_variation_settings(&variations);
        }
        ctx.set_fill_style_css(spec["fillStyle"].as_str().expect("a fill"))
            .expect("a colour");
        let codepoints = spec["codepoints"].as_array().expect("code points");
        for (i, hex) in codepoints.iter().enumerate() {
            let cp = u32::from_str_radix(hex.as_str().expect("hex"), 16)
                .expect("hex");
            let text = char::from_u32(cp).expect("a scalar").to_string();
            let x = (i % columns) as f32 * cell + number(spec, "x") + shift;
            let y = (i / columns) as f32 * cell + number(spec, "baseline");
            ctx.fill_text(&text, x, y, None);
        }
        pixels(&mut canvas)
    }

    /// The page `spec` draws on, painted in its background.
    fn backed(ctx: &mut Context2D, spec: &serde_json::Value) {
        let background = spec["background"].as_str().expect("a background");
        ctx.set_fill_style_css(background).expect("a colour");
        ctx.fill_rect(0.0, 0.0, number(spec, "width"), number(spec, "height"));
    }

    /// The COLR version 0 cases as `spec` lays them out, each clipped to
    /// its cell.
    fn v0_sheet(spec: &serde_json::Value) -> Vec<u8> {
        let mut canvas =
            Canvas::new(number(spec, "width"), number(spec, "height"));
        let ctx = canvas.context();
        backed(ctx, spec);
        let columns = number(spec, "columns") as usize;
        let (cell_width, cell_height) =
            (number(spec, "cellWidth"), number(spec, "cellHeight"));
        let cases = spec["cases"].as_array().expect("cases");
        for (i, case) in cases.iter().enumerate() {
            let size = case["size"].as_f64().expect("a size") as f32;
            ctx.save();
            ctx.translate(
                (i % columns) as f32 * cell_width,
                (i / columns) as f32 * cell_height,
            );
            ctx.begin_path();
            ctx.rect(0.0, 0.0, cell_width, cell_height);
            ctx.clip(FillRule::NonZero);
            assert!(ctx.set_font_css(&format!("{size}px ColrV0")));
            ctx.set_fill_style_css(spec["fillStyle"].as_str().expect("a fill"))
                .expect("a colour");
            ctx.fill_text(
                case["text"].as_str().expect("text"),
                number(spec, "x"),
                number(spec, "baselineAbove") + size,
                None,
            );
            ctx.restore();
        }
        pixels(&mut canvas)
    }

    /// The largest difference among interior pixels -- whose 3x3
    /// neighbourhood is flat in both images to within 2 levels -- and how
    /// many pixels stand past [`EDGE_ENVELOPE`], leaving out what `skip`
    /// names.
    fn compare(
        ours: &[u8],
        chrome: &[u8],
        width: usize,
        height: usize,
        skip: impl Fn(usize, usize) -> bool,
    ) -> (u8, usize) {
        let flat = |px: &[u8], x: usize, y: usize| {
            let at = (y * width + x) * 4;
            (y - 1..=y + 1).all(|yy| {
                (x - 1..=x + 1).all(|xx| {
                    let k = (yy * width + xx) * 4;
                    (0..3).all(|c| px[k + c].abs_diff(px[at + c]) <= 2)
                })
            })
        };
        let (mut interior, mut past) = (0, 0);
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                if skip(x, y) {
                    continue;
                }
                let k = (y * width + x) * 4;
                let d = (0..3)
                    .map(|c| ours[k + c].abs_diff(chrome[k + c]))
                    .max()
                    .unwrap_or(0);
                if d > EDGE_ENVELOPE {
                    past += 1;
                }
                if flat(ours, x, y) && flat(chrome, x, y) {
                    interior = interior.max(d);
                }
            }
        }
        (interior, past)
    }

    /// The cell the sheet puts U+FE000 in, which the font maps to
    /// `.notdef` and Chrome draws as nothing: the missing glyph, not a colour
    /// one.
    fn notdef(spec: &serde_json::Value) -> impl Fn(usize, usize) -> bool {
        let codepoints = spec["codepoints"].as_array().expect("code points");
        let index = codepoints.iter().position(|cp| cp == "fe000");
        let (columns, cell) = (
            number(spec, "columns") as usize,
            number(spec, "cell") as usize,
        );
        move |x, y| Some((y / cell) * columns + x / cell) == index
    }

    fn checked(name: &str, shift: f32) -> (u8, usize) {
        register();
        let spec = layout(name);
        let (width, height) = (
            number(&spec, "width") as usize,
            number(&spec, "height") as usize,
        );
        compare(
            &glyph_sheet(&spec, shift),
            &reference(&spec),
            width,
            height,
            notdef(&spec),
        )
    }

    #[test]
    fn colrv1_glyphs_draw_as_chrome_draws_them() {
        let (interior, past) = checked("chrome-test-glyphs-48px", 0.0);
        assert!(interior <= INTERIOR_TOLERANCE, "interior off by {interior}");
        // 33 measured: pixels on the hard stops and wraps of repeated
        // gradients, and edges -- see docs/browser-differences.md.
        assert!(past <= 40, "{past} edge pixels past the envelope");
    }

    #[test]
    fn colrv1_glyphs_with_their_axes_varied_draw_as_chrome_draws_them() {
        let (interior, past) = checked("chrome-test-glyphs-48px-varied", 0.0);
        assert!(interior <= INTERIOR_TOLERANCE, "interior off by {interior}");
        // 53 measured.
        assert!(past <= 64, "{past} edge pixels past the envelope");
    }

    #[test]
    fn colr_version_0_glyphs_draw_as_chrome_draws_them() {
        register();
        let spec = layout("chrome-colr-v0");
        let (width, height) = (
            number(&spec, "width") as usize,
            number(&spec, "height") as usize,
        );
        let (interior, past) = compare(
            &v0_sheet(&spec),
            &reference(&spec),
            width,
            height,
            |_, _| false,
        );
        assert!(interior <= INTERIOR_TOLERANCE, "interior off by {interior}");
        assert_eq!(past, 0, "{past} edge pixels past the envelope");
    }

    #[test]
    fn the_comparison_refuses_glyphs_drawn_a_pixel_off() {
        // The control for the comparisons above: a sheet moved a pixel
        // across has every glyph's edges past the envelope.
        let (_, past) = checked("chrome-test-glyphs-48px", 1.0);
        assert!(past > 40, "only {past} edge pixels past the envelope");
    }
}
