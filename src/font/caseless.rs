//! Unicode's Default Caseless Matching, which CSS Fonts 4 section 5.1 names
//! for font family names.
//!
//! Two strings match when their full case foldings are equal (The Unicode
//! Standard, section 3.13, D144): the mappings `CaseFolding.txt` gives the
//! statuses C and F, so `Fuß` matches `FUSS` and `ΣΑΣ` matches `σας`. S is
//! the simple folding F replaces, and T the Turkic one a default match leaves
//! out. Nothing is normalised, as D144 normalises nothing: a precomposed
//! letter does not match its decomposed spelling.

// Generated, and checked against its source by a test; rustfmt would rewrap
// the licence it carries.
#[rustfmt::skip]
mod folding;

use std::borrow::Cow;

use folding::{FULL, SIMPLE};

/// The full case folding of `c`: up to three characters where status F
/// expands it, otherwise one.
///
/// A code point has a C mapping or an F one, never both, so the second
/// search runs only when the first finds nothing.
fn fold(c: char) -> impl Iterator<Item = char> {
    let full = FULL
        .binary_search_by(|(from, _)| from.cmp(&c))
        .ok()
        .map(|at| FULL[at].1)
        .unwrap_or_default();
    let simple = full.is_empty().then(|| {
        SIMPLE
            .binary_search_by(|(from, _)| from.cmp(&c))
            .map_or(c, |at| SIMPLE[at].1)
    });
    full.iter().copied().chain(simple)
}

/// The full case folding of `text`, character by character.
fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(fold)
}

/// Whether `a` and `b` match by Default Caseless Matching.
///
/// Two ASCII strings compare without the table, since folding ASCII is
/// lower-casing it, and family names are almost always ASCII.
pub(crate) fn eq(a: &str, b: &str) -> bool {
    match a.is_ascii() && b.is_ascii() {
        true => a.eq_ignore_ascii_case(b),
        false => folded(a).eq(folded(b)),
    }
}

/// `text` case-folded: two keys are equal exactly where [`eq`] matches the
/// strings they came from. Borrowed where `text` is already its own folding.
pub(crate) fn key(text: &str) -> Cow<'_, str> {
    let unchanged = match text.is_ascii() {
        true => !text.bytes().any(|b| b.is_ascii_uppercase()),
        false => folded(text).eq(text.chars()),
    };
    match (unchanged, text.is_ascii()) {
        (true, _) => Cow::Borrowed(text),
        (false, true) => Cow::Owned(text.to_ascii_lowercase()),
        (false, false) => Cow::Owned(folded(text).collect()),
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    /// The Unicode Character Database file the table is generated from.
    const CASE_FOLDING: &str =
        include_str!("../../tests/assets/unicode/CaseFolding.txt");

    /// The licence `CASE_FOLDING` is distributed under, which the table
    /// carries because the crate ships it and not the file.
    const LICENSE: &str =
        include_str!("../../tests/assets/unicode/LICENSE.txt");

    /// Set to write `folding.rs` again from `CASE_FOLDING`.
    const REGENERATE: &str = "MEO_SKIA_CANVAS_REGENERATE";

    /// `folding.rs` as generated from [`CASE_FOLDING`].
    fn generated() -> String {
        let version = CASE_FOLDING
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("# CaseFolding-"))
            .and_then(|line| line.strip_suffix(".txt"))
            .expect("the file names its version on its first line");
        let rows: Vec<(u32, &str, Vec<u32>)> = CASE_FOLDING
            .lines()
            .filter_map(|line| line.split('#').next())
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let fields: Vec<&str> =
                    line.split(';').map(str::trim).collect();
                let hex = |text: &str| {
                    u32::from_str_radix(text, 16).expect("a hex code point")
                };
                let to = fields[2].split(' ').map(hex).collect();
                (hex(fields[0]), fields[1], to)
            })
            .collect();
        let literal = |code: u32| format!("'\\u{{{code:x}}}'");
        let of = |status: &'static str| {
            rows.iter().filter(move |row| row.1 == status)
        };
        let simple: Vec<String> = of("C")
            .map(|(from, _, to)| {
                format!("    ({}, {}),", literal(*from), literal(to[0]))
            })
            .collect();
        let full: Vec<String> = of("F")
            .map(|(from, _, to)| {
                let to: Vec<String> = to.iter().map(|&c| literal(c)).collect();
                format!("    ({}, &[{}]),", literal(*from), to.join(", "))
            })
            .collect();
        let license: Vec<String> = LICENSE
            .lines()
            .map(|line| format!("// {line}").trim_end().to_string())
            .collect();
        format!(
            "// Unicode {version}'s case foldings, generated from\n\
             // tests/assets/unicode/CaseFolding.txt by\n\
             // `font::caseless::tests::the_table_is_the_files`, which fails\n\
             // when the two differ. Run it with {REGENERATE} set to write\n\
             // this file again.\n\
             //\n\
             {}\n\
             \n\
             /// Status C: the folding that maps a character to one other.\n\
             pub(super) const SIMPLE: [(char, char); {}] = [\n{}\n];\n\
             \n\
             /// Status F: the full folding that expands a character to two\n\
             /// or three.\n\
             pub(super) const FULL: [(char, &[char]); {}] = [\n{}\n];\n",
            license.join("\n"),
            simple.len(),
            simple.join("\n"),
            full.len(),
            full.join("\n"),
        )
    }

    #[test]
    fn the_table_is_the_files() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/font/caseless/folding.rs"
        );
        let expected = generated();
        if env::var_os(REGENERATE).is_some() {
            fs::write(path, &expected).expect("folding.rs is writable");
        }
        assert!(
            include_str!("caseless/folding.rs") == expected,
            "src/font/caseless/folding.rs differs from CaseFolding.txt; run \
             this test with {REGENERATE} set"
        );
    }

    #[test]
    fn the_tables_are_sorted() {
        // A binary search over an unsorted table misses entries in silence.
        assert!(SIMPLE.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(FULL.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn names_match_by_their_full_folding() {
        for (a, b) in [
            ("Oswald", "OSWALD"),
            ("Fuß", "FUSS"),
            ("ΣΑΣ", "σας"),
            ("Ǆemal", "ǆEMAL"),
            ("ﬃ", "FFI"),
            ("Ꭰ", "ꭰ"),
            ("𐐀", "𐐨"),
        ] {
            assert!(eq(a, b), "{a} matches {b}");
            assert_eq!(key(a), key(b), "{a} and {b} share a key");
        }
    }

    #[test]
    fn a_key_borrows_a_name_that_is_already_folded() {
        for name in ["oswald", "σίσυφοσ", "ırıs"] {
            assert!(matches!(key(name), Cow::Borrowed(_)), "{name}");
        }
        // `ß` folds to `ss` and a final `ς` to `σ`: neither is its own
        // folding.
        for name in ["straße", "σίσυφος", "Oswald"] {
            assert!(matches!(key(name), Cow::Owned(_)), "{name}");
        }
        assert_eq!(key("Oswald"), "oswald");
        assert_eq!(key("Straße"), "strasse");
        assert_eq!(key("σίσυφος"), "σίσυφοσ");
    }

    #[test]
    fn names_that_differ_do_not_match() {
        for (a, b) in [
            ("Oswald", "Oswald2"),
            ("Fuß", "FUS"),
            // Turkic dotless and dotted i: status T, left out of a default
            // match.
            ("I", "ı"),
            ("İ", "i"),
            // Not normalised: é precomposed and e with a combining acute.
            ("é", "e\u{301}"),
        ] {
            assert!(!eq(a, b), "{a} does not match {b}");
            assert_ne!(key(a), key(b), "{a} and {b} keep their keys apart");
        }
    }
}
