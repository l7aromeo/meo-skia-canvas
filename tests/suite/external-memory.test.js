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
 * Contexts made for a measurement of what the C allocator holds: enough
 * that what they hold, about 5 KB each, stands well clear of what the
 * process allocates on its own between two readings.
 */
const CONTEXTS = 4000;

/**
 * How far what a context reports may stand from what the C allocator
 * counts in use for each, as a share of that count. `CONTEXT_BYTES`, 4,800,
 * stood at 1.04 of glibc's count (4,590 to 4,613 over nine batches) and at
 * 0.95 of macOS's (5,001 to 5,045); a context reporting nothing, or twice
 * what it holds, lands outside.
 */
const CONTEXT_SHARE = [0.9, 1.1];

/**
 * The C allocator's count of bytes in use, `__mallocInUse`, which debug
 * builds on glibc and macOS export. A debug build there that lacks it fails
 * rather than skips, so that a broken export cannot switch the check off.
 */
const glibc = process.report?.getReport().header.glibcVersionRuntime;
const allocatorAnswers = process.platform === "darwin" || Boolean(glibc);
const allocatorWhy =
  why ??
  (allocatorAnswers
    ? null
    : `skipped: ${process.platform} without glibc has no allocator count to read`);

/** Native buffers held to show the allocator's count moves by what is held. */
const HELD_BUFFERS = 4000;

/**
 * Each held buffer's bytes: past the size a `Buffer` takes from Node's
 * shared pool, so that each is an allocation of its own.
 */
const HELD_BUFFER_BYTES = 4096;

/**
 * Draws of one canvas into another, more than one so that a count made for
 * each draw rather than for each image lands outside what the test allows.
 */
const DRAWS = 3;

/** Frames a changing canvas is drawn into a page never cleared. */
const FRAMES = 24;

/**
 * Canvases dropped while a count is being taken: enough that their
 * contexts move the count by more than anything else the process does
 * between two readings, few enough to cost nothing.
 */
const LATE_CANVASES = 10;

/** Fills drawn on a page whose recording is to weigh something. */
const FILLS = 1000;

/**
 * The least one fill can add to a recording: the rectangle it fills, four
 * 32-bit floats, whatever else Skia's record of the draw holds.
 */
const FILL_RECORD_BYTES = 16;

/**
 * Rounds of a collection and a turn that `settled` reads in a row without
 * the count moving before it takes the count: two, so that an object
 * dropped during one round's turn, after its collection, is collected in
 * the next and finalized on that round's turn, and the round after reads
 * it gone.
 */
const QUIET_ROUNDS = 2;

/**
 * The most rounds `settled` takes before it gives up and fails: a count
 * still moving this long is a defect to report rather than one to wait
 * out, and a test that hangs reports nothing.
 */
const SETTLE_ROUNDS = 64;

/** What this addon reports now. */
const reportedNow = () => {
  const reported = skiaNode.__reportedMemory();
  assert.equal(typeof reported, "number", "the addon reports its count");
  return /** @type {number} */ (reported);
};

/**
 * What this addon has reported once everything collectable has been
 * collected and finalized: read after each collection and turn, until
 * `QUIET_ROUNDS` in a row leave it where it stood. Neon runs a finalizer
 * only once the event loop turns, so a collection alone leaves a dropped
 * object counted, and an object dropped while the count is being taken
 * is finalized a round later than one dropped before.
 */
const settled = async () => {
  let reported = reportedNow();
  let quiet = 0;
  for (let round = 0; round < SETTLE_ROUNDS; round++) {
    collect();
    await turn();
    const now = reportedNow();
    quiet = now === reported ? quiet + 1 : 0;
    reported = now;
    if (quiet === QUIET_ROUNDS) return reported;
  }
  assert.fail(
    `the count still moved after ${SETTLE_ROUNDS} rounds, at ${reported}`,
  );
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

  // A canvas drawn into another is its own pixels, held by the destination's
  // recording and by the source as the image of its page. The pictures that
  // hold it count none of its pixels, so the page counts the image itself,
  // once however often it is drawn.
  test(
    "counts a canvas drawn into another once, on both canvases",
    { skip: why ?? false },
    async () => {
      const source = drawn(1);
      const before = await settled();
      const dest = cpuCanvas();
      const ctx = dest.getContext("2d");
      for (let i = 0; i < DRAWS; i++) ctx.drawImage(source, 0, 0);
      // The whole page, as `drawn` reads it: a smaller read is served from
      // tiles, which hold less than a page.
      ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      // A read is what has the source report what it holds again.
      source.getContext("2d").getImageData(0, 0, 1, 1);
      const held = (await settled()) - before - PAGE_BYTES - CONTEXT_BYTES;
      assert.ok(
        held >= 2 * PAGE_BYTES && held < 2 * PAGE_BYTES + PAGE_BYTES / 2,
        `${DRAWS} draws of one canvas count its image on each canvas: ` +
          `${held} bytes beside the destination's page, one image ` +
          `${PAGE_BYTES}`,
      );
      assert.equal(dest.width, SIDE[0]);
    },
  );

  // `drawCanvas` of a canvas that holds a drawn canvas lands its pixels as
  // `drawImage` does, and the destination counts that image as `drawImage`'s
  // does: once however often it is drawn, on both canvases.
  test(
    "counts a canvas holding a canvas drawn in with drawCanvas once, on both canvases",
    { skip: why ?? false },
    async () => {
      const source = drawn(1);
      source.getContext("2d").drawImage(drawn(1), 0, 0);
      source.getContext("2d").getImageData(0, 0, SIDE[0], SIDE[1]);
      const before = await settled();
      const dest = cpuCanvas();
      const ctx = dest.getContext("2d");
      for (let i = 0; i < DRAWS; i++) ctx.drawCanvas(source, 0, 0);
      ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
      source.getContext("2d").getImageData(0, 0, 1, 1);
      const held = (await settled()) - before - PAGE_BYTES - CONTEXT_BYTES;
      assert.ok(
        held >= 2 * PAGE_BYTES && held < 2 * PAGE_BYTES + PAGE_BYTES / 2,
        `${DRAWS} draws of one canvas count its image on each canvas: ` +
          `${held} bytes beside the destination's page, one image ` +
          `${PAGE_BYTES}`,
      );
      assert.equal(dest.width, SIDE[0]);
    },
  );

  // A page never cleared, into which a changing canvas is drawn every frame,
  // holds an image of it for each frame until its entries hold more than a
  // few pages of them, and then lets go of them for one of its own: what is
  // reported levels off where it grew by a page a frame.
  test(
    "levels off for a page a changing canvas is drawn into every frame",
    { skip: why ?? false },
    async () => {
      const source = drawn(1);
      const sctx = source.getContext("2d");
      const before = await settled();
      const dest = cpuCanvas();
      const ctx = dest.getContext("2d");
      ctx.globalAlpha = 0.5;
      let most = 0;
      for (let frame = 0; frame < FRAMES; frame++) {
        sctx.fillRect(frame * 7, 0, 7, 40);
        ctx.drawImage(source, 0, 0);
        ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
        most = Math.max(most, (await settled()) - before);
      }
      // Kept, every frame's image adds a page; flattened, the count stays
      // under half that, whatever the page's budget is.
      assert.ok(
        most < (FRAMES / 2) * PAGE_BYTES,
        `${FRAMES} frames held at most ${most} bytes, a page ${PAGE_BYTES}`,
      );
      assert.equal(dest.width, SIDE[0]);
    },
  );

  // A page never cleared, filled every frame with a pattern of a changing
  // canvas, holds a raster of the canvas's page for each frame's pattern,
  // and for a canvas holding a drawn canvas its image too. The page counts
  // them as it holds them, and lets them go for its own pixels once they
  // come to more than a few pages, as it does a canvas drawn into it.
  for (const nesting of [false, true]) {
    test(
      `counts a pattern of a ${nesting ? "nesting " : ""}canvas filled every frame, and levels off`,
      { skip: why ?? false },
      async () => {
        const source = cpuCanvas();
        const sctx = source.getContext("2d");
        const inner = drawn(1);
        const before = await settled();
        const dest = cpuCanvas();
        const ctx = dest.getContext("2d");
        ctx.globalAlpha = 0.5;
        const held = [];
        for (let frame = 0; frame < FRAMES; frame++) {
          sctx.fillStyle = `hsl(${frame * 15}, 50%, 50%)`;
          sctx.fillRect(0, 0, SIDE[0], SIDE[1]);
          if (nesting) sctx.drawImage(inner, 0, 0);
          ctx.fillStyle = ctx.createPattern(source, "repeat");
          ctx.fillRect(0, 0, SIDE[0], SIDE[1]);
          ctx.getImageData(0, 0, SIDE[0], SIDE[1]);
          held.push((await settled()) - before);
        }
        // Two frames in, below any flatten, two rasters at least are held
        // beside the destination's page and its context. The source never
        // reads its own page, so it reports none.
        const early = held[1] - PAGE_BYTES - CONTEXT_BYTES;
        assert.ok(
          early >= 2 * PAGE_BYTES,
          `two frames' patterns counted ${early} bytes, a page ${PAGE_BYTES}`,
        );
        const most = Math.max(...held);
        assert.ok(
          most < (FRAMES / 2) * PAGE_BYTES,
          `${FRAMES} frames held at most ${most} bytes, a page ${PAGE_BYTES}`,
        );
        assert.equal(inner.width, SIDE[0]);
      },
    );
  }

  // A pattern of a canvas holds the canvas's page as it stood, and for a
  // canvas holding a drawn canvas its own pixels as well, which the canvas
  // stops holding once it draws on. The pattern counts them until it is
  // collected: the page's held image and its own pixels for a nesting
  // canvas, nothing past its pictures for a plain one.
  for (const nesting of [false, true]) {
    test(
      `counts the pixels a pattern of a ${nesting ? "nesting " : ""}canvas holds until it is collected`,
      { skip: why ?? false },
      async () => {
        const source = cpuCanvas();
        const sctx = source.getContext("2d");
        const inner = drawn(1);
        sctx.fillRect(0, 0, SIDE[0], SIDE[1]);
        if (nesting) sctx.drawImage(inner, 0, 0);
        const ctx = cpuCanvas().getContext("2d");
        /** @type {unknown} */
        let pattern = ctx.createPattern(source, "repeat");
        assert.ok(pattern, "a pattern of the canvas");
        const held = await settled();
        pattern = null;
        const released = held - (await settled());
        assert.equal(pattern, null);
        assert.equal(
          released,
          nesting ? 2 * PAGE_BYTES : 0,
          `the pattern released ${released} bytes, a page ${PAGE_BYTES}`,
        );
        assert.equal(inner.width, SIDE[0]);
      },
    );
  }

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

  test(
    "has the C allocator's count where a debug build exports it",
    { skip: why ?? false },
    () => {
      assert.equal(
        typeof skiaNode.__mallocInUse === "function",
        allocatorAnswers,
        `__mallocInUse is exported on ${process.platform}` +
          (glibc ? ` with glibc ${glibc}` : ""),
      );
    },
  );

  // The instrument the next test reads, shown to move by what is held:
  // buffers of native memory, none of which this addon reports.
  test(
    "the C allocator's count moves by the native memory held",
    { skip: allocatorWhy ?? false },
    async () => {
      await settled();
      const before = skiaNode.__mallocInUse();
      const held = Array.from({ length: HELD_BUFFERS }, () =>
        Buffer.alloc(HELD_BUFFER_BYTES),
      );
      await settled();
      const counted = skiaNode.__mallocInUse() - before;
      const expected = HELD_BUFFERS * HELD_BUFFER_BYTES;
      assert.ok(
        counted >= expected * CONTEXT_SHARE[0] &&
          counted <= expected * CONTEXT_SHARE[1],
        `${held.length} buffers of ${HELD_BUFFER_BYTES} bytes moved the ` +
          `count by ${counted}`,
      );
    },
  );

  // `CONTEXT_BYTES` is a measurement, and goes stale when what a context
  // holds changes. Held against what the C allocator counts in use as
  // contexts are made and kept: a batch of canvases with contexts against
  // one of canvases alone, within a share either way, since allocators
  // round and the binding keeps a little of its own for each object. Not
  // against what the process holds in pages: on glibc that moved by 854 to
  // 5,890 bytes a context over nine batches, as pages freed earlier were
  // reused or the heap grew, where the allocator's count moved by 4,590 to
  // 4,613.
  test(
    "reports about what a context holds, by the allocator's count",
    { skip: allocatorWhy ?? false },
    async () => {
      const live = [];
      /** What the allocator holds in use for each canvas `make` makes. */
      const allocatedFor = async (/** @type {() => Canvas} */ make) => {
        await settled();
        const before = skiaNode.__mallocInUse();
        for (let i = 0; i < CONTEXTS; i++) live.push(make());
        await settled();
        return (skiaNode.__mallocInUse() - before) / CONTEXTS;
      };
      const bare = await allocatedFor(cpuCanvas);
      const before = await settled();
      const allocated =
        (await allocatedFor(() => {
          const canvas = cpuCanvas();
          // Nothing drawn, so that the context is all a canvas reports.
          canvas.getContext("2d");
          return canvas;
        })) - bare;
      const reported = ((await settled()) - before) / CONTEXTS;
      assert.equal(reported, CONTEXT_BYTES, "each context reports itself");
      assert.ok(
        reported >= allocated * CONTEXT_SHARE[0] &&
          reported <= allocated * CONTEXT_SHARE[1],
        `each context reports ${reported} bytes where the allocator counts ` +
          `${Math.round(allocated)} in use for each`,
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

  // A canvas a test drops can be finalized after the next test has taken
  // its count, and lands in that test's reading as a release it never
  // made: 4,000 contexts the allocator's test held once did, -19,196,593
  // where 3,407 was expected (#824). Dropped here on the turn `settled`
  // collects on, so that its finalizer is still waiting when a count taken
  // a round too early would be read.
  test(
    "takes its count once a canvas dropped while counting is released",
    { skip: why ?? false },
    async () => {
      const live = Array.from({ length: LATE_CANVASES }, () => {
        const canvas = cpuCanvas();
        canvas.getContext("2d");
        return canvas;
      });
      const held = await settled();
      setImmediate(() => {
        live.length = 0;
      });
      const before = await settled();
      assert.equal(
        held - before,
        LATE_CANVASES * CONTEXT_BYTES,
        "the contexts dropped while the count was taken",
      );
      assert.equal(
        (await settled()) - before,
        0,
        "what is released after the count was taken",
      );
    },
  );
});
