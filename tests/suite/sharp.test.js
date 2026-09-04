// @ts-check

"use strict";

const sharp = require("sharp"),
  { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
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

      let data;
      try {
        data = canvas.getContext("2d").getImageData(0, 0, SIZE, SIZE);
      } catch {
        // `BGR101010x` is the one type `getImageData` refuses, with "Could
        // not get image data", and only on the first call to a GPU-backed
        // canvas -- the second succeeds:
        //
        //   gpu=true   attempt 1 THROW   attempt 2 ok   attempt 3 ok
        //   gpu=false  attempt 1 ok      attempt 2 ok   attempt 3 ok
        //
        // The first read goes through `Surface::read_pixels`, which a
        // Metal-backed surface declines for this format; the second takes the
        // raster-snapshot branch, which accepts it. So this is a bug being
        // fixed rather than a fact about the format, and the skip is here to
        // keep one flaky type from failing a suite about channel counts.
        // No ImageData means nothing here to hand sharp.
        return;
      }

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
