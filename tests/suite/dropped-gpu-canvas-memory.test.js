// @ts-check

"use strict";

// GPU canvases made and dropped are collected without anything forcing it,
// as CPU ones are -- see `dropped-canvas-memory.test.js`, which this file
// follows: what is judged is the count of canvases still alive -- see
// `tests/support/live-canvases.js` -- so that a GPU page, held by the driver
// and on macOS outside the process's resident memory, is judged as a CPU one
// is.
//
// Skipped where a canvas does not take the GPU: there the CPU test already
// judges the same canvases.
//
// A file of its own, so that no other test's canvases land in the count: node
// runs each test file in a process of its own. The control, which keeps its
// canvases, runs last for the same reason.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  {
    PASSES,
    PER_PASS,
    afterEachPass,
    judged,
  } = require("../support/live-canvases");

const why =
  new Canvas(1, 1).engine.renderer != "GPU"
    ? `skipped: a canvas takes the CPU here (${new Canvas(1, 1).engine.device})`
    : null;

/** The count, and what is resident, after each pass of GPU canvases. */
const counted = (/** @type {(canvas: any) => void} */ hold) =>
  afterEachPass({ gpu: true, hold });

describe("GPU canvases that are dropped", () => {
  test(
    "are collected without a forced collection",
    { skip: why ?? false },
    async (t) => {
      const { alive, resident } = await counted(() => {});
      const { kept, message } = judged(alive, resident);
      t.diagnostic(message);
      assert.ok(kept, message);
    },
  );

  // The control: the judgement has to refuse canvases that are never
  // collected. Last in the file, so that what it keeps lands in no other
  // test's count.
  test(
    "are told from GPU canvases kept alive",
    { skip: why ?? false },
    async (t) => {
      /** @type {unknown[]} */
      const held = [];
      const { alive, resident } = await counted((canvas) => held.push(canvas));
      const { kept, message } = judged(alive, resident);
      t.diagnostic(message);
      assert.equal(held.length, PASSES * PER_PASS);
      assert.ok(!kept, `kept canvases passed: ${message}`);
    },
  );
});
