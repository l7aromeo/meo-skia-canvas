//
// A report card: charts, type and effects composed into one image
//
// Run from a checkout:
//
//     just build            # or: npm run build
//     node examples/node/report-card.js [outdir]
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//

const fs = require("fs");
const path = require("path");

// Imported the way a consumer would, from the package entry point.
const {
  Canvas,
  Path2D,
  ColorFilter,
  ImageFilter,
  MaskFilter,
  Shader,
  ParagraphBuilder,
  TextDecoration,
  loadImage,
} = require("../../lib");

const OUT = process.argv[2] || "out";
fs.mkdirSync(OUT, { recursive: true });

const W = 900;
const H = 620;
const DATA = [
  ["Mon", 62],
  ["Tue", 78],
  ["Wed", 45],
  ["Thu", 91],
  ["Fri", 84],
  ["Sat", 33],
  ["Sun", 51],
];

// A logo mark, drawn once on its own canvas and reused as an image -- the
// pattern anyone building a report generator ends up with.
function makeLogo() {
  const c = new Canvas(120, 120);
  const ctx = c.getContext("2d");

  const g = ctx.createConicGradient(0, 60, 60);
  for (let i = 0; i <= 6; i++)
    g.addColorStop(i / 6, `hsl(${200 + i * 25} 85% 60%)`);
  ctx.fillStyle = g;
  ctx.beginPath();
  ctx.arc(60, 60, 52, 0, Math.PI * 2);
  ctx.fill();

  ctx.globalCompositeOperation = "destination-out";
  ctx.beginPath();
  ctx.arc(60, 60, 30, 0, Math.PI * 2);
  ctx.fill();
  ctx.globalCompositeOperation = "source-over";

  return c;
}

function drawCard(ctx) {
  // Background: vertical gradient plus a noise shader for texture.
  const bg = ctx.createLinearGradient(0, 0, 0, H);
  bg.addColorStop(0, "#0f1b2d");
  bg.addColorStop(1, "#1b2b45");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, W, H);

  ctx.save();
  ctx.globalAlpha = 0.06;
  ctx.fillStyle = new Shader("fractal-noise", 0.9, 0.9, 3, 4);
  ctx.fillRect(0, 0, W, H);
  ctx.restore();

  // Header panel with a soft shadow.
  ctx.save();
  ctx.shadowColor = "rgba(0,0,0,0.45)";
  ctx.shadowBlur = 24;
  ctx.shadowOffsetY = 8;
  ctx.fillStyle = "#16233a";
  ctx.beginPath();
  ctx.roundRect(40, 36, W - 80, 96, 18);
  ctx.fill();
  ctx.restore();

  // Logo, drawn from the other canvas.
  ctx.drawCanvas(makeLogo(), 62, 48, 72, 72);

  ctx.fillStyle = "#eaf2ff";
  ctx.font = "600 30px Helvetica";
  ctx.fillText("Weekly throughput", 156, 82);

  ctx.fillStyle = "#7f9ac0";
  ctx.font = "16px Helvetica";
  ctx.fillText("Requests served per day, in thousands", 156, 108);

  ctx.textAlign = "right";
  ctx.fillStyle = "#4ade80";
  ctx.font = "600 26px Helvetica";
  ctx.fillText("+18.4%", W - 68, 92);
  ctx.textAlign = "left";

  // Chart panel.
  const panel = { x: 40, y: 160, w: W - 80, h: 300 };
  ctx.fillStyle = "rgba(255,255,255,0.04)";
  ctx.beginPath();
  ctx.roundRect(panel.x, panel.y, panel.w, panel.h, 16);
  ctx.fill();

  // Gridlines, clipped to the panel.
  ctx.save();
  ctx.beginPath();
  ctx.roundRect(panel.x, panel.y, panel.w, panel.h, 16);
  ctx.clip();

  ctx.strokeStyle = "rgba(255,255,255,0.08)";
  ctx.lineWidth = 1;
  for (let i = 0; i <= 4; i++) {
    const y = panel.y + 40 + (i * (panel.h - 80)) / 4;
    ctx.beginPath();
    ctx.moveTo(panel.x + 20, y);
    ctx.lineTo(panel.x + panel.w - 20, y);
    ctx.stroke();
  }

  // Bars, each a rounded path with a gradient and a glow on the tallest.
  const max = Math.max(...DATA.map(([, v]) => v));
  const slot = (panel.w - 60) / DATA.length;
  DATA.forEach(([label, value], i) => {
    const bh = ((panel.h - 100) * value) / max;
    const x = panel.x + 30 + i * slot + slot * 0.18;
    const bw = slot * 0.64;
    const y = panel.y + panel.h - 46 - bh;

    const grad = ctx.createLinearGradient(0, y, 0, y + bh);
    grad.addColorStop(0, value === max ? "#7dd3fc" : "#3b82f6");
    grad.addColorStop(1, value === max ? "#2563eb" : "#1e3a8a");

    if (value === max) {
      ctx.save();
      ctx.maskFilter = new MaskFilter("outer", 9);
      ctx.fillStyle = "#38bdf8";
      ctx.beginPath();
      ctx.roundRect(x, y, bw, bh, 7);
      ctx.fill();
      ctx.restore();
    }

    ctx.fillStyle = grad;
    ctx.beginPath();
    ctx.roundRect(x, y, bw, bh, 7);
    ctx.fill();

    ctx.fillStyle = "#9fb6d4";
    ctx.font = "14px Helvetica";
    ctx.textAlign = "center";
    ctx.fillText(label, x + bw / 2, panel.y + panel.h - 20);
    ctx.fillStyle = "#dbeafe";
    ctx.font = "600 14px Helvetica";
    ctx.fillText(String(value), x + bw / 2, y - 10);
    ctx.textAlign = "left";
  });
  ctx.restore();

  // A trend line over the bars, using Path2D operations.
  const line = new Path2D();
  DATA.forEach(([, value], i) => {
    const x = panel.x + 30 + i * slot + slot * 0.5;
    const y = panel.y + panel.h - 46 - ((panel.h - 100) * value) / max - 24;
    i ? line.lineTo(x, y) : line.moveTo(x, y);
  });
  ctx.strokeStyle = "rgba(250,204,21,0.9)";
  ctx.lineWidth = 2.5;
  ctx.lineJoin = "round";
  ctx.stroke(line.round(10));

  // Footnote, laid out as a wrapping paragraph with a styled run.
  const builder = new ParagraphBuilder({
    textStyle: { fontSize: 15, color: "#8fa8c8", fontFamilies: ["Helvetica"] },
    textAlign: "left",
  });
  builder.addText("Figures are provisional and exclude cached responses. ");
  builder.pushStyle({
    fontSize: 15,
    color: "#facc15",
    fontFamilies: ["Helvetica"],
    decoration: TextDecoration.Underline,
  });
  builder.addText("Thursday's peak");
  builder.pop();
  builder.addText(
    " coincided with the scheduled reindex, which is expected to recur next week.",
  );

  const para = builder.build();
  para.layout(W - 120);
  ctx.drawParagraph(para, 60, 496);

  return para;
}

(async () => {
  const canvas = new Canvas(W, H);
  const ctx = canvas.getContext("2d");
  const para = drawCard(ctx);

  const results = [];

  // Every export format a consumer might reach for.
  for (const [fmt, opts] of [
    ["png", {}],
    ["jpg", { quality: 0.92 }],
    ["webp", { quality: 0.9 }],
    ["pdf", {}],
    ["svg", {}],
  ]) {
    const file = path.join(OUT, `report.${fmt}`);
    await canvas.toFile(file, opts);
    results.push([fmt, fs.statSync(file).size]);
  }

  // Multi-page PDF through newPage, as the docs describe it.
  const book = new Canvas(400, 300);
  for (let p = 0; p < 3; p++) {
    const c = p === 0 ? book.getContext("2d") : book.newPage(400, 300);
    c.fillStyle = ["#334155", "#475569", "#64748b"][p];
    c.fillRect(0, 0, 400, 300);
    c.fillStyle = "white";
    c.font = "28px Helvetica";
    c.fillText(`Page ${p + 1} of 3`, 40, 160);
  }
  await book.toFile(path.join(OUT, "book.pdf"));
  results.push(["pdf (3 pages)", fs.statSync(path.join(OUT, "book.pdf")).size]);

  // Round-trip: encode, reload through loadImage, redraw, read back.
  const png = await canvas.toBuffer("png");
  const reloaded = await loadImage(png);
  const check = new Canvas(reloaded.width, reloaded.height);
  const cctx = check.getContext("2d");
  cctx.drawImage(reloaded, 0, 0);

  const a = ctx.getImageData(0, 0, W, H).data;
  const b = cctx.getImageData(0, 0, W, H).data;
  let differing = 0;
  for (let i = 0; i < a.length; i += 4)
    if (a[i] !== b[i] || a[i + 3] !== b[i + 3]) differing++;

  const dataURL = await canvas.toDataURL("png");

  console.log("exports");
  for (const [fmt, size] of results) {
    console.log(
      `  ${fmt.padEnd(14)} ${(size / 1024).toFixed(1).padStart(8)} KB`,
    );
  }

  console.log("\nchecks");
  console.log(
    "  paragraph height        ",
    para.getHeight().toFixed(1),
    "px over",
    para.getNumberOfLines(),
    "lines",
  );
  console.log(
    "  reloaded image          ",
    `${reloaded.width}x${reloaded.height}`,
    "natural",
    `${reloaded.naturalWidth}x${reloaded.naturalHeight}`,
  );
  console.log("  png round-trip differs  ", differing, "of", W * H, "pixels");
  console.log(
    "  data URL                ",
    dataURL.slice(0, 30) + "…",
    `(${(dataURL.length / 1024).toFixed(0)} KB)`,
  );
  console.log("  isContextLost()         ", ctx.isContextLost());
  console.log(
    "  engine                  ",
    canvas.engine.renderer,
    "|",
    canvas.engine.device,
  );

  // toBlob, the callback-shaped export added this release.
  await new Promise((resolve) =>
    canvas.toBlob((blob) => {
      console.log(
        "  toBlob                  ",
        blob.type,
        `${(blob.size / 1024).toFixed(1)} KB`,
      );
      resolve();
    }),
  );
})();
