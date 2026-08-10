//
// TextDecoration — CanvasKit-compatible decoration constants
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

module.exports = { TextDecoration, TextDecorationStyle };
