// @ts-check

"use strict";

// What GPU canvases made and dropped hold stays bounded without anything
// forcing a collection. Judged by the memory the process holds, where the CPU
// test, `dropped-canvas-memory.test.js`, judges the count of canvases alive --
// see `tests/support/live-canvases.js`, which both follow.
//
// The count would not hold here. The pages on a thread are kept to a budget,
// and V8 is told exactly what a canvas holds in pixels, which for these pages
// is too little to start a collection: V8 collects them on its own heap's
// schedule, so the count climbs with every pass while what they hold does
// not. What each dropped canvas still holds outside its page, its recording,
// is not yet told to V8, nor is a GPU page's multisample buffer: #704.
//
// Skipped where a canvas does not take the GPU: there the CPU test already
// judges the same canvases.
//
// A file of its own, so that no other test's canvases land in what the
// process holds: node runs each test file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  { afterEachPass, judgedMemory } = require("../support/live-canvases");

const why =
  new Canvas(1, 1).engine.renderer != "GPU"
    ? `skipped: a canvas takes the CPU here (${new Canvas(1, 1).engine.device})`
    : null;

/** Bytes in a megabyte, the unit the series below are recorded in. */
const MB = 1e6;

// The series below are the footprint after each pass of the loop the first
// test runs -- `afterEachPass` -- recorded on 2026-11-13 with a debug build
// of this change on e544cb9d, on an M4 Pro Mac taking the GPU through Metal,
// by a script making the same canvases and reading the same column, with
// `footprint -p <pid> -f bytes --noCategories` after each pass's turn,
// rounded to the MB.

/**
 * The condition the first test holds: the budget at its default, canvases
 * dropped. The budget keeps 256 MB of pages and drops the rest, and the 600
 * canvases stay alive, uncollected, adding their recordings.
 */
const KEPT_TO_BUDGET = [
  436, 554, 554, 555, 538, 555, 555, 555, 556, 556, 556, 556, 557, 557, 557,
  557, 557, 558, 558, 558, 558, 559, 559, 559, 559, 560, 560, 560, 560, 560,
  561, 561, 561, 561, 562, 545, 562, 563, 563, 563, 564, 564, 564, 564, 565,
  565, 565, 566, 566, 566, 550, 569, 569, 569, 569, 569, 569, 570, 570, 570,
];

/**
 * The budget lifted with `__setPageBudget(Number.MAX_SAFE_INTEGER)` before
 * the loop, canvases dropped: every page is kept, so V8 is told enough to
 * collect the canvases every three passes or so, never more than 41 alive.
 * The judgement has to accept this sawtooth, which is how these canvases
 * behave once V8 is told all a GPU page holds.
 */
const COLLECTED = [
  437, 604, 771, 689, 605, 602, 605, 772, 539, 622, 622, 789, 655, 588, 755,
  672, 588, 755, 538, 588, 755, 672, 588, 755, 672, 588, 755, 672, 588, 755,
  538, 588, 755, 672, 588, 755, 672, 588, 755, 672, 588, 755, 672, 588, 755,
  618, 589, 755, 672, 589, 755, 674, 591, 758, 674, 591, 758, 674, 591, 758,
];

/**
 * The budget lifted as above and every canvas held, stopped at 21 passes:
 * each canvas keeps its page and its multisample buffer, 167 MB a pass. The
 * defect the first test exists to refuse -- pages kept past the budget -- at
 * the rate it takes on this machine.
 */
const KEPT_PAST_BUDGET = [
  437, 604, 772, 939, 1106, 1273, 1440, 1607, 1775, 1942, 2109, 2276, 2443,
  2610, 2778, 2945, 3111, 3279, 3446, 3613, 3780,
];

/** `series`, recorded in MB, in bytes. */
const bytes = (/** @type {number[]} */ series) => series.map((mb) => mb * MB);

describe("GPU canvases that are dropped", () => {
  test(
    "hold a bounded amount without a forced collection",
    { skip: why ?? false },
    async (t) => {
      const { memory } = await afterEachPass({
        gpu: true,
        hold: () => {},
        measured: true,
      });
      const { kept, message } = judgedMemory(memory);
      t.diagnostic(message);
      assert.ok(kept, message);
    },
  );

  test("are not taken for growing when kept to the budget", () => {
    const { kept, message } = judgedMemory(bytes(KEPT_TO_BUDGET));
    assert.ok(kept, message);
  });

  test("are not taken for growing when collected", () => {
    const { kept, message } = judgedMemory(bytes(COLLECTED));
    assert.ok(kept, message);
  });

  // The control: the judgement has to refuse pages kept past the budget, or
  // the test above shows nothing. Recorded rather than run, because a run
  // keeping every page climbs 167 MB a pass until it stops.
  test("are told from pages kept past the budget", () => {
    const { kept, message } = judgedMemory(bytes(KEPT_PAST_BUDGET));
    assert.ok(!kept, `pages kept past the budget passed: ${message}`);
  });
});
