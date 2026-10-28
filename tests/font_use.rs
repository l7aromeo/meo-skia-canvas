//! Registering fonts the way JavaScript's `FontLibrary.use()` does, and
//! getting back what was registered. The expected faces are the ones the
//! Node binding returns for the same files.

use std::path::PathBuf;

use meo_skia_canvas::prelude::*;

const RALEWAY: &str =
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";
const RALEWAY_ITALIC: &str =
    "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf";

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

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

/// Oswald's default instance is its regular weight.
fn oswald(family: &str, file: Option<&str>) -> FontFace {
    FontFace {
        weight: 400,
        ..face(family, FontSlant::Normal, file)
    }
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).expect("a bundled font")
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
fn a_registered_family_is_matched_in_any_case() {
    // CSS Fonts 4 section 5.1: "User agents must match these names case
    // insensitively". The faces are filed under the registered spelling,
    // and a font naming the family in another case reached the fallback.
    FontLibrary::new()
        .use_paths(Some("UseCased"), [RALEWAY])
        .expect("registers");
    let mut canvas = Canvas::new(400.0, 60.0);
    let ctx = canvas.context();
    let width = |ctx: &mut Context2D, css: &str| {
        assert!(ctx.set_font_css(css), "{css:?}");
        ctx.measure_text("Wavering", None).width
    };
    let filed = width(ctx, "40px UseCased");
    assert_ne!(
        filed,
        width(ctx, "40px NoSuchFamilyAnywhere"),
        "the control"
    );
    for css in ["40px usecased", "40px USECASED", "40px \"useCased\""] {
        assert_eq!(width(ctx, css), filed, "{css}");
    }

    // And through a text engine, which asks its collection by name too.
    let library = FontLibrary::new();
    library
        .use_paths(Some("UseEngineCased"), [RALEWAY])
        .expect("registers");
    let engine = TextEngine::new(&library);
    let laid_out = |family: &str| {
        let style = TextStyle {
            font_families: vec![family.into()],
            font_size: 40.0,
            ..TextStyle::default()
        };
        engine
            .layout_text("Wavering", &style, 1000.0)
            .max_intrinsic_width()
    };
    let filed = laid_out("UseEngineCased");
    assert_ne!(
        filed,
        laid_out("NoSuchFamilyAnywhere"),
        "the engine's control"
    );
    assert_eq!(
        laid_out("useenginecased"),
        filed,
        "the engine, in lower case"
    );
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

#[test]
fn several_faces_from_bytes_file_under_one_alias() {
    // `FontLibrary.use("Batch", [raleway, ralewayItalic])` answers two faces
    // filed under `Batch`, at 100, normal then italic, from "<buffer>".
    let library = FontLibrary::new();
    let (raleway, italic) = (read(RALEWAY), read(RALEWAY_ITALIC));
    let faces = library
        .use_data_all(Some("UseDataBatch"), [&raleway[..], &italic[..]])
        .expect("both register");
    assert_eq!(
        faces,
        [
            face("UseDataBatch", FontSlant::Normal, None),
            face("UseDataBatch", FontSlant::Italic, None),
        ]
    );
    assert!(library.has_font("UseDataBatch"));
}

#[test]
fn several_faces_from_bytes_keep_their_own_families() {
    // `FontLibrary.use([raleway, oswald])` answers Raleway at 100 and Oswald
    // at 400.
    let library = FontLibrary::new();
    let (raleway, oswald_) = (read(RALEWAY), read(OSWALD));
    let faces = library
        .use_data_all(None, [&raleway[..], &oswald_[..]])
        .expect("both register");
    assert_eq!(
        faces,
        [
            face("Raleway", FontSlant::Normal, None),
            oswald("Oswald", None)
        ]
    );
}

#[test]
fn a_bad_face_among_bytes_registers_none() {
    let library = FontLibrary::new();
    let raleway = read(RALEWAY);
    let result = library.use_data_all(
        Some("UseDataAtomic"),
        [&raleway[..], b"not a font".as_slice()],
    );
    let Err(Error::FontRegister { reason }) = result else {
        panic!("bytes that are not a font are refused: {result:?}");
    };
    assert!(reason.contains("index 1"), "{reason}");
    assert!(
        !library.has_font("UseDataAtomic"),
        "the first face was not kept"
    );
}

#[test]
fn several_families_register_in_one_call() {
    // `FontLibrary.use({ A: [raleway, ralewayItalic], B: oswald })` answers
    // one list per family, in the order given.
    let library = FontLibrary::new();
    let faces = library
        .use_families([
            (Some("UseFamiliesA"), vec![RALEWAY, RALEWAY_ITALIC]),
            (Some("UseFamiliesB"), vec![OSWALD]),
            (None, vec![OSWALD]),
        ])
        .expect("all register");
    assert_eq!(
        faces,
        [
            vec![
                face("UseFamiliesA", FontSlant::Normal, Some(RALEWAY)),
                face("UseFamiliesA", FontSlant::Italic, Some(RALEWAY_ITALIC)),
            ],
            vec![oswald("UseFamiliesB", Some(OSWALD))],
            vec![oswald("Oswald", Some(OSWALD))],
        ]
    );
    assert!(library.has_font("UseFamiliesA"));
    assert!(library.has_font("UseFamiliesB"));
}

#[test]
fn a_bad_file_in_one_family_registers_no_family() {
    // `FontLibrary.use({ MapOk: raleway, MapBad: missing })` throws and leaves
    // `MapOk` unregistered.
    let library = FontLibrary::new();
    let result = library.use_families([
        (Some("UseFamiliesOk"), vec![RALEWAY]),
        (
            Some("UseFamiliesBad"),
            vec!["tests/assets/no-such-font.ttf"],
        ),
    ]);
    let Err(Error::FontRegister { reason }) = result else {
        panic!("a missing file is refused: {result:?}");
    };
    assert!(reason.contains("no-such-font.ttf"), "{reason}");
    assert!(
        !library.has_font("UseFamiliesOk"),
        "the earlier family was not kept"
    );
    assert!(!library.has_font("UseFamiliesBad"));
}
