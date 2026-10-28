// @ts-check

"use strict";

// Once rendering stops, the idle watcher empties each JavaScript thread's font
// caches through a channel the addon left on that thread as it loaded. These
// read the caches through `__cachedParagraphs`, which debug builds alone
// export: against a release binary they are skipped by name.
//
// The watcher is process-wide and waits seconds of quiet, so this file keeps
// to itself: node runs each test file in its own process, and the tests here
// run one after another.

const { execFileSync } = require("child_process"),
  { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  { skiaNode } = require("../../lib/classes/neon");

/** Longer than the watcher's quiet interval and the tick that notices it. */
const PAST_THE_INTERVAL_MS = 5000;

/** Shorter than the interval, as a frame or a request arriving would be. */
const FRAME_MS = 500;

const why = !skiaNode.__cachedParagraphs
  ? "skipped: a release binary does not export __cachedParagraphs"
  : null;
const cached = () => /** @type {number} */ (skiaNode.__cachedParagraphs());
/** How many runs the thread keeps laid out for drawing. */
const laidOut = () => /** @type {number[]} */ (skiaNode.__laidOut())[0];
const pause = (/** @type {number} */ ms) =>
  new Promise((done) => setTimeout(done, ms));

/**
 * One context for every frame, so that its text is the same run each time.
 * Made at the first frame rather than as the file loads: using a font starts
 * the watcher, and the quiet of the test before would then release.
 * @type {CanvasRenderingContext2D | undefined}
 */
let page;

/** Text drawn and read back, as the pixels it left. */
const drawn = () => {
  if (!page) {
    page = new Canvas(240, 60).getContext("2d");
    page.font = "24px sans-serif";
  }
  page.clearRect(0, 0, 240, 60);
  page.fillText("Hamburgefonstiv", 4, 40);
  return Buffer.from(page.getImageData(0, 0, 240, 60).data.buffer);
};

describe("An idle process lets its font caches go", () => {
  test(
    "a script that only draws text exits on its own",
    { skip: why ?? false },
    () => {
      // The channel each thread leaves is unreferenced: the event loop has
      // nothing of the addon's to wait for, whether a release has been posted
      // or not. The second script outlives the interval, so one is.
      const lib = JSON.stringify(require.resolve("../../lib"));
      const run = (/** @type {string} */ after) =>
        execFileSync(
          process.execPath,
          [
            "-e",
            `const { Canvas } = require(${lib});
          const ctx = new Canvas(240, 60).getContext("2d");
          ctx.fillText("Hamburgefonstiv", 4, 40);
          ctx.canvas.toBufferSync("png");
          ${after}`,
          ],
          { encoding: "utf8", timeout: 3 * PAST_THE_INTERVAL_MS },
        );
      const started = Date.now();
      run("");
      assert.ok(
        Date.now() - started < PAST_THE_INTERVAL_MS,
        "a script that has finished exits without waiting for the watcher",
      );
      const neon = JSON.stringify(require.resolve("../../lib/classes/neon"));
      const released = JSON.parse(
        run(`
          const { skiaNode } = require(${neon});
          const now = () => ({
            held: skiaNode.__cachedParagraphs(),
            activity: skiaNode.__activity(),
          });
          const before = now();
          setTimeout(() => {
            const after = now();
            ctx.measureText("Hamburgefonstiv");
            const measured = now();
            console.log(JSON.stringify({ before, after, measured }));
          }, ${PAST_THE_INTERVAL_MS});
        `),
      );
      assert.deepEqual(
        [released.before.held, released.after.held],
        [1, 0],
        "and one that outlives the interval is released, then exits",
      );
      // A release counted as use of the fonts would hold off the next one, and
      // so cause it, every few seconds for as long as the process lives.
      assert.equal(
        released.after.activity,
        released.before.activity,
        "the release is not counted as activity",
      );
      assert.ok(
        released.measured.activity > released.after.activity,
        "where a measurement is",
      );
    },
  );

  test(
    "nothing is let go while frames arrive, and text draws the same after",
    { skip: why ?? false },
    async () => {
      // Drawn twice, for the run to be kept as well as laid out.
      drawn();
      const first = drawn();
      for (let frame = 0; frame < PAST_THE_INTERVAL_MS / FRAME_MS; frame++) {
        await pause(FRAME_MS);
        assert.ok(cached() > 0, `the caches are held through frame ${frame}`);
        assert.ok(laidOut() > 0, `and the runs through frame ${frame}`);
        assert.ok(drawn().equals(first), `frame ${frame} draws the same`);
      }

      await pause(PAST_THE_INTERVAL_MS);
      assert.equal(cached(), 0, "the quiet that follows lets them go");
      assert.equal(laidOut(), 0, "and the runs kept for drawing");

      assert.ok(drawn().equals(first), "the text draws the same after");
      assert.ok(cached() > 0, "and fills the caches again");
      assert.ok(drawn().equals(first), "and again");
      assert.ok(laidOut() > 0, "and keeps the run again");
    },
  );

  test(
    "measuring text alone holds a release off",
    { skip: why ?? false },
    async () => {
      // Laid out without being drawn, as a server answering with widths does:
      // that is use of the fonts as much as drawing is.
      const ctx = new Canvas(10, 10).getContext("2d");
      ctx.font = "24px sans-serif";
      let width = ctx.measureText("Hamburgefonstiv").width;
      for (let at = 0; at < PAST_THE_INTERVAL_MS / FRAME_MS; at++) {
        await pause(FRAME_MS);
        assert.ok(
          cached() > 0,
          `the caches are held through measurement ${at}`,
        );
        width = ctx.measureText(`Hamburgefonstiv ${at}`).width;
      }
      assert.ok(width > 0);

      await pause(PAST_THE_INTERVAL_MS);
      assert.equal(cached(), 0, "and the quiet after it lets them go");
    },
  );
});
