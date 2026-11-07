//! Text a GPU canvas draws does not depend on the fonts registered before
//! it: neither on how many times its own font was, nor on fonts registered
//! under other families in between.
//!
//! A new face for each registration of the same bytes, and a new instance of
//! a variable face after every registration of any font, each gave the text a
//! typeface it had not been drawn in, and the GPU drew such glyphs a level
//! apart in a few edge pixels, in a sequence that followed the registrations.
//! A CPU canvas drew one page throughout.

use std::env;

use meo_skia_canvas::prelude::*;

/// The page, in pixels.
const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 300.0;

/// How many draws, a registration before each.
const DRAWS: usize = 24;

/// A static face.
const MONOTON: &str = "tests/assets/fonts/Monoton-Regular.woff";
/// A variable face, drawn in an instance at the weight asked for.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
/// What is registered under [`BETWEEN`] between draws.
const RALEWAY: &str =
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";

/// The family drawn in, no other test's.
const FAMILY: &str = "Drawn After Registrations";
/// The family registered between draws, no other test's.
const BETWEEN: &str = "Registered Between Draws";

/// The same text tests/gpu_path_coverage.rs reads, for the same reason: CI's
/// Linux runner sets it so that a green run has drawn on lavapipe.
const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// #611's draw: glyphs large enough that the GPU draws them otherwise than
/// as small masks -- 96px at a scale of 2.4.
fn drawn() -> Result<Option<ImageData>, Error> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(true);
    if canvas.engine_kind() != EngineKind::Gpu {
        return Ok(None);
    }
    let ctx = canvas.context();
    ctx.scale(2.4, 2.4);
    assert!(ctx.set_font_css(&format!("italic 900 96px \"{FAMILY}\"")));
    ctx.fill_text("N.T.E", 10.0, 100.0, None);
    ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT).map(Some)
}

/// What is registered before each draw, borrowing the bytes it registers.
type Registration<'a> = Box<dyn Fn(&FontLibrary) -> Result<(), Error> + 'a>;

/// How many distinct pages [`DRAWS`] draws in `file` make, with `between`
/// registering something before each.
fn pages(
    library: &FontLibrary,
    file: &str,
    between: impl Fn(&FontLibrary) -> Result<(), Error>,
) -> Result<usize, Error> {
    let bytes = std::fs::read(file).expect("a bundled font");
    library.register_font_from_data(FAMILY, &bytes)?;
    let mut pages: Vec<ImageData> = Vec::new();
    for _ in 0..DRAWS {
        between(library)?;
        let page = drawn()?.expect("the canvas is on the GPU");
        // A blank page each time would be one page too.
        let (pixels, rest) = page.pixels().as_chunks::<4>();
        assert!(rest.is_empty(), "an 8-bit page is whole RGBA pixels");
        let inked = pixels.iter().filter(|pixel| pixel[3] > 0).count();
        assert!(inked > 10_000, "the text drew {inked} pixels");
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    Ok(pages.len())
}

/// Skips where there is no GPU, unless [`REQUIRE_GPU`] asks for one.
#[test]
fn text_draws_the_same_whatever_fonts_were_registered_before_it()
-> Result<(), Error> {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    if let Ok(required) = env::var(REQUIRE_GPU) {
        let backend = BackendInfo::query();
        assert!(
            probe.engine_kind() == EngineKind::Gpu
                && backend
                    .device
                    .as_deref()
                    .is_some_and(|device| device.contains(&required)),
            "{REQUIRE_GPU} asks for a GPU named {required:?}; the engine is \
             {:?} on {:?}, error {:?}",
            probe.engine_kind(),
            backend.device,
            backend.error,
        );
    }
    if probe.engine_kind() != EngineKind::Gpu {
        return Ok(());
    }

    let raleway = std::fs::read(RALEWAY).expect("the bundled Raleway");
    for file in [MONOTON, OSWALD] {
        let again = std::fs::read(file).expect("a bundled font");
        let cases: [(&str, Registration); 2] = [
            (
                "registered again",
                Box::new(|library| {
                    library.register_font_from_data(FAMILY, &again)
                }),
            ),
            (
                "with another family registered",
                Box::new(|library| {
                    library.register_font_from_data(BETWEEN, &raleway)
                }),
            ),
        ];
        for (between, register) in cases {
            let library = FontLibrary::new();
            let made = pages(&library, file, register)?;
            assert_eq!(
                made, 1,
                "{file}, {between} before each of {DRAWS} draws: {made} pages"
            );
        }
    }
    Ok(())
}
