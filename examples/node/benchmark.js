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
const { execFileSync } = require("child_process");
const { Canvas, Image } = require("../../lib");

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

// Child mode for the memory table below: measure one depth in this otherwise
// untouched process and print the bytes per canvas. Nothing else in this file
// runs, which is the entire point.
if (process.argv[2] === "--memory-probe") {
  const depth = process.argv[3];
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
  process.stdout.write(
    String((process.memoryUsage().rss - before) / held.length),
  );
  process.exit(0);
}

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
console.log("\nencode a drawn 1200x900 page, cpu");
const page = new Canvas(W, H, { gpu: false });
scene(page.getContext("2d"));
// Every format the canvas writes, because the interesting comparisons are
// between them: what a lossless one costs against a lossy one, and what the
// two that carry a clock cost for a single frame.
//
// Size beside the time, because neither figure means much alone: the fastest
// encoder here writes the largest file and the slowest writes the smallest,
// and a table of times would report that as a ranking.
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
]) {
  const ms = time(() => page.toBufferSync(format, options), 8, 2);
  const bytes = page.toBufferSync(format, options).length;
  console.log(
    `  ${format.padEnd(22)} ${ms.toFixed(1).padStart(7)} ms   ` +
      `${(bytes / 1024).toFixed(1).padStart(8)} KB`,
  );
}

// ── animate ────────────────────────────────────────────────────────────────
// The single page above says nothing about the four formats that carry a
// clock, because the work those do is between frames: each sends the
// rectangle it differs from its predecessor in, and compresses frames on
// whatever cores are free. A one-page export reaches none of that, so every
// animated figure this file printed was the cost of the container.
//
// A still background with a moving foreground, which is what a
// dirty-rectangle encoder is actually asked to compress. A scene where the
// whole page moves would make every rectangle the whole page and measure
// something nobody exports.
console.log("\nencode a 30-frame 1200x900 animation, cpu");
const reel = new Canvas(W, H, { gpu: false });
for (let f = 0; f < 30; f++) {
  const ctx = f ? reel.newPage() : reel.getContext("2d");
  scene(ctx);
  ctx.fillStyle = "#f6c453";
  ctx.beginPath();
  ctx.arc(100 + ((W - 200) * f) / 29, H / 2, 80, 0, 2 * Math.PI);
  ctx.fill();
}
for (const [format, options] of [
  ["webp", { quality: 0.9, fps: 30 }],
  ["apng", { fps: 30 }],
  ["gif", { fps: 30 }],
  ["avif", { quality: 0.92, fps: 30 }],
]) {
  // Fewer iterations than the still table: these are whole animations, and
  // the slowest is most of a second each time.
  const ms = time(() => reel.toBufferSync(format, options), 5, 1);
  const bytes = reel.toBufferSync(format, options).length;
  console.log(
    `  ${format.padEnd(22)} ${ms.toFixed(1).padStart(7)} ms   ` +
      `${(bytes / 1024).toFixed(1).padStart(8)} KB`,
  );
}

// ── AVIF's own dials ───────────────────────────────────────────────────────
// The one format here with choices that move both axes at once, so the
// numbers above say nothing about what those choices cost. Size is reported
// beside the time because that is the trade being made: subsampling buys
// bytes on a photograph and nothing on a page like this one, and lossless
// spends them for exactness.
console.log("\nencode the same page as AVIF, cpu, by option");
for (const [label, options] of [
  ["quality 0.5", { quality: 0.5 }],
  ["quality 0.92", { quality: 0.92 }],
  ["quality 1.0", { quality: 1 }],
  ["4:2:2", { quality: 0.92, chromaSampling: "4:2:2" }],
  ["4:2:0", { quality: 0.92, chromaSampling: "4:2:0" }],
  ["lossless", { lossless: true }],
]) {
  // Fewer iterations than the rest of this file, because each of these is a
  // quarter of a second and six options is already most of the run. Still a
  // median of five after two warmups, which is what the encode table uses.
  const ms = time(() => page.toBufferSync("avif", options), 5, 2);
  const bytes = page.toBufferSync("avif", options).length;
  console.log(
    `  ${label.padEnd(22)} ${ms.toFixed(1).padStart(7)} ms   ` +
      `${(bytes / 1024).toFixed(1).padStart(8)} KB`,
  );
}

// ── decode ─────────────────────────────────────────────────────────────────
// AVIF is the one format here Skia cannot read, so its figure is entirely
// this crate's own path: the container parsed here, the frame handed to
// libaom, the planes composed back to RGBA.
//
// Drawn onto a canvas rather than merely constructed. Skia hands back an
// image whose pixels are not decoded until something asks for them, so
// timing the constructor alone measured 0.0 ms for PNG -- the work had not
// happened yet.
//
// That means the figure includes one 1200x900 blit as well as the decode.
// The blit is the same for both rows, so the difference between them is the
// decode; the absolute numbers are a little high for it.
console.log("\ndecode a 1200x900 page, cpu");
for (const [label, options] of [
  ["avif", { quality: 0.92 }],
  ["png", {}],
]) {
  const encoded = page.toBufferSync(label, options);
  const into = new Canvas(W, H, { gpu: false });
  const ctx = into.getContext("2d");
  row(
    label,
    time(
      () => {
        ctx.drawImage(new Image(encoded), 0, 0);
        rasterize(into);
      },
      8,
      2,
    ),
  );
}

// ── memory ─────────────────────────────────────────────────────────────────
// Each depth in a process of its own, three of them, and the median taken.
//
// This used to run inline like every other section, and by the time it got
// here the answer was meaningless: the process holds a large pool of freed
// pages, the twenty new canvases are served out of it, and the RSS delta
// measures the pool rather than the canvases. It reported RGBAF32 at 0.31 MB
// against a surface of 16.48, and before the page cache was bounded it
// reported 6.89 -- impossible either way, since a held canvas cannot cost
// less than its own pixels.
//
// A fresh process has no pool to hide in, so the delta is the allocation. It
// is still a noisy way to weigh anything -- three passes here spread 15.7 to
// 22.4 MB on the same depth -- hence the median of three rather than one
// reading.
console.log("\nresident memory per 1200x900 canvas, cpu");
if (!global.gc) console.log("  (run with --expose-gc for a stable baseline)");
for (const depth of DEPTHS) {
  const readings = [];
  for (let run = 0; run < 3; run++) {
    const out = execFileSync(
      process.execPath,
      ["--expose-gc", __filename, "--memory-probe", depth],
      { encoding: "utf8" },
    );
    readings.push(Number(out.trim()));
  }
  const each = median(readings);
  const surface = (W * H * BYTES[depth]) / 1048576;
  console.log(
    `  ${depth.padEnd(22)} ${(each / 1048576).toFixed(2).padStart(6)} MB` +
      `   surface alone ${surface.toFixed(2)} MB`,
  );
}
