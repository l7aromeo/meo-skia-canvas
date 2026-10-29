// @ts-check

"use strict";

// Reading a GPU canvas back keeps nothing per read. A whole-page
// `getImageData` on the GPU runs Metal work whose objects are autoreleased,
// and Node's main thread has no autorelease pool to drain them: each read of
// a 2000 by 1600 page kept about what it read, resident until the process
// ended.
//
// Measured in its own file, so that no other test's memory lands in the
// process this reads: node runs each test file in a process of its own.
//
// Each pass ends with a collection, a turn of the event loop and a second
// collection. V8 cannot see the page a canvas holds, so without a forced
// collection the dropped canvases are never finalized, and Neon runs a
// finalizer only once the loop turns -- either way every canvas made stays
// resident and the growth measured is V8's, not the read's.

const { assert, describe, test } = require("../runner"),
  { setFlagsFromString } = require("node:v8"),
  { runInNewContext } = require("node:vm"),
  { setImmediate: turn } = require("node:timers/promises"),
  { Canvas } = require("../../lib");

// The suite runs without `--expose-gc`; a context made after the flag is set
// carries `gc`.
setFlagsFromString("--expose-gc");
const collect = runInNewContext("gc");

/**
 * The canvas each read is from. Large enough that the leak shows from the
 * first pass: at 1000 by 800, ten reads a pass kept nothing and forty kept
 * about two thirds of what they read.
 */
const SIDE = [2000, 1600];

/** Canvases made, drawn and read back whole per pass. */
const PER_PASS = 10;

/**
 * Passes; the growth judged is each pass's from the third on, after the
 * first two have made the context, its caches and its pipelines.
 */
const PASSES = 6;

/**
 * The most of what one pass reads that the median pass may keep resident: a
 * read that kept its buffer keeps all of it, and one that kept nothing keeps
 * nothing but the noise of the allocator. The median rather than the total,
 * because a cache filling to its budget steps the resident set up once --
 * 128 MB, once, in one pass of six -- where a leak grows it every pass.
 */
const KEPT_SHARE = 0.25;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

const gpu = new Canvas(16, 16, { gpu: true }).engine?.renderer === "GPU";

/**
 * The resident set after each of `PASSES` passes of whole-page reads, made
 * inside an open `save` layer where `inLayer` says so: the other way a read
 * reaches the page.
 * @param {boolean} inLayer
 */
const residentAfterReads = async (inLayer) => {
  const after = [];
  for (let pass = 0; pass < PASSES; pass++) {
    for (let i = 0; i < PER_PASS; i++) {
      const canvas = new Canvas(SIDE[0], SIDE[1], { gpu: true });
      const ctx = canvas.getContext("2d");
      ctx.fillRect(10, 10, 80, 40);
      if (inLayer) {
        ctx.saveLayer();
        ctx.fillRect(100, 10, 80, 40);
      }
      const data = ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      assert.equal(data.data.length, SIDE[0] * SIDE[1] * PIXEL_BYTES);
    }
    collect();
    await turn();
    collect();
    after.push(process.memoryUsage().rss);
  }
  return after;
};

describe("Reading a GPU canvas back", () => {
  for (const inLayer of [false, true]) {
    test(
      `keeps nothing per whole-page read${inLayer ? " inside a layer" : ""}`,
      { skip: gpu ? false : "skipped: no GPU engine here" },
      async () => {
        const after = await residentAfterReads(inLayer);
        const read = PER_PASS * SIDE[0] * SIDE[1] * PIXEL_BYTES;
        const growth = after
          .slice(3)
          .map((rss, pass) => rss - after[pass + 2])
          .sort((a, b) => a - b);
        const kept = growth[Math.floor(growth.length / 2)];
        assert.ok(
          kept <= KEPT_SHARE * read,
          `the median pass kept ${(kept / 1e6).toFixed(1)} MB of the ` +
            `${(read / 1e6).toFixed(1)} MB it read; after each pass ` +
            `${after.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`,
        );
      },
    );
  }
});
