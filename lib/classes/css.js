//
// Parsers for properties that take CSS-style strings as values
//

"use strict";

// -- Font & Variant --------------------------------------------------------------------
//    https://developer.mozilla.org/en-US/docs/Web/CSS/font-variant
//    https://www.w3.org/TR/css-fonts-3/#font-size-prop

// How many parsed font and variant strings to remember.
//
// These caches were plain objects that nothing ever evicted, so every distinct
// string a process passed to `ctx.font` stayed for its life -- about 431 bytes
// each, 82 MB across 200,000. That is not an exotic shape of program: a font
// string carries a number whenever text is sized to fit, so a chart label
// scaled to its column, a counter, or a zoom animation adds an entry a frame.
//
// The parse is pure, so a cache is only ever a speed trade and an evicted
// entry reparses to the same value. A thousand covers reuse in any real
// drawing loop, which is a handful of strings set over and over.
const CACHE_LIMIT = 1024;

// A `Map` rather than an object also fixes a bug the object shape carried.
// `cache.font["constructor"]` is not `undefined` on a plain object, it is
// `Object`, so the miss test passed and the cache handed back a function where
// a parsed font belonged: `ctx.font = "constructor"` threw "failed to downcast
// any to object" from the Rust side. The same for `toString`, `__proto__`,
// `valueOf` and `hasOwnProperty`. A `Map` has no inherited keys, so those
// strings now fail to parse and are ignored, which is what the Canvas standard
// asks of an unparseable font and what a browser does.
class Memo {
  #entries = new Map();

  has(key) {
    return this.#entries.has(key);
  }

  get(key) {
    return this.#entries.get(key);
  }

  set(key, value) {
    // Re-inserting moves the key to the back of the iteration order, so an
    // entry a caller keeps asking for outlives the ones it does not.
    this.#entries.delete(key);
    this.#entries.set(key, value);
    if (this.#entries.size > CACHE_LIMIT) {
      this.#entries.delete(this.#entries.keys().next().value);
    }
    return value;
  }
}

var splitBy = require("string-split-by"),
  m,
  cache = { font: new Memo(), variant: new Memo(), filter: new Memo() };

// CSS keywords are ASCII case-insensitive, so these match in any case and
// the value is lowercased before it is used.
const stretchRE = /^(normal|(semi-|extra-|ultra-)?(condensed|expanded))$/i,
  parameterizedRE = /([\w-]+)\((.*?)\)/;

// -- The `font` shorthand ------------------------------------------------------
//
// Read as Chrome reads `ctx.font`. `Font::from_css` in src/context2d.rs reads
// the same grammar, and both are held to tests/assets/cases/css-fonts.json,
// which was measured in Chrome.

// What `em` and `%` resolve against: the canvas's default font is
// `10px sans-serif`, and Chrome reads `1em` there as 10px.
const CANVAS_DEFAULT_FONT_PX = 10;

// What `rem` resolves against: the root element's default, CSS's `medium`.
// Chrome reads `2rem` in a canvas font as 32px.
const ROOT_FONT_PX = 16;

// CSS Fonts 4's absolute-size keywords, in pixels at the default `medium` of
// 16px -- the table Chrome reads `xx-small` through `xxx-large` from.
const ABSOLUTE_FONT_SIZES = new Map([
  ["xx-small", 9],
  ["x-small", 10],
  ["small", 13],
  ["medium", 16],
  ["large", 18],
  ["x-large", 24],
  ["xx-large", 32],
  ["xxx-large", 48],
]);

// The step `larger` and `smaller` take from the parent size, which in a
// canvas is the default 10px: CSS Fonts 4 suggests 1.2, and Chrome reads
// `larger` as 12px and `smaller` as 8.33px.
const RELATIVE_FONT_SIZE_STEP = 1.2;

// Pixels per absolute length unit, as CSS Values 4 fixes them: 96 to the inch.
const PX_PER_UNIT = new Map([
  ["px", 1],
  ["pt", 96 / 72],
  ["pc", 96 / 6],
  ["in", 96],
  ["cm", 96 / 2.54],
  ["mm", 96 / 25.4],
  ["q", 96 / 25.4 / 4],
]);

// The weights CSS Fonts 4 names `normal` and `bold`.
const NORMAL_WEIGHT = 400,
  BOLD_WEIGHT = 700;

// The most digits `Number.prototype.toFixed` accepts.
const MAX_FIXED_DIGITS = 100;

// The most hex digits one CSS escape takes, per CSS Syntax 3 section 4.3.7.
const MAX_CSS_ESCAPE_HEX_DIGITS = 6;

// Values that are never a family name, written bare.
const CSS_WIDE_KEYWORDS = new Set([
  "inherit",
  "initial",
  "unset",
  "revert",
  "revert-layer",
  "default",
]);

// A CSS number and what follows it. A `.` or an `e` belongs to the number
// only when a digit follows, so `0.5em` is half an em and `5.px` is not a
// length at all, which Chrome agrees with.
const numberRE = /^([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:e[+-]?\d+)?)(.*)$/is,
  identifierRE = /^[\p{Alphabetic}_-][\p{Alphabetic}\p{N}_-]*$/u,
  hexRE = /[0-9a-f]/i;

const splitNumber = (text) => {
  const match = numberRE.exec(text);
  return match ? [parseFloat(match[1]), match[2]] : null;
};

// One length in a font shorthand, in pixels: an absolute unit, or `em`,
// `rem` or `%`, which resolve against the canvas and root defaults. A bare
// zero is a length too, the one CSS lets go unitless.
function fontLength(text) {
  const [value, unit] = splitNumber(text) ?? [];
  if (value === undefined) return undefined;
  if (unit == "em") return value * CANVAS_DEFAULT_FONT_PX;
  if (unit == "rem") return value * ROOT_FONT_PX;
  if (unit == "%") return (value / 100) * CANVAS_DEFAULT_FONT_PX;
  if (unit == "" && value == 0) return 0;
  const perPx = PX_PER_UNIT.get(unit);
  return perPx === undefined ? undefined : value * perPx;
}

// The index just past the `)` that closes the first `(` in `text`, counting
// nested pairs, so `calc((1px + 2px) * 2)` ends at its last `)`.
function pastClosingParen(text) {
  let depth = 0;
  for (let at = 0; at < text.length; at++) {
    if (text[at] == "(") depth++;
    else if (text[at] == ")") {
      if (--depth < 0) return undefined;
      if (depth == 0) return at + 1;
    }
  }
  return undefined;
}

// What is inside `calc(...)`, when `text` is one call to it and nothing
// else.
const calcBody = (text) =>
  text.slice(0, 5).toLowerCase() == "calc(" &&
  pastClosingParen(text) === text.length
    ? text.slice(5, -1)
    : undefined;

// A `calc()` body made of font lengths and numbers joined by `+`, `-`, `*`
// and `/`, with parentheses: `[value, isLength]`, the value in pixels when
// it is a length -- `calc(2)` is a number, which a size refuses and a line
// height takes as a multiplier. `undefined` when it does not parse.
function calc(text) {
  const tokens = text
    .replace(/[()*/]/g, " $& ")
    .split(/\s+/)
    .filter(Boolean);
  let at = 0;

  // Each value carries whether it is a length, so `2px * 3` is a length and
  // `2px * 3px` is refused, as CSS types them.
  const expression = () => {
    let left = term();
    while (left && (tokens[at] == "+" || tokens[at] == "-")) {
      const op = tokens[at++],
        right = term();
      if (!right || left[1] != right[1]) return undefined;
      left = [op == "+" ? left[0] + right[0] : left[0] - right[0], left[1]];
    }
    return left;
  };
  const term = () => {
    let left = factor();
    while (left && (tokens[at] == "*" || tokens[at] == "/")) {
      const op = tokens[at++],
        right = factor();
      if (!right) return undefined;
      if (op == "*" && !(left[1] && right[1]))
        left = [left[0] * right[0], left[1] || right[1]];
      else if (op == "/" && !right[1]) left = [left[0] / right[0], left[1]];
      else return undefined;
    }
    return left;
  };
  const factor = () => {
    const token = tokens[at++];
    if (token === undefined) return undefined;
    if (token == "(") {
      const inner = expression();
      return tokens[at++] == ")" ? inner : undefined;
    }
    const [value, unit] = splitNumber(token) ?? [];
    if (value !== undefined && unit == "") return [value, false];
    const px = fontLength(token);
    return px === undefined ? undefined : [px, true];
  };

  const value = expression();
  return value && at == tokens.length ? value : undefined;
}

// A font size: a length, a percentage or `em` of the canvas default, a `rem`,
// a keyword, or a `calc()` of those. `undefined` for anything else and for a
// negative size, which the `font-size` property does not allow.
function fontSize(text) {
  const lower = text.toLowerCase(),
    inner = calcBody(lower);
  let size;
  if (inner !== undefined) {
    const [value, isLength] = calc(inner) ?? [];
    size = isLength ? value : undefined;
  } else if (ABSOLUTE_FONT_SIZES.has(lower)) {
    size = ABSOLUTE_FONT_SIZES.get(lower);
  } else if (lower == "larger") {
    size = CANVAS_DEFAULT_FONT_PX * RELATIVE_FONT_SIZE_STEP;
  } else if (lower == "smaller") {
    size = CANVAS_DEFAULT_FONT_PX / RELATIVE_FONT_SIZE_STEP;
  } else {
    size = fontLength(lower);
  }
  return Number.isFinite(size) && size >= 0 ? size : undefined;
}

// A line height after the `/`, in pixels for a font of `size`: `null` for
// `normal`, a number or a percentage multiplies the size, and a length is
// itself, with `em` taken of the font's own size. `undefined` when it does
// not parse or is negative.
function fontLineHeight(text, size) {
  const lower = text.toLowerCase();
  if (lower == "normal") return null;
  let px;
  const inner = calcBody(lower);
  if (inner !== undefined) {
    const [value, isLength] = calc(inner) ?? [];
    px = isLength ? value : value * size;
  } else {
    const [value, unit] = splitNumber(lower) ?? [];
    px =
      value === undefined
        ? undefined
        : unit == ""
          ? value * size
          : unit == "%"
            ? (value / 100) * size
            : unit == "em"
              ? value * size
              : fontLength(lower);
  }
  return Number.isFinite(px) && px >= 0 ? px : undefined;
}

// A weight written as a number: 1 to 1000, any CSS number, with the fraction
// dropped -- Chrome reads `550.7` as 550 and refuses `0.9`.
function fontWeight(text) {
  const [weight, unit] = splitNumber(text) ?? [];
  return unit === "" && weight >= 1 && weight <= 1000
    ? Math.trunc(weight)
    : undefined;
}

// A CSS string opening `text` with its quote, unescaped, and what follows it.
// A backslash escapes the next character, or names one in up to six hex
// digits and an optional following space, as CSS Syntax 3 section 4.3.7 has
// it; Chrome reads `'A\\B'` as the family `A\B`.
function cssString(text) {
  const quote = text[0],
    chars = [...text];
  let name = "",
    at = 1;
  while (at < chars.length) {
    const c = chars[at++];
    if (c == quote) {
      // `at` counts code points; the rest is everything after them.
      return [name, chars.slice(at).join("").trimStart()];
    }
    if (c != "\\") {
      name += c;
      continue;
    }
    let hex = "";
    while (
      hex.length < MAX_CSS_ESCAPE_HEX_DIGITS &&
      at < chars.length &&
      hexRE.test(chars[at])
    )
      hex += chars[at++];
    if (!hex) {
      if (at >= chars.length) return undefined;
      name += chars[at++];
      continue;
    }
    if (/\s/.test(chars[at] ?? "")) at++;
    // Zero, a surrogate and anything past the last code point are all
    // U+FFFD.
    const code = parseInt(hex, 16);
    name +=
      code == 0 || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)
        ? "�"
        : String.fromCodePoint(code);
  }
  return undefined;
}

// A family name list, each name quoted or written as identifiers, with
// unquoted runs of whitespace taken as one space. An unquoted name must be
// identifiers -- `12` is not a family, and Chrome ignores `16px 12` -- and a
// CSS-wide keyword is not one either.
function fontFamilies(text) {
  const families = [];
  let rest = text.trim();
  while (rest) {
    let name, after;
    if (rest[0] == '"' || rest[0] == "'") {
      [name, after] = cssString(rest) ?? [];
      if (name === undefined) return undefined;
    } else {
      const end = rest.includes(",") ? rest.indexOf(",") : rest.length,
        words = rest.slice(0, end).split(/\s+/).filter(Boolean);
      if (!words.length || !words.every((word) => identifierRE.test(word)))
        return undefined;
      name = words.join(" ");
      if (CSS_WIDE_KEYWORDS.has(name.toLowerCase())) return undefined;
      after = rest.slice(end);
    }
    families.push(name);
    if (after.startsWith(",")) {
      // A comma promises another name: `16px Arial,` is refused.
      rest = after.slice(1).trim();
      if (!rest) return undefined;
    } else if (!after.trim()) {
      rest = "";
    } else {
      return undefined;
    }
  }
  return families.length ? families : undefined;
}

// Where the token opening `text` ends: at the first character `stop`
// matches, or past the `)` closing a leading `calc(`, which may hold spaces
// and slashes of its own.
function fontTokenEnd(text, stop) {
  const end =
    text.slice(0, 5).toLowerCase() == "calc("
      ? pastClosingParen(text)
      : text.search(stop);
  return end === undefined || end < 0 ? text.length : end;
}

// A family as Chrome serializes it: bare when it is one identifier, quoted
// otherwise, with `"` and `\` escaped inside the quotes.
const serializeFamily = (family) =>
  identifierRE.test(family) ? family : `"${family.replace(/[\\"]/g, "\\$&")}"`;

// A number as CSS serializes it in a computed value: six significant digits,
// with trailing zeros dropped -- Chrome reads `2cm` back as `75.5906px` and
// `smaller` as `8.33333px`.
const cssNumber = (value) =>
  value == 0 || !Number.isFinite(value)
    ? `${value}`
    : `${parseFloat(
        value.toFixed(
          Math.min(
            MAX_FIXED_DIGITS,
            Math.max(0, 5 - Math.floor(Math.log10(Math.abs(value)))),
          ),
        ),
      )}`;

// The shorthand read in full, or `undefined` for one Chrome ignores. Before
// the size, in any order and each at most once: a style, `small-caps`, a
// weight, and a stretch keyword, with `normal` filling any of them, four
// tokens at most.
function readFont(str) {
  const font = {
    style: "normal",
    variant: "normal",
    weight: NORMAL_WEIGHT,
    stretch: "normal",
  };
  const seen = new Set();
  let rest = str.trim(),
    size;

  for (let prefix = 0; ; prefix++) {
    const end = fontTokenEnd(rest, /[\s/]/),
      token = rest.slice(0, end),
      after = rest.slice(end).trimStart();
    if (!token) return undefined;
    if ((size = fontSize(token)) !== undefined) {
      rest = after;
      break;
    }
    if (prefix >= 4 || !after) return undefined;

    const lower = token.toLowerCase(),
      take = (slot, value) => {
        if (seen.has(slot)) return false;
        seen.add(slot);
        font[slot] = value;
        return true;
      },
      weight = fontWeight(lower);
    const taken =
      lower == "normal" ||
      (lower == "italic" || lower == "oblique"
        ? take("style", lower)
        : lower == "small-caps"
          ? take("variant", lower)
          : lower == "bold"
            ? take("weight", BOLD_WEIGHT)
            : lower == "bolder" || lower == "lighter"
              ? take("weight", relativeWeight(lower, INHERITED_FONT_WEIGHT))
              : weight !== undefined
                ? take("weight", weight)
                : stretchRE.test(lower)
                  ? take("stretch", lower)
                  : // A system font keyword -- `caption`, `icon`, `menu`,
                    // `message-box`, `small-caption`, `status-bar` -- lands
                    // here and is refused, deliberately. Chrome reads each
                    // as the platform's interface font, which on macOS
                    // measured as 16px Arial: a value no other platform
                    // reproduces and nothing here could pin.
                    false);
    if (!taken) return undefined;
    rest = after;
  }

  let lineHeight;
  if (rest.startsWith("/")) {
    const leading = rest.slice(1).trimStart(),
      end = fontTokenEnd(leading, /\s/);
    lineHeight = fontLineHeight(leading.slice(0, end), size);
    if (lineHeight === undefined) return undefined;
    rest = leading.slice(end).trimStart();
  }

  const family = fontFamilies(rest);
  if (!family) return undefined;
  return { ...font, size, lineHeight: lineHeight ?? undefined, family };
}

function parseFont(str) {
  // One lookup, not two. `get` already tells a miss from a hit: an
  // unparseable font is cached as `null`, and only an absent key is
  // `undefined`, so there is nothing a preceding `has` would add.
  const hit = cache.font.get(str);
  if (hit !== undefined) return hit;

  const font = typeof str == "string" ? readFont(str) : undefined;
  if (!font) return cache.font.set(str, null);

  const { style, variant, weight, stretch, size, lineHeight, family } = font,
    smallCaps = variant == "small-caps",
    familyList = family
      .map((nm) => (nm.match(/\s/) ? `"${nm}"` : nm))
      .join(", ");

  return cache.font.set(
    str,
    Object.assign(font, {
      features: smallCaps ? { on: [...featureMap["small-caps"]] } : {},
      // The addon's resolved-font cache keys on this, so it names everything
      // the font carries -- the oblique, the stretch, the line height --
      // where `serialized` below does not. `16px/24px X` and `16px/32px X`
      // share a serialized form and must not share an entry.
      canonical: [
        style,
        variant !== style && variant,
        weight,
        stretch !== "normal" && stretch,
        `${size}px${lineHeight !== undefined ? `/${lineHeight}px` : ""}`,
        familyList,
      ]
        .filter(Boolean)
        .join(" "),
      // What the `font` getter reports, in Chrome's form. HTML: "On getting,
      // the font attribute must return the serialized form of the current
      // font of the context (with no 'line-height' component)". Chrome
      // writes style, weight, `small-caps`, size and families, leaves out
      // each one at its initial value, spells 700 `bold` and any slant
      // `italic`, gives the size six significant digits, and reports the
      // stretch through `fontStretch` rather than here.
      serialized: [
        style !== "normal" && "italic",
        weight !== NORMAL_WEIGHT &&
          (weight === BOLD_WEIGHT ? "bold" : `${weight}`),
        smallCaps && variant,
        `${cssNumber(size)}px`,
        family.map(serializeFamily).join(", "),
      ]
        .filter(Boolean)
        .join(" "),
    }),
  );
}

// A length in an absolute unit, `em` or `rem`, as the whole of `text`:
// `{ size, unit, px }`, with `px` NaN for `em` and `rem`, whose pixels
// depend on a font. `null` for anything else -- a percentage, a bare number,
// a space before the unit, or a number CSS does not have, as `5.px`. The
// crate's `parse_size` reads the same grammar.
function cssLength(text) {
  const [size, written] = splitNumber(text) ?? [];
  if (size === undefined) return null;
  const unit = written.toLowerCase();
  if (unit == "em" || unit == "rem") return { size, unit, px: NaN };
  const perPx = PX_PER_UNIT.get(unit);
  return perPx === undefined ? null : { size, unit, px: size * perPx };
}

// A length in a filter, in pixels, with `em` and `rem` of `emSize`. A
// percentage is not a length: Chrome refuses `blur(50%)`.
function parseLength(str, emSize = 16) {
  let text = str.trim();
  // A zero length may be written without a unit, and only a zero may. That
  // is CSS Values, and it is what a browser takes: `blur(0)`, and the two
  // bare zeros in `drop-shadow(20px 0 0 red)`, which is how most people
  // write an offset shadow. `blur(5)` is not a length and a browser refuses
  // it too.
  if (UNITLESS_ZERO_RE.test(text)) return 0;
  const length = cssLength(text);
  return !length
    ? NaN
    : Number.isNaN(length.px)
      ? length.size * emSize
      : length.px;
}

// `letterSpacing` and `wordSpacing`: the whole string a length, with no
// whitespace around it -- Chrome ignores `" 5px "` -- and `em` and `rem`
// kept as written, since the binding resolves them against the font in force
// when the text is laid out, as Chrome does.
const parseFlexibleSize = (str) =>
  typeof str == "string" ? cssLength(str) : null;

function parseStretch(str) {
  const match = stretchRE.exec(str);
  return match ? match[0] : undefined;
}

function parseVariant(str) {
  const hit = cache.variant.get(str);
  if (hit === undefined) {
    let variants = [],
      features = { on: [], off: [] };

    // An unparseable variant is cached as `null` and ignored by the setter,
    // the way `parseFont` already treats an unparseable font. It used to
    // throw, and nothing caught it, so a context property raised where every
    // one of its neighbours silently ignores -- which is what the Canvas
    // standard asks of an attribute setter. Two spellings reached that throw:
    // an unknown keyword, and a *valid* keyword in the wrong case.
    if (typeof str !== "string" || !str.trim())
      return cache.variant.set(str, null);

    for (let token of splitBy(str.toLowerCase(), /\s+/)) {
      let param;
      if (token == "normal") {
        // `variant`, singular, and a string: the shape every other exit from
        // this function returns and the one the binding reads. Returning
        // `variants` here meant `ctx.fontVariant = "normal"` threw, so a
        // variant could be set but never cleared -- and skipping the cache
        // write meant reparsing it every time.
        return cache.variant.set(str, {
          variant: "normal",
          features: { on: [], off: [] },
        });
      } else if (Object.hasOwn(featureMap, token)) {
        featureMap[token].forEach((feat) => {
          if (feat[0] == "-") features.off.push(feat.slice(1));
          else features.on.push(feat);
        });
        variants.push(token);
      } else if (
        (param = parameterizedRE.exec(token)) &&
        Object.hasOwn(alternatesMap, param[1])
      ) {
        // The membership test is part of the condition, not an assumption
        // after it. `alternatesMap["bogus"]` is `undefined`, and calling
        // `.replace` on it threw a TypeError from inside the parser -- a
        // second way for an invalid value to escape as an exception.
        let subPattern = alternatesMap[param[1]],
          subValue = Math.max(0, Math.min(99, parseInt(param[2], 10))),
          [feat, val] = subPattern
            .replace(/##/, subValue < 10 ? "0" + subValue : subValue)
            .replace(/#/, Math.min(9, subValue))
            .split(" ");
        if (typeof val == "undefined") features.on.push(feat);
        else features[feat] = parseInt(val, 10);
        variants.push(`${param[1]}(${subValue})`);
      } else {
        return cache.variant.set(str, null);
      }
    }

    return cache.variant.set(str, {
      variant: variants.join(" "),
      features: features,
    });
  }

  return hit;
}

function parseTextDecoration(str) {
  let style = "solid",
    line = "none",
    color = "currentColor",
    inherit = "auto",
    thickness,
    _val;

  str = (typeof str == "string" ? str : "").trim().replace(/\s+/, " ");
  for (const raw of str.split(" ")) {
    // Keywords are matched and stored lowercased; a colour is not, because it
    // is passed on to a colour parser that has its own rules and its own
    // names, and lowercasing it here would be this function deciding
    // something that is not its to decide.
    const token = raw.toLowerCase();
    if (token.match(/solid|double|dotted|dashed|wavy/)) style = token;
    else if (token.match(/none|initial|revert(-layer)?|unset/)) line = "none";
    else if (token.match(/underline|overline|line-through/)) line = token;
    else if ((_val = parseFlexibleSize(token))) thickness = _val;
    else if (token.match(/auto|from-font/)) inherit = token;
    else color = raw;
  }

  return { style, line, color, thickness, inherit, str };
}

// -- Window Types -----------------------------------------------------------------------

let cursorTypes = [
  "default",
  "none",
  "context-menu",
  "help",
  "pointer",
  "progress",
  "wait",
  "cell",
  "crosshair",
  "text",
  "vertical-text",
  "alias",
  "copy",
  "move",
  "no-drop",
  "not-allowed",
  "grab",
  "grabbing",
  "e-resize",
  "n-resize",
  "ne-resize",
  "nw-resize",
  "s-resize",
  "se-resize",
  "sw-resize",
  "w-resize",
  "ew-resize",
  "ns-resize",
  "nesw-resize",
  "nwse-resize",
  "col-resize",
  "row-resize",
  "all-scroll",
  "zoom-in",
  "zoom-out",
];

function parseCursor(str) {
  return cursorTypes.includes(str);
}

function parseFit(mode) {
  return [
    "none",
    "contain-x",
    "contain-y",
    "contain",
    "cover",
    "fill",
    "scale-down",
    "resize",
  ].includes(mode);
}

// -- Corner Rounding
//    https://github.com/fserb/canvas2D/blob/master/spec/roundrect.md

function parseCornerRadii(r) {
  r = [r]
    .flat()
    .slice(0, 4)
    .map((n) =>
      n && Object.hasOwn(n, "x") && Object.hasOwn(n, "y") ? n : { x: n, y: n },
    );

  if (r.some((pt) => !Number.isFinite(pt.x) || !Number.isFinite(pt.y))) {
    return null; // silently abort
  } else {
    // A `RangeError`, and deliberately not the `IndexSizeError` its
    // neighbours raise: `roundRect`'s own clause in the Canvas standard names
    // a `RangeError` where `arc`, `ellipse` and `arcTo` name an
    // `IndexSizeError`, and Chrome 148 follows it -- `roundRect(0,0,10,10,-5)`
    // is a `RangeError` from both the context and `Path2D`, while `arc(-5)` is
    // an `IndexSizeError` from both. Making these agree would be consistency
    // overriding the specification.
    //
    // The value is in the message because a caller who passed the wrong one
    // needs to see it, which is the rule #88 established for the range family.
    const negative = r.find((pt) => pt.x < 0 || pt.y < 0);
    if (negative) {
      throw RangeError(
        `Radius value ${negative.x < 0 ? negative.x : negative.y} is negative`,
      );
    }
  }

  return r.length == 1
    ? [r[0], r[0], r[0], r[0]]
    : r.length == 2
      ? [r[0], r[1], r[0], r[1]]
      : r.length == 3
        ? [r[0], r[1], r[2], r[1]]
        : r.length == 4
          ? [r[0], r[1], r[2], r[3]]
          : [0, 0, 0, 0].map((n) => ({ x: n, y: n }));
}

// -- Image Filters -----------------------------------------------------------------------
//    https://developer.mozilla.org/en-US/docs/Web/CSS/filter

// A CSS function name is an identifier, so it is matched in any case and
// lowercased before it is used as a key or written into the canonical string.
// The functions that take one amount, by lower-case name.
const PLAIN_FILTERS = new Set([
  "blur",
  "hue-rotate",
  "brightness",
  "contrast",
  "grayscale",
  "invert",
  "opacity",
  "saturate",
  "sepia",
]);

// What `brightness()`, `contrast()`, `grayscale()`, `invert()`, `opacity()`,
// `saturate()` and `sepia()` take when their argument is left out: Filter
// Effects 1 gives each a default of 1, which for `brightness`, `contrast`,
// `opacity` and `saturate` changes nothing and for the other three is the
// full effect.
const OMITTED_FILTER_AMOUNT = 1;

var // Anchored, and matching CSS's actual number grammar. Unanchored it found
  // an angle anywhere in the string, so `--45deg` matched the `-45deg` inside
  // it and rotated the wrong way, and `+-45deg` did the same -- both of which
  // a browser rejects outright. `[\d.]+` was too loose as well: it took
  // `5.deg` and `4.5.6deg`, which browsers also reject. The sign belongs to
  // the number, without which `hue-rotate(-45deg)` parsed as +45.
  angleValueRE =
    /^([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:e[+-]?\d+)?)(deg|g?rad|turn)$/i,
  // Zero, however it is spelled, and nothing else. A browser takes `0`,
  // `+0`, `-0` and `0.0` as a length or an angle without a unit, and not
  // `0.`, which is no number at all.
  UNITLESS_ZERO_RE = /^[+-]?(?:0+(?:\.0+)?|\.0+)$/;

// Splits a filter on whitespace outside parentheses, and after each `)`
// that closes a call: Chrome reads `blur(2px)brightness(2)` as two
// functions. The crate's `split_functions` splits the same way.
function splitFunctions(text) {
  const parts = [];
  let depth = 0,
    start = 0;
  for (let at = 0; at < text.length; at++) {
    const c = text[at];
    if (c == "(") depth++;
    else if (c == ")") {
      depth = Math.max(0, depth - 1);
      if (depth == 0) {
        parts.push(text.slice(start, at + 1));
        start = at + 1;
      }
    } else if (/\s/.test(c) && depth == 0) {
      if (at > start) parts.push(text.slice(start, at));
      start = at + 1;
    }
  }
  if (start < text.length) parts.push(text.slice(start));
  return parts;
}

// Memoized on the same terms as `parseFont`, and for the same reason: the
// parse is pure, so a hit and a miss answer identically. It is also the whole
// of what setting the property cost -- 3226 ns of 4175, against 80 for
// reading the font the `em` units resolve against and 573 for the crossing.
// The `em` size is part of the key because it is part of the answer: at 16px
// `blur(0.5em)` is 8px and at 40px it is 20px.
//
// The parsed object is handed straight to the binding and never written to,
// so callers sharing one instance cannot see each other.
function parseFilter(str, emSize = 16) {
  const key = `${emSize}|${str}`;
  const hit = cache.filter.get(key);
  if (hit !== undefined || cache.filter.has(key)) return hit;
  return cache.filter.set(key, parseFilterUncached(str, emSize));
}

function parseFilterUncached(str, emSize) {
  const text = typeof str == "string" ? str.trim() : "";
  if (text.toLowerCase() == "none") return { canonical: "none", filters: {} };

  // One function that does not parse voids the whole value, as it does in
  // Chrome: `blur(5px) brightness(2.)` is ignored, not read as a blur. A
  // chain missing a step is a different picture.
  let filters = {};
  let canonical = [];
  for (const part of splitFunctions(text)) {
    const open = part.indexOf("(");
    if (open < 0 || !part.endsWith(")")) return null;
    // A function name is an identifier, so it is matched in any case and
    // lowercased before it is used as a key or written into the canonical
    // string: Chrome reads `BLUR(5px)`.
    const kind = part.slice(0, open).toLowerCase(),
      arg = part.slice(open + 1, -1),
      omitted = !arg.trim();

    if (kind == "drop-shadow") {
      let args = arg.trim().split(/\s+/),
        found = false,
        runOf = (words) => {
          let run = [];
          for (let word of words) {
            let size = parseLength(word, emSize);
            if (!isFinite(size)) break;
            run.push(size);
          }
          return run;
        };

      for (let fromFront of [true, false]) {
        let words = fromFront ? args : [...args].reverse(),
          run = runOf(words),
          dims = fromFront ? run : [...run].reverse(),
          rest = (
            fromFront
              ? args.slice(run.length)
              : args.slice(0, args.length - run.length)
          ).join(" ");
        // The offsets may be negative; the blur radius may not, and Chrome
        // refuses `drop-shadow(2px 2px -3px)`.
        if (dims.length < 2 || dims.length > 3 || dims[2] < 0) continue;
        // The blur radius is the optional one; its initial value is zero.
        if (dims.length == 2) dims.push(0);
        // No colour is `currentColor`, which the Rust side resolves to
        // black -- the same default the typed `FilterOp::DropShadow` takes.
        let color = rest || "black";
        filters[kind] = [...dims, color];
        // The colour keeps its spaces. Stripping them turned
        // `rgb(0 0 0)` into `rgb(000)`, which parses back as nothing --
        // and it was never needed: `splitFunctions` counts parentheses, so
        // a functional colour survives the outer split intact.
        canonical.push(`${kind}(${dims.join("px ")}px ${color})`);
        found = true;
        break;
      }
      if (!found) return null;
    } else if (PLAIN_FILTERS.has(kind)) {
      let val =
        kind == "blur"
          ? omitted
            ? 0
            : parseLength(arg, emSize)
          : kind == "hue-rotate"
            ? parseAngle(arg)
            : parsePercentage(arg);
      // A negative blur is outside its range, and Chrome refuses it rather
      // than clamping. The amounts are refused the same way, in
      // `parsePercentage`.
      if (!isFinite(val) || (kind == "blur" && val < 0)) return null;
      filters[kind] = val;
      // The argument is lowercased in the canonical form as well: it is a
      // number and a unit, both case-insensitive, and echoing `3PX` back
      // would report a spelling this parser has already stopped honouring.
      canonical.push(`${kind}(${arg.trim().toLowerCase()})`);
    } else {
      return null;
    }
  }

  return canonical.length ? { canonical: canonical.join(" "), filters } : null;
}

// A filter amount, a number or a percentage, as a fraction: `150%` and
// `1.5` are one amount. Left out it is `OMITTED_FILTER_AMOUNT`; negative it
// is outside every amount's range, and Chrome refuses `brightness(-1)`.
function parsePercentage(str) {
  const text = str.trim();
  if (!text) return OMITTED_FILTER_AMOUNT;
  const [value, unit] = splitNumber(text) ?? [];
  const amount = unit === "" ? value : unit === "%" ? value / 100 : NaN;
  return amount >= 0 ? amount : NaN;
}

function parseAngle(str) {
  let text = str.trim();
  // Left out, an angle is zero: Chrome reads `hue-rotate()`.
  if (!text) return 0;
  // As for a length: `hue-rotate(0)` is what a browser accepts, and
  // `hue-rotate(45)` is not.
  if (UNITLESS_ZERO_RE.test(text)) return 0;
  if ((m = angleValueRE.exec(text))) {
    // Lowercased: CSS units are case-insensitive, and a browser takes
    // `45DEG` and `1TURN`. The pattern already carried the `i` flag, but
    // comparing the captured unit as written made it inert -- the value
    // matched and then fell through to NaN, so the whole filter was
    // discarded.
    let [amt, unit] = [parseFloat(m[1]), m[2].toLowerCase()];
    return unit == "deg"
      ? amt
      : unit == "rad"
        ? (360 * amt) / (2 * Math.PI)
        : unit == "grad"
          ? (360 * amt) / 400
          : unit == "turn"
            ? 360 * amt
            : NaN;
  }
}

//
// Font attribute keywords & corresponding values
//

// The base `bolder` and `lighter` resolve against.
//
// CSS Fonts 4 section 2.2.1 defines both relative to the *inherited*
// `font-weight`. A canvas has no parent to inherit from, so the base is the
// initial value of the property, which is `normal`. Chrome resolves
// `bolder 16px serif` to 700 and `lighter 16px serif` to 100 for this reason.
const INHERITED_FONT_WEIGHT = 400;

// CSS Fonts 4 section 2.2.1, Relative Weights. The inherited weight picks a
// row of the specification's table by falling under one of the bounds below,
// and that row's entry for the keyword is the result. `null` is the
// specification's "no change", which keeps the inherited weight.
//
// This is a table rather than a step because it is neither a constant offset
// nor symmetric: both keywords saturate, at 900 and 100, and 600 gives 900 for
// `bolder` against 400 for `lighter`. Only the 350-550 row is reachable
// through `ctx.font`, since the base above is fixed, but a weight written as
// `bolder` against a registered face would reach the others.
const relativeWeightMap = {
  bolder: [400, 400, 700, 900, 900, null],
  lighter: [null, 100, 100, 400, 700, 700],
};

// The exclusive upper bounds of the rows above, in order.
const relativeWeightBounds = [100, 350, 550, 750, 900, Infinity];

const relativeWeight = (keyword, inherited) => {
  const row = relativeWeightBounds.findIndex((bound) => inherited < bound);
  return relativeWeightMap[keyword][row] ?? inherited;
};

const featureMap = {
  normal: [],

  // font-variant-ligatures
  "common-ligatures": ["liga", "clig"],
  "no-common-ligatures": ["-liga", "-clig"],
  "discretionary-ligatures": ["dlig"],
  "no-discretionary-ligatures": ["-dlig"],
  "historical-ligatures": ["hlig"],
  "no-historical-ligatures": ["-hlig"],
  contextual: ["calt"],
  "no-contextual": ["-calt"],

  // font-variant-position
  super: ["sups"],
  sub: ["subs"],

  // font-variant-caps
  "small-caps": ["smcp"],
  "all-small-caps": ["c2sc", "smcp"],
  "petite-caps": ["pcap"],
  "all-petite-caps": ["c2pc", "pcap"],
  unicase: ["unic"],
  "titling-caps": ["titl"],

  // font-variant-numeric
  "lining-nums": ["lnum"],
  "oldstyle-nums": ["onum"],
  "proportional-nums": ["pnum"],
  "tabular-nums": ["tnum"],
  "diagonal-fractions": ["frac"],
  "stacked-fractions": ["afrc"],
  ordinal: ["ordn"],
  "slashed-zero": ["zero"],

  // font-variant-east-asian
  jis78: ["jp78"],
  jis83: ["jp83"],
  jis90: ["jp90"],
  jis04: ["jp04"],
  simplified: ["smpl"],
  traditional: ["trad"],
  "full-width": ["fwid"],
  "proportional-width": ["pwid"],
  ruby: ["ruby"],

  // font-variant-alternates (non-parameterized)
  "historical-forms": ["hist"],
};

const alternatesMap = {
  stylistic: "salt #",
  styleset: "ss##",
  "character-variant": "cv##",
  swash: "swsh #",
  ornaments: "ornm #",
  annotation: "nalt #",
};

function parseVariationSettings(str) {
  if (!str || str === "normal") return {};
  let result = {};
  for (let part of str.split(",")) {
    let match = part.trim().match(/^["'](\w{4})["']\s+([-\d.]+)$/);
    if (match) result[match[1]] = parseFloat(match[2]);
  }
  return result;
}

module.exports = {
  // used by context
  font: parseFont,
  variant: parseVariant,
  spacing: parseFlexibleSize,
  stretch: parseStretch,
  decoration: parseTextDecoration,
  filter: parseFilter,
  variationSettings: parseVariationSettings,

  // path & context
  radii: parseCornerRadii,

  // gui
  cursor: parseCursor,
  fit: parseFit,

  // `ctx.font` reaches one row of the relative-weight table, because the base
  // it resolves against is fixed. Exported so the rest of the table can be
  // asserted against the specification.
  relativeWeight,
};
