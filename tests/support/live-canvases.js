// @ts-check

"use strict";

// What the dropped-canvas tests judge collection by: how many of the canvases
// a run made are still alive, counted by the test itself -- one more as each
// is made, one fewer as a `FinalizationRegistry` hears V8 has collected it.
//
// A count rather than memory, because no measure of memory holds both ways.
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
// Shared by the CPU and GPU tests, each a file of its own so that neither's
// canvases land in the other's count: node runs each test file in a process
// of its own.

const { setImmediate: turn } = require("node:timers/promises"),
  { Canvas } = require("../../lib");

/** The canvas each pass makes. */
const SIDE = [1000, 800];

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
 * The canvases alive, and the bytes resident, after each of `PASSES` passes
 * of `PER_PASS` canvases made with `gpu`, each read one column wide and
 * handed to `hold` once read.
 *
 * @param {{ gpu: boolean, hold: (canvas: Canvas) => void }} how
 */
async function afterEachPass({ gpu, hold }) {
  let living = 0;
  const collected = new FinalizationRegistry(() => {
    living--;
  });
  const alive = [];
  const resident = [];
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
  }
  return { alive, resident };
}

module.exports = { PASSES, PER_PASS, afterEachPass, judged };
