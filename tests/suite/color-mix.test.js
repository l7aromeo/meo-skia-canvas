// @ts-check

"use strict";

// color-mix() against the table src/node/utils.rs reads too, each row as
// Chrome 156 reads, paints and takes it as a gradient stop. The getters
// report `expected`; `expected` paints exactly what the mix does; a stop
// takes what fillStyle takes; and where a row has no `why`, the pixel is
// Chrome's to one level.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  table = require("../assets/cases/css-color-mix.json");

/** What a getter keeps when the setter ignores a string. */
const PREVIOUS = "#010203";

/** Each a canvas a read-back must paint exactly as the mix does. */
const COLOR_TYPES = ["rgba", "RGBAF16", "RGBAF32"];

/** The pixel `style` paints over a 1x1 canvas of `colorType`, as numbers. */
function painted(style, colorType = "rgba") {
  const canvas = new Canvas(1, 1, { colorType });
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = style;
  ctx.fillRect(0, 0, 1, 1);
  const { data } = ctx.getImageData(0, 0, 1, 1, { colorType });
  return colorType === "RGBAF32"
    ? [...new Float32Array(data.buffer, data.byteOffset, 4)]
    : [...data];
}

describe("color-mix() as CSS Color 5 section 3 defines it", () => {
  test("the table has every row", () => {
    assert.ok(table.cases.length >= 90, `${table.cases.length} rows`);
  });

  for (const { css, expected, chromePixel, why } of table.cases) {
    test(css, () => {
      const ctx = new Canvas(1, 1).getContext("2d");
      for (const property of ["fillStyle", "strokeStyle", "shadowColor"]) {
        ctx[property] = PREVIOUS;
        ctx[property] = css;
        assert.equal(ctx[property], expected ?? PREVIOUS, property);
      }

      const stop = () =>
        ctx.createLinearGradient(0, 0, 1, 0).addColorStop(0, css);
      if (expected === null) {
        assert.throws(stop, { name: "SyntaxError" }, "a stop refuses it");
        return;
      }
      stop();

      for (const colorType of COLOR_TYPES)
        assert.deepEqual(
          painted(expected, colorType),
          painted(css, colorType),
          `${colorType}: the read-back paints the mix`,
        );

      if (why === undefined) {
        const pixel = painted(css);
        assert.ok(
          pixel.every((level, i) => Math.abs(level - chromePixel[i]) <= 1),
          `${pixel} against Chrome's ${chromePixel}`,
        );
      }
    });
  }
});
