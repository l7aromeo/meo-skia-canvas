//! Registering fonts the way JavaScript's `FontLibrary.use()` does, and
//! getting back what was registered. The expected faces are the ones the
//! Node binding returns for the same files.

use std::path::PathBuf;

use meo_skia_canvas::prelude::*;

const RALEWAY: &str =
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";
const RALEWAY_ITALIC: &str =
    "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf";

fn face(family: &str, slant: FontSlant, file: Option<&str>) -> FontFace {
    FontFace {
        family: family.to_string(),
        // A variable font reports its default instance, which for Raleway is
        // the thinnest.
        weight: 100,
        slant,
        stretch: FontStretch::Normal,
        file: file.map(PathBuf::from),
    }
}

#[test]
fn without_an_alias_each_face_takes_the_family_it_declares() {
    let library = FontLibrary::new();
    let faces = library
        .use_paths(None, [RALEWAY, RALEWAY_ITALIC])
        .expect("both register");
    assert_eq!(
        faces,
        [
            face("Raleway", FontSlant::Normal, Some(RALEWAY)),
            face("Raleway", FontSlant::Italic, Some(RALEWAY_ITALIC)),
        ]
    );
    assert!(library.has_font("Raleway"));
}

#[test]
fn an_alias_files_every_face_under_it() {
    let library = FontLibrary::new();
    let faces = library
        .use_paths(Some("Alias"), [RALEWAY])
        .expect("registers");
    assert_eq!(faces, [face("Alias", FontSlant::Normal, Some(RALEWAY))]);
    assert!(library.has_font("Alias"));
}

#[test]
fn data_has_no_file() {
    let library = FontLibrary::new();
    let bytes = std::fs::read(RALEWAY).expect("the font");
    let face_ = library
        .use_data(Some("FromData"), &bytes)
        .expect("registers");
    assert_eq!(face_, face("FromData", FontSlant::Normal, None));
    assert_eq!(
        library.use_data(None, &bytes).expect("registers").family,
        "Raleway"
    );
}

#[test]
fn a_registered_face_draws_under_the_name_it_was_filed_under() {
    // What the registration is for: the alias reaches a draw, and a family
    // nothing is filed under draws something else.
    FontLibrary::new()
        .use_paths(Some("UseDrawn"), [RALEWAY])
        .expect("registers");
    let mut canvas = Canvas::new(400.0, 60.0);
    let ctx = canvas.context();
    let width = |ctx: &mut Context2D, family: &str| {
        ctx.set_font(&Font::new(family, 40.0));
        ctx.measure_text("Wavering", None).width
    };
    let filed = width(ctx, "UseDrawn");
    let reference = {
        FontLibrary::new()
            .register_font_from_path("UseReference", RALEWAY)
            .expect("registers");
        width(ctx, "UseReference")
    };
    assert_eq!(filed, reference, "the same face either way");
    assert_ne!(filed, width(ctx, "NoSuchFamilyAnywhere"));
}

#[test]
fn a_failure_registers_nothing() {
    let library = FontLibrary::new();
    let result = library.use_paths(
        Some("UseAtomic"),
        [
            PathBuf::from(RALEWAY),
            PathBuf::from("tests/assets/no-such-font.ttf"),
        ],
    );
    let Err(Error::FontRegister { reason }) = result else {
        panic!("a missing file is refused: {result:?}");
    };
    assert!(reason.contains("no-such-font.ttf"), "{reason}");
    assert!(
        !library.has_font("UseAtomic"),
        "the first file was not kept"
    );
}

#[test]
fn bytes_that_are_not_a_font_are_refused() {
    let library = FontLibrary::new();
    let result = library.use_data(Some("UseGarbage"), b"not a font");
    assert!(
        matches!(result, Err(Error::FontRegister { .. })),
        "{result:?}"
    );
    assert!(!library.has_font("UseGarbage"));
}
