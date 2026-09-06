// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

describe("createImageData", () => {
  test("inherits the canvas's colour space", () => {
    // The standard says so directly: "Initialize newImageData given the
    // absolute magnitude of sw, the absolute magnitude of sh, settings, and
    // defaultColorSpace set to this's color space." This overload passed its
    // settings straight through and let `ImageData` default the space, so a
    // Display P3 canvas handed back a buffer labelled sRGB.
    let ctx = new Canvas(8, 8, { colorSpace: "display-p3" }).getContext("2d");
    assert.equal(ctx.createImageData(2, 2).colorSpace, "display-p3");

    let plain = new Canvas(8, 8).getContext("2d");
    assert.equal(plain.createImageData(2, 2).colorSpace, "srgb");
  });

  test("inherits the canvas's pixel format too", () => {
    // Not the standard's requirement -- `colorType` is this crate's -- but
    // `getImageData` and the cloning overload both inherit it, and a
    // `createImageData` that did not would describe a different buffer from
    // the one `getImageData` returns for the same canvas.
    let ctx = new Canvas(8, 8, { colorType: "RGBAF16" }).getContext("2d");
    assert.equal(ctx.createImageData(2, 2).colorType, "RGBAF16");
    assert.equal(ctx.getImageData(0, 0, 2, 2).colorType, "RGBAF16");
  });

  test("labels the buffer so putImageData reads the bytes as written", () => {
    // The label is not decoration: it decides how `putImageData` interprets
    // the components. Mislabelled sRGB, these P3 bytes were converted into P3
    // on the way in and landed on [215, 69, 50].
    let canvas = new Canvas(4, 4, { colorSpace: "display-p3" });
    let ctx = canvas.getContext("2d");

    let buffer = ctx.createImageData(2, 2);
    for (let i = 0; i < buffer.data.length; i += 4)
      buffer.data.set([234, 51, 35, 255], i);
    ctx.putImageData(buffer, 0, 0);

    assert.deepEqual(
      Array.from(
        ctx.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data,
      ),
      [234, 51, 35, 255],
    );
  });

  test("the two forms that were already right are unchanged", () => {
    // These are the controls that localised the defect to the size overload:
    // both already inherited, so a fix that moved them would have been
    // fixing the wrong thing.
    let canvas = new Canvas(4, 4, { colorSpace: "display-p3" });
    let ctx = canvas.getContext("2d");

    let source = ctx.getImageData(0, 0, 2, 2);
    assert.equal(source.colorSpace, "display-p3");
    assert.equal(
      ctx.createImageData(source).colorSpace,
      "display-p3",
      "the cloning overload takes its source's space",
    );

    assert.equal(
      ctx.createImageData(2, 2, { colorSpace: "srgb" }).colorSpace,
      "srgb",
      "an explicit setting still wins over the canvas",
    );
  });
});
