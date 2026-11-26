// @ts-check

"use strict";

// A file of its own, so that a failure here -- a shadowed run composited a
// glyph at a time, or with its shadow in one layer -- is reported as this
// file's: node runs each test file in a process of its own.

const { assert, describe, test, beforeEach } = require("../runner"),
  { Canvas, FontLibrary } = require("../../lib");

// The HTML Standard paints the shapes `fillText` and `strokeText` prepare
// once, through the drawing model: the shadow cast from the painted image,
// composited with the operator, then the image itself, multiplied by
// `globalAlpha` and composited with the operator. Under `source-over` that
// is the run drawn glyph by glyph after its shadow, to within rounding, and
// the run is drawn so. Under every other operator a run casting a shadow is
// held to the model; a run casting none stays as it was drawn before.
describe("A run casting a shadow under an operator other than source-over", () => {
  const OSWALD = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
  // A face with a COLR table: its "A" a colour glyph, its lowercase letters
  // plain boxes, so a run of it is drawn as a colour run.
  const PALETTES = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";
  const W = 320,
    H = 120,
    X = 10,
    Y = 90;
  // A shadow colour whose premultiplied channels are whole bytes at 0.6, the
  // alpha most scenes composite it at, so that the model's tint and the
  // library's land on the same byte away from a tie. At any other alpha the
  // model tints it at the alpha it is composited at, as the library places
  // it -- see `cast`.
  const SHADOW_RGB = "200,30,60",
    SHADOW_ALPHA = 0.6,
    OFFSET = [3, 2];
  // Operators that change the page outside what a draw covers, whose run
  // casting no shadow is composited as one image already.
  const BEYOND = [
    "source-in",
    "source-out",
    "destination-in",
    "destination-out",
    "destination-atop",
    "copy",
  ];
  // Every operator the Canvas standard names but `source-over`.
  const OPERATORS = [
    ...BEYOND,
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
  const RUNS = [
    { font: "48px ShadowBlendOswald", text: "Hello Wq", colour: false },
    { font: "40px ShadowBlendPalettes", text: "Abcdef", colour: true },
  ];
  /**
   * @typedef {{spacing: number, alpha: number, ink: number, stroke: boolean,
   *   blur: number}} Scene
   * @type {Scene[]}
   */
  const SCENES = [
    { spacing: -12, alpha: 1, ink: 1, stroke: false, blur: 0 },
    { spacing: -12, alpha: 0.6, ink: 1, stroke: false, blur: 0 },
    { spacing: -12, alpha: 1, ink: 0.8, stroke: false, blur: 0 },
    { spacing: -12, alpha: 1, ink: 1, stroke: true, blur: 0 },
    { spacing: -12, alpha: 1, ink: 1, stroke: false, blur: 4 },
    { spacing: 0, alpha: 1, ink: 1, stroke: false, blur: 0 },
  ];
  // How far a sharp shadow's page may stand from the model: a level, at a
  // tie. The model is built from the same eight-bit layers the library
  // composites.
  const SHARP_BOUND = 1;
  // A blurred shadow's model rounds the tint before the blur where the
  // library's chain rounds once after it: a level more.
  const BLURRED_BOUND = 2;
  // One level of a byte, which a model is moved by to see how far the
  // operator carries a level of rounding -- see `carried`.
  const LEVEL = 1 / 255;

  beforeEach(() => {
    FontLibrary.use("ShadowBlendOswald", [OSWALD]);
    FontLibrary.use("ShadowBlendPalettes", [PALETTES]);
  });

  /** A CPU canvas the size of every page here. */
  const canvas = (gpu = false) => {
    const c = new Canvas(W, H);
    c.gpu = gpu;
    return c;
  };
  /** @param {CanvasRenderingContext2D} ctx */
  const backdrop = (ctx) => {
    ctx.fillStyle = "rgb(220,180,60)";
    ctx.fillRect(0, 0, W / 2, H);
    ctx.fillStyle = "rgb(30,120,90)";
    ctx.fillRect(W / 2, 0, W / 2, H);
  };
  /**
   * @param {CanvasRenderingContext2D} ctx
   * @param {{font: string}} run
   * @param {Scene} scene
   */
  const style = (ctx, run, scene) => {
    ctx.font = run.font;
    ctx.fontKerning = "none";
    ctx.letterSpacing = `${scene.spacing}px`;
    ctx.fillStyle = ctx.strokeStyle = `rgba(40,90,160,${scene.ink})`;
    ctx.lineWidth = 3;
  };
  /**
   * @param {CanvasRenderingContext2D} ctx
   * @param {Scene} scene
   * @param {string} text
   * @param {number} x
   */
  const ink = (ctx, scene, text, x) =>
    scene.stroke ? ctx.strokeText(text, x, Y) : ctx.fillText(text, x, Y);
  /** The page's premultiplied bytes, which a translucent pixel keeps. */
  const read = (/** @type {Canvas} */ c) =>
    c.getContext("2d").getImageData(0, 0, W, H, { alphaType: "premultiplied" })
      .data;

  /** The page as the library draws the run on it. */
  const drawn = (run, op, scene, shadowed, gpu = false) => {
    const c = canvas(gpu),
      ctx = c.getContext("2d");
    backdrop(ctx);
    style(ctx, run, scene);
    ctx.globalAlpha = scene.alpha;
    ctx.globalCompositeOperation = op;
    if (shadowed) {
      ctx.shadowColor = `rgba(${SHADOW_RGB},${SHADOW_ALPHA})`;
      ctx.shadowBlur = scene.blur;
      [ctx.shadowOffsetX, ctx.shadowOffsetY] = OFFSET;
    }
    ink(ctx, scene, run.text, X);
    return { data: read(c), engine: c.engine };
  };
  /** The run alone, `source-over` on transparent black: image B. */
  const image = (run, scene) => {
    const c = canvas(),
      ctx = c.getContext("2d");
    style(ctx, run, scene);
    ink(ctx, scene, run.text, X);
    return c;
  };
  /**
   * The run's shadow, image C: cast from the run's coverage, drawn opaque,
   * in the shadow's colour at its alpha times the ink's -- a translucent
   * run's shadow is its coverage's, not its compounded alpha's.
   */
  const shadow = (run, scene, alpha = SHADOW_ALPHA) => {
    const sharp = canvas(),
      ctx = sharp.getContext("2d");
    ctx.drawImage(image(run, { ...scene, ink: 1 }), ...OFFSET);
    ctx.globalCompositeOperation = "source-in";
    ctx.fillStyle = `rgba(${SHADOW_RGB},${alpha * scene.ink})`;
    ctx.fillRect(0, 0, W, H);
    if (!scene.blur) return sharp;
    // shadowBlur is twice the blur's standard deviation.
    const blurred = canvas(),
      bctx = blurred.getContext("2d");
    bctx.filter = `blur(${scene.blur / 2}px)`;
    bctx.drawImage(sharp, 0, 0);
    return blurred;
  };
  /** Each glyph's x, from the advance of the text before it. */
  const positions = (run, scene) => {
    const ctx = canvas().getContext("2d");
    style(ctx, run, scene);
    return [...run.text].map(
      (_, i) => X + ctx.measureText(run.text.slice(0, i)).width,
    );
  };
  /** Draws the run one glyph at a time, each composited on its own. */
  const glyphByGlyph = (ctx, run, scene) => {
    const xs = positions(run, scene);
    [...run.text].forEach((glyph, i) => ink(ctx, scene, glyph, xs[i]));
  };
  /**
   * Composites the run's shadow onto `ctx`, under its `op`, tinted at the
   * alpha it is composited at -- `alpha` times the scene's -- as the library
   * places the colour it composites. Tinted at `alpha` and composited at the
   * scene's, the tint rounds to bytes at an alpha the library never
   * composites: at 0.6 under a `globalAlpha` of 0.6, the shadow's page
   * stood a level from the library's, and `color-burn` over this ink's red
   * carried that to 4 levels where the run lies over its shadow.
   */
  const cast = (ctx, run, scene, alpha) => {
    ctx.globalAlpha = 1;
    ctx.drawImage(shadow(run, scene, alpha * scene.alpha), 0, 0);
    ctx.globalAlpha = scene.alpha;
  };
  /** A page with the backdrop, under the scene's alpha and `op`. */
  const page = (op, scene) => {
    const c = canvas(),
      ctx = c.getContext("2d");
    backdrop(ctx);
    ctx.globalAlpha = scene.alpha;
    ctx.globalCompositeOperation = op;
    return { c, ctx };
  };
  /**
   * The Standard's painting: the shadow, then the run as one image. `alpha`
   * is the shadow's.
   */
  const oneImage = (run, op, scene, shadowed = true, alpha = SHADOW_ALPHA) => {
    const { c, ctx } = page(op, scene);
    if (shadowed) cast(ctx, run, scene, alpha);
    ctx.drawImage(image(run, scene), 0, 0);
    return read(c);
  };
  /** The shadow, then the run glyph by glyph. `alpha` is the shadow's. */
  const glyphs = (run, op, scene, shadowed = true, alpha = SHADOW_ALPHA) => {
    const { c, ctx } = page(op, scene);
    if (shadowed) cast(ctx, run, scene, alpha);
    style(ctx, run, scene);
    glyphByGlyph(ctx, run, scene);
    return read(c);
  };
  /** The most any byte stands apart, and how many stand more than `past`. */
  const apart = (a, b, past) => {
    let most = 0,
      over = 0;
    for (let i = 0; i < a.length; i++) {
      const d = Math.abs(a[i] - b[i]);
      most = Math.max(most, d);
      if (d > past) over++;
    }
    return { most, over };
  };
  /**
   * How far `op` carries a level of rounding through the painting `model`
   * draws, `(scene, alpha) => page`: the most a byte of its page moves when
   * the shadow's alpha moves by a level, and when the ink's does. The model
   * rounds the shadow's tint, and the run's image, to bytes before it
   * composites them, where the library composites each exact product once,
   * so each stands up to a level from the library's before the operator
   * takes it -- and an operator that divides by the run's colour carries
   * that level further: `color-burn` over this ink's red, 40 levels of
   * 255, moves six levels for one.
   */
  const carried = (model, scene) => {
    const at = model(scene, SHADOW_ALPHA);
    return Math.max(
      apart(at, model(scene, SHADOW_ALPHA + LEVEL), 0).most,
      apart(at, model({ ...scene, ink: scene.ink - LEVEL }, SHADOW_ALPHA), 0)
        .most,
    );
  };
  const named = (run, op, scene) =>
    `${run.text} ${op} ${JSON.stringify(scene)}`;

  test("the faces the scenes name are the ones drawn", () => {
    // A family not found falls back to the platform's face, which would
    // draw no colour run and other overlaps than the scenes mean.
    const advance = (font) => {
      const ctx = canvas().getContext("2d");
      ctx.font = font;
      return ctx.measureText("Ab").width;
    };
    for (const { font } of RUNS)
      assert.notEqual(
        advance(font),
        advance(font.replace(/\S+$/, "ShadowBlendNowhere")),
        font,
      );
  });

  test("glyphs placed one by one draw the run", () => {
    // The glyph-by-glyph model is only a model if its glyphs land where the
    // run's do: under `source-over` and no shadow the two are one page.
    for (const run of RUNS)
      for (const scene of SCENES) {
        const { c, ctx } = page("source-over", scene);
        style(ctx, run, scene);
        glyphByGlyph(ctx, run, scene);
        const { most } = apart(
          read(c),
          drawn(run, "source-over", scene, false).data,
          0,
        );
        assert.equal(most, 0, named(run, "source-over", scene));
      }
  });

  test("overlapping glyphs are what part one image from glyph by glyph", () => {
    // The control on the scenes: where glyphs do not overlap, the shadow
    // then one image and the shadow then glyph by glyph are one page under
    // an operator within the shape, and where they overlap they are not.
    // Without it, a page held to one image could pass by being equal to
    // both.
    const within = OPERATORS.filter((op) => !BEYOND.includes(op));
    for (const run of RUNS) {
      const flat = { ...SCENES[0], spacing: 0 };
      for (const op of within) {
        const { most } = apart(
          oneImage(run, op, flat),
          glyphs(run, op, flat),
          0,
        );
        const bound =
          SHARP_BOUND +
          carried((s, alpha) => oneImage(run, op, s, true, alpha), flat);
        assert.ok(most <= bound, `${named(run, op, flat)}: ${most}`);
      }
      for (const op of ["difference", "xor", "multiply"]) {
        const { most } = apart(
          oneImage(run, op, SCENES[0]),
          glyphs(run, op, SCENES[0]),
          0,
        );
        assert.ok(most > 40, `${named(run, op, SCENES[0])}: only ${most}`);
      }
    }
  });

  test("its shadow is composited, then the run as one image", () => {
    const missed = [];
    for (const run of RUNS)
      for (const scene of SCENES) {
        // A colour run's translucent ink is not a scene here: what its
        // palette's own colours take of the ink's alpha is another
        // question than the compositing this holds.
        if (run.colour && scene.ink < 1) continue;
        for (const op of OPERATORS) {
          const bound =
            (scene.blur ? BLURRED_BOUND : SHARP_BOUND) +
            carried((s, alpha) => oneImage(run, op, s, true, alpha), scene);
          const { most, over } = apart(
            drawn(run, op, scene, true).data,
            oneImage(run, op, scene),
            bound,
          );
          if (most > bound)
            missed.push(`${named(run, op, scene)}: ${most} on ${over} bytes`);
        }
      }
    assert.deepEqual(missed, []);
  });

  test("under source-over it is drawn after its shadow, glyph by glyph", () => {
    // Glyph by glyph is how Chrome draws a run, and under `source-over` it
    // stands from one image only by rounding, except under a `globalAlpha`,
    // which compounds where glyphs overlap.
    const missed = [];
    for (const run of RUNS)
      for (const scene of SCENES) {
        if (run.colour && scene.ink < 1) continue;
        const bound =
          (scene.blur ? BLURRED_BOUND : SHARP_BOUND) +
          carried(
            (s, alpha) => glyphs(run, "source-over", s, true, alpha),
            scene,
          );
        const { most, over } = apart(
          drawn(run, "source-over", scene, true).data,
          glyphs(run, "source-over", scene),
          bound,
        );
        if (most > bound)
          missed.push(
            `${named(run, "source-over", scene)}: ${most} on ${over} bytes`,
          );
      }
    assert.deepEqual(missed, []);
  });

  /**
   * The glyphs faded by `globalAlpha` inside one picture, which `op`
   * composites whole: Chrome's run casting no shadow under an operator
   * reaching beyond it.
   */
  const fadedInside = (run, op, scene) => {
    const inside = canvas(),
      ictx = inside.getContext("2d");
    style(ictx, run, scene);
    ictx.globalAlpha = scene.alpha;
    ink(ictx, scene, run.text, X);
    const { c, ctx } = page(op, { ...scene, alpha: 1 });
    ctx.drawImage(inside, 0, 0);
    return read(c);
  };

  test("casting no shadow, it is drawn as it was", () => {
    // Glyph by glyph under an operator within the shape, as every browser
    // measured draws it, `destination-out` among them, which erases only
    // where a glyph lands. Beyond it, as one picture of the glyphs faded by
    // `globalAlpha`, as Chrome draws it, where glyph by glyph would leave
    // only the last glyph.
    const missed = [];
    for (const run of RUNS)
      for (const scene of SCENES) {
        if (run.colour && scene.ink < 1) continue;
        for (const op of OPERATORS) {
          const model = (/** @type {Scene} */ s) =>
            BEYOND.includes(op) && op != "destination-out"
              ? fadedInside(run, op, s)
              : glyphs(run, op, s, false);
          const bound = SHARP_BOUND + carried(model, scene);
          const { most, over } = apart(
            drawn(run, op, scene, false).data,
            model(scene),
            bound,
          );
          if (most > bound)
            missed.push(`${named(run, op, scene)}: ${most} on ${over} bytes`);
        }
      }
    assert.deepEqual(missed, []);
  });

  test("under clear and modulate it leaves the page beyond it alone", () => {
    // Each takes a transparent source pixel to a transparent page pixel, so
    // the run composited as one image, transparent around its glyphs, would
    // clear the whole page. Drawn glyph by glyph, the rows above the run
    // keep the backdrop, and the run still changes the page where it lies.
    const above = W * 4 * 20;
    const backdropped = read(page("source-over", SCENES[0]).c);
    // A plain run: a colour run is drawn through `ReplayedText`, which
    // under `clear` still clears the page beyond the run.
    const run = RUNS[0];
    for (const op of ["clear", "modulate"]) {
      const { data } = drawn(run, op, SCENES[0], true);
      const name = named(run, op, SCENES[0]);
      const { most } = apart(
        data.subarray(0, above),
        backdropped.subarray(0, above),
        0,
      );
      assert.equal(most, 0, name);
      assert.ok(apart(data, backdropped, 0).most > 100, `${name}: drew`);
    }
  });

  test("dyed by a pattern or a gradient, it is drawn once", () => {
    // Its shadow is painted alone, then the run: a dye whose own route
    // filed the run with the shadow drew it twice, 97 to 255 levels from
    // the run casting none under `lighter` and `xor`. Cast off the page, the
    // shadow leaves only the run, which glyphs that do not overlap draw as
    // the run casting none draws it under an operator within the shape, and
    // under `destination-out`, which a shadow off the page leaves alone. Not
    // under one that divides by the run's colour: composited as one image,
    // the run is rounded to bytes before the operator takes it, as the
    // model's image is, and `color-burn` carries that level to three.
    const flat = { ...SCENES[0], spacing: 0 };
    const tile = new Canvas(8, 8),
      tctx = tile.getContext("2d");
    tctx.fillStyle = "rgb(40,90,160)";
    tctx.fillRect(0, 0, 8, 8);
    tctx.fillStyle = "rgb(160,40,90)";
    tctx.fillRect(0, 0, 4, 4);
    /** @type {[string, (ctx: CanvasRenderingContext2D) => any][]} */
    const DYES = [
      ["pattern", (ctx) => ctx.createPattern(tile, "repeat")],
      [
        "gradient",
        (ctx) => {
          const g = ctx.createLinearGradient(0, 0, W, 0);
          g.addColorStop(0, "rgb(40,90,160)");
          g.addColorStop(1, "rgb(160,40,90)");
          return g;
        },
      ],
    ];
    const dyed = (dye, op, shadowed) => {
      const c = canvas(),
        ctx = c.getContext("2d");
      backdrop(ctx);
      style(ctx, RUNS[0], flat);
      ctx.fillStyle = dye(ctx);
      ctx.globalCompositeOperation = op;
      ctx.shadowColor = shadowed
        ? `rgba(${SHADOW_RGB},${SHADOW_ALPHA})`
        : "transparent";
      [ctx.shadowOffsetX, ctx.shadowOffsetY] = [0, H];
      ink(ctx, flat, RUNS[0].text, X);
      return read(c);
    };
    for (const [name, dye] of DYES)
      for (const op of [
        "lighter",
        "xor",
        "source-atop",
        "multiply",
        "destination-out",
      ]) {
        const { most } = apart(dyed(dye, op, true), dyed(dye, op, false), 0);
        assert.ok(most <= SHARP_BOUND, `${name} ${op}: ${most}`);
      }
  });

  test("what is drawn after it takes the operator", () => {
    // The run is painted `source-over` inside its layer; the operator is
    // the context's again for the draw that follows.
    const run = RUNS[0],
      scene = SCENES[0],
      flat = { ...scene, spacing: 0 };
    const next = (/** @type {CanvasRenderingContext2D} */ ctx) => {
      style(ctx, run, flat);
      ctx.shadowColor = "transparent";
      ink(ctx, flat, "Wq", X + 140);
    };
    for (const op of ["xor", "difference", "source-atop"]) {
      const { c, ctx } = page(op, scene);
      style(ctx, run, scene);
      ctx.shadowColor = `rgba(${SHADOW_RGB},${SHADOW_ALPHA})`;
      [ctx.shadowOffsetX, ctx.shadowOffsetY] = OFFSET;
      ink(ctx, scene, run.text, X);
      next(ctx);
      // The same page, with the operator set afresh before the next run.
      const again = page(op, scene);
      style(again.ctx, run, scene);
      again.ctx.shadowColor = `rgba(${SHADOW_RGB},${SHADOW_ALPHA})`;
      [again.ctx.shadowOffsetX, again.ctx.shadowOffsetY] = OFFSET;
      ink(again.ctx, scene, run.text, X);
      again.ctx.globalCompositeOperation = "source-over";
      again.ctx.globalCompositeOperation = op;
      next(again.ctx);
      assert.equal(apart(read(c), read(again.c), 0).most, 0, op);
    }
  });

  test("drawn as an image into a GPU canvas, it lands the CPU canvas's bytes", (t) => {
    // A canvas holding the run is drawn as an image through the route a GPU
    // canvas takes for a page whose draws it composites on the CPU -- see
    // `Layer::plays_apart_on_gpu`. Replayed as a recording by Ganesh, the
    // run stood 2 levels from the CPU canvas under multiply and 12 under
    // color-burn.
    if (new Canvas(1, 1).engine.renderer != "GPU") return t.skip("no GPU here");
    const missed = [];
    for (const op of ["multiply", "color-burn", "source-atop", "source-in"])
      for (const gpu of [false, true]) {
        const source = canvas(gpu),
          sctx = source.getContext("2d");
        backdrop(sctx);
        style(sctx, RUNS[0], SCENES[0]);
        sctx.globalCompositeOperation = op;
        sctx.shadowColor = `rgba(${SHADOW_RGB},${SHADOW_ALPHA})`;
        [sctx.shadowOffsetX, sctx.shadowOffsetY] = OFFSET;
        ink(sctx, SCENES[0], RUNS[0].text, X);
        const into = (/** @type {boolean} */ gpu) => {
          const c = canvas(gpu);
          const ctx = c.getContext("2d");
          ctx.fillStyle = "rgb(200,200,210)";
          ctx.fillRect(0, 0, W, H);
          ctx.drawImage(source, 0, 0);
          return read(c);
        };
        const { most } = apart(into(true), into(false), SHARP_BOUND);
        if (most > SHARP_BOUND)
          missed.push(`${op}, from a ${gpu ? "GPU" : "CPU"} canvas: ${most}`);
      }
    assert.deepEqual(missed, []);
  });

  // A GPU canvas composites the run and its shadow on the CPU, over one read
  // back of the page -- see `layer::KeptRun::Image` -- so it stands from the
  // CPU canvas by a level at a tie at most. Composited by Ganesh, the run
  // stood up to 19 levels off under `color-burn`. A run casting no shadow,
  // or under `source-over`, is drawn glyph by glyph as before, and is not
  // this file's.
  test("the GPU canvas composites it as the CPU canvas does", (t) => {
    if (new Canvas(1, 1).engine.renderer != "GPU") return t.skip("no GPU here");
    const missed = [];
    for (const run of RUNS)
      for (const scene of SCENES)
        for (const op of OPERATORS) {
          const gpu = drawn(run, op, scene, true, true);
          assert.equal(gpu.engine.renderer, "GPU", "the canvas is on the GPU");
          const { most, over } = apart(
            gpu.data,
            drawn(run, op, scene, true).data,
            SHARP_BOUND,
          );
          if (most > SHARP_BOUND)
            missed.push(`${named(run, op, scene)}: ${most} on ${over} bytes`);
        }
    assert.deepEqual(missed, []);
  });
});
