// @ts-check

"use strict";

// CSS grammars both surfaces parse, each checked against one table of cases
// that `tests/css_grammar.rs` reads too. The expected answers were measured in
// Chrome rather than taken from either surface.

const { assert, describe, test } = require("../runner"),
  { Canvas, DOMMatrix, FontLibrary } = require("../../lib"),
  filters = require("../assets/cases/css-filters.json"),
  fonts = require("../assets/cases/css-fonts.json"),
  math = require("../assets/cases/css-math.json"),
  spacing = require("../assets/cases/css-spacing.json"),
  transforms = require("../assets/cases/css-transforms.json");

// Oswald, the variable face the font-relative rows were measured on in
// Chrome, under the family name the rows give.
FontLibrary.use("Oswald", [
  "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
]);

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
});

describe("CSS filters and spacing", () => {
  // Whether a value is taken, judged by whether it displaced a sentinel; a
  // setter that throws is wrong whatever Chrome does, since the Canvas
  // standard has every one of these ignore what it cannot parse.
  const assign = (ctx, property, sentinel, css) => {
    ctx[property] = sentinel;
    try {
      ctx[property] = css;
    } catch (error) {
      return `throws ${error.constructor.name}: ${error.message}`;
    }
    return ctx[property] === sentinel ? undefined : ctx[property];
  };

  test("ctx.filter answers the filter table", () => {
    const ctx = new Canvas(10, 10).getContext("2d"),
      wrong = [];
    for (const { css, refused } of filters.cases) {
      const got = assign(ctx, "filter", "blur(3px)", css),
        taken = got !== undefined && !got.startsWith("throws ");
      if (got?.startsWith("throws ") || taken == !!refused)
        wrong.push(
          `${JSON.stringify(css)}: ${got ?? "ignored"}, Chrome ${refused ? "ignores it" : "takes it"}`,
        );
      // Chrome reads every filter it takes back exactly as it was written.
      else if (taken && got !== css)
        wrong.push(
          `${JSON.stringify(css)}: reads back ${JSON.stringify(got)}, Chrome as written`,
        );
    }
    assert.deepEqual(wrong, []);
  });

  // The readback is the string as set and what draws is the parse, so two
  // spellings of one filter draw the same pixels: the spellings this library
  // used to read back, against the ones Chrome does.
  test("a filter draws the same whichever spelling it was set with", () => {
    const pairs = [
      ["BLUR(5PX)", "blur(5px)"],
      ["blur( 5px )", "blur(5px)"],
      ["blur(5px)  brightness(2)", "blur(5px) brightness(2)"],
      ["blur(5px)brightness(2)", "blur(5px) brightness(2)"],
      ["  blur(5px)  ", "blur(5px)"],
      ["brightness(1E1)", "brightness(1e1)"],
      ["drop-shadow(red 2px 2px)", "drop-shadow(2px 2px 0px red)"],
      ["drop-shadow(2px 2px)", "drop-shadow(2px 2px 0px black)"],
      ["drop-shadow(2px  2px   RED)", "drop-shadow(2px 2px 0px RED)"],
      [
        "drop-shadow(2px 2px rgb(255 0 0))",
        "drop-shadow(2px 2px 0px rgb(255 0 0))",
      ],
      ["Drop-Shadow(2px 2px 4px Red)", "drop-shadow(2px 2px 4px Red)"],
      ["NONE", "none"],
      [" none ", "none"],
      ["blur(2PT)", "blur(2pt)"],
    ];
    const draw = (filter) => {
      const ctx = new Canvas(40, 30).getContext("2d");
      ctx.filter = filter;
      ctx.fillStyle = "rgba(20, 120, 220, 0.8)";
      ctx.fillRect(8, 6, 14, 12);
      ctx.fillStyle = "#c00";
      ctx.beginPath();
      ctx.arc(26, 18, 6, 0, Math.PI * 2);
      ctx.fill();
      return Buffer.from(ctx.getImageData(0, 0, 40, 30).data);
    };
    assert.ok(
      !draw("blur(5px)").equals(draw("blur(2px)")),
      "two different blurs draw differently",
    );
    const differ = pairs
      .filter(([asSet, other]) => !draw(asSet).equals(draw(other)))
      .map(([asSet]) => asSet);
    assert.deepEqual(differ, []);
  });

  // Chrome resolves `em` when the filter is set, and setting the same string
  // again after the font changed resolves it again: in Chrome 152 a 20px
  // square under `blur(1em)` inks 76 pixels of a 200-pixel row at 10px and
  // 200 at 40px, and set at 10px then again at 40px it inks 200.
  test("the same em filter set again after a font change is redrawn", () => {
    const inked = (setup) => {
      const ctx = new Canvas(200, 60).getContext("2d");
      setup(ctx);
      ctx.fillRect(90, 20, 20, 20);
      const row = ctx.getImageData(0, 30, 200, 1).data;
      return row.filter((v, i) => i % 4 == 3 && v > 0).length;
    };
    const at40 = inked((ctx) => {
      ctx.font = "40px sans-serif";
      ctx.filter = "blur(1em)";
    });
    const setAgain = inked((ctx) => {
      ctx.font = "10px sans-serif";
      ctx.filter = "blur(1em)";
      ctx.font = "40px sans-serif";
      ctx.filter = "blur(1em)";
    });
    const notAgain = inked((ctx) => {
      ctx.font = "10px sans-serif";
      ctx.filter = "blur(1em)";
      ctx.font = "40px sans-serif";
    });
    assert.equal(setAgain, at40);
    assert.ok(notAgain < at40, `${notAgain} under the 10px blur kept`);
  });

  test("letterSpacing and wordSpacing answer the spacing table", () => {
    const ctx = new Canvas(10, 10).getContext("2d"),
      wrong = [];
    for (const { css, read } of spacing.cases)
      for (const property of ["letterSpacing", "wordSpacing"]) {
        const got = assign(ctx, property, "3px", css);
        if (got !== read)
          wrong.push(
            `${property} ${JSON.stringify(css)}: ${got ?? "ignored"}, Chrome ${read ?? "ignores it"}`,
          );
      }
    assert.deepEqual(wrong, []);
  });

  test("letterSpacing lays out the spacing table's pixels", () => {
    // The rows with a `font` give the spacing Chrome laid out in pixels, and
    // each is checked by laying the same text out with that many pixels
    // written instead. Layout is the same code either way, so this isolates
    // what the unit resolves to.
    const wrong = [];
    for (const { font, css, px } of spacing.cases.filter((c) => c.font)) {
      const ctx = new Canvas(10, 10).getContext("2d");
      ctx.font = font;
      const width = (letterSpacing) => {
        ctx.letterSpacing = letterSpacing;
        return ctx.measureText("abcd").width;
      };
      const [got, want] = [width(css), width(`${px}px`)];
      if (Math.abs(got - want) > 1e-3)
        wrong.push(`${css} in ${font}: ${got}, as ${px}px ${want}`);
    }
    assert.deepEqual(wrong, []);
  });

  test("ctx.filter answers the math table", () => {
    // Each taken row draws the same pixels as the plain filter Chrome drew
    // it as, over the scene it was measured on; each refused row leaves the
    // filter before it standing.
    const filtered = (font, filter) => {
      const ctx = new Canvas(200, 80).getContext("2d");
      if (font) ctx.font = font;
      ctx.filter = filter;
      const taken = ctx.filter === filter;
      ctx.fillStyle = "rgb(40,90,20)";
      ctx.fillRect(60, 20, 60, 30);
      ctx.fillStyle = "rgba(200,50,160,0.6)";
      ctx.fillRect(90, 35, 50, 30);
      return {
        taken,
        pixels: Buffer.from(ctx.getImageData(0, 0, 200, 80).data),
      };
    };
    const wrong = [];
    for (const { font, css, draws } of math.cases) {
      const got = filtered(font, css);
      if (!draws) {
        if (got.taken) wrong.push(`${css}: taken, Chrome ignores it`);
      } else if (!got.taken) wrong.push(`${css}: ignored`);
      else if (!got.pixels.equals(filtered(font, draws).pixels))
        wrong.push(`${css}: does not draw as ${draws}`);
    }
    assert.deepEqual(wrong, []);
  });

  test("an em of spacing is the font's when the text is laid out", () => {
    // Chrome 152 widens four characters of 40px monospace by 40px each when
    // `1em` was set under a 20px font and the font changed afterwards: the
    // em is resolved at layout, not when it is set.
    const ctx = new Canvas(10, 10).getContext("2d"),
      width = () => ctx.measureText("abcd").width;
    ctx.font = "40px monospace";
    ctx.letterSpacing = "0px";
    const unspaced = width();
    ctx.font = "20px monospace";
    ctx.letterSpacing = "1em";
    ctx.font = "40px monospace";
    assert.nearEqual((width() - unspaced) / 4, 40);
  });
});
