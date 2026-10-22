//! CSS grammars both surfaces parse, each checked against one table of
//! cases that `tests/suite/css-grammar.test.js` reads too.
//!
//! The expected answers in each table were measured in Chrome, not taken from
//! either surface, so a disagreement between the two is a disagreement with
//! the browser and not a vote between them.

use std::{fs, path::Path};

use meo_skia_canvas::prelude::*;
use serde_json::Value;

/// The cases in `tests/assets/cases/<name>.json`.
fn cases(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/assets/cases")
        .join(format!("{name}.json"));
    let text = fs::read_to_string(&path).expect("the case file is readable");
    let table: Value =
        serde_json::from_str(&text).expect("the case file is JSON");
    table["cases"].as_array().expect("a `cases` array").clone()
}

/// Registers Oswald, the variable face the font-relative rows were measured
/// on in Chrome, under the family name the rows give.
fn oswald() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf");
    let bytes = fs::read(path).expect("the Oswald test font is readable");
    FontLibrary::new()
        .register_font_from_data("Oswald", &bytes)
        .expect("Oswald registers");
}

/// The pixels a filter draws over a fixed scene, in `font` where one is
/// given -- the scene every row of `css-math.json` was measured on.
fn filtered(font: Option<&str>, css: &str) -> Result<Vec<u8>, Error> {
    let mut canvas = Canvas::new(200.0, 80.0);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    if let Some(font) = font {
        assert!(ctx.set_font_css(font), "{font:?} is a font");
    }
    ctx.set_filter_css(css)?;
    ctx.set_fill_style_css("rgb(40,90,20)").expect("a colour");
    ctx.fill_rect(60.0, 20.0, 60.0, 30.0);
    ctx.set_fill_style_css("rgba(200,50,160,0.6)")
        .expect("a colour");
    ctx.fill_rect(90.0, 35.0, 50.0, 30.0);
    Ok(ctx
        .get_image_data(0.0, 0.0, 200.0, 80.0)
        .expect("readback")
        .pixels()
        .to_vec())
}

#[test]
fn set_filter_css_answers_the_math_table() {
    oswald();
    let cases = cases("css-math");
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let css = case["css"].as_str().expect("each case names its css");
            let font = case.get("font").and_then(Value::as_str);
            let got = filtered(font, css);
            match case.get("draws").and_then(Value::as_str) {
                None => got
                    .is_ok()
                    .then(|| format!("{css:?}: taken, Chrome ignores it")),
                Some(draws) => match got {
                    Err(error) => Some(format!("{css:?}: {error}")),
                    Ok(pixels) => (pixels
                        != filtered(font, draws).expect("the plain filter"))
                    .then(|| format!("{css:?}: does not draw as {draws:?}")),
                },
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with Chrome:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

/// `Affine` holds `f32`; the table holds what Chrome computed in `f64`. Eight
/// `f32` ulps at the largest magnitude in the table, about 35.
const TOLERANCE: f64 = 3e-5;

#[test]
fn affine_from_css_answers_the_transform_table() {
    let cases = cases("css-transforms");
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let css = case["css"].as_str().expect("each case names its css");
            let got = Affine::from_css(css)
                .map(|m| [m.a, m.b, m.c, m.d, m.tx, m.ty].map(f64::from));
            match (case.get("matrix"), got) {
                (Some(want), Some(got)) => {
                    let want: Vec<f64> = want
                        .as_array()
                        .expect("a matrix is an array")
                        .iter()
                        .map(|v| v.as_f64().expect("a number"))
                        .collect();
                    let close = want
                        .iter()
                        .zip(got)
                        .all(|(want, got)| (want - got).abs() <= TOLERANCE);
                    (!close)
                        .then(|| format!("{css:?}: {got:?}, Chrome {want:?}"))
                }
                (Some(want), None) => {
                    Some(format!("{css:?}: refused, Chrome {want}"))
                }
                (None, Some(got)) => {
                    Some(format!("{css:?}: read as {got:?}, Chrome refuses it"))
                }
                (None, None) => None,
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with Chrome:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

/// The `font-stretch` keywords the table's `stretch` column holds, as the
/// variants they name.
const STRETCHES: [(&str, FontStretch); 9] = [
    ("ultra-condensed", FontStretch::UltraCondensed),
    ("extra-condensed", FontStretch::ExtraCondensed),
    ("condensed", FontStretch::Condensed),
    ("semi-condensed", FontStretch::SemiCondensed),
    ("normal", FontStretch::Normal),
    ("semi-expanded", FontStretch::SemiExpanded),
    ("expanded", FontStretch::Expanded),
    ("extra-expanded", FontStretch::ExtraExpanded),
    ("ultra-expanded", FontStretch::UltraExpanded),
];

/// Chrome reports a size to six significant digits, `2cm` as `75.5906px`;
/// a size read here is the `f32` it came from. Relative, so `8.33333` and
/// `192` are held to the same number of digits.
const FONT_SIZE_TOLERANCE: f32 = 1e-5;

/// What `Font::from_css` read, set against Chrome's answer taken apart, or
/// `None` when every part agrees.
fn font_parts_disagree(font: &Font, parts: &Value) -> Option<String> {
    let size = parts["size"].as_f64().expect("a size") as f32;
    let weight = parts["weight"].as_u64().expect("a weight");
    let italic = parts["style"] == "italic";
    let families: Vec<&str> = parts["families"]
        .as_array()
        .expect("a family list")
        .iter()
        .map(|family| family.as_str().expect("a family name"))
        .collect();
    let stretch = STRETCHES
        .iter()
        .find(|(name, _)| parts["stretch"] == *name)
        .map(|&(_, stretch)| stretch)
        .expect("a stretch keyword");
    // Absent where the case does not say, and `null` for `normal`.
    let line_height = parts
        .get("lineHeight")
        .map(|value| value.as_f64().map(|px| px as f32));

    let agrees = (font.size - size).abs()
        <= FONT_SIZE_TOLERANCE * size.max(1.0)
        && u64::from(font.weight) == weight
        && (font.slant != FontSlant::Normal) == italic
        && font.families == families
        && font.stretch == stretch
        && line_height.is_none_or(|want| font.line_height == want);
    (!agrees).then(|| {
        format!(
            "{} {} {:?} {:?} {:?} line height {:?}",
            font.size,
            font.weight,
            font.slant,
            font.families,
            font.stretch,
            font.line_height
        )
    })
}

/// The caps keyword a context reports, in the table's spelling.
fn caps_name(caps: FontVariantCaps) -> String {
    match caps {
        FontVariantCaps::Normal => "normal".into(),
        FontVariantCaps::SmallCaps => "small-caps".into(),
        other => format!("{other:?}"),
    }
}

#[test]
fn font_from_css_answers_the_font_table() {
    let cases = cases("css-fonts");
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let css = case["css"].as_str().expect("each case names its css");
            let parsed = Font::parse(css);
            let read = Font::from_css(css);
            if parsed.is_ok() != read.is_some() {
                return Some(format!("{css:?}: `parse` and `from_css` differ"));
            }
            match (case.get("parts"), read) {
                (Some(parts), Some(font)) => font_parts_disagree(&font, parts)
                    .map(|got| {
                        format!("{css:?}: read as {got}, Chrome {parts}")
                    }),
                (Some(parts), None) => {
                    Some(format!("{css:?}: refused, Chrome {parts}"))
                }
                (None, Some(font)) => Some(format!(
                    "{css:?}: read as {font:?}, Chrome ignores it"
                )),
                (None, None) => None,
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with Chrome:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn set_font_css_answers_the_font_table() {
    /// Set before each case, so a refusal shows as all of it surviving and a
    /// font taken shows the caps and stretch reset, as Chrome's setter
    /// resets them.
    const SENTINEL: &str = "small-caps ultra-expanded 7px Sentinel";
    /// The sentinel as `font` reads it back: Chrome's form, with no stretch.
    const SENTINEL_READ: &str = "small-caps 7px Sentinel";

    let cases = cases("css-fonts");
    let ignored =
        format!("{SENTINEL_READ:?}, caps small-caps, stretch UltraExpanded");
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let css = case["css"].as_str().expect("each case names its css");
            assert!(ctx.set_font_css(SENTINEL), "the sentinel is a font");
            let taken = ctx.set_font_css(css);
            let got = format!(
                "{:?}, caps {}, stretch {:?}",
                ctx.font(),
                caps_name(ctx.font_variant_caps()),
                ctx.font_stretch()
            );
            match case.get("font") {
                Some(font) => {
                    let stretch = STRETCHES
                        .iter()
                        .find(|(name, _)| case["stretch"] == *name)
                        .map(|&(_, stretch)| stretch)
                        .expect("a stretch keyword");
                    let want = format!(
                        "{font}, caps {}, stretch {stretch:?}",
                        case["variantCaps"].as_str().expect("a caps keyword")
                    );
                    (!taken || got != want).then(|| {
                        format!("{css:?}: taken {taken}, {got}; Chrome {want}")
                    })
                }
                None => (taken || got != ignored).then(|| {
                    format!("{css:?}: taken {taken}, {got}; Chrome ignores it")
                }),
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with Chrome:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn set_filter_css_answers_the_filter_table() {
    let cases = cases("css-filters");
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let css = case["css"].as_str().expect("each case names its css");
            let refused = case.get("refused").is_some();
            match (ctx.set_filter_css(css), refused) {
                (Ok(()), true) => {
                    Some(format!("{css:?}: taken, Chrome ignores it"))
                }
                // Chrome reads every filter it takes back as it was written.
                (Ok(()), false) if ctx.filter() != css => Some(format!(
                    "{css:?}: reads back {:?}, Chrome as written",
                    ctx.filter()
                )),
                (Err(error), false) => Some(format!("{css:?}: {error}")),
                _ => None,
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with Chrome:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn the_spacing_setters_answer_the_spacing_table() {
    /// The canvas's default font size, which `em` and `rem` resolve
    /// against here since no font is set.
    const DEFAULT_FONT_PX: f32 = 10.0;
    /// How far a spacing may sit from Chrome's, which lays text out in `f32`
    /// and reported `23.1199951` for an `ex` of 23.12.
    const TOLERANCE: f32 = 1e-3;

    oswald();
    let cases = cases("css-spacing");
    let mut wrong = Vec::new();
    for case in &cases {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        if let Some(font) = case.get("font").and_then(Value::as_str) {
            assert!(ctx.set_font_css(font), "{font:?} is a font");
        }
        let css = case["css"].as_str().expect("each case names its css");
        // Chrome's reading in pixels: measured where the row gives a font,
        // and otherwise a number and a unit, `em` and `rem` of the default.
        let want = match case.get("px").and_then(Value::as_f64) {
            Some(px) => Some(px as f32),
            None => case["read"].as_str().map(|read| {
                let (number, per) = match read.strip_suffix("rem") {
                    Some(number) => (number, DEFAULT_FONT_PX),
                    None => match read.strip_suffix("em") {
                        Some(number) => (number, DEFAULT_FONT_PX),
                        None => (
                            read.strip_suffix("px").expect("a px reading"),
                            1.0,
                        ),
                    },
                };
                number.parse::<f32>().expect("a number") * per
            }),
        };
        let letter = ctx
            .set_letter_spacing_css(css)
            .map(|()| ctx.letter_spacing());
        let word = ctx.set_word_spacing_css(css).map(|()| ctx.word_spacing());
        for (property, got) in [("letter", letter), ("word", word)] {
            let agrees = match (&got, want) {
                (Ok(got), Some(want)) => (got - want).abs() < TOLERANCE,
                (Err(_), None) => true,
                _ => false,
            };
            if !agrees {
                wrong.push(format!(
                    "{property} spacing {css:?} in {:?}: {got:?}, Chrome {want:?}",
                    case.get("font")
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} disagreements with Chrome over {} cases:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn a_filter_resolves_its_root_units_against_a_16px_serif_root() {
    // Chrome resolves them against the document root, 16px Times by
    // default. With no document the root is CSS's initial one, in the
    // generic `serif` family -- so these follow whatever face `serif` is
    // here, and are pinned against that rather than against Chrome's Times.
    oswald();
    for (root, local) in [("rex", "ex"), ("rch", "ch"), ("rcap", "cap")] {
        let got = filtered(Some("40px Oswald"), &format!("blur(1{root})"));
        let want = filtered(Some("16px serif"), &format!("blur(1{local})"));
        assert_eq!(
            got.expect("taken"),
            want.expect("taken"),
            "{root} at 40px Oswald draws as {local} at 16px serif"
        );
    }
}
