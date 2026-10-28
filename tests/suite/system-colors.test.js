// @ts-check

"use strict";

// CSS Color 4's system colours against the table src/node/utils.rs reads
// too, each row as Chrome 156 reads and paints it in its light scheme. The
// getters report `expected`; a refused string leaves the previous colour; and
// the pixel is Chrome's to one level, or the row's own `pixel` where Chrome
// refuses a colour this library reads.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  table = require("../assets/cases/css-system-colors.json");

/** What a getter keeps when the setter ignores a string. */
const PREVIOUS = "#010203";

describe("System colours, as Chrome reads them on a canvas", () => {
  test("the table has every row", () => {
    assert.ok(table.cases.length >= 50, `${table.cases.length} rows`);
  });

  for (const { css, expected, chrome, chromePixel, pixel } of table.cases) {
    test(css, () => {
      const ctx = new Canvas(1, 1).getContext("2d");
      for (const property of ["fillStyle", "strokeStyle", "shadowColor"]) {
        ctx[property] = PREVIOUS;
        ctx[property] = css;
        assert.equal(ctx[property], expected ?? PREVIOUS, property);
      }
      const want = chrome === null ? pixel : chromePixel;
      if (want === undefined) return;
      ctx.fillStyle = css;
      ctx.fillRect(0, 0, 1, 1);
      const painted = [...ctx.getImageData(0, 0, 1, 1).data];
      assert.ok(
        painted.every((level, i) => Math.abs(level - want[i]) <= 1),
        `${painted} against ${want}`,
      );
    });
  }
});
