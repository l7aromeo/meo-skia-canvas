// @ts-check

"use strict";

// A draw the GPU discarded -- which a GPU recovery does to the command
// buffers in flight in every other process -- is drawn once more, and a
// second failure rejects rather than handing back blank or stale pixels.
// No test can make the GPU fail safely, so these arm a seam in the native
// half that makes the next draws report a discard. The seam is compiled into
// debug builds only: against a release binary these tests are skipped by
// name, as they are on a machine with no GPU engine.
//
// The seam is process-wide, so this file keeps to itself: node runs each test
// file in its own process, and the tests here run one after another.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

const DISCARDED = /^The GPU discarded the work of /;

/** @returns {Canvas} */
const drawn = (width = 64, height = 48) => {
  const canvas = new Canvas(width, height);
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "rgb(30 120 210)";
  ctx.fillRect(0, 0, width, height);
  return canvas;
};

const why = !skiaNode.__discardGpuDraws
  ? "skipped: a release binary has no seam to arm"
  : !drawn().gpu
    ? "skipped: no GPU engine, so no draw can be discarded"
    : null;
const discard = (/** @type {number} */ times) =>
  skiaNode.__discardGpuDraws(times);

describe("A draw the GPU discarded is retried once, then refused", () => {
  test(
    "an export whose draw was discarded once is drawn again",
    { skip: why ?? false },
    async () => {
      const want = await drawn().toBuffer("png");
      discard(1);
      const got = await drawn().toBuffer("png");
      assert.ok(Buffer.from(got).equals(Buffer.from(want)));
    },
  );

  test(
    "an export discarded twice rejects with an Error naming the stage",
    { skip: why ?? false },
    async () => {
      const canvas = drawn();
      discard(2);
      const error = await canvas.toBuffer("png").catch((e) => e);
      assert.ok(error instanceof Error, `resolved with ${error?.length} bytes`);
      assert.match(error.message, DISCARDED);
      assert.match(error.message, /compositing the page/);
    },
  );

  test(
    "getImageData whose draw was discarded twice throws",
    { skip: why ?? false },
    () => {
      // Wider than one tile, so the read is served by the whole page.
      const ctx = drawn(600, 300).getContext("2d");
      discard(2);
      assert.throws(
        () => ctx.getImageData(0, 0, 600, 300),
        (/** @type {Error} */ e) =>
          DISCARDED.test(e.message) &&
          /drawing the page for a read/.test(e.message),
      );
    },
  );

  test(
    "a small getImageData served by tiles throws too",
    { skip: why ?? false },
    () => {
      const ctx = drawn(1024, 1024).getContext("2d");
      discard(2);
      assert.throws(
        () => ctx.getImageData(0, 0, 8, 8),
        (/** @type {Error} */ e) =>
          DISCARDED.test(e.message) &&
          /drawing tiles for a read/.test(e.message),
      );
    },
  );
});
