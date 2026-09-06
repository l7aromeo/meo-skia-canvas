// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

describe("letterSpacing measures a space after every character", () => {
  // CSS adds `letter-spacing` after each character including the last, so an
  // `n`-character run is `n` spaces wider than an unspaced one. This measured
  // `n - 1`: a whole space was subtracted from the laid-out box, so a
  // one-character string at any spacing measured exactly what it measures at
  // no spacing at all.
  const ctx = () => {
    const c = new Canvas(600, 80),
      ctx = c.getContext("2d");
    ctx.font = "40px Helvetica";
    return ctx;
  };

  // Where the first and last inked columns fall for a left-aligned draw at
  // x=0, so the rendering can be compared separately from the advance.
  const ink = (ctx, text) => {
    ctx.clearRect(0, 0, 600, 80);
    ctx.fillStyle = "black";
    ctx.fillText(text, 0, 60);
    const d = ctx.getImageData(0, 0, 600, 80).data;
    let lo = 600,
      hi = -1;
    for (let i = 3; i < d.length; i += 4)
      if (d[i] > 0) {
        const x = ((i - 3) / 4) % 600;
        if (x < lo) lo = x;
        if (x > hi) hi = x;
      }
    return hi < 0 ? null : [lo, hi];
  };

  test("one space per character, including a string of one", () => {
    const c = ctx();
    // The anchor: the unspaced widths are what the spaced ones are measured
    // against, so they have to be established first. Without this the loop
    // below is satisfied by any implementation that adds a constant, or by
    // one that adds nothing to either side of the subtraction.
    c.letterSpacing = "0px";
    const bare = ["a", "ab", "abc", "abcd"].map((s) => c.measureText(s).width);
    assert.ok(
      bare[3] > bare[0],
      `unspaced widths must grow with the string: ${bare}`,
    );

    c.letterSpacing = "10px";
    ["a", "ab", "abc", "abcd"].forEach((s, i) => {
      // A one-character string is the case that separates the two rules: at
      // `n - 1` it gains nothing at all.
      assert.nearEqual(
        c.measureText(s).width,
        bare[i] + 10 * s.length,
        `"${s}" gains one space per character`,
      );
    });
  });

  test("and the drawn glyphs do not move when it changes", () => {
    // The subtraction only ever reached the reported box. Asserting that the
    // ink is unchanged is what keeps a future fix from paying for the width
    // by shifting the text: the two halves have to move together or not at
    // all, and here they do not.
    const c = ctx();
    c.letterSpacing = "0px";
    const unspaced = ink(c, "abcd");
    c.letterSpacing = "10px";
    const spaced = ink(c, "abcd");

    assert.equal(
      unspaced[0],
      spaced[0],
      "the first glyph starts in the same place",
    );
    assert.ok(
      spaced[1] > unspaced[1],
      `spacing widens the run: ${spaced} against ${unspaced}`,
    );
  });
});
