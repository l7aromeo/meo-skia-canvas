//
// Timing and memory, measured rather than asserted
//
// Run from a checkout:
//
//     just bench-npm        # builds release first, then runs this file
//     just bench            # this and examples/rust/benchmark.rs, one after the other
//
// Flags:
//
//     --list         print every case id, one a line, and time nothing
//     --quick        one run of each case and no warmup: a smoke test, not a measurement
//     --only PREFIX  run only the cases whose id starts with PREFIX
//     --out DIR      where the report goes (default target/bench)
//
// Every case has an id, and `examples/rust/benchmark.rs` runs the same ids in
// the same order through the crate, so the two reports line up row by row and
// their ratio is what the binding costs. `scripts/check-bench-parity.mjs`
// holds the two lists equal; `just ci` runs it.
//
// Each run writes `<date>-<commit>-npm.md` and `.json` to the report
// directory: per case the median, p10 and p90 of its runs, and for the run the
// binary, the commit, the machine and the load average at both ends -- what a
// figure needs before anyone compares it with another.
//
// The median of N runs after a warmup, because the first draw on a backend
// pays for shader compilation and surface allocation and is not what a caller
// experiences in steady state. Every figure is one machine and one GPU; treat
// the ratios as the transferable part, not the milliseconds.
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//

const os = require("os");
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { execFileSync } = require("child_process");
const { BINARY_OVERRIDE } = require("../../lib/binary.js");

// Loaded after `--list` has had its turn, which names the cases without
// touching the native module, so the parity check in `just ci` needs no
// build of it.
let Canvas, Image;
const loadLibrary = () => ({ Canvas, Image } = require("../../lib"));

const W = 1200;
const H = 900;
const DEPTHS = ["RGBA8888", "RGBAF16", "RGBAF32"];
const BYTES = { RGBA8888: 4, RGBAF16: 8, RGBAF32: 16 };

// The pages every drawing case runs on: the default; the two where 7.0's
// accepted costs live -- an eight-bit wide-gamut page, whose colours are held
// exactly in their own space, and a half-float one; and sRGB in each float
// format, which against the default is what a float colorType costs. A float
// page draws on the CPU whichever engine is asked for.
const SPACES = [
  ["srgb", {}],
  ["p3", { colorSpace: "display-p3" }],
  ["p3-f16", { colorSpace: "display-p3", colorType: "RGBAF16" }],
  ["srgb-f16", { colorType: "RGBAF16" }],
  ["srgb-f32", { colorType: "RGBAF32" }],
];
const ENGINES = [
  ["cpu", { gpu: false }],
  ["gpu", { gpu: true }],
];

// Runs and warmups. `--quick` takes one run and no warmup everywhere, which
// proves every case still runs and measures nothing.
const RUNS = { runs: 8, warmup: 2 };
const LONG_RUNS = { runs: 5, warmup: 1 };

const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => {
  const at = args.indexOf(name);
  return at >= 0 ? args[at + 1] : undefined;
};
const QUICK = flag("--quick");
const ONLY = option("--only");
const OUT = option("--out") || path.resolve(__dirname, "../../target/bench");

// ── measurement ────────────────────────────────────────────────────────────

const sorted = (xs) => [...xs].sort((a, b) => a - b);
// The upper middle on an even count, as `examples/rust/benchmark.rs` takes
// it: averaging the two would invent a duration nothing actually took.
const quantile = (xs, q) => {
  const s = sorted(xs);
  return s[Math.min(s.length - 1, Math.floor(q * s.length))];
};

function time(fn, { runs, warmup } = RUNS) {
  const n = QUICK ? 1 : runs;
  for (let i = 0; i < (QUICK ? 0 : warmup); i++) fn();
  const samples = [];
  for (let i = 0; i < n; i++) {
    const started = performance.now();
    fn();
    samples.push(performance.now() - started);
  }
  return {
    unit: "ms",
    median: quantile(samples, 0.5),
    p10: quantile(samples, 0.1),
    p90: quantile(samples, 0.9),
    runs: n,
  };
}

// A measured value that is not a duration: a fidelity in decibels, an error
// in levels, a residency in megabytes.
const value = (unit, v) => ({ unit, median: v, p10: null, p90: null, runs: 1 });

// Reading one pixel back forces the recording to rasterize. Without it the
// timing measures how fast commands are appended to a picture, not drawing.
const rasterize = (canvas) => canvas.getContext("2d").getImageData(0, 0, 1, 1);

function draw(options, paint) {
  const canvas = new Canvas(W, H, options);
  paint(canvas.getContext("2d"));
  rasterize(canvas);
}

// Scatters `i` across `span` without a random number generator, which would
// make one run's timing incomparable with the next. The strides are the ones
// `examples/rust/benchmark.rs` uses.
const scatter = (i, stride, span) => (i * stride) % span;

// ── what is drawn ──────────────────────────────────────────────────────────

const curves = (ctx, count = 300) => {
  for (let i = 0; i < count; i++) {
    ctx.beginPath();
    ctx.moveTo(scatter(i, 37, W), scatter(i, 53, H));
    ctx.bezierCurveTo(
      scatter(i, 71, W),
      scatter(i, 29, H),
      scatter(i, 13, W),
      scatter(i, 97, H),
      scatter(i, 41, W),
      scatter(i, 61, H),
    );
    ctx.strokeStyle = `hsl(${(i * 7) % 360} 70% 60%)`;
    ctx.lineWidth = 1 + (i % 4);
    ctx.stroke();
  }
};

const panels = (ctx, shadow) => {
  for (let i = 0; i < 60; i++) {
    ctx.save();
    if (shadow) {
      ctx.shadowColor = "rgba(0,0,0,0.5)";
      ctx.shadowBlur = 12;
    }
    ctx.fillStyle = `hsl(${(i * 13) % 360} 60% 55%)`;
    ctx.beginPath();
    ctx.roundRect(
      20 + scatter(i, 19, W - 140),
      20 + scatter(i, 31, H - 90),
      120,
      70,
      10,
    );
    ctx.fill();
    ctx.restore();
  }
};

const lines = (ctx, count = 40) => {
  ctx.font = "600 28px Helvetica";
  ctx.fillStyle = "#e6edf3";
  for (let i = 0; i < count; i++)
    ctx.fillText(`Throughput sample ${i}`, 40, 40 + i * 21);
};

// A mixed vector scene: curves, shadowed panels and text, in the proportions
// a chart or report actually draws them.
function scene(ctx) {
  const bg = ctx.createLinearGradient(0, 0, 0, H);
  bg.addColorStop(0, "#0f1b2d");
  bg.addColorStop(1, "#1b2b45");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, W, H);
  curves(ctx);
  panels(ctx, true);
  lines(ctx);
}

const PARAGRAPH =
  "Benchmarks measure what they draw and nothing else, so a paragraph that " +
  "wraps is its own case: breaking lines, shaping each and laying them out " +
  "is work a single line of text never asks for. ";

// Each the same 60 rectangles, so the cases differ in what fills them.
const rects = (ctx, style) => {
  for (let i = 0; i < 60; i++) {
    ctx.fillStyle = style(i);
    ctx.fillRect(scatter(i, 19, W - 160), scatter(i, 31, H - 120), 160, 120);
  }
};

const gradient = (ctx, i, interpolation) => {
  const x = scatter(i, 19, W - 160);
  const y = scatter(i, 31, H - 120);
  const g =
    i % 3 === 0
      ? ctx.createLinearGradient(x, y, x + 160, y + 120)
      : i % 3 === 1
        ? ctx.createRadialGradient(x + 80, y + 60, 4, x + 80, y + 60, 90)
        : ctx.createConicGradient(i / 10, x + 80, y + 60);
  if (interpolation) g.interpolation = interpolation;
  g.addColorStop(0, `hsl(${(i * 13) % 360} 80% 50%)`);
  g.addColorStop(0.5, "#f0e6d2");
  g.addColorStop(1, `hsl(${(i * 13 + 180) % 360} 80% 40%)`);
  return g;
};

// A 64-pixel tile: four quadrants and a disc, so a pattern of it has edges.
function tile() {
  const t = new Canvas(64, 64, { gpu: false });
  const c = t.getContext("2d");
  c.fillStyle = "#c82878";
  c.fillRect(0, 0, 32, 32);
  c.fillStyle = "#14a05a";
  c.fillRect(32, 32, 32, 32);
  c.fillStyle = "rgba(30 60 220 / 0.8)";
  c.beginPath();
  c.arc(34, 30, 18, 0, 2 * Math.PI);
  c.fill();
  return t;
}

// A canvas holding 200 rectangles, drawn into the page whole.
function innerPage() {
  const inner = new Canvas(600, 450, { gpu: false });
  const ic = inner.getContext("2d");
  ic.fillStyle = "#0b0e14";
  ic.fillRect(0, 0, 600, 450);
  for (let i = 0; i < 200; i++) {
    ic.fillStyle = `hsl(${(i * 9) % 360} 70% 50%)`;
    ic.fillRect(scatter(i, 31, 600), scatter(i, 17, 450), 130, 70);
  }
  return inner;
}

// Fixtures a case reads but does not time, made once and on first use.
const once = (make) => {
  let made;
  return () => (made ??= make());
};
const tileCanvas = once(tile);
const tileImage = once(() => new Image(tileCanvas().toBufferSync("png")));
const innerCanvas = once(innerPage);
const photoImage = once(() => {
  const p = new Canvas(256, 256, { gpu: false });
  const c = p.getContext("2d");
  const g = c.createLinearGradient(0, 0, 256, 256);
  g.addColorStop(0, "#205080");
  g.addColorStop(1, "#e0a040");
  c.fillStyle = g;
  c.fillRect(0, 0, 256, 256);
  curves(c, 40);
  return new Image(p.toBufferSync("png"));
});

// What each drawing case paints, by name. The two layer workloads pull in
// opposite directions on a float page -- blending translucent layers against
// filling opaque ones -- so a single "float costs Nx" figure would be false
// either way.
const DRAWS = [
  ["scene", scene],
  ["curves", (ctx) => curves(ctx)],
  [
    "dashes",
    (ctx) => {
      ctx.setLineDash([12, 6]);
      ctx.lineWidth = 2;
      for (let i = 0; i < 200; i++) {
        ctx.strokeStyle = `hsl(${(i * 11) % 360} 70% 55%)`;
        ctx.beginPath();
        ctx.moveTo(scatter(i, 37, W), scatter(i, 53, H));
        ctx.lineTo(scatter(i, 71, W), scatter(i, 29, H));
        ctx.stroke();
      }
    },
  ],
  ["text", (ctx) => lines(ctx, 120)],
  [
    "wrapped-text",
    (ctx) => {
      ctx.font = "18px Helvetica";
      ctx.fillStyle = "#1b2b45";
      ctx.textWrap = true;
      for (let i = 0; i < 6; i++)
        ctx.fillText(PARAGRAPH.repeat(4), 20, 30 + i * 140, W - 40);
    },
  ],
  ["gradients", (ctx) => rects(ctx, (i) => gradient(ctx, i))],
  ["gradients-oklab", (ctx) => rects(ctx, (i) => gradient(ctx, i, "oklab"))],
  [
    "pattern-image",
    (ctx) => {
      ctx.fillStyle = ctx.createPattern(tileImage(), "repeat");
      ctx.fillRect(0, 1, W, H - 1);
    },
  ],
  [
    "pattern-canvas",
    (ctx) => {
      ctx.fillStyle = ctx.createPattern(tileCanvas(), "repeat");
      ctx.fillRect(0, 1, W, H - 1);
    },
  ],
  [
    "blur",
    (ctx) => {
      ctx.filter = "blur(4px)";
      rects(ctx, (i) => `hsl(${(i * 13) % 360} 60% 55%)`);
    },
  ],
  ["shadows", (ctx) => panels(ctx, true)],
  [
    "clip",
    (ctx) => {
      ctx.beginPath();
      ctx.arc(W / 2, H / 2, 380, 0, 2 * Math.PI);
      ctx.clip();
      curves(ctx, 150);
      rects(ctx, (i) => `hsl(${(i * 13) % 360} 60% 55%)`);
    },
  ],
  [
    "nested",
    (ctx) => {
      for (let i = 0; i < 8; i++)
        ctx.drawCanvas(
          innerCanvas(),
          scatter(i, 97, 600),
          scatter(i, 61, 450),
          300,
          225,
        );
    },
  ],
  [
    "images",
    (ctx) => {
      for (let i = 0; i < 100; i++)
        ctx.drawImage(
          photoImage(),
          scatter(i, 37, W - 128),
          scatter(i, 53, H - 128),
          128,
          128,
        );
    },
  ],
  [
    "translucent",
    (ctx) => {
      for (let i = 0; i < 120; i++) {
        ctx.globalAlpha = 0.02;
        ctx.fillStyle = i % 2 ? "#ff8844" : "#4488ff";
        ctx.fillRect(0, 0, W, H);
      }
    },
  ],
  // Inset by a pixel on purpose. An opaque fill that covers the whole page
  // lets Skia discard everything recorded under it, so a loop of them
  // measures the cull rather than the fill: 1200 of them came in at 2 ms,
  // which is not 1200 fills of anything. One pixel short and every layer is
  // drawn.
  [
    "opaque",
    (ctx) => {
      for (let i = 0; i < 120; i++) {
        ctx.fillStyle = i % 2 ? "#ff8844" : "#4488ff";
        ctx.fillRect(0, 1, W, H - 1);
      }
    },
  ],
];

// ── the page every encoder and decoder is handed ───────────────────────────

const page = once(() => {
  const p = new Canvas(W, H, { gpu: false });
  scene(p.getContext("2d"));
  return p;
});

// The 256-pixel page an icon can hold: ICO stores nothing larger.
const iconPage = once(() => {
  const p = new Canvas(256, 256, { gpu: false });
  scene(p.getContext("2d"));
  return p;
});

// A still background with a moving foreground, which is what a
// dirty-rectangle encoder is actually asked to compress. A scene where the
// whole page moves would make every rectangle the whole page and measure
// something nobody exports.
const reel = once(() => {
  const r = new Canvas(W, H, { gpu: false });
  for (let f = 0; f < 30; f++) {
    const ctx = f ? r.newPage() : r.getContext("2d");
    scene(ctx);
    ctx.fillStyle = "#f6c453";
    ctx.beginPath();
    ctx.arc(100 + ((W - 200) * f) / 29, H / 2, 80, 0, 2 * Math.PI);
    ctx.fill();
  }
  return r;
});

// A page holding what a document has to work hardest at: a canvas pattern,
// blurred and shadowed shapes, gradients and text.
const heavyPage = once(() => {
  const p = new Canvas(W, H, { gpu: false });
  const ctx = p.getContext("2d");
  scene(ctx);
  ctx.save();
  ctx.fillStyle = ctx.createPattern(tileCanvas(), "repeat");
  ctx.fillRect(40, 40, 500, 300);
  ctx.filter = "blur(3px)";
  rects(ctx, (i) => gradient(ctx, i));
  ctx.restore();
  return p;
});

const ENCODES = [
  ["png", "png", {}, page],
  ["jpeg", "jpg", { quality: 0.92 }, page],
  ["webp", "webp", { quality: 0.9 }, page],
  ["avif", "avif", { quality: 0.92 }, page],
  ["gif", "gif", {}, page],
  ["apng", "apng", {}, page],
  ["tiff", "tiff", {}, page],
  ["bmp", "bmp", {}, page],
  ["ico", "ico", {}, iconPage],
  ["pdf", "pdf", {}, page],
  ["svg", "svg", {}, page],
  // AVIF's own dials, the one format here whose choices move both time and
  // size: subsampling buys bytes on a photograph and nothing on a page like
  // this one, and lossless spends them for exactness.
  ["avif-q0.5", "avif", { quality: 0.5 }, page],
  ["avif-q1", "avif", { quality: 1 }, page],
  ["avif-422", "avif", { quality: 0.92, chromaSampling: "4:2:2" }, page],
  ["avif-420", "avif", { quality: 0.92, chromaSampling: "4:2:0" }, page],
  ["avif-lossless", "avif", { lossless: true }, page],
];

const ANIMATIONS = [
  ["webp", { quality: 0.9, fps: 30 }],
  ["apng", { fps: 30 }],
  ["gif", { fps: 30 }],
  ["avif", { quality: 0.92, fps: 30 }],
];

// Each decoder handed a page its own encoder wrote. AVIF and APNG are this
// library's own code -- Skia reads neither -- and the rest are Skia's.
const DECODES = [
  ["png", "png", {}, page],
  ["jpeg", "jpg", { quality: 0.92 }, page],
  ["webp", "webp", { quality: 0.9 }, page],
  ["avif", "avif", { quality: 0.92 }, page],
  ["gif", "gif", {}, page],
  ["apng", "apng", {}, page],
  ["bmp", "bmp", {}, page],
  ["ico", "ico", {}, iconPage],
];

// User units, no `font-family`, no text: the document where the walk this
// library makes before Skia parses -- absolute lengths, text positions,
// generic families, the `font-size` cascade -- finds nothing to rewrite, so
// the row measures the cost of looking. Two sizes, because the walk is per
// element: both rising together is a uniform slowdown, the large one rising
// alone is something that scales worse than linearly.
const svgDoc = (elements) => {
  let body = "";
  for (let i = 0; i < elements; i++) {
    body +=
      `<rect x="${(i * 7) % 900}" y="${(i * 13) % 700}" ` +
      `width="20" height="14" fill="#3a7"/>`;
  }
  return Buffer.from(
    `<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="800">` +
      `${body}</svg>`,
  );
};

// ── the cases ──────────────────────────────────────────────────────────────

const cases = [];
const add = (id, measure) => cases.push({ id, measure });

for (const [name, paint] of DRAWS)
  for (const [engine, engineOptions] of ENGINES)
    for (const [space, spaceOptions] of SPACES)
      add(`draw/${name}/${engine}/${space}`, () =>
        time(() => draw({ ...engineOptions, ...spaceOptions }, paint)),
      );

// A whole page read back and written in, in the page's format: what a filter
// or an image pipeline pays per frame.
for (const [engine, engineOptions] of ENGINES)
  for (const [space, spaceOptions] of SPACES) {
    add(`pixels/get/${engine}/${space}`, () => {
      const canvas = new Canvas(W, H, { ...engineOptions, ...spaceOptions });
      const ctx = canvas.getContext("2d");
      scene(ctx);
      return time(() => ctx.getImageData(0, 0, W, H));
    });
    add(`pixels/put/${engine}/${space}`, () => {
      const canvas = new Canvas(W, H, { ...engineOptions, ...spaceOptions });
      const ctx = canvas.getContext("2d");
      scene(ctx);
      const data = ctx.getImageData(0, 0, W, H);
      return time(() => {
        ctx.putImageData(data, 0, 0);
        rasterize(canvas);
      });
    });
  }

// A canvas drawn into a canvas, behind a clip that shows a sliver of it. The
// cost belongs to the clip: the visible region is rasterized and the rest of
// the source is culled. Two source sizes, because the failure this catches
// is the cost tracking the *source* instead, and a single figure cannot show
// that -- a heavy source reads as "this machine is slow".
//
// Only a source that has itself drawn a canvas takes this path. One that has
// only been drawn on travels as a picture and is never rasterized, so the
// nesting below is what makes the row measure anything.
const nestedSource = (ops) => {
  const inner = new Canvas(W, H, { gpu: false });
  const ic = inner.getContext("2d");
  ic.fillStyle = "#0b0e14";
  ic.fillRect(0, 0, W, H);
  for (let i = 0; i < ops; i++) {
    ic.fillStyle = `hsl(${(i * 9) % 360} 70% 50%)`;
    ic.fillRect((i * 31) % W, (i * 17) % H, 260, 140);
  }
  const source = new Canvas(W, H, { gpu: false });
  source.getContext("2d").drawCanvas(inner, 0, 0);
  return source;
};
const nestedSources = {
  200: once(() => nestedSource(200)),
  20000: once(() => nestedSource(20000)),
};
for (const ops of [200, 20000])
  for (const [engine, engineOptions] of ENGINES)
    add(`nested/clipped-${ops}/${engine}`, () =>
      time(() => {
        const dest = new Canvas(W, H, engineOptions);
        const ctx = dest.getContext("2d");
        ctx.save();
        ctx.beginPath();
        ctx.rect(0, 0, 180, 24);
        ctx.clip();
        ctx.drawImage(nestedSources[ops](), 0, 0);
        ctx.restore();
        // Forces the rasterization, and proves it happened: a clip that
        // misses would leave this transparent and the row would time an
        // empty page.
        if (ctx.getImageData(0, 0, 1, 1).data[3] !== 255)
          throw new Error("the clipped draw painted nothing");
      }),
    );

// Drawn onto a canvas rather than merely constructed. Skia hands back an
// image whose pixels are not decoded until something asks for them, so timing
// the constructor alone measured 0.0 ms for PNG -- the work had not happened
// yet. The figure includes one blit as well as the decode; the blit is the
// same in every row, so the differences between them are the decoders.
for (const [label, format, options, source] of DECODES)
  add(`decode/${label}/cpu`, () => {
    const encoded = source().toBufferSync(format, options);
    const into = new Canvas(source().width, source().height, { gpu: false });
    const ctx = into.getContext("2d");
    return {
      ...time(() => {
        ctx.drawImage(new Image(encoded), 0, 0);
        rasterize(into);
      }),
      bytes: encoded.length,
    };
  });

// Every frame of a 30-frame animation, each drawn: what playing one back
// costs, which a decode of its first frame says nothing about.
for (const [format, options] of ANIMATIONS)
  add(`decode/${format}-animated/cpu`, () => {
    const encoded = reel().toBufferSync(format, options);
    const into = new Canvas(W, H, { gpu: false });
    const ctx = into.getContext("2d");
    return {
      ...time(() => {
        const image = new Image(encoded);
        for (let f = 0; f < image.frames; f++) {
          ctx.drawImage(image.frame(f), 0, 0);
          rasterize(into);
        }
      }, LONG_RUNS),
      bytes: encoded.length,
    };
  });

// Loading an SVG and drawing it onto the page, the same operation as the
// crate's case of this id. Each side takes its own surface's route: here the
// document is recorded as vectors and replayed by the draw, where the crate
// rasterizes it at the page's size first, so the two differ by what that
// route costs and not by what was asked.
for (const elements of [60, 2000])
  add(`decode/svg-${elements}/cpu`, () => {
    const source = svgDoc(elements);
    const into = new Canvas(1000, 800, { gpu: false });
    const ctx = into.getContext("2d");
    return time(() => {
      ctx.drawImage(new Image(source), 0, 0);
      rasterize(into);
    });
  });

// Size beside the time, because neither figure means much alone: the fastest
// encoder here writes the largest file and the slowest writes the smallest,
// and a table of times would report that as a ranking.
for (const [label, format, options, source] of ENCODES)
  add(`encode/${label}/cpu`, () => ({
    ...time(
      () => source().toBufferSync(format, options),
      label.startsWith("avif") ? LONG_RUNS : RUNS,
    ),
    bytes: source().toBufferSync(format, options).length,
  }));

// The four formats that carry a clock do their work between frames: each
// sends the rectangle it differs from its predecessor in, and compresses
// frames on whatever cores are free. A one-page export reaches none of that.
for (const [format, options] of ANIMATIONS)
  add(`animate/${format}/cpu`, () => ({
    ...time(() => reel().toBufferSync(format, options), LONG_RUNS),
    bytes: reel().toBufferSync(format, options).length,
  }));

for (const format of ["pdf", "svg"])
  add(`export/${format}-heavy/cpu`, () => ({
    ...time(() => heavyPage().toBufferSync(format)),
    bytes: heavyPage().toBufferSync(format).length,
  }));

// What the lossy encoders cost in fidelity, beside what they cost in bytes.
// PSNR over the three colour channels, in decibels, against the page as it
// was drawn; alpha is skipped, since this page is opaque. +6 dB is half the
// error, and a difference under about 1 dB is not worth reading. PSNR is
// per-pixel error and knows nothing about vision, so an encoder tuned to a
// perceptual model scores worse than its pictures look.
const psnr = (source, decoded) => {
  let squared = 0;
  for (let i = 0; i < source.length; i += 4)
    for (let c = 0; c < 3; c++) {
      const error = source[i + c] - decoded[i + c];
      squared += error * error;
    }
  const mse = squared / ((source.length / 4) * 3);
  return mse === 0 ? Infinity : 10 * Math.log10((255 * 255) / mse);
};
for (const [label, format, options] of [
  ["jpeg", "jpg", { quality: 0.92 }],
  ["webp", "webp", { quality: 0.9 }],
  ["avif", "avif", { quality: 0.92 }],
])
  add(`quality/${label}`, () => {
    const source = page().getContext("2d").getImageData(0, 0, W, H).data;
    const encoded = page().toBufferSync(format, options);
    const into = new Canvas(W, H, { gpu: false });
    const ctx = into.getContext("2d");
    ctx.drawImage(new Image(encoded), 0, 0);
    return {
      ...value("dB", psnr(source, ctx.getImageData(0, 0, W, H).data)),
      bytes: encoded.length,
    };
  });

// How closely each renderer's edge coverage tracks the geometry: a rectangle
// 0.25 px wide over white should read 25% black. The sweep runs from 0.05 px
// to a full pixel, each on a pixel boundary so all of it lands in one column,
// and the error is summed in 8-bit levels -- comparable between rows,
// meaningless alone. Three rows, because the GPU has two answers and they
// fail differently: MSAA's coverage is a multiple of one over its sample
// count, and shader antialiasing reads high on a thin shape.
const AA_WIDTHS = Array.from({ length: 20 }, (_, i) => (i + 1) / 20);
for (const [label, options, settings] of [
  ["cpu", { gpu: false }, undefined],
  ["gpu-msaa4", { gpu: true }, { msaa: 4 }],
  ["gpu-shader", { gpu: true }, { msaa: 0 }],
])
  add(`aa/${label}`, () => {
    const strip = new Canvas(AA_WIDTHS.length * 4, 8, options);
    const ctx = strip.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, strip.width, strip.height);
    ctx.fillStyle = "black";
    AA_WIDTHS.forEach((w, i) => ctx.fillRect(i * 4, 0, w, 8));
    const px = ctx.getImageData(0, 0, strip.width, strip.height, settings).data;
    // Row 4 of 8, clear of any edge effect at the top and bottom.
    const row = 4 * strip.width * 4;
    const error = AA_WIDTHS.reduce(
      (sum, w, i) => sum + Math.abs(255 - px[row + i * 16] - w * 255),
      0,
    );
    return value("levels", error);
  });

// Each depth in a process of its own, three of them, and the median taken.
// Run inline, the process holds a pool of freed pages that twenty new
// canvases are served out of, and the delta measures the pool: it reported
// RGBAF32 at 0.31 MB against a surface of 16.48. A fresh process has nothing
// to hide in. Two readings a depth: a one-pixel read composites the tile it
// lands in and nothing else, a whole-page read composites the surface and
// hands a copy back -- the range a canvas can occupy.
const MEMORY_CANVASES = 20;
if (args[0] === "--memory-probe") {
  loadLibrary();
  const [, depth, read] = args;
  global.gc?.();
  const before = process.memoryUsage().rss;
  const held = [];
  for (let i = 0; i < MEMORY_CANVASES; i++) {
    const canvas = new Canvas(W, H, { gpu: false, colorType: depth });
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#345";
    ctx.fillRect(0, 0, W, H);
    if (read === "page") ctx.getImageData(0, 0, W, H);
    else rasterize(canvas);
    held.push(canvas);
  }
  process.stdout.write(
    String((process.memoryUsage().rss - before) / held.length),
  );
  process.exit(0);
}
for (const depth of DEPTHS)
  for (const read of ["tile", "page"])
    add(`memory/${depth}/${read}`, () => {
      const readings = [];
      for (let run = 0; run < (QUICK ? 1 : 3); run++)
        readings.push(
          Number(
            execFileSync(
              process.execPath,
              ["--expose-gc", __filename, "--memory-probe", depth, read],
              {
                encoding: "utf8",
              },
            ).trim(),
          ),
        );
      return {
        ...value("MB", quantile(readings, 0.5) / 1048576),
        surface: (W * H * BYTES[depth]) / 1048576,
      };
    });

// ── running them ───────────────────────────────────────────────────────────

if (flag("--list")) {
  for (const { id } of cases) console.log(id);
  process.exit(0);
}
loadLibrary();

// A dev binary leaves the Rust glue unoptimized. Skia is compiled either way,
// so the drawing rows barely move while anything reaching a codec through
// this crate's own per-pixel work multiplies -- AVIF by eight. `just
// bench-npm` builds release first; running this file directly does not, so
// the report names the binary it loaded.
const binary =
  process.env[BINARY_OVERRIDE] ||
  path.resolve(__dirname, "../../lib/skia.node");
// Asked of the checkout this file is in, not of wherever it was run from: a
// report names the commit that was measured.
const git = (...argv) => {
  try {
    return execFileSync("git", argv, {
      cwd: path.resolve(__dirname, "../.."),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
};
const gpuEngine = (() => {
  try {
    return new Canvas(1, 1, { gpu: true }).engine;
  } catch {
    return null;
  }
})();
const run = {
  benchmark: "npm",
  date: new Date().toISOString(),
  quick: QUICK,
  commit: git("rev-parse", "HEAD"),
  dirty: git("status", "--porcelain", "--untracked-files=no") !== "",
  binary: fs.existsSync(binary)
    ? {
        path: binary,
        bytes: fs.statSync(binary).size,
        sha256: crypto
          .createHash("sha256")
          .update(fs.readFileSync(binary))
          .digest("hex"),
      }
    : { path: binary },
  machine: {
    cpu: os.cpus()[0]?.model,
    cores: os.cpus().length,
    os: `${os.platform()} ${os.release()}`,
    arch: os.arch(),
    runtime: `node ${process.version}`,
    gpu: gpuEngine,
  },
  load: { start: os.loadavg() },
  cases: [],
};

console.log(
  `${run.machine.cpu} · ${run.machine.cores} cores · ${run.machine.runtime} · ${run.machine.os}/${run.machine.arch}`,
);
console.log(
  `${path.basename(binary)} · ${((run.binary.bytes ?? 0) / 1048576).toFixed(1)} MB${QUICK ? " · quick" : ""}`,
);

const shown = (r) =>
  r.error
    ? `unavailable: ${r.error}`
    : `${r.median.toFixed(r.unit === "ms" ? 2 : 1).padStart(10)} ${r.unit}` +
      (r.p10 == null
        ? ""
        : `   p10 ${r.p10.toFixed(2)}  p90 ${r.p90.toFixed(2)}`) +
      (r.bytes == null ? "" : `   ${(r.bytes / 1024).toFixed(1)} KB`);

for (const { id, measure } of cases) {
  if (ONLY && !id.startsWith(ONLY)) continue;
  let result;
  try {
    result = { id, ...measure() };
  } catch (err) {
    // A machine with no usable GPU is a normal place to run this file.
    result = { id, error: err.message };
  }
  run.cases.push(result);
  console.log(`  ${id.padEnd(36)} ${shown(result)}`);
}
run.load.end = os.loadavg();

// ── the report ─────────────────────────────────────────────────────────────

const stamp = `${run.date.slice(0, 10)}-${(run.commit ?? "nocommit").slice(0, 8)}`;
fs.mkdirSync(OUT, { recursive: true });
const base = path.join(OUT, `${stamp}-npm`);
fs.writeFileSync(`${base}.json`, `${JSON.stringify(run, null, 2)}\n`);
const cell = (v, digits) =>
  v == null ? "" : Number.isFinite(v) ? v.toFixed(digits) : String(v);
fs.writeFileSync(
  `${base}.md`,
  [
    `# npm benchmark, ${run.date}`,
    "",
    `- Commit: \`${run.commit ?? "unknown"}\`${run.dirty ? " (uncommitted changes)" : ""}`,
    `- Binary: \`${run.binary.path}\`, ${run.binary.bytes ?? "?"} bytes, sha256 \`${run.binary.sha256 ?? "?"}\``,
    `- Machine: ${run.machine.cpu}, ${run.machine.cores} cores, ${run.machine.os} ${run.machine.arch}, ${run.machine.runtime}`,
    `- GPU: ${run.machine.gpu ? `${run.machine.gpu.api ?? ""} ${run.machine.gpu.device ?? ""}`.trim() : "none"}`,
    `- Load average (1, 5, 15 min): ${run.load.start.map((l) => l.toFixed(2)).join(", ")} at the start, ${run.load.end.map((l) => l.toFixed(2)).join(", ")} at the end`,
    run.quick
      ? "- **Quick run: one sample a case, no warmup. Not a measurement.**"
      : "",
    "",
    "| case | unit | median | p10 | p90 | runs | bytes |",
    "| --- | --- | ---: | ---: | ---: | ---: | ---: |",
    ...run.cases.map((r) =>
      r.error
        ? `| ${r.id} | | unavailable: ${r.error} | | | | |`
        : `| ${r.id} | ${r.unit} | ${cell(r.median, 3)} | ${cell(r.p10, 3)} | ${cell(r.p90, 3)} | ${r.runs} | ${r.bytes ?? ""} |`,
    ),
    "",
  ].join("\n"),
);
console.log(`\n${base}.md\n${base}.json`);
