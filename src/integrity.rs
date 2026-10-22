use std::{convert::Infallible, fmt, str::FromStr};

use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha384, Sha512};

/// A hash function Subresource Integrity metadata can name, ordered from
/// weakest to strongest.
///
/// These are the three the Subresource Integrity specification requires a
/// reader to support. Metadata naming any other -- `md5`, `sha1` -- is
/// skipped rather than refused, as it is everywhere, so that a stronger hash
/// added later does not break an older reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntegrityAlgorithm {
    /// SHA-256, written `sha256-...`.
    Sha256,
    /// SHA-384, written `sha384-...`.
    Sha384,
    /// SHA-512, written `sha512-...`.
    Sha512,
}

impl IntegrityAlgorithm {
    /// Every algorithm, weakest first.
    const ALL: [Self; 3] = [Self::Sha256, Self::Sha384, Self::Sha512];

    /// The name metadata spells this algorithm with, in lower case.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    /// The algorithm a metadata item names, compared ASCII
    /// case-insensitively.
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|algorithm| name.eq_ignore_ascii_case(algorithm.name()))
    }

    /// The digest of `bytes`, in padded base64.
    fn digest(self, bytes: &[u8]) -> String {
        match self {
            Self::Sha256 => STANDARD.encode(Sha256::digest(bytes)),
            Self::Sha384 => STANDARD.encode(Sha384::digest(bytes)),
            Self::Sha512 => STANDARD.encode(Sha512::digest(bytes)),
        }
    }
}

impl fmt::Display for IntegrityAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Subresource Integrity metadata -- the value of `RequestInit.integrity` or
/// an `integrity` attribute -- parsed into the hashes it is checked against.
///
/// Parsing never fails. Metadata with nothing this library can check in it --
/// an empty string, whitespace, only `md5-...` -- parses to an `Integrity`
/// that [`is_empty`](Self::is_empty) and accepts any bytes, as `fetch` does.
///
/// ```
/// use meo_skia_canvas::Integrity;
///
/// let integrity =
///     Integrity::parse("sha256-nlgC+4KVs6Jc6ADYWC8hYNkDj10yf0X+S6PQWqJF+/s=");
/// assert!(integrity.verify(b"body 14").is_ok());
/// assert!(integrity.verify(b"body 15").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Integrity {
    /// The strongest algorithm the metadata names, or `None` when it names
    /// none this library knows.
    algorithm: Option<IntegrityAlgorithm>,
    /// That algorithm's expected digests, in the form `comparable` gives.
    digests: Vec<String>,
}

impl Integrity {
    /// Parses `metadata`: the Subresource Integrity specification's "parse
    /// metadata", except where its text, Chrome and Node's `fetch` do not all
    /// agree. There, each rule below says which it follows and why.
    ///
    /// - Items are separated by any ASCII whitespace, as in Chrome. The
    ///   specification strictly splits on U+0020 alone, which Node's `fetch`
    ///   does; but its own examples put one hash per line, and a strict split
    ///   reads the line break as part of the first hash.
    /// - Anything from a `?` on is an option, and none are defined, so it is
    ///   dropped.
    /// - The value is everything after the first `-`, as in Chrome and Node.
    ///   The specification splits on every `-` and keeps only the second part,
    ///   which cuts a base64url value -- a form its grammar admits -- short.
    /// - The algorithm is compared ASCII case-insensitively, as the
    ///   specification says, where Chrome and Node compare it exactly and so
    ///   read `SHA512-...` as an algorithm they do not know and skip the check.
    ///
    /// A known algorithm with no value is kept with an empty one, as the
    /// specification's steps say, and can never match. Chrome instead skips
    /// such an item, and an item whose value holds a character no encoding
    /// uses, and so loads bytes it never checked; this refuses them.
    ///
    /// Only the hashes of the strongest algorithm given are kept, since only
    /// those are ever compared.
    pub fn parse(metadata: &str) -> Self {
        let items: Vec<(IntegrityAlgorithm, &str)> = metadata
            .split(|c: char| c.is_ascii_whitespace())
            .filter_map(|item| {
                let expression =
                    item.split_once('?').map_or(item, |(before, _)| before);
                let (name, value) =
                    expression.split_once('-').unwrap_or((expression, ""));
                IntegrityAlgorithm::from_name(name)
                    .map(|algorithm| (algorithm, value))
            })
            .collect();
        let algorithm = items.iter().map(|&(algorithm, _)| algorithm).max();
        let digests = items
            .into_iter()
            .filter(|&(item, _)| Some(item) == algorithm)
            .map(|(_, value)| comparable(value))
            .collect();
        Self { algorithm, digests }
    }

    /// Whether the metadata names no algorithm this library knows, so that
    /// [`verify`](Self::verify) accepts any bytes.
    pub fn is_empty(&self) -> bool {
        self.algorithm.is_none()
    }

    /// The strongest algorithm the metadata names -- the only one
    /// [`verify`](Self::verify) checks -- or `None` when it names none this
    /// library knows.
    pub fn algorithm(&self) -> Option<IntegrityAlgorithm> {
        self.algorithm
    }

    /// Checks `bytes` against the metadata: the specification's "do bytes
    /// match metadataList". The bytes pass when their digest under the
    /// strongest algorithm given equals any of that algorithm's hashes, and
    /// always pass when the metadata [`is_empty`](Self::is_empty).
    ///
    /// Hashes may be written in base64 or base64url, with or without
    /// padding. The specification compares exact base64, but its grammar
    /// admits both encodings -- CSP, which defines that grammar, treats them
    /// as equivalent -- and Chrome and Node accept either. The comparison is
    /// otherwise exact, so a hash with its case changed does not match.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), IntegrityError> {
        let Some(algorithm) = self.algorithm else {
            return Ok(());
        };
        let actual = algorithm.digest(bytes);
        let wanted = comparable(&actual);
        if self.digests.contains(&wanted) {
            Ok(())
        } else {
            Err(IntegrityError { algorithm, actual })
        }
    }
}

impl FromStr for Integrity {
    type Err = Infallible;

    /// [`Integrity::parse`], which cannot fail.
    fn from_str(metadata: &str) -> Result<Self, Infallible> {
        Ok(Self::parse(metadata))
    }
}

/// A digest as it is compared: base64url's `-` and `_` read as base64's `+`
/// and `/`, and trailing padding dropped.
fn comparable(digest: &str) -> String {
    digest
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect()
}

/// Bytes whose digest is not among the hashes their [`Integrity`] names.
///
/// Displays as `its sha256 digest is "...", which is not among the sha256
/// hashes given`, for a caller to prefix with what was being checked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IntegrityError {
    /// The algorithm checked: the strongest the metadata named.
    pub algorithm: IntegrityAlgorithm,
    /// The bytes' digest under that algorithm, in padded base64 -- what the
    /// metadata would have to say for them to pass.
    pub actual: String,
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { algorithm, actual } = self;
        write!(
            f,
            "its {algorithm} digest is \"{actual}\", which is not among the {algorithm} hashes given"
        )
    }
}

impl std::error::Error for IntegrityError {}
