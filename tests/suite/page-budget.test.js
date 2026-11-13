// @ts-check

"use strict";

// The pages on a thread keep what their reads composited while what they
// keep stays inside a budget. A read that takes the thread past it drops what
// pages read only once keep, the oldest first; a page read more than once is
// never dropped. Canvases are freed by their finalizers, which
// Node runs only on the turn of the event loop after a collection, so a
// synchronous loop that makes and reads canvases is held to the budget
// rather than keeping every page until it ends.
//
// These read the budget's own count, and set it, through hooks debug builds
// alone export; against a release binary they are skipped by name. Which
// page is dropped, and the exact boundary, are held by the Rust tests in
// `src/context/page.rs`.
//
// A file of its own, because the budget is the thread's: node runs each test
// file in a process of its own.

const { assert, describe, test } = require("../runner"),
  { Canvas, Image, loadImage } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

const why = !skiaNode.__setPageBudget
  ? "skipped: a release binary does not export __setPageBudget"
  : null;

/** The page every test draws and reads. */
const [WIDTH, HEIGHT] = [600, 1100];

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/** What one page keeps once read: its surface's pixels. */
const PAGE = WIDTH * HEIGHT * PIXEL_BYTES;

/** The budget a build keeps to unless a test sets another. */
const BUDGET = 256 * 2 ** 20;

/** Canvases the loop makes: enough to take a thread past `BUDGET`. */
const LOOP = Math.ceil(BUDGET / PAGE) + 20;

/** Canvases read every frame past the budget, and the frames. */
const [LIVE, FRAMES] = [20, 10];

/** The engines a page is read on: the CPU, and the GPU where there is one. */
const ENGINES = /** @type {const} */ ([false, true]);

/** A canvas on `gpu` holding the scene, drawn but not read. */
const drawn = (gpu) => {
  const canvas = new Canvas(WIDTH, HEIGHT);
  canvas.gpu = gpu;
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "rgb(40 90 160)";
  ctx.fillRect(0, 0, WIDTH, HEIGHT);
  ctx.fillStyle = "rgb(230 120 30 / 0.6)";
  ctx.beginPath();
  ctx.arc(300, 550, 240, 0, 2 * Math.PI);
  ctx.fill();
  ctx.font = "48px sans-serif";
  ctx.fillStyle = "white";
  ctx.fillText("dropped", 180, 200);
  return canvas;
};

/** Draws the second half of the scene onto `canvas`. */
const more = (canvas) => {
  const ctx = canvas.getContext("2d");
  ctx.strokeStyle = "rgb(20 200 90)";
  ctx.lineWidth = 9;
  ctx.strokeRect(40, 40, WIDTH - 80, HEIGHT - 80);
};

/** The whole page's pixels. */
const read = (canvas) =>
  canvas.getContext("2d").getImageData(0, 0, WIDTH, HEIGHT).data;

/** How many times the budget has dropped a page. */
const dropped = () => /** @type {number} */ (skiaNode.__droppedPages());

/** What the pages on this thread keep, as the budget counts it. */
const kept = () => /** @type {number} */ (skiaNode.__keptPageBytes());

/** Runs `f` under a budget of `bytes`, then restores the build's own. */
const under = async (bytes, f) => {
  skiaNode.__setPageBudget(bytes);
  try {
    return await f();
  } finally {
    skiaNode.__setPageBudget(null);
  }
};

/** Whether `a` and `b` hold the same bytes, with the first difference. */
const same = (a, b, what) => {
  assert.equal(a.length, b.length, `${what}: length`);
  if (Buffer.compare(Buffer.from(a), Buffer.from(b)) === 0) return;
  const i = a.findIndex((v, k) => v !== b[k]);
  assert.fail(`${what}: byte ${i} is ${a[i]}, ${b[i]} on a page kept`);
};

describe("The pages on a thread keep to a budget", () => {
  for (const gpu of ENGINES) {
    const engine = gpu ? "GPU" : "CPU";

    test(
      `canvases made and read in one loop stay inside it on the ${engine}`,
      { skip: why ?? false },
      () => {
        const before = dropped();
        // Held, as a loop's canvases are until it ends.
        const made = [];
        for (let n = 0; n < LOOP; n++) {
          const canvas = new Canvas(WIDTH, HEIGHT);
          canvas.gpu = gpu;
          canvas.getContext("2d").fillRect(10, 10, 80, 40);
          read(canvas);
          made.push(canvas);
          assert.ok(kept() <= BUDGET, `inside the budget after ${n + 1}`);
        }
        assert.ok(
          dropped() - before >= LOOP - Math.floor(BUDGET / PAGE),
          "the pages past it were dropped",
        );
        assert.equal(made.length, LOOP);
      },
    );

    test(
      `canvases read every frame are never dropped past it on the ${engine}`,
      { skip: why ?? false },
      () =>
        // Read twice with room to spare, then held to a budget of nothing,
        // which every one of them is past: a page read again is never
        // dropped, so no read replays a page another read dropped.
        under(Number.MAX_SAFE_INTEGER, () => {
          const live = Array.from({ length: LIVE }, () => drawn(gpu));
          for (const canvas of [...live, ...live]) read(canvas);
          skiaNode.__setPageBudget(0);
          // The first read past the budget drops what the tests before this
          // one left read once; what is counted is the reads after it.
          read(live[0]);
          const before = dropped();
          for (let frame = 0; frame < FRAMES; frame++) {
            for (const canvas of live) {
              const ctx = canvas.getContext("2d");
              ctx.fillStyle = `rgb(${frame * 20} 80 160)`;
              ctx.fillRect(frame * 10, frame * 10, 40, 40);
              read(canvas);
            }
          }
          assert.equal(dropped() - before, 0, "dropped while read");
        }),
    );

    // Each reader meets a canvas read once and then dropped by another
    // canvas's first read, under a budget of nothing; what it gives is what
    // the same reader gave for a canvas never dropped, under no budget. A
    // fresh canvas each time, since a page read twice is never dropped.
    test(
      `every reader of a dropped page sees it on the ${engine}`,
      { skip: why ?? false },
      async () => {
        /** `canvas`, read once and left dropped, with the drop checked. */
        const dropping = (canvas) => {
          read(canvas);
          const before = dropped();
          read(drawn(gpu));
          assert.equal(dropped() - before, 1, "the canvas was dropped");
          return canvas;
        };

        /** `paint` applied to a fresh canvas with `source`, its pixels. */
        const through = (paint) => (source) => {
          const target = new Canvas(WIDTH, HEIGHT);
          target.gpu = gpu;
          paint(target.getContext("2d"), source);
          return read(target);
        };

        /** @type {[string, (canvas: Canvas) => unknown][]} */
        const readers = [
          ["a read", read],
          [
            "a small read",
            (canvas) => canvas.getContext("2d").getImageData(296, 6, 8, 8).data,
          ],
          ["toBufferSync", (canvas) => canvas.toBufferSync("raw")],
          ["a toBuffer after it", (canvas) => canvas.toBuffer("raw")],
          [
            "a pattern of it",
            through((ctx, source) => {
              ctx.fillStyle = ctx.createPattern(source, "repeat");
              ctx.fillRect(0, 0, WIDTH, HEIGHT);
            }),
          ],
          [
            "drawCanvas of it",
            through((ctx, source) => ctx.drawCanvas(source, 0, 0)),
          ],
          [
            "drawImage of it",
            through((ctx, source) => ctx.drawImage(source, 0, 0)),
          ],
          [
            "an image decoded from its PNG",
            async (canvas) => {
              const image = await loadImage(canvas.toBufferSync("png"));
              assert.ok(image instanceof Image);
              return through((ctx) => ctx.drawImage(image, 0, 0))(canvas);
            },
          ],
        ];

        /** What each reader gives for a canvas never dropped. */
        const want = await under(Number.MAX_SAFE_INTEGER, async () => {
          const given = new Map();
          for (const [what, reader] of readers) {
            given.set(what, await reader(drawn(gpu)));
          }
          const reference = drawn(gpu);
          more(reference);
          given.set("drawn on while dropped", read(reference));
          return given;
        });

        await under(0, async () => {
          for (const [what, reader] of readers) {
            same(await reader(dropping(drawn(gpu))), want.get(what), what);
          }

          const canvas = drawn(gpu);
          const pending = canvas.toBuffer("raw");
          dropping(canvas);
          same(
            await pending,
            want.get("a toBuffer after it"),
            "a toBuffer begun before it",
          );

          // Drawn on while dropped, it reads every draw.
          const drawnOn = dropping(drawn(gpu));
          more(drawnOn);
          const what = "drawn on while dropped";
          same(read(drawnOn), want.get(what), what);
        });
      },
    );
  }
});
