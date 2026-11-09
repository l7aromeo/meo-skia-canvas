// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary, loadImage } = require("../../lib"),
  fs = require("fs"),
  path = require("path");

const REFERENCES = path.join(__dirname, "..", "assets", "images", "colr");

/**
 * Where this library draws a COLR glyph from its paint graph as Chrome draws
 * it through Fontations: macOS and Windows. Elsewhere FreeType draws the
 * glyph, with its own anti-aliasing and placement.
 */
// TEMPORARY: MEO_COLR_OWN_ROUTE runs the comparisons with the own painter
// on another platform. Dropped before merge.
const CHROME_MEASURED_HERE =
  process.platform === "darwin" ||
  process.platform === "win32" ||
  process.env.MEO_COLR_OWN_ROUTE
    ? false
    : "Chrome 158's COLR glyphs are compared where this library draws them with its own painter";

/**
 * How far a channel of a colour glyph may stand from Chrome's at any pixel,
 * edges included. Measured at 3, at one pixel of the test glyphs and with
 * their axes varied alike, and past 1 at ten: both draw the same paint graph
 * through Skia's CPU raster.
 */
const COLOUR_GLYPH_TOLERANCE = 3;

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

/**
 * A canvas drawing on the CPU, as Chrome drew the references -- see
 * `tests/assets/images/colr/README.md`.
 */
const cpuCanvas = (width, height) => {
  const canvas = new Canvas(width, height);
  canvas.gpu = false;
  return canvas;
};

/** A reference's pixels, read as RGBA. */
const reference = async (spec) => {
  const image = await loadImage(path.join(REFERENCES, spec.reference)),
    canvas = cpuCanvas(spec.width, spec.height),
    ctx = canvas.getContext("2d");
  ctx.drawImage(image, 0, 0);
  return ctx.getImageData(0, 0, spec.width, spec.height).data;
};

/**
 * The grid of test glyphs as the reference `spec` lays it out, drawn here,
 * moved across by `shift` pixels.
 */
const glyphSheet = (spec, shift = 0) => {
  const canvas = cpuCanvas(spec.width, spec.height),
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
  const canvas = cpuCanvas(spec.width, spec.height),
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
 * The largest difference between `ours` and `chrome`, both `width` wide, in
 * any channel of any pixel `skip` does not name.
 */
const largestDifference = (ours, chrome, width, skip = () => false) => {
  let largest = 0;
  for (let k = 0; k < ours.length; k++) {
    const pixel = k >> 2;
    if (skip(pixel % width, Math.floor(pixel / width))) continue;
    largest = Math.max(largest, Math.abs(ours[k] - chrome[k]));
  }
  return largest;
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
 * The cells a test glyph sheet puts the glyphs with no colour paint in -- the
 * code points its layout lists under `withoutColourPaint`, `.notdef` among
 * them -- which the platform draws rather than this library's painter.
 */
const plainCells = (spec) => {
  const plain = new Set(
    spec.withoutColourPaint.map((hex) => spec.codepoints.indexOf(hex)),
  );
  return (x, y) =>
    plain.has(
      Math.floor(y / spec.cell) * spec.columns + Math.floor(x / spec.cell),
    );
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
      const largest = largestDifference(
        glyphSheet(spec),
        await reference(spec),
        spec.width,
        plainCells(spec),
      );
      assert.ok(
        largest <= COLOUR_GLYPH_TOLERANCE,
        `a colour glyph stands ${largest} from Chrome's`,
      );
    },
  );

  test(
    "COLRv1 glyphs with their axes varied draw as Chrome draws them",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      fonts();
      const spec = layout("chrome-test-glyphs-48px-varied");
      const largest = largestDifference(
        glyphSheet(spec),
        await reference(spec),
        spec.width,
        plainCells(spec),
      );
      assert.ok(
        largest <= COLOUR_GLYPH_TOLERANCE,
        `a colour glyph stands ${largest} from Chrome's`,
      );
    },
  );

  test(
    "COLR version 0 glyphs draw as Chrome draws them",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      fonts();
      const spec = layout("chrome-colr-v0");
      // Byte for byte, at every size.
      assert.equal(
        largestDifference(v0Sheet(spec), await reference(spec), spec.width),
        0,
      );
    },
  );

  test(
    "the comparison refuses glyphs drawn a pixel off Chrome's",
    { skip: CHROME_MEASURED_HERE },
    async () => {
      // The control for the comparisons above: a sheet moved a pixel across
      // stands further from Chrome's than the tolerance passes.
      fonts();
      const spec = layout("chrome-test-glyphs-48px");
      const largest = largestDifference(
        glyphSheet(spec, 1),
        await reference(spec),
        spec.width,
        plainCells(spec),
      );
      assert.ok(
        largest > COLOUR_GLYPH_TOLERANCE,
        `a sheet a pixel off stands only ${largest} from Chrome's`,
      );
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
