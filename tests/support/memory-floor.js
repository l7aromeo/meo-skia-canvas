// @ts-check

"use strict";

// What the dropped-canvas tests judge memory by. A process making canvases
// and dropping them holds memory in a sawtooth: it climbs while the dropped
// ones wait for a collection and falls back when one runs. One sample can
// land anywhere on a tooth, so what is judged is the floor -- the least
// held over a few passes -- over the last few passes against the few before
// them, once the allocator has settled. A canvas never collected raises every
// later sample, the lowest included; one collected late raises only some.
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

/**
 * Passes made in all. Only the last `2 * FLOOR_PASSES` are judged; the rest
 * warm the allocator and the caches. Resident memory settled by pass 4 or 5
 * on macOS, and by pass 11 on CI's Linux arm64 release leg, where it climbed
 * in 64 MB steps to 395.9 MB and stayed there (run 42122640796).
 */
const PASSES = 40;

/**
 * The passes each floor is the least of: the last, and the ones just before
 * them. On CI's Linux runner the sawtooth fell back to its floor every
 * second to fourth pass, so five hold at least one trough.
 */
const FLOOR_PASSES = 5;

/**
 * The most of what one window of passes reads that the last floor may stand
 * over the floor before it. A canvas never collected raises memory with
 * every pass, so each floor is its window's first pass and the two stand a
 * window's reads apart; collected ones leave them within a few MB.
 */
const KEPT_SHARE = 0.25;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/**
 * Whether `after`, the bytes held after each pass, keeps no more than
 * `KEPT_SHARE` of what a window of passes reads -- the floor of the last
 * `FLOOR_PASSES` less the floor of the `FLOOR_PASSES` before them -- and a
 * message saying what it kept, naming what was measured as `what`.
 *
 * @param {number[]} after
 * @param {string} what
 */
function judged(after, what) {
  const floor = (/** @type {number[]} */ passes) => Math.min(...passes);
  const start = floor(after.slice(-2 * FLOOR_PASSES, -FLOOR_PASSES));
  const end = floor(after.slice(-FLOOR_PASSES));
  const kept = end - start;
  const read = FLOOR_PASSES * PER_PASS * SIDE[0] * SIDE[1] * PIXEL_BYTES;
  const first = after.length - 2 * FLOOR_PASSES + 1;
  const message =
    `the floor of ${what} over passes ${first + FLOOR_PASSES} to ` +
    `${after.length} stood ${(kept / 1e6).toFixed(1)} MB over its floor in ` +
    `passes ${first} to ${first + FLOOR_PASSES - 1}, against the ` +
    `${(read / 1e6).toFixed(1)} MB ${FLOOR_PASSES} passes read; after each ` +
    `pass ${after.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`;
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
