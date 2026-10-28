//
// Parsers for properties that take CSS-style strings as values
//

"use strict";

// -- Variant --------------------------------------------------------------------------
//    https://developer.mozilla.org/en-US/docs/Web/CSS/font-variant
//
// The `font` shorthand is read by the crate's parser in the binding, which
// remembers the strings it has resolved; only `fontVariant` is read here.

// How many parsed variant strings to remember.
//
// Bounded, because a cache that nothing evicts keeps every distinct string a
// process passes for its life. The parse is pure, so a cache is only ever a
// speed trade and an evicted entry reparses to the same value. A thousand
// covers reuse in any real drawing loop, which is a handful of strings set
// over and over.
const CACHE_LIMIT = 1024;

// A `Map` rather than an object, because a plain object answers for keys it
// inherits: `cache["constructor"]` is `Object`, and a miss test reading it
// would hand back a function where a parsed value belonged. A `Map` has no
// inherited keys.
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
  cache = { variant: new Memo() };

// CSS whitespace: space, tab, line feed, carriage return and form feed, CSS
// Syntax 3's once its preprocessing has turned the last two into line feeds.
// JavaScript's `\s` and `trim()` take the Unicode spaces as well, which CSS
// reads as part of a token, so neither is used on a CSS value here.
const CSS_SPACE = /[ \t\n\r\f]+/,
  cssTrim = (str) => str.replace(/^[ \t\n\r\f]+|[ \t\n\r\f]+$/g, "");

// `fontStretch` is an enumeration, whose values WebIDL matches exactly, so
// `"CONDENSED"` is no name of it, as in Chrome.
const stretchRE = /^(normal|(semi-|extra-|ultra-)?(condensed|expanded))$/,
  parameterizedRE = /([\w-]+)\((.*?)\)/;

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
    if (typeof str !== "string" || !cssTrim(str))
      return cache.variant.set(str, null);

    for (let token of splitBy(cssTrim(str).toLowerCase(), CSS_SPACE)) {
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

//
// Font attribute keywords & corresponding values
//

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
    let match = cssTrim(part).match(/^["'](\w{4})["'][ \t\n\r\f]+([-\d.]+)$/);
    if (match) result[match[1]] = parseFloat(match[2]);
  }
  return result;
}

module.exports = {
  // used by context
  variant: parseVariant,
  stretch: parseStretch,
  variationSettings: parseVariationSettings,

  // path & context
  radii: parseCornerRadii,
  space: CSS_SPACE,

  // gui
  cursor: parseCursor,
  fit: parseFit,
};
