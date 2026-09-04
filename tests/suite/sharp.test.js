// @ts-check

"use strict";

const sharp = require("sharp"),
  { assert, describe, test } = require("../runner"),
  { Canvas, loadImageData } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

const RED = "#ff0000";
const SIZE = 4;

// Every `colorType` the addon publishes, so a type added there arrives here
// without this file being edited. The list is the addon's own -- the same
// source `PIXEL_SIZES` reads -- rather than a copy that can drift from it.
const COLOR_TYPES = JSON.parse(skiaNode.colorTypes()).map(({ name }) => name);

/** A canvas of `colorType`, filled edge to edge with opaque red. */
function filled(colorType) {
  const canvas = new Canvas(SIZE, SIZE, { colorType });
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = RED;
  ctx.fillRect(0, 0, SIZE, SIZE);
  return canvas;
}

/**
 * What the canvas holds, as 8-bit RGBA, reached through a route that does not
 * involve `toSharp`. PNG is the reference because the addon encodes it from
 * the same surface and every `colorType` can be written to it.
 */
async function viaPNG(canvas) {
  return sharp(canvas.toBufferSync("png")).ensureAlpha().raw().toBuffer();
}

describe("toSharp hands over the pixels it actually has", () => {
  // The failure this guards is silent: sharp is told four 8-bit channels
  // whatever the canvas holds, so a wider format encodes part of its buffer as
  // a plausible image and a narrower one raises a vips error about byte counts
  // that names no option the caller passed.
  for (const colorType of COLOR_TYPES) {
    test(`${colorType} survives Canvas.toSharpSync()`, async () => {
      const canvas = filled(colorType);
      const expected = await viaPNG(canvas);
      const actual = await canvas.toSharpSync().ensureAlpha().raw().toBuffer();

      assert.equal(
        actual.length,
        expected.length,
        `${colorType}: ${actual.length} bytes through toSharpSync, ` +
          `${expected.length} through PNG`,
      );
      assert.deepEqual(
        Array.from(actual.subarray(0, 4)),
        Array.from(expected.subarray(0, 4)),
        `${colorType}: first pixel differs between the two routes`,
      );
    });

    test(`${colorType} survives Canvas.toSharp()`, async () => {
      const canvas = filled(colorType);
      const expected = await viaPNG(canvas);
      const actual = await canvas.toSharp().ensureAlpha().raw().toBuffer();

      assert.equal(actual.length, expected.length, `${colorType}: length`);
      assert.deepEqual(
        Array.from(actual.subarray(0, 4)),
        Array.from(expected.subarray(0, 4)),
        `${colorType}: first pixel`,
      );
    });
  }

  test("density scales what sharp is told, not just the buffer", async () => {
    const canvas = filled("rgba");
    const { width, height } = await canvas
      .toSharpSync({ density: 2 })
      .metadata();
    assert.deepEqual({ width, height }, { width: SIZE * 2, height: SIZE * 2 });
  });
});

describe("ImageData.toSharp reports its own layout", () => {
  // `bytesPerPixel` is not a channel count. The two agree for `rgba` (4) and
  // `Gray8` (1), which is why handing one over as the other reads as correct
  // until a two-byte type reaches it.
  //
  // The canvas is not the reference here, as it is above, because a canvas is
  // not an oracle for its own declared format. Skia has no `Gray8` surface, so
  // a `Gray8` canvas composites in N32 and holds full colour: its PNG is red
  // and its rgba readback is 255,0,0,255. The single-byte read is the only
  // path that converts, applying Rec.709 luma on the way out -- which is why
  // red, green and blue read back as 54, 182 and 18.
  //
  // So this half compares the bytes sharp returns against the bytes
  // `ImageData` exposes, and nothing else. Those are the bytes the caller has.
  for (const colorType of COLOR_TYPES) {
    test(`${colorType} either round-trips its bytes or is refused by name`, async () => {
      const canvas = filled(colorType);

      // Every published type reads back, `BGR101010x` included. It used to
      // refuse the first read on a Metal-backed canvas and accept the second,
      // so this call was wrapped in a `try` that skipped the type entirely.
      // The addon now falls through to the raster-snapshot copy the second
      // read already reached, so there is nothing left to skip -- and a read
      // that fails here should fail the test rather than quietly excuse one
      // colour type from a suite about channel counts.
      const data = canvas.getContext("2d").getImageData(0, 0, SIZE, SIZE);

      // The addon canonicalises its aliases, so a canvas asked for
      // `RGBA8888` yields an `ImageData` reporting `rgba`. The refusal has to
      // name the type the caller can see on the object, which is this one.
      const reported = data.colorType;

      let image;
      try {
        image = data.toSharp();
      } catch (e) {
        // A refusal is a valid answer for a layout sharp cannot express as
        // N channels of 8-bit in its own order, but it has to name the type
        // -- a vips error about byte counts names neither the type nor a way
        // forward.
        assert.match(
          e.message,
          new RegExp(reported.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")),
          `${colorType}: refused without naming \`${reported}\`: ${e.message}`,
        );
        return;
      }

      // Not a byte-for-byte comparison: sharp expands a one- or two-channel
      // image to sRGB on the way out, so the buffer it returns is wider than
      // the one it was given. What has to hold is that it read the buffer at
      // the right dimensions and started at the right pixel -- handing it the
      // wrong channel count gets one or both wrong.
      const meta = await image.metadata();
      assert.deepEqual(
        { width: meta.width, height: meta.height },
        { width: SIZE, height: SIZE },
        `${colorType}: sharp read the buffer at the wrong dimensions`,
      );

      const actual = await image.raw().toBuffer();
      assert.equal(
        actual[0],
        data.data[0],
        `${colorType}: first channel of the first pixel differs`,
      );
    });
  }
});

// `loadImageData` builds its result two ways. A decoded source hands the
// `ImageData` constructor the caller's own options object; a sharp source takes
// the raw branch, which used to pass width and height and nothing else. So the
// same call honored `colorSpace` or discarded it depending on what came back
// from the fetch rather than on how it was written, and only a sharp source hit
// the losing side. See issue #50.
describe("loadImageData with a sharp source", () => {
  const swatch = () =>
    sharp({
      create: {
        width: 2,
        height: 2,
        channels: 3,
        background: { r: 255, g: 128, b: 0 },
      },
    })
      .png()
      .toBuffer()
      .then((png) => sharp(png));

  // The options live at args[2], which is the fourth argument overall --
  // `loadImageData(src, width, height, settings)`, mirroring the `ImageData`
  // constructor. Passing them third is a silent no-op and reads as the bug.
  const load = async (settings) =>
    loadImageData(await swatch(), undefined, undefined, settings);

  test("carries colorSpace through the raw branch", async () => {
    const data = await load({ colorSpace: "display-p3" });
    assert.equal(data.colorSpace, "display-p3");
  });

  test("still defaults to srgb when no colorSpace is named", async () => {
    for (const settings of [undefined, {}]) {
      const data = await load(settings);
      assert.equal(data.colorSpace, "srgb");
    }
  });

  // Not a drop: `fetchData` takes a sharp source through `.ensureAlpha().raw()`,
  // so the bytes are eight-bit RGBA by construction. Any other `colorType`
  // describes them wrongly and would fail the constructor's length check, so it
  // is refused by name rather than accepted and ignored.
  test("refuses a colorType the raw bytes cannot be", async () => {
    for (const colorType of ["rgbaf16", "Gray8", "RGB565"]) {
      await assert.rejects(
        () => load({ colorType }),
        /cannot honor colortype/i,
        `${colorType} should be refused rather than silently dropped`,
      );
    }
  });

  test("accepts the colorType the raw bytes actually are", async () => {
    const data = await load({ colorType: "rgba" });
    assert.equal(data.colorType, "rgba");
    assert.equal(data.data.length, 2 * 2 * 4);
  });
});
