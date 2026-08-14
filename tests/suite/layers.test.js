// @ts-check

"use strict";

const { assert, describe, test, beforeEach } = require("../runner"),
  {
    Canvas,
    ColorFilter,
    ColorMatrix,
    ImageFilter,
    MaskFilter,
    Shader,
  } = require("../../lib");

const WIDTH = 40,
  HEIGHT = 40;

// Compositing a layer lands one 8-bit step darker than the equivalent globalAlpha
// fill (126 vs 127 for 50% black on white) — Skia rasterizes the layer to 8 bits and
// blends that, where a direct fill blends the float colour. Exact at alpha 0 and 1.
// Grey levels are therefore compared with a tolerance of one step.
function assertGrey(actual, expected, message) {
  let [r, g, b, a] = actual;
  assert.ok(
    Math.abs(r - expected) <= 1 && r === g && g === b && a === 255,
    `${message}: expected ~[${expected} x3, 255], got [${actual}]`,
  );
}

describe("saveLayer", () => {
  let canvas, ctx, pixel;

  beforeEach(() => {
    canvas = new Canvas(WIDTH, HEIGHT);
    canvas.gpu = false;
    ctx = canvas.getContext("2d");
    // Opaque backdrop: a layer's alpha is only observable once something is behind it.
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, WIDTH, HEIGHT);
    ctx.fillStyle = "black";
    pixel = (x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data);
  });

  test("composites the layer at its alpha", () => {
    ctx.saveLayer(0.5);
    ctx.fillRect(10, 10, 20, 20);
    ctx.restore();
    assertGrey(pixel(20, 20), 127, "layer at 50%");
  });

  test("survives a transform applied inside it", () => {
    // The layer frame lives on the recording canvas's save stack, and the recorder
    // rebuilds that stack whenever the matrix changes. If the rebuild tears the layer
    // down, the fill lands on the base canvas at full alpha instead.
    ctx.saveLayer(0.5);
    ctx.translate(5, 5);
    ctx.fillRect(5, 5, 20, 20);
    ctx.restore();
    assertGrey(pixel(20, 20), 127, "layer survived the transform");
  });

  test("survives a clip applied inside it", () => {
    ctx.saveLayer(0.5);
    ctx.beginPath();
    ctx.rect(0, 0, WIDTH, HEIGHT);
    ctx.clip();
    ctx.fillRect(10, 10, 20, 20);
    ctx.restore();
    assertGrey(pixel(20, 20), 127, "layer survived the clip");
  });

  test("groups its contents, rather than compositing them one at a time", () => {
    // Two overlapping opaque rects in one 50% layer composite as a single shape:
    // the overlap is 50% grey, not 75%. That is the whole point of a layer.
    ctx.saveLayer(0.5);
    ctx.fillRect(5, 5, 20, 20);
    ctx.fillRect(15, 15, 20, 20);
    ctx.restore();
    let overlap = pixel(20, 20)[0],
      single = pixel(10, 10)[0];
    // Ungrouped, the overlap would darken to ~64 (two 50% blacks). Grouped, it matches
    // the singly-covered area. Compared with a tolerance because the two areas travel
    // slightly different rounding paths.
    assert.ok(
      Math.abs(overlap - single) <= 1,
      `overlap ${overlap} should match single coverage ${single}`,
    );
    assert.ok(Math.abs(single - 127) <= 1, `expected ~127, got ${single}`);
  });

  test("nests", () => {
    ctx.saveLayer(0.5);
    ctx.saveLayer(0.5);
    ctx.fillRect(10, 10, 20, 20);
    ctx.restore();
    ctx.restore();
    assertGrey(pixel(20, 20), 191, "two nested 50% layers");
  });

  test("restores the transform like save() does", () => {
    ctx.saveLayer(1);
    ctx.translate(10, 10);
    ctx.restore();
    ctx.fillRect(0, 0, 5, 5);
    assert.deepEqual(
      pixel(2, 2),
      [0, 0, 0, 255],
      "translate should not have leaked",
    );
  });

  test("rejects a backdrop that is not an ImageFilter", () => {
    assert.throws(() => ctx.saveLayer(1, null, "blur"), TypeError);
  });
});

describe("dither", () => {
  test("round-trips and defaults to false", () => {
    let ctx = new Canvas(10, 10).getContext("2d");
    assert.equal(ctx.dither, false);
    ctx.dither = true;
    assert.equal(ctx.dither, true);
    ctx.save();
    ctx.dither = false;
    ctx.restore();
    assert.equal(ctx.dither, true, "should be part of the saved state");
  });
});

describe("Skia filters on the context", () => {
  let canvas, ctx, pixel;

  beforeEach(() => {
    canvas = new Canvas(WIDTH, HEIGHT);
    canvas.gpu = false;
    ctx = canvas.getContext("2d");
    pixel = (x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data);
  });

  test("colorFilter transforms the drawn color", () => {
    // Swap red into the blue channel: a red fill must come out blue.
    // prettier-ignore
    ctx.colorFilter = ColorFilter.MakeMatrix([
      0, 0, 0, 0, 0,
      0, 0, 0, 0, 0,
      1, 0, 0, 0, 0,
      0, 0, 0, 1, 0,
    ]);
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, WIDTH, HEIGHT);
    assert.deepEqual(pixel(20, 20), [0, 0, 255, 255]);
  });

  test("colorFilter is undone by restore()", () => {
    ctx.save();
    ctx.colorFilter = ColorFilter.MakeMatrix(ColorMatrix.scaled(0, 0, 0, 1));
    ctx.restore();
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, WIDTH, HEIGHT);
    assert.deepEqual(pixel(20, 20), [255, 0, 0, 255]);
  });

  test("maskFilter blurs the edge of a fill", () => {
    ctx.fillStyle = "black";
    ctx.maskFilter = MaskFilter.MakeBlur("normal", 4);
    ctx.fillRect(10, 10, 20, 20);
    // A hard-edged rect has either 0 or 255 alpha just outside its edge; a blurred
    // one has something in between.
    let alpha = pixel(9, 20)[3];
    assert.ok(
      alpha > 0 && alpha < 255,
      `expected a soft edge, got alpha ${alpha}`,
    );
  });

  test("imageFilter applies to a fill", () => {
    ctx.fillStyle = "black";
    ctx.imageFilter = ImageFilter.MakeBlur(4, 4, "decal");
    ctx.fillRect(10, 10, 20, 20);
    let alpha = pixel(9, 20)[3];
    assert.ok(
      alpha > 0 && alpha < 255,
      `expected a soft edge, got alpha ${alpha}`,
    );
  });

  test("a shader can be assigned to fillStyle", () => {
    ctx.fillStyle = Shader.MakeFractalNoise(0.05, 0.05, 2, 0);
    ctx.fillRect(0, 0, WIDTH, HEIGHT);
    let a = pixel(5, 5),
      b = pixel(30, 30);
    assert.notDeepEqual(a, b, "noise should vary across the surface");
  });
});

describe("an image filter's crop rectangle", () => {
  // Declared on three filters and forwarded by none of them: the addon read
  // the argument and the declarations named it, but the JavaScript statics
  // passed four arguments where the addon expected five, so the slot was
  // always undefined and the rectangle was silently ignored.
  //
  // For dilate, erode and matrix convolution the crop bounds the domain the
  // kernel reads from as well as clipping the output, which is why it is an
  // argument rather than a `"crop"` filter composed afterwards: a dilation
  // given one stops spreading at the edge instead of spreading and then
  // being cut.
  const SIZE = 80;

  // A small square in the middle, dilated outward by 12 pixels.
  const spread = (crop) => {
    let canvas = new Canvas(SIZE, SIZE);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.imageFilter =
      crop === undefined
        ? new ImageFilter("dilate", 12, 12, null)
        : new ImageFilter("dilate", 12, 12, null, crop);
    ctx.fillRect(36, 36, 8, 8);
    let pixels = ctx.getImageData(0, 0, SIZE, SIZE).data;
    return (x, y) => pixels[(y * SIZE + x) * 4 + 3] > 0;
  };

  test("stops the spread at its edge", () => {
    let wide = spread(undefined);
    assert.ok(wide(40, 40), "the square itself");
    assert.ok(wide(30, 40), "dilation reaches left");
    assert.ok(wide(50, 40), "and right");

    // Cropped to a rectangle narrower than the dilation radius, the spread
    // stops there. This is the assertion that failed before the argument
    // was forwarded -- the two renders were identical.
    let cropped = spread([38, 38, 4, 4]);
    assert.ok(cropped(40, 40), "inside the crop the square survives");
    assert.ok(!cropped(30, 40), "the spread is bounded on the left");
    assert.ok(!cropped(50, 40), "and on the right");
  });

  test("erode and matrix-convolution take one too", () => {
    // Same argument, same position, on the other two filters that read it.
    assert.doesNotThrow(
      () => new ImageFilter("erode", 2, 2, null, [0, 0, 40, 40]),
    );
    assert.doesNotThrow(
      () =>
        new ImageFilter(
          "matrix-convolution",
          [1, 1],
          [1],
          1,
          0,
          [0, 0],
          "decal",
          true,
          null,
          [0, 0, 40, 40],
        ),
    );
  });

  test("refuses anything that is not a rectangle", () => {
    // Dropped silently, a malformed crop reads as "no crop" and the filter
    // quietly spreads past where it was told to stop.
    for (let wrong of [
      [0, 0, 10],
      [0, 0, 10, 10, 10],
      "0,0,10,10",
      42,
      [0, 0, 10, NaN],
    ]) {
      assert.throws(
        () => new ImageFilter("dilate", 2, 2, null, wrong),
        /four numbers/,
        JSON.stringify(wrong),
      );
    }
    // Null and undefined are how "no crop" is spelled, and both are fine.
    assert.doesNotThrow(() => new ImageFilter("dilate", 2, 2, null, null));
    assert.doesNotThrow(() => new ImageFilter("dilate", 2, 2, null));
  });
});

describe("an image filter that produces nothing", () => {
  test("leaves the page alone rather than erasing it", () => {
    // A fill that covers the page opaquely takes a fast path that resets the
    // recorder, discarding what was under it. That is sound when the fill
    // really lands -- but a filter decides what a draw finally puts down, and
    // MakeEmpty puts down nothing. The page was erased and nothing drawn in
    // its place, while the same fill a pixel smaller left it untouched.
    let canvas = new Canvas(40, 40);
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "black";
    ctx.fillRect(0, 0, 40, 40);

    ctx.imageFilter = new ImageFilter("empty");
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 40, 40);

    assert.deepEqual(
      [...ctx.getImageData(0, 0, 1, 1).data],
      [0, 0, 0, 255],
      "the black page survives a full-canvas fill through MakeEmpty",
    );
  });

  test("does not disable the fast path for an ordinary fill", () => {
    let canvas = new Canvas(40, 40);
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "black";
    ctx.fillRect(0, 0, 40, 40);
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 40, 40);

    assert.deepEqual(
      [...ctx.getImageData(0, 0, 1, 1).data],
      [255, 0, 0, 255],
      "an unfiltered opaque fill still covers the page",
    );
  });
});
