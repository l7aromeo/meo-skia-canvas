//
// Font management & metrics
//

"use strict";

const {
  RustClass,
  core,
  wrap,
  readOnly,
  signature,
  inspect,
  REPR,
} = require("./neon");

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
  constructor(metrics) {
    for (let k in metrics) readOnly(this, k, metrics[k]);
  }
}

class ParagraphBuilder extends RustClass {
  static Make(style, fontLibrary) {
    let pb = new ParagraphBuilder();
    pb.alloc(style || {});
    return pb;
  }
  constructor() {
    super(ParagraphBuilder);
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
  constructor() {
    super(Paragraph);
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

const TextDecoration = Object.freeze({
  NoDecoration: 0x0,
  Underline: 0x1,
  Overline: 0x2,
  LineThrough: 0x4,
});

const TextDecorationStyle = Object.freeze({
  Solid: 0,
  Double: 1,
  Dotted: 2,
  Dashed: 3,
  Wavy: 4,
});

module.exports = {
  FontLibrary: new FontLibrary(),
  TextMetrics,
  ParagraphBuilder,
  Paragraph,
  TextDecoration,
  TextDecorationStyle,
};
