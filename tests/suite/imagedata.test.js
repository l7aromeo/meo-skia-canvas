// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, Image, ImageData } = require("../../lib");

describe("createImageData clones every setting, not only the format", () => {
  // The standard says the one-argument form takes its settings from the
  // source. `new ImageData(source)` already did; this one dropped the colour
  // space, so the two documented ways to copy an `ImageData` disagreed about
  // the same object.
  const SPACES = ["srgb", "display-p3", "rec2020"];

  for (const colorSpace of SPACES) {
    test(`keeps ${colorSpace} across the clone`, () => {
      const ctx = new Canvas(4, 4, { colorSpace }).getContext("2d");
      const source = ctx.getImageData(0, 0, 4, 4);
      assert.equal(source.colorSpace, colorSpace, "the source to clone from");

      const clone = ctx.createImageData(source);
      assert.deepEqual(
        { colorSpace: clone.colorSpace, colorType: clone.colorType },
        { colorSpace: source.colorSpace, colorType: source.colorType },
        `${colorSpace}: the clone changed a setting`,
      );
    });
  }

  test("agrees with the constructor it stands beside", () => {
    const ctx = new Canvas(4, 4, { colorSpace: "display-p3" }).getContext("2d");
    const source = ctx.getImageData(0, 0, 4, 4);

    assert.equal(
      ctx.createImageData(source).colorSpace,
      new ImageData(source).colorSpace,
      "createImageData(source) and new ImageData(source) disagree",
    );
  });

  test("still blanks the pixels and keeps the dimensions", () => {
    const ctx = new Canvas(3, 5).getContext("2d");
    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 3, 5);

    const clone = ctx.createImageData(ctx.getImageData(0, 0, 3, 5));
    assert.deepEqual(
      { width: clone.width, height: clone.height },
      { width: 3, height: 5 },
    );
    assert.ok(
      clone.data.every((byte) => byte === 0),
      "a clone carries blank pixels",
    );
  });
});

describe("an ImageData too large for Skia is refused, not fatal", () => {
  // V8 does not raise an exception for an oversized typed array -- it aborts
  // the process with "Check failed: change_in_bytes < kMaxReasonableBytes",
  // which no `catch` can reach and no test can survive. The addon already
  // refused the same product on its own side of the boundary; this is the
  // allocation that did not.
  const HUGE = 100000;

  test("createImageData refuses rather than killing the process", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    assert.throws(
      () => ctx.createImageData(HUGE, HUGE),
      /Requested image data is too large/,
    );
  });

  test("the constructor refuses the same way", () => {
    assert.throws(
      () => new ImageData(HUGE, HUGE),
      /Requested image data is too large/,
    );
  });

  test("the message names the dimensions and the format", () => {
    assert.throws(
      () => new ImageData(HUGE, HUGE, { colorType: "RGBAF32" }),
      new RegExp(`${HUGE}x${HUGE} at RGBAF32`),
    );
  });

  test("a size Skia can address is still built", () => {
    const data = new ImageData(2048, 2048);
    assert.equal(data.data.length, 2048 * 2048 * 4);
  });
});

describe("Image reports what it expected", () => {
  test("names the two types it takes, spelled correctly", () => {
    assert.throws(
      // @ts-expect-error -- deliberately neither a Buffer nor a string
      () => new Image({}),
      /Expected a Buffer or a String containing a data URL/,
    );
  });
});
