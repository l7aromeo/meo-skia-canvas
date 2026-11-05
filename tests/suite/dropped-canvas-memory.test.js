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
// does between requests and Neon runs a finalizer only once it has. What is
// judged is the floor of resident memory -- see `tests/support/memory-floor.js`.
//
// The pages are on the CPU, where what a context keeps is resident. A page on
// the GPU is held by the driver, outside the process's resident memory on
// macOS: two hundred GPU canvases kept alive there, reporting 640 MB to V8,
// raised it by 6 MB, so a GPU page would pass with or without collection.
// `dropped-gpu-canvas-memory.test.js` judges those by what is reported to V8.
//
// A file of its own, so that no other test's memory lands in the process this
// measures: node runs each test file in a process of its own. The control,
// which keeps its canvases, runs last for the same reason.

const { assert, describe, test } = require("../runner"),
  {
    PASSES,
    PER_PASS,
    afterEachPass,
    judged,
  } = require("../support/memory-floor");

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

/** Resident bytes after each pass of CPU canvases handed to `hold`. */
const resident = (/** @type {(canvas: any) => void} */ hold) =>
  afterEachPass({ gpu: false, hold, measure: () => process.memoryUsage().rss });

describe("Canvases that are dropped", () => {
  test("are collected without a forced collection", async () => {
    const { kept, message } = judged(
      await resident(() => {}),
      "resident memory",
    );
    assert.ok(kept, message);
  });

  test("are not taken for kept by a collection peak on the last pass", () => {
    const { kept, message } = judged(
      SAWTOOTH_ON_CI.map((mb) => mb * 1e6),
      "resident memory",
    );
    assert.ok(kept, message);
  });

  // The control: the judgement has to refuse canvases that are never
  // collected, or the tests above show nothing. Last in the file, so that
  // what it keeps lands in no other test's measure.
  test("are told from canvases kept alive", async () => {
    /** @type {unknown[]} */
    const held = [];
    const { kept, message } = judged(
      await resident((canvas) => held.push(canvas)),
      "resident memory",
    );
    assert.equal(held.length, PASSES * PER_PASS);
    assert.ok(!kept, `kept canvases passed: ${message}`);
  });
});
