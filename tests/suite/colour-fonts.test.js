// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary, loadImage } = require("../../lib"),
  fs = require("fs"),
  path = require("path");

const REFERENCES = path.join(__dirname, "..", "assets", "images", "colr");

/**
 * How far a channel of a colour glyph may stand from Chrome's at any pixel,
 * edges included. Measured at 3, at one pixel of the test glyphs and with
 * their axes varied alike, and past 1 at ten: both draw the same paint graph
 * through Skia's CPU raster.
 */
const COLOUR_GLYPH_TOLERANCE = 3;

/**
 * The family a character the test fonts lack falls back to in the Chrome
 * comparisons, named so that both engines take the same one: a list nothing
 * answers falls back to Chrome's standard font, Times, and to this
 * library's `sans-serif` -- see docs/browser-differences.md.
 */
const FALLBACK = "serif";

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
 * `text`, each of whose characters other than spaces is a colour glyph of
 * the COLRv1 test font: a code point the 48px reference layout lists and
 * not under its `withoutColourPaint`. A character the font lacks falls back
 * to a platform face and is drawn as plain text, so a scene meaning to draw
 * colour glyphs would draw something else and pass for it.
 *
 * @param {string} text
 */
const colourText = (text) => {
  const spec = layout("chrome-test-glyphs-48px");
  for (const c of text) {
    if (/\s/.test(c)) continue;
    const hex = /** @type {number} */ (c.codePointAt(0)).toString(16);
    assert.ok(
      spec.codepoints.includes(hex) && !spec.withoutColourPaint.includes(hex),
      `U+${hex} is no colour glyph of the test font`,
    );
  }
  return text;
};

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
  ctx.font = `${spec.size}px ColrGlyphs, ${FALLBACK}`;
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
    ctx.font = `${size}px ColrV0, ${FALLBACK}`;
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

  test("COLRv1 glyphs draw as Chrome draws them", async () => {
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
  });

  test("COLRv1 glyphs with their axes varied draw as Chrome draws them", async () => {
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
  });

  test("COLR version 0 glyphs draw as Chrome draws them", async () => {
    fonts();
    const spec = layout("chrome-colr-v0");
    // Byte for byte, at every size.
    assert.equal(
      largestDifference(v0Sheet(spec), await reference(spec), spec.width),
      0,
    );
  });

  test("the comparison refuses glyphs drawn a pixel off Chrome's", async () => {
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
  });

  test("a GPU canvas draws colour glyphs as the CPU canvas does", (t) => {
    // Each colour glyph is an image made on the CPU, which the GPU canvas
    // composites as the CPU canvas does: byte for byte where no glyph
    // lands on another, so the cells are spread to twice the layout's.
    fonts();
    const probe = new Canvas(1, 1);
    if (probe.engine.renderer != "GPU") return t.skip("no GPU here");
    const spec = layout("chrome-test-glyphs-48px"),
      spread = { ...spec, cell: spec.cell * 2, x: spec.x + spec.cell / 2 };
    spread.baseline = spec.baseline + spec.cell / 2;
    spread.width = spec.columns * spread.cell;
    spread.height =
      Math.ceil(spec.codepoints.length / spec.columns) * spread.cell;
    const sheet = (gpu) => {
      const canvas = new Canvas(spread.width, spread.height);
      canvas.gpu = gpu;
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = spread.background;
      ctx.fillRect(0, 0, spread.width, spread.height);
      ctx.font = `${spread.size}px ColrGlyphs`;
      ctx.fillStyle = spread.fillStyle;
      spread.codepoints.forEach((hex, i) =>
        ctx.fillText(
          String.fromCodePoint(parseInt(hex, 16)),
          (i % spread.columns) * spread.cell + spread.x,
          Math.floor(i / spread.columns) * spread.cell + spread.baseline,
        ),
      );
      return ctx.getImageData(0, 0, spread.width, spread.height).data;
    };
    assert.equal(
      largestDifference(
        sheet(true),
        sheet(false),
        spread.width,
        plainCells(spread),
      ),
      0,
    );
  });

  test("strokeText draws a colour glyph filled, in the stroke's colour", () => {
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
        ctx.strokeText(colourText("\u{f0b06}"), 10, 85);
      } else {
        ctx.fillStyle = "green";
        ctx.fillText(colourText("\u{f0b06}"), 10, 85);
      }
      return ctx.getImageData(0, 0, 100, 100).data;
    };
    assert.deepEqual(draw(true), draw(false));
  });

  test("an SVG export embeds a colour glyph as pixels", () => {
    // A colour glyph is composited through a layer of its own, which an
    // SVG has no way to write: without asking for it, the export wrote
    // nothing of the glyph at all.
    fonts();
    const canvas = new Canvas(120, 100),
      ctx = canvas.getContext("2d");
    ctx.font = "72px ColrGlyphs";
    ctx.fillText(colourText("\u{f0200}"), 10, 85);
    const svg = canvas.toBufferSync("svg").toString();
    assert.match(svg, /<image/);
  });
});

describe("colour fonts the platform draws", () => {
  /**
   * Skia's own bitmap and SVG colour fonts, each with the text Skia draws in
   * it (`tools/fonts/FontToolUtils.cpp`, `EmojiSample`), registered where the
   * platform can load them: CoreText refuses `cbdt.ttf`, which has no
   * outlines.
   */
  const faces = (() => {
    /** @type {[string, string][] | undefined} */
    let loaded;
    return () => {
      loaded ??= [
        ["PlatformColourSbix", "sbix.ttf", "\u{1F600} \u{2662}"],
        ["PlatformColourCbdt", "cbdt.ttf", "\u{1F600} \u{2662}"],
        ["PlatformColourSvg", "SampleSVG.ttf", "abcdefghij"],
      ].flatMap(([family, file, text]) => {
        try {
          FontLibrary.use(family, [
            path.join(
              __dirname,
              "..",
              "assets",
              "fonts",
              "bitmap-colour",
              file,
            ),
          ]);
          return [[family, text]];
        } catch {
          return [];
        }
      });
      return loaded;
    };
  })();

  /** `text` in `family`, filled with `fill` through `filter` on a CPU canvas. */
  const drawn = (family, text, fill, filter) => {
    const canvas = new Canvas(480, 80);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.font = `48px ${family}`;
    ctx.fillStyle = fill;
    ctx.filter = filter;
    ctx.fillText(text, 10, 58);
    return ctx.getImageData(0, 0, 480, 80).data;
  };

  const FILLS = ["rgb(230, 20, 10)", "rgb(10, 60, 240)"];

  /** Whether two reads hold the same bytes. */
  const same = (a, b) =>
    Buffer.from(a.buffer, a.byteOffset, a.byteLength).equals(
      Buffer.from(b.buffer, b.byteOffset, b.byteLength),
    );

  test("a colour glyph keeps its colours under a small blur", () => {
    // The CPU canvas blurs a run in one flat colour from its coverage and
    // paints the blur in that colour; a run of colour glyphs drawn that way
    // came out in the fill.
    const coloured = faces().filter(([family, text]) => {
      const [a, b] = FILLS.map((fill) => drawn(family, text, fill, "none"));
      return a.some((v, i) => i % 4 === 3 && v !== 0) && same(a, b);
    });
    assert.ok(coloured.length > 0, "no colour face to test");
    for (const [family, text] of coloured)
      for (const filter of ["blur(0.5px)", "blur(2px)"]) {
        const [a, b] = FILLS.map((fill) => drawn(family, text, fill, filter));
        assert.ok(
          same(a, b),
          `${family} under ${filter} took its colour from the fill`,
        );
      }
  });
});
