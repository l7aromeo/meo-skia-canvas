// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary, loadImage } = require("../../lib"),
  fs = require("fs"),
  path = require("path");

const REFERENCES = path.join(__dirname, "..", "assets", "images", "colr");

/**
 * Where Chrome's COLR references were drawn: macOS, where this library draws
 * a COLR glyph from its paint graph as Chrome draws it through Fontations.
 * Elsewhere FreeType draws the glyph, with its own anti-aliasing and
 * placement.
 */
// TEMPORARY: MEO_COLR_OWN_ROUTE runs the comparisons with the own painter
// on another platform. Dropped before merge.
const CHROME_MEASURED_HERE =
  process.platform === "darwin" || process.env.MEO_COLR_OWN_ROUTE
    ? false
    : "Chrome 158's COLR glyphs were drawn on macOS, against this library's own painter there";

/**
 * How far a channel may stand from Chrome's where neither image changes
 * within a pixel of it -- inside a fill or a gradient, away from any edge.
 * Measured at 1 over 764,234 such pixels of the 221 test glyphs, and over
 * 764,523 with their axes varied: a gradient's rounding.
 */
const INTERIOR_TOLERANCE = 1;

/**
 * How far an anti-aliased edge pixel may stand from Chrome's before it is
 * counted. A circle and a triangle filled on a 2D canvas, no font involved,
 * come out of Chrome 158 and this library up to 81 apart at their edges, so
 * an edge further apart than that is not the rasterizer's difference.
 */
const EDGE_ENVELOPE = 81;

/** The test fonts, registered once. */
const fonts = (() => {
  let registered = false;
  return () => {
    if (registered) return;
    const fixture = (...parts) =>
      path.join(__dirname, "..", "assets", "fonts", ...parts);
    FontLibrary.use("ColrGlyphs", [
      fixture("color-fonts", "test_glyphs-glyf_colr_1_variable.ttf"),
    ]);
    FontLibrary.use("ColrV0", [
      fixture("colr-v0", "COLR-palettes-test-font.ttf"),
    ]);
    registered = true;
  };
})();

/** A reference's layout, from the `.json` beside its image. */
const layout = (name) =>
  JSON.parse(fs.readFileSync(path.join(REFERENCES, `${name}.json`), "utf8"));

/** A reference's pixels, read as RGBA. */
const reference = async (spec) => {
  const image = await loadImage(path.join(REFERENCES, spec.reference)),
    canvas = new Canvas(spec.width, spec.height),
    ctx = canvas.getContext("2d");
  ctx.drawImage(image, 0, 0);
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

/**
 * The grid of test glyphs as the reference `spec` lays it out, drawn here,
 * moved across by `shift` pixels.
 */
const glyphSheet = (spec, shift = 0) => {
  const canvas = new Canvas(spec.width, spec.height),
    ctx = canvas.getContext("2d");
  ctx.fillStyle = spec.background;
  ctx.fillRect(0, 0, spec.width, spec.height);
  ctx.font = `${spec.size}px ColrGlyphs`;
  if (spec.fontVariationSettings)
    ctx.fontVariationSettings = spec.fontVariationSettings;
  ctx.fillStyle = spec.fillStyle;
  spec.codepoints.forEach((hex, i) =>
    ctx.fillText(
      String.fromCodePoint(parseInt(hex, 16)),
      (i % spec.columns) * spec.cell + spec.x + shift,
      Math.floor(i / spec.columns) * spec.cell + spec.baseline,
    ),
  );
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

/** The COLR version 0 cases as the reference `spec` lays them out. */
const v0Sheet = (spec) => {
  const canvas = new Canvas(spec.width, spec.height),
    ctx = canvas.getContext("2d");
  ctx.fillStyle = spec.background;
  ctx.fillRect(0, 0, spec.width, spec.height);
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
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

/**
 * `ours` against `chrome`, both `width` wide: the largest difference among
 * interior pixels -- those whose 3x3 neighbourhood is flat in both images to
 * within 2 levels -- and how many pixels stand further than
 * [`EDGE_ENVELOPE`] from Chrome's anywhere, leaving out the cells `skip`
 * names by index in a grid of `cell`-sized squares `columns` wide.
 */
const compare = (ours, chrome, width, height, skip = () => false) => {
  const flat = (px, x, y) => {
    const at = (y * width + x) * 4;
    for (let dy = -1; dy <= 1; dy++)
      for (let dx = -1; dx <= 1; dx++) {
        const k = ((y + dy) * width + x + dx) * 4;
        for (let c = 0; c < 3; c++)
          if (Math.abs(px[k + c] - px[at + c]) > 2) return false;
      }
    return true;
  };
  let interior = 0,
    pastEnvelope = 0;
  for (let y = 1; y < height - 1; y++)
    for (let x = 1; x < width - 1; x++) {
      if (skip(x, y)) continue;
      const k = (y * width + x) * 4;
      const d = Math.max(
        Math.abs(ours[k] - chrome[k]),
        Math.abs(ours[k + 1] - chrome[k + 1]),
        Math.abs(ours[k + 2] - chrome[k + 2]),
      );
      if (d > EDGE_ENVELOPE) pastEnvelope++;
      if (flat(ours, x, y) && flat(chrome, x, y))
        interior = Math.max(interior, d);
    }
  return { interior, pastEnvelope };
};

/** How many pixels of `px` hold a colour rather than a grey. */
const coloured = (px) => {
  let n = 0;
  for (let i = 0; i < px.length; i += 4) {
    const spread = Math.max(
      Math.abs(px[i] - px[i + 1]),
      Math.abs(px[i + 1] - px[i + 2]),
    );
    if (px[i + 3] && spread > 25) n++;
  }
  return n;
};

/**
 * The cell a test glyph sheet puts glyph 0 in: U+FE000, which the font maps
 * to `.notdef`, and which Chrome draws as nothing at all. That is the missing
 * glyph, not a colour one, and is left out here.
 */
const notdefCell = (spec) => {
  const index = spec.codepoints.indexOf("fe000");
  return (x, y) =>
    Math.floor(y / spec.cell) * spec.columns + Math.floor(x / spec.cell) ===
    index;
};

describe("COLR colour fonts", () => {
  test("a COLR version 0 font inks in its palette's colours", () => {
    fonts();
    const canvas = new Canvas(160, 80),
      ctx = canvas.getContext("2d");
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, 160, 80);
    ctx.font = "48px ColrV0";
    ctx.fillStyle = "#000";
    ctx.fillText("A", 20, 60);
    // Chrome 158 inks 1,929 pixels of this, every one of them coloured.
    assert.ok(
      coloured(ctx.getImageData(0, 0, 160, 80).data) > 1500,
      "the COLRv0 glyph inks in colour",
    );
  });

  test(
    "COLRv1 glyphs draw as Chrome draws them",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      fonts();
      const spec = layout("chrome-test-glyphs-48px");
      const { interior, pastEnvelope } = compare(
        glyphSheet(spec),
        await reference(spec),
        spec.width,
        spec.height,
        notdefCell(spec),
      );
      assert.ok(
        interior <= INTERIOR_TOLERANCE,
        `interior pixels stand ${interior} from Chrome's`,
      );
      // 33 measured: pixels on the hard stops and wraps of repeated
      // gradients, and edges -- see docs/browser-differences.md.
      assert.ok(pastEnvelope <= 40, `${pastEnvelope} edge pixels past 81`);
    },
  );

  test(
    "COLRv1 glyphs with their axes varied draw as Chrome draws them",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      fonts();
      const spec = layout("chrome-test-glyphs-48px-varied");
      const { interior, pastEnvelope } = compare(
        glyphSheet(spec),
        await reference(spec),
        spec.width,
        spec.height,
        notdefCell(spec),
      );
      assert.ok(
        interior <= INTERIOR_TOLERANCE,
        `interior pixels stand ${interior} from Chrome's`,
      );
      // 53 measured.
      assert.ok(pastEnvelope <= 64, `${pastEnvelope} edge pixels past 81`);
    },
  );

  test(
    "COLR version 0 glyphs draw as Chrome draws them",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      fonts();
      const spec = layout("chrome-colr-v0");
      const { interior, pastEnvelope } = compare(
        v0Sheet(spec),
        await reference(spec),
        spec.width,
        spec.height,
      );
      assert.ok(
        interior <= INTERIOR_TOLERANCE,
        `interior pixels stand ${interior} from Chrome's`,
      );
      assert.equal(pastEnvelope, 0);
    },
  );

  test(
    "the comparison refuses glyphs drawn a pixel off Chrome's",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      // The control for the two above: a sheet moved a pixel across has
      // every glyph's edges past the envelope, which the tolerance must not
      // pass.
      fonts();
      const spec = layout("chrome-test-glyphs-48px");
      const { pastEnvelope } = compare(
        glyphSheet(spec, 1),
        await reference(spec),
        spec.width,
        spec.height,
        notdefCell(spec),
      );
      assert.ok(pastEnvelope > 40, `only ${pastEnvelope} past the envelope`);
    },
  );

  test(
    "strokeText draws a colour glyph filled, in the stroke's colour",
    { skip: CHROME_MEASURED_HERE },
    () => {
      // As Chrome draws one: the glyph's paint graph fills it, and the
      // stroke style is the colour its foreground paints take.
      fonts();
      const draw = (stroke) => {
        const canvas = new Canvas(100, 100),
          ctx = canvas.getContext("2d");
        ctx.font = "72px ColrGlyphs";
        ctx.lineWidth = 3;
        if (stroke) {
          ctx.strokeStyle = "green";
          ctx.strokeText("\u{f0b06}", 10, 85);
        } else {
          ctx.fillStyle = "green";
          ctx.fillText("\u{f0b06}", 10, 85);
        }
        return ctx.getImageData(0, 0, 100, 100).data;
      };
      assert.deepEqual(draw(true), draw(false));
    },
  );

  test(
    "an SVG export embeds a colour glyph as pixels",
    { skip: CHROME_MEASURED_HERE },
    () => {
      // A colour glyph is composited through a layer of its own, which an
      // SVG has no way to write: without asking for it, the export wrote
      // nothing of the glyph at all.
      fonts();
      const canvas = new Canvas(120, 100),
        ctx = canvas.getContext("2d");
      ctx.font = "72px ColrGlyphs";
      ctx.fillText("\u{f0200}", 10, 85);
      const svg = canvas.toBufferSync("svg").toString();
      assert.match(svg, /<image/);
    },
  );
});
