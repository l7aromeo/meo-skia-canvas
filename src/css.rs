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
//! Written here rather than taken from a CSS crate: what is needed is a few
//! grammars out of a specification that has hundreds, and a general parser
//! would bring a cascade, a selector engine and a property database for
//! them. The JavaScript `filter`, `letterSpacing` and `wordSpacing` are read
//! by these same functions through the binding, so a string that works on
//! one surface works on the other because it is one parser. The CSS math
//! functions are in [`math`], one evaluator for every property that takes
//! them.
//!
//! [`RgbaLinear`]: crate::color::RgbaLinear
//! [`FilterOp`]: crate::filter::FilterOp

mod math;

use std::{borrow::Cow, cell::OnceCell};

use skia_safe::{Font as SkFont, FontHinting, Typeface};

pub(crate) use math::{
    Dimension, Expect, Percent, Quantity, evaluate, is_math, literal,
};

use crate::{
    color::{RgbaLinear, skia_color_to_rgba_linear},
    error::Error,
    filter::FilterOp,
    geometry::Affine,
    image::{EX_PER_EM, x_height_ratio},
    node::utils::css_to_color,
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

/// Millimetres per centimetre.
///
/// The one conversion here that is not a CSS definition but arithmetic
/// everyone already knows, which is exactly why it was the literal left
/// standing among its named siblings.
const MM_PER_CM: f32 = 10.0;

/// Quarter-millimetres per centimetre, for the `q` unit.
///
/// Four to a millimetre, which is what the unit is.
const Q_PER_CM: f32 = 4.0 * MM_PER_CM;

/// Degrees in a full turn.
pub(crate) const DEGREES_PER_TURN: f64 = 360.0;

/// Gradians in a full turn.
const GRADIANS_PER_TURN: f64 = 400.0;

/// A length in CSS units, as pixels.
///
/// `units` is what the font-relative units resolve against -- the font in
/// play, which is why the caller supplies it rather than this assuming one.
///
/// Returns `None` for a unit CSS does not define, and for a number with no
/// unit at all: CSS requires one on every length but zero, and accepting a
/// bare number here would take `blur(4)` that a browser refuses.
///
/// A percentage is not a length and is refused, as Chrome refuses
/// `blur(50%)`. The properties where a percentage *is* legal are font sizes,
/// and those are parsed by `Font::from_css`, not here. Nor may anything come
/// between the number and its unit: Chrome refuses `blur(5 px)`.
pub(crate) fn parse_size(text: &str, units: &FontUnits) -> Option<f32> {
    let text = text.trim();
    let (value, unit) = split_number(text)?;
    let per_px = match unit {
        // Zero is the one length CSS lets go unitless, because zero of
        // anything is the same zero.
        "" if value == 0.0 => 1.0,
        _ => units.px_per(&unit.to_ascii_lowercase())?,
    };
    Some(value * per_px)
}

/// Pixels in one of an absolute length unit, in lower case, or `None` for
/// anything that is not one.
fn absolute_px(unit: &str) -> Option<f32> {
    Some(match unit {
        "px" => 1.0,
        "pt" => PX_PER_INCH / PT_PER_INCH,
        "pc" => PX_PER_INCH / PC_PER_INCH,
        "in" => PX_PER_INCH,
        "cm" => PX_PER_INCH / CM_PER_INCH,
        "mm" => PX_PER_INCH / (CM_PER_INCH * MM_PER_CM),
        "q" => PX_PER_INCH / (CM_PER_INCH * Q_PER_CM),
        _ => return None,
    })
}

/// The size `rem` resolves against where there is a root to resolve it
/// against: the root element's default, CSS's `medium`. Chrome reads `2rem`
/// in a canvas font as 32px, and `blur(1rem)` as 16px.
pub(crate) const ROOT_FONT_PX: f32 = 16.0;

/// The font-relative length units, in lower case, each with its root form.
///
/// What the root forms resolve against depends on the property, as it does in
/// Chrome: `letterSpacing` and `wordSpacing` resolve them against the
/// context's own font, so at 40px Oswald `1rem` is 40px and `1rex` 23.12px,
/// while `ctx.filter` resolves them against the document root -- see
/// [`FontUnits::with_root`].
const FONT_RELATIVE_UNITS: &[&str] = &[
    "em", "rem", "ex", "rex", "ch", "rch", "cap", "rcap", "ic", "ric",
];

/// The length units that measure something a canvas outside a document does
/// not have: a line box, a viewport, a container.
///
/// Each reads as zero, which is what Chrome 152 gives every one of them in a
/// worker's `OffscreenCanvas` -- the context closest to this library, which
/// has no window either -- and in `letterSpacing` everywhere. A canvas in a
/// document is the exception: there Chrome resolves `blur(1vw)` against the
/// window, 8px at 800 wide, and `blur(1lh)` to 12px. So `blur(1vw)` is
/// taken here and draws no blur at all, and docs/browser-differences.md says
/// so.
const ZERO_LENGTH_UNITS: &[&str] = &[
    "lh", "rlh", "vw", "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "svi",
    "svb", "svmin", "svmax", "lvw", "lvh", "lvi", "lvb", "lvmin", "lvmax",
    "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh", "cqi", "cqb",
    "cqmin", "cqmax",
];

/// Whether `unit`, in lower case, resolves against a font and so has to be
/// carried rather than resolved where it is read.
pub(crate) fn is_font_relative(unit: &str) -> bool {
    FONT_RELATIVE_UNITS.contains(&unit)
}

/// What the font-relative length units resolve against: a font size, and
/// the typeface that size is set in.
///
/// The typeface is asked for only when a unit needs it, and once: finding
/// it means searching the font collection, which `em` alone never needs.
pub(crate) struct FontUnits<'a> {
    em: f32,
    face: Option<Box<dyn Fn() -> Option<Typeface> + 'a>>,
    ratios: OnceCell<FaceRatios>,
    /// What the root forms -- `rem`, `rex`, `rch`, `rcap`, `ric` -- resolve
    /// against, or `None` for this font itself.
    root: Option<Box<FontUnits<'a>>>,
    /// Whether only the absolute units are lengths here, as in a transform,
    /// which has no font and no document to measure the others against.
    absolute: bool,
}

/// The font-relative units as fractions of an em, for one typeface.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FaceRatios {
    ex: f32,
    ch: f32,
    cap: f32,
    ic: f32,
}

/// One `ch` as a fraction of an `em` when the face has no `0`.
///
/// CSS Values 4, section 6.1.1: where the measure of the "0" glyph cannot
/// be determined "it must be assumed to be 0.5em wide by 1em tall".
const CH_PER_EM: f32 = 0.5;

/// One `ic` as a fraction of an `em` when the face has no U+6C34.
///
/// CSS Values 4, section 6.1.1: "it must be assumed to be 1em". Chrome gives
/// 40px for `1ic` at 40px Oswald, which has no such glyph.
const IC_PER_EM: f32 = 1.0;

/// The glyph `ic` measures: U+6C34, the CJK water ideograph, which CSS Values
/// 4 names as the representative full-width character.
const IC_GLYPH: char = '\u{6C34}';

/// The size a face is measured at before its metrics are divided back out,
/// so a ratio belongs to the face and not to one size of it.
const MEASURE_SIZE: f32 = 100.0;

impl FaceRatios {
    /// What CSS Values 4 says to assume when there is no face to measure.
    ///
    /// `cap` has no fallback of its own in the standard beyond "the font's
    /// ascent", and with no face there is no ascent, so it takes one em, the
    /// box an ascent would sit in.
    const ASSUMED: Self = Self {
        ex: EX_PER_EM,
        ch: CH_PER_EM,
        cap: 1.0,
        ic: IC_PER_EM,
    };

    /// The units as `typeface` draws them.
    ///
    /// `ex` is the face's x-height, by [`x_height_ratio`]. `ch` and `ic` are
    /// the advances of `0` and of U+6C34, and `cap` is the cap-height metric,
    /// or the ascent where the face declares none, as CSS Values 4 says.
    /// Measured in Chrome 152 on Oswald: `ex` is its sxHeight of 578/1000,
    /// `ch` the 517/1000 advance of its `0`, and `cap` its 810/1000
    /// capHeight, each exactly.
    fn of(typeface: Typeface) -> Self {
        let mut font = SkFont::from_typeface(typeface.clone(), MEASURE_SIZE);
        // Unhinted, because a hinted advance is snapped to the pixel grid at
        // the size it is measured at. FreeType does that and CoreText does
        // not: with Skia's default hinting, Oswald's `0` measures 52 at
        // 100px on Linux, `1ch` 0.52em, where its advance is 517/1000.
        font.set_hinting(FontHinting::None);
        let advance = |glyph: char| {
            let id = font.unichar_to_glyph(glyph as i32);
            (id != 0).then(|| {
                font.measure_str(glyph.to_string(), None).0 / MEASURE_SIZE
            })
        };
        let (_, metrics) = font.metrics();
        let cap = match metrics.cap_height {
            height if height.is_finite() && height > 0.0 => height,
            _ => -metrics.ascent,
        } / MEASURE_SIZE;
        Self {
            ex: x_height_ratio(typeface).unwrap_or(EX_PER_EM),
            ch: advance('0').unwrap_or(CH_PER_EM),
            cap: match cap.is_finite() && cap > 0.0 {
                true => cap,
                false => Self::ASSUMED.cap,
            },
            ic: advance(IC_GLYPH).unwrap_or(IC_PER_EM),
        }
    }
}

impl<'a> FontUnits<'a> {
    /// Units for a font of `em` pixels, set in whatever `face` returns.
    pub(crate) fn new(
        em: f32,
        face: impl Fn() -> Option<Typeface> + 'a,
    ) -> Self {
        Self {
            em,
            face: Some(Box::new(face)),
            ratios: OnceCell::new(),
            root: None,
            absolute: false,
        }
    }

    /// These units with the root forms resolving against `root` rather than
    /// against this font.
    pub(crate) fn with_root(self, root: FontUnits<'a>) -> Self {
        Self {
            root: Some(Box::new(root)),
            ..self
        }
    }

    /// Units for a font of `em` pixels with no typeface to measure, so each
    /// unit takes the value CSS Values 4 says to assume.
    pub(crate) fn assumed(em: f32) -> Self {
        Self {
            em,
            face: None,
            ratios: OnceCell::new(),
            root: None,
            absolute: false,
        }
    }

    /// Units with no font and no document behind them, where the absolute
    /// units are the only lengths: Chrome's `DOMMatrix` refuses
    /// `translate(1em)`, `translate(1rem)`, `translate(1vw)` and
    /// `translate(1lh)`, inside `calc()` or out.
    pub(crate) fn absolute() -> Self {
        Self {
            absolute: true,
            ..Self::assumed(0.0)
        }
    }

    fn ratios(&self) -> FaceRatios {
        *self.ratios.get_or_init(|| {
            self.face
                .as_ref()
                .and_then(|face| face())
                .map_or(FaceRatios::ASSUMED, FaceRatios::of)
        })
    }

    /// Pixels in one of `unit`, in lower case: an absolute unit, a
    /// font-relative one, or one that reads as zero here. `None` for
    /// anything that is not a length unit.
    pub(crate) fn px_per(&self, unit: &str) -> Option<f32> {
        if let Some(px) = absolute_px(unit) {
            return Some(px);
        }
        if self.absolute {
            return None;
        }
        if ZERO_LENGTH_UNITS.contains(&unit) {
            return Some(0.0);
        }
        if let Some(local) = unit.strip_prefix('r')
            && is_font_relative(unit)
        {
            let root = self.root.as_deref().unwrap_or(self);
            return root.px_per(local);
        }
        let ratio = match unit {
            "em" => return Some(self.em),
            "ex" => self.ratios().ex,
            "ch" => self.ratios().ch,
            "cap" => self.ratios().cap,
            "ic" => self.ratios().ic,
            _ => return None,
        };
        Some(self.em * ratio)
    }
}

/// Degrees in one of an angle unit, in lower case, or `None` for anything
/// that is not one.
pub(crate) fn angle_degrees(unit: &str) -> Option<f64> {
    Some(match unit {
        "deg" => 1.0,
        "rad" => 1.0_f64.to_degrees(),
        "grad" => DEGREES_PER_TURN / GRADIANS_PER_TURN,
        "turn" => DEGREES_PER_TURN,
        _ => return None,
    })
}

/// Splits a CSS dimension into its number and its unit.
///
/// The number's extent is not "every character that could appear in one":
/// `e` both continues an exponent and begins `em`, so `0.5em` split that way
/// gives `0.5e` and `m`, and neither half is anything. An `e` therefore
/// belongs to the number only when a digit follows it, with an optional
/// sign in between -- which is exactly when CSS says it is an exponent. A
/// `.` is the same: it belongs to the number only when a digit follows, so
/// `5.px` is not five pixels, as Chrome agrees.
pub(crate) fn split_number(text: &str) -> Option<(f32, &str)> {
    let at = number_length(text);
    let value: f32 = text.get(..at)?.parse().ok()?;
    value
        .is_finite()
        .then(|| (value, text.get(at..).unwrap_or("")))
}

/// How many bytes at the front of `text` are a CSS number, by the rules
/// [`split_number`] gives. Zero when there is none.
pub(crate) fn number_length(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut at = 0;

    if matches!(bytes.first(), Some(b'+' | b'-')) {
        at += 1;
    }
    while matches!(bytes.get(at), Some(c) if c.is_ascii_digit()) {
        at += 1;
    }
    if bytes.get(at) == Some(&b'.')
        && matches!(bytes.get(at + 1), Some(c) if c.is_ascii_digit())
    {
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
    at
}

/// A CSS length split into the number, its unit, and its value in pixels.
///
/// The three a [`Spacing`](crate::node::typography::Spacing) is built from.
/// Absolute units resolve here, and so do the units that read as zero; the
/// font-relative ones cannot, because what they resolve against changes
/// when the font does -- so their pixel value is left `NaN` and the unit is
/// carried, which is how spacing stays relative after it is set rather than
/// being frozen at the font it was set with.
pub(crate) struct Length {
    /// The number as written.
    pub value: f32,
    /// The unit, lowercased.
    pub unit: String,
    /// The length in pixels, or `NaN` for a unit that needs a font.
    pub pixels: f32,
}

/// A CSS length for a property that keeps its unit.
///
/// Returns `None` for a unit CSS does not define and for a bare number other
/// than zero. A percentage is refused for the reason [`parse_size`] refuses
/// it: `letter-spacing` is defined over `<length>`. So is a math function:
/// Chrome 152 ignores `calc()`, `min()` and the rest in `letterSpacing` and
/// `wordSpacing`, while `ctx.filter` takes them.
pub(crate) fn parse_length(text: &str) -> Option<Length> {
    let (value, unit) = split_number(text.trim())?;
    length_of(value, unit)
}

/// A [`Length`] from a number and its unit as written, by the rules
/// [`parse_length`] reads them with.
fn length_of(value: f32, unit: &str) -> Option<Length> {
    let unit = unit.to_ascii_lowercase();
    if unit.is_empty() {
        // Zero is the one length CSS lets go unitless.
        return (value == 0.0).then_some(Length {
            value,
            unit,
            pixels: 0.0,
        });
    }
    if is_font_relative(&unit) {
        // Relative, so the pixel value is not knowable yet.
        return Some(Length {
            value,
            unit,
            pixels: f32::NAN,
        });
    }
    let pixels = value * FontUnits::assumed(f32::NAN).px_per(&unit)?;
    pixels.is_finite().then_some(Length {
        value,
        unit,
        pixels,
    })
}

/// A percentage or bare number as a fraction.
///
/// `"150%"` and `"1.5"` both give 1.5, which is what the CSS filter
/// functions accept interchangeably.
pub(crate) fn parse_percentage(text: &str) -> Option<f32> {
    // Split rather than trimmed first: nothing may come between a number
    // and its `%`, and Chrome refuses `50 %`.
    let plain = |number: &str| match split_number(number)? {
        (value, "") => Some(value),
        _ => None,
    };
    let text = text.trim();
    let value = match text.strip_suffix('%') {
        Some(number) => plain(number)? / 100.0,
        None => plain(text)?,
    };
    value.is_finite().then_some(value)
}

/// An angle in CSS units, as degrees.
///
/// Returns `None` for a bare number other than zero: CSS requires a unit on
/// an angle, and Chrome refuses `hue-rotate(45)` but reads `hue-rotate(0)`,
/// `rotate(0)` and `skewX(0)`.
pub(crate) fn parse_angle(text: &str) -> Option<f32> {
    let (value, unit) = split_number(text.trim())?;
    match unit {
        "" if value == 0.0 => Some(0.0),
        // Case-insensitive: CSS units are, and a browser takes `45DEG`.
        _ => Some(
            (f64::from(value) * angle_degrees(&unit.to_ascii_lowercase())?)
                as f32,
        ),
    }
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
    /// The line's thickness as written, or `None` for the font's own. A
    /// font-relative one is carried rather than resolved, so it follows the
    /// font the text is laid out in, as a spacing does.
    pub thickness: Option<Length>,
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
pub(crate) fn parse_decoration(text: &str) -> Option<Decoration> {
    let mut parsed = Decoration {
        lines: TextDecoration::default(),
        style: TextDecorationStyle::Solid,
        color: None,
        thickness: None,
    };

    // Split where a filter is split, so a colour function keeps its own
    // spaces: `underline hsl(120 50% 50%)` is two tokens, not four.
    for token in split_functions(text.trim()) {
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
            _ => match parse_length(token) {
                Some(thickness) => parsed.thickness = Some(thickness),
                // Anything left has to be a colour, and a token that is not
                // one is a token nobody can act on.
                None => parsed.color = Some(eight_bit_colour(token)?),
            },
        }
    }
    Some(parsed)
}

/// Splits `text` on whitespace that is not inside parentheses, and after
/// each `)` that closes a call.
///
/// A filter chain is space-separated and its arguments are too, so a plain
/// split would cut `drop-shadow(2px 2px 4px red)` into four pieces. Calls
/// may also abut: Chrome reads `blur(2px)brightness(2)` and
/// `translate(10px,20px)rotate(45deg)` as two functions each.
fn split_functions(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (at, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    parts.push(&text[start..=at]);
                    start = at + 1;
                }
            }
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

/// `text` with every parenthesis still open at its end closed.
///
/// CSS Syntax 3 ends a block where the input ends, so Chrome draws
/// `blur(2px` as `blur(2px)` and `drop-shadow(2px 2px rgb(255 0 0` with both
/// closed. A `)` with nothing open is left for the parse to refuse.
fn close_open_blocks(text: &str) -> Cow<'_, str> {
    let mut depth = 0usize;
    for character in text.chars() {
        match character {
            '(' => depth += 1,
            ')' => match depth.checked_sub(1) {
                Some(open) => depth = open,
                None => return Cow::Borrowed(text),
            },
            _ => {}
        }
    }
    match depth {
        0 => Cow::Borrowed(text),
        open => Cow::Owned(format!("{text}{}", ")".repeat(open))),
    }
}

/// A CSS `filter` value as the chain [`Context2D::set_filter`] takes.
///
/// `"none"` is an empty chain, which is how the property spells "no
/// filter".
///
/// [`Context2D::set_filter`]: crate::context2d::Context2D::set_filter
///
/// # Errors
///
/// Returns [`Error::InvalidFilter`] naming the piece that did not parse.
/// The whole string is rejected rather than the recognised parts kept:
/// a chain missing one step is a different picture, and quietly dropping
/// the step nobody could read is how a typo becomes a rendering bug. A
/// blank string is refused too: HTML's `filter` setter parses the value as
/// a `<filter-value-list>`, which an empty one is not, and leaves the
/// filter as it was, as Chrome does.
pub(crate) fn parse_filter(
    text: &str,
    units: &FontUnits,
) -> Result<Vec<FilterOp>, Error> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidFilter {
            reason: format!("the filter {text:?} is blank; \"none\" clears it"),
        });
    }
    if trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    let refuse = |part: &str| Error::InvalidFilter {
        reason: format!("could not parse {part:?} in filter {text:?}"),
    };
    let closed = close_open_blocks(trimmed);
    let trimmed = closed.as_ref();

    let mut ops = Vec::new();
    for part in split_functions(trimmed) {
        let (name, argument) = part
            .split_once('(')
            .and_then(|(name, rest)| {
                rest.strip_suffix(')').map(|argument| (name, argument))
            })
            .ok_or_else(|| refuse(part))?;
        // A function name is an identifier, so any case: Chrome reads
        // `BLUR(5px)`.
        let name = name.to_ascii_lowercase();
        // Every function but `drop-shadow` may leave its argument out, and
        // then takes the value that changes nothing.
        let omitted = argument.trim().is_empty();

        // A negative amount is outside every one of these functions'
        // ranges, and Chrome refuses it when it is written out but clamps it
        // when a math function computes it: `brightness(-1)` is refused and
        // `brightness(calc(-1))` draws as `brightness(0)`.
        let percent = || {
            match omitted {
                true => Some(OMITTED_FILTER_AMOUNT),
                false => math(
                    argument,
                    Percent::Own,
                    units,
                    &[Dimension::NUMBER, Dimension::PERCENT],
                    0.0,
                )
                .unwrap_or_else(|| {
                    parse_percentage(argument).filter(|&amount| amount >= 0.0)
                }),
            }
            .ok_or_else(|| refuse(part))
        };
        ops.push(match name.as_str() {
            // Its arguments are themselves space-separated, so it is the one
            // function that cannot share the shape below.
            "drop-shadow" => {
                drop_shadow(argument, units).ok_or_else(|| refuse(part))?
            }
            "blur" => FilterOp::Blur(
                match omitted {
                    true => Some(0.0),
                    false => math(
                        argument,
                        Percent::Refused,
                        units,
                        &[Dimension::LENGTH],
                        0.0,
                    )
                    .unwrap_or_else(|| {
                        parse_size(argument, units)
                            .filter(|&radius| radius >= 0.0)
                    }),
                }
                .ok_or_else(|| refuse(part))?,
            ),
            "hue-rotate" => FilterOp::HueRotate(
                match omitted {
                    true => Some(0.0),
                    false => math(
                        argument,
                        Percent::Refused,
                        units,
                        &[Dimension::ANGLE],
                        f32::MIN,
                    )
                    .unwrap_or_else(|| parse_angle(argument)),
                }
                .ok_or_else(|| refuse(part))?,
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

/// What `brightness()`, `contrast()`, `grayscale()`, `invert()`, `opacity()`,
/// `saturate()` and `sepia()` take when their argument is left out: Filter
/// Effects 1 gives each a default of 1, which for `brightness`, `contrast`,
/// `opacity` and `saturate` changes nothing and for the other three is the
/// full effect.
const OMITTED_FILTER_AMOUNT: f32 = 1.0;

/// `argument` as a math function of one of `dimensions`, held into
/// `[low, f32::MAX]` by [`held`].
///
/// `None` when `argument` is not a math function at all, so the caller reads
/// it as a written value instead. `Some(None)` when it is one and does not
/// evaluate, or evaluates to another dimension -- a refusal, never a second
/// reading.
fn math(
    argument: &str,
    percent: Percent,
    units: &FontUnits,
    dimensions: &[Dimension],
    low: f32,
) -> Option<Option<f32>> {
    let argument = argument.trim();
    is_math(argument).then(|| {
        evaluate(
            argument,
            &Expect {
                percent,
                units,
                channels: &[],
            },
        )
        .filter(|quantity| dimensions.contains(&quantity.dimension))
        .map(|quantity| held(quantity, low))
    })
}

/// A finished math value as the `f32` a filter carries, held to
/// `[low, f32::MAX]`.
///
/// The top of the range is Chrome's: it clamps an infinity to the largest
/// float, so `brightness(calc(infinity))` draws as
/// `brightness(3.4028234e38)` and an infinite shadow offset as
/// `3.4e38px`. The bottom is the function's own range, which a computed
/// value is clamped into where a written one is refused.
fn held(quantity: Quantity, low: f32) -> f32 {
    // In `f64` first: `as f32` turns anything past the largest float into
    // an infinity rather than saturating.
    quantity
        .finished()
        .clamp(f64::from(low), f64::from(f32::MAX)) as f32
}

/// A CSS colour for a destination that holds eight bits a channel, as a
/// filter shadow and a text decoration both do: quantised in sRGB first,
/// where Chrome quantises it, and then carried as an [`RgbaLinear`] that
/// converts back to the same bytes exactly.
///
/// Through [`RgbaLinear::from_css`] instead, `rgb(10.5 20.4 30.6)` lost its
/// half step in the `f32` round trip through linear light and drew red 10,
/// where Chrome draws 11 for the shadow and the fill alike. The parser is
/// the one `set_fill_style_css` uses, so a colour that works in one place
/// works in the other.
fn eight_bit_colour(css: &str) -> Option<RgbaLinear> {
    css_to_color(css).map(skia_color_to_rgba_linear)
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
fn drop_shadow(args: &str, units: &FontUnits) -> Option<FilterOp> {
    // Split where `split_functions` splits, so a math function or a colour
    // function stays one word whatever spaces it holds.
    let words = split_functions(args.trim());

    // A length, and whether a math function computed it: a computed blur
    // radius below zero is clamped, where a written one is refused.
    let length = |word: &&str| match math(
        word,
        Percent::Refused,
        units,
        &[Dimension::LENGTH],
        f32::MIN,
    ) {
        Some(computed) => computed.map(|px| (px, true)),
        None => parse_size(word, units).map(|px| (px, false)),
    };

    // The colour may sit at either end, so the run of lengths is anchored to
    // one end and then the other, and whatever is left is the colour.
    let leading = |words: &[&str]| {
        let lengths: Vec<(f32, bool)> =
            words.iter().map_while(length).collect();
        let rest = words[lengths.len()..].join(" ");
        (lengths, rest)
    };
    let trailing = |words: &[&str]| {
        let mut lengths: Vec<(f32, bool)> =
            words.iter().rev().map_while(length).collect();
        lengths.reverse();
        let rest = words[..words.len() - lengths.len()].join(" ");
        (lengths, rest)
    };

    // Colour after the lengths first, since that is the common spelling.
    for (lengths, rest) in [leading(&words), trailing(&words)] {
        // The offsets may be negative; the blur radius may not, and Chrome
        // refuses `drop-shadow(2px 2px -3px)` -- but draws
        // `drop-shadow(1px 2px calc(-3px) red)` with no blur.
        let radius = match lengths.get(2) {
            None => 0.0,
            Some(&(radius, true)) => radius.max(0.0),
            Some(&(radius, false)) if radius >= 0.0 => radius,
            Some(_) => continue,
        };
        if !(2..=3).contains(&lengths.len()) {
            continue;
        }
        let color = match rest.is_empty() {
            // The initial value is `currentColor`, which a canvas resolves
            // to the fill; black is what this crate has always used and what
            // the typed `FilterOp::DropShadow` carries when none is given.
            true => RgbaLinear::opaque(0.0, 0.0, 0.0),
            false => match eight_bit_colour(&rest) {
                Some(color) => color,
                None => continue,
            },
        };
        return Some(FilterOp::DropShadow {
            offset_x: lengths[0].0,
            offset_y: lengths[1].0,
            // The blur radius is the optional one, and its initial value is
            // zero -- a hard-edged shadow.
            blur: radius,
            color,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgba_linear_to_skia_color;

    /// The font size CSS relative units resolve against in these tests.
    const EM: f32 = 16.0;

    /// Units for a 16px font with no face, so `ex`, `ch`, `cap` and `ic` take
    /// what CSS Values 4 says to assume.
    fn em() -> FontUnits<'static> {
        FontUnits::assumed(EM)
    }

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
            let got = parse_size(text, &em())
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert!(
                (got - pixels).abs() < 1e-3,
                "{text:?} gave {got}, expected {pixels}"
            );
        }
    }

    #[test]
    fn a_percentage_is_not_a_length() {
        // It reads like one, and Chrome refuses `blur(50%)`: CSS defines
        // `blur()` over `<length>`, and a percentage is not one.
        assert_eq!(parse_size("50%", &em()), None);
        assert!(parse_filter("blur(50%)", &em()).is_err());
    }

    #[test]
    fn a_length_without_a_unit_is_refused_unless_it_is_zero() {
        // CSS requires a unit on every length but zero, and a browser
        // rejects `blur(4)`. Accepting it here would take a filter that
        // does nothing in a browser and make it work in this crate, which
        // is worse than refusing both.
        assert_eq!(parse_size("4", &em()), None);
        assert_eq!(parse_size("0", &em()), Some(0.0));
        assert_eq!(parse_size("4furlongs", &em()), None);
        assert_eq!(parse_size("", &em()), None);
        assert_eq!(parse_size("px", &em()), None);
    }

    #[test]
    fn a_point_needs_a_digit_after_it() {
        // CSS Syntax's number has digits after any `.`, and Chrome ignores
        // `ctx.font = "5.px Arial"`.
        assert_eq!(parse_size("5.px", &em()), None);
        assert_eq!(parse_size("5.", &em()), None);
        assert_eq!(parse_size(".5px", &em()), Some(0.5));
        assert_eq!(parse_percentage("2."), None);
        assert_eq!(parse_percentage("50.%"), None);
        // The point is left as a unit, which no reader takes, and the
        // spellings `str::parse` would take and CSS does not are no number.
        assert_eq!(split_wide_number("2."), Some((2.0, ".".to_string())));
        assert_eq!(split_wide_number("nan"), None);
        assert_eq!(split_wide_number("inf"), None);
        assert_eq!(split_wide_number("-1.5e3"), Some((-1500.0, String::new())));
        assert_eq!(parse_size("5.0px", &em()), Some(5.0));
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
            &em(),
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
    fn none_is_an_empty_chain_and_nothing_is_refused() {
        for text in ["none", "NONE", " none "] {
            assert!(
                parse_filter(text, &em()).expect("accepted").is_empty(),
                "{text:?}"
            );
        }
        // HTML's setter parses the value as a `<filter-value-list>`, which a
        // blank string is not, and Chrome 152 leaves the filter as it was.
        for text in ["", "   ", "\t"] {
            assert!(
                matches!(
                    parse_filter(text, &em()),
                    Err(Error::InvalidFilter { .. })
                ),
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
        let shadow = |css: &str| match parse_filter(css, &em()) {
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
        let ops = parse_filter("drop-shadow(2px 4px 6px red)", &em())
            .expect("a shadow");
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
            &em(),
        )
        .expect("a chain");
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn a_relative_length_follows_the_font_size_it_is_given() {
        // What the string form buys over the typed one: `em` cannot be
        // expressed as a fixed pixel count, because it is not one.
        let at = |em| match parse_filter("blur(0.5em)", &FontUnits::assumed(em))
        {
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
            let ops = parse_filter(text, &em())
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
            let ops = parse_filter(text, &em())
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(ops.len(), 1, "{text:?}");
            assert!(
                matches!(ops[0], FilterOp::DropShadow { blur, .. }
                    if (blur - 3.0).abs() < 1e-3),
                "{text:?}"
            );
        }

        // And one nested colour does not swallow a following function.
        let chain = parse_filter(
            "drop-shadow(1px 2px 3px rgb(0 0 0)) blur(2px)",
            &em(),
        )
        .expect("two steps");
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn malformed_input_is_refused_without_panicking() {
        // A parser reachable from a string a caller did not write has to
        // survive anything. None of these may panic, and none may parse.
        for text in [
            "blur)",
            "()",
            "(((",
            ")))",
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
            "blur(4px) (",
            "\u{1f600}",
            "blur(\u{1f600})",
            "blur(4px) \u{0}",
        ] {
            assert!(
                parse_filter(text, &em()).is_err(),
                "{text:?} should not parse"
            );
        }
        // A function left open at the end is closed, as Chrome 152 closes
        // it: it takes `blur(`, `blur(4px` and `hue-rotate(`.
        for (text, closed) in [
            ("blur(", "blur()"),
            ("blur(4px", "blur(4px)"),
            ("hue-rotate(", "hue-rotate()"),
        ] {
            assert_eq!(
                parse_filter(text, &em()).expect("taken"),
                parse_filter(closed, &em()).expect("taken"),
                "{text:?}"
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
            let _ = parse_size(text, &em());
            let _ = parse_angle(text);
            let _ = parse_percentage(text);
        }

        // The ones with a defined answer.
        assert_eq!(parse_size("1.2.3px", &em()), None, "two decimal points");
        assert_eq!(parse_size("--1px", &em()), None, "two signs");
        assert_eq!(parse_size("1px2", &em()), None, "trailing rubbish");
        assert_eq!(parse_size("1e", &em()), None, "an exponent with no digits");
        // An overflowing exponent parses to infinity, which is not a length.
        assert_eq!(parse_size("1e400px", &em()), None, "overflows to infinity");
        assert_eq!(parse_percentage("NaN"), None);
        assert_eq!(parse_percentage("inf"), None);
    }

    #[test]
    fn an_exponent_and_the_em_unit_do_not_take_each_other_s_characters() {
        // The bug this parser had: `e` both continues an exponent and
        // begins `em`, so splitting on "characters a number may contain"
        // read `0.5em` as `0.5e` and `m`, and neither half is anything.
        assert_eq!(parse_size("0.5em", &FontUnits::assumed(16.0)), Some(8.0));
        assert_eq!(parse_size("2em", &FontUnits::assumed(16.0)), Some(32.0));
        assert_eq!(parse_size("1rem", &FontUnits::assumed(16.0)), Some(16.0));
        // A real exponent still works, including with a sign.
        assert_eq!(parse_size("1e2px", &em()), Some(100.0));
        assert_eq!(parse_size("1e+2px", &em()), Some(100.0));
        assert_eq!(parse_size("1e-2px", &em()), Some(0.01));
        // And an `e` that begins a unit is a unit, not a broken exponent.
        // With no face to measure, half an em, as CSS Values 4 assumes.
        assert_eq!(parse_size("1ex", &em()), Some(8.0), "ex is a unit");
    }

    #[test]
    fn a_function_name_is_matched_whole_in_any_case() {
        // Neither a prefix nor a suffix of a known name is that name, and
        // the case of a name does not matter: CSS names are identifiers,
        // and Chrome reads `BLUR(5px)`.
        for text in [
            "blu(4px)",
            "blurr(4px)",
            "xblur(4px)",
            "drop-shadows(1px 2px 3px red)",
            "shadow(1px 2px 3px red)",
        ] {
            assert!(
                parse_filter(text, &em()).is_err(),
                "{text:?} should not parse"
            );
        }
        for text in ["blur(4px)", "BLUR(4px)", "Drop-Shadow(1px 2px red)"] {
            assert!(parse_filter(text, &em()).is_ok(), "{text:?} should parse");
        }
    }

    #[test]
    fn a_percentage_argument_is_refused_where_a_length_belongs() {
        // `blur` takes a length and `saturate` takes a percentage, and
        // neither accepts the other's form -- which is what stops
        // `blur(50%)` silently blurring by half the font size.
        assert!(parse_filter("blur(50%)", &em()).is_err());
        assert!(parse_filter("saturate(4px)", &em()).is_err());
        assert!(parse_filter("hue-rotate(50%)", &em()).is_err());
        // Though a bare number is a percentage, which CSS allows.
        assert!(parse_filter("saturate(1.5)", &em()).is_ok());
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

        // Every unit that needs the face, and its root form.
        for unit in FONT_RELATIVE_UNITS {
            let length = parse_length(&format!("1{unit}"))
                .unwrap_or_else(|| panic!("{unit} is a length unit"));
            assert!(length.pixels.is_nan(), "{unit} needs a font");
        }

        for (text, unit, pixels) in [
            ("2px", "px", 2.0),
            ("1in", "in", 96.0),
            ("1pc", "pc", 16.0),
            ("1pt", "pt", 96.0 / 72.0),
            ("1cm", "cm", 96.0 / 2.54),
            ("1mm", "mm", 96.0 / 25.4),
            ("1q", "q", 96.0 / 25.4 / 4.0),
            ("-3px", "px", -3.0),
            // Measured as zero in Chrome 152's `letterSpacing`: a canvas has
            // no line box, viewport or container to measure.
            ("2lh", "lh", 0.0),
            ("2vw", "vw", 0.0),
            ("2cqmin", "cqmin", 0.0),
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
            // Every math function, which Chrome refuses in spacing.
            "calc(1px)",
            "min(1px, 2px)",
        ] {
            assert!(parse_length(text).is_none(), "{text:?} should not parse");
        }
        // Zero alone may go unitless here; spacing refuses it on top, in
        // `Spacing::from_css`.
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
            let d = parse_decoration(text)
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
            parse_decoration("underline line-through").expect("two lines");
        assert!(both.lines.underline && both.lines.line_through);
        assert!(!both.lines.overline);

        let all = parse_decoration("underline overline line-through")
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
            let cleared = parse_decoration(text)
                .unwrap_or_else(|| panic!("{text:?} did not parse"));
            assert_eq!(
                cleared.lines,
                TextDecoration::default(),
                "{text:?} should clear"
            );
        }

        // And a later `none` clears an earlier line, which is what makes
        // the tokens order-dependent in exactly this one way.
        let overridden = parse_decoration("underline none").expect("parsed");
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
            let d = parse_decoration(text).expect("parsed");
            assert_eq!(d.style, style, "{text:?}");
        }
        // Solid is the default when none is named.
        assert_eq!(
            parse_decoration("underline").expect("parsed").style,
            TextDecorationStyle::Solid
        );
    }

    #[test]
    fn a_thickness_is_a_length_and_auto_is_not_one() {
        let thick = parse_decoration("underline 3px")
            .and_then(|parsed| parsed.thickness)
            .expect("a thickness");
        assert_eq!((thick.unit.as_str(), thick.pixels), ("px", 3.0));

        // Relative to the font, and kept so: the font the text is laid out
        // in decides it, as it does a spacing.
        let relative = parse_decoration("underline 0.25em")
            .and_then(|parsed| parsed.thickness)
            .expect("a thickness");
        assert_eq!((relative.value, relative.unit.as_str()), (0.25, "em"));
        assert!(relative.pixels.is_nan(), "not resolved yet");

        // `auto` and `from-font` both mean the font's own, which is what
        // `None` already says.
        for text in ["underline auto", "underline from-font", "underline"] {
            assert!(
                parse_decoration(text).expect("parsed").thickness.is_none(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_computed_value_is_clamped_into_its_function_s_range() {
        // Chrome draws `blur(calc(-1px))` as `blur(0px)` and
        // `brightness(calc(-1))` as `brightness(0)`. Skia happens to draw a
        // negative blur as none, so the pixels cannot show this clamp; the
        // operation the parse hands on can.
        let parsed = |css: &str| parse_filter(css, &em()).expect("taken");
        assert_eq!(parsed("blur(calc(-1px))"), [FilterOp::Blur(0.0)]);
        assert_eq!(parsed("brightness(calc(-1))"), [FilterOp::Brightness(0.0)]);
        match parsed("drop-shadow(1px 2px calc(-3px) red)")[..] {
            [FilterOp::DropShadow { blur, .. }] => assert_eq!(blur, 0.0),
            ref other => panic!("{other:?}"),
        }
        // And an infinity is the largest `f32`, as Chrome draws it.
        assert_eq!(
            parsed("blur(calc(1px * infinity))"),
            [FilterOp::Blur(f32::MAX)]
        );
    }

    #[test]
    fn a_decoration_colour_is_the_colour_css_names() {
        // Decoded from sRGB rather than read as linear: the bytes that come
        // back are the bytes CSS names.
        for (css, bytes) in [
            ("rebeccapurple", [102, 51, 153]),
            ("#742", [119, 68, 34]),
            ("hsl(120 50% 50%)", [64, 191, 64]),
            // A half step rounds up at parse, as Chrome rounds it.
            ("rgb(10.5 20.4 30.6)", [11, 20, 31]),
        ] {
            let parsed = parse_decoration(&format!("underline {css}"))
                .expect("a decoration");
            let color =
                rgba_linear_to_skia_color(parsed.color.expect("a colour"));
            assert_eq!([color.r(), color.g(), color.b()], bytes, "{css}");
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
            assert!(parse_decoration(text).is_some(), "{text:?} should parse");
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
                parse_decoration(text).is_none(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn keywords_are_case_insensitive_as_css_keywords_are() {
        let d = parse_decoration("UNDERLINE WAVY").expect("parsed");
        assert!(d.lines.underline);
        assert_eq!(d.style, TextDecorationStyle::Wavy);
        assert_eq!(
            parse_decoration("NONE").expect("parsed").lines,
            TextDecoration::default()
        );
    }

    #[test]
    fn a_long_chain_and_a_repeated_function_both_survive() {
        // CSS applies a repeated function twice rather than replacing the
        // first, and a chain has no length limit worth enforcing.
        let ops = parse_filter("blur(1px) blur(2px) blur(3px)", &em())
            .expect("three blurs");
        assert_eq!(ops.len(), 3, "repeats are kept, not collapsed");

        let long = "invert(1) ".repeat(64);
        assert_eq!(parse_filter(&long, &em()).expect("a long chain").len(), 64);
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
            let refused = parse_filter(text, &em())
                .expect_err(&format!("{text:?} should not parse"));
            assert!(
                format!("{refused}").contains("could not parse"),
                "{text:?}: {refused}"
            );
        }
    }
}

/// A 4x4 transform in the layout `DOMMatrix` uses: `m[column][row]`, so
/// `m[0][1]` is `m12`, which is `b`, and `m[3][0]` is `m41`, which is `e`.
///
/// A transform list is composed at this size and projected to an [`Affine`]
/// once at the end, because a 3D step can move the 2D result: under
/// `rotateX(30deg) translate3d(10px, 20px, 30px)` the `z` of the translation
/// reaches `f`, and Chrome reports `f = 2.32`, where projecting each step on
/// its own would give 20. Composed in `f64` as a browser does, and narrowed
/// to `f32` only in that last step.
#[derive(Clone, Copy)]
struct Matrix4([[f64; 4]; 4]);

impl Matrix4 {
    const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    /// The 2D matrix `a b c d e f`, as `matrix()` and `DOMMatrix` spell it.
    fn planar(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][0] = a;
        m.0[0][1] = b;
        m.0[1][0] = c;
        m.0[1][1] = d;
        m.0[3][0] = e;
        m.0[3][1] = f;
        m
    }

    fn translation(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[3] = [x, y, z, 1.0];
        m
    }

    fn scale(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][0] = x;
        m.0[1][1] = y;
        m.0[2][2] = z;
        m
    }

    /// A rotation of `degrees` about the axis `(x, y, z)`, which need not be
    /// a unit vector, by the formula the Geometry Interfaces specification
    /// gives for `rotateAxisAngle`. A zero-length axis is no rotation at
    /// all, which is what Chrome returns for `rotate3d(0, 0, 0, 45deg)`.
    fn rotation(x: f64, y: f64, z: f64, degrees: f64) -> Self {
        let length = (x * x + y * y + z * z).sqrt();
        if length == 0.0 {
            return Self::IDENTITY;
        }
        let (x, y, z) = (x / length, y / length, z / length);
        let half = degrees.to_radians() / 2.0;
        let sc = half.sin() * half.cos();
        let sq = half.sin() * half.sin();
        let mut m = Self::IDENTITY;
        m.0[0][0] = 1.0 - 2.0 * (y * y + z * z) * sq;
        m.0[0][1] = 2.0 * (x * y * sq + z * sc);
        m.0[0][2] = 2.0 * (x * z * sq - y * sc);
        m.0[1][0] = 2.0 * (x * y * sq - z * sc);
        m.0[1][1] = 1.0 - 2.0 * (x * x + z * z) * sq;
        m.0[1][2] = 2.0 * (y * z * sq + x * sc);
        m.0[2][0] = 2.0 * (x * z * sq + y * sc);
        m.0[2][1] = 2.0 * (y * z * sq - x * sc);
        m.0[2][2] = 1.0 - 2.0 * (x * x + y * y) * sq;
        m
    }

    /// `self` with `other` applied first, as a transform list composes left
    /// to right.
    fn then(&self, other: &Self) -> Self {
        let mut out = [[0.0; 4]; 4];
        for (column, out_column) in out.iter_mut().enumerate() {
            for (row, cell) in out_column.iter_mut().enumerate() {
                *cell =
                    (0..4).map(|k| self.0[k][row] * other.0[column][k]).sum();
            }
        }
        Self(out)
    }

    /// The sixteen values, column by column, as `toFloat64Array` lists them.
    fn flat(self) -> [f64; 16] {
        let mut out = [0.0; 16];
        for (cell, value) in out.iter_mut().zip(self.0.iter().flatten()) {
            *cell = *value;
        }
        out
    }

    /// The matrix [`flat`](Self::flat) lists.
    fn from_flat(values: [f64; 16]) -> Self {
        let mut m = Self::IDENTITY;
        for (cell, value) in m.0.iter_mut().flatten().zip(values) {
            *cell = value;
        }
        m
    }

    /// The 2D part, `a b c d e f`, as `DOMMatrix` reports it.
    fn to_affine(self) -> Affine {
        let m = self.0;
        Affine {
            a: m[0][0] as f32,
            b: m[0][1] as f32,
            c: m[1][0] as f32,
            d: m[1][1] as f32,
            tx: m[3][0] as f32,
            ty: m[3][1] as f32,
        }
    }
}

/// Blink's `LayoutUnit::kFixedPointDenominator`: a layout length carries six
/// fractional bits.
const LAYOUT_UNIT_DENOMINATOR: i32 = 1 << 6;

/// The largest length a transform keeps, in pixels: Blink's
/// `kMaxValueForCssLength`, `INT_MAX / LayoutUnit::kFixedPointDenominator -
/// 2`, in css_primitive_value.cc. `ClampToCSSLengthRange` holds a length to
/// this and [`MIN_CSS_LENGTH`] and then narrows it to a `float`, so Chrome
/// reads `translate(1e40px)` and `translate(calc(1px * infinity))` alike as
/// 33554428, the nearest `float` to 33554429.
const MAX_CSS_LENGTH: f64 = (i32::MAX / LAYOUT_UNIT_DENOMINATOR - 2) as f64;

/// The smallest length a transform keeps, in pixels: Blink's
/// `kMinValueForCssLength`, `INT_MIN / LayoutUnit::kFixedPointDenominator +
/// 2`. Chrome reads `translate(calc(1px * -infinity))` as -33554430.
const MIN_CSS_LENGTH: f64 = (i32::MIN / LAYOUT_UNIT_DENOMINATOR + 2) as f64;

/// The functions whose presence makes a list three-dimensional, whatever
/// their arguments: Chrome reports `translate3d(0, 0, 0)` and `rotateX(0deg)`
/// as not 2D, and `rotateZ(45deg)` as 2D.
const THREE_D_FUNCTIONS: &[&str] = &[
    "translatez",
    "translate3d",
    "rotatex",
    "rotatey",
    "rotate3d",
    "scalez",
    "scale3d",
    "matrix3d",
    "perspective",
];

/// Splits a function's arguments at the commas outside any nested call, so
/// `scale(min(2, 3), 4)` has two arguments rather than three.
fn split_arguments(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (at, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

/// A number as written, at full width, and the unit after it. Read as an
/// `f64` so that a value past the `float` range is held by the reader for
/// its kind, as Chrome holds `scale(1e39)`, rather than refused.
fn split_wide_number(text: &str) -> Option<(f64, String)> {
    let at = number_length(text);
    let value: f64 = text.get(..at)?.parse().ok()?;
    Some((value, text.get(at..)?.to_ascii_lowercase()))
}

/// One argument of a transform function, by the kind its position takes.
///
/// Each reads a math function through the shared evaluator or a value as
/// written, and holds the result where Chrome holds that kind. A math
/// function must give the kind's own dimension: Chrome refuses
/// `rotate(calc(0))`, a number, though it reads `rotate(0)`.
struct TransformArguments<'a> {
    args: Vec<&'a str>,
    units: FontUnits<'a>,
}

impl TransformArguments<'_> {
    fn math(&self, at: usize, dimension: Dimension) -> Option<Option<f64>> {
        let text = *self.args.get(at)?;
        is_math(text).then(|| {
            evaluate(
                text,
                &Expect {
                    percent: Percent::Refused,
                    units: &self.units,
                    channels: &[],
                },
            )
            .filter(|quantity| quantity.dimension == dimension)
            .map(Quantity::finished)
        })
    }

    /// A length in pixels, not yet held to any range: an absolute unit, or
    /// a bare zero.
    fn raw_length(&self, at: usize) -> Option<f64> {
        if let Some(math) = self.math(at, Dimension::LENGTH) {
            return math;
        }
        let (value, unit) = split_wide_number(self.args.get(at)?)?;
        match unit.as_str() {
            "" => (value == 0.0).then_some(0.0),
            unit => Some(value * f64::from(absolute_px(unit)?)),
        }
    }

    /// A length as Chrome keeps one: held to [`MIN_CSS_LENGTH`] and
    /// [`MAX_CSS_LENGTH`] and narrowed to a `float`.
    fn length(&self, at: usize) -> Option<f64> {
        let px = self.raw_length(at)?;
        Some(f64::from(px.clamp(MIN_CSS_LENGTH, MAX_CSS_LENGTH) as f32))
    }

    /// An angle in degrees, held to the `float` range. Chrome's result for
    /// an angle that large is an artefact of its arithmetic and is not
    /// matched here -- tests/assets/cases/css-transforms.json says so.
    fn angle(&self, at: usize) -> Option<f64> {
        let degrees = match self.math(at, Dimension::ANGLE) {
            Some(math) => math?,
            None => {
                let (value, unit) = split_wide_number(self.args.get(at)?)?;
                match unit.as_str() {
                    "" => (value == 0.0).then_some(0.0)?,
                    unit => value * angle_degrees(unit)?,
                }
            }
        };
        Some(held_to_float_range(degrees))
    }

    /// A number at full width, as `matrix()` and `rotate3d()` keep theirs.
    fn number(&self, at: usize) -> Option<f64> {
        let value = match self.math(at, Dimension::NUMBER) {
            Some(math) => math?,
            None => match split_wide_number(self.args.get(at)?)? {
                (value, unit) if unit.is_empty() => value,
                _ => return None,
            },
        };
        Some(held_to_float_range(value))
    }

    /// A scale factor: a number or a written percentage, narrowed to a
    /// `float` as Chrome keeps one, so `scale(0.1)` is 0.10000000149011612.
    /// A math function must be a number: Chrome refuses `scale(calc(50%))`
    /// though it reads `scale(50%)`.
    fn factor(&self, at: usize) -> Option<f64> {
        let value = match self.math(at, Dimension::NUMBER) {
            Some(math) => held_to_float_range(math?),
            None => {
                let (value, unit) = split_wide_number(self.args.get(at)?)?;
                let value = match unit.as_str() {
                    "" => value,
                    "%" => value / 100.0,
                    _ => return None,
                };
                held_to_float_range(value)
            }
        };
        Some(f64::from(value as f32))
    }
}

/// A finished value held where Chrome holds a transform's numbers: a finite
/// one to the `float` range, so `scale(1e39)` is 3.4028234663852886e38, and
/// an infinity to the largest `f64`, which `matrix(calc(infinity), ...)`
/// reports and which a `float` scale factor narrows back to an infinity, as
/// `scale(calc(infinity))` reports.
fn held_to_float_range(value: f64) -> f64 {
    match value.is_infinite() {
        true => f64::MAX.copysign(value),
        false => value.clamp(-f64::from(f32::MAX), f64::from(f32::MAX)),
    }
}

/// One CSS `transform` function as the matrix it names, given in lower case,
/// and whether it is one of the [`THREE_D_FUNCTIONS`].
///
/// Every argument goes through the readers on [`TransformArguments`], so a
/// unit CSS does not define is refused here for the same reason it is
/// refused in a filter: `rotate(45)` has no unit and browsers reject it.
fn transform_function(text: &str) -> Option<(Matrix4, bool)> {
    let (name, inner) = text.split_once('(')?;
    // The list was closed before it was split, so a call missing its `)`
    // has text after it, which is refused here.
    let inner = inner.strip_suffix(')')?;
    let args = TransformArguments {
        args: split_arguments(inner),
        units: FontUnits::absolute(),
    };
    let length = |at| args.length(at);
    let angle = |at| args.angle(at);
    let number = |at| args.number(at);
    let factor = |at| args.factor(at);
    let tan = |degrees: f64| degrees.to_radians().tan();
    let matrix = match (name, args.args.len()) {
        ("translatex", 1) => Matrix4::translation(length(0)?, 0.0, 0.0),
        ("translatey", 1) => Matrix4::translation(0.0, length(0)?, 0.0),
        ("translatez", 1) => Matrix4::translation(0.0, 0.0, length(0)?),
        ("translate", 1) => Matrix4::translation(length(0)?, 0.0, 0.0),
        ("translate", 2) => Matrix4::translation(length(0)?, length(1)?, 0.0),
        ("translate3d", 3) => {
            Matrix4::translation(length(0)?, length(1)?, length(2)?)
        }
        ("rotatex", 1) => Matrix4::rotation(1.0, 0.0, 0.0, angle(0)?),
        ("rotatey", 1) => Matrix4::rotation(0.0, 1.0, 0.0, angle(0)?),
        ("rotate" | "rotatez", 1) => {
            Matrix4::rotation(0.0, 0.0, 1.0, angle(0)?)
        }
        ("rotate3d", 4) => {
            Matrix4::rotation(number(0)?, number(1)?, number(2)?, angle(3)?)
        }
        ("scalex", 1) => Matrix4::scale(factor(0)?, 1.0, 1.0),
        ("scaley", 1) => Matrix4::scale(1.0, factor(0)?, 1.0),
        ("scalez", 1) => Matrix4::scale(1.0, 1.0, factor(0)?),
        // `scale(2)` scales both axes; `scale(2, 3)` names them apart.
        ("scale", 1) => Matrix4::scale(factor(0)?, factor(0)?, 1.0),
        ("scale", 2) => Matrix4::scale(factor(0)?, factor(1)?, 1.0),
        ("scale3d", 3) => Matrix4::scale(factor(0)?, factor(1)?, factor(2)?),
        ("skewx" | "skew", 1) => {
            Matrix4::planar(1.0, 0.0, tan(angle(0)?), 1.0, 0.0, 0.0)
        }
        ("skewy", 1) => {
            Matrix4::planar(1.0, tan(angle(0)?), 0.0, 1.0, 0.0, 0.0)
        }
        ("skew", 2) => {
            Matrix4::planar(1.0, tan(angle(1)?), tan(angle(0)?), 1.0, 0.0, 0.0)
        }
        ("matrix", 6) => Matrix4::planar(
            number(0)?,
            number(1)?,
            number(2)?,
            number(3)?,
            number(4)?,
            number(5)?,
        ),
        // Sixteen values, column by column, as `DOMMatrix` lists them.
        ("matrix3d", 16) => {
            let mut m = Matrix4::IDENTITY;
            for (at, cell) in m.0.iter_mut().flatten().enumerate() {
                *cell = number(at)?;
            }
            m
        }
        // `none` is no perspective, a negative distance is refused, and
        // zero is taken as one pixel, which is what Chrome reports for
        // `perspective(0px)`. A computed distance below zero is held at zero
        // rather than refused: Chrome reads `perspective(calc(-10px))` as
        // `perspective(0px)`. Only `m34` is set, and no composition carries
        // `m34` into `a` through `f`, so the 2D result is the same whatever
        // the distance.
        ("perspective", 1) if args.args[0] == "none" => Matrix4::IDENTITY,
        ("perspective", 1) => {
            let distance = match args.math(0, Dimension::LENGTH) {
                Some(math) => math?.max(0.0),
                None => args.raw_length(0).filter(|px| *px >= 0.0)?,
            };
            let mut m = Matrix4::IDENTITY;
            m.0[2][3] = -1.0 / distance.max(1.0);
            m
        }
        // A name this module knows, given the wrong number of arguments, or
        // a name it does not know. Refused rather than filled in: `scale()`
        // is not `scale(1)`, and `translate3d(1px, 2px)` is not a
        // translation by zero in `z`.
        _ => return None,
    };
    Some((matrix, THREE_D_FUNCTIONS.contains(&name)))
}

/// A CSS `transform` list as the matrix it composes to, in the sixteen
/// values `DOMMatrix.toFloat64Array` lists, and whether it is 2D.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TransformList {
    /// `m11` through `m44`, column by column.
    pub matrix: [f64; 16],
    /// False when any function in the list is one of the
    /// [`THREE_D_FUNCTIONS`], whatever its arguments, as `DOMMatrix.is2D`
    /// reports it.
    pub is_2d: bool,
}

/// A CSS `transform` list, as `DOMMatrix.setMatrixValue` reads one.
///
/// `"none"` and the empty string are the identity, which is how the property
/// spells "no transform". Names and units are case-insensitive, as they are
/// in CSS: Chrome reads `ROTATE(45DEG)`. A function left open at the end is
/// closed, as CSS Syntax 3 closes a block where the input ends: Chrome reads
/// `translate(10px) rotate(45deg` as the two functions.
///
/// The whole string is refused if any function in it is, for the reason
/// [`parse_filter`] refuses a chain: a transform list missing one step is a
/// different place on the canvas, and dropping the step nobody could read is
/// how a typo becomes a rendering bug.
pub(crate) fn parse_transform_list(text: &str) -> Option<TransformList> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() || text == "none" {
        return Some(TransformList {
            matrix: Matrix4::IDENTITY.flat(),
            is_2d: true,
        });
    }
    let closed = close_open_blocks(&text);
    let (matrix, is_3d) = split_functions(&closed).into_iter().try_fold(
        (Matrix4::IDENTITY, false),
        |(out, is_3d), part| {
            let (step, step_is_3d) = transform_function(part)?;
            Some((out.then(&step), is_3d || step_is_3d))
        },
    )?;
    Some(TransformList {
        matrix: matrix.flat(),
        is_2d: !is_3d,
    })
}

/// A CSS `transform` list as the 2D matrix it composes to, by
/// [`parse_transform_list`]'s reading.
pub(crate) fn parse_transform(text: &str) -> Option<Affine> {
    parse_transform_list(text)
        .map(|list| Matrix4::from_flat(list.matrix).to_affine())
}

#[cfg(test)]
mod transform_tests {
    use super::{parse_transform, parse_transform_list};
    use crate::geometry::Affine;

    /// About eight `f32` ulp at these magnitudes.
    const TOL: f64 = 2e-6;

    /// `tan(10 degrees)`, from a table rather than from `skew_x_degrees`,
    /// which is on the path under test.
    const TAN_10: f64 = 0.176_326_980_708_464_6;
    /// `tan(20 degrees)`.
    const TAN_20: f64 = 0.363_970_234_266_202_34;
    /// `cos(45 degrees)` and `sin(45 degrees)`, which are both
    /// `1 / sqrt(2)`. Taken from `std` rather than written out: the value is
    /// still independent of the code under test, and a literal here is a
    /// digit sequence nobody can check by eye.
    const ROOT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
    /// `cos(1 radian)` and `sin(1 radian)`.
    const COS_1: f64 = 0.540_302_305_868_139_7;
    const SIN_1: f64 = 0.841_470_984_807_896_5;

    fn six(text: &str) -> [f32; 6] {
        let m = parse_transform(text).expect("parses");
        [m.a, m.b, m.c, m.d, m.tx, m.ty]
    }

    fn close(actual: f32, expected: f64) {
        assert!(
            (f64::from(actual) - expected).abs() <= TOL,
            "expected {expected}, got {actual}"
        );
    }

    /// The values a browser produces for each function, measured on the
    /// JavaScript side before any of this was written.
    #[test]
    fn each_function_gives_what_a_browser_gives() {
        assert_eq!(
            six("translate(10px, 20px)"),
            [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]
        );
        assert_eq!(six("translate(10px)"), [1.0, 0.0, 0.0, 1.0, 10.0, 0.0]);
        assert_eq!(six("translateX(5px)"), [1.0, 0.0, 0.0, 1.0, 5.0, 0.0]);
        assert_eq!(six("translateY(5px)"), [1.0, 0.0, 0.0, 1.0, 0.0, 5.0]);
        assert_eq!(six("scale(2)"), [2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
        assert_eq!(six("scale(2, 3)"), [2.0, 0.0, 0.0, 3.0, 0.0, 0.0]);
        assert_eq!(six("scaleX(2)"), [2.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(six("matrix(1,2,3,4,5,6)"), [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);

        let rotated = six("rotate(45deg)");
        close(rotated[0], ROOT_HALF);
        close(rotated[1], ROOT_HALF);

        // A quarter turn: exactly the same rotation in a different unit.
        let turned = six("rotate(0.25turn)");
        close(turned[0], 0.0);
        close(turned[1], 1.0);

        let radians = six("rotate(1rad)");
        close(radians[0], COS_1);
        close(radians[1], SIN_1);

        // `skew(ax, ay)` shears x by the first and y by the second, so the
        // first lands in `c` and the second in `b`.
        let skewed = six("skew(10deg, 20deg)");
        close(skewed[2], TAN_10);
        close(skewed[1], TAN_20);
        close(skewed[0], 1.0);
        close(skewed[3], 1.0);

        let skewed_x = six("skewX(10deg)");
        close(skewed_x[2], TAN_10);
        assert_eq!(skewed_x[1], 0.0);
    }

    /// The single-axis names are not read as their shorter prefixes:
    /// `transform_function` takes the whole name before the parenthesis and
    /// matches it exactly, so `translateX` is never `translate` with an `X`
    /// left over.
    #[test]
    fn a_long_name_is_not_read_as_its_prefix() {
        assert_eq!(six("translateX(5px)"), [1.0, 0.0, 0.0, 1.0, 5.0, 0.0]);
        assert_eq!(six("scaleX(2)"), [2.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        close(six("skewX(10deg)")[2], TAN_10);
        // And the short names still parse, which is the other half.
        assert_eq!(six("translate(5px)"), [1.0, 0.0, 0.0, 1.0, 5.0, 0.0]);
        assert_eq!(six("scale(2)"), [2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
    }

    /// A list composes left to right, and the case is chosen so that the two
    /// orders disagree: `translate` then `rotate` keeps the translation where
    /// it was, and the reverse would rotate it to `(7.07, 7.07)`.
    #[test]
    fn a_list_applies_left_to_right() {
        let m = six("translate(10px) rotate(45deg)");
        close(m[0], ROOT_HALF);
        close(m[4], 10.0);
        close(m[5], 0.0);

        // The same two the other way round, to show the fixture can tell them
        // apart: the translation is now rotated.
        let reversed = six("rotate(45deg) translate(10px)");
        close(reversed[4], 10.0 * ROOT_HALF);
        close(reversed[5], 10.0 * ROOT_HALF);
    }

    /// A case that must NOT discriminate, kept so nobody tidies the fixture
    /// above into something measuring the same thing twice. Two scales
    /// commute, so their order cannot be read off the result -- which is why
    /// the ordering test uses a translation and a rotation, which do not.
    ///
    /// If this one ever starts telling the two apart, the composition is
    /// doing something other than multiplying matrices.
    #[test]
    fn two_scales_cannot_tell_their_order_apart() {
        assert_eq!(six("scale(2) scale(3)"), six("scale(3) scale(2)"));
        assert_eq!(six("scale(2) scale(3)"), [6.0, 0.0, 0.0, 6.0, 0.0, 0.0]);
    }

    /// How far a value computed here in `f64` may sit from Chrome's, which is
    /// `f64` too: a few ulps at the largest magnitude in the 3D rows, 36.
    const WIDE_TOL: f64 = 1e-12;

    /// The half of tests/assets/cases/css-transforms.json that
    /// `Affine::from_css` cannot show: all sixteen values of a 3D list, and
    /// `is2D`, which `DOMMatrix` reports from this reading.
    #[test]
    fn a_list_reports_every_value_and_whether_it_is_2d() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/assets/cases/css-transforms.json"
        ))
        .expect("the case file is JSON");
        let cases = table["cases"].as_array().expect("a list of cases");
        let mut checked = 0;
        for case in cases.iter().filter(|case| case.get("matrix").is_some()) {
            let css = case["css"].as_str().expect("each case names its css");
            let list = parse_transform_list(css).expect("Chrome reads it");
            let is_2d = case.get("is2D").is_none_or(|v| v == true);
            assert_eq!(list.is_2d, is_2d, "{css:?}: is2D");
            if let Some(want) = case.get("matrix4") {
                let want = want.as_array().expect("sixteen values");
                for (at, (want, got)) in
                    want.iter().zip(list.matrix).enumerate()
                {
                    let want = want.as_f64().expect("a number");
                    assert!(
                        (want - got).abs() <= WIDE_TOL,
                        "{css:?}: value {at} is {got}, Chrome {want}"
                    );
                }
                checked += 1;
            }
        }
        // Every 3D row carries its sixteen values; a table that lost them
        // would pass the loop above without checking anything.
        assert!(checked >= 30, "only {checked} rows carry sixteen values");
    }

    /// The values Chrome 152 reports past the float range, which the case
    /// file cannot hold since JSON has no infinity. A scale factor is a
    /// `float`, so an infinite one stays infinite; a `matrix()` value is a
    /// `double`, so an infinite one is the largest `double`; and a finite one
    /// past the float range is the largest `float` in either.
    #[test]
    fn a_number_past_the_float_range_is_held_where_chrome_holds_it() {
        let first = |css: &str| parse_transform_list(css).expect(css).matrix;
        assert_eq!(first("scale(calc(infinity))")[0], f64::INFINITY);
        assert_eq!(first("scale(calc(-infinity))")[0], f64::NEG_INFINITY);
        assert_eq!(first("matrix(calc(infinity), 0, 0, 1, 0, 0)")[0], f64::MAX);
        assert_eq!(
            first("matrix(1, 0, 0, 1, calc(-infinity), 0)")[12],
            -f64::MAX
        );
        assert_eq!(first("scale(1e39)")[0], f64::from(f32::MAX));
        assert_eq!(first("scale(calc(1e39))")[0], f64::from(f32::MAX));
        assert_eq!(
            first("matrix(1e39, 0, 0, 1, 0, 0)")[0],
            f64::from(f32::MAX)
        );
    }

    /// And `none` is the whole value or nothing: CSS does not allow it as a
    /// list item, so this is refused rather than read as an identity step.
    #[test]
    fn none_is_not_a_list_item() {
        assert_eq!(parse_transform("none rotate(45deg)"), None);
    }

    #[test]
    fn the_empty_forms_are_the_identity() {
        for text in ["none", "NONE", "", "   "] {
            assert_eq!(
                parse_transform(text),
                Some(Affine::IDENTITY),
                "{text:?}"
            );
        }
    }

    #[test]
    fn what_it_refuses() {
        // CSS requires a unit on an angle and browsers reject this.
        assert_eq!(parse_transform("rotate(45)"), None);
        // A function this module does not know.
        assert_eq!(parse_transform("garbage(1)"), None);
        // A known function with the wrong number of arguments.
        assert_eq!(parse_transform("matrix(1,2,3)"), None);
        assert_eq!(parse_transform("scale()"), None);
        // A unit whose pixel value needs a font size.
        assert_eq!(parse_transform("translate(2em)"), None);
        // One unreadable step refuses the whole list rather than keeping the
        // readable half, which would put the drawing somewhere else entirely.
        assert_eq!(parse_transform("translate(10px) garbage(1)"), None);
    }
}
