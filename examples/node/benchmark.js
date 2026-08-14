//
// Timing and memory, measured rather than asserted
//
// Run from a checkout:
//
//     just build            # or: npm run build
//     just bench            # or: node --expose-gc examples/node/benchmark.js
//
// Reports the median of N runs after a warmup, because the first draw on a
// backend pays for shader compilation and surface allocation and is not what
// a caller experiences in steady state. Every figure is one machine and one
// GPU; treat the ratios as the transferable part, not the milliseconds.
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//

const os = require("os");
const { Canvas } = require("../../lib");

const W = 1200;
const H = 900;
const DEPTHS = ["RGBA8888", "RGBAF16", "RGBAF32"];
const BYTES = { RGBA8888: 4, RGBAF16: 8, RGBAF32: 16 };

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[s.length >> 1];
};

function time(fn, iterations = 10, warmup = 3) {
  for (let i = 0; i < warmup; i++) fn();
  const runs = [];
  for (let i = 0; i < iterations; i++) {
    const started = performance.now();
    fn();
    runs.push(performance.now() - started);
  }
  return median(runs);
}

// A mixed vector scene: curves, shadowed panels and text, in the proportions
// a chart or report actually draws them.
function scene(ctx) {
  const bg = ctx.createLinearGradient(0, 0, 0, H);
  bg.addColorStop(0, "#0f1b2d");
  bg.addColorStop(1, "#1b2b45");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, W, H);

  for (let i = 0; i < 300; i++) {
    ctx.beginPath();
    ctx.moveTo((i * 37) % W, (i * 53) % H);
    ctx.bezierCurveTo(
      (i * 71) % W,
      (i * 29) % H,
      (i * 13) % W,
      (i * 97) % H,
      (i * 41) % W,
      (i * 61) % H,
    );
    ctx.strokeStyle = `hsl(${(i * 7) % 360} 70% 60%)`;
    ctx.lineWidth = 1 + (i % 4);
    ctx.stroke();
  }

  for (let i = 0; i < 60; i++) {
    ctx.save();
    ctx.shadowColor = "rgba(0,0,0,0.5)";
    ctx.shadowBlur = 12;
    ctx.fillStyle = `hsl(${(i * 13) % 360} 60% 55%)`;
    ctx.beginPath();
    ctx.roundRect(
      20 + ((i * 19) % (W - 140)),
      20 + ((i * 31) % (H - 90)),
      120,
      70,
      10,
    );
    ctx.fill();
    ctx.restore();
  }

  ctx.font = "600 28px Helvetica";
  ctx.fillStyle = "#e6edf3";
  for (let i = 0; i < 40; i++)
    ctx.fillText(`Throughput sample ${i}`, 40, 40 + i * 21);
}

// Reading one pixel back forces the recording to rasterize. Without it the
// timing measures how fast commands are appended to a picture, not drawing.
const rasterize = (canvas) => canvas.getContext("2d").getImageData(0, 0, 1, 1);

function draw(options, paint) {
  const canvas = new Canvas(W, H, options);
  paint(canvas.getContext("2d"));
  rasterize(canvas);
}

const row = (label, ms, ratio) =>
  console.log(
    `  ${label.padEnd(22)} ${ms.toFixed(1).padStart(7)} ms` +
      (ratio == null ? "" : `   ${ratio.toFixed(2)}x`),
  );

console.log(
  `${os.cpus()[0].model} · ${os.cpus().length} cores · node ${process.version} · ${os.platform()}/${os.arch()}`,
);

// ── vector scene: GPU against CPU ──────────────────────────────────────────
console.log("\nmixed vector scene, 1200x900");
const gpu = time(() => draw({ gpu: true }, scene));
const cpu = time(() => draw({ gpu: false }, scene));
row("RGBA8888 gpu", gpu);
row("RGBA8888 cpu", cpu, cpu / gpu);

// ── float cost, two workloads that disagree ────────────────────────────────
// Blending translucent layers and filling opaque ones pull in opposite
// directions, so a single "float costs Nx" number would be false either way.
const translucent = (ctx) => {
  for (let i = 0; i < 120; i++) {
    ctx.globalAlpha = 0.02;
    ctx.fillStyle = i % 2 ? "#ff8844" : "#4488ff";
    ctx.fillRect(0, 0, W, H);
  }
};
// Inset by a pixel on purpose. An opaque fill that covers the whole page lets
// Skia discard everything recorded under it, so a loop of them measures the
// cull rather than the fill: 1200 of them came in at 2 ms, which is not 1200
// fills of anything. One pixel short of the bounds and every layer is drawn.
const opaque = (ctx) => {
  for (let i = 0; i < 120; i++) {
    ctx.fillStyle = i % 2 ? "#ff8844" : "#4488ff";
    ctx.fillRect(0, 1, W, H - 1);
  }
};

for (const [name, paint] of [
  ["mixed vector scene", scene],
  ["120 translucent layers", translucent],
  ["120 opaque fills", opaque],
]) {
  console.log(`\n${name}, cpu, by pixel format`);
  const base = time(
    () => draw({ gpu: false, colorType: "RGBA8888" }, paint),
    8,
    2,
  );
  for (const depth of DEPTHS) {
    const ms =
      depth === "RGBA8888"
        ? base
        : time(() => draw({ gpu: false, colorType: depth }, paint), 8, 2);
    row(depth, ms, ms / base);
  }
}

// ── export ─────────────────────────────────────────────────────────────────
console.log("\nencode a drawn 1200x900 page");
const page = new Canvas(W, H, { gpu: false });
scene(page.getContext("2d"));
// Every format the canvas writes, because the interesting comparisons are
// between them: what a lossless one costs against a lossy one, and what the
// two that carry a clock cost for a single frame.
for (const [format, options] of [
  ["png", {}],
  ["jpg", { quality: 0.92 }],
  ["webp", { quality: 0.9 }],
  ["avif", { quality: 0.92 }],
  ["gif", {}],
  ["apng", {}],
  ["tiff", {}],
  ["bmp", {}],
  ["pdf", {}],
  ["svg", {}],
])
  row(
    format,
    time(() => page.toBufferSync(format, options), 8, 2),
  );

// ── memory ─────────────────────────────────────────────────────────────────
console.log("\nresident memory per 1200x900 canvas");
if (!global.gc) console.log("  (run with --expose-gc for a stable baseline)");
for (const depth of DEPTHS) {
  global.gc?.();
  const before = process.memoryUsage().rss;
  const held = [];
  for (let i = 0; i < 20; i++) {
    const canvas = new Canvas(W, H, { gpu: false, colorType: depth });
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#345";
    ctx.fillRect(0, 0, W, H);
    rasterize(canvas);
    held.push(canvas);
  }
  const each = (process.memoryUsage().rss - before) / held.length;
  const surface = (W * H * BYTES[depth]) / 1048576;
  console.log(
    `  ${depth.padEnd(22)} ${(each / 1048576).toFixed(2).padStart(6)} MB` +
      `   surface alone ${surface.toFixed(2)} MB`,
  );
  held.length = 0;
}
