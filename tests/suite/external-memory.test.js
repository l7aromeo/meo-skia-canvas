// @ts-check

"use strict";

// What a canvas or an image holds natively is reported to V8 as external
// memory, and released when the object is finalized, so the engine schedules
// collections by it. These read what this addon has reported and the engine
// accepted, through `__reportedMemory`, which debug builds alone export:
// against a release binary they are skipped by name. The engine's own total,
// which `napi_adjust_external_memory` returns, cannot stand in for it: it
// counts every live array buffer in the process as well, so a buffer held
// for any other reason lands in it. The first test holds one to keep that
// difference visible.
//
// A file of its own, because the count is the process's: node runs each test
// file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { setFlagsFromString } = require("node:v8"),
  { runInNewContext } = require("node:vm"),
  { setImmediate: turn } = require("node:timers/promises"),
  { Canvas, Image } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

// The suite runs without `--expose-gc`; a context made after the flag is set
// carries `gc`.
setFlagsFromString("--expose-gc");
const collect = runInNewContext("gc");

const why = !skiaNode.__reportedMemory
  ? "skipped: a release binary does not export __reportedMemory"
  : null;

/** The canvas read back. */
const SIDE = [1000, 800];

/**
 * A buffer held across the reads, which is none of what this addon reports.
 * Odd, so that no pixel arithmetic lands on it.
 */
const HELD_BYTES = 146_209;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/**
 * What this addon has reported once everything collectable has been
 * collected and finalized. Neon runs a finalizer only once the event loop
 * turns, so a collection alone leaves a dropped object counted.
 */
const settled = async () => {
  collect();
  await turn();
  collect();
  const reported = skiaNode.__reportedMemory();
  assert.equal(typeof reported, "number", "the addon reports its count");
  return /** @type {number} */ (reported);
};

describe("Native memory reported to V8", () => {
  test(
    "counts a page read back for as long as its canvas lives",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      // Held only through this array, so that emptying it is what lets the
      // canvas go: a variable naming it or its context would keep it.
      const live = [new Canvas(SIDE[0], SIDE[1])];
      const held = Buffer.alloc(HELD_BYTES);
      live[0].getContext("2d").fillRect(10, 10, 80, 40);
      live[0].getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      assert.equal(
        (await settled()) - before,
        SIDE[0] * SIDE[1] * PIXEL_BYTES,
        "the page a live canvas holds",
      );

      live.pop();
      assert.equal(
        (await settled()) - before,
        0,
        "what stays counted once the canvas is finalized",
      );
      assert.equal(held.length, HELD_BYTES);
    },
  );

  test(
    "drops the page from the count when a resize drops it",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      const canvas = new Canvas(SIDE[0], SIDE[1]);
      canvas.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      canvas.width = SIDE[0];
      assert.equal(
        (await settled()) - before,
        0,
        "what a canvas whose page a resize dropped counts",
      );
      assert.equal(canvas.width, SIDE[0]);
    },
  );

  test(
    "counts an image's encoded bytes until it is finalized",
    { skip: why ?? false },
    async () => {
      const source = new Canvas(SIDE[0], SIDE[1]);
      source.getContext("2d").fillRect(10, 10, 80, 40);
      const encoded = source.toBufferSync("png");

      const before = await settled();
      const live = [new Image()];
      live[0].src = encoded;
      assert.ok(live[0].complete, "the PNG decoded");
      assert.equal(
        (await settled()) - before,
        encoded.length,
        "what an image decoded on demand counts",
      );

      live.pop();
      assert.equal(
        (await settled()) - before,
        0,
        "what stays counted once the image is finalized",
      );
    },
  );
});
