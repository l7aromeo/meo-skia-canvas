// @ts-check

"use strict";

// What CPU canvases made and dropped hold stays bounded without anything
// forcing a collection. Judged by the memory the process holds, as the GPU
// test, `dropped-gpu-canvas-memory.test.js`, judges it -- see
// `tests/support/live-canvases.js`, which both follow.
//
// The count of canvases alive would not hold. The pages on a thread are kept
// to a budget, and a page the budget drops stops being reported to V8, so
// what V8 is told stops near the budget. V8 starts a full collection when
// what it is told grows about 64 MB past where it stood at the last one,
// and where that last one landed as the budget filled, the next is out of
// reach: the canvases stay alive, uncollected, while what they hold does not
// grow. Whether a run's last collection lands there is timing, so a count
// passed some runs and failed others on the same build. What each dropped
// canvas still holds outside its page is not yet told to V8: #704.
//
// A file of its own, so that no other test's canvases land in what the
// process holds: node runs each test file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { afterEachPass, judgedMemory } = require("../support/live-canvases");

/** Bytes in a megabyte, the unit the series below are recorded in. */
const MB = 1e6;

// The series below are the footprint after each pass of the loop the first
// test runs -- `afterEachPass` -- recorded on 2026-11-13 with a debug build
// of this change on c99af2f9, on an M4 Pro Mac, by a script making the same
// canvases and reading the same column on the CPU, measured as the loop
// measures, rounded to the MB.

/**
 * The budget at its default and every canvas held, so that all 600 stay
 * alive while the budget drops their pages: what a run whose collections
 * stopped holds, with nothing left to chance. It flattens once the budget is
 * full, climbing by what each canvas keeps outside its page.
 */
const KEPT_TO_BUDGET = [
  63, 96, 129, 162, 194, 227, 260, 292, 305, 306, 306, 306, 306, 306, 307, 307,
  307, 307, 307, 308, 308, 308, 308, 308, 309, 309, 309, 309, 310, 310, 310,
  310, 310, 311, 311, 311, 311, 312, 312, 312, 313, 313, 313, 314, 314, 314,
  314, 315, 315, 315, 316, 318, 318, 318, 319, 319, 319, 320, 320, 320,
];

/**
 * The budget lifted with `__setPageBudget(Number.MAX_SAFE_INTEGER)` before
 * the loop, canvases dropped: every page is kept and reported, so V8
 * collects the canvases, never more than 80 alive. The judgement has to
 * accept this, which is how these canvases behave where V8 is told all a
 * page holds.
 */
const COLLECTED = [
  63, 96, 129, 129, 129, 129, 162, 194, 227, 259, 259, 259, 259, 259, 259, 259,
  259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259,
  259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259, 259,
  259, 259, 259, 259, 259, 261, 261, 261, 261, 261, 261, 261, 261, 261,
];

/**
 * The budget lifted as above and every canvas held: each keeps its page,
 * 32 MB a pass. The defect the first test exists to refuse -- pages kept
 * past the budget -- at the rate it takes on this machine.
 */
const KEPT_PAST_BUDGET = [
  63, 96, 129, 162, 194, 227, 259, 292, 325, 357, 390, 422, 455, 487, 519, 552,
  584, 617, 649, 682, 714, 747, 779, 812, 844, 877, 909, 942, 975, 1007, 1040,
  1072, 1105, 1137, 1170, 1202, 1235, 1267, 1300, 1332, 1365, 1397, 1430, 1463,
  1495, 1528, 1560, 1593, 1625, 1658, 1690, 1725, 1758, 1790, 1823, 1856, 1888,
  1921, 1953, 1986,
];

/** `series`, recorded in MB, in bytes. */
const bytes = (/** @type {number[]} */ series) => series.map((mb) => mb * MB);

describe("Canvases that are dropped", () => {
  test("hold a bounded amount without a forced collection", async (t) => {
    const { alive, memory } = await afterEachPass({
      gpu: false,
      hold: () => {},
      measured: true,
    });
    const { kept, message } = judgedMemory(memory);
    t.diagnostic(
      `${message}; canvases alive after each pass ${alive.join(", ")}`,
    );
    assert.ok(kept, message);
  });

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
  // keeping every page climbs 32 MB a pass to 2 GB.
  test("are told from pages kept past the budget", () => {
    const { kept, message } = judgedMemory(bytes(KEPT_PAST_BUDGET));
    assert.ok(!kept, `pages kept past the budget passed: ${message}`);
  });
});
