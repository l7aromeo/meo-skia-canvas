// @ts-check

"use strict";

// What a canvas or an image holds natively is reported to V8 as external
// memory, and released when the object is finalized, so the engine schedules
// collections by it: a canvas's page in pixels, with a GPU page's
// multisample buffer, its recording, as Skia estimates its pictures, and a
// fixed figure for its context. These read what this addon has reported and
// the engine accepted, through `__reportedMemory`, which debug builds alone
// export: against a release binary they are skipped by name. The engine's own total,
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
  { skiaNode } = require("../../lib/classes/neon"),
  { processMemory } = require("../support/live-canvases");

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
 * What a context reports whatever is drawn on it: `CONTEXT_BYTES` in
 * src/context/mod.rs, read from the build under test.
 */
const CONTEXT_BYTES = why ? 0 : skiaNode.__contextBytes();

/**
 * A buffer held across the reads, which is none of what this addon reports.
 * Odd, so that no pixel arithmetic lands on it.
 */
const HELD_BYTES = 146_209;

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/** What one page of `SIDE` holds in pixels. */
const PAGE_BYTES = SIDE[0] * SIDE[1] * PIXEL_BYTES;

/**
 * The samples a GPU page takes for each pixel where nothing asks otherwise:
 * the default 4x of `lib/classes/canvas.js`, kept in a buffer beside the
 * page's own pixels.
 */
const DEFAULT_SAMPLES = 4;

/**
 * A canvas of `SIDE` on the CPU, whose page holds `PAGE_BYTES` and nothing
 * beside them: a GPU page keeps a sample buffer too, which the GPU test
 * counts.
 */
const cpuCanvas = () => {
  const canvas = new Canvas(SIDE[0], SIDE[1]);
  canvas.gpu = false;
  return canvas;
};

/**
 * A CPU canvas with `fills` rectangles filled on it and read back, so that
 * its page and its recording are both reported.
 */
const drawn = (/** @type {number} */ fills) => {
  const canvas = cpuCanvas();
  const ctx = canvas.getContext("2d");
  for (let i = 0; i < fills; i++) ctx.fillRect(i % 900, (i * 7) % 700, 80, 40);
  ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
  return canvas;
};

/**
 * Contexts made for a measurement of the process's memory: enough that
 * what they hold, about 5 KB each, stands well clear of the memory a
 * process moves by on its own between two readings.
 */
const CONTEXTS = 4000;

/**
 * How far what a context reports may stand from what the process grows by
 * natively for each, as a share of that growth. Measured on this Mac at
 * 0.78 to 0.91 over ten batches; a context reporting nothing, or twice what
 * it holds, lands outside.
 */
const CONTEXT_SHARE = [0.6, 1.4];

/** Fills drawn on a page whose recording is to weigh something. */
const FILLS = 1000;

/**
 * The least one fill can add to a recording: the rectangle it fills, four
 * 32-bit floats, whatever else Skia's record of the draw holds.
 */
const FILL_RECORD_BYTES = 16;

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
      // Nothing drawn, so that the recording holds nothing and the page is
      // all there is to count.
      const live = [cpuCanvas()];
      const held = Buffer.alloc(HELD_BYTES);
      live[0].getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      assert.equal(
        (await settled()) - before,
        PAGE_BYTES + CONTEXT_BYTES,
        "the page and the context a live canvas holds",
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
      const canvas = cpuCanvas();
      canvas.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      assert.equal(
        (await settled()) - before,
        PAGE_BYTES + CONTEXT_BYTES,
        "the page kept before the resize",
      );
      canvas.width = SIDE[0];
      assert.equal(
        (await settled()) - before,
        CONTEXT_BYTES,
        "what a canvas whose page a resize dropped counts",
      );
      assert.equal(canvas.width, SIDE[0]);
    },
  );

  // The budget the pages on a thread keep to drops one canvas's page during
  // another's read: the engine is told then, and the dropped canvas's
  // finalizer releases only what it still reports.
  test(
    "stops counting a page the budget drops, and releases it once",
    { skip: why ?? false },
    async () => {
      const page = PAGE_BYTES;
      const before = await settled();
      // A budget of one page: the second canvas's read drops the first's.
      skiaNode.__setPageBudget(page);
      try {
        const live = [cpuCanvas()];
        live[0].getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
        assert.equal(
          (await settled()) - before,
          page + CONTEXT_BYTES,
          "the first page",
        );
        const second = cpuCanvas();
        second.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
        assert.equal(
          (await settled()) - before,
          page + 2 * CONTEXT_BYTES,
          "the first page dropped and the second kept",
        );
        live.pop();
        assert.equal(
          (await settled()) - before,
          page + CONTEXT_BYTES,
          "what the dropped canvas's finalizer releases",
        );
        assert.equal(second.width, SIDE[0]);
      } finally {
        skiaNode.__setPageBudget(null);
      }
    },
  );

  test(
    "counts what a canvas's recording holds, by what is drawn",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      const live = [drawn(1)];
      const canvas = PAGE_BYTES + CONTEXT_BYTES;
      const one = (await settled()) - before - canvas;
      assert.ok(one > 0, `one fill's recording counts: ${one} bytes`);
      live.push(drawn(FILLS));
      const many = (await settled()) - before - 2 * canvas - one;
      assert.ok(
        many - one >= (FILLS - 1) * FILL_RECORD_BYTES,
        `${FILLS} fills' recording counts ${many} bytes, one fill's ${one}`,
      );
      live.length = 0;
      assert.equal(
        (await settled()) - before,
        0,
        "what stays counted once the canvases are finalized",
      );
    },
  );

  // The budget drops a page's pixels and never its recording, so a canvas
  // whose page was dropped still counts what it recorded until it is
  // finalized.
  test(
    "keeps counting a dropped page's recording until it is finalized",
    { skip: why ?? false },
    async () => {
      const before = await settled();
      skiaNode.__setPageBudget(PAGE_BYTES);
      try {
        // Drawn through `drawn`, so that no variable here names its
        // context and keeps the canvas alive.
        const live = [drawn(FILLS)];
        const recorded =
          (await settled()) - before - PAGE_BYTES - CONTEXT_BYTES;
        assert.ok(recorded > 0, `the recording counts: ${recorded} bytes`);
        const second = cpuCanvas();
        second.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
        assert.equal(
          (await settled()) - before,
          PAGE_BYTES + recorded + 2 * CONTEXT_BYTES,
          "the first page dropped, its recording, both contexts and the " +
            "second page kept",
        );
        live.pop();
        assert.equal(
          (await settled()) - before,
          PAGE_BYTES + CONTEXT_BYTES,
          "what the dropped canvas's finalizer releases",
        );
        assert.equal(second.width, SIDE[0]);
      } finally {
        skiaNode.__setPageBudget(null);
      }
    },
  );

  test(
    "counts a GPU page's multisample buffer",
    {
      skip:
        why ??
        (new Canvas(1, 1).engine.renderer != "GPU" ? "no GPU here" : false),
    },
    async () => {
      const before = await settled();
      const live = [new Canvas(SIDE[0], SIDE[1])];
      live[0].gpu = true;
      live[0].getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      assert.equal(live[0].engine.renderer, "GPU", "the canvas is on the GPU");
      assert.equal(
        (await settled()) - before,
        PAGE_BYTES * (1 + DEFAULT_SAMPLES) + CONTEXT_BYTES,
        "the page, a sample buffer of four of it and the context",
      );
      live.pop();
      assert.equal(
        (await settled()) - before,
        0,
        "what stays counted once the canvas is finalized",
      );
    },
  );

  // `CONTEXT_BYTES` is a measurement, and goes stale when what a context
  // holds changes. Held against what the process grows by natively as
  // contexts are made and kept: a batch of canvases with contexts against
  // one of canvases alone, the engine's own heap taken out of each, within
  // a share either way, since allocators round and the binding keeps a
  // little of its own for each object.
  test(
    "reports about what a context holds, by the process's growth",
    { skip: why ?? false },
    async () => {
      const live = [];
      /** What the process grows by natively for each canvas `make` makes. */
      const grownFor = async (/** @type {() => Canvas} */ make) => {
        await settled();
        const memory = processMemory();
        const heap = process.memoryUsage().heapTotal;
        for (let i = 0; i < CONTEXTS; i++) live.push(make());
        await settled();
        const engine = process.memoryUsage().heapTotal - heap;
        return (processMemory() - memory - engine) / CONTEXTS;
      };
      const bare = await grownFor(cpuCanvas);
      const before = await settled();
      const grown =
        (await grownFor(() => {
          const canvas = cpuCanvas();
          // Nothing drawn, so that the context is all a canvas reports.
          canvas.getContext("2d");
          return canvas;
        })) - bare;
      const reported = ((await settled()) - before) / CONTEXTS;
      assert.equal(reported, CONTEXT_BYTES, "each context reports itself");
      assert.ok(
        reported >= grown * CONTEXT_SHARE[0] &&
          reported <= grown * CONTEXT_SHARE[1],
        `each context reports ${reported} bytes where the process grew ` +
          `natively by ${Math.round(grown)} for each`,
      );
      assert.equal(live.length, 2 * CONTEXTS);
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
