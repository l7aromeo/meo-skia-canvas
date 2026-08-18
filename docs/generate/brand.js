//
// The hero banners on README.md, docs/index.md and docs/node.md
//
// Run from a checkout:
//
//     just docs-assets      # or: node docs/generate/brand.js
//
// The banners this replaced were Skia Canvas's own -- samizdatco's wordmark
// and diamond mark, describing a library for Node.js. They were inherited
// with the fork and were never ours to fly: the mark belongs to that
// project, and the sentence under it stopped being true the moment the crate
// grew a Rust surface of its own.
//
// Drawn by the library rather than by hand, so the banner is regenerated
// like every other image here and cannot drift from what the code does.
//

const { Canvas } = require("../../lib");
const { writeFileSync } = require("fs");
const { join } = require("path");

const OUT = process.argv[2] || join(__dirname, "..", "assets", "brand");
const DENSITY = 2;

const WIDTH = 1280;
const HEIGHT = 330;

// The mark: a ring swept through the spectrum, on a rounded tile.
const MARK = { x: 96, y: 65, size: 200 };

// Two lines, because what the library is and what it writes are two
// different claims and the second one is the newer half of the project.
const TAGLINE = [
  "A multi-threaded, GPU-accelerated 2D graphics environment",
  "for Rust and Node \u2014 animation, raster and vector exports",
];

/// Light and dark are the same drawing with six colors swapped, so the two
/// files cannot say different things about the project.
const THEMES = {
  hero: {
    background: "#ffffff",
    tile: "#f2f4f8",
    title: "#101828",
    tagline: "#475467",
    handle: "#98a2b3",
    point: "#101828",
  },
  "hero-dark": {
    background: "#0b1220",
    tile: "#141d2f",
    title: "#f8fafc",
    tagline: "#94a3b8",
    handle: "#5b6b85",
    point: "#f8fafc",
  },
};

function save(name, canvas) {
  const file = join(OUT, `${name}@2x.png`);
  writeFileSync(file, canvas.toBufferSync("png", { density: DENSITY }));
  console.log(
    `  ${`${name}@2x.png`.padEnd(30)} ${canvas.width * DENSITY}x${canvas.height * DENSITY}`,
  );
}

/// The mark: one cubic Bézier with its control handles showing.
///
/// A curve mid-edit rather than a picture of a finished drawing -- the four
/// points, the two handles and the segment between them are what a path
/// actually is, and every one of them is a call this library exports. The
/// anchors sit on the diagonal so the whole thing reads as a rising stroke
/// at any size; the handles are what stops it reading as a plain squiggle.
function mark(ctx, theme) {
  const { x, y, size } = MARK;
  // Everything below is written for a 200-unit tile and scaled from there,
  // so the geometry stays put if the banner is laid out differently.
  const unit = size / 200;
  const at = (u, v) => [x + u * unit, y + v * unit];

  const [startX, startY] = at(44, 150);
  const [c1X, c1Y] = at(76, 44);
  const [c2X, c2Y] = at(124, 156);
  const [endX, endY] = at(156, 50);

  ctx.save();
  ctx.fillStyle = theme.tile;
  ctx.beginPath();
  ctx.roundRect(x, y, size, size, 46 * unit);
  ctx.fill();

  // The curve, swept through the spectrum along its own length.
  const ink = ctx.createLinearGradient(startX, startY, endX, endY);
  ["#22d3ee", "#6366f1", "#ec4899", "#f59e0b"].forEach((color, i, all) =>
    ink.addColorStop(i / (all.length - 1), color),
  );
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.strokeStyle = ink;
  ctx.lineWidth = 20 * unit;
  ctx.beginPath();
  ctx.moveTo(startX, startY);
  ctx.bezierCurveTo(c1X, c1Y, c2X, c2Y, endX, endY);
  ctx.stroke();

  // The handles: thin, dashed, and under the points they belong to.
  ctx.strokeStyle = theme.handle;
  ctx.lineWidth = 3 * unit;
  ctx.setLineDash([7 * unit, 7 * unit]);
  ctx.beginPath();
  ctx.moveTo(startX, startY);
  ctx.lineTo(c1X, c1Y);
  ctx.moveTo(endX, endY);
  ctx.lineTo(c2X, c2Y);
  ctx.stroke();
  ctx.setLineDash([]);

  // Anchors filled, control points hollow -- the convention every vector
  // editor uses, and the reason the mark reads as a path being drawn.
  const dot = (cx, cy, radius, filled) => {
    ctx.beginPath();
    ctx.arc(cx, cy, radius * unit, 0, Math.PI * 2);
    if (filled) {
      ctx.fillStyle = theme.point;
      ctx.fill();
      return;
    }
    ctx.fillStyle = theme.tile;
    ctx.fill();
    ctx.strokeStyle = theme.point;
    ctx.lineWidth = 4 * unit;
    ctx.stroke();
  };
  dot(startX, startY, 11, true);
  dot(endX, endY, 11, true);
  dot(c1X, c1Y, 8, false);
  dot(c2X, c2Y, 8, false);
  ctx.restore();
}

function banner(name, theme) {
  const canvas = new Canvas(WIDTH, HEIGHT, { gpu: false });
  const ctx = canvas.getContext("2d");

  ctx.fillStyle = theme.background;
  ctx.fillRect(0, 0, WIDTH, HEIGHT);

  mark(ctx, theme);

  const left = MARK.x + MARK.size + 90;
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";

  ctx.fillStyle = theme.title;
  ctx.font = "600 76px Helvetica";
  ctx.fillText("meo-skia-canvas", left, 155);

  ctx.fillStyle = theme.tagline;
  ctx.font = "27px Helvetica";
  TAGLINE.forEach((line, i) => ctx.fillText(line, left, 213 + i * 42));

  save(name, canvas);
}

Object.entries(THEMES).forEach(([name, theme]) => banner(name, theme));
