// @ts-check

"use strict";

// What the colour getters read back, by CSS Color 4 section 16, against the
// table `src/node/utils.rs` reads too. Each row is checked three ways: the
// getters report `expected`; `expected` set again reports itself; and the two
// paint the same pixels, which is what makes the read-back the colour held.

const { assert, describe, test } = require("../runner"),
  { Canvas, Image } = require("../../lib"),
  table = require("../assets/cases/css-color-serialisation.json");

/** What a getter keeps when the setter ignores a string. */
const PREVIOUS = "#010203";

// Each a canvas the read-back must paint exactly as the original does. A
// float canvas is the strict one: six decimal places would put `#ff000080`'s
// 128/255 1.8e-7 away there, which an eight-bit or half-float canvas hides.
const COLOR_TYPES = ["rgba", "RGBAF16", "RGBAF32"];

/** The pixel `style` paints over a 1x1 canvas of `colorType`, as numbers. */
function painted(style, colorType) {
  const canvas = new Canvas(1, 1, { colorType });
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = style;
  ctx.fillRect(0, 0, 1, 1);
  const { data } = ctx.getImageData(0, 0, 1, 1, { colorType });
  return colorType === "RGBAF32"
    ? [...new Float32Array(data.buffer, data.byteOffset, 4)]
    : [...data];
}

describe("Colour getters serialise by CSS Color 4 section 16", () => {
  test("the table has every row", () => {
    assert.ok(table.cases.length >= 80, `${table.cases.length} rows`);
  });

  for (const { css, expected } of table.cases) {
    test(css, () => {
      const ctx = new Canvas(1, 1).getContext("2d");
      for (const property of ["fillStyle", "strokeStyle", "shadowColor"]) {
        ctx[property] = PREVIOUS;
        ctx[property] = css;
        assert.equal(ctx[property], expected ?? PREVIOUS, property);
      }
      if (expected === null) return;

      ctx.fillStyle = expected;
      assert.equal(ctx.fillStyle, expected, "set again, it reads as itself");

      for (const colorType of COLOR_TYPES)
        assert.deepEqual(
          painted(expected, colorType),
          painted(css, colorType),
          `${colorType}: the read-back paints the same pixel`,
        );
    });
  }

  test("a short value reads back short", () => {
    // The fewest figures that repaint the colour, so a value with a short
    // spelling takes it: the exact form would write hwb()'s 114.75 as
    // 114.74999999999999, the f64 the conversion leaves.
    const ctx = new Canvas(1, 1).getContext("2d");
    for (const [css, read] of [
      ["rgba(255,0,255,0.5)", "rgba(255, 0, 255, 0.5)"],
      ["hwb(90 10% 20%)", "rgb(114.75, 204, 25.5)"],
      ["lab(29% 39 20)", "lab(29 39 20)"],
      ["oklch(0.7 0.35 30)", "oklch(0.7 0.35 30)"],
      ["color(display-p3 1 0 0 / 0.25)", "color(display-p3 1 0 0 / 0.25)"],
    ]) {
      ctx.fillStyle = css;
      assert.equal(ctx.fillStyle, read, css);
    }
  });

  test("a set is read back as itself after the colour before it was read", () => {
    // The read-back is written once per colour set and kept; a set must
    // never be answered with the string of the colour it replaced.
    const first = "lab(from red l a b)",
      second = "oklch(0.7 0.35 30)";
    const ctx = new Canvas(1, 1).getContext("2d");
    for (const property of ["fillStyle", "strokeStyle", "shadowColor"]) {
      ctx[property] = first;
      const read = ctx[property];
      assert.equal(ctx[property], read, `${property} read twice`);
      ctx[property] = second;
      assert.equal(ctx[property], second, property);

      // `save()` keeps the colour, and `restore()` brings back its string.
      ctx[property] = first;
      ctx.save();
      ctx[property] = second;
      assert.equal(ctx[property], second, `${property} after save`);
      ctx.restore();
      assert.equal(ctx[property], read, `${property} after restore`);
    }
    const image = new Image();
    image.currentColor = first;
    const read = image.currentColor;
    image.currentColor = second;
    assert.equal(image.currentColor, second, "currentColor");
    assert.notEqual(read, second);
  });

  test("alpha 1 and alpha 0 are exact", () => {
    const ctx = new Canvas(1, 1).getContext("2d");
    for (const [css, read] of [
      ["#ff000000", "rgba(255, 0, 0, 0)"],
      ["transparent", "rgba(0, 0, 0, 0)"],
      ["#ff0000ff", "#ff0000"],
      ["rgb(255 0 0 / 1)", "#ff0000"],
    ]) {
      ctx.fillStyle = css;
      assert.equal(ctx.fillStyle, read, css);
    }
  });
});
