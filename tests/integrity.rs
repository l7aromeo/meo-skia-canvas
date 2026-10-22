//! Subresource Integrity against the cases in `tests/assets/sri-cases.json`.
//!
//! The JavaScript suite reads the same file, so the two surfaces answer every
//! case from one table rather than from two copies that can drift. Each case
//! records what Chrome 152 did with the metadata as well as what this library
//! does; where those differ, the case says why, and the test below holds the
//! count of such rows so that a new divergence has to be written down.

use meo_skia_canvas::{Integrity, IntegrityAlgorithm};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    body: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    metadata: String,
    chrome: String,
    expect: String,
    why: Option<String>,
}

fn fixture() -> Fixture {
    let path =
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/assets/sri-cases.json");
    let text = std::fs::read_to_string(path).expect("the shared SRI fixture");
    serde_json::from_str(&text).expect("the shared SRI fixture parses")
}

/// "load" or "refuse", as the fixture spells an outcome.
fn outcome(integrity: &Integrity, body: &[u8]) -> &'static str {
    match integrity.verify(body) {
        Ok(()) => "load",
        Err(_) => "refuse",
    }
}

#[test]
fn every_shared_case_loads_or_refuses_as_the_fixture_says() {
    let fixture = fixture();
    let wrong: Vec<String> = fixture
        .cases
        .iter()
        .filter_map(|case| {
            let got = outcome(
                &Integrity::parse(&case.metadata),
                fixture.body.as_bytes(),
            );
            (got != case.expect).then(|| {
                format!("{:?}: {got}, expected {}", case.name, case.expect)
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
fn every_departure_from_chrome_says_why() {
    let fixture = fixture();
    let unexplained: Vec<&str> = fixture
        .cases
        .iter()
        .filter(|case| case.expect != case.chrome && case.why.is_none())
        .map(|case| case.name.as_str())
        .collect();
    assert!(
        unexplained.is_empty(),
        "differ from Chrome with no `why`: {unexplained:?}"
    );
    // Seven outcomes differ from Chrome; an eighth case loads in both, for a
    // different reason, and says so.
    let differing = fixture
        .cases
        .iter()
        .filter(|case| case.expect != case.chrome)
        .count();
    assert_eq!(differing, 7);
}

#[test]
fn the_fixture_covers_both_outcomes_on_both_sides() {
    // A table in which every row loads would pass against a check that never
    // refuses anything.
    let fixture = fixture();
    for side in ["load", "refuse"] {
        assert!(
            fixture.cases.iter().any(|case| case.expect == side),
            "no case expects {side}"
        );
        assert!(
            fixture.cases.iter().any(|case| case.chrome == side),
            "no Chrome result is {side}"
        );
    }
}

#[test]
fn a_refusal_names_the_strongest_algorithm_and_the_padded_digest() {
    // The SHA-512 of "body 14", from `openssl dgst -sha512 -binary | openssl
    // base64 -A`.
    let sha512 = "Z0UKqUh+C/Ed0Bfuopkjn7nwYcE/cIe9j6uEnaE79A1ItVS6qCy27GlhXKDalmq9iEHRFHaCnsmeI4yzHM6RiA==";
    let integrity = Integrity::parse(
        "sha256-nlgC+4KVs6Jc6ADYWC8hYNkDj10yf0X+S6PQWqJF+/s= sha512-AAAA",
    );
    assert_eq!(integrity.algorithm(), Some(IntegrityAlgorithm::Sha512));
    let error = integrity
        .verify(b"body 14")
        .expect_err("only the strongest algorithm is checked");
    assert_eq!(error.algorithm, IntegrityAlgorithm::Sha512);
    assert_eq!(error.actual, sha512);
    assert_eq!(
        error.to_string(),
        format!(
            "its sha512 digest is \"{sha512}\", which is not among the sha512 hashes given"
        )
    );
}

#[test]
fn metadata_naming_no_known_algorithm_checks_nothing() {
    for metadata in [
        "",
        " \t\n",
        "md5-AAAA",
        "sha1-FE18wGDgXQoHBcd5gXQL7sU0Ev4=",
        "notahash",
        "sha-256-AAAA",
    ] {
        let integrity = Integrity::parse(metadata);
        assert!(integrity.is_empty(), "{metadata:?}");
        assert_eq!(integrity.algorithm(), None, "{metadata:?}");
        assert_eq!(
            integrity.verify(b"anything at all"),
            Ok(()),
            "{metadata:?}"
        );
    }
    // A known algorithm with no value is not "no metadata": it can never match.
    let empty_value = Integrity::parse("sha256-");
    assert!(!empty_value.is_empty());
    assert!(empty_value.verify(b"").is_err());
}

#[test]
fn parsing_through_from_str_is_parse() {
    let metadata = "sha384-uq+v7cOQqJmAKq+xkY/iplqwd+h0AcskvZ9vun8s/zsMFbiyushdwG4YAiDcxMYo";
    let parsed: Integrity =
        metadata.parse().unwrap_or_else(|never| match never {});
    assert_eq!(parsed, Integrity::parse(metadata));
    assert_eq!(parsed.algorithm(), Some(IntegrityAlgorithm::Sha384));
}

#[test]
fn algorithms_are_named_as_metadata_spells_them_and_ordered_by_strength() {
    use IntegrityAlgorithm::*;
    assert_eq!(
        [Sha256, Sha384, Sha512].map(|a| a.to_string()),
        ["sha256", "sha384", "sha512"]
    );
    assert!(Sha256 < Sha384 && Sha384 < Sha512);
}

#[test]
fn a_hash_counts_only_under_the_algorithm_it_is_written_with() {
    // The right SHA-512 digest, written as a sha256 hash beside a wrong
    // sha512 one. Only the strongest algorithm's hashes are compared, so the
    // bytes are refused even though the digest they produce is present.
    let sha512 = "Z0UKqUh+C/Ed0Bfuopkjn7nwYcE/cIe9j6uEnaE79A1ItVS6qCy27GlhXKDalmq9iEHRFHaCnsmeI4yzHM6RiA==";
    let integrity = Integrity::parse(&format!("sha256-{sha512} sha512-AAAA"));
    assert!(integrity.verify(b"body 14").is_err());
    assert!(
        Integrity::parse(&format!("sha512-{sha512}"))
            .verify(b"body 14")
            .is_ok()
    );
}
