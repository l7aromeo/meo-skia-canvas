// The decoration metrics a TrueType or OpenType file states, read from the
// file itself, so a test can hold drawn lines to what the font says rather
// than to what the library reports.
//
// - `post.underlinePosition`: "Suggested y-coordinate of the top of the
//   underline", and `post.underlineThickness` (OpenType 1.9.1, 'post').
// - `OS/2.yStrikeoutPosition`: "The position of the top of the strikeout
//   stroke relative to the baseline", positive above, and
//   `OS/2.yStrikeoutSize`: "Thickness of the strikeout stroke" (OS/2).
// - `hhea.ascender`, the ascent an overline sits on, and `hhea.descender`,
//   negative below the baseline: with them, the content area a background
//   covers.
//
// Values are in font units; `at(size)` scales them to pixels.

"use strict";

const fs = require("fs");

/** The offset of each table in an sfnt `data`, by tag. */
const tables = (data) => {
  const count = data.readUInt16BE(4),
    out = {};
  for (let i = 0; i < count; i++) {
    const at = 12 + 16 * i;
    out[data.toString("latin1", at, at + 4)] = data.readUInt32BE(at + 8);
  }
  return out;
};

/** The decoration metrics of the font file at `path`. */
const fontTables = (path) => {
  const data = fs.readFileSync(path),
    t = tables(data),
    upm = data.readUInt16BE(t.head + 18);
  const units = {
    upm,
    underlinePosition: data.readInt16BE(t.post + 8),
    underlineThickness: data.readInt16BE(t.post + 10),
    strikeoutSize: data.readInt16BE(t["OS/2"] + 26),
    strikeoutPosition: data.readInt16BE(t["OS/2"] + 28),
    ascender: data.readInt16BE(t.hhea + 4),
    descender: data.readInt16BE(t.hhea + 6),
  };
  return {
    ...units,
    /** The same metrics in pixels at `size`, every position measured up
     * from the baseline. */
    at: (size) => {
      const k = size / upm;
      return {
        underlineTop: units.underlinePosition * k,
        underlineThickness: units.underlineThickness * k,
        strikeoutTop: units.strikeoutPosition * k,
        strikeoutThickness: units.strikeoutSize * k,
        ascent: units.ascender * k,
        descent: -units.descender * k,
      };
    },
  };
};

module.exports = { fontTables };
