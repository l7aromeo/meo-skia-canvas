//! Text set in a font with a `COLR` table, of version 0 or 1, drawn in its
//! own colours -- and on macOS, where this crate draws such a glyph from its
//! paint graph rather than through CoreText, drawn as Chrome draws it. The
//! references are Chrome 158's, described in `tests/assets/images/colr`.

use meo_skia_canvas::prelude::*;

const COLR_V1: &str =
    "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";
const COLR_V0: &str = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";
const REFERENCES: &str = "tests/assets/images/colr";

/// How far a channel of a colour glyph may stand from Chrome's at any pixel,
/// edges included. Measured at 3, at one pixel of the test glyphs and with
/// their axes varied alike, and past 1 at ten: both draw the same paint graph
/// through Skia's CPU raster.
#[cfg(target_os = "macos")]
const COLOUR_GLYPH_TOLERANCE: u8 = 3;

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
        let mut canvas = cpu_canvas(spec);
        canvas.context().draw_image(&image, 0.0, 0.0);
        pixels(&mut canvas)
    }

    /// The grid of test glyphs as `spec` lays it out, drawn here, moved
    /// across by `shift` pixels.
    fn glyph_sheet(spec: &serde_json::Value, shift: f32) -> Vec<u8> {
        let (columns, cell) =
            (number(spec, "columns") as usize, number(spec, "cell"));
        let mut canvas = cpu_canvas(spec);
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

    /// A canvas of `spec`'s size drawing on the CPU, as Chrome drew the
    /// references -- see `tests/assets/images/colr/README.md`.
    fn cpu_canvas(spec: &serde_json::Value) -> Canvas {
        let mut canvas =
            Canvas::new(number(spec, "width"), number(spec, "height"));
        canvas.set_gpu(false);
        canvas
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
        let mut canvas = cpu_canvas(spec);
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

    /// The largest difference between `ours` and `chrome`, both `width`
    /// wide, in any channel of any pixel `skip` does not name.
    fn largest_difference(
        ours: &[u8],
        chrome: &[u8],
        width: usize,
        skip: impl Fn(usize, usize) -> bool,
    ) -> u8 {
        ours.iter()
            .zip(chrome)
            .enumerate()
            .filter(|(k, _)| !skip((k / 4) % width, (k / 4) / width))
            .map(|(_, (a, b))| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    }

    /// The cells the sheet puts the glyphs with no colour paint in -- the
    /// code points its layout lists under `withoutColourPaint`, `.notdef`
    /// among them -- which the platform draws rather than this crate's
    /// painter.
    fn plain_cells(spec: &serde_json::Value) -> impl Fn(usize, usize) -> bool {
        let codepoints = spec["codepoints"].as_array().expect("code points");
        let plain: Vec<usize> = spec["withoutColourPaint"]
            .as_array()
            .expect("the cells left out")
            .iter()
            .filter_map(|hex| codepoints.iter().position(|cp| cp == hex))
            .collect();
        let (columns, cell) = (
            number(spec, "columns") as usize,
            number(spec, "cell") as usize,
        );
        move |x, y| plain.contains(&((y / cell) * columns + x / cell))
    }

    /// The largest difference between the sheet `name` lays out, drawn here
    /// and moved across by `shift`, and Chrome's, in its colour glyphs.
    fn checked(name: &str, shift: f32) -> u8 {
        register();
        let spec = layout(name);
        largest_difference(
            &glyph_sheet(&spec, shift),
            &reference(&spec),
            number(&spec, "width") as usize,
            plain_cells(&spec),
        )
    }

    #[test]
    fn colrv1_glyphs_draw_as_chrome_draws_them() {
        let largest = checked("chrome-test-glyphs-48px", 0.0);
        assert!(largest <= COLOUR_GLYPH_TOLERANCE, "off by {largest}");
    }

    #[test]
    fn colrv1_glyphs_with_their_axes_varied_draw_as_chrome_draws_them() {
        let largest = checked("chrome-test-glyphs-48px-varied", 0.0);
        assert!(largest <= COLOUR_GLYPH_TOLERANCE, "off by {largest}");
    }

    #[test]
    fn colr_version_0_glyphs_draw_as_chrome_draws_them() {
        register();
        let spec = layout("chrome-colr-v0");
        // Byte for byte, at every size.
        let largest = largest_difference(
            &v0_sheet(&spec),
            &reference(&spec),
            number(&spec, "width") as usize,
            |_, _| false,
        );
        assert_eq!(largest, 0);
    }

    #[test]
    fn the_comparison_refuses_glyphs_drawn_a_pixel_off() {
        // The control for the comparisons above: a sheet moved a pixel
        // across stands further from Chrome's than the tolerance passes.
        let largest = checked("chrome-test-glyphs-48px", 1.0);
        assert!(largest > COLOUR_GLYPH_TOLERANCE, "only {largest}");
    }
}
