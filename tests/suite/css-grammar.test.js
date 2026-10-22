// @ts-check

"use strict";

// CSS grammars both surfaces parse, each checked against one table of cases
// that `tests/css_grammar.rs` reads too. The expected answers were measured in
// Chrome rather than taken from either surface.

const { assert, describe, test } = require("../runner"),
  { Canvas, DOMMatrix } = require("../../lib"),
  css = require("../../lib/classes/css"),
  fonts = require("../assets/cases/css-fonts.json"),
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

describe("CSS font shorthands", () => {
  test("ctx.font answers the font table", () => {
    // Set before each case, so a refusal shows as all of it surviving and a
    // font taken shows the caps and stretch reset, as Chrome's setter resets
    // them. `font` reads it back in Chrome's form, with no stretch.
    const SENTINEL = "small-caps ultra-expanded 7px Sentinel",
      ignored =
        "small-caps 7px Sentinel, caps small-caps, stretch ultra-expanded";

    const ctx = new Canvas(10, 10).getContext("2d"),
      wrong = [];
    for (const { css, font, variantCaps, stretch } of fonts.cases) {
      ctx.font = SENTINEL;
      ctx.font = css;
      const got = `${ctx.font}, caps ${ctx.fontVariantCaps}, stretch ${ctx.fontStretch}`,
        want =
          font === undefined
            ? ignored
            : `${font}, caps ${variantCaps}, stretch ${stretch}`;
      if (got !== want)
        wrong.push(
          `${JSON.stringify(css)}: ${got}; Chrome ${font === undefined ? "ignores it" : want}`,
        );
    }
    assert.deepEqual(wrong, []);
  });

  // What the addon is handed, against Chrome's answer taken apart: the parts
  // the getters do not show, the line height and the caps features among
  // them.
  test("the parsed font matches the table's parts", () => {
    // Chrome reports a size to six significant digits, `2cm` as `75.5906px`.
    const SIZE_TOLERANCE = 1e-5;

    const wrong = [];
    for (const { css: text, parts } of fonts.cases) {
      const spec = css.font(text);
      if (!parts) {
        if (spec)
          wrong.push(`${JSON.stringify(text)}: read, Chrome ignores it`);
        continue;
      }
      if (!spec) {
        wrong.push(`${JSON.stringify(text)}: refused`);
        continue;
      }
      const got = {
          size: spec.size,
          weight: spec.weight,
          italic: spec.style != "normal",
          families: spec.family,
          features: spec.features.on ?? [],
          stretch: spec.stretch,
          lineHeight: spec.lineHeight ?? null,
        },
        want = {
          size: parts.size,
          weight: parts.weight,
          italic: parts.style == "italic",
          families: parts.families,
          // CSS Fonts 4 section 6.7: `small-caps` is `smcp`, and nothing else.
          features: parts.variantCaps == "small-caps" ? ["smcp"] : [],
          stretch: parts.stretch,
          lineHeight: "lineHeight" in parts ? parts.lineHeight : got.lineHeight,
        };
      const agrees =
        Math.abs(got.size - want.size) <=
          SIZE_TOLERANCE * Math.max(1, want.size) &&
        JSON.stringify({ ...got, size: 0 }) ==
          JSON.stringify({ ...want, size: 0 });
      if (!agrees)
        wrong.push(
          `${JSON.stringify(text)}: ${JSON.stringify(got)}, Chrome ${JSON.stringify(want)}`,
        );
    }
    assert.deepEqual(wrong, []);
  });
});
