// @ts-check

"use strict";

// CSS grammars both surfaces parse, each checked against one table of cases
// that `tests/css_grammar.rs` reads too. The expected answers were measured in
// Chrome rather than taken from either surface.

const { assert, describe, test } = require("../runner"),
  { DOMMatrix } = require("../../lib"),
  transforms = require("../assets/cases/css-transforms.json");

// Chrome resolves a length in float32: `2cm` reads 75.59055328 where
// 2 * 96 / 2.54 is 75.59055118. Wide enough for that, and far narrower than
// any difference a wrong composition makes.
const TOLERANCE = 1e-5;

describe("CSS transform lists", () => {
  test("DOMMatrix.setMatrixValue answers the transform table", () => {
    const wrong = [];
    for (const { css, matrix } of transforms.cases) {
      let got;
      try {
        const m = new DOMMatrix();
        m.setMatrixValue(css);
        got = [m.a, m.b, m.c, m.d, m.e, m.f];
      } catch (error) {
        // Chrome's is a DOMException; a plain JavaScript SyntaxError has the
        // same name and is not one.
        got =
          error instanceof DOMException
            ? error.name
            : `${error.constructor.name}, not a DOMException`;
      }
      if (matrix) {
        const close =
          Array.isArray(got) &&
          got.every((value, i) => Math.abs(value - matrix[i]) <= TOLERANCE);
        if (!close)
          wrong.push(`${JSON.stringify(css)}: ${got}, Chrome ${matrix}`);
      } else if (got !== "SyntaxError") {
        wrong.push(
          `${JSON.stringify(css)}: ${got}, Chrome throws a SyntaxError DOMException`,
        );
      }
    }
    assert.deepEqual(wrong, []);
  });
});
