// @ts-check

"use strict";

// What a canvas or an image holds natively is reported to V8 as external
// memory, and released when the object is finalized, so the engine schedules
// collections by it. These read the engine's total of what addons reported
// through `__externalMemory`, which debug builds alone export: against a
// release binary they are skipped by name. `process.memoryUsage().external`
// cannot stand in for it -- on Node 26 it counts the engine's own external
// allocations, such as array buffers, and moved by nothing when a page was
// reported.
//
// A file of its own, because the total is the process's: node runs each test
// file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { setFlagsFromString } = require("node:v8"),
  { runInNewContext } = require("node:vm"),
  { setImmediate: turn } = require("node:timers/promises"),
  { Canvas, Image } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

// The suite runs without `--expose-gc`; a context made after the flag is set
// carries `gc`.
setFlagsFromString("--expose-gc");
const collect = runInNewContext("gc");

const why = !skiaNode.__externalMemory
  ? "skipped: a release binary does not export __externalMemory"
  : null;

/** The canvas read back. */
const SIDE = [1000, 800];

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/**
 * How far the total may sit from what the test predicts. Node counts its
 * own allocations in the same total -- array buffers among them, which the
 * collections in `settled` free -- and moved it by up to 10 kB between two
 * reads here, so a figure is checked to within this rather than exactly,
 * and every figure checked is far larger.
 */
const SLACK = 64 * 1024;

/**
 * Asserts `actual` is within [`SLACK`] of `expected`.
 * @param {number} actual
 * @param {number} expected
 * @param {string} what
 */
const near = (actual, expected, what) =>
  assert.ok(
    Math.abs(actual - expected) <= SLACK,
    `${what}: ${actual} bytes, where ${expected} were expected`,
  );

/**
 * The reported total once everything collectable has been collected and
 * finalized. Neon runs a finalizer only once the event loop turns, so a
 * collection alone leaves a dropped object counted.
 */
const settled = async () => {
  collect();
  await turn();
  collect();
  const total = skiaNode.__externalMemory();
  assert.equal(typeof total, "number", "the host reports external memory");
  return /** @type {number} */ (total);
};

describe("Native memory reported to V8", () => {
  test(
    "counts a page read back for as long as its canvas lives",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      // Held only through this array, so that emptying it is what lets the
      // canvas go: a variable naming it or its context would keep it.
      const live = [new Canvas(SIDE[0], SIDE[1])];
      live[0].getContext("2d").fillRect(10, 10, 80, 40);
      live[0].getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      near(
        (await settled()) - before,
        SIDE[0] * SIDE[1] * PIXEL_BYTES,
        "the page a live canvas holds",
      );

      live.pop();
      near(
        (await settled()) - before,
        0,
        "what stays counted once the canvas is finalized",
      );
    },
  );

  test(
    "drops the page from the count when a resize drops it",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      const canvas = new Canvas(SIDE[0], SIDE[1]);
      canvas.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      canvas.width = SIDE[0];
      near(
        (await settled()) - before,
        0,
        "what a canvas whose page a resize dropped counts",
      );
      assert.equal(canvas.width, SIDE[0]);
    },
  );

  test(
    "counts an image's encoded bytes until it is finalized",
    { skip: why ?? false },
    async () => {
      const source = new Canvas(SIDE[0], SIDE[1]),
        ctx = source.getContext("2d"),
        noise = ctx.createImageData(SIDE[0], SIDE[1]);
      // Noise, which PNG cannot compress, so the encoding is far larger than
      // the slack: a drawing encodes to a few kilobytes, inside it.
      let seed = 1;
      for (let i = 0; i < noise.data.length; i++) {
        seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
        noise.data[i] = i % 4 == 3 ? 255 : seed >>> 24;
      }
      ctx.putImageData(noise, 0, 0);
      const encoded = source.toBufferSync("png");
      assert.ok(encoded.length > 16 * SLACK, `${encoded.length} bytes`);

      const before = await settled();
      const live = [new Image()];
      live[0].src = encoded;
      assert.ok(live[0].complete, "the PNG decoded");
      near(
        (await settled()) - before,
        encoded.length,
        "what an image decoded on demand counts",
      );

      live.pop();
      near(
        (await settled()) - before,
        0,
        "what stays counted once the image is finalized",
      );
    },
  );
});
