// @ts-check

// A colour built into a texture or a CanvasKit filter before the canvas it
// lands on is known paints there as a fill of the same colour does. The
// binding builds these filters itself, apart from the crate's, so the
// crate's tests/canvaskit_colour_exact.rs cannot say so for it.

"use strict";

const { assert, describe, test } = require("../runner"),
  {
    Canvas,
    CanvasTexture,
    ColorFilter,
    ImageFilter,
    Path2D,
  } = require("../../lib");

// The side of the square every case is drawn in, and read from the middle
// of.
const SIDE = 8;

// Nineteen greys named in linear light, opaque and at half alpha: a colour
// in a space other than the canvas's, which is the one Skia converted.
const GREYS = [1, 0.5].flatMap((alpha) =>
  Array.from(
    { length: 19 },
    (_, i) =>
      `color(srgb-linear ${(i + 1) / 20} ${(i + 1) / 20} ${(i + 1) / 20} / ${alpha})`,
  ),
);

/** The middle pixel of what `draw` paints on an F32 canvas in `colorSpace`. */
function painted(colorSpace, draw) {
  const canvas = new Canvas(SIDE, SIDE, { colorType: "RGBAF32", colorSpace });
  const ctx = canvas.getContext("2d");
  draw(ctx);
  const { data } = ctx.getImageData(SIDE / 2, SIDE / 2, 1, 1, {
    colorType: "RGBAF32",
    colorSpace,
  });
  return [...new Float32Array(data.buffer, data.byteOffset, 4)];
}

/** The greys `draw`, handed each, paints otherwise than a fill of it. */
function misses(colorSpace, draw) {
  return GREYS.filter((grey) => {
    const want = painted(colorSpace, (ctx) => {
      ctx.fillStyle = grey;
      ctx.fillRect(0, 0, SIDE, SIDE);
    });
    const got = painted(colorSpace, (ctx) => draw(ctx, grey));
    return got.some((channel, i) => channel !== want[i]);
  });
}

// Every space a float canvas lands a fill in exactly: sRGB, where Skia's
// conversion of a colour is the identity, and three where the canvas keeps
// a recording for its pixels.
const SPACES = ["srgb", "srgb-linear", "display-p3", "rec2020"];

/** The space and grey of each case `draw` paints otherwise than a fill. */
function missesEverywhere(draw) {
  return SPACES.flatMap((colorSpace) =>
    misses(colorSpace, draw).map((grey) => `${colorSpace}: ${grey}`),
  );
}

/** White over the square, which a filter turns into its colour. */
function white(ctx) {
  ctx.fillStyle = "white";
  ctx.fillRect(0, 0, SIDE, SIDE);
}

describe("a colour built into a texture or a CanvasKit filter", () => {
  test("a texture's mark lands as a fill of its colour", () => {
    const mark = new Path2D("M-1 -1H9V9H-1Z");
    assert.deepEqual(
      missesEverywhere((ctx, color) => {
        ctx.fillStyle = new CanvasTexture(SIDE, { path: mark, color });
        ctx.fillRect(0, 0, SIDE, SIDE);
      }),
      [],
    );
  });

  test("a blend colour filter lands as a fill of its colour", () => {
    assert.deepEqual(
      missesEverywhere((ctx, color) => {
        ctx.colorFilter = new ColorFilter("blend", color, "copy");
        white(ctx);
      }),
      [],
    );
  });

  test("a colour filter built over a blend lands as a fill of its colour", () => {
    const identity = () =>
      new ColorFilter(
        "matrix",
        [1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0],
      );
    for (const build of [
      (/** @type {ColorFilter} */ blend) =>
        new ColorFilter("compose", identity(), blend),
      (/** @type {ColorFilter} */ blend) =>
        new ColorFilter("lerp", 1, identity(), blend),
    ]) {
      assert.deepEqual(
        missesEverywhere((ctx, color) => {
          ctx.colorFilter = build(new ColorFilter("blend", color, "copy"));
          white(ctx);
        }),
        [],
      );
    }
  });

  test("a drop shadow lands as a fill of its colour", () => {
    assert.deepEqual(
      missesEverywhere((ctx, color) => {
        ctx.imageFilter = new ImageFilter(
          "drop-shadow-only",
          0,
          0,
          0,
          0,
          color,
        );
        white(ctx);
      }),
      [],
    );
    // Beside what casts it: cast from a square off the canvas, so that the
    // shadow alone falls on the square read.
    assert.deepEqual(
      missesEverywhere((ctx, color) => {
        ctx.imageFilter = new ImageFilter("drop-shadow", SIDE, 0, 0, 0, color);
        ctx.fillStyle = "white";
        ctx.fillRect(-SIDE, 0, SIDE, SIDE);
      }),
      [],
    );
  });

  test("an image filter built over one holding a colour lands as a fill of it", () => {
    for (const build of [
      (/** @type {string} */ color) =>
        new ImageFilter(
          "color-filter",
          new ColorFilter("blend", color, "source-in"),
        ),
      (/** @type {string} */ color) =>
        new ImageFilter(
          "compose",
          new ImageFilter("offset", 0, 0),
          new ImageFilter("drop-shadow-only", 0, 0, 0, 0, color),
        ),
      (/** @type {string} */ color) =>
        new ImageFilter(
          "offset",
          0,
          0,
          new ImageFilter("drop-shadow-only", 0, 0, 0, 0, color),
        ),
      (/** @type {string} */ color) =>
        new ImageFilter("merge", [
          new ImageFilter("drop-shadow-only", 0, 0, 0, 0, color),
        ]),
    ]) {
      assert.deepEqual(
        missesEverywhere((ctx, color) => {
          ctx.imageFilter = build(color);
          white(ctx);
        }),
        [],
      );
    }
  });
});

describe("a CanvasKit light on an eight-bit page", () => {
  /** The middle pixel of what `draw` paints on an eight-bit sRGB canvas. */
  const at = (/** @type {(ctx: any) => void} */ draw) => {
    const canvas = new Canvas(SIDE, SIDE);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    draw(ctx);
    return [...ctx.getImageData(SIDE / 2, SIDE / 2, 1, 1).data];
  };

  test("lights a flat surface overhead at its own colour", () => {
    // Straight overhead a flat surface takes the light at N.L = 1, and its
    // highlight at N.H = 1, so a diffuse light at kd 1 and a specular one at
    // ks 1 and exponent 1 paint the light's colour. A specular result's
    // alpha is its largest channel, so those colours carry a 255. The odd
    // channels hold the light to its own bytes: a light half a level off one
    // lands on the even byte beside it.
    const off = [];
    for (const [make, colours] of [
      [
        (/** @type {string} */ c) =>
          ImageFilter.MakeDistantLitDiffuse([0, 0, 1], c, 0, 1),
        ["rgb(201 17 99)", "rgb(59 127 207)", "rgb(255 33 7)"],
      ],
      [
        (/** @type {string} */ c) =>
          ImageFilter.MakeDistantLitSpecular([0, 0, 1], c, 0, 1, 1),
        ["rgb(255 33 7)", "rgb(3 255 171)", "rgb(201 17 255)"],
      ],
    ])
      for (const colour of /** @type {string[]} */ (colours)) {
        const want = at((ctx) => {
          ctx.fillStyle = colour;
          ctx.fillRect(0, 0, SIDE, SIDE);
        });
        const got = at((ctx) => {
          ctx.imageFilter = make(colour);
          white(ctx);
        });
        if (got.some((channel, i) => channel !== want[i]))
          off.push(`${make}: ${colour} lit ${got}, filled ${want}`);
      }
    assert.deepEqual(off, []);
  });
});
