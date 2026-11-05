// @ts-check

"use strict";

// What the dropped-canvas tests judge memory by. A process making canvases
// and dropping them holds memory in a sawtooth: it climbs while the dropped
// ones wait for a collection and falls back when one runs. One sample can
// land anywhere on a tooth, so what is judged is the floor -- the least
// held over a few passes -- at the end against early on, once the allocator
// has settled. A canvas never collected raises every later sample, the
// lowest included; one collected late raises only some.
//
// Shared by the test of resident memory on the CPU and the test of what is
// reported to V8 for the GPU, each a file of its own so that neither's
// canvases land in the other's measure: node runs each test file in a process
// of its own.

const { setImmediate: turn } = require("node:timers/promises"),
  { Canvas } = require("../../lib");

/** The canvas each pass makes and reads. */
const SIDE = [1000, 800];

/** Canvases made, read back whole and dropped per pass. */
const PER_PASS = 10;

/** Passes made in all. */
const PASSES = 30;

/**
 * Passes left out of the judgement while the allocator and the caches warm
 * up. Resident memory on macOS settled by the end of pass 4 or 5, and in one
 * run pass 3 stood 107 MB under where it settled.
 */
const WARM_PASSES = 5;

/**
 * The passes each floor is the least of: the first after warming up, and the
 * last. On CI's Linux runner the sawtooth fell back to its floor every second
 * to fourth pass, so five hold at least one trough.
 */
const FLOOR_PASSES = 5;

/**
 * The most of what the passes between the two floors read that may stay
 * under the last. Collected canvases leave the floors within a few MB of
 * each other; uncollected ones raise the last by all of it.
 */
const KEPT_SHARE = 0.25;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/**
 * Whether `after`, the bytes held after each pass, keeps no more than
 * `KEPT_SHARE` of what its passes read -- the floor of the last
 * `FLOOR_PASSES` less the floor of the `FLOOR_PASSES` after `WARM_PASSES`,
 * against what the passes between them read -- and a message saying what it
 * kept, naming what was measured as `what`.
 *
 * @param {number[]} after
 * @param {string} what
 */
function judged(after, what) {
  const start = Math.min(
    ...after.slice(WARM_PASSES, WARM_PASSES + FLOOR_PASSES),
  );
  const end = Math.min(...after.slice(-FLOOR_PASSES));
  const kept = end - start;
  // Everything read after the latest pass the first floor can fall on, up to
  // the end of the earliest the last can, stays under the last floor.
  const passes = after.length - FLOOR_PASSES + 1 - (WARM_PASSES + FLOOR_PASSES);
  const read = passes * PER_PASS * SIDE[0] * SIDE[1] * PIXEL_BYTES;
  const message =
    `the floor of ${what} over the last ${FLOOR_PASSES} passes stood ` +
    `${(kept / 1e6).toFixed(1)} MB over its floor in passes ` +
    `${WARM_PASSES + 1} to ${WARM_PASSES + FLOOR_PASSES}, against the ` +
    `${(read / 1e6).toFixed(1)} MB the ${passes} passes between read; ` +
    `after each pass ${after.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`;
  return { kept: kept <= KEPT_SHARE * read, message };
}

/**
 * `measure()` after each of `PASSES` passes of `PER_PASS` canvases made with
 * `gpu`, read back, and each handed to `hold` once read.
 *
 * @param {{ gpu: boolean, hold: (canvas: Canvas) => void, measure: () => number }} how
 */
async function afterEachPass({ gpu, hold, measure }) {
  const after = [];
  for (let pass = 0; pass < PASSES; pass++) {
    for (let i = 0; i < PER_PASS; i++) {
      const canvas = new Canvas(SIDE[0], SIDE[1]);
      canvas.gpu = gpu;
      const ctx = canvas.getContext("2d");
      ctx.fillRect(10, 10, 80, 40);
      ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      hold(canvas);
    }
    await turn();
    after.push(measure());
  }
  return after;
}

module.exports = { PASSES, PER_PASS, SIDE, afterEachPass, judged };
