//! Parsers for the CSS strings the Canvas API takes as property values.
//!
//! The crate's typed setters are the primary way to say these things --
//! [`Context2D::set_filter`](crate::context2d::Context2D::set_filter) takes
//! a slice of [`FilterOp`], and a typo in one is a compile error. The `_css`
//! variants beside them take the string a stylesheet or a JavaScript port
//! would carry, which is the same arrangement colour already had:
//! `set_fill_style` takes an [`RgbaLinear`] and `set_fill_style_css` takes
//! `"red"`, and neither replaced the other.
//!
//! Written here rather than taken from a CSS crate: what is needed is four
//! grammars out of a specification that has hundreds, and a general parser
//! would bring a cascade, a selector engine and a property database for
//! them. The JavaScript side parses the same strings in
//! `lib/classes/css.js`, and this follows it deliberately -- including its
//! unit table -- so a string that works on one surface works on the other.
//!
//! [`RgbaLinear`]: crate::color::RgbaLinear
//! [`FilterOp`]: crate::filter::FilterOp

use crate::{
    color::{RgbaLinear, unpremul_color4f_to_rgba_linear},
    error::Error,
    filter::FilterOp,
    node::utils::css_to_color4f_in_space,
    text::{TextDecoration, TextDecorationStyle},
};

/// Pixels per CSS inch, which every absolute unit below is defined against.
const PX_PER_INCH: f32 = 96.0;

/// Centimetres per inch.
const CM_PER_INCH: f32 = 2.54;

/// Points per inch. A CSS point is a 72nd of an inch, so a pixel is 0.75 of
/// one.
const PT_PER_INCH: f32 = 72.0;

/// Picas per inch. Six, so a pica is 16 pixels.
const PC_PER_INCH: f32 = 6.0;

/// Quarter-millimetres per centimetre, for the `q` unit.
const Q_PER_CM: f32 = 40.0;

/// Degrees in a full turn.
const DEGREES_PER_TURN: f32 = 360.0;

/// Gradians in a full turn.
const GRADIANS_PER_TURN: f32 = 400.0;

/// A length in CSS units, as pixels.
///
/// `em_size` is what `em` and `rem` resolve against -- the font size in
/// play, which is why the caller supplies it rather than this assuming one.
///
/// Returns `None` for a unit CSS does not define, and for a number with no
/// unit at all: CSS requires one on every length but zero, and accepting a
/// bare number here would take `blur(4)` that a browser refuses.
///
/// A percentage is not a length and is refused. It reads like one -- and the
/// JavaScript side's `parseSize` accepts it, which is why `blur(50%)` is
/// honoured there and rejected by Chrome. The properties where a percentage
/// *is* legal are font sizes, and those are parsed by `Font::parse`, not
/// here.
pub(crate) fn parse_size(text: &str, em_size: f32) -> Option<f32> {
    let text = text.trim();
    let (value, unit) = split_number(text)?;

    let per_px = match unit.trim().to_ascii_lowercase().as_str() {
        "px" => 1.0,
        "pt" => PX_PER_INCH / PT_PER_INCH,
        "pc" => PX_PER_INCH / PC_PER_INCH,
        "in" => PX_PER_INCH,
        "cm" => PX_PER_INCH / CM_PER_INCH,
        "mm" => PX_PER_INCH / (CM_PER_INCH * 10.0),
        "q" => PX_PER_INCH / (CM_PER_INCH * Q_PER_CM),
        "em" | "rem" => em_size,
        // Zero is the one length CSS lets go unitless, because zero of
        // anything is the same zero.
        "" if value == 0.0 => 1.0,
        _ => return None,
    };
    Some(value * per_px)
}

/// Splits a CSS dimension into its number and its unit.
///
/// The number's extent is not "every character that could appear in one":
/// `e` both continues an exponent and begins `em`, so `0.5em` split that way
/// gives `0.5e` and `m`, and neither half is anything. An `e` therefore
/// belongs to the number only when a digit follows it, with an optional
/// sign in between -- which is exactly when CSS says it is an exponent.
fn split_number(text: &str) -> Option<(f32, &str)> {
    let bytes = text.as_bytes();
    let mut at = 0;

    if matches!(bytes.first(), Some(b'+' | b'-')) {
        at += 1;
    }
    while matches!(bytes.get(at), Some(c) if c.is_ascii_digit()) {
        at += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while matches!(bytes.get(at), Some(c) if c.is_ascii_digit()) {
            at += 1;
        }
    }
    // The exponent, only where one really follows.
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        let mut after = at + 1;
        if matches!(bytes.get(after), Some(b'+' | b'-')) {
            after += 1;
        }
        if matches!(bytes.get(after), Some(c) if c.is_ascii_digit()) {
            at = after;
            while matches!(bytes.get(at), Some(c) if c.is_ascii_digit()) {
                at += 1;
            }
        }
    }

    let value: f32 = text.get(..at)?.parse().ok()?;
    value
        .is_finite()
        .then(|| (value, text.get(at..).unwrap_or("")))
}

/// A CSS length split into the number, its unit, and its value in pixels.
///
/// The three a [`Spacing`](crate::node::typography::Spacing) is built from.
/// Absolute units resolve here; `em` and `rem` cannot, because what they
/// resolve against changes when the font does -- so their pixel value is
/// left `NaN` and the unit is carried, which is how spacing stays relative
/// after it is set rather than being frozen at the size it was set at.
pub(crate) struct Length {
    /// The number as written.
    pub value: f32,
    /// The unit, lowercased. Empty only for a bare zero.
    pub unit: String,
    /// The length in pixels, or `NaN` for a unit that needs a font size.
    pub pixels: f32,
}

/// A CSS length for a property that keeps its unit.
///
/// Returns `None` for a unit CSS does not define, and for a bare number
/// other than zero. A percentage is refused for the reason
/// [`parse_size`] refuses it: `letter-spacing` is defined over `<length>`.
pub(crate) fn parse_length(text: &str) -> Option<Length> {
    let text = text.trim();
    let (value, unit) = split_number(text)?;
    let unit = unit.trim().to_ascii_lowercase();
    match unit.as_str() {
        // Relative, so the pixel value is not knowable yet.
        "em" | "rem" => Some(Length {
            value,
            unit,
            pixels: f32::NAN,
        }),
        _ => {
            let pixels = parse_size(text, f32::NAN)?;
            pixels.is_finite().then_some(Length {
                value,
                unit,
                pixels,
            })
        }
    }
}

/// A percentage or bare number as a fraction.
///
/// `"150%"` and `"1.5"` both give 1.5, which is what the CSS filter
/// functions accept interchangeably.
pub(crate) fn parse_percentage(text: &str) -> Option<f32> {
    let text = text.trim();
    let value = match text.strip_suffix('%') {
        Some(number) => number.trim().parse::<f32>().ok()? / 100.0,
        None => text.parse::<f32>().ok()?,
    };
    value.is_finite().then_some(value)
}

/// An angle in CSS units, as degrees.
///
/// Returns `None` for a bare number: CSS requires a unit on an angle, and
/// `hue-rotate(45)` is rejected by browsers.
pub(crate) fn parse_angle(text: &str) -> Option<f32> {
    let (value, unit) = split_number(text.trim())?;
    // Case-insensitive: CSS units are, and a browser takes `45DEG`.
    let degrees = match unit.trim().to_ascii_lowercase().as_str() {
        "deg" => value,
        "rad" => value.to_degrees(),
        "grad" => value * DEGREES_PER_TURN / GRADIANS_PER_TURN,
        "turn" => value * DEGREES_PER_TURN,
        _ => return None,
    };
    Some(degrees)
}

/// The four parts a CSS `text-decoration` shorthand can carry.
///
/// Every one is optional and they may appear in any order, which is why
/// this is parsed by classifying each token rather than by position.
pub(crate) struct Decoration {
    /// Which lines are drawn. All false for `none`.
    pub lines: TextDecoration,
    /// How the line is drawn.
    pub style: TextDecorationStyle,
    /// The line's colour, or `None` to follow the fill.
    pub color: Option<RgbaLinear>,
    /// The line's thickness in pixels, or `None` for the font's own.
    pub thickness: Option<f32>,
}

/// A CSS `text-decoration` shorthand.
///
/// Order-insensitive, as CSS is: `"underline wavy red"` and
/// `"red underline wavy"` are the same declaration. A token is classified
/// by what it looks like -- a line keyword, a style keyword, a length, or
/// failing those a colour.
///
/// `"none"` and the empty string clear the decoration, and so do the CSS
/// global keywords, which have no cascade here to inherit through.
///
/// # Errors
///
/// Returns `None` for a token that is none of those things -- most often a
/// colour that does not parse, which is the only way a caller can get this
/// wrong without misspelling a keyword.
pub(crate) fn parse_decoration(text: &str, em_size: f32) -> Option<Decoration> {
    let mut parsed = Decoration {
        lines: TextDecoration::default(),
        style: TextDecorationStyle::Solid,
        color: None,
        thickness: None,
    };

    for token in text.split_whitespace() {
        let lowered = token.to_ascii_lowercase();
        match lowered.as_str() {
            // The global keywords, and `none` itself. Each clears the lines
            // rather than meaning anything of its own: there is no cascade
            // here for `inherit` or `revert` to reach into.
            "none" | "initial" | "inherit" | "unset" | "revert"
            | "revert-layer" => parsed.lines = TextDecoration::default(),
            "underline" => parsed.lines.underline = true,
            "overline" => parsed.lines.overline = true,
            "line-through" => parsed.lines.line_through = true,
            "solid" => parsed.style = TextDecorationStyle::Solid,
            "double" => parsed.style = TextDecorationStyle::Double,
            "dotted" => parsed.style = TextDecorationStyle::Dotted,
            "dashed" => parsed.style = TextDecorationStyle::Dashed,
            "wavy" => parsed.style = TextDecorationStyle::Wavy,
            // `text-decoration-thickness` takes these two alongside a
            // length, and both mean "whatever the font says", which is what
            // `None` already means here.
            "auto" | "from-font" => parsed.thickness = None,
            // Not a colour value but a keyword meaning "the one being
            // painted with", which is what `None` says on this side. It
            // reaches the colour arm otherwise and fails there, since no
            // colour parser resolves it without a fill to resolve against.
            "currentcolor" => parsed.color = None,
            _ => match parse_size(token, em_size) {
                Some(thickness) => parsed.thickness = Some(thickness),
                // Anything left has to be a colour, and a token that is not
                // one is a token nobody can act on.
                None => {
                    let (color, _) = css_to_color4f_in_space(token)?;
                    parsed.color = Some(unpremul_color4f_to_rgba_linear(color));
                }
            },
        }
    }
    Some(parsed)
}

/// Splits `text` on whitespace that is not inside parentheses.
///
/// A filter chain is space-separated and its arguments are too, so a plain
/// split would cut `drop-shadow(2px 2px 4px red)` into four pieces.
fn split_functions(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (at, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => {
                if at > start {
                    parts.push(&text[start..at]);
                }
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// Splits one `name(args)` call into its two halves.
fn call<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// A CSS `filter` value as the chain [`Context2D::set_filter`] takes.
///
/// `"none"` and the empty string are an empty chain, which is how the
/// property spells "no filter".
///
/// [`Context2D::set_filter`]: crate::context2d::Context2D::set_filter
///
/// # Errors
///
/// Returns [`Error::InvalidFilter`] naming the piece that did not parse.
/// The whole string is rejected rather than the recognised parts kept:
/// a chain missing one step is a different picture, and quietly dropping
/// the step nobody could read is how a typo becomes a rendering bug.
pub(crate) fn parse_filter(
    text: &str,
    em_size: f32,
) -> Result<Vec<FilterOp>, Error> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    let refuse = |part: &str| Error::InvalidFilter {
        reason: format!("could not parse {part:?} in filter {text:?}"),
    };

    let mut ops = Vec::new();
    for part in split_functions(trimmed) {
        // `drop-shadow` first: its arguments are themselves space-separated,
        // so it is the one function that cannot share the shape below.
        if let Some(args) = call(part, "drop-shadow") {
            ops.push(drop_shadow(args, em_size).ok_or_else(|| refuse(part))?);
            continue;
        }

        let (name, argument) = part
            .split_once('(')
            .and_then(|(name, rest)| {
                rest.strip_suffix(')').map(|argument| (name, argument))
            })
            .ok_or_else(|| refuse(part))?;

        let percent = || parse_percentage(argument).ok_or_else(|| refuse(part));
        ops.push(match name {
            "blur" => FilterOp::Blur(
                parse_size(argument, em_size).ok_or_else(|| refuse(part))?,
            ),
            "hue-rotate" => FilterOp::HueRotate(
                parse_angle(argument).ok_or_else(|| refuse(part))?,
            ),
            "brightness" => FilterOp::Brightness(percent()?),
            "contrast" => FilterOp::Contrast(percent()?),
            "grayscale" => FilterOp::Grayscale(percent()?),
            "invert" => FilterOp::Invert(percent()?),
            "opacity" => FilterOp::Opacity(percent()?),
            "saturate" => FilterOp::Saturate(percent()?),
            "sepia" => FilterOp::Sepia(percent()?),
            _ => return Err(refuse(part)),
        });
    }
    Ok(ops)
}

/// The lengths and optional colour a `drop-shadow` carries.
///
/// Filter Effects 1 gives the grammar as `<color>? && <length>{2,3}`: two
/// lengths or three, and a colour on either side of them or not at all. The
/// `&&` is what makes the colour's position free.
///
/// This used to scan lengths from the front only and take the colour from a
/// fixed `words[3..]`, so it required exactly three lengths with the colour
/// after them. `drop-shadow(red 2px 2px 4px)` and `drop-shadow(2px 4px red)`
/// were both refused -- and refusing one step rejects the whole chain, so a
/// filter Chrome draws became an error here.
fn drop_shadow(args: &str, em_size: f32) -> Option<FilterOp> {
    let words: Vec<&str> = args.split_whitespace().collect();

    // Both orderings are tried rather than the lengths being located in one
    // pass, because a colour can contain something that parses as a length:
    // `rgb(0 0 0)` splits into three words of which the middle one is a
    // valid CSS zero. Anchoring the run to one end or the other cannot be
    // fooled by that.
    let leading = |words: &[&str]| {
        let lengths: Vec<f32> = words
            .iter()
            .map_while(|word| parse_size(word, em_size))
            .collect();
        let rest = words[lengths.len()..].join(" ");
        (lengths, rest)
    };
    let trailing = |words: &[&str]| {
        let mut lengths: Vec<f32> = words
            .iter()
            .rev()
            .map_while(|word| parse_size(word, em_size))
            .collect();
        lengths.reverse();
        let rest = words[..words.len() - lengths.len()].join(" ");
        (lengths, rest)
    };

    // Colour after the lengths first, since that is the common spelling.
    for (lengths, rest) in [leading(&words), trailing(&words)] {
        if !(2..=3).contains(&lengths.len()) {
            continue;
        }
        let color = match rest.is_empty() {
            // The initial value is `currentColor`, which a canvas resolves
            // to the fill; black is what this crate has always used and what
            // the typed `FilterOp::DropShadow` carries when none is given.
            true => RgbaLinear::opaque(0.0, 0.0, 0.0),
            // Through the same parser `set_fill_style_css` uses, so a colour
            // that works in one place works in the other.
            false => match css_to_color4f_in_space(&rest) {
                Some((color, _)) => unpremul_color4f_to_rgba_linear(color),
                None => continue,
            },
        };
        return Some(FilterOp::DropShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            // The blur radius is the optional one, and its initial value is
            // zero -- a hard-edged shadow.
            blur: lengths.get(2).copied().unwrap_or(0.0),
            color,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The font size CSS relative units resolve against in these tests.
    const EM: f32 = 16.0;

    #[test]
    fn every_absolute_length_unit_converts_to_the_pixels_css_defines() {
        // The whole table, because each is one division and a wrong
        // denominator produces a plausible number rather than a failure.
        for (text, pixels) in [
            ("1px", 1.0),
            ("0", 0.0),
            ("1in", 96.0),
            ("1cm", 96.0 / 2.54),
            ("1mm", 96.0 / 25.4),
            ("1q", 96.0 / 25.4 / 4.0),
            ("1pt", 96.0 / 72.0),
            ("1pc", 16.0),
            ("1em", EM),
            ("1rem", EM),
            // Signs, decimals and exponents all belong to the number.
            ("-4px", -4.0),
            ("+2.5px", 2.5),
            (".5px", 0.5),
            ("1e2px", 100.0),
            // Case-insensitive, as CSS units are.
            ("1PX", 1.0),
            ("1IN", 96.0),
        ] {
            let got = parse_size(text, EM)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert!(
                (got - pixels).abs() < 1e-3,
                "{text:?} gave {got}, expected {pixels}"
            );
        }
    }

    #[test]
    fn a_percentage_is_not_a_length() {
        // It reads like one, and the JavaScript side's `parseSize` takes it
        // -- which is why `blur(50%)` is honoured there and refused by
        // Chrome. CSS defines `blur()` over `<length>`, and a percentage is
        // not one.
        assert_eq!(parse_size("50%", EM), None);
        assert!(parse_filter("blur(50%)", EM).is_err());
    }

    #[test]
    fn a_length_without_a_unit_is_refused_unless_it_is_zero() {
        // CSS requires a unit on every length but zero, and a browser
        // rejects `blur(4)`. Accepting it here would take a filter that
        // does nothing in a browser and make it work in this crate, which
        // is worse than refusing both.
        assert_eq!(parse_size("4", EM), None);
        assert_eq!(parse_size("0", EM), Some(0.0));
        assert_eq!(parse_size("4furlongs", EM), None);
        assert_eq!(parse_size("", EM), None);
        assert_eq!(parse_size("px", EM), None);
    }

    #[test]
    fn an_angle_carries_its_unit_and_its_sign() {
        for (text, degrees) in [
            ("45deg", 45.0),
            ("-45deg", -45.0),
            ("+45deg", 45.0),
            ("1turn", 360.0),
            ("0.5turn", 180.0),
            ("400grad", 360.0),
            ("3.14159265rad", 180.0),
            ("45DEG", 45.0),
        ] {
            let got = parse_angle(text)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert!(
                (got - degrees).abs() < 1e-2,
                "{text:?} gave {got}, expected {degrees}"
            );
        }

        // A bare number is not an angle, and neither is a length.
        assert_eq!(parse_angle("45"), None);
        assert_eq!(parse_angle("45px"), None);
    }

    #[test]
    fn a_percentage_and_a_bare_number_mean_the_same_thing() {
        // Which is true of the filter functions and not of lengths.
        assert_eq!(parse_percentage("150%"), Some(1.5));
        assert_eq!(parse_percentage("1.5"), Some(1.5));
        assert_eq!(parse_percentage("0%"), Some(0.0));
        assert_eq!(parse_percentage("-50%"), Some(-0.5));
        assert_eq!(parse_percentage("half"), None);
    }

    #[test]
    fn a_chain_parses_into_the_ops_it_names() {
        let ops = parse_filter(
            "blur(4px) saturate(150%) hue-rotate(-45deg) opacity(0.5)",
            EM,
        )
        .expect("a well formed chain");
        assert_eq!(ops.len(), 4);
        assert!(
            matches!(ops[0], FilterOp::Blur(px) if (px - 4.0).abs() < 1e-3)
        );
        assert!(
            matches!(ops[1], FilterOp::Saturate(v) if (v - 1.5).abs() < 1e-3)
        );
        // The sign belongs to the number: an unanchored parse took `45`
        // out of `-45deg` and rotated the wrong way, which is the bug the
        // JavaScript side's own comment records.
        assert!(
            matches!(ops[2], FilterOp::HueRotate(d) if (d + 45.0).abs() < 1e-3)
        );
        assert!(
            matches!(ops[3], FilterOp::Opacity(v) if (v - 0.5).abs() < 1e-3)
        );
    }

    #[test]
    fn none_and_nothing_are_an_empty_chain() {
        for text in ["none", "NONE", "", "   "] {
            assert!(
                parse_filter(text, EM).expect("accepted").is_empty(),
                "{text:?}"
            );
        }
    }

    /// `<color>? && <length>{2,3}` -- every spelling the grammar allows.
    ///
    /// The parser scanned lengths from the front and took the colour from a
    /// fixed offset, so it accepted exactly one of these five. A refused
    /// step rejects the whole chain, so the other four were errors here and
    /// pictures in Chrome.
    #[test]
    fn a_drop_shadow_takes_its_colour_from_either_end() {
        let shadow = |css: &str| match parse_filter(css, EM) {
            Ok(ops) => match ops.first() {
                Some(&FilterOp::DropShadow {
                    offset_x,
                    offset_y,
                    blur,
                    color,
                }) => Some((offset_x, offset_y, blur, color)),
                _ => None,
            },
            Err(_) => None,
        };

        let black = RgbaLinear::opaque(0.0, 0.0, 0.0);
        let red = RgbaLinear::from_hex("#ff0000").expect("a literal");

        // Colour last, which is the spelling that already worked.
        assert_eq!(
            shadow("drop-shadow(2px 4px 6px red)"),
            Some((2.0, 4.0, 6.0, red))
        );
        // Colour first. `&&` in the grammar is what makes this legal.
        assert_eq!(
            shadow("drop-shadow(red 2px 4px 6px)"),
            Some((2.0, 4.0, 6.0, red))
        );
        // Blur omitted, which the grammar's `{2,3}` allows. Its initial
        // value is zero: a shadow with a hard edge.
        assert_eq!(
            shadow("drop-shadow(2px 4px red)"),
            Some((2.0, 4.0, 0.0, red))
        );
        assert_eq!(
            shadow("drop-shadow(red 2px 4px)"),
            Some((2.0, 4.0, 0.0, red))
        );
        // No colour at all.
        assert_eq!(
            shadow("drop-shadow(2px 4px 6px)"),
            Some((2.0, 4.0, 6.0, black))
        );
        assert_eq!(
            shadow("drop-shadow(2px 4px)"),
            Some((2.0, 4.0, 0.0, black))
        );

        // A functional colour survives even though it contains a word that
        // parses as a length on its own: the `0` in the middle of
        // `rgb(0 0 0)` is a valid CSS zero. Anchoring the length run to one
        // end is what keeps that from being read as an offset.
        assert_eq!(
            shadow("drop-shadow(rgb(0 0 0) 2px 4px)"),
            Some((2.0, 4.0, 0.0, black))
        );

        // And the grammar still has edges. One length is not two, four is
        // not three, and a colour on both sides is not a colour.
        assert_eq!(shadow("drop-shadow(2px)"), None);
        assert_eq!(shadow("drop-shadow(1px 2px 3px 4px)"), None);
        assert_eq!(shadow("drop-shadow(red 2px 4px blue)"), None);
        assert_eq!(shadow("drop-shadow(nonsense 2px 4px)"), None);
    }

    #[test]
    fn a_drop_shadow_keeps_its_arguments_together() {
        // The one function whose arguments are themselves space-separated,
        // so a plain split on whitespace would cut it into four pieces.
        let ops =
            parse_filter("drop-shadow(2px 4px 6px red)", EM).expect("a shadow");
        assert_eq!(ops.len(), 1);
        match ops[0] {
            FilterOp::DropShadow {
                offset_x,
                offset_y,
                blur,
                ..
            } => {
                assert!((offset_x - 2.0).abs() < 1e-3);
                assert!((offset_y - 4.0).abs() < 1e-3);
                assert!((blur - 6.0).abs() < 1e-3);
            }
            ref other => panic!("expected a drop shadow, got {other:?}"),
        }

        // And one beside other functions still finds its own boundaries.
        let chain = parse_filter(
            "blur(2px) drop-shadow(1px 1px 1px blue) invert(1)",
            EM,
        )
        .expect("a chain");
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn a_relative_length_follows_the_font_size_it_is_given() {
        // What the string form buys over the typed one: `em` cannot be
        // expressed as a fixed pixel count, because it is not one.
        let at = |em| match parse_filter("blur(0.5em)", em) {
            Ok(ops) => match ops[0] {
                FilterOp::Blur(px) => px,
                ref other => panic!("{other:?}"),
            },
            Err(e) => panic!("{e}"),
        };
        assert!((at(16.0) - 8.0).abs() < 1e-3);
        assert!((at(40.0) - 20.0).abs() < 1e-3);
    }

    #[test]
    fn whitespace_around_and_between_pieces_is_tolerated() {
        // A chain arriving from a template or a config file carries
        // whatever spacing it was written with.
        for text in [
            "blur(4px) invert(1)",
            "  blur(4px)   invert(1)  ",
            "blur(4px)\tinvert(1)",
            "blur(4px)\ninvert(1)",
            "blur( 4px ) invert( 1 )",
        ] {
            let ops = parse_filter(text, EM)
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(ops.len(), 2, "{text:?}");
            assert!(
                matches!(ops[0], FilterOp::Blur(px) if (px - 4.0).abs() < 1e-3),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_colour_function_inside_a_shadow_survives_its_own_parentheses() {
        // The nesting case: `rgb(255 0 0)` contains the spaces and the
        // parentheses the outer split is looking for. A depth counter is
        // what keeps the shadow whole, and a colour with commas is the
        // other spelling of the same trap.
        for text in [
            "drop-shadow(1px 2px 3px rgb(255 0 0))",
            "drop-shadow(1px 2px 3px rgba(255, 0, 0, 0.5))",
            "drop-shadow(1px 2px 3px hsl(0 100% 50%))",
            "drop-shadow(1px 2px 3px #ff0000)",
        ] {
            let ops = parse_filter(text, EM)
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(ops.len(), 1, "{text:?}");
            assert!(
                matches!(ops[0], FilterOp::DropShadow { blur, .. }
                    if (blur - 3.0).abs() < 1e-3),
                "{text:?}"
            );
        }

        // And one nested colour does not swallow a following function.
        let chain =
            parse_filter("drop-shadow(1px 2px 3px rgb(0 0 0)) blur(2px)", EM)
                .expect("two steps");
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn malformed_input_is_refused_without_panicking() {
        // A parser reachable from a string a caller did not write has to
        // survive anything. None of these may panic, and none may parse.
        for text in [
            "blur(",
            "blur)",
            "blur()",
            "()",
            "(((",
            ")))",
            "blur(4px",
            "blur 4px)",
            "drop-shadow(",
            "drop-shadow()",
            "drop-shadow(1px)",
            // Not `drop-shadow(1px 2px)`: the grammar is `<length>{2,3}`,
            // so two is a shadow with no blur. This list asserted otherwise
            // for as long as the parser demanded three.
            "drop-shadow(1px 2px 3px 4px)",
            "drop-shadow(1px 2px 3px notacolour)",
            "blur(NaN)",
            "blur(inf)",
            "blur(--4px)",
            "blur(+-4px)",
            "hue-rotate(4.5.6deg)",
            "blur(4px",
            "\u{1f600}",
            "blur(\u{1f600})",
            "blur(4px) \u{0}",
        ] {
            assert!(
                parse_filter(text, EM).is_err(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn the_unit_parsers_survive_anything_handed_to_them() {
        // Same contract one level down, and these are reachable directly
        // from the spacing setters.
        for text in [
            "",
            " ",
            "-",
            "+",
            ".",
            "e",
            "em",
            "px",
            "-.",
            "+e",
            "e5",
            "1e",
            "1e+",
            "1.2.3px",
            "--1px",
            "1px2",
            "\u{1f600}px",
            "999999999999999999999999999999999999999999px",
            "1e400px",
            "-1e400px",
            "NaNpx",
            "infpx",
        ] {
            // No assertion on the answer, only that there is one.
            let _ = parse_size(text, EM);
            let _ = parse_angle(text);
            let _ = parse_percentage(text);
        }

        // The ones with a defined answer.
        assert_eq!(parse_size("1.2.3px", EM), None, "two decimal points");
        assert_eq!(parse_size("--1px", EM), None, "two signs");
        assert_eq!(parse_size("1px2", EM), None, "trailing rubbish");
        assert_eq!(parse_size("1e", EM), None, "an exponent with no digits");
        // An overflowing exponent parses to infinity, which is not a length.
        assert_eq!(parse_size("1e400px", EM), None, "overflows to infinity");
        assert_eq!(parse_percentage("NaN"), None);
        assert_eq!(parse_percentage("inf"), None);
    }

    #[test]
    fn an_exponent_and_the_em_unit_do_not_take_each_other_s_characters() {
        // The bug this parser had: `e` both continues an exponent and
        // begins `em`, so splitting on "characters a number may contain"
        // read `0.5em` as `0.5e` and `m`, and neither half is anything.
        assert_eq!(parse_size("0.5em", 16.0), Some(8.0));
        assert_eq!(parse_size("2em", 16.0), Some(32.0));
        assert_eq!(parse_size("1rem", 16.0), Some(16.0));
        // A real exponent still works, including with a sign.
        assert_eq!(parse_size("1e2px", EM), Some(100.0));
        assert_eq!(parse_size("1e+2px", EM), Some(100.0));
        assert_eq!(parse_size("1e-2px", EM), Some(0.01));
        // And an `e` that begins a unit is a unit, not a broken exponent.
        assert_eq!(parse_size("1ex", EM), None, "ex is not a unit here");
    }

    #[test]
    fn a_function_name_is_matched_exactly() {
        // Neither a prefix nor a suffix of a known name is that name. CSS
        // function names are case-insensitive, which this deliberately is
        // not -- matching the JavaScript side, which lowercases nothing.
        for text in [
            "blu(4px)",
            "blurr(4px)",
            "xblur(4px)",
            "BLUR(4px)",
            "drop-shadows(1px 2px 3px red)",
            "shadow(1px 2px 3px red)",
        ] {
            assert!(
                parse_filter(text, EM).is_err(),
                "{text:?} should not parse"
            );
        }
        assert!(parse_filter("blur(4px)", EM).is_ok());
    }

    #[test]
    fn a_percentage_argument_is_refused_where_a_length_belongs() {
        // `blur` takes a length and `saturate` takes a percentage, and
        // neither accepts the other's form -- which is what stops
        // `blur(50%)` silently blurring by half the font size.
        assert!(parse_filter("blur(50%)", EM).is_err());
        assert!(parse_filter("saturate(4px)", EM).is_err());
        assert!(parse_filter("hue-rotate(50%)", EM).is_err());
        // Though a bare number is a percentage, which CSS allows.
        assert!(parse_filter("saturate(1.5)", EM).is_ok());
    }

    #[test]
    fn a_relative_length_keeps_its_unit_and_an_absolute_one_resolves() {
        // The distinction the type exists for. An absolute length knows its
        // pixels now; a relative one cannot, because what it is relative to
        // has not happened yet.
        let em = parse_length("0.1em").expect("a relative length");
        assert_eq!(em.unit, "em");
        assert!((em.value - 0.1).abs() < 1e-6);
        assert!(em.pixels.is_nan(), "em cannot be pixels yet");

        let rem = parse_length("2rem").expect("also relative");
        assert_eq!(rem.unit, "rem");
        assert!(rem.pixels.is_nan());

        for (text, unit, pixels) in [
            ("2px", "px", 2.0),
            ("1in", "in", 96.0),
            ("1pc", "pc", 16.0),
            ("1pt", "pt", 96.0 / 72.0),
            ("1cm", "cm", 96.0 / 2.54),
            ("1mm", "mm", 96.0 / 25.4),
            ("1q", "q", 96.0 / 25.4 / 4.0),
            ("-3px", "px", -3.0),
            ("0", "", 0.0),
        ] {
            let length = parse_length(text)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert_eq!(length.unit, unit, "{text:?}");
            assert!(
                (length.pixels - pixels).abs() < 1e-3,
                "{text:?} gave {} pixels",
                length.pixels
            );
        }
    }

    #[test]
    fn a_length_for_spacing_refuses_what_css_refuses() {
        // Spacing is defined over `<length>`: no percentages, no bare
        // numbers, no units that do not exist. Same rules as the filter's
        // lengths, reached through a different setter.
        for text in [
            "50%",
            "2",
            "-2",
            "2deg",
            "2s",
            "2fr",
            "px",
            "",
            " ",
            "e",
            "1.2.3px",
            "--1px",
            "1px2",
            "1e400px",
            "NaNpx",
            "\u{1f600}",
        ] {
            assert!(parse_length(text).is_none(), "{text:?} should not parse");
        }
        // Zero alone may go unitless.
        assert!(parse_length("0").is_some());
    }

    #[test]
    fn spacing_units_are_case_insensitive_and_survive_whitespace() {
        for text in ["2PX", " 2px ", "2Px", "0.5EM", "  0.5em"] {
            assert!(parse_length(text).is_some(), "{text:?}");
        }
        assert_eq!(parse_length("0.5EM").expect("relative").unit, "em");
    }

    #[test]
    fn a_decoration_is_read_in_any_order() {
        // CSS is order-insensitive here, so the parser classifies each
        // token rather than reading positions.
        for text in [
            "underline wavy red",
            "red underline wavy",
            "wavy red underline",
            "underline red wavy",
        ] {
            let d = parse_decoration(text, EM)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert!(d.lines.underline, "{text:?}");
            assert!(!d.lines.overline && !d.lines.line_through, "{text:?}");
            assert_eq!(d.style, TextDecorationStyle::Wavy, "{text:?}");
            assert!(d.color.is_some(), "{text:?}");
        }
    }

    #[test]
    fn several_lines_combine_and_none_clears_them() {
        let both =
            parse_decoration("underline line-through", EM).expect("two lines");
        assert!(both.lines.underline && both.lines.line_through);
        assert!(!both.lines.overline);

        let all = parse_decoration("underline overline line-through", EM)
            .expect("three lines");
        assert!(
            all.lines.underline && all.lines.overline && all.lines.line_through
        );

        // `none` and the global keywords all clear, because there is no
        // cascade here for them to mean anything else against.
        for text in [
            "none",
            "initial",
            "inherit",
            "unset",
            "revert",
            "revert-layer",
            "",
            "   ",
        ] {
            let cleared = parse_decoration(text, EM)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert_eq!(
                cleared.lines,
                TextDecoration::default(),
                "{text:?} should clear"
            );
        }

        // And a later `none` clears an earlier line, which is what makes
        // the tokens order-dependent in exactly this one way.
        let overridden =
            parse_decoration("underline none", EM).expect("parsed");
        assert_eq!(overridden.lines, TextDecoration::default());
    }

    #[test]
    fn every_line_style_keyword_is_recognised() {
        for (text, style) in [
            ("underline solid", TextDecorationStyle::Solid),
            ("underline double", TextDecorationStyle::Double),
            ("underline dotted", TextDecorationStyle::Dotted),
            ("underline dashed", TextDecorationStyle::Dashed),
            ("underline wavy", TextDecorationStyle::Wavy),
        ] {
            let d = parse_decoration(text, EM).expect("parsed");
            assert_eq!(d.style, style, "{text:?}");
        }
        // Solid is the default when none is named.
        assert_eq!(
            parse_decoration("underline", EM).expect("parsed").style,
            TextDecorationStyle::Solid
        );
    }

    #[test]
    fn a_thickness_is_a_length_and_auto_is_not_one() {
        let thick = parse_decoration("underline 3px", EM).expect("parsed");
        assert_eq!(thick.thickness, Some(3.0));

        // Relative to the font, resolved now: the decoration is stored as a
        // resolved style, so there is nowhere to keep the unit.
        let relative =
            parse_decoration("underline 0.25em", 40.0).expect("parsed");
        assert_eq!(relative.thickness, Some(10.0));

        // `auto` and `from-font` both mean the font's own, which is what
        // `None` already says.
        for text in ["underline auto", "underline from-font", "underline"] {
            assert_eq!(
                parse_decoration(text, EM).expect("parsed").thickness,
                None,
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_colour_may_be_named_any_way_css_names_one() {
        for text in [
            "underline red",
            "underline #f00",
            "underline #ff0000",
            "underline rgb(255,0,0)",
            "underline currentColor",
        ] {
            assert!(
                parse_decoration(text, EM).is_some(),
                "{text:?} should parse"
            );
        }
    }

    #[test]
    fn a_token_that_is_nothing_at_all_is_refused() {
        // The one way to get this wrong that is not a misspelled keyword:
        // a token that is neither a line, a style, a length nor a colour.
        for text in [
            "underline notacolour",
            "underline 3furlongs",
            "underline 50%",
            "underline \u{1f600}",
        ] {
            assert!(
                parse_decoration(text, EM).is_none(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn keywords_are_case_insensitive_as_css_keywords_are() {
        let d = parse_decoration("UNDERLINE WAVY", EM).expect("parsed");
        assert!(d.lines.underline);
        assert_eq!(d.style, TextDecorationStyle::Wavy);
        assert_eq!(
            parse_decoration("NONE", EM).expect("parsed").lines,
            TextDecoration::default()
        );
    }

    #[test]
    fn a_long_chain_and_a_repeated_function_both_survive() {
        // CSS applies a repeated function twice rather than replacing the
        // first, and a chain has no length limit worth enforcing.
        let ops = parse_filter("blur(1px) blur(2px) blur(3px)", EM)
            .expect("three blurs");
        assert_eq!(ops.len(), 3, "repeats are kept, not collapsed");

        let long = "invert(1) ".repeat(64);
        assert_eq!(parse_filter(&long, EM).expect("a long chain").len(), 64);
    }

    #[test]
    fn an_unreadable_step_rejects_the_whole_chain() {
        // Rather than keeping the parts that did parse. A chain missing a
        // step is a different picture, and dropping the one nobody could
        // read is how a typo becomes a rendering bug.
        for text in [
            "blur(4px) slaturate(150%)",
            "blur(4)",
            "hue-rotate(45)",
            "blur 4px",
            "drop-shadow(2px red)",
        ] {
            let refused = parse_filter(text, EM)
                .expect_err(&format!("{text:?} should not parse"));
            assert!(
                format!("{refused}").contains("could not parse"),
                "{text:?}: {refused}"
            );
        }
    }
}
