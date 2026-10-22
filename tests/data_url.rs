//! `decode_data_url` against the cases in `tests/assets/data-url-cases.json`,
//! each the bytes Chrome fetched from a `data:` URL or its refusal.

use meo_skia_canvas::{DataUrlError, decode_data_url};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    url: String,
    chrome: Option<String>,
}

fn fixture() -> Fixture {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/data-url-cases.json"
    );
    let text = std::fs::read_to_string(path).expect("the data: URL fixture");
    serde_json::from_str(&text).expect("the data: URL fixture parses")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn every_case_decodes_to_the_bytes_chrome_fetched() {
    let fixture = fixture();
    let wrong: Vec<String> = fixture
        .cases
        .iter()
        .filter_map(|case| {
            let got = decode_data_url(&case.url).ok().map(|bytes| hex(&bytes));
            (got != case.chrome).then(|| {
                format!("{:?}: {got:?}, Chrome {:?}", case.url, case.chrome)
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} cases wrong:\n{}",
        wrong.len(),
        fixture.cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn the_fixture_holds_refusals_and_empty_bodies() {
    // A table every row of which decodes would pass against a decoder that
    // never refuses, and one with no empty body against one that refuses
    // those.
    let fixture = fixture();
    assert!(
        fixture
            .cases
            .iter()
            .filter(|case| case.chrome.is_none())
            .count()
            >= 5
    );
    assert!(
        fixture
            .cases
            .iter()
            .any(|case| case.chrome.as_deref() == Some(""))
    );
}

#[test]
fn a_refusal_says_which_part_is_wrong() {
    assert_eq!(
        decode_data_url("https://example.com/a.png"),
        Err(DataUrlError::NotData)
    );
    assert_eq!(
        decode_data_url("data:image/png;base64"),
        Err(DataUrlError::MissingComma)
    );
    assert_eq!(
        decode_data_url("data:;base64,a"),
        Err(DataUrlError::InvalidBase64)
    );
    assert_eq!(
        DataUrlError::InvalidBase64.to_string(),
        "its body is marked `;base64` and is not valid base64"
    );
}
