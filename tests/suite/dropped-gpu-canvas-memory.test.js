// @ts-check

"use strict";

// GPU canvases made and dropped are collected without anything forcing it,
// as CPU ones are -- see `dropped-canvas-memory.test.js`. A GPU page is held
// by the driver, and on macOS outside the process's resident memory, so what
// is judged here is what this addon has reported to V8 for the pages it
// holds, through `__reportedMemory`, which debug builds alone export. A
// canvas is counted until it is finalized, so the count falls when V8
// collects the dropped ones and climbs only if it never does. What is judged
// is its floor -- see `tests/support/memory-floor.js`.
//
// Skipped against a release binary, and where a canvas does not take the
// GPU: there the CPU test already judges the same pages.
//
// A file of its own, so that no other test's canvases land in the count:
// node runs each test file in a process of its own. The control, which keeps
// its canvases, runs last for the same reason.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon"),
  {
    PASSES,
    PER_PASS,
    afterEachPass,
    judged,
  } = require("../support/memory-floor");

const why = !skiaNode.__reportedMemory
  ? "skipped: a release binary does not export __reportedMemory"
  : new Canvas(1, 1).engine.renderer != "GPU"
    ? `skipped: a canvas takes the CPU here (${new Canvas(1, 1).engine.device})`
    : null;

/** What this addon has reported to V8, in bytes. */
const reported = () => /** @type {number} */ (skiaNode.__reportedMemory());

/** The count after each pass of GPU canvases handed to `hold`. */
const counted = (/** @type {(canvas: any) => void} */ hold) =>
  afterEachPass({ gpu: true, hold, measure: reported });

describe("GPU canvases that are dropped", () => {
  test(
    "are collected without a forced collection",
    { skip: why ?? false },
    async () => {
      const { kept, message } = judged(await counted(() => {}), "the count");
      assert.ok(kept, message);
    },
  );

  // The control: the judgement has to refuse canvases that are never
  // collected. Last in the file, so that what it keeps lands in no other
  // test's count.
  test(
    "are told from GPU canvases kept alive",
    { skip: why ?? false },
    async () => {
      /** @type {unknown[]} */
      const held = [];
      const { kept, message } = judged(
        await counted((canvas) => held.push(canvas)),
        "the count",
      );
      assert.equal(held.length, PASSES * PER_PASS);
      assert.ok(!kept, `kept canvases passed: ${message}`);
    },
  );
});
