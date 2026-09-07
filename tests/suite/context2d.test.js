// @ts-check

"use strict";

const { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  {
    Canvas,
    DOMMatrix,
    DOMPoint,
    ImageData,
    ImageFilter,
    Path2D,
    FontLibrary,
    loadImage,
  } = require("../../lib"),
  css = require("../../lib/classes/css"),
  fs = require("fs"),
  { loadSkiaNode } = require("../../lib/binary.js");

const native = loadSkiaNode();

const BLACK = [0, 0, 0, 255],
  WHITE = [255, 255, 255, 255],
  GREEN = [0, 128, 0, 255],
  CLEAR = [0, 0, 0, 0];

const _each = (obj, fn) =>
  Object.entries(obj).forEach(([term, val]) => fn(val, term));

describe("Context2D", () => {
  let canvas,
    ctx,
    WIDTH = 512,
    HEIGHT = 512,
    pixel = (x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data),
    loadAsset = (url) => loadImage(`tests/assets/images/${url}`),
    mockedWarn = () => {},
    realWarn = console.warn;

  beforeEach(() => {
    canvas = new Canvas(WIDTH, HEIGHT);
    ctx = canvas.getContext("2d");
    console.warn = mockedWarn;
  });

  afterEach(() => {
    console.warn = realWarn;
  });

  describe("can get & set", () => {
    test("currentTransform", () => {
      ctx.scale(0.1, 0.3);
      let matrix = ctx.currentTransform;
      _each({ a: 0.1, b: 0, c: 0, d: 0.3, e: 0, f: 0 }, (val, term) =>
        assert.nearEqual(matrix[term], val),
      );

      ctx.resetTransform();
      _each({ a: 1, d: 1 }, (val, term) =>
        assert.nearEqual(ctx.currentTransform[term], val),
      );

      ctx.currentTransform = matrix;
      _each({ a: 0.1, d: 0.3 }, (val, term) =>
        assert.nearEqual(ctx.currentTransform[term], val),
      );
    });

    test("font", () => {
      assert.equal(ctx.font, "10px sans-serif");
      let font = "16px Baskerville, serif",
        serialized = css.font(font).serialized;
      ctx.font = font;
      assert.equal(ctx.font, serialized);
      ctx.font = "invalid";
      assert.equal(ctx.font, serialized);
    });

    test("globalAlpha", () => {
      assert.equal(ctx.globalAlpha, 1);
      ctx.globalAlpha = 0.25;
      assert.nearEqual(ctx.globalAlpha, 0.25);
      ctx.globalAlpha = -1;
      assert.nearEqual(ctx.globalAlpha, 0.25);
      ctx.globalAlpha = 3;
      assert.nearEqual(ctx.globalAlpha, 0.25);
      ctx.globalAlpha = 0;
      assert.equal(ctx.globalAlpha, 0);

      // Exactly, not nearly. The attribute is a double in the IDL, and the
      // state stored an f32, so a value with no f32 spelling came back
      // changed: 0.37 read as 0.3700000047683716.
      for (let alpha of [0.37, 0.1, 0.2 + 0.1, 1 / 3]) {
        ctx.globalAlpha = alpha;
        assert.equal(ctx.globalAlpha, alpha);
      }
    });

    test("globalCompositeOperation", () => {
      // The standard's twenty-six. A caller using only these moves to a
      // browser canvas unchanged, which is what makes this half a
      // conformance check rather than a list of what happens to work.
      let standard = [
        "source-over",
        "destination-over",
        "copy",
        "source-in",
        "destination-in",
        "source-out",
        "destination-out",
        "source-atop",
        "destination-atop",
        "xor",
        "lighter",
        "multiply",
        "screen",
        "overlay",
        "darken",
        "lighten",
        "color-dodge",
        "color-burn",
        "hard-light",
        "soft-light",
        "difference",
        "exclusion",
        "hue",
        "saturation",
        "color",
        "luminosity",
      ];

      // The three this build adds, declared as `CompositeExtension`. Kept
      // separate here for the same reason the type separates them: a reader
      // should be able to tell which half a name belongs to without going to
      // look. What each one does is pinned by the test below.
      let extensions = ["clear", "destination", "modulate"];

      let ops = [...standard, ...extensions];

      assert.equal(standard.length, 26, "the standard's operator count");
      assert.equal(extensions.length, 3);

      assert.equal(ctx.globalCompositeOperation, "source-over");
      ctx.globalCompositeOperation = "invalid";
      assert.equal(ctx.globalCompositeOperation, "source-over");

      for (let op of ops) {
        ctx.globalCompositeOperation = op;
        assert.equal(ctx.globalCompositeOperation, op);
      }

      // The CSS compositing names are not canvas ones. Chrome refuses both
      // of these exactly as it refuses a typo, leaving the mode alone, and
      // the additive operator is reachable as `lighter` -- which is in the
      // list above and renders additively.
      for (let op of ["plus-lighter", "plus-darker"]) {
        ctx.globalCompositeOperation = "multiply";
        ctx.globalCompositeOperation = op;
        assert.equal(ctx.globalCompositeOperation, "multiply");
      }
    });

    test("the composite extensions do what nothing standard does", () => {
      // Each of the three earns its place by being unreachable through the
      // standard's twenty-six, and what proves that is which channel moves
      // and how it moves with the source's alpha. No colour channel is
      // asserted: they differ by a step between Core Text and FreeType builds
      // and say nothing about any of these claims.
      //
      // Every expectation below is one of three kinds, and none of them is a
      // number this machine produced:
      //
      //   exact      only where the operator's own definition pins the value.
      //              `clear` yields transparent black, and zero cannot round
      //              two ways.
      //   within one where the expectation comes from arithmetic of ours
      //              rather than from the rasterizer -- `204 x 0.1` is 20.4
      //              and nothing here decides which way that goes.
      //   separated  where the claim is that two operators differ, asserted
      //              with a margin far wider than a rounding step.
      //
      // Nothing compares two *different* blend pipelines for exact equality.
      // That was the last failure: `multiply` and `source-over` agree on
      // alpha here and differ by one on a FreeType build, because agreeing on
      // the formula does not mean agreeing on the rounding of an
      // intermediate. Two operators can only be compared loosely.
      const ALPHA = 3;
      const over = (op, alpha = 0.5) => {
        const ctx = new Canvas(4, 4).getContext("2d");
        ctx.fillStyle = "rgba(255,128,0,0.8)";
        ctx.fillRect(0, 0, 4, 4);
        if (op) {
          ctx.globalCompositeOperation = op;
          ctx.fillStyle = `rgba(0,128,255,${alpha})`;
          ctx.fillRect(0, 0, 4, 4);
        }
        return Array.from(ctx.getImageData(2, 2, 1, 1).data);
      };
      const destinationAlpha = over(null)[ALPHA];
      // One and a half steps, and the halves are separate: up to 0.5 because
      // `want` is unrounded and nothing here decides which way the rasterizer
      // takes a fraction -- `204 x 0.1` is 20.4 -- and a further 1 because two
      // builds can land a step apart. Centring on `Math.round(want)` instead
      // would assert our rounding rule rather than tolerate not knowing it.
      // Still far below the 66 that separates the operators being told apart.
      const near = (got, want, why) =>
        assert.ok(
          Math.abs(got - want) <= 1.5,
          `${why}: expected about ${want}, got ${got}`,
        );

      for (const alpha of [0.25, 0.5, 0.9]) {
        // `clear` wipes whatever the source's alpha is; `destination-out`
        // erases in proportion to it. Three source alphas, because one cannot
        // separate "ignores it" from "reaches zero at this one".
        //
        // Asserted exactly, and deliberately the one that is: transparent
        // black is zero, and zero cannot round two ways, so a build reporting
        // 1 here has a defect rather than a difference. Everything else in
        // this test is loosened, which would make an exact assertion look
        // like an oversight -- it is the opposite. Loosening this one would
        // cost the only place a real regression could still show.
        assert.equal(
          over("clear", alpha)[ALPHA],
          0,
          "`clear` ignores the source's alpha",
        );
        near(
          over("destination-out", alpha)[ALPHA],
          destinationAlpha * (1 - alpha),
          "`destination-out` erases in proportion to it",
        );

        // `modulate` multiplies alpha by the source's. `multiply` does not --
        // it leaves alpha where ordinary compositing puts it, which is where
        // `source-over` puts it, to within the rounding of an intermediate.
        // The separation from `modulate` is what carries the claim: the two
        // are 66 apart at their closest here, against a step of one.
        near(
          over("modulate", alpha)[ALPHA],
          destinationAlpha * alpha,
          "`modulate` multiplies alpha",
        );
        near(
          over("multiply", alpha)[ALPHA],
          over("source-over", alpha)[ALPHA],
          "`multiply` composites alpha the ordinary way",
        );
        assert.ok(
          over("multiply", alpha)[ALPHA] - over("modulate", alpha)[ALPHA] > 10,
          "`multiply` is nowhere near multiplying alpha",
        );
      }

      // `destination` ignores the source entirely, so the pixel it covers
      // keeps the destination's alpha. Compared to that alpha rather than to
      // the whole no-draw pixel, since those are two pipelines and only the
      // claim about alpha is being made.
      near(
        over("destination")[ALPHA],
        destinationAlpha,
        "`destination` keeps the destination",
      );
      assert.notDeepEqual(
        over("copy"),
        over(null),
        "`copy` is the mirror -- it keeps the source instead",
      );
    });

    test("imageSmoothingEnabled", () => {
      assert.equal(ctx.imageSmoothingEnabled, true);
      ctx.imageSmoothingEnabled = false;
      assert.equal(ctx.imageSmoothingEnabled, false);
    });

    test("imageSmoothingQuality", () => {
      let vals = ["low", "medium", "high"];

      assert.equal(ctx.imageSmoothingQuality, "low");
      ctx.imageSmoothingQuality = "invalid";
      assert.equal(ctx.imageSmoothingQuality, "low");

      for (let val of vals) {
        ctx.imageSmoothingQuality = val;
        assert.equal(ctx.imageSmoothingQuality, val);
      }
    });

    test("lineCap", () => {
      let vals = ["butt", "square", "round"];

      assert.equal(ctx.lineCap, "butt");
      ctx.lineCap = "invalid";
      assert.equal(ctx.lineCap, "butt");

      for (let val of vals) {
        ctx.lineCap = val;
        assert.equal(ctx.lineCap, val);
      }
    });

    test("lineDash", () => {
      assert.deepEqual(ctx.getLineDash(), []);
      ctx.setLineDash([1, 2, 3, 4]);
      assert.deepEqual(ctx.getLineDash(), [1, 2, 3, 4]);
      ctx.setLineDash([NaN]);
      assert.deepEqual(ctx.getLineDash(), [1, 2, 3, 4]);
    });

    test("lineJoin", () => {
      let vals = ["miter", "round", "bevel"];

      assert.equal(ctx.lineJoin, "miter");
      ctx.lineJoin = "invalid";
      assert.equal(ctx.lineJoin, "miter");

      for (let val of vals) {
        ctx.lineJoin = val;
        assert.equal(ctx.lineJoin, val);
      }
    });

    test("lineWidth", () => {
      ctx.lineWidth = 10.0;
      assert.equal(ctx.lineWidth, 10);
      ctx.lineWidth = Infinity;
      assert.equal(ctx.lineWidth, 10);
      ctx.lineWidth = -Infinity;
      assert.equal(ctx.lineWidth, 10);
      ctx.lineWidth = -5;
      assert.equal(ctx.lineWidth, 10);
      ctx.lineWidth = 0;
      assert.equal(ctx.lineWidth, 10);
    });

    test("textAlign", () => {
      let vals = ["start", "end", "left", "center", "right", "justify"];

      assert.equal(ctx.textAlign, "start");
      ctx.textAlign = "invalid";
      assert.equal(ctx.textAlign, "start");

      for (let val of vals) {
        ctx.textAlign = val;
        assert.equal(ctx.textAlign, val);
      }
    });
  });

  describe("can create", () => {
    test("a context", () => {
      assert.strictEqual(canvas.getContext("invalid"), null);
      assert.strictEqual(canvas.getContext("2d"), ctx);
      assert.strictEqual(canvas.pages[0], ctx);
      assert.strictEqual(ctx.canvas, canvas);
    });

    test("multiple pages", () => {
      let ctx2 = canvas.newPage(WIDTH * 2, HEIGHT * 2);
      assert.equal(canvas.width, WIDTH * 2);
      assert.equal(canvas.height, HEIGHT * 2);
      assert.strictEqual(canvas.pages[0], ctx);
      assert.strictEqual(canvas.pages[1], ctx2);
      assert.strictEqual(ctx.canvas, canvas);
      assert.strictEqual(ctx2.canvas, canvas);
    });

    test("ImageData", () => {
      let [width, height] = [123, 456],
        bmp = ctx.createImageData(width, height);
      assert.equal(bmp.width, width);
      assert.equal(bmp.height, height);
      assert.equal(bmp.data.length, width * height * 4);
      assert.deepEqual(Array.from(bmp.data.slice(0, 4)), CLEAR);

      let blank = new ImageData(width, height);
      assert.equal(blank.width, width);
      assert.equal(blank.height, height);
      assert.equal(blank.data.length, width * height * 4);
      assert.deepEqual(Array.from(blank.data.slice(0, 4)), CLEAR);

      new ImageData(blank.data, width, height);
      new ImageData(blank.data, height, width);
      new ImageData(blank.data, width);
      new ImageData(blank.data, height);
      assert.throws(() => new ImageData(blank.data, width + 1, height));
      assert.throws(() => new ImageData(blank.data, width + 1));

      // @ts-ignore
      new ImageData(blank);
      // @ts-ignore
      assert.throws(() => new ImageData(blank.data));
    });

    describe("CanvasPattern", () => {
      test("from Image", async () => {
        let image = await loadAsset("checkers.png"),
          pattern = ctx.createPattern(image, "repeat"),
          [width, height] = [20, 20];

        ctx.imageSmoothingEnabled = false;
        ctx.fillStyle = pattern;
        ctx.fillRect(0, 0, width, height);

        let bmp = ctx.getImageData(0, 0, width, height);
        let blackPixel = true;
        assert.equal(bmp.data.length, width * height * 4);
        for (var i = 0; i < bmp.data.length; i += 4) {
          if (i % (bmp.width * 4) != 0) blackPixel = !blackPixel;
          assert.deepEqual(
            Array.from(bmp.data.slice(i, i + 4)),
            blackPixel ? BLACK : WHITE,
          );
        }
      });

      test("from ImageData", () => {
        let blank = new Canvas();
        ctx.fillStyle = ctx.createPattern(blank, "repeat");
        ctx.fillRect(0, 0, 20, 20);

        let checkers = new Canvas(2, 2),
          patCtx = checkers.getContext("2d");
        patCtx.fillStyle = "white";
        patCtx.fillRect(0, 0, 2, 2);
        patCtx.fillStyle = "black";
        patCtx.fillRect(0, 0, 1, 1);
        patCtx.fillRect(1, 1, 1, 1);

        let checkersData = patCtx.getImageData(0, 0, 2, 2);

        let pattern = ctx.createPattern(checkersData, "repeat");
        ctx.fillStyle = pattern;
        ctx.fillRect(0, 0, 20, 20);

        let bmp = ctx.getImageData(0, 0, 20, 20);
        let blackPixel = true;
        for (var i = 0; i < bmp.data.length; i += 4) {
          if (i % (bmp.width * 4) != 0) blackPixel = !blackPixel;
          assert.deepEqual(
            Array.from(bmp.data.slice(i, i + 4)),
            blackPixel ? BLACK : WHITE,
          );
        }
      });

      test("from Canvas", () => {
        let blank = new Canvas();
        ctx.fillStyle = ctx.createPattern(blank, "repeat");
        ctx.fillRect(0, 0, 20, 20);

        let checkers = new Canvas(2, 2),
          patCtx = checkers.getContext("2d");
        patCtx.fillStyle = "white";
        patCtx.fillRect(0, 0, 2, 2);
        patCtx.fillStyle = "black";
        patCtx.fillRect(0, 0, 1, 1);
        patCtx.fillRect(1, 1, 1, 1);

        let pattern = ctx.createPattern(checkers, "repeat");
        ctx.fillStyle = pattern;
        ctx.fillRect(0, 0, 20, 20);

        let bmp = ctx.getImageData(0, 0, 20, 20);
        let blackPixel = true;
        for (var i = 0; i < bmp.data.length; i += 4) {
          if (i % (bmp.width * 4) != 0) blackPixel = !blackPixel;
          assert.deepEqual(
            Array.from(bmp.data.slice(i, i + 4)),
            blackPixel ? BLACK : WHITE,
          );
        }
      });

      test("with local transform", () => {
        // call func with an ImageData-offset and pixel color value appropriate for a 4-quadrant pattern within
        // the width and height that's white in the upper-left & lower-right and black in the other corners
        function eachPixel(bmp, func) {
          let { width, height } = bmp;
          for (let x = 0; x < width; x++) {
            for (let y = 0; y < height; y++) {
              let i = y * 4 * width + x * 4,
                clr =
                  (x < width / 2 && y < height / 2) ||
                  (x >= width / 2 && y >= height / 2)
                    ? 255
                    : 0;
              func(i, clr);
            }
          }
        }

        // create a canvas with a single repeat of the pattern within its dims
        function makeCheckerboard(w, h) {
          let check = new Canvas(w, h),
            ctx = check.getContext("2d"),
            bmp = ctx.createImageData(w, h);
          eachPixel(bmp, (i, clr) => bmp.data.set([clr, clr, clr, 255], i));
          ctx.putImageData(bmp, 0, 0);
          return check;
        }

        // verify that the region looks like a single 4-quadrant checkerboard cell
        function isCheckerboard(ctx, w, h) {
          let bmp = ctx.getImageData(0, 0, w, h);
          eachPixel(bmp, (i, clr) => {
            let px = Array.from(bmp.data.slice(i, i + 4));
            assert.deepEqual(px, [clr, clr, clr, 255]);
          });
        }

        let w = 160,
          h = 160,
          pat = ctx.createPattern(makeCheckerboard(w, h), "repeat"),
          mat = new DOMMatrix();

        ctx.fillStyle = pat;

        // draw a single repeat of the pattern at each scale and then confirm that
        // the transformation succeeded
        [1, 0.5, 0.25, 0.125, 0.0625].forEach((mag) => {
          mat = new DOMMatrix().scale(mag);
          pat.setTransform(mat);
          // make sure the alternative matrix syntaxes also work
          assert.doesNotThrow(() => {
            pat.setTransform(mag, 0, 0, mag, 0, 0);
          });
          assert.doesNotThrow(() => {
            pat.setTransform([mag, 0, 0, mag, 0, 0]);
          });
          assert.doesNotThrow(() => {
            pat.setTransform({ a: mag, b: 0, c: 0, d: mag, e: 0, f: 0 });
          });
          ctx.fillRect(0, 0, w * mag, h * mag);
          isCheckerboard(ctx, w * mag, h * mag);
        });
      });
    });

    describe("CanvasGradient", () => {
      test("linear", () => {
        let gradient = ctx.createLinearGradient(1, 1, 19, 1);
        ctx.fillStyle = gradient;
        gradient.addColorStop(0, "#fff");
        gradient.addColorStop(1, "#000");
        ctx.fillRect(0, 0, 21, 1);

        assert.deepEqual(pixel(0, 0), WHITE);
        assert.deepEqual(pixel(20, 0), BLACK);
      });

      test("a degenerate gradient paints nothing", () => {
        // Verbatim, for linear: "If x0 = x1 and y0 = y1, then the linear
        // gradient must paint nothing." For radial: "If x0 = x1 and y0 = y1
        // and r0 = r1, then the radial gradient must paint nothing."
        //
        // Painting nothing is a transparent shader, not the absence of one:
        // clearing the shader leaves the paint's own colour, which is opaque
        // black, and that is what a gradient with no stops used to paint
        // over the fill area.
        const degenerate = [
          [
            "linear, both ends at one point",
            (c) => c.createLinearGradient(8, 8, 8, 8),
          ],
          [
            "radial, one centre and one radius",
            (c) => c.createRadialGradient(8, 8, 4, 8, 8, 4),
          ],
          [
            "radial, both radii zero",
            (c) => c.createRadialGradient(8, 8, 0, 8, 8, 0),
          ],
        ];
        for (const [what, make] of degenerate) {
          const c = new Canvas(16, 16).getContext("2d");
          const g = make(c);
          g.addColorStop(0, "red");
          g.addColorStop(1, "blue");
          c.fillStyle = g;
          c.fillRect(0, 0, 16, 16);
          assert.deepEqual(
            Array.from(c.getImageData(8, 8, 1, 1).data),
            CLEAR,
            what,
          );
        }

        // "If there are no stops, the gradient is transparent black" --
        // whatever its geometry, so this covers the conic case the two
        // degeneracy clauses above do not describe.
        for (const [what, make] of [
          ["linear", (c) => c.createLinearGradient(0, 0, 16, 16)],
          ["radial", (c) => c.createRadialGradient(0, 0, 0, 8, 8, 8)],
          ["conic", (c) => c.createConicGradient(0, 8, 8)],
        ]) {
          const c = new Canvas(16, 16).getContext("2d");
          c.fillStyle = make(c);
          c.fillRect(0, 0, 16, 16);
          assert.deepEqual(
            Array.from(c.getImageData(8, 8, 1, 1).data),
            CLEAR,
            `${what} with no stops`,
          );
        }

        // The control. A gradient that is not degenerate still paints, so a
        // fix that simply stopped painting gradients would fail here.
        const c = new Canvas(16, 16).getContext("2d");
        const g = c.createLinearGradient(0, 0, 16, 0);
        g.addColorStop(0, "red");
        g.addColorStop(1, "blue");
        c.fillStyle = g;
        c.fillRect(0, 0, 16, 16);
        const mid = Array.from(c.getImageData(8, 8, 1, 1).data);
        assert.notDeepEqual(mid, CLEAR, "an ordinary gradient still paints");
        assert.equal(mid[3], 255, "and paints it opaque");
      });

      test("radial", () => {
        let [x, y, inside, outside] = [100, 100, 45, 55],
          inner = [x, y, 25],
          outer = [x, y, 50],
          gradient = ctx.createRadialGradient(...inner, ...outer);
        ctx.fillStyle = gradient;
        gradient.addColorStop(0, "#fff");
        gradient.addColorStop(0.5, "#000");
        gradient.addColorStop(1, "#000");
        gradient.addColorStop(1, "red");
        ctx.fillRect(0, 0, 200, 200);

        assert.deepEqual(pixel(x, y), WHITE);
        assert.deepEqual(pixel(x + inside, y), BLACK);
        assert.deepEqual(pixel(x, y + inside), BLACK);
        assert.deepEqual(pixel(x + outside, y), [255, 0, 0, 255]);
        assert.deepEqual(pixel(x, y + outside), [255, 0, 0, 255]);
      });

      test("conic", () => {
        // draw a sweep with white at top and black on bottom
        let gradient = ctx.createConicGradient(0, 256, 256);
        ctx.fillStyle = gradient;
        gradient.addColorStop(0, "#fff");
        gradient.addColorStop(0.5, "#000");
        gradient.addColorStop(1, "#fff");
        ctx.fillRect(0, 0, 512, 512);

        assert.deepEqual(pixel(5, 256), BLACK);
        assert.deepEqual(pixel(500, 256), WHITE);

        // rotate 90° so black is left and white is right
        gradient = ctx.createConicGradient(Math.PI / 2, 256, 256);
        ctx.fillStyle = gradient;
        gradient.addColorStop(0, "#fff");
        gradient.addColorStop(0.5, "#000");
        gradient.addColorStop(1, "#fff");
        ctx.fillRect(0, 0, 512, 512);

        assert.deepEqual(pixel(256, 500), WHITE);
        assert.deepEqual(pixel(256, 5), BLACK);
      });
    });

    describe("CanvasTexture", () => {
      var waves, nylon, lines;

      beforeEach(() => {
        let w = 40;
        let wavePath = new Path2D();
        wavePath.moveTo(-w / 2, w / 2);
        wavePath.bezierCurveTo(
          (-w * 3) / 8,
          (w * 3) / 4,
          -w / 8,
          (w * 3) / 4,
          0,
          w / 2,
        );
        wavePath.bezierCurveTo(w / 8, w / 4, (w * 3) / 8, w / 4, w / 2, w / 2);
        wavePath.bezierCurveTo(
          (w * 5) / 8,
          (w * 3) / 4,
          (w * 7) / 8,
          (w * 3) / 4,
          w,
          w / 2,
        );
        wavePath.bezierCurveTo(
          (w * 9) / 8,
          w / 4,
          (w * 11) / 8,
          w / 4,
          (w * 3) / 2,
          w / 2,
        );
        waves = ctx.createTexture([w, w / 2], {
          path: wavePath,
          color: "black",
          line: 3,
          angle: Math.PI / 7,
        });

        let n = 50;
        let nylonPath = new Path2D();
        nylonPath.moveTo(0, n / 4);
        nylonPath.lineTo(n / 4, n / 4);
        nylonPath.lineTo(n / 4, 0);
        nylonPath.moveTo((n * 3) / 4, n);
        nylonPath.lineTo((n * 3) / 4, (n * 3) / 4);
        nylonPath.lineTo(n, (n * 3) / 4);
        nylonPath.moveTo(n / 4, n / 2);
        nylonPath.lineTo(n / 4, (n * 3) / 4);
        nylonPath.lineTo(n / 2, (n * 3) / 4);
        nylonPath.moveTo(n / 2, n / 4);
        nylonPath.lineTo((n * 3) / 4, n / 4);
        nylonPath.lineTo((n * 3) / 4, n / 2);
        nylon = ctx.createTexture(n, {
          path: nylonPath,
          color: "black",
          line: 5,
          cap: "square",
          angle: Math.PI / 8,
        });

        lines = ctx.createTexture(8, { line: 4, color: "black" });
      });

      test("with filled Path2D", async () => {
        ctx.fillStyle = nylon;
        ctx.fillRect(10, 10, 80, 80);

        assert.deepEqual(pixel(26, 24), CLEAR);
        assert.deepEqual(pixel(28, 26), BLACK);
        assert.deepEqual(pixel(48, 48), BLACK);
        assert.deepEqual(pixel(55, 40), CLEAR);
      });

      test("with stroked Path2D", async () => {
        ctx.strokeStyle = waves;
        ctx.lineWidth = 10;
        ctx.moveTo(0, 0);
        ctx.lineTo(100, 100);
        ctx.stroke();

        assert.deepEqual(pixel(10, 10), CLEAR);
        assert.deepEqual(pixel(16, 16), BLACK);
        assert.deepEqual(pixel(73, 73), BLACK);
        assert.deepEqual(pixel(75, 75), CLEAR);
      });

      test("with lines", async () => {
        ctx.fillStyle = lines;
        ctx.fillRect(10, 10, 80, 80);

        assert.deepEqual(pixel(22, 22), CLEAR);
        assert.deepEqual(pixel(25, 25), BLACK);
        assert.deepEqual(pixel(73, 73), CLEAR);
        assert.deepEqual(pixel(76, 76), BLACK);
      });
    });
  });

  describe("supports", () => {
    test("filter", () => {
      // results differ b/t cpu & gpu renderers so make sure test doesn't fail if gpu support isn't present
      let { gpu } = canvas;
      canvas.gpu = false;
      // make sure chains of filters compose correctly <https://codepen.io/sosuke/pen/Pjoqqp>
      ctx.filter =
        "blur(5px) invert(56%) sepia(63%) saturate(4837%) hue-rotate(163deg) brightness(96%) contrast(101%)";
      ctx.fillRect(0, 0, 20, 20);
      assert.deepEqual(pixel(10, 10), [0, 162, 213, 245]);
      canvas.gpu = gpu;
    });

    test("shadow", async () => {
      const sin = Math.sin(1.15 * Math.PI);
      const cos = Math.cos(1.15 * Math.PI);
      ctx.translate(150, 150);
      ctx.transform(cos, sin, -sin, cos, 0, 0);

      ctx.shadowColor = "#000";
      ctx.shadowBlur = 5;
      ctx.shadowOffsetX = 10;
      ctx.shadowOffsetY = 10;
      ctx.fillStyle = "#eee";
      ctx.fillRect(25, 25, 65, 10);

      // ensure that the shadow is actually fuzzy despite the transforms
      assert.notEqual(pixel(143, 117), BLACK);
    });

    test("clip()", () => {
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, 2, 2);

      // overlapping rectangles to use as a clipping mask
      ctx.rect(0, 0, 2, 1);
      ctx.rect(1, 0, 1, 2);

      // b | w
      // -----
      // w | b
      ctx.save();
      ctx.clip("evenodd");
      ctx.fillStyle = "black";
      ctx.fillRect(0, 0, 2, 2);
      ctx.restore();

      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(1, 0), WHITE);
      assert.deepEqual(pixel(0, 1), WHITE);
      assert.deepEqual(pixel(1, 1), BLACK);

      // b | b
      // -----
      // w | b
      ctx.save();
      ctx.clip(); // nonzero
      ctx.fillStyle = "black";
      ctx.fillRect(0, 0, 2, 2);
      ctx.restore();

      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(1, 0), BLACK);
      assert.deepEqual(pixel(0, 1), WHITE);
      assert.deepEqual(pixel(1, 1), BLACK);

      // test intersection of sequential clips while incorporating transform
      ctx.fillStyle = "black";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);

      ctx.save();
      ctx.beginPath();
      ctx.rect(20, 20, 60, 60);
      ctx.clip();
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);

      ctx.beginPath();
      ctx.translate(20, 20);
      ctx.rect(0, 0, 30, 30);
      ctx.clip();
      ctx.fillStyle = "green";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);
      ctx.restore();

      assert.deepEqual(pixel(10, 10), BLACK);
      assert.deepEqual(pixel(90, 90), BLACK);
      assert.deepEqual(pixel(22, 22), GREEN);
      assert.deepEqual(pixel(48, 48), GREEN);
      assert.deepEqual(pixel(52, 52), WHITE);

      // non-overlapping clips & empty clips should prevent drawing altogether
      ctx.beginPath();
      ctx.rect(20, 20, 30, 30);
      ctx.clip();
      ctx.fillStyle = "black";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);

      ctx.save();
      ctx.beginPath();
      ctx.rect(25, 25, 0, 0);
      ctx.clip();
      ctx.fillStyle = "green";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);
      ctx.restore();

      ctx.save();
      ctx.beginPath();
      ctx.rect(0, 0, 10, 10);
      ctx.clip();
      ctx.fillStyle = "green";
      ctx.fillRect(0, 0, WIDTH, HEIGHT);
      ctx.restore();

      assert.deepEqual(pixel(30, 30), BLACK);
    });

    test("fill()", () => {
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, 2, 2);

      // set the current path to a pair of overlapping rects
      ctx.fillStyle = "black";
      ctx.rect(0, 0, 2, 1);
      ctx.rect(1, 0, 1, 2);

      // b | w
      // -----
      // w | b
      ctx.fill("evenodd");
      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(1, 0), WHITE);
      assert.deepEqual(pixel(0, 1), WHITE);
      assert.deepEqual(pixel(1, 1), BLACK);

      // b | b
      // -----
      // w | b
      ctx.fill(); // nonzero
      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(1, 0), BLACK);
      assert.deepEqual(pixel(0, 1), WHITE);
      assert.deepEqual(pixel(1, 1), BLACK);
    });

    test("fillText()", () => {
      /** @type {[args: any[], shouldDraw: boolean][]} */
      let argsets = [
        [["A", 10, 10], true],
        [["A", 10, 10, undefined], true],
        [["A", 10, 10, NaN], false],
        [["A", 10, 10, Infinity], false],
        [[1234, 10, 10], true],
        [[false, 10, 10], true],
        [[{}, 10, 10], true],
      ];

      _each(argsets, ([args, shouldDraw]) => {
        canvas.width = WIDTH;
        ctx.textBaseline = "middle";
        ctx.textAlign = "center";
        ctx.fillText(...args);
        assert.equal(
          ctx.getImageData(0, 0, 20, 20).data.some((a) => a),
          shouldDraw,
        );
      });
    });

    test("roundRect()", () => {
      let dim = WIDTH / 2;
      let radii = [50, 25, { x: 15, y: 15 }, new DOMPoint(20, 10)];
      ctx.beginPath();
      ctx.roundRect(dim, dim, dim, dim, radii);
      ctx.roundRect(dim, dim, -dim, -dim, radii);
      ctx.roundRect(dim, dim, -dim, dim, radii);
      ctx.roundRect(dim, dim, dim, -dim, radii);
      ctx.fill();

      let off = [
        [3, 3],
        [dim - 14, dim - 14],
        [dim - 4, 3],
        [7, dim - 6],
      ];
      let on = [
        [5, 5],
        [dim - 17, dim - 17],
        [dim - 9, 3],
        [9, dim - 9],
      ];

      for (const [x, y] of on) {
        assert.deepEqual(pixel(x, y), BLACK);
        assert.deepEqual(pixel(x, HEIGHT - y - 1), BLACK);
        assert.deepEqual(pixel(WIDTH - x - 1, y), BLACK);
        assert.deepEqual(pixel(WIDTH - x - 1, HEIGHT - y - 1), BLACK);
      }

      for (const [x, y] of off) {
        assert.deepEqual(pixel(x, y), CLEAR);
        assert.deepEqual(pixel(x, HEIGHT - y - 1), CLEAR);
        assert.deepEqual(pixel(WIDTH - x - 1, y), CLEAR);
        assert.deepEqual(pixel(WIDTH - x - 1, HEIGHT - y - 1), CLEAR);
      }
    });

    test("roundRect ignores a non-finite argument", () => {
      // The context's half of the same divergence the Path2D suite pins.
      // Nothing may be painted, and nothing may be thrown: both entry
      // points now read their arguments through the strict-only helper the
      // other eight path methods use.
      for (const bad of [NaN, Infinity, -Infinity]) {
        ctx.beginPath();
        assert.doesNotThrow(() => ctx.roundRect(bad, 10, 20, 20, 5));
        assert.doesNotThrow(() => ctx.roundRect(10, bad, 20, 20, 5));
        assert.doesNotThrow(() => ctx.roundRect(10, 10, 20, 20, bad));
        ctx.fill();
      }

      assert.deepEqual(
        pixel(15, 15),
        CLEAR,
        "a non-finite roundRect paints nothing",
      );
    });

    test("getImageData()", () => {
      ctx.fillStyle = "rgba(255,0,0, 0.25)";
      ctx.fillRect(0, 0, 1, 6);

      ctx.fillStyle = "rgba(0,255,0, 0.5)";
      ctx.fillRect(1, 0, 1, 6);

      ctx.fillStyle = "rgba(0,0,255, 0.75)";
      ctx.fillRect(2, 0, 1, 6);

      let [width, height] = [3, 6],
        bmp1 = ctx.getImageData(0, 0, width, height),
        bmp2 = ctx.getImageData(width, height, -width, -height); // negative dimensions shift origin
      for (const bmp of [bmp1, bmp2]) {
        assert.equal(bmp.width, width);
        assert.equal(bmp.height, height);
        assert.equal(bmp.data.length, width * height * 4);
        assert.deepEqual(Array.from(bmp.data.slice(0, 4)), [255, 0, 0, 64]);
        assert.deepEqual(Array.from(bmp.data.slice(4, 8)), [0, 255, 0, 128]);
        assert.deepEqual(Array.from(bmp.data.slice(8, 12)), [0, 0, 255, 191]);
        for (var x = 0; x < width; x++) {
          for (var y = 0; y < height; y++) {
            let i = 4 * (y * width + x);
            let px = Array.from(bmp.data.slice(i, i + 4));
            assert.deepEqual(pixel(x, y), px);
          }
        }
      }
    });

    test("putImageData()", () => {
      assert.throws(() => ctx.putImageData({}, 0, 0));
      assert.throws(() => ctx.putImageData(undefined, 0, 0));

      var srcImageData = ctx.createImageData(2, 2);
      srcImageData.data.set(
        [1, 2, 3, 255, 5, 6, 7, 255, 0, 1, 2, 255, 4, 5, 6, 255],
        0,
      );

      ctx.putImageData(srcImageData, -1, -1);
      var resImageData = ctx.getImageData(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(resImageData.data),
        [4, 5, 6, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
      );

      // try mask rect
      ctx.reset();
      ctx.putImageData(srcImageData, 0, 0, 1, 1, 1, 1);
      resImageData = ctx.getImageData(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(resImageData.data),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 5, 6, 255],
      );

      // try negative dimensions
      ctx.reset();
      ctx.putImageData(srcImageData, 0, 0, 1, 1, -1, -1);
      resImageData = ctx.getImageData(0, 0, 2, 2);
      assert.deepEqual(
        Array.from(resImageData.data),
        [1, 2, 3, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
      );
    });

    test("isPointInPath()", () => {
      let inStroke = [100, 94],
        inFill = [150, 150],
        inBoth = [100, 100];

      ctx.rect(100, 100, 100, 100);
      ctx.lineWidth = 12;

      assert.equal(ctx.isPointInPath(...inStroke), false);
      assert.equal(ctx.isPointInStroke(...inStroke), true);

      assert.equal(ctx.isPointInPath(...inFill), true);
      assert.equal(ctx.isPointInStroke(...inFill), false);

      assert.equal(ctx.isPointInPath(...inBoth), true);
      assert.equal(ctx.isPointInStroke(...inBoth), true);
    });

    test("the query point is unaffected by the current transform", () => {
      // Stated twice in the standard, once per method: the coordinates are
      // "treated as coordinates in the canvas coordinate space unaffected by
      // the current transformation".
      //
      // The context's own path is accumulated in device space, so the point
      // goes in untouched. It used to be mapped through the matrix's
      // inverse, which was right only while the matrix had not changed since
      // the path was built -- and silently inverted the answer once it had.
      ctx.beginPath();
      ctx.rect(4, 4, 8, 8);
      ctx.scale(2, 2);

      assert.equal(ctx.isPointInPath(6, 6), true, "inside, as built");
      assert.equal(ctx.isPointInPath(20, 20), false, "outside, as built");

      // The discriminator between "the point is mapped" and "the path is
      // kept in user space and mapped at query time": build under a scale,
      // then reset. Only device-space storage answers true to both.
      const other = new Canvas(100, 100).getContext("2d");
      other.scale(2, 2);
      other.beginPath();
      other.rect(4, 4, 8, 8);
      other.setTransform(1, 0, 0, 1, 0, 0);
      assert.equal(other.isPointInPath(10, 10), true, "device-space storage");
      assert.equal(other.isPointInPath(20, 20), true, "device-space storage");
    });

    test("a Path2D still takes the transform, and the point still does not", () => {
      // The other half of the same rule, and the reason the fix is not
      // "stop mapping the point". A `Path2D` is in its own space and takes
      // the current transform at query time, so under `scale(2)` a rect at
      // 4..12 covers device 8..24 -- and the point, unaffected, is compared
      // against that. Mapping the point is what puts the two in one space
      // here, and it stays.
      const p = new Path2D();
      p.rect(4, 4, 8, 8);
      ctx.scale(2, 2);

      assert.equal(ctx.isPointInPath(p, 6, 6), false, "6,6 is outside 8..24");
      assert.equal(ctx.isPointInPath(p, 20, 20), true, "20,20 is inside");

      // With no transform the two overloads have to agree, which is the
      // case that hid this for so long.
      const plain = new Canvas(100, 100).getContext("2d");
      const q = new Path2D();
      q.rect(4, 4, 8, 8);
      plain.beginPath();
      plain.rect(4, 4, 8, 8);
      assert.equal(plain.isPointInPath(6, 6), plain.isPointInPath(q, 6, 6));
      assert.equal(plain.isPointInPath(20, 20), plain.isPointInPath(q, 20, 20));
    });

    test("isPointInPath(Path2D)", () => {
      let inStroke = [100, 94],
        inFill = [150, 150],
        inBoth = [100, 100];

      let path = new Path2D();
      path.rect(100, 100, 100, 100);
      ctx.lineWidth = 12;

      assert.equal(ctx.isPointInPath(path, ...inStroke), false);
      assert.equal(ctx.isPointInStroke(path, ...inStroke), true);

      assert.equal(ctx.isPointInPath(path, ...inFill), true);
      assert.equal(ctx.isPointInStroke(path, ...inFill), false);

      assert.equal(ctx.isPointInPath(path, ...inBoth), true);
      assert.equal(ctx.isPointInStroke(path, ...inBoth), true);
    });

    test("letterSpacing", () => {
      FontLibrary.use(`tests/assets/fonts/Monoton-Regular.woff`);

      let [x, y] = [40, 100];
      let size = 32;
      let text = "RR";
      ctx.font = `${size}px Monoton`;
      ctx.letterSpacing = "20px";
      ctx.fillStyle = "black";
      ctx.fillText(text, x, y);

      // there should be no initial added space indenting the beginning of the line
      assert.equal(
        ctx.getImageData(x, y - size, 10, size).data.some((a) => a),
        true,
      );

      // there should be whitespace between the first and second characters
      assert.equal(
        ctx.getImageData(x + 28, y - size, 18, size).data.some((a) => a),
        false,
      );

      // the compensation for the indent bug must not itself outdent
      assert.equal(
        ctx.getImageData(x - 20, y - size, 18, size).data.some((a) => a),
        false,
      );

      // Two glyphs at 20px spacing measure two spaces wide, not one. This
      // asserted 74 -- the width with a whole space subtracted -- under a
      // comment saying the space Skia adds at each end had been taken back
      // off. CSS adds `letter-spacing` after every character including the
      // last, so `n` characters carry `n` spaces and Chrome measures them
      // that way. The three assertions above are what say the rendering did
      // not move with it: no indent, a gap between the glyphs, no outdent.
      assert.nearEqual(ctx.measureText(text).width, 94);
      ctx.textWrap = true;
      assert.nearEqual(ctx.measureText(text).width, 94);
    });

    test("a hard break in an unwrapped string becomes a space, not a cut", () => {
      // With wrapping off the paragraph is built with a one-line limit, so
      // any character Skia breaks on discarded the rest of the string --
      // from the canvas as well as from measureText, and with nothing
      // reported. Only U+000A was replaced beforehand, so a form feed or a
      // vertical tab painted the first character alone.
      //
      // The anchor comes first: comparing the forms against a spaced
      // reference is free if every one of them truncates alike, so the
      // reference has to be shown wider than the first character on its own
      // or the loop below proves nothing.
      ctx.font = "24px Arial, DejaVu Sans";
      ctx.textWrap = false;
      let spaced = ctx.measureText("A B C D").width,
        alone = ctx.measureText("A").width;
      assert.ok(
        spaced > alone,
        `the reference must be wider than one glyph: ${spaced} against ${alone}`,
      );

      // TAB, LF, FF and CR are the ASCII whitespace the text preparation
      // algorithm names. U+000B, U+2028 and U+2029 are not, and are here
      // because the alternative to a space is discarding the string.
      for (const [name, cp] of [
        ["TAB", 0x09],
        ["LF", 0x0a],
        ["VT", 0x0b],
        ["FF", 0x0c],
        ["CR", 0x0d],
        ["LINE SEPARATOR", 0x2028],
        ["PARAGRAPH SEPARATOR", 0x2029],
      ]) {
        let text = "A" + String.fromCodePoint(cp) + "B C D",
          label = `U+${cp.toString(16).toUpperCase().padStart(4, "0")} ${name}`;
        assert.nearEqual(
          ctx.measureText(text).width,
          spaced,
          `${label} measures as a space`,
        );
      }

      // And the canvas agrees with the measurement, since the defect reached
      // both: a form feed painted 236 pixels against 1051 for the spaced
      // form, byte for byte what the first character alone paints.
      let inked = (text) => {
        ctx.clearRect(0, 0, WIDTH, HEIGHT);
        ctx.fillStyle = "black";
        ctx.fillText(text, 0, 30);
        return ctx
          .getImageData(0, 0, WIDTH, HEIGHT)
          .data.filter((_, i) => i % 4 === 3 && _ > 0).length;
      };
      assert.equal(
        inked("A" + String.fromCodePoint(0x0c) + "B C D"),
        inked("A B C D"),
        "a form feed paints what a space paints",
      );
    });

    test("measureText()", () => {
      ctx.font = "20px Arial, DejaVu Sans";

      let ø = ctx.measureText("").width,
        _ = ctx.measureText(" ").width,
        __ = ctx.measureText("  ").width,
        foo = ctx.measureText("foo").width,
        foobar = ctx.measureText("foobar").width,
        __foo = ctx.measureText("  foo").width,
        __foo__ = ctx.measureText("  foo  ").width;
      assert(ø < _);
      assert(_ < __);
      assert(foo < foobar);
      assert(__foo > foo);
      assert(__foo__ > __foo);

      // start from the default, alphabetic baseline
      let msg = "Lordran gypsum",
        metrics = ctx.measureText(msg);

      // + means up, - means down when it comes to baselines
      assert.equal(metrics.alphabeticBaseline, 0);
      assert(metrics.hangingBaseline > 0);
      assert(metrics.ideographicBaseline < 0);

      // for ascenders + means up, for descenders + means down
      assert(metrics.actualBoundingBoxAscent > 0);
      assert(metrics.actualBoundingBoxDescent > 0);
      assert(
        metrics.actualBoundingBoxAscent > metrics.actualBoundingBoxDescent,
      );

      // make sure the polarity has flipped for 'top' baseline
      ctx.textBaseline = "top";
      metrics = ctx.measureText("Lordran gypsum");
      assert(metrics.alphabeticBaseline < 0);
      assert(metrics.hangingBaseline < 0);
      assert(metrics.actualBoundingBoxAscent < 0);
      assert(metrics.actualBoundingBoxDescent > 0);

      // width calculations should be the same (modulo rounding) for any alignment
      let [lft, cnt, rgt] = ["left", "center", "right"].map((align) => {
        ctx.textAlign = align;
        return ctx.measureText(msg).width;
      });
      assert.nearEqual(lft, cnt);
      assert.nearEqual(cnt, rgt);

      // make sure string indices account for trailing whitespace and non-8-bit characters
      let text = " 石 ",
        { startIndex, endIndex } = ctx.measureText(text).lines[0];
      assert.equal(text.substring(startIndex, endIndex), text);
    });

    test("createProjection()", () => {
      let quad = [
        WIDTH * 0.33,
        HEIGHT / 2,
        WIDTH * 0.66,
        HEIGHT / 2,
        WIDTH,
        HEIGHT * 0.9,
        0,
        HEIGHT * 0.9,
      ];

      let matrix = ctx.createProjection(quad);
      ctx.setTransform(matrix);

      ctx.fillStyle = "black";
      ctx.fillRect(0, 0, WIDTH / 4, HEIGHT);
      ctx.fillStyle = "white";
      ctx.fillRect(WIDTH / 4, 0, WIDTH / 4, HEIGHT);
      ctx.fillStyle = "green";
      ctx.fillRect(WIDTH / 2, 0, WIDTH / 4, HEIGHT);
      ctx.resetTransform();

      let x = WIDTH / 2,
        y = HEIGHT / 2 + 2;
      assert.deepEqual(pixel(x, y - 5), CLEAR);
      assert.deepEqual(pixel(x + 25, y), GREEN);
      assert.deepEqual(pixel(x + 75, y), CLEAR);
      assert.deepEqual(pixel(x - 25, y), WHITE);
      assert.deepEqual(pixel(x - 75, y), BLACK);
      assert.deepEqual(pixel(x - 100, y), CLEAR);

      y = HEIGHT * 0.9 - 2;
      assert.deepEqual(pixel(x + 100, y), GREEN);
      assert.deepEqual(pixel(x + 130, y), CLEAR);
      assert.deepEqual(pixel(x - 75, y), WHITE);
      assert.deepEqual(pixel(x - 200, y), BLACK);
      assert.deepEqual(pixel(0, y), CLEAR);
    });

    test("a negative source extent crops the normalised rectangle", () => {
      // The other half of the destination case below, and the same defect:
      // `Rect::from_xywh` gives a negative extent `left > right`, which Skia
      // declines. Chrome selects the same pixels for `s(16, 0, -16, 16)` as
      // for `s(0, 0, 16, 16)` -- sorted, not mirrored -- so the left edge of
      // the result stays red either way.
      //
      // The source is red on the left and blue on the right precisely so
      // that sorting and mirroring give different pictures. A uniform source
      // cannot tell them apart, and a pixel count cannot either.
      const src = new Canvas(16, 16),
        s = src.getContext("2d");
      s.fillStyle = "red";
      s.fillRect(0, 0, 8, 16);
      s.fillStyle = "blue";
      s.fillRect(8, 0, 8, 16);

      const paint = (sr, how) => {
        const c = new Canvas(24, 24),
          x = c.getContext("2d");
        x.imageSmoothingEnabled = false;
        if (how === "canvas") x.drawCanvas(src, ...sr, 4, 4, 16, 16);
        else x.drawImage(src, ...sr, 4, 4, 16, 16);
        const d = x.getImageData(0, 0, 24, 24).data;
        let painted = 0;
        for (let i = 3; i < d.length; i += 4) if (d[i] > 0) painted++;
        const at = (px, py) =>
          Array.from(d.slice((py * 24 + px) * 4, (py * 24 + px) * 4 + 3));
        return { painted, left: at(6, 12), right: at(17, 12) };
      };

      for (const how of ["image", "canvas"]) {
        const control = paint([0, 0, 16, 16], how);
        assert.equal(control.painted, 256, `${how}: control fills`);
        assert.deepEqual(control.left, [255, 0, 0], `${how}: red on the left`);
        assert.deepEqual(control.right, [0, 0, 255], `${how}: blue right`);

        for (const [what, sr] of [
          ["sw negative", [16, 0, -16, 16]],
          ["sh negative", [0, 16, 16, -16]],
          ["both negative", [16, 16, -16, -16]],
        ]) {
          assert.deepEqual(paint(sr, how), control, `${how}: ${what}`);
        }

        // The boundary is zero, not "not positive": a zero-width crop draws
        // nothing in a browser too, and sorting leaves it zero-width.
        assert.equal(paint([0, 0, 0, 16], how).painted, 0, `${how}: zero`);
      }
    });

    test("a negative destination extent draws the normalised rectangle", () => {
      // The standard defines the destination by its corners -- "the
      // rectangle whose corners are the four points (dx, dy), (dx+dw, dy),
      // (dx+dw, dy+dh), (dx, dy+dh)" -- so `dx = 12, dw = -8` spans x from 4
      // to 12 and is well formed. `Rect::from_xywh` gave it `left > right`
      // instead, which Skia declines to draw, so all three cases below
      // painted nothing at all.
      //
      // Sorted rather than mirrored, which is the part worth pinning: a
      // browser draws the same orientation into the normalised rectangle, so
      // the red half stays on the left in every row. A fix that flipped the
      // content would satisfy "something is painted" and be wrong.
      const src = new Canvas(8, 8),
        s = src.getContext("2d");
      s.fillStyle = "red";
      s.fillRect(0, 0, 4, 8);
      s.fillStyle = "blue";
      s.fillRect(4, 0, 4, 8);

      const paint = (args) => {
        const c = new Canvas(16, 16),
          x = c.getContext("2d");
        x.imageSmoothingEnabled = false;
        x.drawImage(src, ...args);
        const d = x.getImageData(0, 0, 16, 16).data;
        let painted = 0;
        for (let i = 3; i < d.length; i += 4) if (d[i] > 0) painted++;
        const at = (px, py) =>
          Array.from(d.slice((py * 16 + px) * 4, (py * 16 + px) * 4 + 3));
        return { painted, left: at(6, 8), right: at(10, 8) };
      };

      const control = paint([0, 0, 8, 8, 4, 4, 8, 8]);
      assert.equal(control.painted, 64, "the control paints the whole rect");
      assert.deepEqual(control.left, [255, 0, 0], "red on the left");
      assert.deepEqual(control.right, [0, 0, 255], "blue on the right");

      for (const [what, args] of [
        ["dw negative", [0, 0, 8, 8, 12, 4, -8, 8]],
        ["dh negative", [0, 0, 8, 8, 4, 12, 8, -8]],
        ["both negative", [0, 0, 8, 8, 12, 12, -8, -8]],
      ]) {
        assert.deepEqual(paint(args), control, `${what} matches the control`);
      }

      // The four-argument form takes its size from the call too.
      const short = paint([12, 4, -8, 8]);
      assert.equal(short.painted, 64, "four-argument form, negative width");
    });

    test("drawImage()", async () => {
      let image = await loadAsset("checkers.png");
      ctx.imageSmoothingEnabled = false;

      ctx.drawImage(image, 0, 0);
      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(1, 0), WHITE);
      assert.deepEqual(pixel(0, 1), WHITE);
      assert.deepEqual(pixel(1, 1), BLACK);

      ctx.drawImage(image, -256, -256, 512, 512);
      assert.deepEqual(pixel(0, 0), BLACK);
      assert.deepEqual(pixel(149, 149), BLACK);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.save();
      ctx.translate(WIDTH / 2, HEIGHT / 2);
      ctx.rotate(0.25 * Math.PI);
      ctx.drawImage(image, -256, -256, 512, 512);
      ctx.restore();
      assert.deepEqual(pixel(0, 0), CLEAR);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.25), BLACK);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.75), BLACK);
      assert.deepEqual(pixel(WIDTH * 0.25, HEIGHT / 2), WHITE);
      assert.deepEqual(pixel(WIDTH * 0.75, HEIGHT / 2), WHITE);
      assert.deepEqual(pixel(WIDTH - 1, HEIGHT - 1), CLEAR);

      let srcCanvas = new Canvas(3, 3),
        srcCtx = srcCanvas.getContext("2d");
      srcCtx.fillStyle = "green";
      srcCtx.fillRect(0, 0, 3, 3);
      srcCtx.clearRect(1, 1, 1, 1);

      ctx.drawImage(srcCanvas, 0, 0);
      assert.deepEqual(pixel(0, 0), GREEN);
      assert.deepEqual(pixel(1, 1), CLEAR);
      assert.deepEqual(pixel(2, 2), GREEN);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.drawImage(srcCanvas, -2, -2, 6, 6);
      assert.deepEqual(pixel(0, 0), CLEAR);
      assert.deepEqual(pixel(2, 0), GREEN);
      assert.deepEqual(pixel(2, 2), GREEN);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.save();
      ctx.translate(WIDTH / 2, HEIGHT / 2);
      ctx.rotate(0.25 * Math.PI);
      ctx.drawImage(srcCanvas, -256, -256, 512, 512);
      ctx.restore();
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.25), GREEN);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.75), GREEN);
      assert.deepEqual(pixel(WIDTH * 0.25, HEIGHT / 2), GREEN);
      assert.deepEqual(pixel(WIDTH * 0.75, HEIGHT / 2), GREEN);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT / 2), CLEAR);
    });

    test("drawImage() clips a crop to the source image", async () => {
      // The HTML spec, on establishing the two rectangles: "If the source
      // rectangle is not entirely within the source image, then clip the
      // source rectangle to the source image, and clip the destination
      // rectangle in the same proportion."
      //
      // Skia does that itself for a bitmap -- the source rect goes to
      // `drawImageRect` under a Strict constraint -- and does not for a
      // picture, where nothing but the destination clip bounds the draw. So
      // an SVG painting outside its own viewport used to show through the
      // part of the destination the crop had excluded.
      const svg = (body) =>
        loadImage(
          Buffer.from(
            `<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">${body}</svg>`,
          ),
        );
      const RED = [255, 0, 0, 255];
      const inside = '<rect width="20" height="20" fill="#ff0000"/>';
      const outside = '<rect x="20" width="20" height="20" fill="#00ff00"/>';

      for (const [what, image] of [
        ["staying inside its viewport", await svg(inside)],
        ["painting outside it", await svg(inside + outside)],
      ]) {
        ctx.clearRect(0, 0, WIDTH, HEIGHT);
        ctx.imageSmoothingEnabled = false;
        // Five units outside the image on every side, so the source rect
        // reaches x = 25 -- into where the second SVG's green rect starts.
        ctx.drawImage(image, -5, -5, 30, 30, 0, 0, 40, 40);
        // Without the destination clipped alongside the source, the green
        // lands from x = (20 + 5) * 40 / 30, which is 33.3.
        assert.deepEqual(pixel(36, 10), CLEAR, `${what}: past the crop`);
        assert.deepEqual(pixel(12, 10), RED, `${what}: inside the crop`);
      }
    });

    test("drawCanvas()", async () => {
      let srcCanvas = new Canvas(3, 3),
        srcCtx = srcCanvas.getContext("2d");
      srcCtx.fillStyle = "green";
      srcCtx.fillRect(0, 0, 3, 3);
      srcCtx.clearRect(1, 1, 1, 1);

      ctx.drawCanvas(srcCanvas, 0, 0);
      assert.deepEqual(pixel(0, 0), GREEN);
      assert.deepEqual(pixel(1, 1), CLEAR);
      assert.deepEqual(pixel(2, 2), GREEN);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.drawCanvas(srcCanvas, -2, -2, 6, 6);
      assert.deepEqual(pixel(0, 0), CLEAR);
      assert.deepEqual(pixel(2, 0), GREEN);
      assert.deepEqual(pixel(2, 2), GREEN);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.save();
      ctx.translate(WIDTH / 2, HEIGHT / 2);
      ctx.rotate(0.25 * Math.PI);
      ctx.drawCanvas(srcCanvas, -256, -256, 512, 512);
      ctx.restore();
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.25), GREEN);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT * 0.75), GREEN);
      assert.deepEqual(pixel(WIDTH * 0.25, HEIGHT / 2), GREEN);
      assert.deepEqual(pixel(WIDTH * 0.75, HEIGHT / 2), GREEN);
      assert.deepEqual(pixel(WIDTH / 2, HEIGHT / 2), CLEAR);

      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      ctx.drawCanvas(srcCanvas, 1, 1, 2, 2, 0, 0, 2, 2);
      assert.deepEqual(pixel(0, 0), CLEAR);
      assert.deepEqual(pixel(0, 1), GREEN);
      assert.deepEqual(pixel(1, 0), GREEN);
      assert.deepEqual(pixel(1, 1), GREEN);

      let image = await loadAsset("checkers.png");
      assert.doesNotThrow(() => ctx.drawCanvas(image, 0, 0));
    });

    test("reset()", async () => {
      ctx.fillStyle = "green";
      ctx.scale(2, 2);
      ctx.translate(0, -HEIGHT / 4);

      ctx.fillRect(WIDTH / 4, HEIGHT / 4, WIDTH / 8, HEIGHT / 8);
      assert.deepEqual(pixel(WIDTH * 0.5 + 1, 0), GREEN);
      assert.deepEqual(pixel(WIDTH * 0.75 - 1, 0), GREEN);

      ctx.beginPath();
      ctx.rect(WIDTH / 4, HEIGHT / 2, 100, 100);
      ctx.reset();
      ctx.fill();
      assert.deepEqual(pixel(WIDTH / 2 + 1, HEIGHT / 2 + 1), CLEAR);
      assert.deepEqual(pixel(WIDTH * 0.5 + 1, 0), CLEAR);
      assert.deepEqual(pixel(WIDTH * 0.75 - 1, 0), CLEAR);

      ctx.globalAlpha = 0.4;
      ctx.reset();
      ctx.fillRect(WIDTH / 2, HEIGHT / 2, 3, 3);
      assert.deepEqual(pixel(WIDTH / 2 + 1, HEIGHT / 2 + 1), BLACK);
    });

    describe("transform()", () => {
      const a = 0.1,
        b = 0,
        c = 0,
        d = 0.3,
        e = 0,
        f = 0;

      test("with args list", () => {
        ctx.transform(a, b, c, d, e, f);
        let matrix = ctx.currentTransform;
        _each({ a, b, c, d, e, f }, (val, term) =>
          assert.nearEqual(matrix[term], val),
        );
      });

      test("with DOMMatrix", () => {
        ctx.transform(new DOMMatrix().scale(0.1, 0.3));
        let matrix = ctx.currentTransform;
        _each({ a, b, c, d, e, f }, (val, term) =>
          assert.nearEqual(matrix[term], val),
        );
      });

      test("with matrix-like object", () => {
        ctx.transform({ a, b, c, d, e, f });
        let matrix = ctx.currentTransform;
        _each({ a, b, c, d, e, f }, (val, term) =>
          assert.nearEqual(matrix[term], val),
        );
      });

      test("a partial DOMMatrixInit keeps its 3D cells", () => {
        // `fromMatrix` required all sixteen cells to take the 4x4 path and
        // otherwise read only `a` through `f`, so a partial dictionary --
        // the ordinary case, and the one the declarations describe when they
        // say a cell left out takes the identity -- silently lost every 3D
        // cell. `{m13: 5}` read back as `0`, and `is2D` then said true
        // because the content it described had already been discarded.
        for (const [cell, value, identity] of [
          ["m13", 5, 0],
          ["m14", 5, 0],
          ["m23", 5, 0],
          ["m24", 5, 0],
          ["m31", 5, 0],
          ["m32", 5, 0],
          ["m34", 5, 0],
          ["m43", 5, 0],
          ["m33", 2, 1],
          ["m44", 2, 1],
        ]) {
          const m = DOMMatrix.fromMatrix({ [cell]: value });
          assert.equal(m[cell], value, `${cell} survives`);
          assert.equal(m.is2D, false, `${cell} makes it 3D`);
          assert.notEqual(identity, value, "the case is not vacuous");
        }

        // The 2D half must still work, and a dictionary naming nothing 3D
        // is still 2D -- the failure a fix that simply forced 3D would show.
        const flat = DOMMatrix.fromMatrix({ a: 2, f: 3 });
        assert.equal(flat.m11, 2);
        assert.equal(flat.m42, 3);
        assert.equal(flat.is2D, true, "no 3D cell named, so still 2D");
        assert.equal(DOMMatrix.fromMatrix({}).is2D, true, "identity is 2D");
      });

      test("a DOMMatrixInit that contradicts itself is refused", () => {
        // Both promised by `lib/index.d.ts` and neither could fire: the
        // contradiction was erased before anything could see it.
        assert.throws(
          () => DOMMatrix.fromMatrix({ is2D: true, m13: 5 }),
          TypeError,
          "is2D true beside a 3D cell",
        );
        assert.throws(
          () => DOMMatrix.fromMatrix({ a: 1, m11: 2 }),
          TypeError,
          "an alias and its long name disagreeing",
        );

        // An alias agreeing with its long name is not a contradiction, and
        // neither is is2D:true on a matrix that really is 2D.
        assert.doesNotThrow(() => DOMMatrix.fromMatrix({ a: 2, m11: 2 }));
        assert.doesNotThrow(() => DOMMatrix.fromMatrix({ is2D: true, a: 2 }));
      });

      test("with css-style string", () => {
        // try a range of string inits
        const transforms = {
          "matrix(1, 2, 3, 4, 5, 6)": "matrix(1, 2, 3, 4, 5, 6)",
          "matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)":
            "matrix(1, 0, 0, 1, 0, 0)",
          "rotate(0.5turn)": "matrix(-1, 0, 0, -1, 0, 0)",
          "rotate3d(1, 2, 3, 10deg)":
            "matrix3d(0.985892913511, 0.141398603856, -0.089563373741, 0, -0.137057961859, 0.989148395009, 0.052920390614, 0, 0.096074336736, -0.039898464624, 0.994574197504, 0, 0, 0, 0, 1)",
          "rotateX(10deg)":
            "matrix3d(1, 0, 0, 0, 0, 0.984807753012, 0.173648177667, 0, 0, -0.173648177667, 0.984807753012, 0, 0, 0, 0, 1)",
          "rotateY(10deg)":
            "matrix3d(0.984807753012, 0, -0.173648177667, 0, 0, 1, 0, 0, 0.173648177667, 0, 0.984807753012, 0, 0, 0, 0, 1)",
          "rotateZ(10deg)":
            "matrix(0.984807753012, 0.173648177667, -0.173648177667, 0.984807753012, 0, 0)",
          "translate(12px, 50px)": "matrix(1, 0, 0, 1, 12, 50)",
          "translate3d(12px, 50px, 3px)":
            "matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 12, 50, 3, 1)",
          "translateX(2px)": "matrix(1, 0, 0, 1, 2, 0)",
          "translateY(3px)": "matrix(1, 0, 0, 1, 0, 3)",
          "translateZ(2px)":
            "matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 2, 1)",
          "scale(2, 0.5)": "matrix(2, 0, 0, 0.5, 0, 0)",
          "scale3d(2.5, 120%, 0.3)":
            "matrix3d(2.5, 0, 0, 0, 0, 1.2, 0, 0, 0, 0, 0.3, 0, 0, 0, 0, 1)",
          "scaleX(2)": "matrix(2, 0, 0, 1, 0, 0)",
          "scaleY(0.5)": "matrix(1, 0, 0, 0.5, 0, 0)",
          "scaleZ(0.3)":
            "matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0.3, 0, 0, 0, 0, 1)",
          "skew(30deg, 20deg)":
            "matrix(1, 0.363970234266, 0.577350269190, 1, 0, 0)",
          "skewX(30deg)": "matrix(1, 0, 0.577350269190, 1, 0, 0)",
          "skewY(1.07rad)": "matrix(1, 1.827028196535, 0, 1, 0, 0)",
          "translate(10px, 20px) matrix(1, 2, 3, 4, 5, 6)":
            "matrix(1, 2, 3, 4, 15, 26)",
          "translate(5px, 6px) scale(2) translate(7px,8px)":
            "matrix(2, 0, 0, 2, 19, 22)",
          "rotate(30deg) rotate(-.1turn) rotate(.444rad)":
            "matrix(0.942994450354, 0.332808453321, -0.332808453321, 0.942994450354, 0, 0)",
          none: "matrix(1, 0, 0, 1, 0, 0)",
          unset: "matrix(1, 0, 0, 1, 0, 0)",
        };

        for (const input in transforms) {
          let matrix = new DOMMatrix(input),
            roundtrip = new DOMMatrix(matrix.toString());
          assert.equal(matrix.toString(), transforms[input]);
          assert.equal(roundtrip.toString(), transforms[input]);
        }

        // check that the context can also take a string
        ctx.transform(`scale(${a}, ${d})`);
        let matrix = ctx.currentTransform;
        _each({ a, b, c, d, e, f }, (val, term) =>
          assert.nearEqual(matrix[term], val),
        );
      });

      test("rejects invalid args", () => {
        assert.throws(
          () => ctx.transform("nonesuch"),
          /Invalid transform matrix/,
        );
        assert.throws(() => ctx.transform(0, 0, 0), /not enough arguments/);
        assert.doesNotThrow(() => ctx.transform(0, 0, 0, NaN, 0, 0));
      });
    });
  });

  describe("parses", () => {
    test("fonts", () => {
      let cases = {
        "20px Arial": { size: 20, family: ["Arial"] },
        "33pt Arial": { size: 44, family: ["Arial"] },
        "75pt Arial": { size: 100, family: ["Arial"] },
        "20% Arial": { size: 16 * 0.2, family: ["Arial"] },
        "20mm Arial": { size: 75.59055118110237, family: ["Arial"] },
        "20px serif": { size: 20, family: ["serif"] },
        "20px sans-serif": { size: 20, family: ["sans-serif"] },
        "20px monospace": { size: 20, family: ["monospace"] },
        "50px Arial, sans-serif": { size: 50, family: ["Arial", "sans-serif"] },
        "bold italic 50px Arial, sans-serif": {
          style: "italic",
          weight: 700,
          size: 50,
          family: ["Arial", "sans-serif"],
        },
        "50px Helvetica ,  Arial, sans-serif": {
          size: 50,
          family: ["Helvetica", "Arial", "sans-serif"],
        },
        '50px "Helvetica Neue", sans-serif': {
          size: 50,
          family: ["Helvetica Neue", "sans-serif"],
        },
        '50px "Helvetica Neue", "foo bar baz" , sans-serif': {
          size: 50,
          family: ["Helvetica Neue", "foo bar baz", "sans-serif"],
        },
        "50px 'Helvetica Neue'": { size: 50, family: ["Helvetica Neue"] },
        "italic 20px Arial": { size: 20, style: "italic", family: ["Arial"] },
        "oblique 20px Arial": { size: 20, style: "oblique", family: ["Arial"] },
        "normal 20px Arial": { size: 20, style: "normal", family: ["Arial"] },
        "300 20px Arial": { size: 20, weight: 300, family: ["Arial"] },
        "800 20px Arial": { size: 20, weight: 800, family: ["Arial"] },
        "bolder 20px Arial": { size: 20, weight: 700, family: ["Arial"] },
        "lighter 20px Arial": { size: 20, weight: 100, family: ["Arial"] },
        "normal normal normal 16px Impact": {
          size: 16,
          weight: 400,
          family: ["Impact"],
          style: "normal",
          variant: "normal",
        },
        "italic small-caps bolder 16px cursive": {
          size: 16,
          style: "italic",
          variant: "small-caps",
          weight: 700,
          family: ["cursive"],
        },
        '20px "new century schoolbook", serif': {
          size: 20,
          family: ["new century schoolbook", "serif"],
        },
        '20px "Arial bold 300"': {
          size: 20,
          family: ["Arial bold 300"],
          variant: "normal",
        }, // synthetic case with weight keyword inside family
      };

      _each(cases, (spec, font) => {
        let expected = {
            style: "normal",
            stretch: "normal",
            variant: "normal",
            ...spec,
          },
          parsed = css.font(font);
        assert.matchesSubset(parsed, expected);
      });
    });

    // Units and keywords in CSS are ASCII case-insensitive, so every one of
    // these is a valid font that was being dropped. The value is normalised
    // on the way in, so the canonical form a caller reads back is lowercase
    // whatever case they wrote -- which is what a browser reports.
    test("fonts, in any case", () => {
      for (let [font, canonical] of [
        ["5PX serif", "normal 400 5px serif"],
        ["5Px serif", "normal 400 5px serif"],
        ["1EM serif", "normal 400 16px serif"],
        // Written as the parser computes it -- `size * (1 / 0.75)` rather
        // than `size / 0.75`, which differ in the last bit.
        ["5PT serif", `normal 400 ${5 * (1 / 0.75)}px serif`],
        ["2Q serif", "normal 400 1.8897637795275593px serif"],
        ["5REM serif", "normal 400 80px serif"],
        ["MEDIUM serif", "normal 400 16px serif"],
        ["X-LARGE serif", "normal 400 24px serif"],
        ["ITALIC 20px serif", "italic normal 400 20px serif"],
        ["Oblique 20px serif", "oblique normal 400 20px serif"],
        ["SMALL-CAPS 20px serif", "normal small-caps 400 20px serif"],
        ["CONDENSED 20px serif", "normal 400 condensed 20px serif"],
        ["BOLD 20px serif", "normal 700 20px serif"],
        ["Bolder 20px serif", "normal 700 20px serif"],
        ["LIGHTER 20px serif", "normal 100 20px serif"],
      ]) {
        assert.equal(css.font(font)?.canonical, canonical, font);
      }
    });

    test("a mixed-case font reaches ctx.font", () => {
      ctx.font = "ITALIC BOLD 20PX serif";
      // The getter reports the serialized form, so `bold` rather than 700
      // and no `normal` variant; the table above is the canonical string,
      // which is the addon's cache key and keeps both.
      assert.equal(ctx.font, "italic bold 20px serif");
    });

    // CSS defines `font-size` over a non-negative length, and `line-height`
    // the same way, so a negative one makes the whole shorthand invalid and
    // the assignment is ignored. Zero is not negative and stays legal.
    test("fonts, refusing a negative size", () => {
      for (let font of [
        "-5px serif",
        "-0.5em serif",
        "-1pt serif",
        "normal -5px serif",
        "bold italic -20px Arial, sans-serif",
        "12px/-1.2 serif",
        "-5px/1.2 serif",
      ]) {
        assert.equal(css.font(font), null, `${font} should not parse`);
      }

      for (let [font, size] of [
        ["0px serif", 0],
        ["5px serif", 5],
        ["0.5em serif", 8],
      ]) {
        assert.matchesSubset(css.font(font), { size }, font);
      }
    });

    test("a negative size leaves ctx.font alone", () => {
      let before = ctx.font;
      ctx.font = "-5px serif";
      assert.equal(ctx.font, before, "an invalid font is ignored");
    });

    // The shared length parser stays permissive on purpose: a shadow offset
    // is legitimately negative and reaches `parseSize` by the same route a
    // font size does, so the refusal belongs in the shorthand and not there.
    test("a negative shadow offset is still accepted", () => {
      ctx.filter = "drop-shadow(-20px 0 0 #f00)";
      assert.match(ctx.filter, /drop-shadow\(-20px/);
      ctx.filter = "none";
    });

    // Units and keywords are ASCII case-insensitive wherever they appear, not
    // only in the `font` shorthand. Each of these reaches a different parser.
    describe("case-insensitivity outside the font shorthand", () => {
      test("fontStretch", () => {
        for (let [written, expected] of [
          ["condensed", "condensed"],
          ["CONDENSED", "condensed"],
          ["Semi-Expanded", "semi-expanded"],
          ["ULTRA-CONDENSED", "ultra-condensed"],
        ]) {
          ctx.fontStretch = written;
          assert.equal(ctx.fontStretch, expected, written);
        }
      });

      test("letterSpacing and wordSpacing", () => {
        // Only the absolute units. `parseFlexibleSize` has no `em` arm, so a
        // font-relative spacing produces `NaN` and the addon refuses it out
        // loud -- true of `"1em"` as much as `"1EM"`, so it is not this
        // function's problem and is reported separately.
        for (let [written, expected] of [
          ["2px", "2px"],
          ["2PX", "2px"],
          ["3PT", "3pt"],
          ["-1MM", "-1mm"],
        ]) {
          ctx.letterSpacing = written;
          assert.equal(ctx.letterSpacing, expected, `letterSpacing ${written}`);
          ctx.wordSpacing = written;
          assert.equal(ctx.wordSpacing, expected, `wordSpacing ${written}`);
        }
        ctx.letterSpacing = "0px";
        ctx.wordSpacing = "0px";
      });

      test("textDecoration", () => {
        for (let written of [
          "UNDERLINE",
          "Underline WAVY",
          "OVERLINE DOTTED",
          "line-through DOUBLE",
        ]) {
          ctx.textDecoration = written;
          assert.equal(
            ctx.textDecoration.toLowerCase(),
            written.toLowerCase(),
            written,
          );
        }
        ctx.textDecoration = "none";
      });

      test("filter function names", () => {
        for (let [written, expected] of [
          ["blur(3px)", "blur(3px)"],
          ["BLUR(3px)", "blur(3px)"],
          ["blur(3PX)", "blur(3px)"],
          ["Drop-Shadow(2px 2px 2px red)", "drop-shadow(2px 2px 2px red)"],
          ["HUE-ROTATE(45DEG)", "hue-rotate(45deg)"],
          ["Grayscale(50%)", "grayscale(50%)"],
        ]) {
          ctx.filter = written;
          assert.equal(ctx.filter, expected, written);
        }
        ctx.filter = "none";
      });

      // The `i` flag on the shared `numSizeRE` made this worse before the
      // normalisation caught up with it: `2PX` began matching, then missed
      // every `unit ==` arm, and the `NaN` reached the addon as a value it
      // refused out loud. A drop that became a throw.
      test("a bad unit is still refused, and quietly", () => {
        ctx.letterSpacing = "0px";
        // `"2pxx"` is absent deliberately: `numSizeRE` is unanchored at the
        // end, so it reads the `2px` inside and accepts it. That is not
        // case-related and is reported rather than changed here -- anchoring
        // it reaches every caller of the shared expression.
        for (let bad of ["2 px", "px", "2ZZ", ""]) {
          assert.doesNotThrow(
            () => {
              ctx.letterSpacing = bad;
            },
            `${JSON.stringify(bad)} should be ignored, not thrown`,
          );
        }
        assert.equal(ctx.letterSpacing, "0px", "an invalid spacing is ignored");
      });
    });

    // Every other context property ignores what it cannot parse, which is
    // what the Canvas standard asks of an attribute setter. This one threw,
    // so an unparseable variant reached the caller as an exception -- and a
    // *valid* one did too, because the match was case-sensitive.
    describe("fontVariant", () => {
      test("takes a keyword in any case", () => {
        for (let [written, expected] of [
          ["SMALL-CAPS", "small-caps"],
          ["Small-Caps", "small-caps"],
          ["OLDSTYLE-NUMS", "oldstyle-nums"],
          ["NORMAL", "normal"],
          ["small-caps", "small-caps"],
        ]) {
          ctx.fontVariant = written;
          assert.equal(ctx.fontVariant, expected, written);
        }
      });

      test("takes a parameterized alternate in any case", () => {
        ctx.fontVariant = "STYLISTIC(2)";
        assert.equal(ctx.fontVariant, "stylistic(2)");
      });

      test("ignores what it cannot parse rather than throwing", () => {
        ctx.fontVariant = "small-caps";
        for (let bad of [
          "bogus",
          "small-caps bogus",
          "bogus(1)",
          "stylistic(", // a parameterized form that does not close
          "",
        ]) {
          assert.doesNotThrow(
            () => {
              ctx.fontVariant = bad;
            },
            `${JSON.stringify(bad)} should be ignored, not thrown`,
          );
          assert.equal(
            ctx.fontVariant,
            "small-caps",
            `${JSON.stringify(bad)} changed the value`,
          );
        }
      });

      test("fontVariantCaps still reads and rewrites it", () => {
        ctx.fontVariant = "SMALL-CAPS";
        assert.equal(ctx.fontVariantCaps, "small-caps");
        ctx.fontVariantCaps = "normal";
        assert.equal(ctx.fontVariant, "normal");
      });
    });

    test("colors", () => {
      ctx.fillStyle = "#ffccaa";
      assert.equal(ctx.fillStyle, "#ffccaa");

      ctx.fillStyle = "#FFCCAA";
      assert.equal(ctx.fillStyle, "#ffccaa");

      ctx.fillStyle = "#FCA";
      assert.equal(ctx.fillStyle, "#ffccaa");

      ctx.fillStyle = "#0ff";
      ctx.fillStyle = "#FGG";
      assert.equal(ctx.fillStyle, "#00ffff");

      ctx.fillStyle = "#fff";
      ctx.fillStyle = "afasdfasdf";
      assert.equal(ctx.fillStyle, "#ffffff");

      // #rgba and #rrggbbaa

      ctx.fillStyle = "#ffccaa80";
      assert.equal(ctx.fillStyle, "rgba(255, 204, 170, 0.502)");

      ctx.fillStyle = "#acf8";
      assert.equal(ctx.fillStyle, "rgba(170, 204, 255, 0.533)");

      ctx.fillStyle = "#BEAD";
      assert.equal(ctx.fillStyle, "rgba(187, 238, 170, 0.867)");

      ctx.fillStyle = "rgb(255,255,255)";
      assert.equal(ctx.fillStyle, "#ffffff");

      ctx.fillStyle = "rgb(0,0,0)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "rgb( 0  ,   0  ,  0)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "rgba( 0  ,   0  ,  0, 1)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "rgba( 255, 200, 90, 0.5)";
      assert.equal(ctx.fillStyle, "rgba(255, 200, 90, 0.502)");

      ctx.fillStyle = "rgba( 255, 200, 90, 0.75)";
      assert.equal(ctx.fillStyle, "rgba(255, 200, 90, 0.749)");

      ctx.fillStyle = "rgba( 255, 200, 90, 0.7555)";
      assert.equal(ctx.fillStyle, "rgba(255, 200, 90, 0.757)");

      ctx.fillStyle = "rgba( 255, 200, 90, .7555)";
      assert.equal(ctx.fillStyle, "rgba(255, 200, 90, 0.757)");

      ctx.fillStyle = "rgb(0, 0, 9000)";
      assert.equal(ctx.fillStyle, "#0000ff");

      ctx.fillStyle = "rgba(0, 0, 0, 42.42)";
      assert.equal(ctx.fillStyle, "#000000");

      // hsl / hsla tests

      ctx.fillStyle = "hsl(0, 0%, 0%)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "hsl(3600, -10%, -10%)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "hsl(10, 100%, 42%)";
      assert.equal(ctx.fillStyle, "#d62400");

      ctx.fillStyle = "hsl(370, 120%, 42%)";
      assert.equal(ctx.fillStyle, "#d62400");

      ctx.fillStyle = "hsl(0, 100%, 100%)";
      assert.equal(ctx.fillStyle, "#ffffff");

      ctx.fillStyle = "hsl(0, 150%, 150%)";
      assert.equal(ctx.fillStyle, "#ffffff");

      ctx.fillStyle = "hsl(237, 76%, 25%)";
      assert.equal(ctx.fillStyle, "#0f1470");

      ctx.fillStyle = "hsl(240, 73%, 25%)";
      assert.equal(ctx.fillStyle, "#11116e");

      ctx.fillStyle = "hsl(262, 32%, 42%)";
      assert.equal(ctx.fillStyle, "#62498d");

      ctx.fillStyle = "hsla(0, 0%, 0%, 1)";
      assert.equal(ctx.fillStyle, "#000000");

      ctx.fillStyle = "hsla(0, 100%, 100%, 1)";
      assert.equal(ctx.fillStyle, "#ffffff");

      ctx.fillStyle = "hsla(120, 25%, 75%, 0.5)";
      assert.equal(ctx.fillStyle, "rgba(175, 207, 175, 0.502)");

      ctx.fillStyle = "hsla(240, 75%, 25%, 0.75)";
      assert.equal(ctx.fillStyle, "rgba(16, 16, 112, 0.749)");

      ctx.fillStyle = "hsla(172.0, 33.00000e0%, 42%, 1)";
      assert.equal(ctx.fillStyle, "#488e85");

      ctx.fillStyle = "hsl(124.5, 76.1%, 47.6%)";
      assert.equal(ctx.fillStyle, "#1dd62b");

      ctx.fillStyle = "hsl(1.24e2, 760e-1%, 4.7e1%)";
      assert.equal(ctx.fillStyle, "#1dd329");

      // case-insensitive css names

      ctx.fillStyle = "sILveR";
      assert.equal(ctx.fillStyle, "#c0c0c0");

      // wrong type args

      let transparent = "rgba(0, 0, 0, 0)";
      ctx.fillStyle = "transparent";
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = null;
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = NaN;
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = [undefined, 255, false];
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = true;
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = {};
      assert.equal(ctx.fillStyle, transparent);

      // objects with .toString methods

      ctx.fillStyle = { toString: () => "red" };
      assert.equal(ctx.fillStyle, "#ff0000");

      ctx.fillStyle = "transparent";
      ctx.fillStyle = { toString: "red" };
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = { toString: () => "gobbledygook" };
      assert.equal(ctx.fillStyle, transparent);

      ctx.fillStyle = { toString: () => NaN };
      assert.equal(ctx.fillStyle, transparent);
    });

    test("CSS Color 4 functions", () => {
      // The parser was CSS Color 3, so every one of these fell through to the
      // "unparseable" path and left the previous colour standing -- which
      // reads as black on a fresh context.
      ctx.fillStyle = "hwb(90 10% 20%)";
      assert.equal(ctx.fillStyle, "#73cc1a", "matches what a browser gives");

      // These assert the colour, not merely that the string parsed. Asserting
      // "not black" cannot see a wrong answer: `lab()` resolved against the
      // wrong white point and every non-black result still passed. The
      // expected values are computed from CSS Color 4's conversion code --
      // D50 through a Bradford adaptation to D65 -- rather than read back
      // from this implementation.
      ctx.fillStyle = "#000";
      ctx.fillStyle = "lab(50% 70 50)";
      assert.equal(ctx.fillStyle, "#e32427", "lab() resolves against D50");

      ctx.fillStyle = "#000";
      ctx.fillStyle = "lch(50% 70 50)";
      assert.equal(ctx.fillStyle, "#c55218", "lch() resolves against D50");

      // Oklab is defined D65-referred and has no adaptation step, so it is
      // the control: it was correct throughout and must stay so.
      ctx.fillStyle = "#000";
      ctx.fillStyle = "oklch(0.7 0.2 140)";
      assert.equal(ctx.fillStyle, "#4dba30", "oklch() is unaffected");

      // On the neutral axis both conversions agree exactly, so this grey
      // says nothing about the white point. Here to stop it being mistaken
      // for coverage.
      ctx.fillStyle = "#000";
      ctx.fillStyle = "lab(50% 0 0)";
      assert.equal(ctx.fillStyle, "#777777", "cannot discriminate; see above");

      // A colour the parser cannot read still leaves the previous one alone.
      ctx.fillStyle = "#123456";
      ctx.fillStyle = "oklch(nonsense)";
      assert.equal(ctx.fillStyle, "#123456");
    });

    test("a colour outside sRGB survives being set", () => {
      // `oklch(0.7 0.35 30)` is well outside the sRGB gamut. Quantising it to
      // eight bits on the way in threw that away before the surface saw it,
      // and reading it back as `#ff0000` reported a colour the context was
      // not holding.
      ctx.fillStyle = "oklch(0.7 0.35 30)";
      let read = ctx.fillStyle;
      assert.match(
        read,
        /^color\(srgb /,
        `an out-of-gamut colour keeps its components, got ${read}`,
      );
      assert.ok(
        read
          .split(" ")
          .slice(1)
          .some((n) => parseFloat(n) > 1),
        `and they are outside 0..1, got ${read}`,
      );

      // Setting it back reproduces the same colour, so the reported form is
      // one the parser understands.
      ctx.fillStyle = "#000";
      ctx.fillStyle = read;
      assert.equal(ctx.fillStyle, read, "the serialisation round-trips");

      // Anything inside the gamut still reads back the way a browser writes
      // it -- hex, and rounded rather than floored.
      ctx.fillStyle = "hwb(90 10% 20%)";
      assert.equal(ctx.fillStyle, "#73cc1a");
    });

    test("color() names a space of its own", () => {
      // `color(display-p3 …)` is how CSS Color 4 names a colour outside sRGB.
      // csscolorparser does not implement the function, so this is parsed
      // here -- and the colour is kept in the space it was named in rather
      // than converted, which is what makes it exact on a canvas of that
      // space.
      for (let css of [
        "color(display-p3 0.4 0.8 0.3)",
        "color(rec2020 1 0 0)",
        "color(display-p3 1 0 0 / 0.5)",
      ]) {
        ctx.fillStyle = "#000";
        ctx.fillStyle = css;
        assert.equal(ctx.fillStyle, css, "echoed in the space it named");
      }

      // srgb is the space everything else reports in, so it serialises the
      // ordinary way.
      ctx.fillStyle = "color(srgb 1 0 0)";
      assert.equal(ctx.fillStyle, "#ff0000");

      // Percentages are components too, and an unknown space is not a colour.
      ctx.fillStyle = "#000";
      ctx.fillStyle = "color(srgb 100% 0% 0%)";
      assert.equal(ctx.fillStyle, "#ff0000");

      ctx.fillStyle = "#123456";
      ctx.fillStyle = "color(bogus 1 0 0)";
      assert.equal(ctx.fillStyle, "#123456", "an unknown space is ignored");
    });

    test("color() lands on the pixel a browser lands on", () => {
      // Measured in Chrome: the same three draws, read back through a P3
      // canvas. Converting via sRGB on the way in cost a level on the third.
      let drawn = (canvasSpace, css) => {
        let canvas = new Canvas(2, 2, { colorSpace: canvasSpace });
        let ctx2 = canvas.getContext("2d");
        ctx2.fillStyle = css;
        ctx2.fillRect(0, 0, 2, 2);
        return Array.from(
          canvas.toBufferSync("raw", { colorSpace: "display-p3" }).slice(0, 4),
        );
      };

      assert.deepEqual(
        drawn("display-p3", "color(display-p3 1 0 0)"),
        [255, 0, 0, 255],
      );
      assert.deepEqual(
        drawn("display-p3", "color(display-p3 0.4 0.8 0.3)"),
        [102, 204, 77, 255],
      );
      assert.deepEqual(
        drawn("srgb", "color(display-p3 1 0 0)"),
        [234, 51, 35, 255],
        "and an sRGB canvas clips it, as a browser's does",
      );
    });

    test("a color() stop paints what the same color fills", () => {
      // A gradient stop dropped the space and kept the raw components, so
      // they were read as sRGB: `color(srgb-linear 0.2 0.4 0.6)` filled
      // 124,170,203 and painted 51,102,153 through a stop. Skia interpolates
      // the stops it is handed and has no paint to tag, so a stop is
      // converted before it is stored rather than carrying its space along.
      let painted = (css, through) => {
        let ctx2 = new Canvas(4, 4).getContext("2d");
        ctx2.clearRect(0, 0, 4, 4);
        if (through === "fill") {
          ctx2.fillStyle = css;
        } else {
          let gradient = ctx2.createLinearGradient(0, 0, 4, 0);
          gradient.addColorStop(0, css);
          gradient.addColorStop(1, css);
          ctx2.fillStyle = gradient;
        }
        ctx2.fillRect(0, 0, 4, 4);
        return Array.from(ctx2.getImageData(1, 1, 1, 1).data).slice(0, 3);
      };

      for (let css of [
        "color(srgb 0.2 0.4 0.6)",
        "color(srgb-linear 0.2 0.4 0.6)",
        "color(display-p3 0.2 0.4 0.6)",
        "color(rec2020 0.2 0.4 0.6)",
      ]) {
        assert.deepEqual(painted(css, "stop"), painted(css, "fill"), css);
      }

      // Pinned as well as compared, so the pair agreeing on a wrong answer
      // would still fail. Linear 0.2 is 124 through the sRGB transfer curve.
      assert.deepEqual(
        painted("color(srgb-linear 0.2 0.4 0.6)", "fill"),
        [124, 170, 203],
      );
    });

    test("color(rec2020 ...) converts through Rec. 2020's own curve", () => {
      // Skia has no transfer function for Rec. 2020: `skia_safe`'s CICP
      // transfer functions are reference EOTFs, and `REC2020_10BIT` and
      // `REC2020_12BIT` are both aliases of `REC709`, which is a pure 2.4
      // gamma. Tagging the paint therefore decoded the components with the
      // wrong curve. 0,120,168 is what the CSS Color 4 conversion matrices
      // give for these components, and what Chrome paints.
      let ctx2 = new Canvas(4, 4).getContext("2d");
      ctx2.fillStyle = "color(rec2020 0.2 0.4 0.6)";
      ctx2.fillRect(0, 0, 4, 4);
      assert.deepEqual(
        Array.from(ctx2.getImageData(1, 1, 1, 1).data).slice(0, 3),
        [0, 120, 168],
      );

      // Every surface has to answer alike. A grey isolates the transfer
      // function from the primaries: the wrong curve gave 40 where 67 is
      // right, and each of these reached the paint by a different route.
      let grey = "color(rec2020 0.2 0.2 0.2)";
      let sample = (draw) => {
        let c = new Canvas(4, 4).getContext("2d");
        c.clearRect(0, 0, 4, 4);
        draw(c);
        return Array.from(c.getImageData(1, 1, 1, 1).data).slice(0, 3);
      };
      let byFill = sample((c) => {
        c.fillStyle = grey;
        c.fillRect(0, 0, 4, 4);
      });
      let byStop = sample((c) => {
        let gradient = c.createLinearGradient(0, 0, 4, 0);
        gradient.addColorStop(0, grey);
        gradient.addColorStop(1, grey);
        c.fillStyle = gradient;
        c.fillRect(0, 0, 4, 4);
      });
      let byShadow = sample((c) => {
        c.shadowColor = grey;
        c.shadowOffsetX = 8;
        c.fillStyle = "#000";
        c.fillRect(-8, 0, 4, 4);
      });
      assert.deepEqual(byFill, [67, 67, 67], "through a fill");
      assert.deepEqual(byStop, byFill, "through a gradient stop");
      assert.deepEqual(byShadow, byFill, "through a shadow");
    });
  });

  describe("validates", () => {
    let g, id, img, p2d;
    beforeEach(async () => {
      g = ctx.createLinearGradient(0, 0, 10, 10);
      id = ctx.getImageData(0, 0, 10, 10);
      img = await loadAsset("checkers.png");
      p2d = new Path2D();
      p2d.rect(0, 0, 100, 100);
      ctx.rect(0, 0, 100, 100);
    });

    test("not enough arguments", async () => {
      let ERR = /not enough arguments/;
      assert.throws(() => ctx.transform(), ERR);
      assert.throws(() => ctx.transform(0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.setTransform(0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.translate(0), ERR);
      assert.throws(() => ctx.scale(0), ERR);
      assert.throws(() => ctx.rotate(), ERR);
      assert.throws(() => ctx.rect(0, 0, 0), ERR);
      assert.throws(() => ctx.arc(0, 0, 0, 0), ERR);
      assert.throws(() => ctx.arcTo(0, 0, 0, 0), ERR);
      assert.throws(() => ctx.ellipse(0, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.moveTo(0), ERR);
      assert.throws(() => ctx.lineTo(0), ERR);
      assert.throws(() => ctx.bezierCurveTo(0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.quadraticCurveTo(0, 0, 0), ERR);
      assert.throws(() => ctx.conicCurveTo(0, 0, 0, 0), ERR);
      assert.throws(() => ctx.roundRect(0, 0, 0), ERR);
      assert.throws(() => ctx.fillRect(0, 0, 0), ERR);
      assert.throws(() => ctx.strokeRect(0, 0, 0), ERR);
      assert.throws(() => ctx.clearRect(0, 0, 0), ERR);
      assert.throws(() => ctx.fillText("text", 0), ERR);
      assert.throws(() => ctx.isPointInPath(10), ERR);
      assert.throws(() => ctx.isPointInStroke(10), ERR);
      assert.throws(() => ctx.createLinearGradient(0, 0, 1), ERR);
      assert.throws(() => ctx.createRadialGradient(0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.createConicGradient(0, 0), ERR);
      assert.throws(() => ctx.setLineDash(), ERR);
      assert.throws(() => ctx.createImageData(), ERR);
      assert.throws(() => ctx.createPattern(img), ERR);
      assert.throws(() => ctx.createTexture(), ERR);
      assert.throws(() => ctx.getImageData(1, 1, 10), ERR);
      assert.throws(() => ctx.putImageData({}, 0), ERR);
      assert.throws(() => ctx.putImageData(id, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawImage(img), ERR);
      assert.throws(() => ctx.drawImage(img, 0), ERR);
      assert.throws(() => ctx.drawImage(img, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawImage(img, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawImage(img, 0, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawImage(img, 0, 0, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawCanvas(canvas), ERR);
      assert.throws(() => ctx.drawCanvas(canvas, 0), ERR);
      assert.throws(() => ctx.drawCanvas(canvas, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawCanvas(canvas, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawCanvas(canvas, 0, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => ctx.drawCanvas(canvas, 0, 0, 0, 0, 0, 0, 0), ERR);
      assert.throws(() => g.addColorStop(0), ERR);
    });

    test("value errors", async () => {
      assert.throws(
        () => ctx.ellipse(0, 0, -10, -10, 0, 0, 0, false),
        /Radius value must be positive/,
      );
      // The one that was missed. Chrome throws for arc as it does for the
      // three below; this drew an inverted oval instead.
      assert.throws(
        () => ctx.arc(0, 0, -10, 0, 1, false),
        /Radius value must be positive/,
      );
      assert.throws(
        () => ctx.arcTo(0, 0, 0, 0, -10),
        /Radius value must be positive/,
      );
      // A `RangeError` naming the value, against the `IndexSizeError` the
      // line above asserts for `arcTo`. Both are Chrome 148's, verified
      // together: `roundRect`'s clause names a `RangeError` and `arc`,
      // `ellipse` and `arcTo` name an `IndexSizeError`. Asserted side by side
      // so a later pass at consistency has to notice it is deliberate.
      assert.throws(() => ctx.roundRect(0, 0, 0, 0, -10), {
        name: "RangeError",
        message: /Radius value -10 is negative/,
      });
      // An `IndexSizeError` since #85, where the standard names one, and
      // the message names what the caller passed rather than the internal
      // arithmetic it failed.
      assert.throws(() => ctx.createImageData(1, 0), {
        name: "IndexSizeError",
        message: /zero, negative or not a number/,
      });
      assert.throws(() => ctx.getImageData(1, 1, NaN, 10), /Expected a number/);
      assert.throws(
        () => ctx.getImageData(1, NaN, 10, 10),
        /Expected a number/,
      );
      assert.throws(
        () => ctx.createImageData(1, {}),
        /zero, negative or not a number/,
      );
      assert.throws(
        () => ctx.createImageData(1, NaN),
        /zero, negative or not a number/,
      );
      assert.throws(() => ctx.putImageData(id, NaN, 0), /Expected a number/);
      assert.throws(
        () => ctx.putImageData(id, 0, 0, 0, 0, NaN, 0),
        /Expected a number for `dirtyWidth`/,
      );
      assert.throws(
        () => ctx.putImageData({}, 0, 0),
        /Expected an ImageData as 1st arg/,
      );
      assert.throws(() => ctx.drawImage(), /Expected an Image or a Canvas/);
      assert.throws(() => ctx.drawCanvas(), /Expected an Image or a Canvas/);
      assert.throws(() => ctx.fill(NaN), /Expected `fillRule`/);
      assert.throws(() => ctx.clip(NaN), /Expected `fillRule`/);
      assert.throws(() => ctx.stroke(NaN), /Expected a Path2D/);
      assert.throws(() => ctx.fill(NaN, "evenodd"), /Expected a Path2D/);
      assert.throws(() => ctx.clip(NaN, "evenodd"), /Expected a Path2D/);
      assert.throws(() => ctx.fill(p2d, {}), /Expected `fillRule`/);
      assert.throws(
        () => ctx.createTexture([1, NaN]),
        /Expected a number or array/,
      );
      assert.throws(
        () => ctx.createTexture(1, { path: null }),
        /Expected a Path2D/,
      );
      assert.throws(
        () => ctx.createTexture(20, { line: {} }),
        /Expected a number for `line`/,
      );
      assert.throws(
        () => ctx.createTexture(20, { angle: {} }),
        /Expected a number for `angle`/,
      );
      assert.throws(
        () => ctx.createTexture(20, { offset: {} }),
        /Expected a number or array/,
      );
      assert.throws(
        () => ctx.createTexture(20, { cap: {} }),
        /Expected a string/,
      );
      assert.throws(
        () => ctx.createTexture(20, { cap: "" }),
        /Expected "butt", "square"/,
      );
      assert.throws(
        () => ctx.createTexture(20, { offset: [1, NaN] }),
        /Expected a number or array/,
      );
      assert.throws(() => ctx.isPointInPath(0, 10, 10), /Expected `fillRule`/);
      assert.throws(
        () => ctx.isPointInPath(false, 10, 10),
        /Expected `fillRule`/,
      );
      assert.throws(() => ctx.isPointInPath({}, 10, 10), /Expected `fillRule`/);
      assert.throws(
        () => ctx.isPointInPath({}, 10, 10, "___"),
        /Expected a Path2D/,
      );
      assert.throws(
        () => ctx.isPointInPath({}, 10, 10, "evenodd"),
        /Expected a Path2D/,
      );
      assert.throws(
        () => ctx.isPointInPath(10, 10, "___"),
        /Expected `fillRule`/,
      );
      assert.throws(
        () => ctx.isPointInPath(p2d, 10, 10, ""),
        /Expected `fillRule`/,
      );
      assert.throws(
        () => ctx.createLinearGradient(0, 0, NaN, 1),
        /Expected a number for/,
      );
      assert.throws(
        () => ctx.createRadialGradient(0, 0, NaN, 0, 0, 0),
        /Expected a number for/,
      );
      assert.throws(
        () => ctx.createConicGradient(0, NaN, 0),
        /Expected a number for/,
      );
      assert.throws(
        () => ctx.createPattern(img, "___"),
        /Expected `repetition`/,
      );
      assert.throws(() => g.addColorStop(NaN, "#000"), /Expected a number/);
      // A `SyntaxError` DOMException, which is what the Canvas standard
      // specifies for a stop colour it cannot parse and what Chrome raises.
      // The value is in the message now, so the pattern anchors on the part
      // that does not depend on what was passed.
      assert.throws(() => g.addColorStop(0, {}), {
        name: "SyntaxError",
        message: /could not be parsed as a color/,
      });
      assert.throws(() => ctx.setLineDash(NaN), /Value is not a sequence/);
    });

    test("the exception type follows the rule, not the site", async () => {
      // Four rules, recorded in AGENTS.md because nothing at a call site
      // recorded them and they drifted: a `DOMException` where the standard
      // names one, a `TypeError` for a value outside an enumeration or a
      // sequence of the wrong length, a `RangeError` for a number outside a
      // permitted set. Every row below is Chrome 148's class and name for the
      // same call, except `bitDepth`, which no browser has.
      const g = ctx.createLinearGradient(0, 0, 1, 1);

      // 1. The standard names the exception.
      [2, -1].forEach((offset) => {
        assert.throws(() => g.addColorStop(offset, "red"), {
          name: "IndexSizeError",
          // The offending value, which this was the only refusal in the
          // range family to omit.
          message: new RegExp(`\\(${offset}\\)`),
        });
      });
      assert.throws(() => g.addColorStop(0.5, "notacolor"), {
        name: "SyntaxError",
      });

      // 2. A value outside an enumeration. `chromaSampling` was the odd one
      // out of six such sites, raising a `RangeError` where the other four
      // raise this.
      assert.throws(() => new Canvas(4, 4, { colorSpace: "nope" }), TypeError);
      assert.throws(() => new Canvas(4, 4, { colorType: "nope" }), TypeError);
      assert.throws(
        () => canvas.toBuffer("avif", { chromaSampling: "4:1:1" }),
        TypeError,
      );

      // 3. A sequence of the wrong length. This one was a bare `Error`, which
      // gives calling code nothing to branch on at all.
      assert.throws(() => ImageFilter.MakeMatrixTransform([1, 2, 3]), {
        name: "TypeError",
        message: /got 3/,
      });

      // 4. A number outside a permitted set stays a `RangeError`: the
      // argument is a number and its value is wrong, which is the case
      // `RangeError` is for. Here so that a later pass at "consistency" has
      // to argue with the rule rather than quietly flatten it.
      assert.throws(() => canvas.toBuffer("avif", { bitDepth: 7 }), RangeError);
    });

    test("NaN arguments", async () => {
      // silently fail
      assert.doesNotThrow(() => ctx.setTransform({}));
      assert.doesNotThrow(() => ctx.setTransform(0, 0, 0, NaN, 0, 0));
      assert.doesNotThrow(() => ctx.translate(NaN, 0));
      assert.doesNotThrow(() => ctx.scale(NaN, 0));
      assert.doesNotThrow(() => ctx.rotate(NaN));
      assert.doesNotThrow(() => ctx.rect(0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.arc(0, 0, NaN, 0, 0));
      assert.doesNotThrow(() => ctx.arc(0, 0, NaN, 0, 0, false));
      assert.doesNotThrow(() => ctx.arc(0, 0, NaN, 0, 0, new Date()));
      assert.doesNotThrow(() => ctx.ellipse(0, 0, 0, NaN, 0, 0, 0));
      assert.doesNotThrow(() => ctx.moveTo(NaN, 0));
      assert.doesNotThrow(() => ctx.lineTo(NaN, 0));
      assert.doesNotThrow(() => ctx.arcTo(0, 0, 0, 0, NaN));
      assert.doesNotThrow(() => ctx.bezierCurveTo(0, 0, 0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.quadraticCurveTo(0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.conicCurveTo(0, 0, NaN, 0, 1));
      assert.doesNotThrow(() => ctx.roundRect(0, 0, 0, 0, NaN));
      assert.doesNotThrow(() => ctx.fillRect(0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.strokeRect(0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.clearRect(0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.fillText("text", 0, NaN));
      assert.doesNotThrow(() => ctx.fillText("text", 0, 0, NaN));
      assert.doesNotThrow(() => ctx.strokeText("text", 0, NaN));
      assert.doesNotThrow(() => ctx.strokeText("text", 0, 0, NaN));
      assert.doesNotThrow(() => ctx.setLineDash([NaN, 0, 0]));
      assert.doesNotThrow(() => ctx.outlineText("text", NaN));
      assert.doesNotThrow(() => ctx.drawImage(img, NaN, 0));
      assert.doesNotThrow(() => ctx.drawImage(img, 0, 0, NaN, 0));
      assert.doesNotThrow(() => ctx.drawImage(img, 0, 0, 0, 0, NaN, 0, 0, 0));
      assert.doesNotThrow(() => ctx.drawCanvas(canvas, NaN, 0));
      assert.doesNotThrow(() => ctx.drawCanvas(canvas, 0, 0, NaN, 0));
      assert.doesNotThrow(() =>
        ctx.drawCanvas(canvas, 0, 0, 0, 0, NaN, 0, 0, 0),
      );

      // no error, returns false
      assert.equal(ctx.isPointInPath(10, NaN, "evenodd"), false);
      assert.equal(ctx.isPointInPath(p2d, 10, NaN, "evenodd"), false);
      assert.equal(ctx.isPointInPath(p2d, 10), false);
      assert.equal(ctx.isPointInStroke(10, NaN), false);
      assert.equal(ctx.isPointInStroke(p2d, 10, NaN), false);
      assert.equal(ctx.isPointInStroke(p2d, 10), false);
    });
  });

  describe("textDecoration", () => {
    // Every form but `underline <color>` used to be discarded in silence:
    // the parser treated `currentColor` -- the value the shorthand yields
    // when no color is named -- as an unparseable color and dropped the
    // whole declaration. Nothing here was covered, so it shipped broken.
    let inked = () => {
      let { data } = ctx.getImageData(0, 0, WIDTH, HEIGHT),
        n = 0;
      for (let i = 3; i < data.length; i += 4) if (data[i] > 0) n++;
      return n;
    };

    // The underline for 24px text on a baseline at y=40 lands on rows 41-42
    // and peaks at alpha 191, so both the band and the threshold matter --
    // measured rather than assumed.
    let underlineColor = () => {
      let { data } = ctx.getImageData(0, 41, WIDTH, 6);
      for (let i = 0; i < data.length; i += 4) {
        if (data[i + 3] > 100) return [data[i], data[i + 1], data[i + 2]];
      }
      return null;
    };

    let drawText = (decoration) => {
      ctx.fillStyle = "red";
      ctx.font = "24px Helvetica";
      if (decoration) ctx.textDecoration = decoration;
      ctx.fillText("nnn", 10, 40);
      return inked();
    };

    test("defaults to none", () => {
      assert.equal(ctx.textDecoration, "none");
    });

    test("draws without an explicit color", () => {
      let plain = drawText(null);
      _each({ underline: 1, overline: 1, "line-through": 1 }, (_, line) => {
        ctx.clearRect(0, 0, WIDTH, HEIGHT);
        assert.ok(
          drawText(line) > plain,
          `${line} should add ink (got the same as undecorated)`,
        );
      });
    });

    test("inherits the fill color", () => {
      ctx.fillStyle = "red";
      ctx.font = "24px Helvetica";
      ctx.textDecoration = "underline";
      ctx.fillText("nnn", 10, 40);

      // "nnn" has no descender, so ink below the baseline is the underline.
      assert.deepEqual(underlineColor(), [255, 0, 0]);
    });

    test("honours an explicit color over the fill", () => {
      ctx.fillStyle = "red";
      ctx.font = "24px Helvetica";
      ctx.textDecoration = "underline blue";
      ctx.fillText("nnn", 10, 40);

      assert.deepEqual(underlineColor(), [0, 0, 255]);
    });

    test("round-trips the values it accepts", () => {
      _each(
        {
          underline: "underline",
          "underline wavy": "underline wavy",
          "underline currentColor": "underline currentColor",
          "line-through": "line-through",
          "underline red": "underline red",
        },
        (expected, input) => {
          ctx.textDecoration = input;
          assert.equal(ctx.textDecoration, expected);
        },
      );
    });

    test("ignores an unparseable color", () => {
      ctx.textDecoration = "underline red";
      ctx.textDecoration = "underline notacolor";
      assert.equal(
        ctx.textDecoration,
        "underline red",
        "a bad color leaves the previous decoration in place",
      );
    });

    test("styles the line differently from solid", () => {
      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      let solid = drawText("underline solid");
      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      let wavy = drawText("underline wavy");
      assert.notEqual(solid, wavy);
    });
  });

  describe("filter angles", () => {
    // The angle regex did not capture a leading sign, so `hue-rotate(-45deg)`
    // parsed as +45 and rotated the wrong way.
    let hueRotated = (angle) => {
      ctx.filter = `hue-rotate(${angle})`;
      ctx.fillStyle = "rgb(255,128,0)";
      ctx.fillRect(0, 0, 4, 4);
      return pixel(1, 1);
    };

    test("keeps a negative angle negative", () => {
      let negative = hueRotated("-45deg");
      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      let equivalent = hueRotated("315deg");
      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      let opposite = hueRotated("45deg");

      assert.deepEqual(
        negative,
        equivalent,
        "-45deg and 315deg are the same rotation",
      );
      assert.notDeepEqual(negative, opposite, "and are not +45deg");
    });

    test("drop-shadow takes its colour from either end", () => {
      // `<color>? && <length>{2,3}` -- Filter Effects 1. The parser used to
      // read exactly three lengths from the front and require a colour after
      // them, so four of these five were dropped while Chrome drew each one.
      _each(
        {
          "drop-shadow(2px 4px 6px red)": "drop-shadow(2px 4px 6px red)",
          "drop-shadow(red 2px 4px 6px)": "drop-shadow(2px 4px 6px red)",
          "drop-shadow(2px 4px red)": "drop-shadow(2px 4px 0px red)",
          "drop-shadow(red 2px 4px)": "drop-shadow(2px 4px 0px red)",
          "drop-shadow(2px 4px 6px)": "drop-shadow(2px 4px 6px black)",
        },
        (expected, spec) => {
          ctx.filter = "none";
          ctx.filter = spec;
          assert.equal(ctx.filter, expected, spec);
        },
      );
    });

    test("a drop-shadow whose colour will not parse is ignored", () => {
      // An unparseable colour used to be dropped on its own: the shadow
      // vanished from the render while the getter still named it, so
      // `ctx.filter` reported a filter nothing was drawing. An invalid
      // declaration leaves the previous one standing, which is what
      // `blur(NaN)` already did and what a browser does.
      for (const spec of [
        "drop-shadow(2px 4px 6px notacolour)",
        "drop-shadow(nonsense 2px 4px)",
      ]) {
        ctx.filter = "blur(1px)";
        ctx.filter = spec;
        assert.equal(ctx.filter, "blur(1px)", `${spec} should be ignored`);
      }
    });

    // The same rotation written four ways, so they must move a pixel to the
    // same place. Asserting only that each parsed accepts any non-`none`
    // answer, which leaves a wrong radians or turns factor -- or a sign --
    // invisible: the shape that let a wrong Lab white point ship for weeks
    // behind `notEqual(ctx.fillStyle, "#000000")`.
    //
    // `0.125turn` and `+45deg` are exactly 45 degrees; `0.7854rad` is
    // 45.0001, which is why the comparison allows one level a channel rather
    // than asserting equality. Nothing here needs a reference value: the
    // forms are checked against each other.
    const painted = () => {
      ctx.fillStyle = "rgb(255,128,0)";
      ctx.fillRect(0, 0, 4, 4);
      const px = pixel(1, 1);
      ctx.clearRect(0, 0, WIDTH, HEIGHT);
      return px;
    };

    const rotated = (angle) => {
      ctx.filter = "none";
      ctx.filter = `hue-rotate(${angle})`;
      assert.notEqual(ctx.filter, "none", `${angle} should parse`);
      return painted();
    };

    const unfiltered = () => {
      ctx.filter = "none";
      return painted();
    };

    // The anchor, without which the comparisons below are free: if
    // `hue-rotate` were ignored altogether, every angle would land on the
    // same pixel and holding the forms against each other would pass
    // forever. That the reference differs from the unfiltered fill is what
    // makes their agreement mean anything.
    const reference = () => {
      const at45 = rotated("45deg");
      assert.notDeepEqual(at45, unfiltered(), "hue-rotate moved the pixel");
      return at45;
    };

    const rotatesLike = (angle, expected, message) => {
      const px = rotated(angle);
      assert.ok(
        px.every((level, i) => Math.abs(level - expected[i]) <= 1),
        `${message}: ${px} against ${expected}`,
      );
    };

    test("accepts a leading plus and other units", () => {
      const at45 = reference();
      for (const angle of ["+45deg", "0.7854rad", "0.125turn"]) {
        rotatesLike(angle, at45, `${angle} rotates like 45deg`);
      }
    });

    // The pattern was unanchored, so it found an angle anywhere in the
    // string: `--45deg` matched the `-45deg` inside it and rotated -45
    // where a browser rejects the value outright. `[\d.]+` was too loose
    // as well. Every expectation here was read off Chrome.
    test("rejects what a browser rejects", () => {
      for (const angle of [
        "--45deg",
        "+-45deg",
        "5.deg",
        "4.5.6deg",
        "1e2.5deg",
        "45 deg",
        "45",
        "45px",
        "45Deg 90deg",
      ]) {
        ctx.filter = "none";
        ctx.filter = `hue-rotate(${angle})`;
        assert.equal(ctx.filter, "none", `${angle} should be refused`);
      }
    });

    // CSS units are case-insensitive and a browser takes every one of
    // these. The pattern carried the `i` flag already, but `parseAngle`
    // compared the captured unit as written, so a match fell through to
    // NaN and the whole filter was discarded.
    test("reads a unit in any case", () => {
      for (const [angle, same] of [
        ["45DEG", "45deg"],
        ["45Deg", "45deg"],
        ["45dEg", "45deg"],
        ["1TURN", "1turn"],
        ["0.5RAD", "0.5rad"],
        ["100GRAD", "100grad"],
      ]) {
        ctx.filter = "none";
        ctx.filter = `hue-rotate(${angle})`;
        assert.notEqual(ctx.filter, "none", `${angle} should parse`);

        ctx.filter = "none";
        ctx.filter = `hue-rotate(${angle})`;
        ctx.fillStyle = "rgb(255,128,0)";
        ctx.fillRect(0, 0, 4, 4);
        const upper = pixel(1, 1);
        ctx.clearRect(0, 0, WIDTH, HEIGHT);

        ctx.filter = "none";
        ctx.filter = `hue-rotate(${same})`;
        ctx.fillRect(0, 0, 4, 4);
        const lower = pixel(1, 1);
        ctx.clearRect(0, 0, WIDTH, HEIGHT);

        assert.deepEqual(upper, lower, `${angle} rotates like ${same}`);
      }
    });

    test("accepts what a browser accepts", () => {
      for (const angle of [
        "45deg",
        "-45deg",
        "+45deg",
        ".5deg",
        "1e2deg",
        "45deg ",
        " 45deg",
      ]) {
        ctx.filter = "none";
        ctx.filter = `hue-rotate(${angle})`;
        assert.notEqual(ctx.filter, "none", `${angle} should parse`);
      }

      // Acceptance is this test's subject, and acceptance alone is what let
      // the defect above through. Four of the seven are the same angle, so
      // they are also held to rotating alike; the other three are different
      // angles and have nothing to be compared against here.
      const at45 = reference();
      for (const angle of ["+45deg", "45deg ", " 45deg"]) {
        rotatesLike(angle, at45, `${angle} rotates like 45deg`);
      }
    });
  });
});

describe("drop-shadow", () => {
  // A zero length may be written without its unit, and a browser takes
  // `drop-shadow(20px 0 0 red)` -- which is how an offset shadow with no blur
  // is usually written. Requiring the unit did not just ignore the zero: the
  // length failed to parse, so the function failed, and the declaration was
  // discarded whole. `ctx.filter` read back `"none"` after being set to a
  // shadow, so nothing was drawn and nothing said why.
  const W = 300,
    H = 60;

  function painted(spec, draw) {
    let canvas = new Canvas(W, H);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, W, H);
    if (spec) ctx.filter = spec;
    draw(ctx);
    return ctx;
  }

  // Columns along the middle row holding anything but white.
  function inkWidth(ctx) {
    let { data } = ctx.getImageData(0, H / 2, W, 1);
    let first = -1,
      last = -1;
    for (let x = 0; x < W; x++) {
      let at = x * 4;
      if (data[at] < 250 || data[at + 1] < 250 || data[at + 2] < 250) {
        if (first < 0) first = x;
        last = x;
      }
    }
    return first < 0 ? 0 : last - first + 1;
  }

  const at = (ctx, x) => Array.from(ctx.getImageData(x, H / 2, 1, 1).data);
  const box = (ctx) => {
    ctx.fillStyle = "black";
    ctx.fillRect(30, 10, 40, 40);
  };

  test("a shadow offset by a bare zero is still a shadow", () => {
    let plain = painted(null, box);
    let shadowed = painted("drop-shadow(20px 0 0 #f00)", box);
    assert.equal(inkWidth(plain), 40, "the shape alone");
    assert.equal(
      inkWidth(shadowed),
      60,
      "the shape plus 20 pixels of shadow beside it",
    );
    assert.deepEqual(
      at(shadowed, 80),
      [255, 0, 0, 255],
      "the shadow is the colour it was given",
    );
  });

  test("a bare zero does not take the declaration down with it", () => {
    // The failure was at the parser, so the property itself is worth
    // asserting: a rejected length discarded the whole function, and in a
    // chain it discarded that function alone while the rest stood.
    let ctx = new Canvas(10, 10).getContext("2d");
    ctx.filter = "drop-shadow(20px 0 0 #f00)";
    assert.notEqual(ctx.filter, "none", "the shadow parses");
    ctx.filter = "none";
    ctx.filter = "blur(3px) drop-shadow(20px 0 0 #f00)";
    assert.match(ctx.filter, /drop-shadow/, "and survives in a chain");
    assert.match(ctx.filter, /blur/, "beside the function it is chained to");
  });

  test("a bare zero is a length and an angle, and only zero is", () => {
    // `blur(5)` is not a length and a browser refuses it too. Widening the
    // parser past zero would accept what nothing else accepts.
    let ctx = new Canvas(10, 10).getContext("2d");
    const reads = (spec) => {
      ctx.filter = "none";
      ctx.filter = spec;
      return ctx.filter;
    };
    for (const spec of ["blur(0)", "hue-rotate(0)", "blur(-0)", "blur(0.0)"]) {
      assert.notEqual(reads(spec), "none", `${spec} is valid CSS`);
    }
    for (const spec of ["blur(5)", "hue-rotate(45)", "drop-shadow(20 0 red)"]) {
      assert.equal(reads(spec), "none", `${spec} is not`);
    }
  });

  test("the offset, the blur and the colour each reach the output", () => {
    let far = painted("drop-shadow(40px 0 0 #f00)", box);
    assert.equal(inkWidth(far), 80, "a larger offset moves the shadow further");

    let soft = painted("drop-shadow(20px 0 8px #f00)", box);
    assert.ok(
      inkWidth(soft) > inkWidth(painted("drop-shadow(20px 0 0 #f00)", box)),
      "a blur radius spreads the shadow beyond a hard one",
    );

    let green = painted("drop-shadow(20px 0 0 #0f0)", box);
    assert.deepEqual(at(green, 80), [0, 255, 0, 255], "the colour is used");
  });

  test("the shadow is cast from the drawn alpha, not from its bounding box", () => {
    // A circle has to cast a circle. Taking the shadow from the draw's box
    // would ink the corners, which the shape itself never touches.
    let ctx = painted("drop-shadow(60px 0 0 #f00)", (c) => {
      c.beginPath();
      c.arc(60, 30, 25, 0, Math.PI * 2);
      c.fillStyle = "black";
      c.fill();
    });
    assert.deepEqual(
      at(ctx, 120),
      [255, 0, 0, 255],
      "the shadow is inked at the circle's centre line",
    );
    // The corner of the shadow's bounding box, which a circle does not reach.
    let corner = Array.from(ctx.getImageData(96, 6, 1, 1).data);
    assert.deepEqual(corner, [255, 255, 255, 255], "and not at its corner");
  });

  test("an image casts a shadow of the shape it actually paints", () => {
    let source = new Canvas(60, 60);
    source.gpu = false;
    let sctx = source.getContext("2d");
    sctx.beginPath();
    sctx.arc(30, 30, 25, 0, Math.PI * 2);
    sctx.fillStyle = "black";
    sctx.fill();

    let ctx = painted("drop-shadow(60px 0 0 #f00)", (c) =>
      c.drawImage(source, 30, 0),
    );
    assert.deepEqual(at(ctx, 120), [255, 0, 0, 255], "the disc casts a shadow");
    let corner = Array.from(ctx.getImageData(96, 6, 1, 1).data);
    assert.deepEqual(
      corner,
      [255, 255, 255, 255],
      "the transparent corner of the image casts none",
    );
  });
});

describe("a CSS blur is the same width whatever it is drawing", () => {
  // `filter: blur(<length>)` gives the standard deviation directly -- Filter
  // Effects says so, and Chrome renders a geometry draw and an image draw
  // identically through one. This crate had two conversions: geometry passed
  // the length to a mask filter as its sigma, and anything going through an
  // image took `value / 2`, which is the `box-shadow` convention and belongs
  // only to `shadowBlur`. An image blurred at half the radius asked for.
  //
  // Measured as the width of the blurred edge rather than by comparing
  // pixels, because the two draws do not produce identical rasters even when
  // they agree -- one is a coverage mask and the other a filtered bitmap.
  // Spread is what the bug moved, and by a factor of two.
  const W = 240,
    H = 40;

  // How many columns along the middle row are neither white nor fully black:
  // the width of the blurred edge, in device pixels.
  function spread(ctx) {
    let { data } = ctx.getImageData(0, H / 2, W, 1);
    let first = -1,
      last = -1;
    for (let x = 0; x < W; x++) {
      if (data[x * 4] < 250) {
        if (first < 0) first = x;
        last = x;
      }
    }
    return first < 0 ? 0 : last - first + 1;
  }

  // Within a pixel of each other. The two draws do not rasterize identically
  // even when they agree on the radius -- one blurs a coverage mask, the other
  // a bitmap -- and the edge lands a pixel apart at some radii: 51 against 52
  // at 6px. The defect this guards moved the spread by a factor of two, 46
  // against 52 and 52 against 63, so a pixel of slack costs nothing and
  // asserting equality only produces a test that fails for the wrong reason.
  function assertSpread(actual, expected, message) {
    assert.ok(
      Math.abs(actual - expected) <= 1,
      `${message}: ${actual} against ${expected}`,
    );
  }

  // A white strip with `draw` performed on it under `blur(radius)`.
  function blurred(radius, draw, transform) {
    let canvas = new Canvas(W, H);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, W, H);
    if (transform) transform(ctx);
    ctx.filter = `blur(${radius}px)`;
    draw(ctx);
    return spread(ctx);
  }

  // A black square, on its own canvas, with `pad` of transparency around it.
  function square(pad = 40) {
    let off = new Canvas(40 + pad * 2, 40 + pad * 2);
    off.gpu = false;
    let octx = off.getContext("2d");
    octx.fillStyle = "black";
    octx.fillRect(pad, pad, 40, 40);
    return { off, pad };
  }

  // The reference every case is measured against: the same edge, same radius,
  // drawn as geometry.
  const asGeometry = (radius, transform) =>
    blurred(
      radius,
      (ctx) => {
        ctx.fillStyle = "black";
        ctx.fillRect(0, 0, 40, 40);
      },
      transform,
    );

  for (const radius of [3, 6, 12]) {
    test(`drawImage matches fillRect at ${radius}px`, () => {
      let { off, pad } = square();
      let image = blurred(radius, (ctx) => ctx.drawImage(off, -pad, -pad));
      assertSpread(image, asGeometry(radius), "drawImage against fillRect");
    });

    test(`drawImage with source and destination rects matches at ${radius}px`, () => {
      // The nine-argument form takes a different path -- an image and two
      // rects rather than a whole canvas -- and reads the same filter.
      let { off, pad } = square();
      let image = blurred(radius, (ctx) =>
        ctx.drawImage(off, pad, pad, 40, 40, 0, 0, 40, 40),
      );
      assertSpread(image, asGeometry(radius), "nine-argument drawImage");
    });

    test(`a repeating pattern fill matches at ${radius}px`, () => {
      // A pattern is a shader on an ordinary fill, so it takes the geometry
      // conversion rather than the image one. Asserted rather than assumed:
      // it is an image being drawn, which is what the broken branch keyed on.
      //
      // Repeating, because `"no-repeat"` measures something else. A coverage
      // blur cannot spread a fill past where its shader paints, so a
      // non-repeating pattern exactly covering its own rect stays hard-edged
      // whatever the radius -- 40 pixels at 12px and still 40 at 30px. That
      // is a separate defect from this one and is not what this test is for.
      let { off } = square(0);
      let image = blurred(radius, (ctx) => {
        ctx.fillStyle = ctx.createPattern(off, "repeat");
        ctx.fillRect(0, 0, 40, 40);
      });
      assertSpread(image, asGeometry(radius), "a repeating pattern");
    });
  }

  test("a pattern's own detail blurs, not just its outline", () => {
    // The sharpest form of the question. A blur applied to a shape's coverage
    // never touches the paint inside it, so a pattern of hard stripes came out
    // byte-identical to no blur at all -- the silhouette softened and every
    // stripe edge stayed razor sharp. A browser blurs the drawn result, stripes
    // and all.
    let source = new Canvas(20, 20);
    source.gpu = false;
    let sctx = source.getContext("2d");
    sctx.fillStyle = "white";
    sctx.fillRect(0, 0, 20, 20);
    sctx.fillStyle = "black";
    sctx.fillRect(0, 0, 10, 20);

    let striped = (radius) => {
      let canvas = new Canvas(W, H);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, W, H);
      if (radius) ctx.filter = `blur(${radius}px)`;
      ctx.fillStyle = ctx.createPattern(source, "repeat");
      ctx.fillRect(0, 0, W, H);
      let { data } = ctx.getImageData(0, H / 2, W, 1);
      // Columns spanning one stripe edge, well inside the fill.
      return Array.from({ length: 8 }, (_, i) => data[(i + 44) * 4]);
    };

    let sharp = striped(0);
    let soft = striped(6);
    assert.notDeepEqual(
      soft,
      sharp,
      `a blurred pattern must not match an unblurred one: ${soft}`,
    );
    // Every sampled column sits strictly between the two stripe colours once
    // the edge has been blurred across them.
    assert.ok(
      soft.every((level) => level > 0 && level < 255),
      `the stripe edge is a ramp, not a step: ${soft}`,
    );
  });

  test("a gradient's hard stop softens", () => {
    // The same defect reached gradients, where it is easier to miss: a smooth
    // ramp looks much the same blurred or not. A stop with no transition has
    // nowhere to hide.
    let atStop = (radius) => {
      let canvas = new Canvas(W, H);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, W, H);
      if (radius) ctx.filter = `blur(${radius}px)`;
      let ramp = ctx.createLinearGradient(0, 0, W, 0);
      ramp.addColorStop(0, "black");
      ramp.addColorStop(0.5, "black");
      ramp.addColorStop(0.5, "white");
      ramp.addColorStop(1, "white");
      ctx.fillStyle = ramp;
      ctx.fillRect(0, 0, W, H);
      let { data } = ctx.getImageData(0, H / 2, W, 1);
      let mid = W / 2;
      return [data[(mid - 3) * 4], data[(mid + 2) * 4]];
    };

    assert.deepEqual(atStop(0), [0, 255], "unblurred, the stop is a step");
    let [before, after] = atStop(8);
    assert.ok(
      before > 0 && after < 255,
      `blurred, the stop is a ramp: ${before} then ${after}`,
    );
  });

  for (const repeat of ["no-repeat", "repeat-y"]) {
    test(`a ${repeat} pattern spreads as far as a solid fill`, () => {
      // A coverage blur can only spread a fill where its shader paints, and
      // neither of these paints outside the source horizontally -- so the fill
      // kept a hard edge at 40 pixels whatever the radius, against 62 for the
      // same shape filled with a colour. `repeat-x` did not, which is what
      // identified the cause.
      let { off } = square(0);
      let image = blurred(12, (ctx) => {
        ctx.fillStyle = ctx.createPattern(off, repeat);
        ctx.fillRect(0, 0, 40, 40);
      });
      assertSpread(image, asGeometry(12), `a ${repeat} pattern`);
    });
  }

  test("the radius is not read as a diameter", () => {
    // The failure was exactly a factor of two, so "both paths agree" is worth
    // little on its own -- halving both would still pass. This pins the
    // absolute: an image at radius r must not match geometry at r/2.
    let { off, pad } = square();
    let image = blurred(12, (ctx) => ctx.drawImage(off, -pad, -pad));
    assert.notEqual(
      image,
      asGeometry(6),
      "an image at 12px must not blur like geometry at 6px",
    );
  });

  test("a non-uniform scale reaches both the same way", () => {
    // Both conversions mean to produce a device-space sigma -- the mask filter
    // by not respecting the CTM, the image filter by dividing the length by
    // the scale -- so a transform that differs per axis has to leave them
    // agreeing. A fix that dropped the divisor instead of the factor of two
    // would pass every case above and fail this one.
    let { off, pad } = square();
    let stretch = (ctx) => ctx.scale(2, 3);
    let image = blurred(12, (ctx) => ctx.drawImage(off, -pad, -pad), stretch);
    assertSpread(image, asGeometry(12, stretch), "under scale(2, 3)");
  });

  test("padding around the source does not change the answer", () => {
    // Rules out the other explanation for a narrower blur: that the tail is
    // being cropped at the source's edge rather than the radius being wrong.
    let widths = [0, 6, 18, 40].map((pad) => {
      let { off } = square(pad);
      return blurred(12, (ctx) => ctx.drawImage(off, -pad, -pad));
    });
    assert.equal(
      new Set(widths).size,
      1,
      `every padding gives the same spread: ${widths}`,
    );
  });
});

describe("imageSmoothingQuality", () => {
  // "high" follows Chrome, the only engine besides Safari that implements this
  // property at all (Firefox has none, and the HTML spec mandates no algorithm).
  // Chrome picks the sampler from the device-space scale — Mitchell bicubic for a
  // strict upscale, trilinear otherwise — so "high" beats "medium" when magnifying
  // without giving up the mipmap chain that keeps minification from aliasing.
  let noise = async (size) => {
    let canvas = new Canvas(size, size);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d"),
      data = ctx.createImageData(size, size),
      k = 7;
    for (let i = 0; i < data.data.length; i += 4) {
      k = (k * 1103515245 + 12345) & 0x7fffffff;
      let v = k % 256;
      data.data[i] = v;
      data.data[i + 1] = (v * 3) % 256;
      data.data[i + 2] = (v * 7) % 256;
      data.data[i + 3] = 255;
    }
    ctx.putImageData(data, 0, 0);
    return loadImage(await canvas.toBuffer("png"));
  };

  let render = (img, quality, { ctm = 1, dst = 128, size = 256 } = {}) => {
    let canvas = new Canvas(size, size);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = quality;
    ctx.scale(ctm, ctm);
    ctx.drawImage(img, 0, 0, dst, dst);
    return ctx.getImageData(0, 0, size, size).data;
  };

  let differing = (a, b) => {
    let n = 0;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) n++;
    return n;
  };

  test("uses a sharper sampler than medium when magnifying", async () => {
    let img = await noise(8);
    assert.ok(
      differing(
        render(img, "medium", { dst: 128 }),
        render(img, "high", { dst: 128 }),
      ) > 0,
      "high should differ from medium on an upscale",
    );
  });

  test("matches medium when minifying, so it does not lose the mipmaps", async () => {
    // A cubic resampler makes Skia ignore the mipmap chain, which aliases badly
    // on heavy downscales. Chrome only reaches for cubic when magnifying.
    let img = await noise(256);
    assert.equal(
      differing(
        render(img, "medium", { dst: 32, size: 64 }),
        render(img, "high", { dst: 32, size: 64 }),
      ),
      0,
      "high should fall back to the mipmapped sampler when minifying",
    );
  });

  test("magnifies with a cubic that does not ring", async () => {
    // The three tests around this pin *sharper when magnifying, mipmapped
    // when minifying*, and none of them pins which sharper sampler. Swapping
    // `CubicResampler::mitchell()` for `catmull_rom()` at
    // `src/node/filter.rs:592` left every one of them passing.
    //
    // Mitchell (B=C=1/3) is approximating and CatmullRom (B=0, C=1/2) is
    // interpolating, so a hard step separates them by how far each overshoots
    // its endpoints. Measured on this exact case, both engines, and the
    // separation does not depend on either:
    //
    //     step   Mitchell   CatmullRom
    //     128    5 levels   9-10
    //     192    7 levels   14
    //
    // Ten is the threshold because it sits between 7 and 14 with room on both
    // sides. It pins a property rather than the kernel's identity -- another
    // approximating cubic would pass -- which is the cheap half of the
    // roughness measurement AGENTS.md records for this choice, and it is the
    // half that catches the swap.
    const LO = 32,
      HI = 224,
      RING = 10;

    const source = new Canvas(8, 1);
    source.gpu = false;
    const src = source.getContext("2d");
    src.fillStyle = `rgb(${LO},${LO},${LO})`;
    src.fillRect(0, 0, 4, 1);
    src.fillStyle = `rgb(${HI},${HI},${HI})`;
    src.fillRect(4, 0, 4, 1);

    const canvas = new Canvas(128, 8);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(source, 0, 0, 128, 8);

    const px = ctx.getImageData(0, 0, 128, 8).data;
    let min = 255,
      max = 0;
    for (let x = 0; x < 128; x++) {
      const v = px[(4 * 128 + x) * 4];
      if (v < min) min = v;
      if (v > max) max = v;
    }

    // The levels either side of the step, so a failure says which direction
    // rang and by how much rather than only that something moved.
    assert.ok(
      LO - min <= RING,
      `undershoot below ${LO} stays within ${RING} levels: ${LO - min}`,
    );
    assert.ok(
      max - HI <= RING,
      `overshoot above ${HI} stays within ${RING} levels: ${max - HI}`,
    );
  });

  test("decides from the device-space scale, not the drawImage arguments", async () => {
    // Identical drawImage arguments in all three; only the transform differs.
    let img = await noise(64);
    assert.ok(
      differing(
        render(img, "medium", { ctm: 2 }),
        render(img, "high", { ctm: 2 }),
      ) > 0,
      "CTM 2 magnifies 64 -> 256, so high should use the cubic sampler",
    );
    assert.equal(
      differing(
        render(img, "medium", { ctm: 0.25 }),
        render(img, "high", { ctm: 0.25 }),
      ),
      0,
      "CTM 0.25 shrinks 64 -> 32, so high should stay mipmapped",
    );
  });

  test("still round-trips the property", () => {
    let ctx = new Canvas(10, 10).getContext("2d");
    assert.equal(ctx.imageSmoothingQuality, "low");
    for (let q of ["low", "medium", "high"]) {
      ctx.imageSmoothingQuality = q;
      assert.equal(ctx.imageSmoothingQuality, q);
    }
  });
});

describe("measureText's return shape", () => {
  // The metrics used to cross the binding as a JSON string that the wrapper
  // parsed. They cross as an object now -- about 40 µs of the call's 73 was
  // serialising and reparsing them, more than the typesetting they report --
  // so what needs pinning is that nothing about the shape moved with it.
  const measured = () => {
    let canvas = new Canvas(200, 100);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.font = "16px Helvetica";
    return ctx;
  };

  test("vertical metrics come from hhea on every platform", () => {
    // The value a caller builds a line box from, and it has to be the same
    // everywhere or a layout computed on one machine is wrong on another.
    // Skia reaches fonts through CoreText on macOS, FreeType on Linux and
    // DirectWrite on Windows, and those do not have to agree about which
    // table a font's vertical metrics come from -- a browser on macOS
    // answers 0.9199em for Helvetica's ascent, which is no table in the
    // file at all.
    //
    // Two fonts, because one cannot isolate the source. Amstelvar's hhea
    // and usWin agree with each other and differ from sTypo; Oswald's hhea
    // and sTypo agree and differ from usWin. Only hhea satisfies both, so
    // the pair pins the answer where either alone leaves two candidates.
    let read = (file) => {
      let buf = fs.readFileSync(file);
      let u16 = (o) => buf.readUInt16BE(o),
        i16 = (o) => buf.readInt16BE(o);
      let dir = {};
      for (let i = 0, n = u16(4); i < n; i++) {
        let rec = 12 + i * 16;
        dir[buf.toString("ascii", rec, rec + 4).trim()] = buf.readUInt32BE(
          rec + 8,
        );
      }
      let upem = u16(dir.head + 18);
      return {
        hhea: [i16(dir.hhea + 4) / upem, i16(dir.hhea + 6) / upem],
        typo: [i16(dir["OS/2"] + 68) / upem, i16(dir["OS/2"] + 70) / upem],
        win: [u16(dir["OS/2"] + 74) / upem, -u16(dir["OS/2"] + 76) / upem],
      };
    };

    for (let file of [
      "tests/assets/fonts/AmstelvarAlpha-VF.ttf",
      "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
    ]) {
      let loaded = FontLibrary.use(file),
        family = (Array.isArray(loaded) ? loaded[0] : loaded).family,
        table = read(file),
        canvas = new Canvas(10, 10),
        ctx = canvas.getContext("2d");

      // A sweep rather than one size, so a backend that rounds to whole
      // pixels cannot land on the right ratio by accident at one of them.
      for (let px of [16, 64, 256, 1024]) {
        ctx.font = `${px}px "${family}"`;
        let m = ctx.measureText("Hxg");
        assert.ok(
          Math.abs(m.fontBoundingBoxAscent / px - table.hhea[0]) < 1e-3,
          `${family} at ${px}px: ascent ${(m.fontBoundingBoxAscent / px).toFixed(4)}em ` +
            `should be hhea's ${table.hhea[0].toFixed(4)} ` +
            `(sTypo ${table.typo[0].toFixed(4)}, usWin ${table.win[0].toFixed(4)})`,
        );
        assert.ok(
          Math.abs(-m.fontBoundingBoxDescent / px - table.hhea[1]) < 1e-3,
          `${family} at ${px}px: descent should be hhea's ${table.hhea[1].toFixed(4)}`,
        );
      }
    }
  });

  test("every documented field survives the crossing", () => {
    let m = measured().measureText("Hamburgefonstiv");
    for (const key of [
      "width",
      "actualBoundingBoxLeft",
      "actualBoundingBoxRight",
      "actualBoundingBoxAscent",
      "actualBoundingBoxDescent",
      "fontBoundingBoxAscent",
      "fontBoundingBoxDescent",
      "emHeightAscent",
      "emHeightDescent",
      "hangingBaseline",
      "alphabeticBaseline",
      "ideographicBaseline",
    ]) {
      assert.equal(typeof m[key], "number", `${key} should be a number`);
      assert.ok(Number.isFinite(m[key]), `${key} should be finite`);
    }
    assert.equal(m.constructor.name, "TextMetrics");
    assert.ok(m.width > 0);
  });

  test("the nested per-line detail crosses too", () => {
    // An array of objects, which is the part a hand-written converter is
    // most likely to flatten or drop.
    let m = measured().measureText("Hamburgefonstiv");
    assert.ok(Array.isArray(m.lines), "lines should be an array");
    assert.equal(m.lines.length, 1);
    let [line] = m.lines;
    for (const key of ["x", "y", "width", "height", "baseline"]) {
      assert.equal(typeof line[key], "number", `lines[0].${key}`);
    }
  });

  test("a zero edge stays positive zero", () => {
    // `0.0 - x` rather than `-x` in the Rust, because negating zero gives
    // negative zero and `Object.is` can see it where `===` cannot. A number
    // conversion is exactly where that could have been reintroduced.
    let m = measured().measureText("");
    assert.ok(
      !Object.is(m.actualBoundingBoxLeft, -0),
      "actualBoundingBoxLeft came back as -0",
    );
    assert.ok(
      !Object.is(m.actualBoundingBoxAscent, -0),
      "actualBoundingBoxAscent came back as -0",
    );
  });

  test("the per-run detail crosses, strings and absences included", () => {
    // A run reports the family it resolved to, which is a string and cannot
    // travel in a buffer of numbers, and two measurements the font may not
    // make at all. Both are the parts of the encoding with somewhere to go
    // wrong: a string taken out of step with the numbers beside it, or an
    // absence read back as the `NaN` that stands for it.
    const [line] = measured().measureText("Hamburgefonstiv").lines;
    assert.ok(Array.isArray(line.runs), "runs should be an array");
    assert.ok(line.runs.length >= 1, "and hold at least one run");

    for (const run of line.runs) {
      assert.equal(typeof run.family, "string", "runs[].family");
      assert.ok(run.family.length > 0, "runs[].family is named");
      for (const key of [
        "x",
        "y",
        "width",
        "height",
        "ascent",
        "descent",
        "capHeight",
        "xHeight",
      ]) {
        assert.equal(typeof run[key], "number", `runs[].${key}`);
        assert.ok(Number.isFinite(run[key]), `runs[].${key} is finite`);
      }
      for (const key of ["underline", "strikethrough"]) {
        assert.ok(
          run[key] === null || Number.isFinite(run[key]),
          `runs[].${key} is a number or null, got ${run[key]}`,
        );
      }
    }
  });

  test("more than one line reads back in order", () => {
    // Everything travels in one buffer with the line and run counts written
    // inline, so a cursor that advanced by the wrong amount shows up as a
    // later line reading an earlier one's tail. One line cannot catch that.
    const ctx = measured();
    ctx.textWrap = true;
    const text = "Hamburgefonstiv ".repeat(8);
    const m = ctx.measureText(text, 120);

    assert.ok(m.lines.length > 1, `expected a wrap, got ${m.lines.length}`);
    let above = -Infinity;
    let reached = 0;
    for (const line of m.lines) {
      assert.ok(line.y >= above, "lines come back top to bottom");
      above = line.y;
      assert.ok(line.endIndex > line.startIndex, "the line spans some text");
      assert.ok(line.runs.length >= 1, "and has a run in it");
      assert.ok(Number.isFinite(line.baseline), "with a real baseline");
      reached = Math.max(reached, line.endIndex);
    }
    assert.ok(reached >= text.trimEnd().length, "every character landed");
  });

  test("every field the binding publishes reaches the object", () => {
    // The reader is built from the table Rust publishes rather than from a
    // list repeated here, so what this catches is the buffer and the table
    // disagreeing about length: a cursor that runs past the end reads
    // `undefined`, and a field added to one table and not written reads the
    // next field's number.
    const fields = native.CanvasRenderingContext2D_textMetricsFields();
    const m = measured().measureText("Hamburgefonstiv");
    const check = (spec, value, what) => {
      for (const { name, kind } of spec) {
        if (kind === "family") assert.equal(typeof value[name], "string", what);
        else if (kind === "optional")
          assert.ok(
            value[name] === null || Number.isFinite(value[name]),
            `${what}.${name}`,
          );
        else assert.ok(Number.isFinite(value[name]), `${what}.${name}`);
      }
    };
    check(fields.metrics, m, "metrics");
    check(fields.line, m.lines[0], "line");
    check(fields.run, m.lines[0].runs[0], "run");
  });

  test("the properties are read-only, as TextMetrics defines them", () => {
    let m = measured().measureText("Hi"),
      before = m.width;
    try {
      m.width = 999;
    } catch {
      // Strict mode throws; sloppy mode ignores. Either is fine -- what
      // matters is that the value did not change.
    }
    assert.equal(m.width, before);
  });
});

describe("getImageData after a draw", () => {
  // A read is served from a CPU copy of the surface once a second read
  // arrives at the same state, because `Surface::read_pixels` on the GPU
  // flushes and waits for the device -- 154 µs against 7, flat against both
  // the rectangle and the canvas. The copy is what makes a repeated read
  // cheap and is also the only way this can go wrong: a draw between two
  // reads must throw it away, or the second read answers with the picture
  // before the draw. Run on both engines because only one of them caches.
  for (const gpu of [true, false]) {
    test(`a draw invalidates the readback cache (gpu=${gpu})`, () => {
      let canvas = new Canvas(64, 64);
      canvas.gpu = gpu;
      let ctx = canvas.getContext("2d"),
        at = (x, y) => [...ctx.getImageData(x, y, 1, 1).data].join(",");

      ctx.fillStyle = "red";
      ctx.fillRect(0, 0, 64, 64);
      // Three reads: the first goes direct, the second builds the copy, the
      // third is served from it. All three must agree.
      assert.equal(at(0, 0), "255,0,0,255", "first read");
      assert.equal(at(0, 0), "255,0,0,255", "second read");
      assert.equal(at(0, 0), "255,0,0,255", "third read");

      ctx.fillStyle = "lime";
      ctx.fillRect(0, 0, 64, 64);
      assert.equal(at(0, 0), "0,255,0,255", "read after a draw");
      assert.equal(at(0, 0), "0,255,0,255", "and again");

      // A partial draw, so a stale copy shows up as the wrong colour inside
      // the new rectangle while the outside stays correct.
      ctx.fillStyle = "blue";
      ctx.fillRect(0, 0, 32, 32);
      assert.equal(at(0, 0), "0,0,255,255", "inside the new rect");
      assert.equal(at(40, 40), "0,255,0,255", "outside it");
    });
  }

  test("a cached read still honours the rectangle it was given", () => {
    // Crops are served out of one copy, so an offset that was applied to the
    // surface read has to be applied to the copy too.
    let canvas = new Canvas(64, 64);
    canvas.gpu = true;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "black";
    ctx.fillRect(0, 0, 64, 64);
    ctx.fillStyle = "white";
    ctx.fillRect(32, 32, 32, 32);

    ctx.getImageData(0, 0, 1, 1);
    ctx.getImageData(0, 0, 1, 1); // the copy exists from here on
    assert.equal([...ctx.getImageData(0, 0, 1, 1).data].join(","), "0,0,0,255");
    assert.equal(
      [...ctx.getImageData(40, 40, 1, 1).data].join(","),
      "255,255,255,255",
    );
    let block = ctx.getImageData(30, 30, 4, 4);
    assert.equal(block.width, 4);
    assert.equal(block.height, 4);
    // Straddles the corner: the first pixel is black, the last is white.
    assert.equal([...block.data.slice(0, 4)].join(","), "0,0,0,255");
    assert.equal([...block.data.slice(-4)].join(","), "255,255,255,255");
  });
});

describe("the readback cache against every way pixels change", () => {
  // The cache is keyed on the layer count, which is what `update` itself
  // uses to decide what to replay. That holds only if every operation that
  // changes pixels also adds a layer -- so each of these draws, reads twice
  // to make sure the copy exists, mutates by a different route, and reads
  // again. A miss here is the wrong picture, not an error.
  const primed = (gpu = true) => {
    let canvas = new Canvas(64, 64);
    canvas.gpu = gpu;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 64, 64);
    ctx.getImageData(0, 0, 1, 1);
    ctx.getImageData(0, 0, 1, 1); // the copy exists from here
    return { canvas, ctx };
  };
  const at = (ctx, x = 0, y = 0) =>
    [...ctx.getImageData(x, y, 1, 1).data].join(",");

  test("clearRect is seen", () => {
    let { ctx } = primed();
    ctx.clearRect(0, 0, 64, 64);
    assert.equal(at(ctx), "0,0,0,0");
  });

  test("putImageData is seen", () => {
    let { ctx } = primed();
    let block = ctx.createImageData(4, 4);
    for (let i = 0; i < block.data.length; i += 4) {
      block.data[i + 2] = 255;
      block.data[i + 3] = 255;
    }
    ctx.putImageData(block, 0, 0);
    assert.equal(at(ctx), "0,0,255,255");
  });

  test("drawImage is seen", () => {
    let { ctx } = primed();
    let source = new Canvas(8, 8);
    source.gpu = false;
    let sctx = source.getContext("2d");
    sctx.fillStyle = "lime";
    sctx.fillRect(0, 0, 8, 8);
    ctx.drawImage(source, 0, 0);
    assert.equal(at(ctx), "0,255,0,255");
  });

  test("a resize is seen", () => {
    // `set_bounds` replaces the whole recorder, cache included, so this is
    // the path where the copy is dropped rather than invalidated.
    let { canvas, ctx } = primed();
    canvas.width = 32;
    assert.equal(at(ctx), "0,0,0,0", "a resize clears the canvas");
    ctx.fillStyle = "magenta";
    ctx.fillRect(0, 0, 32, 32);
    assert.equal(at(ctx), "255,0,255,255");
  });

  test("a draw inside save/restore is seen", () => {
    let { ctx } = primed();
    ctx.save();
    ctx.globalAlpha = 1;
    ctx.fillStyle = "black";
    ctx.fillRect(0, 0, 64, 64);
    ctx.restore();
    assert.equal(at(ctx), "0,0,0,255");
  });

  test("an export between two reads does not disturb the copy", () => {
    // Exports run on a rayon worker and go through `PageCache`, not through
    // this surface. What is asserted is that a read after one still answers
    // with the canvas as it stands.
    let { ctx, canvas } = primed();
    canvas.toBufferSync("png");
    assert.equal(at(ctx), "255,0,0,255");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, 64, 64);
    canvas.toBufferSync("png");
    assert.equal(at(ctx), "255,255,255,255");
  });
});

describe("bolder and lighter resolve against the inherited weight", () => {
  // CSS Fonts 4 section 2.2.1 defines both keywords relative to the inherited
  // `font-weight` and gives a table for the result. A canvas inherits nothing,
  // so the base is the property's initial value, `normal`, whose row maps
  // `bolder` to 700 and `lighter` to 100 -- which is what Chrome 148 answers.
  // We answered 800 and 300, one fixed step either side of 400.
  const FAMILY = "RelativeWeightVF";

  test("both keywords land on the row the specification gives for 400", () => {
    assert.equal(css.font("bolder 16px serif").weight, 700);
    assert.equal(css.font("lighter 16px serif").weight, 100);
  });

  test("the mapping is the specification's table, not a step", () => {
    // Only the 350-550 row is reachable through `ctx.font`, so the rest is
    // asserted directly. Every row is written out against CSS Fonts 4 section
    // 2.2.1 rather than derived, so a table that drifts into arithmetic fails
    // here: no offset reproduces these, since both ends saturate and 600 gives
    // 900 against 400.
    const table = [
      // inherited, bolder, lighter
      [50, 400, 50],
      [100, 400, 100],
      [300, 400, 100],
      [350, 700, 100],
      [400, 700, 100],
      [500, 700, 100],
      [550, 900, 400],
      [700, 900, 400],
      [750, 900, 700],
      [900, 900, 700],
      [1000, 1000, 700],
    ];

    for (let [inherited, bolder, lighter] of table) {
      assert.equal(
        css.relativeWeight("bolder", inherited),
        bolder,
        `bolder from ${inherited}`,
      );
      assert.equal(
        css.relativeWeight("lighter", inherited),
        lighter,
        `lighter from ${inherited}`,
      );
    }
  });

  test("the keyword picks a different face, not just a different number", () => {
    // The parse decides which face is drawn, so the keyword has to be measured
    // as ink rather than read back off `ctx.font`. Raleway is a `wght`
    // variable font, so 100 and 300 are genuinely different instances -- on a
    // family whose faces are 200 and 400 both would round to the same one and
    // this would pass without discriminating.
    FontLibrary.use(FAMILY, [
      "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    ]);

    const ink = (weight) => {
      let canvas = new Canvas(320, 60),
        ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, 320, 60);
      ctx.fillStyle = "black";
      ctx.font = `${weight} 30px ${FAMILY}`;
      ctx.fillText("Handgloves", 6, 42);
      let { data } = ctx.getImageData(0, 0, 320, 60),
        dark = 0;
      for (let i = 0; i < data.length; i += 4) if (data[i] < 128) dark++;
      return dark;
    };

    // The control: this family has to be able to tell the two weights apart at
    // all, or the assertions below hold for a family that renders one face.
    assert.notEqual(ink(100), ink(300), "100 and 300 render differently");
    assert.notEqual(ink(700), ink(800), "700 and 800 render differently");

    assert.equal(ink("lighter"), ink(100));
    assert.equal(ink("bolder"), ink(700));
  });
});

describe("a density-scaled read is bounded in device pixels", () => {
  // The crop handed to the page is in device pixels and the page's bounds are
  // in canvas units, so an early return comparing the two directly was
  // measuring different spaces. At density 2 on a 20-wide canvas a read at
  // x=10 has a crop starting at device 20, which misses unscaled bounds of 0
  // to 20 -- so it returned a zeroed buffer, while a read starting one pixel
  // to its left returned those same pixels correctly.
  const inked = (width, height, paint) => {
    let canvas = new Canvas(width, height),
      ctx = canvas.getContext("2d");
    ctx.fillStyle = "red";
    ctx.fillRect(...paint);
    return ctx;
  };

  // Which columns of the returned row carry paint, as a string, so a failure
  // shows where the ink was rather than only how much of it there was.
  const columns = (ctx, [x, y, w, h], density) => {
    let { data, width } = ctx.getImageData(x, y, w, h, { density });
    return [...data]
      .filter((_, i) => i % 4 == 3)
      .slice(0, width)
      .map((alpha) => (alpha ? 1 : 0))
      .join("");
  };

  test("a crop landing exactly on the ink returns it", () => {
    const ctx = inked(20, 4, [10, 0, 4, 4]);

    // The two controls. Both of these were correct while the read below was
    // empty, and they are what says the ink is present and reachable -- a
    // fix that returns nothing everywhere would pass the assertion below by
    // agreeing with a canvas that was never painted.
    assert.equal(
      columns(ctx, [0, 0, 20, 1], 2),
      "0000000000000000000011111111000000000000",
      "the whole row",
    );
    assert.equal(
      columns(ctx, [8, 0, 8, 1], 2),
      "0000111111110000",
      "a crop wider than the ink on both sides",
    );

    assert.equal(columns(ctx, [10, 0, 4, 1], 2), "11111111", "the ink itself");
  });

  test("at a density other than 2", () => {
    // 3 rather than 1.5: `getImageData` takes a whole number, so a fractional
    // density cannot reach this. What varying it rules out is a fix keyed on
    // one factor -- and the boundary moves with it, from x=10 at density 2 to
    // x=6.67 here, so this read starts past it where the one at density 2
    // does not.
    const ctx = inked(20, 4, [10, 0, 4, 4]);
    assert.equal(
      columns(ctx, [7, 0, 4, 1], 3),
      "000000000111",
      "device 21 to 33, with the ink from 30",
    );
  });

  test("on the vertical axis, on a canvas that is not square", () => {
    // The transposed twin: a 4x20 canvas fails the same way down the y axis,
    // so a fix that scaled one bound and not the other passes the rows above
    // and fails here.
    const ctx = inked(4, 20, [0, 10, 4, 4]);
    let { data, width, height } = ctx.getImageData(0, 10, 1, 4, { density: 2 });
    assert.deepEqual([width, height], [2, 8]);
    assert.equal(
      [...data].filter((_, i) => i % 4 == 3).filter((alpha) => alpha > 0)
        .length,
      16,
      "every pixel of the crop is inked",
    );
  });

  test("and a read wholly outside the canvas still returns zeroes", () => {
    // The early return this fixes is not removed, only measured in the right
    // space -- so a read past the scaled bounds still short-circuits rather
    // than rasterizing a page to find nothing.
    const ctx = inked(20, 4, [10, 0, 4, 4]);
    assert.equal(columns(ctx, [40, 0, 4, 1], 2), "00000000");
    assert.equal(columns(ctx, [-8, 0, 4, 1], 2), "00000000");
  });
});

describe("direction carries `inherit` rather than resolving it away", () => {
  // The Canvas standard makes `inherit` the initial value of the attribute
  // and a state it holds -- it names the surrounding document's direction,
  // which a canvas does not have. We reported `ltr` for it, so a fresh
  // context could not report the state it was actually in, and assigning
  // `inherit` was indistinguishable from assigning `ltr`.
  test("a fresh context reports inherit", () => {
    assert.equal(new Canvas(8, 8).getContext("2d").direction, "inherit");
  });

  test("an explicit direction replaces it, and inherit comes back", () => {
    let ctx = new Canvas(8, 8).getContext("2d");
    ctx.direction = "rtl";
    assert.equal(ctx.direction, "rtl");
    ctx.direction = "inherit";
    assert.equal(ctx.direction, "inherit");
  });

  test("an invalid value is ignored, which is not the same as inherit", () => {
    // Probed from `rtl` rather than from the initial state: from a context
    // reading `ltr`, an ignored value and a reset to the default are the
    // same observation, and WebIDL requires an unknown enum value to be
    // ignored.
    let ctx = new Canvas(8, 8).getContext("2d");
    ctx.direction = "rtl";
    ctx.direction = "sideways";
    assert.equal(ctx.direction, "rtl");
  });

  test("it takes part in save and restore", () => {
    let ctx = new Canvas(8, 8).getContext("2d");
    ctx.direction = "rtl";
    ctx.save();
    ctx.direction = "inherit";
    assert.equal(ctx.direction, "inherit");
    ctx.restore();
    assert.equal(ctx.direction, "rtl");
  });

  test("what is drawn does not move", () => {
    // `inherit` resolves to left-to-right for layout, so the keyword changes
    // what the attribute reports and nothing about the output. Measured as
    // ink rather than as a reported width, since the width is the quantity
    // the getter change could plausibly have disturbed.
    const ink = (direction) => {
      let canvas = new Canvas(120, 40),
        ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, 120, 40);
      ctx.fillStyle = "black";
      ctx.font = "20px Helvetica";
      if (direction) ctx.direction = direction;
      ctx.fillText("Handgloves", 4, 28);
      let { data } = ctx.getImageData(0, 0, 120, 40),
        dark = 0;
      for (let i = 0; i < data.length; i += 4) if (data[i] < 128) dark++;
      return dark;
    };

    assert.equal(ink(undefined), ink("inherit"), "inherit against untouched");
    assert.equal(ink("inherit"), ink("ltr"), "inherit against ltr");
  });
});

describe("the two roundRect entry points agree", () => {
  // The Canvas standard has one `roundRect`, reachable as a context method
  // and as a `Path2D` method, and a browser's two agree. Ours did not: the
  // context took Skia's default start corner (6 clockwise, 7 anticlockwise)
  // while `Path2D` pinned 0. That does not change the shape -- it changes
  // where the contour begins, so where a following segment attaches and
  // where a dash phase falls.
  const ink = (build, dash) => {
    let canvas = new Canvas(80, 80),
      ctx = canvas.getContext("2d");
    ctx.strokeStyle = "black";
    ctx.lineWidth = dash ? 2 : 1;
    if (dash) ctx.setLineDash([6, 6]);
    build(ctx);
    ctx.stroke();
    let { data } = ctx.getImageData(0, 0, 80, 80),
      lit = 0;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 128) lit++;
    return lit;
  };

  test("a following segment leaves from the same corner", () => {
    // The current point after the call. Measured as the stroked result
    // rather than read back, because the context has no path to serialise.
    assert.equal(
      ink((c) => {
        c.beginPath();
        c.roundRect(10, 10, 40, 30, 8);
        c.lineTo(60, 60);
      }),
      ink((c) => {
        let path = new Path2D();
        path.roundRect(10, 10, 40, 30, 8);
        path.lineTo(60, 60);
        c.stroke(path);
      }),
    );
  });

  test("a dash phase falls in the same place", () => {
    for (let radii of [8, [8, 4, 8, 4]]) {
      assert.equal(
        ink((c) => {
          c.beginPath();
          c.roundRect(10, 10, 40, 30, radii);
        }, true),
        ink((c) => {
          let path = new Path2D();
          path.roundRect(10, 10, 40, 30, radii);
          c.stroke(path);
        }, true),
        `radii ${JSON.stringify(radii)}`,
      );
    }
  });

  test("and the shape was never what differed", () => {
    // The control. This assertion held before the change as well, so it is
    // here to catch a fix that moved the outline rather than the phase.
    const filled = (build) => {
      let canvas = new Canvas(80, 80),
        ctx = canvas.getContext("2d");
      ctx.fillStyle = "black";
      build(ctx);
      let { data } = ctx.getImageData(0, 0, 80, 80),
        lit = 0;
      for (let i = 3; i < data.length; i += 4) if (data[i] > 128) lit++;
      return lit;
    };
    assert.equal(
      filled((c) => {
        c.beginPath();
        c.roundRect(10, 10, 40, 30, 8);
        c.fill();
      }),
      filled((c) => {
        let path = new Path2D();
        path.roundRect(10, 10, 40, 30, 8);
        c.fill(path);
      }),
    );
  });
});

describe("a gradient that paints nothing leaves the page alone", () => {
  // "Painting nothing" and "erasing what is already there" are the same
  // pixel on an empty canvas, and that is why `a degenerate gradient paints
  // nothing` could not see this. It fills a transparent page with these same
  // five shapes and expects transparent black, which is equally what both
  // answers give, so it passed against the defect throughout. Everything
  // here draws first, which is the only change that separates them.
  //
  // What went wrong: `Context2D::draw_path` discards the recorded content
  // rather than painting over it when a fill covers the whole page opaquely,
  // and a gradient answered that it was opaque whenever no stop of it was
  // translucent -- which a gradient with no stops satisfies with nothing to
  // check, and a degenerate one satisfies while its shader is transparent.
  // So the page was thrown away and then not painted over.
  //
  // Only the full-page fill was ever wrong, which is why the same shapes at
  // less than full width are here as well: they pin the route, and a fix
  // that stopped painting gradients altogether would pass the first test
  // and fail the third.

  const RED = [255, 0, 0, 255];

  const ramp = (g) => {
    g.addColorStop(0, "red");
    g.addColorStop(1, "blue");
    return g;
  };

  // Two opaque stops, so the only thing that can make these paint nothing
  // is their geometry.
  const paintsNothing = {
    "linear with no stops": (c) => c.createLinearGradient(0, 0, 4, 4),
    "radial with no stops": (c) => c.createRadialGradient(0, 0, 0, 4, 4, 4),
    "conic with no stops": (c) => c.createConicGradient(0, 4, 2),
    "linear with both ends at one point": (c) =>
      ramp(c.createLinearGradient(1, 1, 1, 1)),
    "radial with one centre and one radius": (c) =>
      ramp(c.createRadialGradient(2, 2, 3, 2, 2, 3)),
  };

  // Each is one clause away from a shape above: a ramp rather than no
  // stops, a circle that grows rather than one that does not, endpoints
  // apart rather than equal. The linear clause is exact equality, so a hair
  // of separation is a real and very steep gradient.
  const paints = {
    "an ordinary ramp": (c) => ramp(c.createLinearGradient(0, 0, 8, 0)),
    "a circle growing from a point": (c) =>
      ramp(c.createRadialGradient(4, 2, 0, 4, 2, 4)),
    "endpoints a hair apart": (c) =>
      ramp(c.createLinearGradient(1, 1, 1 + 1e-6, 1)),
    "a conic sweep": (c) => ramp(c.createConicGradient(0, 4, 2)),
  };

  // White under the whole page and a red square inside it, so a fill that
  // erases is visible twice over: once against the background and once
  // against geometry that a browser would keep.
  const painted = () => {
    const ctx = new Canvas(8, 4).getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, 8, 4);
    ctx.fillStyle = "red";
    ctx.fillRect(2, 1, 2, 2);
    return ctx;
  };

  const at = (ctx, x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data);

  test("a fill covering the whole page keeps what is under it", () => {
    _each(paintsNothing, (make, what) => {
      const ctx = painted();
      ctx.fillStyle = make(ctx);
      ctx.fillRect(0, 0, 8, 4);
      assert.deepEqual(at(ctx, 0, 0), WHITE, `${what}: the background`);
      assert.deepEqual(at(ctx, 2, 1), RED, `${what}: the square`);
    });
  });

  test("and so does one that covers less of it", () => {
    // Half the width and seven of the eight columns were both correct
    // throughout, so this fails only for a fix that reached wider than the
    // page-covering case did.
    _each(paintsNothing, (make, what) => {
      for (const width of [4, 7]) {
        const ctx = painted();
        ctx.fillStyle = make(ctx);
        ctx.fillRect(0, 0, width, 4);
        assert.deepEqual(at(ctx, 0, 0), WHITE, `${what} across ${width}`);
        assert.deepEqual(at(ctx, 2, 1), RED, `${what} across ${width}`);
      }
    });
  });

  test("a gradient that does paint still covers the page", () => {
    _each(paints, (make, what) => {
      const ctx = painted();
      ctx.fillStyle = make(ctx);
      ctx.fillRect(0, 0, 8, 4);
      const background = at(ctx, 0, 0),
        square = at(ctx, 2, 1);
      assert.notDeepEqual(background, WHITE, `${what} paints`);
      assert.notDeepEqual(square, RED, `${what} covers the square`);
      assert.equal(background[3], 255, `${what} paints it opaque`);
    });
  });
});

describe("gradient interpolation", () => {
  // Nothing in the suite read `interpolation` or `hueInterpolation` before
  // this block -- the two properties shipped, and are documented in
  // `docs/api/context.md`, with no test of any kind behind them.
  //
  // Every assertion here is against a painted midpoint rather than the
  // string that comes back out. A round trip cannot tell a working property
  // from one that stores the name and then interpolates in sRGB whatever it
  // was told, and that is the failure worth catching: the name is the part
  // that is obviously right.

  // Two independent reasons a pinned byte can differ between machines, and
  // both have to be closed or the exact assertions below are not portable.
  //
  // The first is the rounding tie -- see `no midpoint sits on a rounding
  // tie`. The second is the engine: a `Canvas` is GPU-backed by default,
  // and an exact byte has to come from one named rasteriser or it
  // describes only the machine that produced it. That is why everything
  // here goes through `raster`, and it stays true whether or not the two
  // engines currently agree -- see `these midpoints are the CPU
  // rasteriser's`, which is where the state of that difference is the
  // subject rather than the reason.
  //
  // The two hazards are not one wearing different clothes, and it matters
  // because closing either does nothing for the other. A tie is a value the
  // arithmetic puts exactly between two bytes; an engine difference is a
  // different value. Under the endpoints this block previously used,
  // `hsl` read 168.3652 on the CPU -- which rounds to 168 under any mode --
  // against a flat 169 on the GPU. So no choice of endpoints closes the
  // second one, and the tie test cannot see it.
  //
  // A float-typed canvas reads back exact integers when it is GPU-backed,
  // so the readback in `no midpoint sits on a rounding tie` measures
  // something real only on the CPU path. Another reason everything here
  // goes through `raster`.
  const raster = (width, height, options) => {
    const canvas = new Canvas(width, height, options);
    canvas.gpu = false;
    return canvas;
  };

  // Shared with `tests/gradient_interpolation.rs`, so a value that
  // disagrees between the two surfaces means something. Every channel sum
  // is even, so the sRGB midpoint is the exact integer `rgb(125 1 127)`.
  const FROM = "rgb(250 2 0)",
    TO = "rgb(0 0 254)";

  const midpoint = (space, hue, from = FROM, to = TO) => {
    const ctx = raster(9, 1).getContext("2d"),
      gradient = ctx.createLinearGradient(0, 0, 9, 0);
    if (space) gradient.interpolation = space;
    if (hue) gradient.hueInterpolation = hue;
    gradient.addColorStop(0, from);
    gradient.addColorStop(1, to);
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, 9, 1);
    return [...ctx.getImageData(4, 0, 1, 1).data];
  };

  // Red to blue, sampled halfway. `hsl` and `hwb` coincide on this pair and
  // that is not a defect: both endpoints are fully saturated pure hues, so
  // whiteness and blackness stay at zero and the two spaces have nothing
  // left to disagree about. `a saturated pair cannot separate hsl from hwb`
  // below is what separates them.
  const midpoints = {
    // Three of these sixteen cannot tell their space from another. Two are
    // necessary and one is a property of these endpoints:
    //
    //   destination = srgb          this canvas *is* sRGB; the P3 rows are
    //                               what separate them
    //   xyz, xyz-d50, xyz-d65
    //               = srgb-linear   exactly, for every input, because
    //                               interpolation is linear and so is the
    //                               transform between them
    //   hsl         = hwb           both ends leave whiteness and blackness
    //                               at zero; `a desaturated end separates
    //                               hsl from hwb` is what parts them
    destination: [125, 1, 127, 255],
    srgb: [125, 1, 127, 255],
    "srgb-linear": [184, 1, 187, 255],
    "display-p3": [125, 10, 144, 255],
    // The narrowest row here by a wide margin: read it against `srgb`
    // above -- 125,0,129 against 125,1,127, so one level of green and two
    // of blue. Those two rows are what the gap is, so it cannot go stale
    // on its own; if either moves, the assertion moves with it. It discriminates, but nothing rests on
    // that -- `a98-rgb is not sRGB` carries the claim, and does it by
    // counting divergence along the whole ramp, where these endpoints give
    // 98 of 101 pixels differing and a largest gap of 11. Those two figures
    // are measurements and not what that test asserts -- it takes 80 and 8,
    // deliberately loose, because the exact counts have no tie check behind
    // them and a level of platform drift in either should not be a failure.
    // So they can go stale without anything going red; re-measure rather
    // than trust them.
    "a98-rgb": [125, 0, 129, 255],
    "prophoto-rgb": [183, 4, 156, 255],
    rec2020: [159, 19, 147, 255],
    lab: [190, 0, 135, 255],
    oklab: [138, 82, 161, 255],
    xyz: [184, 1, 187, 255],
    "xyz-d50": [184, 1, 187, 255],
    "xyz-d65": [184, 1, 187, 255],
    hsl: [252, 0, 251, 255],
    hwb: [252, 0, 251, 255],
    lch: [242, 0, 132, 255],
    oklch: [184, 0, 191, 255],
  };

  test("defaults to the canvas's own space, with the shorter hue arc", () => {
    const gradient = raster(9, 1)
      .getContext("2d")
      .createLinearGradient(0, 0, 9, 0);
    // Reads back `"destination"`, not `"srgb"`. It reported `"srgb"` before
    // the two were separated, so a caller comparing against `"srgb"` sees
    // that comparison stop holding even though nothing they set changed.
    assert.equal(gradient.colorInterpolationSpace, "destination");
    assert.equal(gradient.interpolation, "destination");
    assert.equal(gradient.hueInterpolation, "shorter");
    // On an sRGB canvas the default's answer is the sRGB one, and this
    // canvas cannot tell `"destination"` from `"srgb"` -- that is what the
    // P3 rows below are for.
    assert.deepEqual(midpoint(null, null), midpoints.srgb);
  });

  test("each space paints its own midpoint", () => {
    _each(midpoints, (expected, space) =>
      assert.deepEqual(midpoint(space, null), expected, space),
    );
  });

  test("a desaturated end separates hsl from hwb", () => {
    // Both shared endpoints are fully saturated, so whiteness and blackness
    // stay at zero and the two spaces have nothing to disagree about.
    // Desaturating one end gives whiteness something to carry. Without this
    // the two rows above would pass for a binding resolving `hwb` to `hsl`.
    //
    // `red` to `silver` also parts them and is not used: its `hwb` midpoint
    // sits exactly on a rounding tie, and the two engines disagree on it.
    const GREY = "rgb(190 190 190)";
    assert.deepEqual(midpoint("hsl", null, FROM, GREY), [206, 110, 109, 255]);
    assert.deepEqual(midpoint("hwb", null, FROM, GREY), [220, 96, 95, 255]);
  });

  test("every declared space is accepted and round-trips", () => {
    // Three lists have to agree: the union in `lib/index.d.ts`, the array
    // the setter validates against, and `str_to_color_space` in the binding.
    // Nothing else checks that they do -- `check-dts-surface` compares which
    // members exist, not which values they accept -- and the pair inside the
    // binding has already drifted once, when closing the Rust enum made the
    // getter total and left the setter alone. The compiler enforced one half
    // of a pair that has to stay symmetrical and was silent about the other.
    //
    // A value the union declares but the binding refuses is a lie in the
    // types; a value that reads back as a different string means two names
    // share one variant. Setting each and reading it back catches both.
    const declared = fs
      .readFileSync(require.resolve("../../lib/index.d.ts"), "utf8")
      .match(/type GradientColorSpace =([^;]+);/)[1]
      .match(/"([a-z0-9-]+)"/g)
      .map((q) => q.slice(1, -1));

    // The regex is an instrument: if it silently matched the wrong block or
    // stopped matching, an empty or short list would pass everything below.
    assert.ok(declared.length >= 9, `parsed ${declared.length} names`);
    assert.ok(declared.includes("destination") && declared.includes("oklch"));

    const gradient = raster(9, 1)
      .getContext("2d")
      .createLinearGradient(0, 0, 9, 0);
    for (const space of declared) {
      gradient.colorInterpolationSpace = space;
      assert.equal(gradient.colorInterpolationSpace, space, space);
    }
  });

  test("these midpoints are the CPU rasteriser's, and the GPU keeps them apart", () => {
    // The other half of why a byte pinned here can differ between machines.
    // A `Canvas` is GPU-backed by default, CI has no GPU, and the two
    // rasterisers do not agree to the last level on every space -- so a
    // table measured through Metal and asserted on a Linux runner compares
    // two different implementations.
    //
    // The pinning is not justified by that difference existing today. Exact
    // bytes have to come from one named rasteriser whether or not the two
    // currently agree, or the table means nothing on a machine with
    // different hardware. So this stays even if the engines converge.
    assert.equal(raster(9, 1).gpu, false);

    // What the GPU is held to is not a set of bytes. Pinning those would
    // pin one vendor's arithmetic -- Metal here, Vulkan on a Linux box --
    // and asserting that the two engines *differ* would be worse still: it
    // would pin the defect, so that fixing the GPU shader failed the suite
    // and read as a regression.
    //
    // What holds on any backend is that a shader must not lose a
    // distinction the reference makes. Two interpolation spaces that paint
    // different colours on the CPU must not collapse into each other on the
    // GPU, whatever the last level does.
    const accelerated = new Canvas(9, 1);
    accelerated.gpu = true;
    // Assigning `true` falls back to raster in silence where there is no
    // GPU, so read it back rather than assume. That is CI, and there is
    // nothing to compare against there.
    if (!accelerated.gpu) return;

    const through = (canvas, space) => {
      const ctx = canvas.getContext("2d"),
        gradient = ctx.createLinearGradient(0, 0, 9, 0);
      gradient.colorInterpolationSpace = space;
      gradient.addColorStop(0, FROM);
      gradient.addColorStop(1, TO);
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, 9, 1);
      return [...ctx.getImageData(4, 0, 1, 1).data].join();
    };
    const spaces = Object.keys(midpoints),
      onGpu = Object.fromEntries(
        spaces.map((space) => {
          const canvas = new Canvas(9, 1);
          canvas.gpu = true;
          return [space, through(canvas, space)];
        }),
      ),
      onCpu = Object.fromEntries(
        spaces.map((space) => [space, through(raster(9, 1), space)]),
      );

    let compared = 0;
    for (const [i, one] of spaces.entries())
      for (const other of spaces.slice(i + 1)) {
        if (onCpu[one] === onCpu[other]) continue; // Agree by construction.
        compared++;
        assert.notEqual(
          onGpu[one],
          onGpu[other],
          `${one} and ${other} differ on the CPU and collapse on the GPU`,
        );
      }

    // A comparison that never fires proves nothing, and the pairs that
    // agree by construction are skipped above -- so check that a real
    // number of them were examined, and that the ones skipped were skipped
    // for the stated reason rather than by an error in the loop.
    assert.ok(compared > 100, `only ${compared} pairs compared`);
    assert.equal(onCpu.hsl, onCpu.hwb);
    assert.equal(onCpu.xyz, onCpu["srgb-linear"]);
  });

  test("no midpoint sits on a rounding tie", async () => {
    // The reason this exists: every exact value in this block is a byte, and
    // a byte is a rounded float. When the float lands exactly on `x.5` the
    // byte is decided by the rounding mode rather than by the arithmetic,
    // and that is not the same on every platform -- macOS rounded 127.5 up
    // and Linux rounded it down, which turned five of these tests red on CI
    // and green on every developer machine.
    //
    // Endpoints of `red` and `blue` guarantee that: 255 and 0 average to
    // exactly 127.5 in two channels. Nothing local could see it, because a
    // tie is perfectly stable on the platform you are standing on. What is
    // observable locally is the float *before* it is rounded, through a
    // float-typed canvas, and that is what this reads.
    //
    // So this is not a test of the gradient. It is a test of whether the
    // other tests in this block are asking a question the hardware can
    // answer the same way twice.
    const floats = async (space) => {
      const canvas = raster(9, 1, { colorType: "RGBAF32" }),
        ctx = canvas.getContext("2d"),
        gradient = ctx.createLinearGradient(0, 0, 9, 0);
      gradient.colorInterpolationSpace = space;
      gradient.addColorStop(0, FROM);
      gradient.addColorStop(1, TO);
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, 9, 1);
      const buffer = await canvas.raw,
        pixels = new Float32Array(
          buffer.buffer,
          buffer.byteOffset,
          buffer.length / 4,
        );
      return [...pixels.slice(16, 19)].map((v) => v * 255);
    };

    // Distance from the nearest rounding boundary: 0 is a tie, 0.5 is an
    // exact integer and as safe as a value can be. An exact tie is a coin
    // flip; anything else needs the two platforms' arithmetic to disagree by
    // that much in 255ths, which is far more than a different `pow` costs.
    // The tightest of the 48 values under these endpoints is 0.0875, at
    // `lch`'s green channel, so the bound below has room and still fails
    // loudly on a genuine tie. That figure is a measurement and not what
    // is asserted -- the bound is 0.02, and nothing checks the 0.0875 --
    // so re-measure it if the endpoints move rather than trusting it. It
    // said 0.0457 until this line was corrected, which was the tightest
    // value under the endpoints used before them.
    const clearance = (v) => Math.abs(v - Math.floor(v) - 0.5);

    for (const space of Object.keys(midpoints)) {
      const channels = await floats(space);
      for (const [i, value] of channels.entries())
        assert.ok(
          clearance(value) > 0.02,
          `${space}.${"RGB"[i]} is ${value}, ${clearance(value).toFixed(4)} from a rounding tie`,
        );
    }

    // The instrument has to be able to fail, and the endpoints this block
    // replaced are the case it was built for: red to blue puts two channels
    // on exactly 127.5. If this stops throwing, the float readback has
    // stopped reporting floats and every assertion above is vacuous.
    const canvas = raster(9, 1, { colorType: "RGBAF32" }),
      ctx = canvas.getContext("2d"),
      tied = ctx.createLinearGradient(0, 0, 9, 0);
    tied.addColorStop(0, "red");
    tied.addColorStop(1, "blue");
    ctx.fillStyle = tied;
    ctx.fillRect(0, 0, 9, 1);
    const buffer = await canvas.raw,
      red =
        new Float32Array(
          buffer.buffer,
          buffer.byteOffset,
          buffer.length / 4,
        )[16] * 255;
    assert.equal(red, 127.5);
    assert.equal(clearance(red), 0);
  });

  test("a98-rgb is not sRGB, at the midpoint and along the ramp", () => {
    // The table separates these at the midpoint -- see the `a98-rgb` and
    // `srgb` rows, which is where that gap is recorded -- but only because
    // of the endpoints, and narrowly. Red to blue, which this block used
    // two changes ago, collapsed them exactly: Adobe RGB 1998 shares sRGB's
    // red and blue primaries and white point, so a ramp between those two
    // exercises only the axes where the two spaces already agree.
    //
    // That is worth keeping a test for rather than trusting to the choice
    // of endpoints: the ramp differs along its whole length whatever pair
    // is used, because the transfer curves differ -- a plain 2.19921875
    // gamma against sRGB's piecewise one. Counting the divergence rather
    // than pinning a pixel of it, since the size of the gap varies along
    // the ramp and no single sample of it is a fact worth asserting.
    const ramp = (space) => {
      const ctx = raster(101, 1).getContext("2d"),
        gradient = ctx.createLinearGradient(0, 0, 101, 0);
      gradient.colorInterpolationSpace = space;
      gradient.addColorStop(0, FROM);
      gradient.addColorStop(1, TO);
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, 101, 1);
      return [...ctx.getImageData(0, 0, 101, 1).data];
    };
    const straight = ramp("srgb"),
      adobe = ramp("a98-rgb"),
      differing = Array.from({ length: 101 }, (_, i) =>
        Math.max(
          ...[0, 1, 2].map((c) =>
            Math.abs(straight[i * 4 + c] - adobe[i * 4 + c]),
          ),
        ),
      );

    assert.ok(
      differing.filter(Boolean).length > 80,
      `${differing.filter(Boolean).length} of 101 pixels differ`,
    );
    assert.ok(
      Math.max(...differing) >= 8,
      `largest gap ${Math.max(...differing)}`,
    );
  });

  test("the XYZ spaces are linear sRGB, and cannot be otherwise", () => {
    // Not a coincidence to be pinned at one pair and not an implementation
    // detail that might change: interpolation is a linear operation and the
    // transform between XYZ and linear sRGB is a linear map, so the two
    // commute for every input. A test looking for a pair that separates
    // them would be looking for something that cannot exist. Asserted over
    // several shapes so the claim is about the identity, not one sample.
    for (const [from, to] of [
      [FROM, TO],
      ["black", "white"],
      ["red", "lime"],
      ["#ff8000", "#0080ff"],
    ])
      for (const space of ["xyz", "xyz-d50", "xyz-d65"])
        assert.deepEqual(
          midpoint(space, null, from, to),
          midpoint("srgb-linear", null, from, to),
          `${space} on ${from} to ${to}`,
        );
  });

  test("the hue arc is chosen by direction, not by name", () => {
    // Two stops leave two arcs, so four names can only ever produce two
    // answers -- which means asserting that `longer` differs from `shorter`
    // proves almost nothing. What pins the semantics is *which* of
    // `increasing` and `decreasing` collapses onto `shorter`, and that it
    // swaps when the short way round changes direction.
    const arcs = (from, to) =>
      ["shorter", "longer", "increasing", "decreasing"].map((hue) =>
        midpoint("oklch", hue, from, to).join(),
      );

    // Red to blue is 235 degrees going up and 125 going down, so the short
    // way is decreasing.
    const [shorter, longer, increasing, decreasing] = arcs("red", "blue");
    assert.notEqual(shorter, longer);
    assert.equal(shorter, decreasing);
    assert.equal(longer, increasing);

    // Red to yellow is 81 degrees going up, and the pairing flips.
    const [shorterUp, longerUp, increasingUp, decreasingUp] = arcs(
      "red",
      "yellow",
    );
    assert.notEqual(shorterUp, longerUp);
    assert.equal(shorterUp, increasingUp);
    assert.equal(longerUp, decreasingUp);
  });

  test("a refused value leaves the previous one painting", () => {
    // An invalid value is substitutive -- there is no earlier setting to
    // fall back to that the caller did not ask for -- so it throws rather
    // than being ignored, and the gradient keeps rendering what it had.
    const ctx = raster(9, 1).getContext("2d"),
      gradient = ctx.createLinearGradient(0, 0, 9, 0);
    gradient.interpolation = "oklch";
    gradient.hueInterpolation = "longer";

    // `display-p3` and `rec2020` are spellings this library already accepts
    // for a canvas's own colour space. They are not interpolation spaces
    // here, and reaching for one is the mistake most likely to be made.
    for (const bad of ["specified", "oklab ", "OKLCH", "", "xyz-d55"])
      assert.throws(() => (gradient.interpolation = bad), TypeError, bad);
    for (const bad of ["nearest", "longer hue", "Shorter", ""])
      assert.throws(() => (gradient.hueInterpolation = bad), TypeError, bad);

    assert.equal(gradient.interpolation, "oklch");
    assert.equal(gradient.hueInterpolation, "longer");
  });

  test("the two properties are independent", () => {
    const ctx = raster(9, 1).getContext("2d"),
      gradient = ctx.createLinearGradient(0, 0, 9, 0);
    gradient.interpolation = "lab";
    assert.equal(gradient.hueInterpolation, "shorter");
    gradient.hueInterpolation = "longer";
    assert.equal(gradient.interpolation, "lab");
  });

  // `colorInterpolationSpace` and `hueInterpolationMethod` are the same two
  // settings under accurate names -- a space is not a method. The shipped
  // spellings are deprecated in the declarations and go on working, so
  // both pairs are exercised the same way: a deprecation that quietly
  // stopped working would be a breaking change wearing a doc tag.

  const spellings = {
    "as it shipped": ["interpolation", "hueInterpolation"],
    accurate: ["colorInterpolationSpace", "hueInterpolationMethod"],
  };

  test("both spellings reach the interpolation, not just the field", () => {
    // The whole point of painting rather than reading back: an alias that
    // stored the string and never reached Rust would pass a round trip and
    // fail here.
    _each(spellings, ([space, hue]) => {
      _each(midpoints, (expected, name) => {
        const ctx = raster(9, 1).getContext("2d"),
          gradient = ctx.createLinearGradient(0, 0, 9, 0);
        gradient[space] = name;
        gradient.addColorStop(0, FROM);
        gradient.addColorStop(1, TO);
        ctx.fillStyle = gradient;
        ctx.fillRect(0, 0, 9, 1);
        assert.deepEqual(
          [...ctx.getImageData(4, 0, 1, 1).data],
          expected,
          `${space} = ${name}`,
        );
      });

      // The hue name under the same spelling, painted as well: red to blue
      // ascends the long way round, so `"increasing"` has to move the pixel
      // off the `"shorter"` answer.
      const arc = (method) => {
        const ctx = raster(9, 1).getContext("2d"),
          gradient = ctx.createLinearGradient(0, 0, 9, 0);
        gradient[space] = "oklch";
        gradient[hue] = method;
        gradient.addColorStop(0, FROM);
        gradient.addColorStop(1, TO);
        ctx.fillStyle = gradient;
        ctx.fillRect(0, 0, 9, 1);
        return [...ctx.getImageData(4, 0, 1, 1).data].join();
      };
      assert.equal(arc("shorter"), midpoints.oklch.join(), hue);
      assert.notEqual(arc("increasing"), arc("shorter"), hue);
    });
  });

  test("the two spellings are one setting", () => {
    const gradient = raster(9, 1)
      .getContext("2d")
      .createLinearGradient(0, 0, 9, 0);

    // Written through either name, read back through both. There is one
    // accessor pair in the binding and one field behind it, so this is a
    // property of the shape rather than of two implementations agreeing.
    gradient.colorInterpolationSpace = "oklab";
    gradient.hueInterpolationMethod = "increasing";
    assert.equal(gradient.interpolation, "oklab");
    assert.equal(gradient.hueInterpolation, "increasing");

    gradient.interpolation = "lch";
    gradient.hueInterpolation = "longer";
    assert.equal(gradient.colorInterpolationSpace, "lch");
    assert.equal(gradient.hueInterpolationMethod, "longer");
  });

  test('"srgb" means sRGB, and "destination" means the canvas', () => {
    // This began as a tripwire for a change that had not landed: `"srgb"`
    // used to map to Skia's `Destination`, which follows the surface, so a
    // caller wrote `"srgb"` and did not get sRGB. The two are now separate
    // values and the numbers below are the ones that arrived. Keeping the
    // shape rather than replacing it, because what is worth pinning is the
    // same thing either way -- that the two names come apart on a canvas
    // that can tell them apart, and only there.
    const mid = (colorSpace, space) => {
      const ctx = raster(9, 1, { colorSpace }).getContext("2d"),
        gradient = ctx.createLinearGradient(0, 0, 9, 0);
      if (space) gradient.colorInterpolationSpace = space;
      gradient.addColorStop(0, FROM);
      gradient.addColorStop(1, TO);
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, 9, 1);
      return [...ctx.getImageData(4, 0, 1, 1).data];
    };

    // An sRGB canvas cannot separate them -- the canvas's space *is* sRGB.
    // Here as the control that says why the old conflation was invisible,
    // not as evidence that the two differ.
    assert.deepEqual(mid("srgb", "srgb"), midpoints.srgb);
    assert.deepEqual(mid("srgb", "destination"), midpoints.srgb);
    assert.deepEqual(mid("srgb", null), midpoints.srgb);

    // A P3 canvas separates them, which is the whole point of the split.
    assert.notDeepEqual(
      mid("display-p3", "srgb"),
      mid("display-p3", "destination"),
    );

    // `"destination"` carries the behaviour `"srgb"` used to have, so code
    // migrating one to the other renders identically. If this row ever
    // stops matching, the migration advice in `GradientColorSpace` is wrong.
    assert.deepEqual(mid("display-p3", "destination"), [115, 25, 139, 255]);
    assert.deepEqual(mid("display-p3", null), mid("display-p3", "destination"));

    // And `"srgb"` is now sRGB itself, converted into the canvas
    // afterwards. 114,20,123 is not a third behaviour: it is what a flat
    // fill of the sRGB midpoint reads back as on this canvas. Every channel
    // sum of these endpoints is even, so that midpoint is the exact integer
    // colour below rather than a fractional one.
    const flat = (css) => {
      const ctx = raster(9, 1, { colorSpace: "display-p3" }).getContext("2d");
      ctx.fillStyle = css;
      ctx.fillRect(0, 0, 9, 1);
      return [...ctx.getImageData(4, 0, 1, 1).data];
    };
    assert.deepEqual(mid("display-p3", "srgb"), [114, 20, 123, 255]);
    assert.deepEqual(mid("display-p3", "srgb"), flat("rgb(125 1 127)"));
  });

  test("both spellings refuse the same values", () => {
    _each(spellings, ([space, hue]) => {
      const gradient = raster(9, 1)
        .getContext("2d")
        .createLinearGradient(0, 0, 9, 0);
      gradient[space] = "oklch";
      gradient[hue] = "longer";
      // `specified` was in the proposal's straw man and its author removed
      // it, so it is the value most likely to be reached for in error.
      for (const bad of ["specified", "srgb-lienar", "SRGB", "lab ", ""])
        assert.throws(
          () => (gradient[space] = bad),
          TypeError,
          `${space} ${bad}`,
        );
      for (const bad of ["specified", "nearest", ""])
        assert.throws(() => (gradient[hue] = bad), TypeError, `${hue} ${bad}`);
      assert.equal(gradient[space], "oklch");
      assert.equal(gradient[hue], "longer");
    });
  });

  describe("alpha interpolation", () => {
    // The third of the trio, and the one that did not exist until now: the
    // crate has carried `AlphaInterpolation` since rust-v0.15.0 and the
    // binding hard-coded `InPremul::No`, so a JavaScript caller could not
    // reach it. The parity gate found that, which is what it is for.

    // Fading to `transparent` rather than to a transparent red, and the
    // difference is the whole test. Premultiplication decides what happens
    // to the COLOUR as the alpha falls, so two stops of the same hue cannot
    // separate the two modes: red to `rgba(250 2 0 / 0)` reads
    // `249,1,0,184` unpremultiplied and `251,1,0,184` premultiplied, two
    // levels apart in one channel, and identical at the midpoint. Measured
    // before this test was written, on stops chosen for being tidy.
    //
    // `transparent` is transparent *black*, so the colour has somewhere to
    // travel and the modes separate by 123 levels instead of two.
    const alphaMidpoint = (method) => {
      const ctx = raster(9, 1).getContext("2d"),
        gradient = ctx.createLinearGradient(0, 0, 9, 0);
      if (method) gradient.alphaInterpolationMethod = method;
      gradient.addColorStop(0, FROM);
      gradient.addColorStop(1, "transparent");
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, 9, 1);
      return [...ctx.getImageData(4, 0, 1, 1).data];
    };

    test("unpremultiplied carries the colour down with the alpha", () => {
      assert.deepEqual(alphaMidpoint("unpremultiplied"), [126, 2, 0, 128]);
    });

    test("premultiplied holds the colour and drops only the alpha", () => {
      assert.deepEqual(alphaMidpoint("premultiplied"), [249, 2, 0, 128]);
    });

    test("and the alpha channel is the same either way", () => {
      // What separates the modes is the colour, not the coverage. Without
      // this the two assertions above are satisfied by an implementation
      // that changes the alpha as well, which would be a different bug
      // wearing the right numbers.
      const [, , , unpre] = alphaMidpoint("unpremultiplied"),
        [, , , pre] = alphaMidpoint("premultiplied");
      assert.equal(unpre, pre, "only the colour channels move");
      assert.equal(unpre, 128);
    });

    test("the default is unpremultiplied, which is what a browser does", () => {
      assert.deepEqual(alphaMidpoint(null), alphaMidpoint("unpremultiplied"));
    });

    test("a same-hue ramp cannot tell the two apart", () => {
      // The trap this test block was nearly written on, pinned so that
      // nobody tidies the stops above into something that reads better and
      // measures nothing. If this ever starts discriminating, the
      // assertions above are testing something other than what they say.
      const sameHue = (method) => {
        const ctx = raster(9, 1).getContext("2d"),
          gradient = ctx.createLinearGradient(0, 0, 9, 0);
        gradient.alphaInterpolationMethod = method;
        gradient.addColorStop(0, FROM);
        gradient.addColorStop(1, "rgba(250 2 0 / 0)");
        ctx.fillStyle = gradient;
        ctx.fillRect(0, 0, 9, 1);
        return [...ctx.getImageData(4, 0, 1, 1).data];
      };
      assert.deepEqual(sameHue("unpremultiplied"), sameHue("premultiplied"));
    });

    test("it round-trips and refuses anything else", () => {
      const gradient = new Canvas(9, 1)
        .getContext("2d")
        .createLinearGradient(0, 0, 9, 0);
      assert.equal(gradient.alphaInterpolationMethod, "unpremultiplied");
      gradient.alphaInterpolationMethod = "premultiplied";
      assert.equal(gradient.alphaInterpolationMethod, "premultiplied");
      for (const bad of ["premultiply", "yes", "", "Premultiplied"])
        assert.throws(
          () => (gradient.alphaInterpolationMethod = bad),
          TypeError,
          bad,
        );
      assert.equal(gradient.alphaInterpolationMethod, "premultiplied");
    });
  });
});
