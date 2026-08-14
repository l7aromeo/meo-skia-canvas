//
// The illustrations on docs/api/context.md
//
// Run from a checkout:
//
//     just docs-assets      # or: node docs/generate/context.js
//
// As with `path2d.js`: the drawings are the code the page already prints,
// run rather than described, so each image demonstrates its own snippet
// instead of resembling it. Where the page shows a program in a `<details>`
// block -- `texturesDemo`, `metricsDemo`, `baselinesDemo` -- that program is
// what runs here.
//

const { Canvas, Path2D } = require("../../lib");
const { writeFileSync } = require("fs");
const { join } = require("path");

const OUT = process.argv[2] || join(__dirname, "..", "assets", "api");
const DENSITY = 2;

function save(name, canvas, ext = "png") {
  const file = join(OUT, `${name}@2x.${ext}`);
  writeFileSync(file, canvas.toBufferSync(ext, { density: DENSITY }));
  console.log(
    `  ${`${name}@2x.${ext}`.padEnd(30)} ${canvas.width * DENSITY}x${canvas.height * DENSITY}`,
  );
}

/// A white plate under a drawing, so one file reads on either docs theme.
function plate(width, height) {
  const canvas = new Canvas(width, height, { gpu: false });
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, width, height);
  return { canvas, ctx };
}

const LABEL = "#666666";
function caption(ctx, text, x, y) {
  ctx.save();
  ctx.fillStyle = LABEL;
  ctx.font = "13px Helvetica";
  ctx.textAlign = "center";
  // Set, not inherited. `outlineText` leaves `textBaseline` on "top" from
  // the snippet it runs, and a caption that inherited that grew downward
  // from its y instead of sitting on it -- thirteen pixels off the bottom of
  // the canvas, which is exactly how far the clipping check said it was.
  ctx.textBaseline = "alphabetic";
  ctx.fillText(text, x, y);
  ctx.restore();
}

// ── lineDashMarker ─────────────────────────────────────────────────────────
function lineDashMarker() {
  // Each arc spans `x + 20 .. x + 120` and the five are stepped by 100, so
  // the ink runs 20..520 and the canvas has to cover it. An earlier version
  // shifted everything left by 40 and put the captions on a different
  // stride, which left every label sixty pixels right of its own arc and the
  // last arc off the edge entirely.
  const STEP = 100;
  const RADIUS = 100;
  const { canvas, ctx } = plate(560, 195);

  // The marker paths, verbatim from the page.
  const caret = new Path2D();
  caret.moveTo(-8, -8);
  caret.lineTo(0, 0);
  caret.lineTo(-8, 8);

  const dot = new Path2D();
  dot.arc(0, 0, 4, 0, 2 * Math.PI);
  dot.closePath(); // use fill rather than stroke

  const cross = new Path2D();
  cross.moveTo(-6, -6);
  cross.lineTo(6, 6);
  cross.moveTo(-6, 6);
  cross.lineTo(6, -6);

  // draw arcs using different markers
  function drawArc(x, color) {
    ctx.strokeStyle = color;
    ctx.lineWidth = 4;
    ctx.beginPath();
    ctx.arc(x + 120, 120, 100, -Math.PI, -Math.PI / 2);
    ctx.stroke();
  }

  ctx.save();
  ctx.setLineDash([20]);
  drawArc(0, "orange");

  ctx.lineDashMarker = caret;
  drawArc(100, "deepskyblue");

  ctx.lineDashMarker = dot;
  drawArc(200, "limegreen");

  ctx.lineDashMarker = cross;
  drawArc(300, "red");

  ctx.setLineDash([]);
  drawArc(400, "#aaa");
  ctx.restore();

  // Centred under the arc each one names: arc `i` spans `i*STEP + 20` to
  // `i*STEP + 120`, so its middle is `i*STEP + 70`.
  ["dashes", "caret", "dot", "cross", "none"].forEach((name, i) =>
    caption(ctx, name, i * STEP + RADIUS - 30, 175),
  );
  save("lineDashMarker", canvas);
}

// ── projection ─────────────────────────────────────────────────────────────
// The page: "The results below show the image generated when the
// `createProjection()` call is omitted entirely, called (as above) with just
// a `quad` argument, or called with two different values for the optional
// `basis` argument."
function projection() {
  const SIDE = 210;

  const one = (label, basis) => {
    const canvas = new Canvas(512, 512, { gpu: false });
    const ctx = canvas.getContext("2d");
    const { width: w, height: h } = canvas;
    ctx.font = "900 480px Times";
    ctx.textAlign = "center";
    ctx.fillStyle = "#aaa";
    ctx.fillRect(0, 0, w, h);

    const quad = [
      w * 0.33,
      h / 2, // upper left
      w * 0.66,
      h / 2, // upper right
      w,
      h * 0.9, // bottom right
      0,
      h * 0.9, // bottom left
    ];

    if (label !== "no projection") {
      const matrix = basis
        ? ctx.createProjection(quad, basis)
        : ctx.createProjection(quad); // use default basis
      ctx.setTransform(matrix);
    }

    ctx.fillStyle = "white";
    ctx.fillRect(10, 10, w - 20, h - 20);
    ctx.fillStyle = "#900";
    ctx.fillText("@", w / 2, h - 40);
    return { canvas, label };
  };

  // The two `basis` panels have to differ from the default one, or the plate
  // shows the same picture three times. A basis equal to the canvas rectangle
  // *is* the default, and a unit square magnifies by 512, which puts every
  // drawn pixel outside the frame -- both render as no illustration at all.
  // So: a rectangle larger than the canvas, which shrinks the drawing inside
  // the quad, and a skewed source, which counter-skews it.
  const cases = [
    one("no projection"),
    one("quad only"),
    one("basis: [-256,-256, 768,768]", [-256, -256, 768, 768]),
    one("basis: skewed quad", [0, 0, 512, 0, 640, 512, 128, 512]),
  ];

  const pad = 10;
  const { canvas, ctx } = plate(
    pad + cases.length * (SIDE + pad),
    SIDE + pad * 2 + 16,
  );
  cases.forEach(({ canvas: src, label }, i) => {
    const x = pad + i * (SIDE + pad);
    ctx.drawCanvas(src, x, pad, SIDE, SIDE);
    caption(ctx, label, x + SIDE / 2, pad + SIDE + 13);
  });
  save("projection", canvas);
}

// ── drawCanvas ─────────────────────────────────────────────────────────────
function drawCanvas() {
  const src = new Canvas(10, 10, { gpu: false });
  const srcCtx = src.getContext("2d");
  srcCtx.font = "italic 10px Times";
  srcCtx.fillText("¶", 2, 8);

  const dst = new Canvas(350, 175, { gpu: false });
  const dstCtx = dst.getContext("2d");
  dstCtx.fillStyle = "#ffffff";
  dstCtx.fillRect(0, 0, 350, 175);
  dstCtx.drawImage(src, 0, 0, 150, 150);
  dstCtx.drawCanvas(src, 200, 0, 150, 150);

  // The point of the picture: one was resampled, the other replayed.
  caption(dstCtx, "drawImage — resampled", 75, 166);
  caption(dstCtx, "drawCanvas — replayed", 275, 166);
  save("drawCanvas", dst);
}

// ── outlineText ────────────────────────────────────────────────────────────
function outlineText() {
  const { canvas, ctx } = plate(320, 215);
  ctx.textBaseline = "top";
  ctx.font = "bold 140px Helvetica";
  const ampersand = ctx.outlineText("&");

  ctx.save();
  ctx.translate(90, 30);
  // The page writes this loop with `Math.random()`. A committed image drawn
  // that way is different on every regeneration, which is noise in every
  // diff and the same objection the README makes to letting the GPU draw the
  // still sheets -- so the scatter is seeded here. Same picture, same file,
  // every run.
  let seed = 1337;
  const random = () => {
    // xorshift32: four lines, and the only property wanted is that it gives
    // the same sequence twice.
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return (seed >>> 0) / 0x100000000;
  };
  for (let i = 0; i < 8000; i++) {
    const x = random() * 100,
      y = random() * 120;
    ctx.fillStyle = ampersand.contains(x, y) ? "lightblue" : "#eee";
    ctx.fillRect(x, y, 2, 2);
  }
  ctx.restore();

  caption(ctx, "contains() against an outlineText() path", 160, 205);
  save("outlineText", canvas);
}

// ── createTexture ──────────────────────────────────────────────────────────
// The `texturesDemo` from the page's sample-code block, trimmed to the two
// patterns that show what the argument does rather than all four.
function textures() {
  const build = (outline) => {
    const canvas = new Canvas(512, 256, { gpu: false });
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, 512, 256);

    const n = 10;
    const nylonPath = new Path2D();
    nylonPath.moveTo(0, n / 4);
    nylonPath.lineTo(n / 4, n / 4);
    nylonPath.lineTo(n / 4, 0);
    nylonPath.moveTo((n * 3) / 4, n);
    nylonPath.lineTo((n * 3) / 4, (n * 3) / 4);
    nylonPath.lineTo(n, (n * 3) / 4);
    nylonPath.moveTo(n / 4, n / 2);
    nylonPath.lineTo(n / 4, (n * 3) / 4);
    nylonPath.lineTo(n / 2, (n * 3) / 4);
    nylonPath.moveTo(n / 2, n / 4);
    nylonPath.lineTo((n * 3) / 4, n / 4);
    nylonPath.lineTo((n * 3) / 4, n / 2);

    const d = 1;
    const dotPath = new Path2D();
    dotPath.arc(0, 0, d, 0, 2 * Math.PI);

    const shapes = [
      { texture: ctx.createTexture(n, { path: nylonPath, line: 1, outline }) },
      {
        texture: ctx.createTexture([6, 6], {
          path: dotPath,
          line: 0,
          outline,
        }),
      },
      { texture: ctx.createTexture([8, 8], { line: 2, outline }) },
    ];

    shapes.forEach(({ texture }, i) => {
      ctx.save();
      ctx.fillStyle = texture;
      ctx.strokeStyle = texture;
      ctx.lineWidth = 18;
      ctx.beginPath();
      ctx.arc(90 + i * 165, 120, 70, 0, 2 * Math.PI);
      outline ? ctx.stroke() : ctx.fill();
      ctx.restore();
      ctx.strokeStyle = "#2a2a2a";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.arc(90 + i * 165, 120, 70, 0, 2 * Math.PI);
      ctx.stroke();
    });

    caption(
      ctx,
      outline ? "outline: true — the marks only" : "outline: false — clipped",
      256,
      240,
    );
    return canvas;
  };

  save("createTexture", build(false));
  save("createTexture-outline", build(true));
}

// ── measureText, and the two metrics diagrams ──────────────────────────────
function metrics() {
  const TEXT = "Sphinx of black quartz";
  const FONT = "600 64px Helvetica";

  // measureText: the box the metrics describe, drawn over the text.
  {
    const { canvas, ctx } = plate(750, 300);
    ctx.font = FONT;
    const m = ctx.measureText(TEXT);
    const x = 40,
      y = 180;

    ctx.fillStyle = "rgba(88,166,255,0.18)";
    ctx.fillRect(
      x - m.actualBoundingBoxLeft,
      y - m.actualBoundingBoxAscent,
      m.actualBoundingBoxLeft + m.actualBoundingBoxRight,
      m.actualBoundingBoxAscent + m.actualBoundingBoxDescent,
    );
    ctx.fillStyle = "#2a2a2a";
    ctx.fillText(TEXT, x, y);

    // The advance, on the baseline.
    ctx.strokeStyle = "#f0883e";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(x, y);
    ctx.lineTo(x + m.width, y);
    ctx.stroke();

    ctx.fillStyle = LABEL;
    ctx.font = "14px Helvetica";
    [
      `width ${m.width.toFixed(1)}`,
      `actualBoundingBoxAscent ${m.actualBoundingBoxAscent.toFixed(1)}`,
      `actualBoundingBoxDescent ${m.actualBoundingBoxDescent.toFixed(1)}`,
      `fontBoundingBoxAscent ${m.fontBoundingBoxAscent.toFixed(1)}`,
    ].forEach((line, i) => ctx.fillText(line, 40, 230 + i * 20));
    save("measureText", canvas);
  }

  // The baselines, each drawn as a rule through the same run.
  //
  // Row spacing has to clear the type, not the rule: at 34px with rows 22px
  // apart every line of "Baseline" landed on the one above it and the whole
  // image was a single smear.
  //
  // `ideographic` and `bottom` come out identical, and that is correct
  // rather than a fault in the drawing. Both resolve to `-descent` -- see
  // `Baseline::get_offset`, which follows Chromium's
  // `TextMetrics::GetFontBaseline` and maps the two the same way. Measured
  // here before it was believed: for a 30px run every other baseline lands
  // somewhere distinct, and those two land on the same pixel in every font
  // tried, with CJK text as well as Latin. The image says so, because two
  // identical rows with no explanation read as a bug.
  {
    const BASELINES = [
      "top",
      "hanging",
      "middle",
      "alphabetic",
      "ideographic",
      "bottom",
    ];
    const SIZE = 30;
    const STEP = 56;
    const LEFT = 36;
    const RULE_END = 470;
    const { canvas, ctx } = plate(500, 44 + BASELINES.length * STEP);

    ctx.font = `500 ${SIZE}px Helvetica`;
    const wordWidth = ctx.measureText("Baseline").width;

    BASELINES.forEach((baseline, i) => {
      const y = 44 + i * STEP;

      // The rule is the baseline being named; the word sits against it.
      ctx.strokeStyle = "#c8c8c8";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(24, y + 0.5);
      ctx.lineTo(RULE_END, y + 0.5);
      ctx.stroke();

      ctx.font = `500 ${SIZE}px Helvetica`;
      ctx.textBaseline = baseline;
      ctx.fillStyle = "#2a2a2a";
      ctx.fillText("Baseline", LEFT, y);

      // Immediately after the word, so the label and the thing it labels are
      // read together rather than across a gap.
      ctx.font = "14px Helvetica";
      ctx.textBaseline = "middle";
      ctx.fillStyle = LABEL;
      ctx.fillText(`"${baseline}"`, LEFT + wordWidth + 18, y);
    });

    // Brace the two that coincide, and say why.
    const first = 44 + 4 * STEP;
    const second = 44 + 5 * STEP;
    ctx.strokeStyle = "#f0883e";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(RULE_END - 6, first);
    ctx.lineTo(RULE_END + 4, first);
    ctx.lineTo(RULE_END + 4, second);
    ctx.lineTo(RULE_END - 6, second);
    ctx.stroke();

    ctx.save();
    ctx.translate(RULE_END + 14, (first + second) / 2);
    ctx.rotate(-Math.PI / 2);
    ctx.font = "12px Helvetica";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = "#f0883e";
    ctx.fillText("both = -descent", 0, 0);
    ctx.restore();

    ctx.textBaseline = "alphabetic";
    save("measureTextBaselines", canvas);
  }

  // The per-line metrics a wrapped run reports.
  //
  // `x` and `y` are the left and *top* edges of each line's box, relative to
  // the point the text was drawn from -- so the box is `(drawX + x, drawY +
  // y)` and nothing else. Subtracting `baseline` as well, which is what this
  // did at first, stacked all three boxes on top of one another and left the
  // picture showing a single stray rectangle.
  {
    const LEFT = 40;
    const TOP = 50;
    const WRAP = 520;
    const { canvas, ctx } = plate(620, 170);
    ctx.font = "500 28px Helvetica";
    ctx.textWrap = true;
    const wrapped =
      "Text that wraps across several lines reports one entry per line, " +
      "each with its own width, baseline and extent.";
    const m = ctx.measureText(wrapped, WRAP);

    ctx.fillStyle = "#2a2a2a";
    ctx.fillText(wrapped, LEFT, TOP, WRAP);

    // Each line's own box, from `lines`.
    ctx.strokeStyle = "#58a6ff";
    ctx.lineWidth = 1;
    for (const line of m.lines) {
      ctx.strokeRect(
        LEFT + line.x + 0.5,
        TOP + line.y + 0.5,
        line.width,
        line.height,
      );
    }

    ctx.fillStyle = LABEL;
    ctx.font = "13px Helvetica";
    ctx.fillText(
      `${m.lines.length} entries in TextMetrics.lines, one box each`,
      LEFT,
      156,
    );
    save("measureTextLines", canvas);
  }
}

console.log("docs/api/context.md");
drawCanvas();
lineDashMarker();
metrics();
outlineText();
projection();
textures();
