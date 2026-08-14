//
// The illustrations on docs/api/path2d.md
//
// Run from a checkout:
//
//     just docs-assets      # or: node docs/generate/path2d.js
//
// Every drawing here is the code the surrounding documentation shows, run
// rather than described. That is the whole point: these images were
// inherited with no way to reproduce them, so nothing checked that they
// still matched the library, and a change to `trim` or `simplify` would
// have left the page illustrating the old behaviour indefinitely.
//
// Written to match the look of what they replace -- white plates on
// transparency, so the same file reads on a light or a dark docs theme --
// rather than to reproduce those files pixel for pixel. Thirteen of the
// originals had been through an image optimiser, so byte-equality was never
// available even in principle.
//

const { Canvas, Path2D } = require("../../lib");
const { writeFileSync } = require("fs");
const { join } = require("path");

const OUT = process.argv[2] || join(__dirname, "..", "assets", "api");

// The docs render these at twice their logical size, which is what the
// `@2x` in every filename means.
const DENSITY = 2;
const WIDTH = 870;

const PLATE = "#ffffff";
const INK = "#2a2a2a";
const HAIR = "#000000";
const LABEL = "#666666";
const GHOST = "#b2b2b2";

const PLATE_RADIUS = 6;
const LABEL_SIZE = 13;
const LABEL_FOOT = 14;

/// One row of labelled panels.
///
/// `draw` is handed a context already translated to its panel's origin and
/// clipped to it, so each drawing works in its own coordinates and does not
/// need to know where in the row it landed.
function row(name, panels, { height, pad = 10, label = LABEL_FOOT } = {}) {
  const cell = (WIDTH - pad * (panels.length + 1)) / panels.length;
  const canvas = new Canvas(WIDTH, height + label + pad * 2, { gpu: false });
  const ctx = canvas.getContext("2d");

  panels.forEach(({ caption, draw }, i) => {
    const x = pad + i * (cell + pad);
    const y = pad;

    ctx.save();
    ctx.fillStyle = PLATE;
    ctx.beginPath();
    ctx.roundRect(x, y, cell, height, PLATE_RADIUS);
    ctx.fill();
    ctx.clip();
    ctx.translate(x, y);
    draw(ctx, cell, height);
    ctx.restore();

    if (caption) {
      ctx.fillStyle = LABEL;
      ctx.font = `${LABEL_SIZE}px Helvetica`;
      ctx.textAlign = "center";
      ctx.fillText(caption, x + cell / 2, y + height + label - 2);
      ctx.textAlign = "left";
    }
  });

  const file = join(OUT, `${name}@2x.png`);
  writeFileSync(file, canvas.toBufferSync("png", { density: DENSITY }));
  console.log(
    `  ${`${name}@2x.png`.padEnd(30)} ${canvas.width * DENSITY}x${canvas.height * DENSITY}`,
  );
}

/// Fits `draw`'s natural coordinates into the panel it was given.
function fit(ctx, box, { width, height, x = 0, y = 0 }) {
  const scale = Math.min((box.w - 16) / width, (box.h - 16) / height);
  ctx.translate(box.w / 2, box.h / 2);
  ctx.scale(scale, scale);
  ctx.translate(-x - width / 2, -y - height / 2);
}

// ── the two shapes every boolean example starts from ───────────────────────
// Straight from the page: "In all the following examples we'll be starting
// off with a pair of overlapping shapes."
const overlapping = () => {
  const oval = new Path2D();
  oval.arc(100, 100, 100, 0, 2 * Math.PI);
  const rect = new Path2D();
  rect.rect(0, 100, 100, 100);
  return { oval, rect };
};

function booleanOps() {
  const { oval, rect } = overlapping();
  const box = { width: 200, height: 200, x: 0, y: 0 };

  // The layered pair, before any operation. An SVG rather than a PNG,
  // because it is line art and the page links it as one.
  {
    const canvas = new Canvas(240, 240, { gpu: false });
    const ctx = canvas.getContext("2d");
    fit(ctx, { w: 240, h: 240 }, box);
    ctx.fillStyle = "rgba(42,42,42,0.35)";
    ctx.fill(oval);
    ctx.fill(rect);
    ctx.strokeStyle = HAIR;
    ctx.lineWidth = 1.5;
    ctx.stroke(oval);
    ctx.stroke(rect);
    const file = join(OUT, "operation-none.svg");
    writeFileSync(file, canvas.toBufferSync("svg"));
    console.log(`  ${"operation-none.svg".padEnd(30)} 240x240`);
  }

  // `xor` is filled with "evenodd" because the page says so: it "is liable
  // to create a path with lines that cross over one another so you'll get
  // different results" under the two winding rules.
  const ops = [
    ["complement", (a, b) => a.complement(b), "nonzero"],
    ["difference", (a, b) => a.difference(b), "nonzero"],
    ["intersect", (a, b) => a.intersect(b), "nonzero"],
    ["union", (a, b) => a.union(b), "nonzero"],
    ["xor", (a, b) => a.xor(b), "evenodd"],
  ];

  row(
    "operations",
    ops.map(([caption, apply, rule]) => ({
      caption,
      draw: (ctx, w, h) => {
        fit(ctx, { w, h }, box);
        ctx.fillStyle = INK;
        ctx.fill(apply(rect, oval), rule);
      },
    })),
    { height: 150 },
  );
}

// ── jitter ─────────────────────────────────────────────────────────────────
function jitter() {
  const cube = new Path2D();
  cube.rect(100, 100, 100, 100);
  cube.rect(150, 50, 100, 100);
  cube.moveTo(100, 100);
  cube.lineTo(150, 50);
  cube.moveTo(200, 100);
  cube.lineTo(250, 50);
  cube.moveTo(200, 200);
  cube.lineTo(250, 150);

  const box = { width: 200, height: 200, x: 90, y: 40 };
  const stroke = (ctx, path, w, h) => {
    fit(ctx, { w, h }, box);
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2;
    ctx.stroke(path);
  };

  row(
    "effect-jitter",
    [
      { caption: "original", draw: (c, w, h) => stroke(c, cube, w, h) },
      {
        caption: "jitter(1, 2)",
        draw: (c, w, h) => stroke(c, cube.jitter(1, 2), w, h),
      },
      {
        caption: "jitter(1, 2, 1337)",
        draw: (c, w, h) => stroke(c, cube.jitter(1, 2, 1337), w, h),
      },
      {
        caption: "jitter(10, 1)",
        draw: (c, w, h) => stroke(c, cube.jitter(10, 1), w, h),
      },
    ],
    { height: 190 },
  );
}

// ── round ──────────────────────────────────────────────────────────────────
function round() {
  const spikes = new Path2D();
  spikes.moveTo(50, 225);
  spikes.lineTo(100, 25);
  spikes.lineTo(150, 225);
  spikes.lineTo(200, 25);
  spikes.lineTo(250, 225);
  spikes.lineTo(300, 25);

  const box = { width: 260, height: 210, x: 45, y: 20 };
  const stroke = (ctx, path, w, h) => {
    fit(ctx, { w, h }, box);
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3;
    ctx.lineJoin = "round";
    ctx.stroke(path);
  };

  row(
    "effect-round",
    [
      { caption: "original", draw: (c, w, h) => stroke(c, spikes, w, h) },
      {
        caption: "round(80)",
        draw: (c, w, h) => stroke(c, spikes.round(80), w, h),
      },
    ],
    { height: 190 },
  );
}

// ── trim ───────────────────────────────────────────────────────────────────
function trim() {
  const orig = new Path2D();
  orig.arc(100, 100, 50, Math.PI, 0);

  const box = { width: 110, height: 60, x: 45, y: 45 };
  const panel = (path) => (ctx, w, h) => {
    fit(ctx, { w, h }, box);
    // The original underneath in grey, so what was trimmed away is visible
    // rather than merely absent.
    ctx.strokeStyle = GHOST;
    ctx.lineWidth = 4;
    ctx.stroke(orig);
    ctx.strokeStyle = INK;
    ctx.stroke(path);
  };

  row(
    "effect-trim",
    [
      { caption: "original", draw: panel(orig) },
      { caption: "trim(0.25, 0.75)", draw: panel(orig.trim(0.25, 0.75)) },
      {
        caption: "trim(0.25, 0.75, true)",
        draw: panel(orig.trim(0.25, 0.75, true)),
      },
      { caption: "trim(0.25)", draw: panel(orig.trim(0.25)) },
      { caption: "trim(-0.25)", draw: panel(orig.trim(-0.25)) },
    ],
    { height: 130 },
  );
}

// ── simplify ───────────────────────────────────────────────────────────────
function simplify() {
  const cross = new Path2D(`
    M 10,50 h 100 v 20 h -100 Z
    M 50,10 h 20 v 100 h -20 Z
  `);

  const box = { width: 110, height: 110, x: 5, y: 5 };
  const panel = (path, rule) => (ctx, w, h) => {
    fit(ctx, { w, h }, box);
    ctx.fillStyle = INK;
    ctx.fill(path, rule);
  };

  row(
    "effect-simplify",
    [
      { caption: 'original, "nonzero"', draw: panel(cross, "nonzero") },
      { caption: 'original, "evenodd"', draw: panel(cross, "evenodd") },
      {
        caption: 'simplify(), "evenodd"',
        draw: panel(cross.simplify(), "evenodd"),
      },
    ],
    { height: 170 },
  );
}

// ── unwind ─────────────────────────────────────────────────────────────────
function unwind() {
  const orig = new Path2D(`
    M 0 0 h 100 v 100 h -100 Z
    M 50 30 l 20 20 l -20 20 l -20 -20 Z
  `);

  const box = { width: 100, height: 100, x: 0, y: 0 };
  const panel = (path, rule) => (ctx, w, h) => {
    fit(ctx, { w, h }, box);
    ctx.fillStyle = INK;
    ctx.fill(path, rule);
  };

  row(
    "effect-unwind",
    [
      { caption: 'original, "nonzero"', draw: panel(orig, "nonzero") },
      { caption: 'original, "evenodd"', draw: panel(orig, "evenodd") },
      {
        caption: 'unwind(), "nonzero"',
        draw: panel(orig.unwind(), "nonzero"),
      },
    ],
    { height: 170 },
  );
}

// ── interpolate ────────────────────────────────────────────────────────────
function interpolate() {
  const start = new Path2D();
  start.moveTo(-200, 100);
  start.bezierCurveTo(-300, 100, -200, 200, -300, 200);
  start.bezierCurveTo(-200, 200, -300, 300, -200, 300);

  const end = new Path2D();
  end.moveTo(-100, 100);
  end.bezierCurveTo(0, 100, -100, 200, 0, 200);
  end.bezierCurveTo(-100, 200, 0, 300, -100, 300);

  // Every panel gets the same 100x200 frame, centred on its own shape.
  //
  // A single fixed frame does not work here: interpolation moves the path
  // as well as morphing it -- `weight 0` sits at x -300..-200 and `weight 1`
  // at -100..0 -- so one frame centred on the start left the end hanging off
  // the right edge, which is what the first version of this drew. Fitting
  // each panel to its *own* bounds does not work either, because at
  // `weight 0.5` the two shapes cross and the path is exactly zero wide: a
  // vertical line, correctly, and a divide by zero for anything scaling to
  // fit it.
  const FRAME = { width: 100, height: 200 };
  const weights = [0, 0.25, 0.5, 0.75, 1];

  row(
    "effect-interpolate",
    weights.map((weight) => ({
      caption: `weight ${weight}`,
      draw: (ctx, w, h) => {
        const path = start.interpolate(end, weight);
        const { left, right } = path.bounds;
        fit(
          ctx,
          { w, h },
          {
            ...FRAME,
            x: (left + right) / 2 - FRAME.width / 2,
            y: 100,
          },
        );
        ctx.strokeStyle = INK;
        ctx.lineWidth = 6;
        ctx.stroke(path);
      },
    })),
    { height: 190 },
  );
}

// ── points ─────────────────────────────────────────────────────────────────
function points() {
  let path = new Path2D();
  path.arc(100, 100, 50, 0, 2 * Math.PI);
  path.rect(100, 50, 50, 50);
  path = path.simplify();

  const box = { width: 130, height: 130, x: 45, y: 45 };

  row(
    "effect-points",
    [
      {
        caption: "the path",
        draw: (ctx, w, h) => {
          fit(ctx, { w, h }, box);
          ctx.strokeStyle = INK;
          ctx.lineWidth = 2;
          ctx.stroke(path);
        },
      },
      {
        caption: "points(10)",
        draw: (ctx, w, h) => {
          fit(ctx, { w, h }, box);
          ctx.strokeStyle = GHOST;
          ctx.lineWidth = 2;
          ctx.stroke(path);
          ctx.fillStyle = INK;
          for (const [x, y] of path.points(10)) {
            ctx.beginPath();
            ctx.arc(x, y, 3, 0, 2 * Math.PI);
            ctx.fill();
          }
        },
      },
    ],
    { height: 170 },
  );
}

console.log("docs/api/path2d.md");
booleanOps();
interpolate();
jitter();
points();
round();
simplify();
trim();
unwind();
