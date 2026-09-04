//
// Font management & metrics
//

"use strict";

const { RustClass, wrap, readOnly, signature, neon } = require("./neon");

class FontLibrary extends RustClass {
  constructor() {
    super(FontLibrary);
  }

  get families() {
    return this.prop("families");
  }

  has(familyName) {
    return this.ƒ("has", familyName);
  }

  family(name) {
    return this.ƒ("family", name);
  }

  use(...args) {
    // Check for buffer-based registration: use("name", Buffer) or use("name", [Buffer, ...])
    let lastArg = args[args.length - 1];
    let bufferArgs = [lastArg].flat();
    if (
      bufferArgs.some((b) => Buffer.isBuffer(b) || b instanceof ArrayBuffer)
    ) {
      let data = args.pop();
      let alias = args.shift();
      let buffers = [data]
        .flat()
        .map((b) => (b instanceof ArrayBuffer ? Buffer.from(b) : b));
      return this.ƒ("addFamilyFromData", alias, buffers);
    }

    let sig = signature(args);
    if (sig == "o") {
      let results = {};
      for (let [alias, paths] of Object.entries(args.shift())) {
        results[alias] = this.ƒ("addFamily", alias, [paths].flat());
      }
      return results;
    } else if (sig.match(/^s?[as]$/)) {
      let fonts = [args.pop()].flat();
      let alias = args.shift();
      return this.ƒ("addFamily", alias, fonts);
    } else {
      throw new Error(
        "Expected an array of file paths or an object mapping family names to font files",
      );
    }
  }

  reset() {
    return this.ƒ("reset");
  }
}

class TextMetrics {
  // Built from the measurements `measureText` collects, never from nothing:
  // `new TextMetrics()` used to answer with an object whose every documented
  // property was undefined. Like CanvasGradient, the constructor is its own
  // producer, so it refuses the arguments rather than refusing outright.
  constructor(metrics) {
    if (metrics === null || typeof metrics !== "object") {
      throw new TypeError(
        `Function is not a constructor (use CanvasRenderingContext2D's "measureText" method instead)`,
      );
    }
    for (let k in metrics) readOnly(this, k, metrics[k]);
  }
}

// What `measureText` packs into the buffer it hands over, published by Rust
// so that the order these are read in is the order they were written in.
// Repeating the list here instead would make a field landing in the wrong
// slot a measurement reported under another measurement's name, which
// nothing would raise.
const FIELDS = neon.CanvasRenderingContext2D.textMetricsFields();

/**
 * Reads one value's fields out of `data`, advancing `at`.
 *
 * `optional` is a measurement the font did not report: `NaN` in the buffer
 * and `null` here, which is what these have always been. `family` is a
 * string, taken in turn from the array travelling beside the numbers and
 * keeping the place the published order gives it.
 */
function read(fields, data, at, families, into) {
  for (const { name, kind } of fields) {
    if (kind === "family") {
      into[name] = families[at.name++];
    } else {
      const value = data[at.number++];
      into[name] = kind === "optional" && Number.isNaN(value) ? null : value;
    }
  }
  return into;
}

/**
 * Builds the metrics from the buffer `measureText` answered with.
 *
 * Not through the constructor below, which is the guard a caller reaching
 * for `new TextMetrics()` meets. This is the producer, and it writes the
 * properties on rather than copying them off a second object -- that copy
 * was thirteen `defineProperty` calls, about a microsecond of the call.
 */
function textMetrics([data, families]) {
  const at = { number: 0, name: 0 };
  const metrics = read(
    FIELDS.metrics,
    data,
    at,
    families,
    Object.create(TextMetrics.prototype),
  );

  const lines = [];
  for (let line = data[at.number++]; line > 0; line--) {
    const entry = read(FIELDS.line, data, at, families, {});
    const runs = [];
    for (let run = data[at.number++]; run > 0; run--) {
      runs.push(read(FIELDS.run, data, at, families, {}));
    }
    entry.runs = runs;
    lines.push(entry);
  }
  metrics.lines = lines;

  // Only the metrics themselves, as before: the lines and runs inside them
  // were never frozen.
  return Object.freeze(metrics);
}

class ParagraphBuilder extends RustClass {
  // Text is shaped with the process-global font library. CanvasKit takes a FontMgr
  // as a second argument; there is no per-builder equivalent here, so it is not
  // accepted rather than accepted and ignored.
  //
  // Allocating here rather than in Make is what makes `new` usable: a builder
  // that skipped the allocation would still be `instanceof ParagraphBuilder`
  // and would fail later, inside a method, with a downcast error naming none
  // of the caller's code.
  constructor(style) {
    super(ParagraphBuilder);
    this.alloc(style || {});
  }
  static Make(style) {
    return new ParagraphBuilder(style);
  }
  pushStyle(style) {
    this.ƒ("pushStyle", style || {});
    return this;
  }
  pop() {
    this.ƒ("pop");
    return this;
  }
  addText(text) {
    this.ƒ("addText", text);
    return this;
  }
  addPlaceholder(width, height, align, baseline, offset) {
    this.ƒ(
      "addPlaceholder",
      width,
      height,
      align || 0,
      baseline || 0,
      offset || 0,
    );
    return this;
  }
  build() {
    return wrap(Paragraph, this.ƒ("build"));
  }
}

class Paragraph extends RustClass {
  // A laid-out paragraph is the output of a builder, not something describable
  // from arguments -- the builder exists so several styled runs can share one
  // paragraph, which a constructor taking text and a style could not express.
  // Nothing here calls this: `build()` goes through `wrap`, which bypasses the
  // constructor entirely.
  constructor() {
    throw new TypeError(
      `Function is not a constructor (use a ParagraphBuilder's "build" method instead)`,
    );
  }
  layout(width) {
    this.ƒ("layout", width);
  }
  getHeight() {
    return this.ƒ("getHeight");
  }
  getLongestLine() {
    return this.ƒ("getLongestLine");
  }
  getMaxWidth() {
    return this.ƒ("getMaxWidth");
  }
  getMaxIntrinsicWidth() {
    return this.ƒ("getMaxIntrinsicWidth");
  }
  getMinIntrinsicWidth() {
    return this.ƒ("getMinIntrinsicWidth");
  }
  getAlphabeticBaseline() {
    return this.ƒ("getAlphabeticBaseline");
  }
  getIdeographicBaseline() {
    return this.ƒ("getIdeographicBaseline");
  }
  getGlyphPositionAtCoordinate(x, y) {
    return this.ƒ("getGlyphPositionAtCoordinate", x, y);
  }
  getRectsForRange(start, end, hStyle, wStyle) {
    return this.ƒ("getRectsForRange", start, end, hStyle || 0, wStyle || 0);
  }
  getLineMetrics() {
    return this.ƒ("getLineMetrics");
  }
  getFirstLineAscent() {
    // Derived rather than a native call, the same way `Path2D.points` is:
    // the metrics already cross the boundary, and asking for one number
    // out of them a second time would be a second round trip for something
    // this side can read.
    let [first] = this.getLineMetrics();
    return first ? first.ascent : 0;
  }
  didExceedMaxLines() {
    return this.ƒ("didExceedMaxLines");
  }
  getNumberOfLines() {
    return this.ƒ("getNumberOfLines");
  }
  getRectsForPlaceholders() {
    return this.ƒ("getRectsForPlaceholders");
  }
  getUnresolvedCodepoints() {
    return this.ƒ("getUnresolvedCodepoints");
  }
}

// Re-exported from their own module so the browser build can have them too;
// this file requires ./neon, which a browser bundle cannot load.
const {
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
} = require("./text_decoration");

module.exports = {
  textMetrics,
  FontLibrary: new FontLibrary(),
  TextMetrics,
  ParagraphBuilder,
  Paragraph,
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
};
