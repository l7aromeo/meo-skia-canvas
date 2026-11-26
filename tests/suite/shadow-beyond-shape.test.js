// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

// Under an operator that changes the page beyond what a draw covers, the
// HTML Standard composites a draw's shadow and then the draw, each over the
// whole page. Drawn into one layer composited once, the shadow of a shape
// under `source-in` was cleared by the shape's own composite rather than
// composited first: Chrome and Firefox fit the two composites to rounding,
// and the one layer stood 200 to 255 levels from both.
describe("A shadowed draw under an operator reaching beyond it", () => {
  const W = 320,
    H = 160;
  const BEYOND = [
    "source-in",
    "source-out",
    "destination-in",
    "destination-out",
    "destination-atop",
    "copy",
  ];
  // A shadow colour whose premultiplied channels are whole bytes, so the
  // model's tint and the library's land on one byte away from a tie.
  const SHADOW = "rgba(200,30,60,0.6)",
    OFFSET = [6, 4];
  // How far a page may stand from the model: two levels. The library
  // multiplies `globalAlpha` into the shape before storing it on a byte and
  // the model after, so a shape's alpha may land a level apart; a composite
  // moves a byte by at most a level for each level of alpha it is scaled
  // by, and each side rounds its own composite once more.
  const BOUND = 2;

  const canvas = (gpu = false) => {
    const c = new Canvas(W, H);
    c.gpu = gpu;
    return c;
  };
  const read = (/** @type {Canvas} */ c) =>
    c.getContext("2d").getImageData(0, 0, W, H, { alphaType: "premultiplied" })
      .data;
  /** @param {CanvasRenderingContext2D} ctx */
  const backdrop = (ctx) => {
    ctx.fillStyle = "rgb(220,180,60)";
    ctx.fillRect(0, 0, W / 2, H);
    ctx.fillStyle = "rgb(30,120,90)";
    ctx.fillRect(W / 2, 0, W / 2, H);
  };
  // An image with an opaque part and a translucent one.
  const picture = (() => {
    const c = canvas(),
      ctx = c.getContext("2d");
    ctx.fillStyle = "rgba(90,40,160,0.8)";
    ctx.beginPath();
    ctx.arc(110, 70, 45, 0, 2 * Math.PI);
    ctx.fill();
    ctx.fillStyle = "rgb(40,160,90)";
    ctx.fillRect(70, 40, 40, 30);
    return c;
  })();
  /** @type {Record<string, (ctx: CanvasRenderingContext2D, fill: string) => void>} */
  const SHAPES = {
    rect: (ctx, fill) => {
      ctx.fillStyle = fill;
      ctx.fillRect(40, 30, 160, 70);
    },
    path: (ctx, fill) => {
      ctx.fillStyle = fill;
      ctx.beginPath();
      ctx.moveTo(30, 120);
      ctx.lineTo(150, 15);
      ctx.lineTo(260, 130);
      ctx.closePath();
      ctx.moveTo(130, 60);
      ctx.arc(100, 60, 30, 0, 2 * Math.PI);
      ctx.fill();
    },
    image: (ctx) => ctx.drawImage(picture, 30, 10),
  };
  const SCENES = [
    { fill: "rgb(40,90,160)", alpha: 1 },
    { fill: "rgb(40,90,160)", alpha: 0.6 },
    { fill: "rgba(40,90,160,0.8)", alpha: 1 },
    { fill: "rgba(40,90,160,0.8)", alpha: 0.6 },
  ];
  /** The shape alone, `source-over` on transparent black. */
  const shape = (name, fill) => {
    const c = canvas();
    SHAPES[name](c.getContext("2d"), fill);
    return c;
  };
  /** Its shadow: the shape's alpha in the shadow's colour, offset. */
  const shadow = (name, fill) => {
    const c = canvas(),
      ctx = c.getContext("2d");
    ctx.drawImage(shape(name, fill), ...OFFSET);
    ctx.globalCompositeOperation = "source-in";
    ctx.fillStyle = SHADOW;
    ctx.fillRect(0, 0, W, H);
    return c;
  };
  const page = (op, alpha, gpu = false) => {
    const c = canvas(gpu),
      ctx = c.getContext("2d");
    backdrop(ctx);
    ctx.globalCompositeOperation = op;
    ctx.globalAlpha = alpha;
    return { c, ctx };
  };
  const drawn = (name, op, { fill, alpha }, gpu = false) => {
    const { c, ctx } = page(op, alpha, gpu);
    ctx.shadowColor = SHADOW;
    [ctx.shadowOffsetX, ctx.shadowOffsetY] = OFFSET;
    SHAPES[name](ctx, fill);
    return read(c);
  };
  /** The Standard's painting: the shadow composited, then the shape. */
  const painted = (name, op, { fill, alpha }) => {
    const { c, ctx } = page(op, alpha);
    ctx.drawImage(shadow(name, fill), 0, 0);
    ctx.drawImage(shape(name, fill), 0, 0);
    return read(c);
  };
  const apart = (a, b) => {
    let most = 0,
      over = 0;
    for (let i = 0; i < a.length; i++) {
      const d = Math.abs(a[i] - b[i]);
      most = Math.max(most, d);
      if (d > BOUND) over++;
    }
    return { most, over };
  };

  test("its shadow is composited, then the draw", () => {
    const missed = [];
    for (const name of Object.keys(SHAPES))
      for (const scene of SCENES) {
        if (name == "image" && scene.fill.startsWith("rgba")) continue;
        for (const op of BEYOND) {
          const { most, over } = apart(
            drawn(name, op, scene),
            painted(name, op, scene),
          );
          if (most > BOUND)
            missed.push(
              `${name} ${op} ${JSON.stringify(scene)}: ${most} on ${over} bytes`,
            );
        }
      }
    assert.deepEqual(missed, []);
  });
});
