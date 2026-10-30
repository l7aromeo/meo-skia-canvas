// @ts-check

"use strict";

// Canvases made and dropped are collected without anything forcing it. A
// context is a small object on V8's heap however large the page it holds, so
// until that page was reported as external memory a process making canvases
// and allocating little JavaScript was never collected: every canvas it made
// stayed resident, 3.2 MB for each 1000 by 800 page read back.
//
// No forced collection here, on purpose: what is tested is that V8 decides to
// collect on its own. The loop does turn between passes, because a server
// does between requests and Neon runs a finalizer only once it has.
//
// A file of its own, so that no other test's memory lands in the process this
// measures: node runs each test file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { setImmediate: turn } = require("node:timers/promises"),
  { Canvas } = require("../../lib");

/** The canvas each pass makes and reads. */
const SIDE = [1000, 800];

/** Canvases made, read back whole and dropped per pass. */
const PER_PASS = 10;

/**
 * Passes; the growth judged is from the end of the third to the end of the
 * last, after the first two have warmed the allocator and the caches.
 */
const PASSES = 20;

/**
 * The most of what the judged passes read that may stay resident. Collected
 * canvases leave a sawtooth well under this; uncollected ones keep all of it.
 */
const KEPT_SHARE = 0.25;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

describe("Canvases that are dropped", () => {
  test("are collected without a forced collection", async () => {
    const after = [];
    for (let pass = 0; pass < PASSES; pass++) {
      for (let i = 0; i < PER_PASS; i++) {
        const ctx = new Canvas(SIDE[0], SIDE[1]).getContext("2d");
        ctx.fillRect(10, 10, 80, 40);
        ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      }
      await turn();
      after.push(process.memoryUsage().rss);
    }
    const read = (PASSES - 3) * PER_PASS * SIDE[0] * SIDE[1] * PIXEL_BYTES;
    const kept = after[PASSES - 1] - after[2];
    assert.ok(
      kept <= KEPT_SHARE * read,
      `${(kept / 1e6).toFixed(1)} MB of the ${(read / 1e6).toFixed(1)} MB ` +
        `the last ${PASSES - 3} passes read stayed resident; after each ` +
        `pass ${after.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`,
    );
  });
});
