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
// Resident memory under collection is a sawtooth: it climbs while dropped
// canvases wait for a collection and falls back when one runs. One sample
// can land anywhere on a tooth, so what is judged is the floor -- the least
// resident over a few passes -- at the end against early on, after the
// allocator has settled. A canvas never collected raises every later sample,
// the lowest included; one collected late raises only some.
//
// The pages are on the CPU, where what a context keeps is resident. A page on
// the GPU is held by the driver, outside the process's resident memory on
// macOS: two hundred GPU canvases kept alive there, reporting 640 MB to V8,
// raised it by 6 MB, so a GPU page would pass with or without collection.
//
// A file of its own, so that no other test's memory lands in the process this
// measures: node runs each test file in a process of its own. The control,
// which keeps its canvases, runs last for the same reason.

const { assert, describe, test } = require("../runner"),
  { setImmediate: turn } = require("node:timers/promises"),
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
 * The passes each floor is the least resident of: the first after warming
 * up, and the last. On CI's Linux runner the sawtooth fell back to its
 * floor every second to fourth pass, so five hold at least one trough.
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
 * Resident memory after each of 20 passes of a run on CI's Linux runner, in
 * MB: collected throughout, its last pass on a peak. Run 42103176700, the
 * `binding (Rust + JS on a fresh build)` job, where the last sample's rise of
 * 170.6 MB over pass 3 was refused (#595).
 */
const SAWTOOTH_ON_CI = [
  369.1, 556.5, 514.9, 521.3, 515.0, 563.4, 595.7, 760.3, 518.4, 524.8, 605.3,
  517.8, 566.3, 517.9, 524.3, 524.2, 572.6, 524.2, 530.7, 685.5,
];

/**
 * Whether `after`, the resident bytes after each pass, keeps no more than
 * `KEPT_SHARE` of what its passes read -- the floor of the last
 * `FLOOR_PASSES` less the floor of the `FLOOR_PASSES` after `WARM_PASSES`,
 * against what the passes between them read -- and a message saying what it
 * kept.
 *
 * @param {number[]} after
 */
function judged(after) {
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
    `the floor of the last ${FLOOR_PASSES} passes stood ` +
    `${(kept / 1e6).toFixed(1)} MB over the floor of passes ` +
    `${WARM_PASSES + 1} to ${WARM_PASSES + FLOOR_PASSES}, against the ` +
    `${(read / 1e6).toFixed(1)} MB the ${passes} passes between read; ` +
    `after each pass ${after.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`;
  return { kept: kept <= KEPT_SHARE * read, message };
}

/**
 * Resident bytes after each of `PASSES` passes of `PER_PASS` CPU canvases
 * made and read back, each handed to `hold` once read.
 *
 * @param {(canvas: Canvas) => void} hold
 */
async function residentAfterEachPass(hold) {
  const after = [];
  for (let pass = 0; pass < PASSES; pass++) {
    for (let i = 0; i < PER_PASS; i++) {
      const canvas = new Canvas(SIDE[0], SIDE[1]);
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      ctx.fillRect(10, 10, 80, 40);
      ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      hold(canvas);
    }
    await turn();
    after.push(process.memoryUsage().rss);
  }
  return after;
}

describe("Canvases that are dropped", () => {
  test("are collected without a forced collection", async () => {
    const { kept, message } = judged(await residentAfterEachPass(() => {}));
    assert.ok(kept, message);
  });

  test("are not taken for kept by a collection peak on the last pass", () => {
    const { kept, message } = judged(SAWTOOTH_ON_CI.map((mb) => mb * 1e6));
    assert.ok(kept, message);
  });

  // The control: the judgement has to refuse canvases that are never
  // collected, or the tests above show nothing. Last in the file, so that
  // what it keeps lands in no other test's measure.
  test("are told from canvases kept alive", async () => {
    /** @type {Canvas[]} */
    const held = [];
    const { kept, message } = judged(
      await residentAfterEachPass((canvas) => held.push(canvas)),
    );
    assert.equal(held.length, PASSES * PER_PASS);
    assert.ok(!kept, `kept canvases passed: ${message}`);
  });
});
