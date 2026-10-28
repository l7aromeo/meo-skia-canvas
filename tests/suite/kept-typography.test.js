// @ts-check

"use strict";

// A context keeps the typography it builds from its text settings and
// shares it between draws until one of them changes. Each case here draws
// once, so that there is a kept build, changes one setting, and draws and
// measures again: what comes out has to be what a context given that setting
// from the start draws, and has to differ from the first draw, so that the
// setting is shown to matter rather than assumed to.

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary } = require("../../lib");

const FONTS = `${__dirname}/../assets/fonts`;
const OSWALD = `${FONTS}/Oswald/Oswald-VariableFont_wght.ttf`;
const AMSTELVAR = `${FONTS}/AmstelvarAlpha-VF.ttf`;
// Has small capitals, which nothing here synthesises for a face without.
const RALEWAY = `${FONTS}/Raleway/Raleway-VariableFont_wght.ttf`;
FontLibrary.use("KeptOswald", [OSWALD]);
FontLibrary.use("KeptAmstelvar", [AMSTELVAR]);
FontLibrary.use("KeptRaleway", [RALEWAY]);

const TEXT = "Hamburgefonstiv fi AV";

/**
 * What `ctx` draws and measures `TEXT` as now.
 * @param {CanvasRenderingContext2D} ctx
 */
const typeset = (ctx) => {
  ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  ctx.fillText(TEXT, 20, 60, 180);
  const pixels = Buffer.from(
    ctx.getImageData(0, 0, ctx.canvas.width, ctx.canvas.height).data.buffer,
  ).toString("base64");
  return { pixels, measured: JSON.stringify(ctx.measureText(TEXT)) };
};

/** @param {(ctx: CanvasRenderingContext2D) => void} [setup] */
const context = (setup) => {
  const ctx = new Canvas(400, 160).getContext("2d");
  ctx.font = "24px KeptOswald";
  setup?.(ctx);
  return ctx;
};

/**
 * @typedef {(ctx: CanvasRenderingContext2D) => void} Apply
 * @type {[string, Apply, Apply?][]}
 */
const SETTERS = [
  ["font", (ctx) => (ctx.font = "30px KeptOswald")],
  [
    "font with a line height",
    (ctx) => (ctx.font = "24px/60px KeptOswald"),
    (ctx) => (ctx.textWrap = true),
  ],
  ["fontVariant", (ctx) => (ctx.fontVariant = "no-common-ligatures")],
  [
    "fontVariant small-caps",
    (ctx) => (ctx.fontVariant = "small-caps"),
    (ctx) => (ctx.font = "24px KeptRaleway"),
  ],
  [
    "fontVariantCaps",
    (ctx) => (ctx.fontVariantCaps = "all-small-caps"),
    (ctx) => (ctx.font = "24px KeptRaleway"),
  ],
  [
    "fontStretch",
    (ctx) => (ctx.fontStretch = "condensed"),
    (ctx) => (ctx.font = "24px KeptAmstelvar"),
  ],
  // The variations reach the typography through a spacing in `ch`, resolved
  // against the face instanced at them.
  [
    "fontVariationSettings",
    (ctx) => (ctx.fontVariationSettings = '"wght" 700'),
    (ctx) => (ctx.letterSpacing = "0.25ch"),
  ],
  ["letterSpacing", (ctx) => (ctx.letterSpacing = "3px")],
  ["wordSpacing", (ctx) => (ctx.wordSpacing = "12px")],
  ["textBaseline", (ctx) => (ctx.textBaseline = "top")],
  ["textAlign", (ctx) => (ctx.textAlign = "center")],
  ["direction", (ctx) => (ctx.direction = "rtl")],
  ["textWrap", (ctx) => (ctx.textWrap = true)],
  ["fontKerning", (ctx) => (ctx.fontKerning = "none")],
  ["textRendering", (ctx) => (ctx.textRendering = "optimizeSpeed")],
  [
    "textRendering geometricPrecision",
    (ctx) => (ctx.textRendering = "geometricPrecision"),
  ],
  ["fontHinting", (ctx) => (ctx.fontHinting = false)],
  ["textDecoration", (ctx) => (ctx.textDecoration = "underline")],
];

describe("A context's kept typography follows every setting it is built from", () => {
  for (const [name, apply, setup] of SETTERS) {
    test(name, () => {
      const kept = context(setup);
      const before = typeset(kept);
      apply(kept);
      const after = typeset(kept);

      const fresh = context(setup);
      apply(fresh);
      const wanted = typeset(fresh);

      assert.notDeepEqual(after, before, `${name} changes what is drawn`);
      assert.equal(
        after.measured,
        wanted.measured,
        `${name}: measured as a fresh context does`,
      );
      assert.ok(
        after.pixels === wanted.pixels,
        `${name}: drawn as a fresh context draws`,
      );
    });
  }

  test("a font registered under the family moves a spacing in ch", () => {
    // Nothing on the context changes: the face `1ch` resolves against does.
    const family = "KeptSwapped";
    FontLibrary.use(family, [OSWALD]);
    const setup = (/** @type {CanvasRenderingContext2D} */ ctx) => {
      ctx.font = `24px ${family}`;
      ctx.letterSpacing = "1ch";
    };
    const kept = context(setup);
    const before = typeset(kept);
    FontLibrary.use(family, [AMSTELVAR]);
    const after = typeset(kept);
    const wanted = typeset(context(setup));
    const width = (/** @type {{measured: string}} */ shot) =>
      JSON.parse(shot.measured).width;

    assert.notEqual(
      width(after),
      width(before),
      "the registration moves the spacing",
    );
    assert.equal(
      after.measured,
      wanted.measured,
      "measured as a fresh context does",
    );
    assert.ok(after.pixels === wanted.pixels, "drawn as a fresh context draws");
  });

  test("a font registered under the family moves the baselines", () => {
    // Oswald hangs at 22.91 and sits its ideographic baseline at -6.94 at
    // 24px; Amstelvar at 18.24 and -6.00. A context whose font was set before
    // the registration has to read the new face's, and to draw at the
    // baseline the new face puts `top` at.
    const family = "KeptBaselines";
    FontLibrary.use(family, [OSWALD]);
    const setup = (/** @type {CanvasRenderingContext2D} */ ctx) => {
      ctx.font = `24px ${family}`;
      ctx.textBaseline = "top";
    };
    const kept = context(setup);
    const before = typeset(kept);
    FontLibrary.use(family, [AMSTELVAR]);
    const after = typeset(kept);
    const wanted = typeset(context(setup));
    const read = (/** @type {{measured: string}} */ shot) => {
      const { hangingBaseline, ideographicBaseline, alphabeticBaseline } =
        JSON.parse(shot.measured);
      return { hangingBaseline, ideographicBaseline, alphabeticBaseline };
    };

    assert.notDeepEqual(
      read(wanted),
      read(before),
      "the faces' baselines differ",
    );
    assert.deepEqual(read(after), read(wanted), "the new face's baselines");
    assert.equal(
      after.measured,
      wanted.measured,
      "measured as a fresh context does",
    );
    assert.ok(after.pixels === wanted.pixels, "drawn at the new face's top");
  });

  test("restore brings back the typography that was saved", () => {
    const kept = context();
    const before = typeset(kept);
    kept.save();
    kept.font = "36px KeptAmstelvar";
    kept.letterSpacing = "4px";
    const changed = typeset(kept);
    kept.restore();
    const after = typeset(kept);

    assert.notDeepEqual(changed, before);
    assert.deepEqual(
      after,
      before,
      "restored, it draws as it did before the save",
    );
  });

  test("reset brings back the initial typography", () => {
    const kept = context();
    kept.letterSpacing = "4px";
    typeset(kept);
    kept.reset();
    kept.font = "24px KeptOswald";
    assert.deepEqual(typeset(kept), typeset(context()));
  });
});
