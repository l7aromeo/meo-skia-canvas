// @ts-check

"use strict";

const fs = require("fs"),
  tmp = require("tmp"),
  path = require("path"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  {
    Canvas,
    DOMMatrix,
    DOMPoint,
    DOMRect,
    ParagraphBuilder,
  } = require("../../lib");

// Behaviour the browser Canvas defines, that the declaration files already
// promised, and that the runtime got wrong. Each of these typechecked against
// lib/index.d.ts and then threw, returned undefined, or silently produced NaN.
describe("browser conformance", () => {
  describe("CanvasRenderingContext2D", () => {
    /** @type {Canvas} */ let canvas;
    /** @type {any} */ let ctx;

    beforeEach(() => {
      canvas = new Canvas(64, 64);
      ctx = canvas.getContext("2d");
    });

    test("setTransform() with no arguments resets to identity", () => {
      ctx.translate(11, 13);
      ctx.scale(2, 3);
      ctx.setTransform();

      let m = ctx.getTransform();
      assert.equal(m.isIdentity, true);
      assert.equal(m.e, 0);
      assert.equal(m.f, 0);
    });

    test("setTransform() still accepts the six-argument form", () => {
      ctx.setTransform(2, 0, 0, 2, 3, 4);

      let m = ctx.getTransform();
      assert.equal(m.a, 2);
      assert.equal(m.e, 3);
      assert.equal(m.f, 4);
    });

    test("createImageData() clones the dimensions of an ImageData", () => {
      let source = ctx.createImageData(7, 5);
      source.data[0] = 255;

      let clone = ctx.createImageData(source);
      assert.equal(clone.width, 7);
      assert.equal(clone.height, 5);
      // A clone copies the dimensions, not the pixels.
      assert.equal(clone.data[0], 0);
    });

    test("createImageData() still accepts width and height", () => {
      let data = ctx.createImageData(7, 5);
      assert.equal(data.width, 7);
      assert.equal(data.height, 5);
    });
  });

  describe("DOMMatrix", () => {
    test("invertSelf() inverts in place and returns itself", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 0, 0]),
        returned = m.invertSelf();

      assert.equal(m.a, 0.5);
      assert.equal(m.d, 0.25);
      assert.equal(returned, m);
    });

    test("inverse() leaves the receiver untouched", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 10, 20]),
        inverted = m.inverse();

      assert.equal(inverted.a, 0.5);
      assert.equal(m.a, 2);
      assert.equal(m.d, 4);
      assert.equal(m.e, 10);
    });

    test("inverse() round-trips a point", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 10, 20]),
        there = m.transformPoint({ x: 7, y: 9 }),
        back = m.inverse().transformPoint(there);

      assert.ok(Math.abs(back.x - 7) < 1e-9);
      assert.ok(Math.abs(back.y - 9) < 1e-9);
    });

    test("inverse() of a singular matrix is all-NaN and not 2D", () => {
      let m = new DOMMatrix([0, 0, 0, 0, 0, 0]).inverse();

      assert.equal(m.is2D, false);
      assert.ok(Number.isNaN(m.a));
      assert.ok(Number.isNaN(m.f));
    });

    test("multiply() accepts a plain DOMMatrixInit", () => {
      let m = new DOMMatrix().multiply({ a: 2, b: 0, c: 0, d: 3, e: 0, f: 0 });

      assert.equal(m.a, 2);
      assert.equal(m.d, 3);
    });

    test("multiply() with no argument is the identity", () => {
      assert.equal(new DOMMatrix([3, 0, 0, 3, 0, 0]).multiply().a, 3);
    });

    test("transformPoint() defaults the omitted DOMPointInit fields", () => {
      let p = new DOMMatrix().transformPoint({ x: 3, y: 4 });

      assert.equal(p.x, 3);
      assert.equal(p.y, 4);
      assert.equal(p.z, 0);
      assert.equal(p.w, 1);
    });

    test("transformPoint() with no argument is the origin", () => {
      let p = new DOMMatrix().transformPoint();

      assert.equal(p.x, 0);
      assert.equal(p.w, 1);
    });
  });

  // https://drafts.csswg.org/geometry/#dom-domrectreadonly-top -- each edge is
  // the NaN-safe min/max of the coordinate and coordinate+extent. Returning
  // `y` and `x + width` directly is only correct for non-negative extents.
  describe("DOMRect edges", () => {
    test("are unchanged for positive extents", () => {
      let r = new DOMRect(10, 10, 20, 15);

      assert.equal(r.left, 10);
      assert.equal(r.right, 30);
      assert.equal(r.top, 10);
      assert.equal(r.bottom, 25);
    });

    test("normalize negative extents rather than inverting", () => {
      let r = new DOMRect(10, 10, -6, -4);

      assert.equal(r.left, 4);
      assert.equal(r.right, 10);
      assert.equal(r.top, 6);
      assert.equal(r.bottom, 10);
      // The point of the spec rule: an edge pair can never come out reversed.
      assert.ok(r.left <= r.right);
      assert.ok(r.top <= r.bottom);
    });

    test("propagate NaN", () => {
      let r = new DOMRect(NaN, 10, 5, 5);

      assert.ok(Number.isNaN(r.left));
      assert.ok(Number.isNaN(r.right));
    });

    test("toJSON reports the normalized edges", () => {
      let json = new DOMRect(10, 10, -6, -4).toJSON();

      assert.equal(json.left, 4);
      assert.equal(json.right, 10);
    });

    // The edges are prototype accessors; x/y/width/height are own properties.
    // Spread has always copied the latter, and callers rely on it.
    test("spread still yields the stored fields", () => {
      assert.deepStrictEqual(
        { ...new DOMRect(1, 2, 3, 4) },
        { x: 1, y: 2, width: 3, height: 4 },
      );
    });
  });

  describe("static factories default their argument", () => {
    test("DOMPoint.fromPoint()", () => {
      let p = DOMPoint.fromPoint();
      assert.equal(p.x, 0);
      assert.equal(p.w, 1);
    });

    test("DOMRect.fromRect()", () => {
      let r = DOMRect.fromRect();
      assert.equal(r.x, 0);
      assert.equal(r.width, 0);
    });

    test("DOMMatrix.fromMatrix()", () => {
      assert.equal(DOMMatrix.fromMatrix().isIdentity, true);
    });
  });
});

describe("Canvas", () => {
  /** @type {any} */ let dir;

  beforeEach(() => (dir = tmp.dirSync().name));
  afterEach(() => fs.rmSync(dir, { recursive: true, force: true }));

  // The deprecation shims forwarded to the new method but dropped its return
  // value, so `await canvas.saveAs(...)` resolved before the write finished.
  test("saveAs() resolves only once the file is written", async () => {
    let canvas = new Canvas(16, 16),
      dst = path.join(dir, "out.png");

    await canvas.saveAs(dst);
    assert.equal(fs.existsSync(dst), true);
  });

  test("toDataURLSync() returns the data URL", () => {
    let url = new Canvas(16, 16).toDataURLSync("png");
    assert.equal(typeof url, "string");
    assert.ok(url.startsWith("data:image/png;base64,"));
  });

  // `gpu` reported the global default rather than what the constructor
  // selected, so it disagreed with `engine.renderer` for the whole life of the
  // canvas.
  //
  // The fix is native, so this fails against the previous release's binary --
  // which is exactly what `ci.yml` runs the current JS against. That failure
  // is true, not spurious: the published binary really does report the wrong
  // engine. It clears when the release carrying the fix ships, and until then
  // it is the documented cost of landing a native change (see AGENTS.md).
  //
  // Deliberately not skipped. Every gate cheap enough to write here also
  // matched a genuine regression, so skipping would have silenced the one
  // case this test exists for.
  //
  // Only visible on a host with a GPU: where none is reachable the old default
  // was CPU anyway, which is why this passed on Linux and failed on macOS.
  test("gpu agrees with the selected renderer", () => {
    let cpu = new Canvas(8, 8, { gpu: false });
    assert.equal(cpu.gpu, false);
    assert.equal(cpu.engine.renderer, "CPU");
  });
});

// `drawParagraph` reaches Skia's `Paragraph::paint`, which draws with the text
// styles' own paints. The context's paint state has to be applied around it or
// it is silently dropped -- so `globalAlpha` did nothing and every blend mode
// behaved as source-over. Native fix, so these fail against a binary that
// predates it, as the engine test above does.
describe("drawParagraph honours canvas paint state", () => {
  function paragraph() {
    let builder = ParagraphBuilder.Make({
      textStyle: { fontSize: 24, color: [0, 0, 0, 1] },
    });
    builder.addText("XXXX");

    let para = builder.build();
    para.layout(200);
    return para;
  }

  // Counts pixels by kind over a red backdrop the glyphs are drawn onto.
  function draw({ alpha = 1, op = "source-over" } = {}) {
    let canvas = new Canvas(120, 40),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 120, 40);
    ctx.globalAlpha = alpha;
    ctx.globalCompositeOperation = op;
    ctx.drawParagraph(paragraph(), 2, 2);

    let data = ctx.getImageData(0, 0, 120, 40).data,
      tally = { red: 0, glyph: 0, transparent: 0 };

    for (let i = 0; i < data.length; i += 4) {
      if (data[i + 3] === 0) tally.transparent++;
      else if (data[i] > 200 && data[i + 1] < 60) tally.red++;
      else tally.glyph++;
    }
    return tally;
  }

  test("globalAlpha fades the glyphs", () => {
    let opaque = draw({ alpha: 1 }),
      faded = draw({ alpha: 0.5 });

    assert.ok(opaque.glyph > 0, "baseline should draw glyphs");
    // Half-opacity glyphs blend toward the red backdrop, so fewer pixels read
    // as glyph-coloured than at full opacity.
    assert.ok(
      faded.glyph < opaque.glyph,
      `expected fewer glyph pixels at 0.5 alpha, got ${faded.glyph} vs ${opaque.glyph}`,
    );
  });

  test("destination-out erases where the glyphs land", () => {
    let out = draw({ op: "destination-out" });

    assert.equal(out.glyph, 0);
    assert.ok(out.transparent > 0, "glyph area should be punched out");
  });

  test("copy discards what was already there", () => {
    let copied = draw({ op: "copy" });

    assert.equal(copied.red, 0);
    assert.ok(copied.glyph > 0);
  });

  test("the default path is unchanged", () => {
    let plain = draw();

    assert.ok(plain.red > 0 && plain.glyph > 0);
    assert.equal(plain.transparent, 0);
  });
});

// Declared in the types since before this fork, never implemented -- upstream
// still ships both declarations against no implementation.
describe("declared API that had no implementation", () => {
  test("Canvas.contexts maps a canvas to its contexts", () => {
    let canvas = new Canvas(16, 16);
    canvas.getContext("2d");

    assert.ok(Canvas.contexts instanceof WeakMap);
    assert.equal(Canvas.contexts.get(canvas).length, 1);

    // Holds the live array, so later pages show up without re-registering.
    canvas.newPage(16, 16);
    assert.equal(Canvas.contexts.get(canvas).length, 2);
  });

  test("toSharpSync() returns the same image as toSharp()", async () => {
    let canvas = new Canvas(32, 20),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "#3366cc";
    ctx.fillRect(0, 0, 32, 20);
    ctx.fillStyle = "#ffcc00";
    ctx.fillRect(4, 4, 10, 8);

    // Compare decoded pixels, not encoded PNG bytes: the byte stream depends
    // on sharp's encoder and is not stable run to run, which made an earlier
    // version of this test flaky for reasons that had nothing to do with the
    // canvas. Sequential, so the two reads cannot interleave.
    let asynchronous = await canvas.toSharp().raw().toBuffer(),
      synchronous = await canvas.toSharpSync().raw().toBuffer();

    assert.equal(synchronous.length, asynchronous.length);
    assert.equal(Buffer.compare(asynchronous, synchronous), 0);
  });
});
