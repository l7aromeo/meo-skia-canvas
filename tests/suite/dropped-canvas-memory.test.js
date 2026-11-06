// @ts-check

"use strict";

// Canvases made and dropped are collected without anything forcing it. A
// context is a small object on V8's heap however large the page it holds, so
// until that page was reported as external memory a process making canvases
// and allocating little JavaScript was never collected: every canvas it made
// stayed alive, and with it the 3.2 MB each 1000 by 800 page holds.
//
// No forced collection here, on purpose: what is tested is that V8 decides to
// collect on its own. The loop does turn between passes, because a server
// does between requests and Neon runs a finalizer only once it has. What is
// judged is the count of canvases still alive -- see
// `tests/support/live-canvases.js`.
//
// What is not judged here is memory left behind once a canvas is finalized:
// a page its finalizer fails to free stays resident with no live canvas to
// count. That a page is reported to V8 while its canvas lives, and released
// when it is finalized, is `tests/suite/external-memory.test.js`.
//
// A file of its own, so that no other test's canvases land in the count: node
// runs each test file in a process of its own. The control, which keeps its
// canvases, runs last for the same reason.

const { assert, describe, test } = require("../runner"),
  {
    PASSES,
    PER_PASS,
    afterEachPass,
    judged,
  } = require("../support/live-canvases");

/**
 * The canvases alive after each of 60 passes on this Mac's debug build, read
 * one column wide as the test reads them: collected every three or four
 * passes, never more than 60 alive.
 */
const COLLECTED_OFTEN = [
  10, 20, 30, 40, 50, 60, 20, 30, 40, 30, 40, 50, 27, 37, 47, 26, 36, 46, 19,
  29, 39, 19, 29, 39, 49, 29, 39, 49, 19, 29, 39, 19, 29, 39, 19, 29, 39, 19,
  29, 39, 19, 29, 39, 49, 29, 39, 49, 19, 29, 39, 19, 5, 15, 25, 35, 19, 29, 39,
  19, 29,
];

/**
 * The canvases alive after each of 60 passes on this Mac's debug build, each
 * read a pixel at a time on a 128-pixel grid, which reports 128 KB of a page
 * where a column reports all 3.2 MB, so V8 waits longer between collections:
 * collected 25 passes apart, with 260 alive at the peak, then every 8 to 10.
 */
const COLLECTED_FAR_APART = [
  10, 20, 30, 40, 50, 60, 70, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120,
  130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240, 250, 260, 20, 30,
  40, 50, 60, 70, 80, 90, 100, 110, 20, 30, 40, 50, 60, 70, 80, 90, 20, 30, 40,
  50, 60, 70, 80, 90, 100, 110,
];

/** The count, and what is resident, after each pass of CPU canvases. */
const counted = (/** @type {(canvas: any) => void} */ hold) =>
  afterEachPass({ gpu: false, hold });

describe("Canvases that are dropped", () => {
  test("are collected without a forced collection", async (t) => {
    const { alive, resident } = await counted(() => {});
    const { kept, message } = judged(alive, resident);
    t.diagnostic(message);
    assert.ok(kept, message);
  });

  test("are not taken for kept when collected often", () => {
    const { kept, message } = judged(COLLECTED_OFTEN);
    assert.ok(kept, message);
  });

  test("are not taken for kept when collected passes apart", () => {
    const { kept, message } = judged(COLLECTED_FAR_APART);
    assert.ok(kept, message);
  });

  // The control: the judgement has to refuse canvases that are never
  // collected, or the tests above show nothing. Last in the file, so that
  // what it keeps lands in no other test's count.
  test("are told from canvases kept alive", async (t) => {
    /** @type {unknown[]} */
    const held = [];
    const { alive, resident } = await counted((canvas) => held.push(canvas));
    const { kept, message } = judged(alive, resident);
    t.diagnostic(message);
    assert.equal(held.length, PASSES * PER_PASS);
    assert.ok(!kept, `kept canvases passed: ${message}`);
  });
});
