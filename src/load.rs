use std::fmt;

use base64::{
    Engine,
    alphabet::STANDARD as BASE64_ALPHABET,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use percent_encoding::percent_decode;

#[cfg(feature = "fetch")]
mod fetch;
#[cfg(feature = "fetch")]
pub use fetch::*;

/// Why [`decode_data_url`] refused a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataUrlError {
    /// The URL's scheme is not `data:`.
    NotData,
    /// There is no `,` ending the media type, so there is no body.
    MissingComma,
    /// The body is marked `;base64` and is not valid base64.
    InvalidBase64,
}

impl fmt::Display for DataUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotData => "it is not a data: URL",
            Self::MissingComma => "it has no `,` before its body",
            Self::InvalidBase64 => {
                "its body is marked `;base64` and is not valid base64"
            }
        })
    }
}

impl std::error::Error for DataUrlError {}

/// Decodes a `data:` URL (RFC 2397) to the bytes it holds, as `fetch` does in
/// Chrome. Needs no network and no `fetch` feature: pass the result to
/// [`Image::from_encoded`](crate::Image::from_encoded) for an image.
///
/// The rules are the Fetch Standard's "data: URL processor", measured
/// against Chrome 152 on the cases in `tests/assets/data-url-cases.json`:
///
/// - Leading and trailing C0 controls and spaces are ignored, and the scheme is
///   matched ASCII case-insensitively, so ` DATA:,a` is `a`.
/// - A fragment is not part of the body: `data:,a#b` is `a`.
/// - Everything before the first `,` is the media type, which is not otherwise
///   read; everything after it is the body, percent-decoded. A `%` not followed
///   by two hex digits is kept as written rather than refused, and `%FF` is the
///   byte 0xFF rather than text.
/// - The body is base64 when the media type ends in `;base64`, ASCII
///   case-insensitively and with spaces allowed after the `;`. It is then
///   decoded after percent-decoding, so `%3D` is padding, and ASCII whitespace
///   in it is ignored. Padding is optional, but when given it must be complete:
///   `YQ==` and `YQ` are `a`, and `YQ=` is refused, as is the base64url
///   alphabet.
///
/// Chrome keeps a tab or line break inside the body, which the URL
/// Standard's parser would remove; this follows Chrome.
///
/// ```
/// use meo_skia_canvas::decode_data_url;
///
/// assert_eq!(decode_data_url("data:,hello%20world")?, b"hello world");
/// assert_eq!(decode_data_url("data:text/plain;base64,aGk=")?, b"hi");
/// # Ok::<(), meo_skia_canvas::DataUrlError>(())
/// ```
///
/// # Errors
///
/// [`DataUrlError::NotData`] for any other scheme,
/// [`DataUrlError::MissingComma`] when there is no `,`, and
/// [`DataUrlError::InvalidBase64`] for a base64 body that does not decode.
pub fn decode_data_url(url: &str) -> Result<Vec<u8>, DataUrlError> {
    // The URL parser's trim: C0 controls and U+0020 at either end.
    let url = url.trim_matches(|c: char| c <= ' ');
    let rest = url
        .get(..DATA_SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(DATA_SCHEME))
        .map(|_| &url[DATA_SCHEME.len()..])
        .ok_or(DataUrlError::NotData)?;
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (media_type, body) =
        rest.split_once(',').ok_or(DataUrlError::MissingComma)?;
    let body: Vec<u8> = percent_decode(body.as_bytes()).collect();
    if is_base64(media_type) {
        forgiving_base64(&body)
    } else {
        Ok(body)
    }
}

/// The scheme [`decode_data_url`] takes, compared ASCII case-insensitively.
const DATA_SCHEME: &str = "data:";

/// Whether a `data:` URL's media type marks its body as base64: it ends in
/// `;`, any number of U+0020, and `base64` in any case, once surrounding
/// ASCII whitespace is trimmed -- the Fetch Standard's "data: URL
/// processor".
fn is_base64(media_type: &str) -> bool {
    const MARKER: &str = "base64";
    let media_type = media_type.trim_matches(|c: char| c.is_ascii_whitespace());
    let Some(split) = media_type.len().checked_sub(MARKER.len()) else {
        return false;
    };
    media_type
        .get(split..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(MARKER))
        && media_type[..split].trim_end_matches(' ').ends_with(';')
}

/// Infra's "forgiving-base64 decode": ASCII whitespace is dropped, up to two
/// `=` may end a length that is a multiple of four, and anything else outside
/// the standard alphabet refuses. Bits left over past the last whole byte are
/// discarded rather than checked.
fn forgiving_base64(body: &[u8]) -> Result<Vec<u8>, DataUrlError> {
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &BASE64_ALPHABET,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let mut data: Vec<u8> = body
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if data.len().is_multiple_of(4) {
        for _ in 0..2 {
            if data.last() == Some(&b'=') {
                data.pop();
            }
        }
    }
    // A length one past a multiple of four, which Infra refuses by name, is
    // refused by the engine too: one symbol cannot finish a byte.
    LENIENT
        .decode(&data)
        .map_err(|_| DataUrlError::InvalidBase64)
}
