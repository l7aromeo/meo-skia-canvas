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
      let ops = [
        "source-over",
        "destination-over",
        "copy",
        "destination",
        "clear",
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
