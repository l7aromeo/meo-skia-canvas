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

// Keywords and unit identifiers in CSS are ASCII case-insensitive, so every
// one of these matches in any case and the value is lowercased before it is
// looked up or stringified. The flag alone is not the fix: without the
// normalisation below, `5PX` would match here and then miss every `unit ==`
// arm in `parseSize`, which is the same silent drop by a longer route.
//
// `%` is not an identifier and has no case, but it lives in the same
// alternation.
const styleRE = /^(normal|italic|oblique)$/i,
  smallcapsRE = /^(normal|small-caps)$/i,
  stretchRE = /^(normal|(semi-|extra-|ultra-)?(condensed|expanded))$/i,
  namedSizeRE = /(?:xx?-)?small|smaller|medium|larger|(?:xx?-)?large|normal/i,
  numSizeRE = /^(-?[\d.]+)(px|pt|pc|in|cm|mm|%|em|ex|ch|rem|q)/i,
  namedWeightRE = /^(normal|bold(er)?|lighter)$/i,
  numWeightRE = /^(1000|\d{1,3})$/,
  parameterizedRE = /([\w-]+)\((.*?)\)/,
  unquote = (s) => s.replace(/^(['"])(.*?)\1$/, "$2"),
  isSize = (s) => namedSizeRE.test(s) || numSizeRE.test(s),
  isWeight = (s) => namedWeightRE.test(s) || numWeightRE.test(s);

function parseFont(str) {
  // One lookup, not two. `get` already tells a miss from a hit: an
  // unparseable font is cached as `null`, and only an absent key is
  // `undefined`, so there is nothing a preceding `has` would add.
  const hit = cache.font.get(str);
  if (hit === undefined) {
    try {
      if (typeof str !== "string")
        throw new Error("Font specification must be a string");
      if (!str) throw new Error("Font specification cannot be an empty string");

      let font = {
          style: "normal",
          variant: "normal",
          weight: "normal",
          stretch: "normal",
        },
        value = str.replace(/\s*\/\*s/, "/"),
        tokens = splitBy(value, /\s+/),
        token;

      while ((token = tokens.shift())) {
        let match = styleRE.test(token)
          ? "style"
          : smallcapsRE.test(token)
            ? "variant"
            : stretchRE.test(token)
              ? "stretch"
              : isWeight(token)
                ? "weight"
                : isSize(token)
                  ? "size"
                  : null;

        switch (match) {
          case "style":
          case "variant":
          case "stretch":
          case "weight":
            font[match] = token.toLowerCase();
            break;

          case "size": {
            // size is the pivot point between the style fields and the family name stack,
            // so start processing what's been collected
            let [emSize, leading] = splitBy(token, "/"),
              size = parseSize(emSize),
              lineHeight = leading
                ? parseSize(leading.replace(/(\d)$/, "$1em"), size)
                : undefined,
              weight = parseWeight(font.weight),
              family = splitBy(tokens.join(" "), /\s*,\s*/).map(unquote),
              features =
                font.variant == "small-caps" ? { on: ["smcp", "onum"] } : {},
              familyList = family
                .map((nm) => (nm.match(/\s/) ? `"${nm}"` : nm))
                .join(", "),
              { style, stretch, variant } = font;

            // make sure all the numeric fields have legitimate values.
            //
            // `font-size` and `line-height` are both defined over a
            // non-negative length, so a negative one makes the whole
            // shorthand invalid and the assignment is ignored -- which is
            // what a browser does with `ctx.font = "-5px serif"`. Zero is not
            // negative and stays legal.
            //
            // Checked here rather than in `parseSize`, which is shared: a
            // `drop-shadow` offset reaches it by the same route and is
            // legitimately negative, so refusing there would take
            // `drop-shadow(-20px 0 0 red)` with it.
            let invalid =
              !isFinite(size) || size < 0
                ? `font size "${emSize}"`
                : lineHeight !== undefined &&
                    (!isFinite(lineHeight) || lineHeight < 0)
                  ? `line height "${leading}"`
                  : !isFinite(weight)
                    ? `font weight "${font.weight}"`
                    : family.length == 0
                      ? `font family "${tokens.join(", ")}"`
                      : false;

            if (!invalid) {
              // include a re-stringified version of the decoded/absified values
              return cache.font.set(
                str,
                Object.assign(font, {
                  size,
                  lineHeight,
                  weight,
                  family,
                  features,
                  canonical: [
                    style,
                    variant !== style && variant,
                    [variant, style].indexOf(weight) == -1 && weight,
                    [variant, style, weight].indexOf(stretch) == -1 && stretch,
                    `${size}px${isFinite(lineHeight) ? `/${lineHeight}px` : ""}`,
                    familyList,
                  ]
                    .filter(Boolean)
                    .join(" "),
                  // What the `font` getter reports, which is not the
                  // string above. HTML: "On getting, the font attribute must
                  // return the serialized form of the current font of the
                  // context (with no 'line-height' component)", and CSS
                  // shorthand serialisation omits any component sitting at
                  // its initial value. So the line height goes, a `normal`
                  // style, variant or stretch goes, weight 400 goes, and 700
                  // is spelled `bold`. Chrome 148 returns exactly this for
                  // `24px/2 Helvetica`, `bold 16px Helvetica` and
                  // `italic bold 24px Helvetica`.
                  //
                  // Two strings rather than one rewritten one, because
                  // `canonical` keys the addon's resolved-font cache and has
                  // to name the specification uniquely. A string with the
                  // line height taken out does not: `16px/24px X` and
                  // `16px/32px X` would share an entry, and whichever
                  // arrived second would be laid out with the first's
                  // leading.
                  //
                  // Two places this deliberately differs from Chrome, and
                  // both keep the round trip whole where Chrome's loses
                  // something. Chrome parses a stretch out of the shorthand
                  // into `ctx.fontStretch` and then omits it here, so
                  // `ctx.font = ctx.font` widens a condensed face back to
                  // normal; and it reports `oblique` as `italic`, which are
                  // separate faces to the font matcher.
                  serialized: [
                    style !== "normal" && style,
                    variant !== "normal" && variant,
                    weight !== 400 && (weight === 700 ? "bold" : `${weight}`),
                    stretch !== "normal" && stretch,
                    `${size}px`,
                    familyList,
                  ]
                    .filter(Boolean)
                    .join(" "),
                }),
              );
            }
            throw new Error(`Invalid ${invalid}`);
          }

          default:
            throw new Error(`Unrecognized font attribute "${token}"`);
        }
      }
      throw new Error("Could not find a font size value");
    } catch {
      return cache.font.set(str, null);
    }
  }
  return hit;
}

// A length only -- no percentage. `parseSize` takes one because a font size
// may be a percentage; `blur()` is defined over `<length>` and Chrome refuses
// `blur(50%)`, which this accepted for as long as it shared `parseSize`.
function parseLength(str, emSize = 16) {
  let text = str.trim();
  // A zero length may be written without a unit, and only a zero may. That
  // is CSS Values, and it is what a browser takes: `blur(0)`, and the two
  // bare zeros in `drop-shadow(20px 0 0 red)`, which is how most people
  // write an offset shadow. Requiring the unit did not merely ignore the
  // zero -- the length failed to parse, so the whole function failed, and
  // the whole declaration with it. `ctx.filter` read back `none` after
  // being set to a shadow, and in a chain the shadow vanished while the
  // rest of it stood: `blur(3px) drop-shadow(20px 0 0 #f00)` became
  // `blur(3px)`.
  //
  // Anchored to zero on purpose. `blur(5)` is not a length and a browser
  // refuses it too; only the zero is special.
  if (UNITLESS_ZERO_RE.test(text)) return 0;
  return /%\s*$/.test(text) ? NaN : parseSize(text, emSize);
}

function parseSize(str, emSize = 16) {
  if ((m = numSizeRE.exec(str))) {
    let [size, unit] = [parseFloat(m[1]), m[2].toLowerCase()];
    return (
      size *
      (unit == "px"
        ? 1
        : unit == "pt"
          ? 1 / 0.75
          : unit == "%"
            ? emSize / 100
            : unit == "pc"
              ? 16
              : unit == "in"
                ? 96
                : unit == "cm"
                  ? 96.0 / 2.54
                  : unit == "mm"
                    ? 96.0 / 25.4
                    : unit == "q"
                      ? 96 / 25.4 / 4
                      : unit.match("r?em")
                        ? emSize
                        : NaN)
    );
  }

  if ((m = namedSizeRE.exec(str))) {
    return emSize * (sizeMap[m[0].toLowerCase()] || 1.0);
  }

  return NaN;
}

function parseFlexibleSize(str) {
  if ((m = numSizeRE.exec(str))) {
    // Lowercased for the same reason `parseSize` does it, and with a sharper
    // consequence: `numSizeRE` matches in any case, so `"2PX"` gets past the
    // regex and then misses every arm below, and the `NaN` it produces
    // reaches the addon as a value it refuses out loud. Matching without
    // normalising turns a silent drop into a thrown error.
    let [size, unit] = [parseFloat(m[1]), m[2].toLowerCase()],
      px =
        size *
        (unit == "px"
          ? 1
          : unit == "pt"
            ? 1 / 0.75
            : unit == "pc"
              ? 16
              : unit == "in"
                ? 96
                : unit == "cm"
                  ? 96.0 / 2.54
                  : unit == "mm"
                    ? 96.0 / 25.4
                    : unit == "q"
                      ? 96 / 25.4 / 4
                      : NaN);
    return { size, unit, px };
  }
  return null;
}

function parseStretch(str) {
  const match = stretchRE.exec(str);
  return match ? match[0] : undefined;
}

function parseWeight(str) {
  const numeric = numWeightRE.exec(str);
  if (numeric) return parseInt(numeric[0]) || NaN;

  const named = namedWeightRE.exec(str);
  if (!named) return NaN;

  // Both lookups are keyed by the lower-case spelling, which `parseFont` has
  // already normalised the token to.
  const keyword = named[0];
  return keyword in relativeWeightMap
    ? relativeWeight(keyword, INHERITED_FONT_WEIGHT)
    : weightMap[keyword];
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
var plainFilterRE =
    /(blur|hue-rotate|brightness|contrast|grayscale|invert|opacity|saturate|sepia)\((.*?)\)/i,
  shadowFilterRE = /drop-shadow\((.*)\)/i,
  percentValueRE = /^(\+|-)?\d+%$/,
  // Anchored, and matching CSS's actual number grammar. Unanchored it found
  // an angle anywhere in the string, so `--45deg` matched the `-45deg` inside
  // it and rotated the wrong way, and `+-45deg` did the same -- both of which
  // a browser rejects outright. `[\d.]+` was too loose as well: it took
  // `5.deg` and `4.5.6deg`, which browsers also reject. The sign belongs to
  // the number, without which `hue-rotate(-45deg)` parsed as +45.
  angleValueRE =
    /^([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:e[+-]?\d+)?)(deg|g?rad|turn)$/i,
  // Zero, however it is spelled, and nothing else. A browser takes `0`,
  // `+0`, `-0` and `0.0` as a length or an angle without a unit.
  UNITLESS_ZERO_RE = /^[+-]?(?:0+(?:\.0*)?|\.0+)$/;

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
  let filters = {};
  let canonical = [];

  for (var spec of splitBy(str, /\s+/) || []) {
    if ((m = shadowFilterRE.exec(spec))) {
      // `<color>? && <length>{2,3}` -- Filter Effects 1. Two lengths or
      // three, with the colour on either side of them or absent. This used
      // to read exactly three lengths from the front and require a colour
      // after them, so `drop-shadow(red 2px 2px 4px)`, `drop-shadow(2px 4px
      // red)` and `drop-shadow(2px 2px 4px)` were all dropped on the floor
      // while Chrome drew every one.
      //
      // Both orderings are tried rather than the lengths being located in
      // one pass, because a colour can contain a word that parses as a
      // length: the middle `0` of `rgb(0 0 0)` is a valid CSS zero.
      let kind = "drop-shadow",
        args = m[1].trim().split(/\s+/),
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
        if (dims.length < 2 || dims.length > 3) continue;
        // The blur radius is the optional one; its initial value is zero.
        if (dims.length == 2) dims.push(0);
        // No colour is `currentColor`, which the Rust side resolves to
        // black -- the same default the typed `FilterOp::DropShadow` takes.
        let color = rest || "black";
        filters[kind] = [...dims, color];
        // The colour keeps its spaces. Stripping them turned
        // `rgb(0 0 0)` into `rgb(000)`, which parses back as nothing --
        // and it was never needed: `splitBy` counts parentheses, so a
        // functional colour survives the outer split intact.
        canonical.push(`${kind}(${dims.join("px ")}px ${color})`);
        break;
      }
    } else if ((m = plainFilterRE.exec(spec))) {
      let [kind, arg] = [m[1].toLowerCase(), m[2]];
      let val =
        kind == "blur"
          ? parseLength(arg, emSize)
          : kind == "hue-rotate"
            ? parseAngle(arg)
            : parsePercentage(arg);
      if (isFinite(val)) {
        filters[kind] = val;
        // The argument is lowercased in the canonical form as well: it is a
        // number and a unit, both case-insensitive, and echoing `3PX` back
        // would report a spelling this parser has already stopped honouring.
        canonical.push(`${kind}(${arg.trim().toLowerCase()})`);
      }
    }
  }

  return str.trim() == "none"
    ? { canonical: "none", filters }
    : canonical.length
      ? { canonical: canonical.join(" "), filters }
      : null;
}

function parsePercentage(str) {
  return percentValueRE.test(str.trim())
    ? parseInt(str, 10) / 100
    : !isNaN(str)
      ? parseFloat(str)
      : NaN;
}

function parseAngle(str) {
  let text = str.trim();
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

const weightMap = {
  normal: 400,
  bold: 700,
};

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

const sizeMap = {
  "xx-small": 3 / 5,
  "x-small": 3 / 4,
  small: 8 / 9,
  smaller: 8 / 9,
  large: 6 / 5,
  larger: 6 / 5,
  "x-large": 3 / 2,
  "xx-large": 2 / 1,
  normal: 1.2, // special case for lineHeight
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
  size: parseSize,
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
