//
// Font management & metrics
//

"use strict";

const {
  RustClass,
  wrap,
  signature,
  neon,
  inspect,
  ALLOC,
  PROP,
  CALL,
  REPR,
} = require("./neon");

class FontLibrary extends RustClass {
  constructor() {
    super(FontLibrary);
  }

  get families() {
    return this[PROP]("families");
  }

  has(familyName) {
    return this[CALL]("has", familyName);
  }

  family(name) {
    return this[CALL]("family", name);
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
      return this[CALL]("addFamilyFromData", alias, buffers);
    }

    // One native call for the whole of `use()`, a map's families included,
    // so that a font that fails to load registers none of them -- the same
    // all-or-nothing the crate's `FontLibrary::use_paths` gives.
    let sig = signature(args);
    if (sig == "o") {
      let entries = Object.entries(args.shift()),
        faces = this[CALL](
          "addFamily",
          entries.map(([alias, paths]) => [alias, [paths].flat()]),
        );
      return Object.fromEntries(entries.map(([alias], i) => [alias, faces[i]]));
    } else if (sig.match(/^s?[as]$/)) {
      let fonts = [args.pop()].flat();
      let alias = args.shift();
      return this[CALL]("addFamily", [[alias, fonts]])[0];
    } else {
      throw new Error(
        "Expected an array of file paths or an object mapping family names to font files",
      );
    }
  }

  reset() {
    return this[CALL]("reset");
  }
}

// What `measureText` packs into the buffers it hands over, published by Rust
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

// Lets `textMetrics` below past the constructor, which refuses everyone
// else.
const MEASURED = Symbol("measured");

// Reads a measurement's ink, taken off the class once it is built so that
// it is not reachable as a static.
let ink;

class TextMetrics {
  // The values every measurement has, read from what `measureText` answered.
  #values;
  // The ink box and `lines`, once worked out.
  #ink;
  // What the ink is worked out from, until then: a box the binding spends
  // on the first read.
  #pending;

  // Rest parameters so that `TextMetrics.length` is 0, as an interface
  // object's is.
  constructor(...args) {
    if (args[0] !== MEASURED) {
      throw new TypeError(
        `Function is not a constructor (use CanvasRenderingContext2D's "measureText" method instead)`,
      );
    }
    this.#values = args[1];
    this.#pending = args[2];
  }

  // HTML's attributes, in the order its interface declares them, then this
  // library's. Each reads a private field, so calling one on anything but a
  // measurement is a `TypeError`, as a browser's brand check makes it.
  get width() {
    return this.#values.width;
  }
  get actualBoundingBoxLeft() {
    return ink(this).actualBoundingBoxLeft;
  }
  get actualBoundingBoxRight() {
    return ink(this).actualBoundingBoxRight;
  }
  get fontBoundingBoxAscent() {
    return this.#values.fontBoundingBoxAscent;
  }
  get fontBoundingBoxDescent() {
    return this.#values.fontBoundingBoxDescent;
  }
  get actualBoundingBoxAscent() {
    return ink(this).actualBoundingBoxAscent;
  }
  get actualBoundingBoxDescent() {
    return ink(this).actualBoundingBoxDescent;
  }
  get emHeightAscent() {
    return this.#values.emHeightAscent;
  }
  get emHeightDescent() {
    return this.#values.emHeightDescent;
  }
  get hangingBaseline() {
    return this.#values.hangingBaseline;
  }
  get alphabeticBaseline() {
    return this.#values.alphabeticBaseline;
  }
  get ideographicBaseline() {
    return this.#values.ideographicBaseline;
  }
  get height() {
    return this.#values.height;
  }
  get lines() {
    return ink(this).lines;
  }

  /**
   * Every value, in the order 6.x serialised them as own properties, so that
   * `JSON.stringify` of a measurement gives what it gave. HTML's
   * `TextMetrics` has no `toJSON`; this is WebIDL's `[Default]` one, as
   * `DOMRect` has, over this library's values as well. It reads the ink, so
   * it works the ink out, once, where nothing before it has. Called on
   * anything but a measurement it is a `TypeError`, as the getters are.
   */
  toJSON() {
    const values = this.#values;
    const found = ink(this);
    return {
      width: values.width,
      actualBoundingBoxLeft: found.actualBoundingBoxLeft,
      actualBoundingBoxRight: found.actualBoundingBoxRight,
      actualBoundingBoxAscent: found.actualBoundingBoxAscent,
      actualBoundingBoxDescent: found.actualBoundingBoxDescent,
      fontBoundingBoxAscent: values.fontBoundingBoxAscent,
      fontBoundingBoxDescent: values.fontBoundingBoxDescent,
      emHeightAscent: values.emHeightAscent,
      emHeightDescent: values.emHeightDescent,
      hangingBaseline: values.hangingBaseline,
      alphabeticBaseline: values.alphabeticBaseline,
      ideographicBaseline: values.ideographicBaseline,
      height: values.height,
      lines: found.lines,
    };
  }

  // What Node's `console.log` and `util.inspect` show. A measurement has no
  // properties of its own, so they would print `TextMetrics {}`; this shows
  // what `toJSON` gives, working the ink out as reading every value does.
  [REPR](depth, options) {
    return `TextMetrics ${inspect(this.toJSON(), options)}`;
  }

  static {
    // Worked out on the first read and kept, so a second read of any of
    // these costs a property lookup.
    ink = (metrics) => {
      if (metrics.#ink === undefined) {
        // Looked up on every call rather than once, which is what lets a
        // test count the calls.
        const [data, families] = neon.CanvasRenderingContext2D.measureTextInk(
          metrics.#pending,
        );
        metrics.#pending = undefined;
        const at = { number: 0, name: 0 };
        const found = read(FIELDS.ink, data, at, families, {});
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
        found.lines = lines;
        metrics.#ink = found;
      }
      return metrics.#ink;
    };
  }
}

// WebIDL makes an interface's attributes and operations enumerable, where a
// class makes its accessors and methods not, so `for...in` over a
// measurement lists them as a browser's does.
for (const name of Object.getOwnPropertyNames(TextMetrics.prototype)) {
  if (name !== "constructor") {
    Object.defineProperty(TextMetrics.prototype, name, { enumerable: true });
  }
}

Object.defineProperty(TextMetrics.prototype, Symbol.toStringTag, {
  value: "TextMetrics",
  writable: false,
  enumerable: false,
  configurable: true,
});

/**
 * Builds the metrics from what `measureText` answered with: the values every
 * measurement has, and the ink still to be worked out.
 */
function textMetrics([data, pending]) {
  const values = read(FIELDS.metrics, data, { number: 0, name: 0 }, [], {});
  return new TextMetrics(MEASURED, values, pending);
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
    this[ALLOC](style || {});
  }
  static Make(style) {
    return new ParagraphBuilder(style);
  }
  pushStyle(style) {
    this[CALL]("pushStyle", style || {});
    return this;
  }
  pop() {
    this[CALL]("pop");
    return this;
  }
  addText(text) {
    this[CALL]("addText", text);
    return this;
  }
  addPlaceholder(width, height, align, baseline, offset) {
    this[CALL](
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
    return wrap(Paragraph, this[CALL]("build"));
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
    this[CALL]("layout", width);
  }
  getHeight() {
    return this[CALL]("getHeight");
  }
  getLongestLine() {
    return this[CALL]("getLongestLine");
  }
  getMaxWidth() {
    return this[CALL]("getMaxWidth");
  }
  getMaxIntrinsicWidth() {
    return this[CALL]("getMaxIntrinsicWidth");
  }
  getMinIntrinsicWidth() {
    return this[CALL]("getMinIntrinsicWidth");
  }
  getAlphabeticBaseline() {
    return this[CALL]("getAlphabeticBaseline");
  }
  getIdeographicBaseline() {
    return this[CALL]("getIdeographicBaseline");
  }
  getGlyphPositionAtCoordinate(x, y) {
    return this[CALL]("getGlyphPositionAtCoordinate", x, y);
  }
  getRectsForRange(start, end, hStyle, wStyle) {
    return this[CALL]("getRectsForRange", start, end, hStyle || 0, wStyle || 0);
  }
  getLineMetrics() {
    return this[CALL]("getLineMetrics");
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
    return this[CALL]("didExceedMaxLines");
  }
  getNumberOfLines() {
    return this[CALL]("getNumberOfLines");
  }
  getRectsForPlaceholders() {
    return this[CALL]("getRectsForPlaceholders");
  }
  getUnresolvedCodepoints() {
    return this[CALL]("getUnresolvedCodepoints");
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
