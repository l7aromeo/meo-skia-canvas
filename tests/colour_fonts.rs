//! Text set in a font with a `COLR` table, of version 0 or 1, drawn in its
//! own colours -- and on macOS and Windows, where this crate draws such a
//! glyph from its paint graph rather than through the platform, drawn as
//! Chrome draws it. The references are Chrome 158's, described in
//! `tests/assets/images/colr`.

use meo_skia_canvas::prelude::*;

const COLR_V1: &str =
    "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";
const COLR_V0: &str = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const REFERENCES: &str = "tests/assets/images/colr";

/// How far a channel of a colour glyph may stand from Chrome's at any pixel,
/// edges included. Measured at 3, at one pixel of the test glyphs and with
/// their axes varied alike, and past 1 at ten: both draw the same paint graph
/// through Skia's CPU raster.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const COLOUR_GLYPH_TOLERANCE: u8 = 3;

/// How far from a tie, in levels, the exact composite of a translucent
/// pixel may lie where the GPU canvas stores it a level off it: Metal blends
/// an image at half precision, and every such miss of an image or a layer
/// lies within this of a tie -- see the eight-bit composite entry in
/// `docs/browser-differences.md`.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const GPU_COMPOSITE_TIE_LEVELS: f64 = 0.18;

/// How far a pixel's red must stand above its blue to count as red: well
/// past where anti-aliasing a blue glyph on white puts any pixel.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const RED_OVER_BLUE: u8 = 60;

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

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod against_chrome {
    use super::*;

    /// The family a character the test fonts lack falls back to in the
    /// Chrome comparisons, named so that both engines take the same one: a
    /// list nothing answers falls back to Chrome's standard font, Times, and
    /// to this crate's `sans-serif` -- see `docs/browser-differences.md`.
    const FALLBACK: &str = "serif";

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

    /// The grid of test glyphs as `spec` lays it out, drawn here on the CPU,
    /// moved across by `shift` pixels.
    fn glyph_sheet(spec: &serde_json::Value, shift: f32) -> Vec<u8> {
        Sheet {
            shift,
            ..Sheet::default()
        }
        .draw(spec)
    }

    /// How a sheet of the test glyphs `spec` lays out is drawn.
    struct Sheet<'a> {
        /// How far across every glyph is moved, in pixels.
        shift: f32,
        /// Whether the canvas draws on the GPU.
        gpu: bool,
        /// How many times wider and taller each cell is than `spec`'s, the
        /// glyph centred in the room it gains.
        spread: usize,
        /// The cells drawn, in the order drawn; every cell, in order, where
        /// `None`.
        cells: Option<&'a [usize]>,
        /// Whether the page is painted in `spec`'s background first.
        backed: bool,
        /// Whether the pixels are read back premultiplied.
        premultiplied: bool,
    }

    impl Default for Sheet<'_> {
        fn default() -> Self {
            Self {
                shift: 0.0,
                gpu: false,
                spread: 1,
                cells: None,
                backed: true,
                premultiplied: false,
            }
        }
    }

    impl Sheet<'_> {
        /// The width and height of the sheet in pixels.
        fn size(&self, spec: &serde_json::Value) -> (f32, f32) {
            let spread = self.spread as f32;
            let columns = number(spec, "columns") as usize;
            let rows = spec["codepoints"]
                .as_array()
                .expect("code points")
                .len()
                .div_ceil(columns);
            let cell = number(spec, "cell") * spread;
            match self.spread {
                1 => (number(spec, "width"), number(spec, "height")),
                _ => (columns as f32 * cell, rows as f32 * cell),
            }
        }

        fn draw(&self, spec: &serde_json::Value) -> Vec<u8> {
            let columns = number(spec, "columns") as usize;
            let cell = number(spec, "cell") * self.spread as f32;
            let margin = (cell - number(spec, "cell")) / 2.0;
            let (width, height) = self.size(spec);
            let mut canvas = Canvas::new(width, height);
            canvas.set_gpu(self.gpu);
            let ctx = canvas.context();
            if self.backed {
                let background =
                    spec["background"].as_str().expect("a background");
                ctx.set_fill_style_css(background).expect("a colour");
                ctx.fill_rect(0.0, 0.0, width, height);
            }
            assert!(ctx.set_font_css(&format!(
                "{}px ColrGlyphs, {FALLBACK}",
                number(spec, "size")
            )));
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
            let codepoints =
                spec["codepoints"].as_array().expect("code points");
            let every: Vec<usize> = (0..codepoints.len()).collect();
            for &i in self.cells.unwrap_or(&every) {
                let hex = codepoints[i].as_str().expect("hex");
                let cp = u32::from_str_radix(hex, 16).expect("hex");
                let text = char::from_u32(cp).expect("a scalar").to_string();
                let x = (i % columns) as f32 * cell
                    + margin
                    + number(spec, "x")
                    + self.shift;
                let y = (i / columns) as f32 * cell
                    + margin
                    + number(spec, "baseline");
                ctx.fill_text(&text, x, y, None);
            }
            let export = ImageDataExport {
                pixels: PixelExportOptions {
                    premultiplied: self.premultiplied,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            };
            canvas
                .to_image_data(None, &export)
                .expect("pixels")
                .pixels()
                .to_vec()
        }
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
            assert!(ctx.set_font_css(&format!("{size}px ColrV0, {FALLBACK}")));
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
        plain_cells_spread(spec, 1)
    }

    /// [`plain_cells`] on a sheet whose cells are `spread` times as wide and
    /// tall -- see [`Sheet::spread`].
    fn plain_cells_spread(
        spec: &serde_json::Value,
        spread: usize,
    ) -> impl Fn(usize, usize) -> bool {
        let codepoints = spec["codepoints"].as_array().expect("code points");
        let plain: Vec<usize> = spec["withoutColourPaint"]
            .as_array()
            .expect("the cells left out")
            .iter()
            .filter_map(|hex| codepoints.iter().position(|cp| cp == hex))
            .collect();
        let (columns, cell) = (
            number(spec, "columns") as usize,
            number(spec, "cell") as usize * spread,
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

    /// Whether this machine has a GPU engine, saying so where it has not.
    fn has_gpu() -> bool {
        let mut probe = Canvas::new(1.0, 1.0);
        probe.set_gpu(true);
        let gpu = probe.engine_kind() == EngineKind::Gpu;
        if !gpu {
            eprintln!("no GPU engine here: the GPU comparison is not run");
        }
        gpu
    }

    #[test]
    fn a_gpu_canvas_draws_colour_glyphs_as_the_cpu_canvas_does() {
        // A GPU canvas gives the CPU canvas's output: every colour glyph of
        // both sheets, byte for byte. The cells are spread so that no glyph
        // overlaps another, which holds the glyph route alone: where one
        // translucent glyph lands on another, the GPU composites the two as
        // it composites any image -- see the test below.
        if !has_gpu() {
            return;
        }
        register();
        for name in
            ["chrome-test-glyphs-48px", "chrome-test-glyphs-48px-varied"]
        {
            let spec = layout(name);
            let on = |gpu| {
                Sheet {
                    gpu,
                    spread: 2,
                    ..Sheet::default()
                }
                .draw(&spec)
            };
            let width = Sheet {
                spread: 2,
                ..Sheet::default()
            }
            .size(&spec)
            .0 as usize;
            let largest = largest_difference(
                &on(true),
                &on(false),
                width,
                plain_cells_spread(&spec, 2),
            );
            assert_eq!(
                largest, 0,
                "{name}: the GPU canvas stands {largest} off"
            );
        }
    }

    #[test]
    fn an_overlapping_colour_glyph_parts_from_the_cpu_only_near_a_tie() {
        // U+F0600 with its axes varied is translucent, and lands on U+F0508
        // beside it. Drawn last over its neighbours, each channel is the exact
        // composite `s + d * (255 - a) / 255` of its premultiplied bytes
        // `s`, alpha `a`, over the page `d` its neighbours left: rounded to
        // nearest on the CPU, and on the GPU a level from that only within
        // `GPU_COMPOSITE_TIE_LEVELS` of a tie.
        if !has_gpu() {
            return;
        }
        register();
        let spec = layout("chrome-test-glyphs-48px-varied");
        let (neighbours, glyph) = ([89, 90, 92], 91);
        let all = [89, 90, 92, 91];
        let source = Sheet {
            cells: Some(&[glyph]),
            backed: false,
            premultiplied: true,
            ..Sheet::default()
        }
        .draw(&spec);
        let mut near_ties = 0;
        let mut composited = 0;
        for gpu in [false, true] {
            let page = Sheet {
                gpu,
                cells: Some(&neighbours),
                ..Sheet::default()
            }
            .draw(&spec);
            let drawn = Sheet {
                gpu,
                cells: Some(&all),
                ..Sheet::default()
            }
            .draw(&spec);
            for (k, (&got, (&s, &d))) in
                drawn.iter().zip(source.iter().zip(&page)).enumerate()
            {
                let a = source[k / 4 * 4 + 3];
                if k % 4 == 3 || a == 0 || a == u8::MAX {
                    continue;
                }
                let exact = f64::from(s)
                    + f64::from(d) * f64::from(u8::MAX - a) / 255.0;
                let nearest = exact.round();
                let from_tie = (exact - exact.floor() - 0.5).abs();
                if !gpu {
                    assert_eq!(
                        f64::from(got),
                        nearest,
                        "the CPU stores {got} for {exact} at byte {k}"
                    );
                    composited += usize::from(d != u8::MAX);
                    continue;
                }
                if f64::from(got) != nearest {
                    near_ties += 1;
                    assert!(
                        (f64::from(got) - nearest).abs() <= 1.0
                            && from_tie <= GPU_COMPOSITE_TIE_LEVELS,
                        "the GPU stores {got} for {exact}, {from_tie} from \
                         a tie, at byte {k}"
                    );
                }
            }
        }
        // The case holds what it is for: translucent bytes over a page
        // that is not white, where a composite can land off the byte grid.
        assert!(composited > 0, "no translucent byte over a neighbour");
        eprintln!(
            "{composited} translucent bytes over a neighbour, {near_ties} \
             stored a level apart on the GPU"
        );
    }

    #[test]
    fn the_comparison_refuses_glyphs_drawn_a_pixel_off() {
        // The control for the comparisons above: a sheet moved a pixel
        // across stands further from Chrome's than the tolerance passes.
        let largest = checked("chrome-test-glyphs-48px", 1.0);
        assert!(largest > COLOUR_GLYPH_TOLERANCE, "only {largest}");
    }
}

/// Colour glyphs drawn where a page is played back at another scale or
/// place than it was recorded at: each glyph is made for the pixels it
/// lands on there, as it would have been drawn there directly.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod played_back {
    use super::*;

    /// The canvas's size in its own units.
    const SIDE: (f32, f32) = (220.0, 80.0);

    /// Colour glyphs of the test font, each with colour paint: a radial
    /// gradient, a sweep, a translucent fill and a composite.
    const TEXT: &str = "\u{f0200}\u{f0b06}\u{f0508}\u{f0600}";

    /// Draws [`TEXT`] at an origin off the pixel grid, under `scale`.
    fn scene(ctx: &mut Context2D, scale: f32, at: (f32, f32)) {
        ctx.scale(scale, scale);
        ctx.translate(at.0, at.1);
        assert!(ctx.set_font_css("48px ColrGlyphs"));
        ctx.set_fill_style_css("rgb(40,90,160)").expect("a colour");
        ctx.fill_text(TEXT, 10.3, 60.7, None);
    }

    /// A canvas `scale` times [`SIDE`], on the GPU where `gpu`.
    fn canvas(scale: f32, gpu: bool) -> Option<Canvas> {
        let mut canvas = Canvas::new(SIDE.0 * scale, SIDE.1 * scale);
        canvas.set_gpu(gpu);
        (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
    }

    /// [`scene`] drawn straight onto a canvas `scale` times the size.
    fn direct(scale: f32, at: (f32, f32), gpu: bool) -> Option<Vec<u8>> {
        let mut canvas = canvas(scale, gpu)?;
        scene(canvas.context(), scale, at);
        Some(pixels(&mut canvas))
    }

    /// [`scene`] at 1x, on the CPU.
    fn source() -> Canvas {
        let mut source = canvas(1.0, false).expect("a CPU canvas");
        scene(source.context(), 1.0, (0.0, 0.0));
        source
    }

    /// Whether `ours` and `drawn` are the same bytes, saying how far apart
    /// they stand where they are not.
    fn same(what: &str, ours: &[u8], drawn: &[u8]) {
        let apart = ours.iter().zip(drawn).filter(|(a, b)| a != b).count();
        let largest = ours
            .iter()
            .zip(drawn)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert_eq!(
            (apart, largest),
            (0, 0),
            "{what}: {apart} bytes apart, by up to {largest}"
        );
    }

    #[test]
    fn a_page_exported_at_density_2_draws_its_colour_glyphs_at_2x() {
        register();
        for gpu in [false, true] {
            let Some(mut canvas) = canvas(1.0, gpu) else {
                continue;
            };
            scene(canvas.context(), 1.0, (0.0, 0.0));
            let exported = canvas
                .to_image_data(
                    None,
                    &ImageDataExport {
                        density: 2,
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels")
                .pixels()
                .to_vec();
            let drawn = direct(2.0, (0.0, 0.0), gpu).expect("the canvas");
            same(&format!("gpu {gpu}"), &exported, &drawn);
        }
    }

    #[test]
    fn a_canvas_drawn_in_under_a_scale_draws_its_colour_glyphs_at_that_scale() {
        register();
        for gpu in [false, true] {
            let mut source = source();
            let Some(mut into) = canvas(2.0, gpu) else {
                continue;
            };
            let ctx = into.context();
            ctx.scale(2.0, 2.0);
            ctx.draw_canvas(&mut source, 0.0, 0.0);
            let drawn = direct(2.0, (0.0, 0.0), gpu).expect("the canvas");
            same(&format!("gpu {gpu}"), &pixels(&mut into), &drawn);
        }
    }

    #[test]
    fn a_canvas_drawn_in_half_a_pixel_across_snaps_its_colour_glyphs_there() {
        // The glyph origin is snapped to the device pixel where the page is
        // played: drawn half a pixel across, a canvas places its glyphs as
        // text drawn half a pixel across places them.
        register();
        for gpu in [false, true] {
            let mut source = source();
            let Some(mut into) = canvas(1.0, gpu) else {
                continue;
            };
            into.context().draw_canvas(&mut source, 0.5, 0.0);
            let drawn = direct(1.0, (0.5, 0.0), gpu).expect("the canvas");
            same(&format!("gpu {gpu}"), &pixels(&mut into), &drawn);
        }
    }

    #[test]
    fn a_pattern_of_a_canvas_draws_its_colour_glyphs_at_the_scale_it_is_drawn()
    {
        register();
        for gpu in [false, true] {
            let mut source = source();
            let Some(mut into) = canvas(2.0, gpu) else {
                continue;
            };
            let ctx = into.context();
            let pattern = ctx.create_pattern_from_canvas(
                &mut source,
                PatternRepeat::NoRepeat,
            );
            ctx.scale(2.0, 2.0);
            ctx.set_fill_pattern(&pattern);
            ctx.fill_rect(0.0, 0.0, SIDE.0, SIDE.1);
            let drawn = direct(2.0, (0.0, 0.0), gpu).expect("the canvas");
            same(&format!("gpu {gpu}"), &pixels(&mut into), &drawn);
        }
    }
}

/// Colour glyphs on a GPU canvas through each route a text draw takes, held
/// to the CPU canvas's bytes. The glyphs stand apart, so that no glyph's
/// image composites over another's -- see
/// `against_chrome::an_overlapping_colour_glyph_parts_from_the_cpu_only_near_a_tie`.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod on_the_gpu {
    use super::*;

    /// How far below its glyphs a shadow is cast: clear of them, blur and
    /// all, so that no glyph composites over its own shadow -- the overlap
    /// the near-tie case holds.
    const SHADOW_DROP: f32 = 64.0;

    /// The bytes in a row of the canvas [`drawn`] draws on.
    const ROW: usize = 360 * 4;

    /// The rows the shadow falls in, dropped by [`SHADOW_DROP`] below the
    /// glyphs' 48px, whose baseline is at 70.7, and clear of the glyphs.
    const SHADOW_BAND: (usize, usize) = (90, 140);

    /// Colour glyphs of the test font, each with colour paint, a space
    /// between each so that none overlaps another.
    const TEXT: &str = "\u{f0200}  \u{f0b06}  \u{f0508}  \u{f0600}";

    /// The bytes of a white canvas with `TEXT` drawn after `set_up`, on
    /// the GPU where `gpu`; `None` where this machine has no GPU engine.
    fn drawn(gpu: bool, set_up: &SetUp) -> Option<Vec<u8>> {
        let mut canvas = Canvas::new(360.0, 180.0);
        canvas.set_gpu(gpu);
        if gpu && canvas.engine_kind() != EngineKind::Gpu {
            return None;
        }
        let ctx = canvas.context();
        ctx.set_fill_style_css("white").expect("a colour");
        ctx.fill_rect(0.0, 0.0, 360.0, 180.0);
        assert!(ctx.set_font_css("48px ColrGlyphs"));
        ctx.set_fill_style_css("rgb(40,90,160)").expect("a colour");
        set_up(ctx);
        ctx.fill_text(TEXT, 10.3, 70.7, None);
        Some(pixels(&mut canvas))
    }

    /// What a case sets on the context before the run is drawn.
    type SetUp = dyn Fn(&mut Context2D);

    /// Whether the GPU canvas draws what the CPU canvas does after
    /// `set_up`, naming the case where it does not.
    fn holds(case: &str, set_up: &SetUp) -> Option<String> {
        register();
        let gpu = drawn(true, set_up)?;
        let cpu = drawn(false, set_up).expect("a CPU canvas");
        let apart = gpu.iter().zip(&cpu).filter(|(a, b)| a != b).count();
        let largest = gpu
            .iter()
            .zip(&cpu)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        (apart > 0).then(|| format!("{case}: {apart} bytes, up to {largest}"))
    }

    #[test]
    fn every_route_draws_colour_glyphs_on_the_gpu_as_on_the_cpu() {
        let cases: [(&str, &SetUp); 6] = [
            ("sharp shadow", &|ctx| {
                ctx.set_shadow_color_css("rgba(0,0,0,0.5)")
                    .expect("a colour");
                ctx.set_shadow_offset(3.0, SHADOW_DROP);
            }),
            ("plain", &|_| {}),
            ("shadow", &|ctx| {
                ctx.set_shadow_color_css("rgba(0,0,0,0.5)")
                    .expect("a colour");
                ctx.set_shadow_blur(4.0);
                ctx.set_shadow_offset(3.0, SHADOW_DROP);
            }),
            ("source-in", &|ctx| {
                ctx.set_global_composite_operation(BlendMode::SourceIn);
            }),
            ("filter", &|ctx| {
                ctx.set_filter_css("blur(2px)").expect("a filter");
            }),
            ("rotated", &|ctx| ctx.rotate(0.1)),
        ];
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(case, set_up)| holds(case, *set_up))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The lowest a channel can be with anything composited at an alpha of
    /// a half over white: the exact composite's least, `255 - 128`, less the
    /// level the raster pipeline's byte rounding can take it under -- it
    /// stood up to 1.25 off the exact composite on the test glyphs.
    const HALF_OVER_WHITE: u8 = u8::MAX - HALF_ALPHA - 1;

    #[test]
    fn colour_glyphs_take_the_global_alpha_and_cast_their_shadow() {
        // What the paint asks of a colour glyph's image: drawn at a
        // `globalAlpha` of a half over white, no channel falls below half;
        // a shadow dropped clear of the glyphs is cast, dark and grey.
        register();
        let faint = drawn(false, &|ctx| ctx.set_global_alpha(0.5))
            .expect("a CPU canvas");
        let darkest = faint.iter().copied().min().unwrap_or(u8::MAX);
        assert!(darkest >= HALF_OVER_WHITE, "a channel at {darkest}");
        let cast = drawn(false, &|ctx| {
            ctx.set_shadow_color_css("black").expect("a colour");
            ctx.set_shadow_offset(0.0, SHADOW_DROP);
        })
        .expect("a CPU canvas");
        let below = &cast[SHADOW_BAND.0 * ROW..SHADOW_BAND.1 * ROW];
        let dark = below.chunks(4).filter(|p| p[1] < 64).count();
        let coloured = below
            .chunks(4)
            .filter(|p| p[0].abs_diff(p[2]) > COLOURED_SPREAD)
            .count();
        assert!(
            dark > 500 && coloured == 0,
            "{dark} dark, {coloured} coloured"
        );
    }

    /// The premultiplied bytes of [`TEXT`] drawn as [`drawn`] draws it, in
    /// `fill`, onto nothing, on the CPU.
    fn source(fill: &str) -> Vec<u8> {
        let mut canvas = Canvas::new(360.0, 180.0);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        assert!(ctx.set_font_css("48px ColrGlyphs"));
        ctx.set_fill_style_css(fill).expect("a colour");
        ctx.fill_text(TEXT, 10.3, 70.7, None);
        let export = ImageDataExport {
            pixels: PixelExportOptions {
                premultiplied: true,
                ..PixelExportOptions::default()
            },
            ..ImageDataExport::default()
        };
        canvas
            .to_image_data(None, &export)
            .expect("pixels")
            .pixels()
            .to_vec()
    }

    /// The alpha a `globalAlpha` of a half composites through, as a byte:
    /// 0.5 of 255, rounded.
    const HALF_ALPHA: u8 = 128;

    #[test]
    fn under_a_global_alpha_the_gpu_misses_the_exact_composite_only_near_a_tie()
    {
        // A glyph's image composites through the `globalAlpha` as its layer
        // did, which is Chrome's CPU canvas byte for byte. The GPU blends it
        // at half precision, as it blends any image under an alpha: a level
        // from the CPU at most, and off the exact composite only near a tie.
        // The exact composite is known where a glyph's bytes do not depend
        // on the fill, whose colour carries the alpha into the glyph's
        // foreground paint: the pixels drawn alike in two fills.
        register();
        let half = |ctx: &mut Context2D| ctx.set_global_alpha(0.5);
        let Some(gpu) = drawn(true, &half) else {
            return;
        };
        let cpu = drawn(false, &half).expect("a CPU canvas");
        let apart = gpu.iter().zip(&cpu).map(|(a, b)| a.abs_diff(*b)).max();
        assert!(apart <= Some(1), "the GPU stands {apart:?} from the CPU");
        let (blue, red) = (source("rgb(40,90,160)"), source("rgb(250,10,10)"));
        let alpha = f64::from(HALF_ALPHA) / 255.0;
        let (mut held, mut near) = (0, 0);
        for i in (0..blue.len()).step_by(4) {
            let a = f64::from(blue[i + 3]);
            if a == 0.0 || blue[i..i + 4] != red[i..i + 4] {
                continue;
            }
            for c in i..i + 3 {
                let exact = f64::from(blue[c]) * alpha
                    + 255.0 * (1.0 - a * alpha / 255.0);
                held += 1;
                if f64::from(gpu[c]) == exact.round() {
                    continue;
                }
                near += 1;
                let from_tie = (exact - exact.floor() - 0.5).abs();
                assert!(
                    (f64::from(gpu[c]) - exact).abs() < 1.0
                        && from_tie <= GPU_COMPOSITE_TIE_LEVELS,
                    "the GPU stores {} for {exact}, {from_tie} from a tie",
                    gpu[c]
                );
            }
        }
        assert!(held > 1000, "only {held} channels to hold");
        eprintln!("{held} channels held, {near} off the exact composite");
    }

    #[test]
    fn colour_glyphs_under_a_filter_keep_their_colours() {
        // A run under `blur()` can be drawn through the chain's coverage
        // route, which paints a run in its one colour; a run of colour
        // glyphs drawn there lost its glyphs' own colours.
        register();
        let blurred = drawn(false, &|ctx| {
            ctx.set_filter_css("blur(2px)").expect("a filter");
        })
        .expect("a CPU canvas");
        let red = blurred
            .chunks(4)
            .filter(|p| p[0] > p[2].saturating_add(RED_OVER_BLUE))
            .count();
        assert!(red > 50, "only {red} red pixels under the blur");
    }

    #[test]
    fn a_paragraph_draws_colour_glyphs_on_the_gpu_as_on_the_cpu() {
        let library = FontLibrary::new();
        library
            .use_paths(Some("ColrGlyphs"), [COLR_V1])
            .expect("the COLRv1 font registers");
        let engine = TextEngine::new(&library);
        let style = TextStyle {
            font_families: vec!["ColrGlyphs".into()],
            font_size: 48.0,
            color: RgbaLinear::opaque(0.1, 0.2, 0.4),
            ..TextStyle::default()
        };
        let paragraph = engine.layout_text(TEXT, &style, 360.0);
        let draw = |gpu: bool| {
            let mut canvas = Canvas::new(360.0, 110.0);
            canvas.set_gpu(gpu);
            if gpu && canvas.engine_kind() != EngineKind::Gpu {
                return None;
            }
            let ctx = canvas.context();
            ctx.set_fill_style_css("white").expect("a colour");
            ctx.fill_rect(0.0, 0.0, 360.0, 110.0);
            ctx.draw_paragraph(&paragraph, 10.3, 10.7);
            Some(pixels(&mut canvas))
        };
        let Some(gpu) = draw(true) else {
            return;
        };
        let cpu = draw(false).expect("a CPU canvas");
        // The paragraph sets the colour glyphs, in their own colours: the
        // radial gradient's red among them, which the paragraph's blue is not.
        let red = cpu
            .chunks(4)
            .filter(|p| p[0] > p[2].saturating_add(RED_OVER_BLUE))
            .count();
        assert!(red > 50, "only {red} red pixels");
        let apart = gpu.iter().zip(&cpu).filter(|(a, b)| a != b).count();
        assert_eq!(apart, 0, "{apart} bytes apart");
    }
}
