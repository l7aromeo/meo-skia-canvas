//
// Test cards: one labelled panel per feature area
//
// Run from a checkout:
//
//     just build            # or: npm run build
//     node examples/node/feature-sheet.js [outdir]
//
// The require below is relative because the repo is not linked to itself.
// In your own project it is:  require("meo-skia-canvas")
//

const fs = require("fs");
const path = require("path");
const {
  Canvas,
  Path2D,
  DOMMatrix,
  ColorFilter,
  ImageFilter,
  MaskFilter,
  Shader,
  ParagraphBuilder,
  TextDecoration,
  TextDecorationStyle,
  CanvasTexture,
  loadImage,
} = require("../../lib");

const OUT = process.argv[2] || "out";
fs.mkdirSync(OUT, { recursive: true });

const COLS = 4,
  CELL = 300,
  PAD = 16,
  HEAD = 34;
const notes = [];

// Every canvas here is drawn on the CPU so the committed images are
// reproducible on any machine. The GPU path antialiases differently -- it
// resolves partial coverage in a shader rather than by sampling -- so the
// same script on a GPU box would rewrite these files without a code change.
const CPU = { gpu: false };

function sheet(title, panels) {
  const rows = Math.ceil(panels.length / COLS);
  const W = COLS * CELL + PAD * 2;
  const H = rows * CELL + PAD * 2 + 58;
  const canvas = new Canvas(W, H, CPU);
  const ctx = canvas.getContext("2d");

  ctx.fillStyle = "#0d1117";
  ctx.fillRect(0, 0, W, H);
  ctx.fillStyle = "#e6edf3";
  ctx.font = "600 24px Helvetica";
  ctx.fillText(title, PAD + 4, 38);

  panels.forEach(([label, draw], i) => {
    const x = PAD + (i % COLS) * CELL;
    const y = PAD + 50 + Math.floor(i / COLS) * CELL;

    ctx.save();
    ctx.fillStyle = "#161b22";
    ctx.beginPath();
    ctx.roundRect(x + 4, y + 4, CELL - 8, CELL - 8, 10);
    ctx.fill();

    ctx.fillStyle = "#7d8590";
    ctx.font = "500 12px Helvetica";
    ctx.fillText(label, x + 16, y + 24);
    ctx.restore();

    ctx.save();
    ctx.beginPath();
    ctx.rect(x + 8, y + HEAD, CELL - 16, CELL - HEAD - 12);
    ctx.clip();
    ctx.translate(x + 8, y + HEAD);
    try {
      draw(ctx, CELL - 16, CELL - HEAD - 12);
    } catch (e) {
      notes.push(`${label}: ${e.message.slice(0, 70)}`);
      ctx.fillStyle = "#f85149";
      ctx.font = "12px Helvetica";
      ctx.fillText("failed", 8, 24);
    }
    ctx.restore();
  });

  return canvas;
}

// ── swatch used by several panels ──────────────────────────────────────────
function swatch(w, h) {
  const c = new Canvas(w, h, CPU);
  const g = c.getContext("2d");
  const grad = g.createLinearGradient(0, 0, w, h);
  grad.addColorStop(0, "#f97316");
  grad.addColorStop(0.5, "#ec4899");
  grad.addColorStop(1, "#6366f1");
  g.fillStyle = grad;
  g.fillRect(0, 0, w, h);
  g.fillStyle = "rgba(255,255,255,0.9)";
  for (let i = 0; i < 5; i++) {
    g.beginPath();
    g.arc(20 + i * 22, h / 2, 8, 0, Math.PI * 2);
    g.fill();
  }
  g.strokeStyle = "#111";
  g.lineWidth = 3;
  g.strokeRect(0, 0, w, h);
  return c;
}

// ═══════════════════════════════════════════════════════════ TYPOGRAPHY ════
const TYPO = [
  [
    "textAlign · every value",
    (ctx, w) => {
      ctx.fillStyle = "#30363d";
      ctx.fillRect(w / 2 - 0.5, 6, 1, 200);
      ctx.font = "15px Helvetica";
      ["left", "center", "right", "start", "end"].forEach((a, i) => {
        ctx.textAlign = a;
        ctx.fillStyle = "#58a6ff";
        ctx.fillText(a, w / 2, 30 + i * 28);
      });
      ctx.textAlign = "left";
    },
  ],

  [
    "textBaseline · every value",
    (ctx, w) => {
      ctx.font = "14px Helvetica";
      [
        "top",
        "hanging",
        "middle",
        "alphabetic",
        "ideographic",
        "bottom",
      ].forEach((b, i) => {
        const y = 24 + i * 30;
        ctx.strokeStyle = "#30363d";
        ctx.beginPath();
        ctx.moveTo(6, y);
        ctx.lineTo(w - 6, y);
        ctx.stroke();
        ctx.textBaseline = b;
        ctx.fillStyle = "#7ee787";
        ctx.fillText(b, 10, y);
      });
      ctx.textBaseline = "alphabetic";
    },
  ],

  [
    "fontVariant · caps & figures",
    (ctx) => {
      ctx.font = "20px Helvetica";
      const rows = [
        ["normal", "normal"],
        ["small-caps", "small-caps"],
        ["titling-caps", "titling-caps"],
        ["oldstyle-nums", "oldstyle-nums"],
      ];
      rows.forEach(([variant, label], i) => {
        ctx.fontVariant = variant;
        ctx.fillStyle = "#e6edf3";
        ctx.fillText("Hamburg 2026", 10, 34 + i * 42);
        ctx.fillStyle = "#7d8590";
        ctx.font = "10px Helvetica";
        ctx.fillText(label, 10, 48 + i * 42);
        ctx.font = "20px Helvetica";
      });
      ctx.fontVariant = "normal";
    },
  ],

  [
    "letterSpacing · wordSpacing",
    (ctx) => {
      ctx.font = "17px Helvetica";
      const rows = [
        ["0px", "0px"],
        ["4px", "0px"],
        ["0px", "14px"],
        ["-1px", "0px"],
      ];
      rows.forEach(([ls, ws], i) => {
        ctx.letterSpacing = ls;
        ctx.wordSpacing = ws;
        ctx.fillStyle = "#e6edf3";
        ctx.fillText("spaced out text", 10, 34 + i * 44);
        ctx.fillStyle = "#7d8590";
        ctx.font = "10px Helvetica";
        ctx.fillText(`letter ${ls} · word ${ws}`, 10, 50 + i * 44);
        ctx.font = "17px Helvetica";
      });
      ctx.letterSpacing = "0px";
      ctx.wordSpacing = "0px";
    },
  ],

  [
    "outlineText → Path2D",
    (ctx) => {
      ctx.font = "700 46px Helvetica";
      const p = ctx.outlineText("Glyph");
      ctx.save();
      ctx.translate(10, 70);
      ctx.strokeStyle = "#f778ba";
      ctx.lineWidth = 1.2;
      ctx.stroke(p);
      ctx.translate(0, 66);
      ctx.fillStyle = "#1f6feb";
      ctx.fill(p.jitter(4, 1.4, 7));
      ctx.restore();
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("stroked, then jitter()", 10, 200);
    },
  ],

  [
    "measureText · TextMetrics",
    (ctx) => {
      ctx.font = "26px Helvetica";
      const text = "Measure me";
      const m = ctx.measureText(text);
      const x = 12,
        y = 70;
      ctx.fillStyle = "rgba(88,166,255,0.18)";
      ctx.fillRect(
        x - m.actualBoundingBoxLeft,
        y - m.actualBoundingBoxAscent,
        m.actualBoundingBoxLeft + m.actualBoundingBoxRight,
        m.actualBoundingBoxAscent + m.actualBoundingBoxDescent,
      );
      ctx.strokeStyle = "#f0883e";
      ctx.beginPath();
      ctx.moveTo(x, y);
      ctx.lineTo(x + m.width, y);
      ctx.stroke();
      ctx.fillStyle = "#e6edf3";
      ctx.fillText(text, x, y);
      ctx.fillStyle = "#7d8590";
      ctx.font = "11px Helvetica";
      ctx.fillText(`width ${m.width.toFixed(1)}`, 12, 120);
      ctx.fillText(`ascent ${m.actualBoundingBoxAscent.toFixed(1)}`, 12, 138);
      ctx.fillText(`descent ${m.actualBoundingBoxDescent.toFixed(1)}`, 12, 156);
    },
  ],

  [
    "textWrap · ctx.fillText",
    (ctx, w) => {
      ctx.textWrap = true;
      ctx.font = "14px Helvetica";
      ctx.fillStyle = "#e6edf3";
      ctx.fillText(
        "With textWrap enabled the context breaks a long string across lines by itself, using the width given to fillText.",
        10,
        26,
        w - 20,
      );
      ctx.textWrap = false;
    },
  ],

  [
    "Paragraph · decoration styles",
    (ctx, w) => {
      const styles = [
        ["Solid", TextDecorationStyle.Solid],
        ["Double", TextDecorationStyle.Double],
        ["Dotted", TextDecorationStyle.Dotted],
        ["Dashed", TextDecorationStyle.Dashed],
        ["Wavy", TextDecorationStyle.Wavy],
      ];
      styles.forEach(([name, ds], i) => {
        const b = new ParagraphBuilder({
          textStyle: {
            fontSize: 17,
            color: "#e6edf3",
            fontFamilies: ["Helvetica"],
            decoration: TextDecoration.Underline,
            decorationStyle: ds,
            decorationColor: "#f778ba",
          },
        });
        b.addText(name + " underline");
        const p = b.build();
        p.layout(w - 20);
        ctx.drawParagraph(p, 10, 12 + i * 38);
      });
    },
  ],
];

// ═══════════════════════════════════════════════════════ IMAGE & COLOUR ════
const IMAGE = [
  [
    "drawImage · 9-arg crop",
    async (ctx, w, h) => {
      const img = IMAGE_ASSET;
      ctx.drawImage(img, 0, 0, 60, 60, 8, 10, 118, 118);
      ctx.drawImage(img, 60, 60, 60, 60, 134, 10, 118, 118);
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("top-left crop", 8, 146);
      ctx.fillText("bottom-right crop", 134, 146);
      // The uncropped source the two above were cut from. Square, because
      // the swatch is: a 244x60 box would flatten its circles into ellipses
      // and look like a rendering fault rather than a deliberate stretch.
      ctx.drawImage(img, 8, 152, 94, 94);
      ctx.fillText("source, uncropped", 112, 202);
    },
  ],

  [
    "imageSmoothingQuality",
    // Resampling applies to an *image* source. A canvas source goes through
    // drawCanvas, which replays the recording at the destination scale
    // instead of resampling pixels, so the smoothing settings have nothing
    // to filter -- the bottom-right cell is the same checker with no
    // resampling artifacts at all.
    (ctx) => {
      // Square cells: the source is 8x8, so anything but an equal scale on
      // both axes turns its squares into rectangles and reads as a defect.
      const S = 104;
      const cell = (i) => 8 + i * (S + 26);
      ctx.font = "10px Helvetica";
      ["low", "high"].forEach((q, i) => {
        ctx.imageSmoothingQuality = q;
        ctx.drawImage(CHECKER_ASSET, cell(i), 10, S, S);
        ctx.fillStyle = "#7d8590";
        ctx.fillText(q, cell(i), 126);
      });
      ctx.imageSmoothingEnabled = false;
      ctx.drawImage(CHECKER_ASSET, cell(0), 134, S, S);
      ctx.imageSmoothingEnabled = true;
      ctx.drawCanvas(CHECKER_CANVAS, cell(1), 134, S, S);
      ctx.fillStyle = "#7d8590";
      ctx.fillText("smoothing off", cell(0), 250);
      ctx.fillText("drawCanvas · replayed", cell(1), 250);
    },
  ],

  [
    "createPattern · repetition",
    (ctx, w, h) => {
      const tile = new Canvas(24, 24, CPU),
        t = tile.getContext("2d");
      t.fillStyle = "#1f6feb";
      t.fillRect(0, 0, 12, 12);
      t.fillStyle = "#f778ba";
      t.fillRect(12, 12, 12, 12);
      ctx.fillStyle = ctx.createPattern(tile, "repeat");
      ctx.fillRect(8, 10, w - 16, 100);
      // A pattern is anchored to the coordinate origin, not to the rect it
      // fills. Filling at y=120 with repeat-x drew nothing at all: the one
      // tile-high band lives at y=0..24, which that rect never touches.
      ctx.save();
      ctx.translate(8, 120);
      ctx.fillStyle = ctx.createPattern(tile, "repeat-x");
      ctx.fillRect(0, 0, w - 16, 90);
      ctx.restore();
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("repeat", 8, 226);
      ctx.fillText("repeat-x: one band, then nothing below it", 8, 240);
    },
  ],

  [
    "ImageData · direct pixels",
    (ctx, w, h) => {
      const id = ctx.createImageData(w - 16, 150);
      for (let y = 0; y < 150; y++)
        for (let x = 0; x < w - 16; x++) {
          const i = (y * (w - 16) + x) * 4;
          const d = Math.hypot(x - (w - 16) / 2, y - 75);
          id.data[i] = 40 + 200 * Math.abs(Math.sin(d / 12));
          id.data[i + 1] = 60 + 120 * Math.abs(Math.cos(d / 18));
          id.data[i + 2] = 200;
          id.data[i + 3] = 255;
        }
      const holder = new Canvas(id.width, id.height, CPU);
      holder.getContext("2d").putImageData(id, 0, 0);
      ctx.drawCanvas(holder, 8, 14);
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText(
        `${id.width}x${id.height}, ${id.bytesPerPixel} bytes/px`,
        8,
        186,
      );
      ctx.fillText(
        "putImageData ignores the transform, so it goes via a canvas",
        8,
        202,
      );
    },
  ],
];

// ═════════════════════════════════════════════════════ EFFECTS & PATHS ═════
const EFFECTS = [
  [
    "ImageFilter · drop-shadow",
    (ctx, w) => {
      ctx.imageFilter = new ImageFilter("drop-shadow", 6, 6, 5, 5, "#000");
      ctx.fillStyle = "#f0883e";
      ctx.beginPath();
      ctx.roundRect(20, 24, 110, 90, 14);
      ctx.fill();
      ctx.imageFilter = new ImageFilter(
        "drop-shadow-only",
        6,
        6,
        5,
        5,
        "#58a6ff",
      );
      ctx.fillStyle = "#fff";
      ctx.beginPath();
      ctx.roundRect(150, 24, 110, 90, 14);
      ctx.fill();
      ctx.imageFilter = null;
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("drop-shadow", 20, 132);
      ctx.fillText("drop-shadow-only", 150, 132);

      ctx.imageFilter = new ImageFilter("dilate", 3, 3);
      ctx.fillStyle = "#7ee787";
      ctx.font = "700 26px Helvetica";
      ctx.fillText("dilate", 20, 190);
      ctx.imageFilter = new ImageFilter("erode", 1, 1);
      ctx.fillText("erode", 150, 190);
      ctx.imageFilter = null;
    },
  ],

  [
    "ColorFilter · matrix & table",
    (ctx, w) => {
      const src = swatch(120, 84);
      ctx.drawCanvas(src, 8, 14, 120, 84);
      ctx.colorFilter = new ColorFilter("luma");
      ctx.drawCanvas(src, 140, 14, 120, 84);
      const table = new Uint8Array(256);
      for (let i = 0; i < 256; i++) table[i] = 255 - i;
      ctx.colorFilter = new ColorFilter("table", table);
      ctx.drawCanvas(src, 8, 110, 120, 84);
      ctx.colorFilter = new ColorFilter("blend", "#1f6feb", "multiply");
      ctx.drawCanvas(src, 140, 110, 120, 84);
      ctx.colorFilter = null;
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("plain · luma · inverted table · blend", 8, 210);
    },
  ],

  [
    "Shader · noise fills",
    (ctx, w, h) => {
      ctx.fillStyle = new Shader("turbulence", 0.05, 0.05, 4, 3);
      ctx.fillRect(8, 14, w - 16, 92);
      ctx.fillStyle = new Shader("fractal-noise", 0.02, 0.02, 5, 9);
      ctx.fillRect(8, 112, w - 16, 92);
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("turbulence / fractal-noise", 8, 218);
    },
  ],

  [
    "CanvasTexture · hatching",
    (ctx, w) => {
      const dash = new Path2D();
      dash.moveTo(0, 0);
      dash.lineTo(10, 10);
      ctx.fillStyle = new CanvasTexture(12, {
        path: dash,
        color: "#7ee787",
        line: 2,
      });
      ctx.beginPath();
      ctx.roundRect(8, 14, w - 16, 92, 10);
      ctx.fill();
      ctx.fillStyle = new CanvasTexture([14, 8], {
        color: "#f778ba",
        line: 3,
        angle: Math.PI / 2,
      });
      ctx.beginPath();
      ctx.roundRect(8, 116, w - 16, 92, 10);
      ctx.fill();
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("path texture / line texture", 8, 222);
    },
  ],

  [
    "Path2D · boolean ops",
    (ctx, w) => {
      const a = new Path2D();
      a.rect(30, 24, 92, 92);
      const b = new Path2D();
      b.arc(122, 116, 52, 0, Math.PI * 2);
      const ops = [
        ["union", "#1f6feb"],
        ["intersect", "#f0883e"],
        ["xor", "#7ee787"],
      ];
      ops.forEach(([op, col], i) => {
        ctx.save();
        ctx.translate(0, 0);
        ctx.globalAlpha = 0.55;
        ctx.fillStyle = col;
        ctx.fill(a[op](b).offset(i * 6, i * 6));
        ctx.restore();
      });
      ctx.globalAlpha = 1;
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("union · intersect · xor", 8, 210);
    },
  ],

  [
    "Path2D · trim & interpolate",
    (ctx, w) => {
      const star = new Path2D();
      for (let i = 0; i < 10; i++) {
        const r = i % 2 ? 26 : 58,
          a = (i / 10) * Math.PI * 2 - Math.PI / 2;
        const x = 132 + Math.cos(a) * r,
          y = 74 + Math.sin(a) * r;
        i ? star.lineTo(x, y) : star.moveTo(x, y);
      }
      star.closePath();
      ctx.strokeStyle = "#30363d";
      ctx.lineWidth = 2;
      ctx.stroke(star);
      ctx.strokeStyle = "#f778ba";
      ctx.lineWidth = 4;
      ctx.stroke(star.trim(0, 0.55));

      const circle = new Path2D();
      for (let i = 0; i < 10; i++) {
        const a = (i / 10) * Math.PI * 2 - Math.PI / 2;
        const x = 132 + Math.cos(a) * 44,
          y = 180 + Math.sin(a) * 30;
        i ? circle.lineTo(x, y) : circle.moveTo(x, y);
      }
      circle.closePath();
      const small = new Path2D();
      for (let i = 0; i < 10; i++) {
        const a = (i / 10) * Math.PI * 2 - Math.PI / 2;
        const x = 132 + Math.cos(a) * 12,
          y = 180 + Math.sin(a) * 12;
        i ? small.lineTo(x, y) : small.moveTo(x, y);
      }
      small.closePath();
      ctx.strokeStyle = "#58a6ff";
      ctx.lineWidth = 2;
      [0, 0.35, 0.7, 1].forEach((t) =>
        ctx.stroke(circle.interpolate(small, t)),
      );
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("trim() / interpolate()", 8, 218);
    },
  ],

  [
    "createProjection · perspective",
    (ctx, w) => {
      const board = new Canvas(120, 120, CPU),
        g = board.getContext("2d");
      for (let y = 0; y < 6; y++)
        for (let x = 0; x < 6; x++) {
          g.fillStyle = (x + y) % 2 ? "#e6edf3" : "#1f6feb";
          g.fillRect(x * 20, y * 20, 20, 20);
        }
      ctx.save();
      const quad = [40, 20, 240, 50, 210, 190, 60, 160];
      ctx.transform(
        ctx.createProjection(quad, [0, 0, 120, 0, 120, 120, 0, 120]),
      );
      ctx.drawCanvas(board, 0, 0);
      ctx.restore();
      ctx.fillStyle = "#7d8590";
      ctx.font = "10px Helvetica";
      ctx.fillText("quad-mapped drawCanvas", 8, 216);
    },
  ],

  [
    "lineDash · caps · joins",
    (ctx, w) => {
      ctx.lineWidth = 7;
      [
        [[], "solid"],
        [[14, 8], "dashed"],
        [[2, 8], "dotted"],
      ].forEach(([d, label], i) => {
        ctx.setLineDash(d);
        ctx.lineCap = i === 2 ? "round" : "butt";
        ctx.strokeStyle = ["#58a6ff", "#f0883e", "#7ee787"][i];
        ctx.beginPath();
        ctx.moveTo(14, 28 + i * 34);
        ctx.lineTo(w - 14, 28 + i * 34);
        ctx.stroke();
      });
      ctx.setLineDash([]);
      ["miter", "round", "bevel"].forEach((j, i) => {
        ctx.lineJoin = j;
        ctx.strokeStyle = "#f778ba";
        ctx.lineWidth = 10;
        ctx.beginPath();
        ctx.moveTo(24 + i * 84, 190);
        ctx.lineTo(56 + i * 84, 140);
        ctx.lineTo(88 + i * 84, 190);
        ctx.stroke();
        ctx.fillStyle = "#7d8590";
        ctx.font = "10px Helvetica";
        ctx.fillText(j, 32 + i * 84, 208);
      });
    },
  ],
];

// ── run ────────────────────────────────────────────────────────────────────
let IMAGE_ASSET, CHECKER_ASSET, CHECKER_CANVAS;

// An 8x8 checker, small enough that upscaling it makes the resampling
// filter obvious.
function checker() {
  const c = new Canvas(8, 8, CPU),
    t = c.getContext("2d");
  for (let y = 0; y < 8; y++)
    for (let x = 0; x < 8; x++) {
      t.fillStyle = (x + y) % 2 ? "#58a6ff" : "#0d1117";
      t.fillRect(x, y, 1, 1);
    }
  return c;
}

(async () => {
  IMAGE_ASSET = await loadImage(await swatch(120, 120).toBuffer("png"));
  CHECKER_CANVAS = checker();
  CHECKER_ASSET = await loadImage(await CHECKER_CANVAS.toBuffer("png"));

  const sheets = [
    ["Typography", TYPO, "typography"],
    ["Images & pixels", IMAGE, "images"],
    ["Effects & paths", EFFECTS, "effects"],
  ];

  for (const [title, panels, name] of sheets) {
    const canvas = sheet(title, panels);
    await canvas.toFile(path.join(OUT, `${name}.png`));
    console.log(
      `${name}.png`.padEnd(18),
      `${canvas.width}x${canvas.height}`,
      (fs.statSync(path.join(OUT, `${name}.png`)).size / 1024).toFixed(0) +
        " KB",
    );
  }

  console.log(
    notes.length ? "\nfailures:" : "\nall panels drew without throwing",
  );
  notes.forEach((n) => console.log("  - " + n));
})();
