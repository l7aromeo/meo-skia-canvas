// @ts-check

"use strict";

const { assert, describe, test, beforeEach } = require("../runner"),
  { Canvas, FontLibrary, ParagraphBuilder } = require("../../lib");

// A paragraph is painted as a canvas paints a background box and then a run
// of text: its backgrounds, each text shadow and then its glyphs and
// decorations, each composited through the context's alpha and operator.
// Composited as one group, a paragraph's text was differenced against its
// own background under `difference` -- 140 levels from the backgrounds and
// the text composited apart.
describe("A paragraph under an alpha or an operator", () => {
  const OSWALD = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
  const W = 320,
    H = 120,
    AT = [10, 20];
  const OPERATORS = [
    "source-in",
    "source-out",
    "destination-in",
    "destination-out",
    "destination-atop",
    "copy",
    "source-atop",
    "destination-over",
    "lighter",
    "xor",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color-dodge",
    "color-burn",
    "hard-light",
    "soft-light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
  ];
  const TEXT = "rgba(40,90,160,0.8)",
    BACKGROUND = "rgba(240,240,250,0.7)",
    TRANSPARENT = "rgba(0,0,0,0)",
    // A style naming no background at all.
    NONE = null;
  // Two shadows, the first listed lying on top, as CSS lists them.
  const SHADOWS = [
    { color: "rgba(200,30,60,0.6)", offset: [3, 2], blurRadius: 0 },
    { color: "rgba(30,60,200,0.6)", offset: [6, 5], blurRadius: 0 },
  ];
  // How far a page may stand from the model: none at full alpha, where
  // the model composites the same parts, each drawn alone into an
  // eight-bit layer, as the library does. Two levels at a `globalAlpha`:
  // the library composites the alpha exactly where `drawImage` holds it
  // on a byte, so an alpha may land a level apart, which moves a byte by up
  // to a level under these operators, and each side rounds once more. Off
  // arm64, two more: Skia's eight-bit pipeline, which composites the
  // model's parts, divides by 255 as `(v + 255) / 256`, a level high at
  // worst, rather than exactly as on NEON (`div255` in
  // SkRasterPipeline_opts.h), and the parts stack four deep.
  const bound = (alpha) => (alpha == 1 ? 0 : process.arch == "arm64" ? 2 : 4);

  beforeEach(() => FontLibrary.use("ParagraphBlendOswald", [OSWALD]));

  /** A paragraph of `text` in `color` over `backgroundColor`. */
  const paragraph = ({
    color = TEXT,
    backgroundColor = BACKGROUND,
    shadows = /** @type {any[]} */ ([]),
  } = {}) => {
    const builder = ParagraphBuilder.Make({
      textStyle: {
        fontSize: 48,
        fontFamilies: ["ParagraphBlendOswald"],
        color,
        ...(backgroundColor == NONE ? {} : { backgroundColor }),
        letterSpacing: -12,
        shadows,
      },
    });
    builder.addText("Hello Wq");
    const built = builder.build();
    built.layout(W);
    return built;
  };
  const canvas = () => {
    const c = new Canvas(W, H);
    c.gpu = false;
    return c;
  };
  const read = (/** @type {Canvas} */ c) =>
    c.getContext("2d").getImageData(0, 0, W, H, { alphaType: "premultiplied" })
      .data;
  const page = (op, alpha) => {
    const c = canvas(),
      ctx = c.getContext("2d");
    ctx.fillStyle = "rgb(220,180,60)";
    ctx.fillRect(0, 0, W / 2, H);
    ctx.fillStyle = "rgb(30,120,90)";
    ctx.fillRect(W / 2, 0, W / 2, H);
    ctx.globalCompositeOperation = op;
    ctx.globalAlpha = alpha;
    return { c, ctx };
  };
  /** A part of a paragraph alone, `source-over` on transparent black. */
  const part = (built, [dx, dy] = [0, 0]) => {
    const c = canvas();
    c.getContext("2d").drawParagraph(built, AT[0] + dx, AT[1] + dy);
    return c;
  };
  const drawn = (op, alpha, shadows, backgroundColor = BACKGROUND) => {
    const { c, ctx } = page(op, alpha);
    ctx.drawParagraph(paragraph({ shadows, backgroundColor }), ...AT);
    return read(c);
  };
  /**
   * The backgrounds, then each shadow -- the glyphs alone in its colour,
   * at its offset, the last listed first -- then the text, each composited
   * apart. A paragraph naming no background composites nothing beneath
   * its shadows.
   */
  const painted = (op, alpha, shadows, backgroundColor = BACKGROUND) => {
    const { c, ctx } = page(op, alpha);
    if (backgroundColor != NONE)
      ctx.drawImage(
        part(paragraph({ color: TRANSPARENT, backgroundColor })),
        0,
        0,
      );
    for (const shadow of [...shadows].reverse())
      ctx.drawImage(
        part(
          paragraph({ color: shadow.color, backgroundColor: TRANSPARENT }),
          shadow.offset,
        ),
        0,
        0,
      );
    ctx.drawImage(part(paragraph({ backgroundColor: TRANSPARENT })), 0, 0);
    return read(c);
  };
  /** The whole paragraph drawn alone and composited once. */
  const grouped = (op, alpha, shadows) => {
    const { c, ctx } = page(op, alpha);
    ctx.drawImage(part(paragraph({ shadows })), 0, 0);
    return read(c);
  };
  const most = (a, b) => {
    let m = 0;
    for (let i = 0; i < a.length; i++) m = Math.max(m, Math.abs(a[i] - b[i]));
    return m;
  };

  test("its parts are composited one after another", () => {
    const missed = [];
    for (const shadows of [[], SHADOWS])
      for (const alpha of [1, 0.6])
        for (const op of OPERATORS) {
          const apart = most(
            drawn(op, alpha, shadows),
            painted(op, alpha, shadows),
          );
          if (apart > bound(alpha))
            missed.push(
              `${op} at ${alpha}, ${shadows.length} shadows: ${apart}`,
            );
        }
    assert.deepEqual(missed, []);
  });

  test("with no background, nothing is composited beneath its shadows", () => {
    // Composited, an empty part is transparent black over the whole page,
    // which an operator reaching beyond what it draws erases the page with.
    const missed = [];
    for (const alpha of [1, 0.6])
      for (const op of OPERATORS) {
        const apart = most(
          drawn(op, alpha, SHADOWS, NONE),
          painted(op, alpha, SHADOWS, NONE),
        );
        if (apart > bound(alpha)) missed.push(`${op} at ${alpha}: ${apart}`);
      }
    assert.deepEqual(missed, []);
  });

  test("a transparent background is no background", () => {
    // A background that paints nothing is none: composited as a part, it
    // was transparent black over the whole page, which `source-in` and the
    // other operators reaching beyond it erased the page with.
    const missed = [];
    for (const alpha of [1, 0.6])
      for (const op of OPERATORS) {
        const apart = most(
          drawn(op, alpha, SHADOWS, TRANSPARENT),
          painted(op, alpha, SHADOWS, NONE),
        );
        if (apart > bound(alpha)) missed.push(`${op} at ${alpha}: ${apart}`);
      }
    assert.deepEqual(missed, []);
  });

  test("composited apart, its parts differ from one group", () => {
    // The control on the scene: the backgrounds lie under the text, so
    // compositing them apart is not compositing the paragraph whole.
    for (const op of ["difference", "source-in", "xor"])
      assert.ok(
        most(grouped(op, 1, []), painted(op, 1, [])) > 100,
        `${op}: one group and the parts apart are one page`,
      );
  });

  test("under source-over at full alpha it is drawn as it was", () => {
    // Nothing to composite, so nothing changes: the parts are drawn
    // straight onto the page, one after another, as each would be drawn on
    // its own.
    for (const shadows of [[], SHADOWS]) {
      const { c, ctx } = page("source-over", 1);
      ctx.drawParagraph(paragraph({ color: TRANSPARENT }), ...AT);
      for (const shadow of [...shadows].reverse())
        ctx.drawParagraph(
          paragraph({ color: shadow.color, backgroundColor: TRANSPARENT }),
          AT[0] + shadow.offset[0],
          AT[1] + shadow.offset[1],
        );
      ctx.drawParagraph(paragraph({ backgroundColor: TRANSPARENT }), ...AT);
      assert.equal(
        most(drawn("source-over", 1, shadows), read(c)),
        0,
        `${shadows.length} shadows`,
      );
    }
  });

  test("under source-over at full alpha an SVG keeps it as vectors", () => {
    // An SVG writes a part composited through an operator as pixels, and
    // keeps the parts as vectors where nothing needs compositing. Under an
    // operator, the control that the export can tell the two apart.
    const svg = (op) => {
      const c = new Canvas(W, H),
        ctx = c.getContext("2d");
      ctx.globalCompositeOperation = op;
      ctx.drawParagraph(paragraph({ shadows: SHADOWS }), ...AT);
      return c.toBufferSync("svg").toString("utf-8");
    };
    assert.doesNotMatch(svg("source-over"), /<image/);
    assert.match(svg("difference"), /<image/);
  });
});
