// @ts-check

"use strict";

// What the dropped-canvas tests judge by: the memory the process holds after
// each pass of canvases made, read and dropped, the slope of it pass by pass.
// A run keeping its pages climbs by them every pass; a run whose canvases
// are collected, or whose pages the budget drops, holds level.
//
// Memory rather than the count of canvases alive, which no longer says
// whether what they hold is bounded. The pages on a thread are kept to a
// budget, and a page the budget drops stops being reported to V8, so V8 is
// told too little of what dropped canvases still hold to start a collection
// on its own schedule: the count of canvases alive can climb with every pass
// while what they hold does not -- on the CPU some runs and not others,
// since whether a collection is still in reach depends on where the last one
// landed. The count is still kept, for the message of a run.
//
// The memory is the process's physical footprint on macOS, which counts what
// the GPU driver allocates for the process and what the system compressed,
// where resident memory counts neither; elsewhere it is resident memory.
// Each canvas reads one column, which still gives it its whole page,
// reported to V8 like any other, while handing JavaScript a few KB: a
// full-page `getImageData` allocates enough JavaScript to drive collection
// by itself, whether or not a canvas reports its page.
//
// Shared by the CPU and GPU tests, each a file of its own so that neither's
// canvases land in the other's memory: node runs each test file in a process
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
 * Passes made in all, every one judged. Sixty keep a sawtooth's slope small
 * for collections up to 30 passes apart, and a run keeping every page passes
 * the bound by its slope well before `CEILING_BYTES` stops it.
 */
const PASSES = 60;

/**
 * The most of the pages one pass makes that what the process holds may grow
 * by a pass. A run keeping every page slopes at all of them; a run whose
 * pages are collected or dropped slopes within a few per cent of them.
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
 * Whether `memory`, the bytes the process holds after each pass, grows by no
 * more than `KEPT_SHARE` of the pages a pass makes -- the least-squares slope
 * of every pass, so that a run stopped at `CEILING_BYTES` is refused by its
 * slope too -- and a message saying what it grew by and the most it held, for
 * a failure and for the log of a run that passes.
 *
 * Every pass: what the first passes add -- the budget filling, the GPU's
 * pipelines loaded -- is a single step, which sixty passes read as a megabyte
 * or two a pass on this Mac.
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
  judgedMemory,
};
