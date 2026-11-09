// TEMPORARY: prints how COLR glyphs drawn on this platform compare with
// Chrome 158's macOS references, for the decision on whether this platform
// draws them through the library's own painter. Dropped before merge.
/* global __dirname, console, process */
"use strict";

const path = require("path"),
  fs = require("fs"),
  root = path.resolve(__dirname, "..", ".."),
  { Canvas, FontLibrary, loadImage } = require(path.join(root, "lib")),
  refs = path.join(root, "tests", "assets", "images", "colr"),
  fonts = path.join(root, "tests", "assets", "fonts");

FontLibrary.use("ColrGlyphs", [
  path.join(fonts, "color-fonts", "test_glyphs-glyf_colr_1_variable.ttf"),
]);
FontLibrary.use("ColrV0", [
  path.join(fonts, "colr-v0", "COLR-palettes-test-font.ttf"),
]);

const layout = (n) =>
  JSON.parse(fs.readFileSync(path.join(refs, `${n}.json`), "utf8"));

const reference = async (spec) => {
  const image = await loadImage(path.join(refs, spec.reference)),
    c = new Canvas(spec.width, spec.height),
    ctx = c.getContext("2d");
  ctx.drawImage(image, 0, 0);
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

const sheet = (spec) => {
  const c = new Canvas(spec.width, spec.height),
    ctx = c.getContext("2d");
  ctx.fillStyle = spec.background;
  ctx.fillRect(0, 0, spec.width, spec.height);
  if (spec.cases) {
    spec.cases.forEach(({ size, text }, i) => {
      ctx.save();
      ctx.translate(
        (i % spec.columns) * spec.cellWidth,
        Math.floor(i / spec.columns) * spec.cellHeight,
      );
      ctx.beginPath();
      ctx.rect(0, 0, spec.cellWidth, spec.cellHeight);
      ctx.clip();
      ctx.font = `${size}px ColrV0`;
      ctx.fillStyle = spec.fillStyle;
      ctx.fillText(text, spec.x, spec.baselineAbove + size);
      ctx.restore();
    });
  } else {
    ctx.font = `${spec.size}px ColrGlyphs`;
    if (spec.fontVariationSettings)
      ctx.fontVariationSettings = spec.fontVariationSettings;
    ctx.fillStyle = spec.fillStyle;
    spec.codepoints.forEach((hex, i) =>
      ctx.fillText(
        String.fromCodePoint(parseInt(hex, 16)),
        (i % spec.columns) * spec.cell + spec.x,
        Math.floor(i / spec.columns) * spec.cell + spec.baseline,
      ),
    );
  }
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

const coloured = (px) => {
  let n = 0;
  for (let i = 0; i < px.length; i += 4) {
    const s = Math.max(
      Math.abs(px[i] - px[i + 1]),
      Math.abs(px[i + 1] - px[i + 2]),
    );
    if (px[i + 3] && s > 25) n++;
  }
  return n;
};

const compare = (ours, chrome, w, h, skip) => {
  const flat = (px, x, y) => {
    const at = (y * w + x) * 4;
    for (let dy = -1; dy <= 1; dy++)
      for (let dx = -1; dx <= 1; dx++) {
        const k = ((y + dy) * w + x + dx) * 4;
        for (let c = 0; c < 3; c++)
          if (Math.abs(px[k + c] - px[at + c]) > 2) return false;
      }
    return true;
  };
  let interior = 0,
    interiorPixels = 0,
    past81 = 0,
    past8 = 0;
  for (let y = 1; y < h - 1; y++)
    for (let x = 1; x < w - 1; x++) {
      if (skip(x, y)) continue;
      const k = (y * w + x) * 4;
      const d = Math.max(
        Math.abs(ours[k] - chrome[k]),
        Math.abs(ours[k + 1] - chrome[k + 1]),
        Math.abs(ours[k + 2] - chrome[k + 2]),
      );
      if (d > 81) past81++;
      if (d > 8) past8++;
      if (flat(ours, x, y) && flat(chrome, x, y)) {
        interiorPixels++;
        interior = Math.max(interior, d);
      }
    }
  return { interior, interiorPixels, past81, past8 };
};

// The cell holding U+FE000, which the font maps to .notdef: Chrome draws it
// as nothing, and it is not a colour glyph.
const notdef = (spec) => {
  if (!spec.codepoints) return () => false;
  const index = spec.codepoints.indexOf("fe000");
  return (x, y) =>
    Math.floor(y / spec.cell) * spec.columns + Math.floor(x / spec.cell) ===
    index;
};

(async () => {
  console.log(
    `platform ${process.platform} ${process.arch} ${require("os").release()}`,
  );
  for (const name of [
    "chrome-test-glyphs-48px",
    "chrome-test-glyphs-48px-varied",
    "chrome-colr-v0",
  ]) {
    const spec = layout(name),
      ours = sheet(spec),
      chrome = await reference(spec);
    console.log(
      name,
      JSON.stringify({
        colouredOurs: coloured(ours),
        colouredChrome: coloured(chrome),
        ...compare(ours, chrome, spec.width, spec.height, notdef(spec)),
      }),
    );
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
