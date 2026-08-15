//
// CanvasKit-compatible text constants
//
// Their own module, with no imports, so `browser.js` can export them. They are
// plain integers with no native counterpart; leaving them in typography.js put
// them behind that file's `require("./neon")` and therefore behind the native
// binary, which a browser bundle cannot load.
//

"use strict";

// Bit flags, so they combine: Underline | LineThrough === 0x5.
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

// How a placeholder sits against the line it interrupts, for
// `ParagraphBuilder.addPlaceholder`.
const PlaceholderAlignment = Object.freeze({
  Baseline: 0,
  AboveBaseline: 1,
  BelowBaseline: 2,
  Top: 3,
  Bottom: 4,
  Middle: 5,
});

// Which baseline `PlaceholderAlignment.Baseline` aligns against.
const TextBaseline = Object.freeze({
  Alphabetic: 0,
  Ideographic: 1,
});

// How tall the rectangles `Paragraph.getRectsForRange` returns are. A
// selection highlight and a hit test want different answers from the same
// range: the highlight should meet its neighbours with no gap, the hit test
// should cover only the glyphs.
const RectHeightStyle = Object.freeze({
  Tight: 0,
  Max: 1,
  IncludeLineSpacingMiddle: 2,
  IncludeLineSpacingTop: 3,
  IncludeLineSpacingBottom: 4,
  Strut: 5,
});

// How wide they are: only the glyphs, or out to the edge of the line.
const RectWidthStyle = Object.freeze({
  Tight: 0,
  Max: 1,
});

module.exports = {
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
};
