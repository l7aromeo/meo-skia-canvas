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
