//! Glyphs sheared past what FreeType draws are drawn sheared all the same.
//!
//! FreeType draws a glyph unsheared once its transform is sheared past
//! about 79.65 degrees, so on Linux the run painters draw such glyphs from
//! their outlines instead -- see `node::typography::drawn_as_outlines`. Each
//! case here draws a run and holds the columns it inks to the shape its
//! outline has, which is sheared on every platform: an `oblique` run, by
//! `outline_text`; a run under a sheared transform, by mapping that outline
//! through the transform. A run drawn upright inks a strip a fraction as
//! wide, so the cases fail by hundreds of pixels where the shear is lost.

use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "ObliqueOutlinesOswald";
const TEXT: &str = "IH";

/// How far the inked columns may lie from the outline's, in pixels: the
/// pixel an edge's antialiasing reaches past it, and one more for the
/// rounding of the outline's own bounds.
const EDGE: f32 = 2.0;

/// The space left around a run, in pixels.
const MARGIN: f32 = 20.0;

/// A canvas transform's shear past FreeType's limit of `sqrt(30)`, about
/// 5.48.
const STEEP: f32 = 5.6;

fn register() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("bundled Oswald registers");
}

/// The first and last columns of `canvas` with any ink, or `None` where it
/// has none.
fn inked_columns(canvas: &mut Canvas) -> Option<(f32, f32)> {
    let width = canvas.width() as usize;
    let data = canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels");
    let columns: Vec<usize> = data
        .pixels()
        .chunks(4)
        .enumerate()
        .filter(|(_, px)| px[3] > 0)
        .map(|(i, _)| i % width)
        .collect();
    Some((
        *columns.iter().min()? as f32,
        *columns.iter().max()? as f32 + 1.0,
    ))
}

/// The columns `outline` covers, `x` to the right and under `transform`:
/// the outline's corners mapped, which for an `I` and an `H`, whose stems
/// reach the outline's top and bottom at its left and right, are where the
/// mapped outline reaches.
fn outline_columns(outline: Rect, x: f32, transform: Affine) -> (f32, f32) {
    let xs = [
        (outline.left, outline.top),
        (outline.left, outline.bottom),
        (outline.right, outline.top),
        (outline.right, outline.bottom),
    ]
    .map(|(px, py)| transform.a * (px + x) + transform.c * py + transform.tx);
    (
        xs.iter().copied().fold(f32::INFINITY, f32::min),
        xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    )
}

fn assert_columns(case: &str, inked: Option<(f32, f32)>, outline: (f32, f32)) {
    let (left, right) = inked.unwrap_or_else(|| panic!("{case}: no ink"));
    assert!(
        (left - outline.0).abs() <= EDGE && (right - outline.1).abs() <= EDGE,
        "{case}: inks columns {left}..{right}, its outline {:.1}..{:.1}",
        outline.0,
        outline.1,
    );
}

#[test]
fn a_shear_past_freetypes_limit_draws_sheared_glyphs() {
    register();
    // An `oblique` run, filled and stroked, at sizes Skia draws from glyph
    // masks and from paths: both asked FreeType for a sheared glyph.
    for (degrees, size) in [(80.0, 40.0), (85.0, 40.0), (85.0, 200.0)] {
        for stroked in [false, true] {
            let font = format!("oblique {degrees}deg {size}px {FAMILY}");
            let mut probe = Canvas::new(1.0, 1.0);
            let ctx = probe.context();
            assert!(ctx.set_font_css(&font));
            let outline = ctx.outline_text(TEXT, None).bounds();
            let (width, height) = (
                (outline.right - outline.left + 2.0 * MARGIN).ceil(),
                (outline.bottom - outline.top + 2.0 * MARGIN).ceil(),
            );
            let (x, y) = (MARGIN - outline.left, MARGIN - outline.top);
            let mut canvas = Canvas::new(width, height);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&font));
            // A pen of a pixel reaches half a pixel past the outline, within
            // the edge allowed for.
            ctx.set_line_width(1.0);
            match stroked {
                true => ctx.stroke_text(TEXT, x, y, None),
                false => ctx.fill_text(TEXT, x, y, None),
            }
            assert_columns(
                &format!("{font}, stroked {stroked}"),
                inked_columns(&mut canvas),
                outline_columns(outline, x, Affine::IDENTITY),
            );
        }
    }
    // An upright run under a transform sheared as far.
    let font = format!("20px {FAMILY}");
    let mut probe = Canvas::new(1.0, 1.0);
    let ctx = probe.context();
    assert!(ctx.set_font_css(&font));
    let outline = ctx.outline_text(TEXT, None).bounds();
    let y = MARGIN - outline.top;
    let sheared = Affine {
        a: 1.0,
        b: 0.0,
        c: -STEEP,
        d: 1.0,
        tx: MARGIN + STEEP * (y + outline.bottom),
        ty: 0.0,
    };
    let columns = outline_columns(
        Rect {
            top: outline.top + y,
            bottom: outline.bottom + y,
            ..outline
        },
        0.0,
        sheared,
    );
    let mut canvas = Canvas::new(
        (columns.1 + MARGIN).ceil(),
        (outline.bottom - outline.top + 2.0 * MARGIN).ceil(),
    );
    let ctx = canvas.context();
    assert!(ctx.set_font_css(&font));
    ctx.set_transform(sheared);
    ctx.fill_text(TEXT, 0.0, y, None);
    assert_columns(
        &format!("{font} under a shear of {STEEP}"),
        inked_columns(&mut canvas),
        columns,
    );
}

#[test]
fn a_paragraph_sheared_past_freetypes_limit_draws_sheared_glyphs() {
    // The engine sets the paragraphs in the faces its own library holds.
    let library = FontLibrary::new();
    library
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("bundled Oswald registers");
    let engine = TextEngine::new(&library);
    let style = |size: f32, slant: TextSlant, rgb: (u8, u8, u8)| TextStyle {
        font_families: vec![FAMILY.into()],
        font_size: size,
        slant,
        color: RgbaLinear::from_srgb8(rgb.0, rgb.1, rgb.2, 1.0),
        ..TextStyle::default()
    };
    let (red, blue) = ((255, 0, 0), (0, 0, 255));
    // The width an `I` and an `H` ink is the outline's, wherever the
    // paragraph puts them: an `oblique` style, which the paragraph shears
    // itself.
    let oblique = TextSlant::Oblique(ObliqueAngle::new(85.0).expect("angle"));
    let mut probe = Canvas::new(1.0, 1.0);
    let ctx = probe.context();
    assert!(ctx.set_font_css(&format!("oblique 85deg 40px {FAMILY}")));
    let outline = ctx.outline_text(TEXT, None).bounds();
    let layout = engine.layout_text(TEXT, &style(40.0, oblique, red), 1.0e6);
    let mut canvas = Canvas::new(
        (outline.right - outline.left + 2.0 * MARGIN).ceil() + 100.0,
        200.0,
    );
    canvas.context().draw_paragraph(&layout, MARGIN, MARGIN);
    let (left, right) = inked_columns(&mut canvas).expect("ink");
    assert!(
        ((right - left) - (outline.right - outline.left)).abs() <= 2.0 * EDGE,
        "an oblique 85deg paragraph inks {} columns ({left}..{right} of {}), its outline {:.1} ({:?})",
        right - left,
        canvas.width(),
        outline.right - outline.left,
        outline,
    );

    // Under a transform sheared as far: a paragraph in one style, which
    // SkParagraph would paint alone, and one in two colours, painted with
    // the paint each glyph's style has.
    let mut probe = Canvas::new(1.0, 1.0);
    let ctx = probe.context();
    assert!(ctx.set_font_css(&format!("20px {FAMILY}")));
    let outline = ctx.outline_text(TEXT, None).bounds();
    let upright = TextSlant::Upright;
    let one = engine.layout_text(TEXT, &style(20.0, upright, red), 1.0e6);
    let two = {
        let mut builder = engine.paragraph_builder(&style(20.0, upright, red));
        builder.add_text("I");
        builder.push_style(&style(20.0, upright, blue));
        builder.add_text("H");
        builder.build(1.0e6)
    };
    for (name, layout) in [("one style", &one), ("two colours", &two)] {
        let height = 2.0 * MARGIN + 40.0;
        let sheared = Affine {
            a: 1.0,
            b: 0.0,
            c: -STEEP,
            d: 1.0,
            tx: MARGIN + STEEP * height,
            ty: 0.0,
        };
        let mut canvas = Canvas::new(MARGIN * 3.0 + STEEP * height, height);
        let ctx = canvas.context();
        ctx.set_transform(sheared);
        ctx.draw_paragraph(layout, 0.0, MARGIN);
        let data = canvas
            .to_image_data(None, &ImageDataExport::default())
            .expect("pixels");
        let blue_ink = data
            .pixels()
            .chunks(4)
            .filter(|px| px[3] > 0 && px[2] > px[0])
            .count();
        let (left, right) = inked_columns(&mut canvas).expect("ink");
        // Sheared, the run spans its width and the shear of its height.
        let spans = (outline.right - outline.left)
            + STEEP * (outline.bottom - outline.top);
        assert!(
            ((right - left) - spans).abs() <= 2.0 * EDGE,
            "{name} under a shear of {STEEP} inks {} columns, sheared {spans:.1}",
            right - left,
        );
        assert_eq!(
            blue_ink > 0,
            name == "two colours",
            "{name}: {blue_ink} pixels in the second style's blue",
        );
    }
}

#[test]
fn a_paragraph_shadow_sheared_past_freetypes_limit_spans_its_run() {
    // A blurred text shadow is blurred within its run's bounds, which have
    // to hold the sheared run, not the upright one FreeType measures past its
    // limit: the shadow of an 85deg run spans the run's outline and the
    // blur's reach either side, not a strip at its foot.
    let library = FontLibrary::new();
    library
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("bundled Oswald registers");
    let engine = TextEngine::new(&library);
    let sigma = 2.0;
    let style = TextStyle {
        font_families: vec![FAMILY.into()],
        font_size: 40.0,
        slant: TextSlant::Oblique(ObliqueAngle::new(85.0).expect("angle")),
        // The text itself transparent, so only its shadow is drawn.
        color: RgbaLinear::from_srgb8(0, 0, 0, 0.0),
        shadows: vec![TextShadow {
            color: RgbaLinear::from_srgb8(0, 0, 255, 1.0),
            offset_x: 0.0,
            offset_y: 0.0,
            blur_sigma: sigma,
        }],
        ..TextStyle::default()
    };
    let mut probe = Canvas::new(1.0, 1.0);
    let ctx = probe.context();
    assert!(ctx.set_font_css(&format!("oblique 85deg 40px {FAMILY}")));
    let outline = ctx.outline_text(TEXT, None).bounds();
    let layout = engine.layout_text(TEXT, &style, 1.0e6);
    let mut canvas = Canvas::new(
        (outline.right - outline.left + 2.0 * MARGIN).ceil() + 100.0,
        200.0,
    );
    canvas.context().draw_paragraph(&layout, MARGIN, MARGIN);
    let (left, right) = inked_columns(&mut canvas).expect("a shadow");
    assert!(
        right - left >= outline.right - outline.left - 2.0 * EDGE,
        "an oblique 85deg paragraph's shadow inks {} columns, its run's \
         outline {:.1}",
        right - left,
        outline.right - outline.left,
    );
}
