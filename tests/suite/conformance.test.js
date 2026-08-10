// @ts-check

"use strict";

const fs = require("fs"),
  tmp = require("tmp"),
  path = require("path"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  { Canvas, DOMMatrix, DOMPoint, DOMRect } = require("../../lib");

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

  // `gpu` reported the default rather than what the constructor selected, so it
  // disagreed with `engine.renderer` for the whole life of the canvas.
  test("gpu agrees with the selected renderer", () => {
    let cpu = new Canvas(8, 8, { gpu: false });
    assert.equal(cpu.gpu, false);
    assert.equal(cpu.engine.renderer, "CPU");
  });
});
