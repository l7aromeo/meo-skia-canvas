//! A family's faces are narrowed by slant before their weight is weighed,
//! as CSS Fonts 4 section 5.2 narrows them, on every surface that matches
//! registered faces: canvas text, a `TextEngine` paragraph and an SVG's
//! text.
//!
//! Skia's own matcher scores a face's weight so that it can outrank its
//! slant, so these hold on a Skia without
//! `containers/skia-font-match-slant-first.patch` only through the family
//! names the library sends a style to -- see `StyleGroups`.
//!
//! The italic face leans 12deg of its own, so it tells apart from the upright
//! face skewed; each test checks first that the two faces it compares draw
//! differently, so that an equality says which face was drawn.

use meo_skia_canvas::prelude::*;

const UPRIGHT: &str =
    "tests/assets/fonts/Recursive/Recursive-upright-subset.ttf";
const UPRIGHT_900: &str =
    "tests/assets/fonts/Recursive/Recursive-upright900-subset.ttf";
const ITALIC_900: &str =
    "tests/assets/fonts/Recursive/Recursive-italic900-subset.ttf";
const ITALIC_400: &str =
    "tests/assets/fonts/Recursive/Recursive-italic400-subset.ttf";

/// The glyphs every subset holds.
const TEXT: &str = "IH";

const SIZE: f32 = 300.0;

/// Registers `files` under `family`, in order.
fn register(library: &FontLibrary, family: &str, files: &[&str]) {
    for file in files {
        library
            .register_font_from_path(family, file)
            .expect("a bundled face registers");
    }
}

/// Unencoded RGBA of `canvas`, drawn on the CPU.
fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export")
}

/// `TEXT` drawn by a canvas in the CSS font `font`.
fn canvas_text(font: &str) -> Vec<u8> {
    let mut canvas = Canvas::new(SIZE, SIZE);
    canvas.set_gpu(false);
    {
        let ctx = canvas.context();
        assert!(ctx.set_font_css(font), "{font} parses");
        ctx.fill_text(TEXT, 40.0, 250.0, None);
    }
    pixels(&mut canvas)
}

/// `TEXT` laid out by a `TextEngine` over `library` in `family` at `weight`
/// and `slant`, and drawn.
fn engine_text(
    library: &FontLibrary,
    family: &str,
    weight: i32,
    slant: TextSlant,
) -> Vec<u8> {
    let engine = TextEngine::new(library);
    let style = TextStyle {
        font_families: vec![family.into()],
        font_size: 200.0,
        font_weight: weight,
        slant,
        ..TextStyle::default()
    };
    let mut builder = engine.paragraph_builder(&style);
    builder.add_text(TEXT);
    let paragraph = builder.build(SIZE);
    let mut canvas = Canvas::new(SIZE, SIZE);
    canvas.set_gpu(false);
    canvas.context().draw_paragraph(&paragraph, 40.0, 20.0);
    pixels(&mut canvas)
}

/// `TEXT` set by an SVG document in `family` with the `font-style` and
/// `font-weight` given, rasterized.
fn svg_text(family: &str, style: &str, weight: &str) -> Vec<u8> {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}"><text x="40" y="250" font-size="200" font-family="{family}" font-style="{style}" font-weight="{weight}">{TEXT}</text></svg>"#
    );
    let image = Svg::parse(&svg)
        .expect("the document parses")
        .rasterize(SIZE as u32, SIZE as u32)
        .expect("rasterizes");
    let mut canvas = Canvas::new(SIZE, SIZE);
    canvas.set_gpu(false);
    canvas.context().draw_image(&image, 0.0, 0.0);
    pixels(&mut canvas)
}

#[test]
fn canvas_italic_takes_the_italic_face_far_in_weight() {
    // #588: an upright 400 face and an italic 900 one, in either order.
    let library = FontLibrary::new();
    register(&library, "CanvasUprightAlone", &[UPRIGHT]);
    register(&library, "CanvasItalicAlone", &[ITALIC_900]);
    register(&library, "CanvasMixed", &[UPRIGHT, ITALIC_900]);
    register(&library, "CanvasMixedReversed", &[ITALIC_900, UPRIGHT]);
    let italic = canvas_text("italic 200px CanvasItalicAlone");
    assert_ne!(
        italic,
        canvas_text("italic 200px CanvasUprightAlone"),
        "the italic face draws unlike the upright one skewed",
    );
    assert!(
        canvas_text("italic 200px CanvasMixed") == italic,
        "italic draws the italic face",
    );
    assert!(
        canvas_text("italic 200px CanvasMixedReversed") == italic,
        "italic draws the italic face whichever was registered first",
    );
    assert!(
        canvas_text("200px CanvasMixed")
            == canvas_text("200px CanvasUprightAlone"),
        "plain text draws the upright face",
    );
    // At 900 Skia's own matcher answers plain text with the italic 900 face.
    assert!(
        canvas_text("900 200px CanvasMixed")
            == canvas_text("900 200px CanvasUprightAlone"),
        "plain text at 900 draws the upright face",
    );
}

#[test]
fn canvas_normal_takes_the_upright_face_far_in_weight() {
    // An upright 900 face and an italic 400 one: Skia's own matcher answers
    // plain text at 400 with the italic face, the nearer in weight.
    let library = FontLibrary::new();
    register(&library, "HeavyUprightAlone", &[UPRIGHT_900]);
    register(&library, "LightItalicAlone", &[ITALIC_400]);
    register(&library, "HeavyMixed", &[UPRIGHT_900, ITALIC_400]);
    let upright = canvas_text("200px HeavyUprightAlone");
    assert_ne!(
        upright,
        canvas_text("200px LightItalicAlone"),
        "the two faces draw differently",
    );
    assert!(
        canvas_text("200px HeavyMixed") == upright,
        "plain text draws the upright face",
    );
    assert!(
        canvas_text("italic 200px HeavyMixed")
            == canvas_text("italic 200px LightItalicAlone"),
        "italic draws the italic face",
    );
}

#[test]
fn a_list_naming_nothing_falls_back_by_slant_first() {
    // The faces a list naming no available family is set in are the
    // `sans-serif` generic's, matched in the style asked for -- here a
    // claim of `sans-serif` holding an upright 400 face and an italic 900
    // one.
    let library = FontLibrary::new();
    register(&library, "sans-serif", &[UPRIGHT, ITALIC_900]);
    register(&library, "FallbackItalicAlone", &[ITALIC_900]);
    register(&library, "FallbackUprightAlone", &[UPRIGHT]);
    let italic = canvas_text("italic 200px FallbackItalicAlone");
    assert_ne!(
        italic,
        canvas_text("italic 200px FallbackUprightAlone"),
        "the italic face draws unlike the upright one skewed",
    );
    assert!(
        canvas_text("italic 200px NoFamilyOfThisNameAnywhere") == italic,
        "the fallback draws the italic face",
    );
}

#[test]
fn a_text_engine_takes_the_italic_face_far_in_weight() {
    let library = FontLibrary::new();
    register(&library, "EngineUprightAlone", &[UPRIGHT]);
    register(&library, "EngineItalicAlone", &[ITALIC_900]);
    register(&library, "EngineMixed", &[UPRIGHT, ITALIC_900]);
    register(&library, "EngineHeavyMixed", &[UPRIGHT_900, ITALIC_400]);
    register(&library, "EngineHeavyAlone", &[UPRIGHT_900]);
    let italic =
        engine_text(&library, "EngineItalicAlone", 400, TextSlant::Italic);
    assert_ne!(
        italic,
        engine_text(&library, "EngineUprightAlone", 400, TextSlant::Italic),
        "the italic face draws unlike the upright one skewed",
    );
    assert!(
        engine_text(&library, "EngineMixed", 400, TextSlant::Italic) == italic,
        "italic draws the italic face",
    );
    assert!(
        engine_text(&library, "EngineHeavyMixed", 400, TextSlant::Upright)
            == engine_text(
                &library,
                "EngineHeavyAlone",
                400,
                TextSlant::Upright
            ),
        "plain text draws the upright face",
    );
    assert!(
        engine_text(&library, "EngineMixed", 900, TextSlant::Upright)
            == engine_text(
                &library,
                "EngineUprightAlone",
                900,
                TextSlant::Upright
            ),
        "plain text at 900 draws the upright face",
    );
}

#[test]
fn an_svg_takes_the_italic_face_far_in_weight() {
    let library = FontLibrary::new();
    register(&library, "SvgUprightAlone", &[UPRIGHT]);
    register(&library, "SvgItalicAlone", &[ITALIC_900]);
    register(&library, "SvgMixed", &[UPRIGHT, ITALIC_900]);
    register(&library, "SvgHeavyMixed", &[UPRIGHT_900, ITALIC_400]);
    register(&library, "SvgHeavyAlone", &[UPRIGHT_900]);
    let italic = svg_text("SvgItalicAlone", "italic", "normal");
    assert_ne!(
        italic,
        svg_text("SvgUprightAlone", "italic", "normal"),
        "the italic face draws unlike the upright one skewed",
    );
    assert!(
        svg_text("SvgMixed", "italic", "normal") == italic,
        "italic draws the italic face",
    );
    assert!(
        svg_text("SvgHeavyMixed", "normal", "normal")
            == svg_text("SvgHeavyAlone", "normal", "normal"),
        "plain text draws the upright face",
    );
    // The weight is read from the document: at 900 Skia's own matcher
    // answers plain text with the italic 900 face, which a document read at
    // the initial 400 would not have been sent away from.
    assert!(
        svg_text("SvgMixed", "normal", "900")
            == svg_text("SvgUprightAlone", "normal", "900"),
        "plain text at 900 draws the upright face",
    );
}
