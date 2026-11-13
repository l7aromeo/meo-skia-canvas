// @ts-check

"use strict";

// What the dropped-canvas tests judge collection by: how many of the canvases
// a run made are still alive, counted by the test itself -- one more as each
// is made, one fewer as a `FinalizationRegistry` hears V8 has collected it.
//
// A count rather than resident memory, which does not hold both ways.
// Resident memory hid kept canvases: on a Mac under memory pressure, 400 held
// alive (1.28 GB of pixels) stood at 940 MB resident at most, having fallen
// from 922.2 to 808.2 MB at pass 36, as the system compressed or paged out
// what they held. And what the readback itself allocates in
// JavaScript drives collection whether or not a canvas reports its page, so
// a full-page `getImageData` hid the very defect the reporting fixes. Each
// canvas here reads one column instead: that still gives it its whole page,
// reported to V8 like any other, while handing JavaScript a few KB.
//
// Collected canvases leave a sawtooth in the count, climbing until a
// collection runs and falling back when one does, its troughs at no fixed
// spacing. So what is judged is the trend: the least-squares slope of the
// count, pass by pass, after the first few. A canvas never collected adds one
// to every later pass.
//
// The GPU test judges memory instead, by the same slope -- see
// `judgedMemory`. The pages on a thread are kept to a budget, which counts a
// GPU page with its multisample buffer, so about sixteen of these pages fit
// in it, and V8 is told only their pixels: too little to start a collection.
// So the count of GPU canvases alive climbs with every pass while what they
// hold does not, and V8 collects them on its own heap's schedule. The memory
// is the process's physical footprint on macOS, which counts what the GPU
// driver allocates for the process and what the system compressed, where
// resident memory counts neither; elsewhere it is resident memory.
//
// Shared by the CPU and GPU tests, each a file of its own so that neither's
// canvases land in the other's count: node runs each test file in a process
// of its own.

const { execFileSync } = require("node:child_process"),
  { setImmediate: turn } = require("node:timers/promises"),
  { Canvas } = require("../../lib");

/** The canvas each pass makes. */
const SIDE = [1000, 800];

/** The bytes a 32-bit pixel takes. */
const PIXEL_BYTES = 4;

/** What one canvas's page holds in pixels. */
const PAGE_BYTES = SIDE[0] * SIDE[1] * PIXEL_BYTES;

/** Canvases made, read and dropped per pass. */
const PER_PASS = 10;

/**
 * Passes made before any is judged, while V8 sizes its heap and the first
 * collections settle.
 */
const WARM_PASSES = 10;

/**
 * Passes made in all, the last `PASSES - WARM_PASSES` judged. Fifty judged
 * passes keep a sawtooth's slope small for troughs up to 30 passes apart: one
 * climbing as far as 240 canvases between them slopes at most 1.8 canvases a
 * pass, wherever its troughs fall, where thirty judged passes would allow
 * 8.3.
 */
const PASSES = WARM_PASSES + 50;

/**
 * The most of the canvases one pass makes that the count may grow by a pass.
 * A run keeping every canvas slopes at all of them; collected ones slope
 * within a fraction of one canvas of zero.
 */
const KEPT_SHARE = 0.25;

/**
 * How far above its first pass what the process holds may climb before a
 * measured run stops: past the 352 MB a run of collected GPU canvases swings
 * by on this Mac, and short of what a CI runner holds, which a run keeping
 * every GPU page would pass by its thirtieth pass. A run stopped here has
 * climbed by much more than its slope is allowed.
 */
const CEILING_BYTES = 2 * 2 ** 30;

/**
 * The least-squares slope of `values`, against their index.
 *
 * @param {number[]} values
 */
function slope(values) {
  const mean = (/** @type {number[]} */ xs) =>
    xs.reduce((sum, x) => sum + x, 0) / xs.length;
  const at = mean(values.map((_, i) => i));
  const held = mean(values);
  const [across, spread] = values.reduce(
    ([a, s], v, i) => [a + (i - at) * (v - held), s + (i - at) ** 2],
    [0, 0],
  );
  return across / spread;
}

/**
 * Whether `alive`, the canvases alive after each pass, grows by no more than
 * `KEPT_SHARE` of those a pass makes -- the least-squares slope of the passes
 * after the first `WARM_PASSES` -- and a message saying what it grew by and
 * the most alive at once, for a failure and for the log of a run that passes.
 * `resident`, the bytes resident after each pass where they were read, is
 * shown in the message and not judged.
 *
 * @param {number[]} alive
 * @param {number[]} [resident]
 */
function judged(alive, resident = []) {
  const grown = slope(alive.slice(WARM_PASSES));
  const message =
    `the canvases alive grew by ${grown.toFixed(2)} a pass over passes ` +
    `${WARM_PASSES + 1} to ${alive.length}, against the ${PER_PASS} each ` +
    `pass makes, with ${Math.max(...alive)} alive at most; alive after each ` +
    `pass ${alive.join(", ")}` +
    (resident.length
      ? `; resident after each pass, not judged, ` +
        `${resident.map((b) => (b / 1e6).toFixed(1)).join(", ")} MB`
      : "");
  return { kept: grown <= KEPT_SHARE * PER_PASS, message };
}

/**
 * Whether `memory`, the bytes the process holds after each pass, grows by no
 * more than `KEPT_SHARE` of the pages a pass makes -- the least-squares slope
 * of every pass, so that a run stopped at `CEILING_BYTES` is refused by its
 * slope too -- and a message saying what it grew by and the most it held, for
 * a failure and for the log of a run that passes.
 *
 * Every pass, where the count skips the first `WARM_PASSES`: what the first
 * pass adds -- the budget filled, the GPU's pipelines loaded -- is a single
 * step, which sixty passes read as half a megabyte a pass on this Mac.
 *
 * @param {number[]} memory
 */
function judgedMemory(memory) {
  const grown = slope(memory);
  const mb = (/** @type {number} */ bytes) => (bytes / 1e6).toFixed(1);
  const message =
    `what the process holds grew by ${mb(grown)} MB a pass over passes ` +
    `1 to ${memory.length}, against the ` +
    `${mb(PER_PASS * PAGE_BYTES)} MB of pages each pass makes, with ` +
    `${mb(Math.max(...memory))} MB at most; after each pass ` +
    `${memory.map(mb).join(", ")} MB`;
  return { kept: grown <= KEPT_SHARE * PER_PASS * PAGE_BYTES, message };
}

/**
 * The bytes this process holds: its physical footprint on macOS, as the
 * system's `footprint` tool reports it, and its resident set elsewhere.
 * Throws where neither can be read rather than measuring something else,
 * since on macOS resident memory does not see a GPU page at all.
 */
function processMemory() {
  if (process.platform !== "darwin") {
    const rss = process.memoryUsage().rss;
    if (!(rss > 0)) throw new Error(`resident memory reads as ${rss}`);
    return rss;
  }
  let report;
  try {
    report = execFileSync(
      "footprint",
      ["-p", String(process.pid), "-f", "bytes", "--noCategories"],
      { encoding: "utf8" },
    );
  } catch (error) {
    throw new Error(`macOS's footprint tool did not run: ${error}`, {
      cause: error,
    });
  }
  const bytes = /Footprint: (\d+) B/.exec(report);
  if (!bytes) throw new Error(`no footprint in footprint's report: ${report}`);
  return Number(bytes[1]);
}

/**
 * The canvases alive, the bytes resident, and where `measured` the bytes the
 * process holds -- see `processMemory` -- after each of `PASSES` passes of
 * `PER_PASS` canvases made with `gpu`, each read one column wide and handed
 * to `hold` once read. A measured run stops early once what the process holds
 * is `CEILING_BYTES` above its first pass.
 *
 * @param {{ gpu: boolean, hold: (canvas: Canvas) => void, measured?: boolean }} how
 */
async function afterEachPass({ gpu, hold, measured = false }) {
  let living = 0;
  const collected = new FinalizationRegistry(() => {
    living--;
  });
  const alive = [];
  const resident = [];
  const memory = [];
  for (let pass = 0; pass < PASSES; pass++) {
    for (let i = 0; i < PER_PASS; i++) {
      const canvas = new Canvas(SIDE[0], SIDE[1]);
      canvas.gpu = gpu;
      const ctx = canvas.getContext("2d");
      ctx.fillRect(10, 10, 80, 40);
      ctx.getImageData(0, 0, 1, SIDE[1]);
      living++;
      collected.register(canvas, null);
      hold(canvas);
    }
    await turn();
    alive.push(living);
    resident.push(process.memoryUsage().rss);
    if (measured) {
      memory.push(processMemory());
      if (memory[memory.length - 1] - memory[0] > CEILING_BYTES) break;
    }
  }
  return { alive, resident, memory };
}

module.exports = {
  PAGE_BYTES,
  PASSES,
  PER_PASS,
  afterEachPass,
  judged,
  judgedMemory,
};
