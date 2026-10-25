//! The CSS math functions -- `calc()` and the rest of CSS Values 4 and 5 --
//! as one typed evaluator.
//!
//! Every value carries its dimension as exponents of length, angle and
//! percentage, so `calc(1px * 2px / 1px)` is a length, `calc(1px / 1px)` a
//! number and `calc(2px * 2px)` an area no property takes. Addition needs
//! both sides to have the same dimension, which is what refuses
//! `calc(1px + 2)`. The caller says which dimension it takes and whether a
//! percentage may appear at all, since `blur(calc(10% + 1px))` is refused
//! for the `%` rather than for the sum.
//!
//! Values are `f64`, as Chrome evaluates them, and keep their signed zeros
//! and infinities until the top: `calc(1 / round(-0.4, 1))` is negative
//! infinity because the round gave `-0`. Only the finished value is
//! censored, by [`Quantity::finished`].
//!
//! Every function and constant here was measured in Chrome 152's
//! `ctx.filter`, accepted and refused: `random()`, `calc-size()`,
//! `anchor-size()`, `calc-mix()`, `mix()`, `if()`, `env()`, `var()`,
//! `attr()`, `container-progress()` and `media-progress()` are refused
//! there, and so are refused here.

use super::{
    FontUnits, angle_degrees, is_ident_code_point, is_ident_start_code_point,
    number_length,
};

/// A dimension, as the exponent of each base type it is built from.
///
/// Length is in pixels, angle in degrees and percentage as a fraction, so
/// `calc(50% / 1%)` is 50 and `calc(1deg / 1rad)` is pi over 180.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Dimension {
    length: i8,
    angle: i8,
    percent: i8,
}

impl Dimension {
    /// An angle, in degrees.
    pub(crate) const ANGLE: Self = Self {
        length: 0,
        angle: 1,
        percent: 0,
    };
    /// A length, in pixels.
    pub(crate) const LENGTH: Self = Self {
        length: 1,
        angle: 0,
        percent: 0,
    };
    /// A plain number.
    pub(crate) const NUMBER: Self = Self {
        length: 0,
        angle: 0,
        percent: 0,
    };
    /// A percentage, as a fraction.
    pub(crate) const PERCENT: Self = Self {
        length: 0,
        angle: 0,
        percent: 1,
    };

    /// `None` when an exponent leaves `i8`, which no expression a caller
    /// would write reaches and an adversarial one can.
    fn times(self, other: Self) -> Option<Self> {
        Some(Self {
            length: self.length.checked_add(other.length)?,
            angle: self.angle.checked_add(other.angle)?,
            percent: self.percent.checked_add(other.percent)?,
        })
    }

    fn over(self, other: Self) -> Option<Self> {
        Some(Self {
            length: self.length.checked_sub(other.length)?,
            angle: self.angle.checked_sub(other.angle)?,
            percent: self.percent.checked_sub(other.percent)?,
        })
    }
}

/// A value and its dimension.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Quantity {
    /// In pixels, degrees or a fraction, whichever the dimension is built of.
    pub value: f64,
    /// What the value measures.
    pub dimension: Dimension,
}

impl Quantity {
    fn new(value: f64, dimension: Dimension) -> Self {
        Self { value, dimension }
    }

    fn number(value: f64) -> Self {
        Self::new(value, Dimension::NUMBER)
    }

    fn with(self, value: f64) -> Self {
        Self::new(value, self.dimension)
    }

    /// The value as a property receives it: NaN becomes zero, as CSS Values 4
    /// section 10.9 censors it at the top level, and an infinity stays for
    /// the caller to clamp into its own range. Chrome draws
    /// `blur(calc(NaN * 1px))` as `blur(0px)`.
    pub(crate) fn finished(self) -> f64 {
        match self.value.is_nan() {
            true => 0.0,
            false => self.value,
        }
    }
}

/// What a `%` inside a math function means, which depends on the property.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Percent {
    /// Not allowed at all. A property that takes no percentage refuses the
    /// token, not just the result: Chrome refuses `blur(calc(10% + 1px))`.
    Refused,
    /// A dimension of its own, as in `brightness()`, which takes a number or
    /// a percentage and refuses `calc(50% + 0.5)`.
    Own,
    /// A length, as that percentage of this many pixels -- a font size's
    /// percentage is of its parent's size, so Chrome reads
    /// `calc(50% + 2px)` in a canvas font as 7px.
    Of(f64),
}

/// What a math function may contain, which depends on where it is written.
#[derive(Clone, Copy)]
pub(crate) struct Expect<'a> {
    /// What a `%` means here.
    pub percent: Percent,
    /// What the font-relative lengths resolve against.
    pub units: &'a FontUnits<'a>,
    /// The keywords a relative colour binds, in lower case, each to a plain
    /// number: `h`, `s`, `l` and `alpha` inside `hsl(from ...)`. Empty
    /// everywhere else. CSS Color 5 types every channel keyword as a
    /// `<number>`, so `calc(h + 30deg)` is refused as `calc(1 + 30deg)` is.
    pub channels: &'a [(&'a str, f64)],
}

/// The names that open a math function, in lower case.
///
/// `-webkit-calc` is Chrome's alias for `calc`, and reads the same.
const MATH_FUNCTIONS: &[&str] = &[
    "calc",
    "-webkit-calc",
    "min",
    "max",
    "clamp",
    "round",
    "mod",
    "rem",
    "abs",
    "sign",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "sqrt",
    "hypot",
    "log",
    "exp",
    "progress",
    "sibling-index",
    "sibling-count",
];

/// Whether `text` opens with a math function's name and its `(`, in any
/// case. A caller that finds one evaluates the whole token rather than
/// reading it as a plain dimension.
pub(crate) fn is_math(text: &str) -> bool {
    text.split_once('(').is_some_and(|(name, _)| {
        MATH_FUNCTIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(name))
    })
}

/// `text`, which must be one math function call and nothing else.
///
/// `None` for anything that is not exactly one call, and for a syntax or
/// type error anywhere inside. Which dimension the result may have is the
/// caller's to check, since `brightness()` takes a number or a percentage.
/// The value is not yet [`finished`](Quantity::finished).
pub(crate) fn evaluate(text: &str, expect: &Expect) -> Option<Quantity> {
    let mut parser = Parser {
        text: text.trim_ascii(),
        at: 0,
        expect,
    };
    let quantity = parser.function()?;
    (parser.at == parser.text.len()).then_some(quantity)
}

/// `text` as one number, percentage or dimension and nothing else: a value
/// written without a math function, read by the same rules as one inside.
///
/// `None` for anything else, a constant included, since only a math function
/// may name one: Chrome refuses `hsl(from red e s l)` and reads
/// `hsl(from red calc(e) s l)`.
pub(crate) fn literal(text: &str, expect: &Expect) -> Option<Quantity> {
    let mut parser = Parser {
        text: text.trim_ascii(),
        at: 0,
        expect,
    };
    let quantity = parser.dimension()?;
    (parser.at == parser.text.len()).then_some(quantity)
}

struct Parser<'t, 'e> {
    text: &'t str,
    at: usize,
    expect: &'e Expect<'e>,
}

/// The rounding strategies `round()` takes as its optional first argument.
#[derive(Clone, Copy)]
enum Strategy {
    Nearest,
    Up,
    Down,
    ToZero,
}

impl Parser<'_, '_> {
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.text.as_bytes().get(self.at + offset).copied()
    }

    /// Skips whitespace and says whether there was any.
    fn space(&mut self) -> bool {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.at += 1;
        }
        self.at > start
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.peek() == Some(byte)).then(|| self.at += 1)
    }

    /// An identifier: what CSS Syntax 3 allows in a name, its code points
    /// those [`is_ident_code_point`] takes. No unit, constant or function
    /// here is anything but ASCII, so a name reaching past ASCII is read
    /// whole and then matches nothing, rather than stopping short of it.
    fn identifier(&mut self) -> Option<&str> {
        let start = self.at;
        let rest = &self.text[self.at..];
        let mut chars = rest.chars();
        let opens = match chars.next() {
            Some('-') => chars
                .next()
                .is_some_and(|c| is_ident_start_code_point(c) || c == '-'),
            Some(c) => is_ident_start_code_point(c),
            None => false,
        };
        if !opens {
            return None;
        }
        self.at += rest
            .find(|c: char| !is_ident_code_point(c))
            .unwrap_or(rest.len());
        Some(&self.text[start..self.at])
    }

    /// `calc-sum`: products joined by `+` and `-`, which CSS Values 4
    /// requires to have whitespace on both sides -- Chrome refuses
    /// `calc(1px+2px)` and `calc(1px -1px)`, the second because `-1px` is a
    /// signed number standing next to the first.
    fn sum(&mut self) -> Option<Quantity> {
        let mut left = self.product()?;
        loop {
            let before = self.at;
            let spaced = self.space();
            let operator = self.peek();
            let followed =
                self.peek_at(1).is_some_and(|c| c.is_ascii_whitespace());
            if !(spaced && matches!(operator, Some(b'+' | b'-')) && followed) {
                self.at = before;
                return Some(left);
            }
            self.at += 1;
            self.space();
            let right = self.product()?;
            if left.dimension != right.dimension {
                return None;
            }
            left.value = match operator {
                Some(b'+') => left.value + right.value,
                _ => left.value - right.value,
            };
        }
    }

    /// `calc-product`: values joined by `*` and `/`, which multiply and
    /// divide their dimensions too. Division by zero is an infinity, or NaN
    /// for zero over zero, as IEEE 754 and CSS Values 4 both give it.
    fn product(&mut self) -> Option<Quantity> {
        let mut left = self.value()?;
        loop {
            let before = self.at;
            self.space();
            let operator = self.peek();
            if !matches!(operator, Some(b'*' | b'/')) {
                self.at = before;
                return Some(left);
            }
            self.at += 1;
            self.space();
            let right = self.value()?;
            left = match operator {
                Some(b'*') => Quantity::new(
                    left.value * right.value,
                    left.dimension.times(right.dimension)?,
                ),
                _ => Quantity::new(
                    left.value / right.value,
                    left.dimension.over(right.dimension)?,
                ),
            };
        }
    }

    /// `calc-value`: a number, a dimension, a percentage, a constant or a
    /// channel keyword, a parenthesised sum, or another math function.
    fn value(&mut self) -> Option<Quantity> {
        match self.peek()? {
            b'(' => {
                self.at += 1;
                self.space();
                let inner = self.sum()?;
                self.space();
                self.eat(b')')?;
                Some(inner)
            }
            c if c.is_ascii_digit()
                || c == b'.'
                || ((c == b'+' || c == b'-')
                    && self.peek_at(1).is_some_and(|next| {
                        next.is_ascii_digit() || next == b'.'
                    })) =>
            {
                self.dimension()
            }
            _ => {
                let before = self.at;
                let name = self.identifier()?.to_ascii_lowercase();
                if self.peek() == Some(b'(') {
                    self.at = before;
                    return self.function();
                }
                self.expect
                    .channels
                    .iter()
                    .find(|(channel, _)| *channel == name)
                    .map(|&(_, value)| value)
                    .or_else(|| constant(&name))
                    .map(Quantity::number)
            }
        }
    }

    /// A number, with a unit or a `%` or neither.
    fn dimension(&mut self) -> Option<Quantity> {
        let length = number_length(self.rest());
        let value: f64 = self.rest().get(..length)?.parse().ok()?;
        self.at += length;
        if self.eat(b'%').is_some() {
            let fraction = value / 100.0;
            return match self.expect.percent {
                Percent::Refused => None,
                Percent::Own => {
                    Some(Quantity::new(fraction, Dimension::PERCENT))
                }
                Percent::Of(basis) => {
                    Some(Quantity::new(fraction * basis, Dimension::LENGTH))
                }
            };
        }
        let Some(unit) = self.identifier() else {
            return Some(Quantity::number(value));
        };
        let unit = unit.to_ascii_lowercase();
        if let Some(px) = self.expect.units.px_per(&unit) {
            return Some(Quantity::new(
                value * f64::from(px),
                Dimension::LENGTH,
            ));
        }
        angle_degrees(&unit)
            .map(|per| Quantity::new(value * per, Dimension::ANGLE))
    }

    /// One math function call, from its name to its `)`.
    fn function(&mut self) -> Option<Quantity> {
        let name = self.identifier()?.to_ascii_lowercase();
        self.eat(b'(')?;
        self.space();
        let result = match name.as_str() {
            "calc" | "-webkit-calc" => {
                let inner = self.sum()?;
                self.space();
                inner
            }
            "sibling-index" | "sibling-count" => {
                // A canvas here is not in a document, so it is its own only
                // sibling: Chrome gives both 1 for a detached canvas.
                Quantity::number(DETACHED_SIBLING)
            }
            "round" => self.round()?,
            "clamp" => self.clamp()?,
            _ => {
                let arguments = self.arguments()?;
                apply(&name, &arguments)?
            }
        };
        self.eat(b')')?;
        Some(result)
    }

    /// Comma-separated sums, up to the closing `)`, which is left in place.
    fn arguments(&mut self) -> Option<Vec<Quantity>> {
        let mut arguments = vec![self.sum()?];
        loop {
            self.space();
            if self.eat(b',').is_none() {
                return Some(arguments);
            }
            self.space();
            arguments.push(self.sum()?);
        }
    }

    /// `round(<strategy>?, A, B?)`. The interval may be left out only when
    /// `A` is a plain number: Chrome reads `round(2.5)` as 3 and refuses
    /// `round(2.5px)`.
    fn round(&mut self) -> Option<Quantity> {
        let before = self.at;
        let strategy = match self.identifier().map(str::to_ascii_lowercase) {
            Some(name) if self.peek() != Some(b'(') => {
                let strategy = match name.as_str() {
                    "nearest" => Strategy::Nearest,
                    "up" => Strategy::Up,
                    "down" => Strategy::Down,
                    "to-zero" => Strategy::ToZero,
                    // A constant, which is the value rather than a strategy.
                    _ => {
                        self.at = before;
                        Strategy::Nearest
                    }
                };
                if self.at != before {
                    self.space();
                    self.eat(b',')?;
                    self.space();
                }
                strategy
            }
            _ => {
                self.at = before;
                Strategy::Nearest
            }
        };
        let arguments = self.arguments()?;
        let (value, step) = match arguments[..] {
            [value] if value.dimension == Dimension::NUMBER => {
                (value, Quantity::number(1.0))
            }
            [value, step] if value.dimension == step.dimension => (value, step),
            _ => return None,
        };
        Some(value.with(round(strategy, value.value, step.value)))
    }

    /// `clamp(MIN, VAL, MAX)`, where either bound may be `none`.
    fn clamp(&mut self) -> Option<Quantity> {
        let low = self.bound()?;
        let comma = |parser: &mut Self| {
            parser.space();
            parser.eat(b',')?;
            parser.space();
            Some(())
        };
        comma(self)?;
        let value = self.sum()?;
        comma(self)?;
        let high = self.bound()?;
        self.space();
        if [low, high]
            .iter()
            .flatten()
            .any(|bound| bound.dimension != value.dimension)
        {
            return None;
        }
        let nan = [low, high]
            .iter()
            .flatten()
            .any(|bound| bound.value.is_nan())
            || value.value.is_nan();
        // The lower bound wins when the two cross: Chrome reads
        // `clamp(3px, 1px, 2px)` as 3px, as CSS Values 4 defines it.
        let clamped = match nan {
            true => f64::NAN,
            false => {
                let capped = high
                    .map_or(value.value, |high| value.value.min(high.value));
                low.map_or(capped, |low| capped.max(low.value))
            }
        };
        Some(value.with(clamped))
    }

    /// One bound of `clamp()`: a sum, or `none` for no bound on that side.
    fn bound(&mut self) -> Option<Option<Quantity>> {
        let before = self.at;
        match self.identifier().map(str::to_ascii_lowercase) {
            Some(name) if name == "none" && self.peek() != Some(b'(') => {
                Some(None)
            }
            _ => {
                self.at = before;
                self.sum().map(Some)
            }
        }
    }
}

/// What `sibling-index()` and `sibling-count()` give for a canvas that is
/// not in a document: it is the first of one.
const DETACHED_SIBLING: f64 = 1.0;

/// A math constant by its lower-case name. Only inside a math function:
/// Chrome refuses `brightness(e)` and reads `brightness(calc(e))`.
fn constant(name: &str) -> Option<f64> {
    match name {
        "e" => Some(std::f64::consts::E),
        "pi" => Some(std::f64::consts::PI),
        "infinity" => Some(f64::INFINITY),
        "-infinity" => Some(f64::NEG_INFINITY),
        "nan" => Some(f64::NAN),
        _ => None,
    }
}

/// The functions whose arguments are a plain comma-separated list.
fn apply(name: &str, arguments: &[Quantity]) -> Option<Quantity> {
    let same = || {
        arguments
            .iter()
            .all(|argument| argument.dimension == arguments[0].dimension)
    };
    let numbers = || {
        arguments
            .iter()
            .all(|argument| argument.dimension == Dimension::NUMBER)
    };
    let first = arguments[0];
    let any_nan = || arguments.iter().any(|argument| argument.value.is_nan());
    match (name, arguments.len()) {
        ("min" | "max", _) if same() => {
            let pick = |a: f64, b: f64| match name {
                "min" => a.min(b),
                _ => a.max(b),
            };
            let value = match any_nan() {
                true => f64::NAN,
                false => arguments
                    .iter()
                    .map(|argument| argument.value)
                    .reduce(pick)?,
            };
            Some(first.with(value))
        }
        ("hypot", _) if same() => {
            let value = arguments
                .iter()
                .map(|argument| argument.value)
                .fold(0.0, f64::hypot);
            Some(first.with(value))
        }
        ("mod" | "rem", 2) if same() => {
            let modulus = arguments[1].value;
            let value = match name {
                "mod" => modulo(first.value, modulus),
                _ => remainder(first.value, modulus),
            };
            Some(first.with(value))
        }
        ("abs", 1) => Some(first.with(first.value.abs())),
        // The result is a number whatever the argument measured. Zero keeps
        // its sign, which `f64::signum` does not: `1 / sign(-0)` is negative
        // infinity in Chrome.
        ("sign", 1) => Some(Quantity::number(match first.value {
            value if value.is_nan() || value == 0.0 => value,
            value => value.signum(),
        })),
        ("sin" | "cos" | "tan", 1) => {
            let radians = match first.dimension {
                Dimension::NUMBER => first.value,
                Dimension::ANGLE => first.value.to_radians(),
                _ => return None,
            };
            Some(Quantity::number(match name {
                "sin" => radians.sin(),
                "cos" => radians.cos(),
                _ => tangent(first, radians),
            }))
        }
        ("asin" | "acos" | "atan", 1) if numbers() => {
            let radians = match name {
                "asin" => first.value.asin(),
                "acos" => first.value.acos(),
                _ => first.value.atan(),
            };
            Some(Quantity::new(radians.to_degrees(), Dimension::ANGLE))
        }
        ("atan2", 2) if same() => Some(Quantity::new(
            first.value.atan2(arguments[1].value).to_degrees(),
            Dimension::ANGLE,
        )),
        ("pow", 2) if numbers() => {
            Some(Quantity::number(first.value.powf(arguments[1].value)))
        }
        ("sqrt", 1) if numbers() => Some(Quantity::number(first.value.sqrt())),
        ("exp", 1) if numbers() => Some(Quantity::number(first.value.exp())),
        ("log", 1) if numbers() => Some(Quantity::number(first.value.ln())),
        ("log", 2) if numbers() => {
            Some(Quantity::number(first.value.ln() / arguments[1].value.ln()))
        }
        ("progress", 3) if same() => {
            let [value, start, end] = [0, 1, 2].map(|at| arguments[at].value);
            Some(Quantity::number(progress(value, start, end)))
        }
        _ => None,
    }
}

/// `tan()`, exact at the quarter turns. The floating-point tangent of
/// `90deg` is about 1.6e16; CSS Values 4 gives positive infinity there and
/// negative infinity at `270deg`, and Chrome reads `tan(90deg)` as infinity.
/// Only an angle written in a unit can land exactly on one: a number is in
/// radians, and no `f64` is a right angle in radians.
fn tangent(argument: Quantity, radians: f64) -> f64 {
    const QUARTER_TURN: f64 = 90.0;
    const HALF_TURN: f64 = 180.0;
    if argument.dimension == Dimension::ANGLE
        && (argument.value - QUARTER_TURN).rem_euclid(HALF_TURN) == 0.0
    {
        return match (argument.value - QUARTER_TURN).rem_euclid(2.0 * HALF_TURN)
        {
            0.0 => f64::INFINITY,
            _ => f64::NEG_INFINITY,
        };
    }
    radians.tan()
}

/// `round()` by CSS Values 4 section 10.3, including its infinite cases.
///
/// The interval's sign is ignored, `nearest` breaks a tie upward, and a
/// zero result keeps the sign of the value -- `round(-0.4, 1)` is `-0`.
fn round(strategy: Strategy, value: f64, step: f64) -> f64 {
    let step = step.abs();
    if value.is_nan() || step.is_nan() || step == 0.0 {
        return f64::NAN;
    }
    if value.is_infinite() {
        return match step.is_infinite() {
            true => f64::NAN,
            false => value,
        };
    }
    if step.is_infinite() {
        return match strategy {
            Strategy::Nearest | Strategy::ToZero => 0.0_f64.copysign(value),
            Strategy::Up if value > 0.0 => f64::INFINITY,
            Strategy::Down if value < 0.0 => f64::NEG_INFINITY,
            _ if value == 0.0 => value,
            _ => 0.0_f64.copysign(value),
        };
    }
    let lower = (value / step).floor() * step;
    let upper = (value / step).ceil() * step;
    let rounded = match strategy {
        Strategy::Up => upper,
        Strategy::Down => lower,
        Strategy::ToZero if value < 0.0 => upper,
        Strategy::ToZero => lower,
        Strategy::Nearest if value - lower < upper - value => lower,
        Strategy::Nearest => upper,
    };
    match rounded == 0.0 {
        true => 0.0_f64.copysign(value),
        false => rounded,
    }
}

/// `mod()`: the result takes the sign of the modulus. An infinite modulus
/// returns the value when the two share a sign and NaN when they do not,
/// which Chrome then censors to zero: `mod(1, infinity)` is 1 and
/// `mod(-1, infinity)` is 0.
fn modulo(value: f64, modulus: f64) -> f64 {
    if modulus == 0.0 || value.is_infinite() {
        return f64::NAN;
    }
    if modulus.is_infinite() {
        return match value == 0.0
            || value.is_sign_negative() == modulus.is_sign_negative()
        {
            true => value,
            false => f64::NAN,
        };
    }
    value - modulus * (value / modulus).floor()
}

/// `rem()`: the result takes the sign of the value.
fn remainder(value: f64, modulus: f64) -> f64 {
    if modulus == 0.0 || value.is_infinite() {
        return f64::NAN;
    }
    if modulus.is_infinite() {
        return value;
    }
    value % modulus
}

/// `progress()`, clamped to `[0, 1]` as Chrome clamps it: `progress(15, 0,
/// 10)` is 1. Where the start and end meet, Chrome gives 1 for
/// `progress(5, 5, 5)`.
fn progress(value: f64, start: f64, end: f64) -> f64 {
    if start == end {
        return 1.0;
    }
    ((value - start) / (end - start)).clamp(0.0, 1.0)
}

/// What drawing cannot show: signed zeros, infinities and NaN before the
/// top-level censor. Every expectation was read from Chrome 152 through a
/// computed style -- `scale` for a number, `rotate` for an angle -- where
/// `1 / x` exposes a zero's sign as an infinity's.
#[cfg(test)]
mod tests {
    use super::*;

    fn number(text: &str) -> f64 {
        let units = FontUnits::assumed(16.0);
        let expect = Expect {
            percent: Percent::Own,
            units: &units,
            channels: &[],
        };
        let quantity = evaluate(text, &expect)
            .unwrap_or_else(|| panic!("{text:?} evaluates"));
        assert_eq!(quantity.dimension, Dimension::NUMBER, "{text:?}");
        quantity.value
    }

    #[test]
    fn a_zero_keeps_its_sign_until_it_is_divided_by() {
        // Chrome: `calc(1 / round(-0.4, 1))` and `calc(1 / sign(-0))` are
        // negative infinity, `calc(1/ calc(-0))` too.
        for text in [
            "calc(1 / round(-0.4, 1))",
            "calc(1 / sign(-0))",
            "calc(1 / calc(-0))",
            "calc(1 / round(-1, infinity))",
        ] {
            assert_eq!(number(text), f64::NEG_INFINITY, "{text}");
        }
        assert_eq!(number("calc(1 / round(1, infinity))"), f64::INFINITY);
    }

    #[test]
    fn round_mod_and_rem_meet_infinity_as_css_values_4_says() {
        // Chrome: round(up, 1, infinity) is infinity, round(infinity, 1) is
        // infinity, mod(1, infinity) 1, rem(1, infinity) 1, and
        // mod(-1, infinity), mod(infinity, 1) and round(5, 0) NaN.
        assert_eq!(number("round(up, 1, infinity)"), f64::INFINITY);
        assert_eq!(number("round(down, -1, infinity)"), f64::NEG_INFINITY);
        assert_eq!(number("round(infinity, 1)"), f64::INFINITY);
        assert_eq!(number("mod(1, infinity)"), 1.0);
        assert_eq!(number("rem(1, infinity)"), 1.0);
        for text in ["mod(-1, infinity)", "mod(infinity, 1)", "round(5, 0)"] {
            assert!(number(text).is_nan(), "{text}");
        }
    }

    #[test]
    fn the_quarter_turns_are_exact_tangents() {
        // Chrome reads `tan(90deg)` as infinity; floating point gives 1.6e16.
        assert_eq!(number("tan(90deg)"), f64::INFINITY);
        assert_eq!(number("tan(270deg)"), f64::NEG_INFINITY);
        assert_eq!(number("tan(-90deg)"), f64::NEG_INFINITY);
        assert!(number("tan(1.5707963267948966)").is_finite());
    }

    #[test]
    fn a_sum_needs_its_operator_spaced_and_its_sides_alike() {
        let units = FontUnits::assumed(16.0);
        let expect = Expect {
            percent: Percent::Refused,
            units: &units,
            channels: &[],
        };
        for text in [
            "calc(1px+2px)",
            "calc(1px -1px)",
            "calc(1px + 2)",
            "calc(2px * 2px) ",
            "calc(1px) junk",
            "calc(e",
        ] {
            let quantity = evaluate(text, &expect);
            assert!(
                quantity.is_none_or(|q| q.dimension != Dimension::LENGTH),
                "{text:?} is not a length"
            );
        }
        let area = evaluate("calc(2px * 3px)", &expect).expect("an area");
        assert_eq!(area.value, 6.0);
        assert_ne!(area.dimension, Dimension::LENGTH);
    }

    #[test]
    fn nan_censors_to_zero_only_when_finished() {
        let units = FontUnits::assumed(16.0);
        let expect = Expect {
            percent: Percent::Refused,
            units: &units,
            channels: &[],
        };
        let nan = evaluate("calc(NaN * 1px)", &expect).expect("a length");
        assert!(nan.value.is_nan());
        assert_eq!(nan.finished(), 0.0);
        // Chrome: min(1, nan) and clamp(1, nan, 3) are NaN, censored to 0.
        assert!(number("min(1, nan)").is_nan());
        assert!(number("clamp(1, nan, 3)").is_nan());
    }
}
