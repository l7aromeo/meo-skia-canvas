// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, Image } = require("../../lib");

// A read of one 256-pixel tile is served from the tile where the tile makes
// the page's draws as the page does, and from the page where it would not
// (#771). A tile's sides re-quantise an anti-aliased path's edges, on the
// GPU anywhere the path touches it, and its offset rounds the coordinates a
// gradient, a pattern or an image off whole pixels is sampled at. Each
// scene is read whole on one canvas and tile by tile on another; the bytes
// have to agree. Every scene came out apart on at least one engine with the
// tiles serving every read. Which draws keep the tiles is pinned by the
// Rust suite, which counts the reads they serve.
describe("a canvas read tile by tile stores what a whole read does", () => {
  const W = 768,
    H = 512,
    TILE = 256;

  /** A `side`-pixel image whose every neighbouring pixel differs. */
  const busy = (side) => {
    const canvas = new Canvas(side, side),
      ctx = canvas.getContext("2d"),
      data = ctx.createImageData(side, side);
    for (let i = 0; i < side * side; i++) {
      const c = ((i % side) * 37 + Math.floor(i / side) * 101) % 256;
      data.data.set([c, 255 - c, (c * 53) % 256, 255], i * 4);
    }
    ctx.putImageData(data, 0, 0);
    return new Image(canvas.toBufferSync("png"));
  };
  const big = busy(500),
    small = busy(64),
    tile = busy(50);

  /** A three-stop gradient along the diagonal, moved by `(f, g)`. */
  const gradient = (ctx, f, g) => {
    const grad = ctx.createLinearGradient(13.7 + f, 21.3 + g, 700.3 - f, 480.9);
    grad.addColorStop(0, "rgb(255 128 0)");
    grad.addColorStop(0.37, "rgb(25 230 75)");
    grad.addColorStop(1, "rgb(0 50 255)");
    return grad;
  };

  /** Sub-pixel placements, spread as the Rust sweep spreads them. */
  const OFFSETS = Array.from({ length: 12 }, (_, i) => [
    (i * 0.137 + 0.013) % 1,
    (i * 0.291 + 0.071) % 1,
  ]);

  /** @type {[string, (ctx: any, f: number, g: number) => void][]} */
  const SCENES = [
    [
      "a triangle across tiles",
      (ctx) => {
        ctx.beginPath();
        ctx.moveTo(10, 300.3);
        ctx.lineTo(760.7, 240.2);
        ctx.lineTo(700, 500.6);
        ctx.fill();
      },
    ],
    [
      "a gradient filling the page",
      (ctx, f, g) => {
        ctx.fillStyle = gradient(ctx, f, g);
        ctx.fillRect(0, 0, W, H);
      },
    ],
    [
      "a pattern moved part of a pixel",
      (ctx, f, g) => {
        ctx.fillStyle = ctx.createPattern(tile, "repeat");
        ctx.translate(f * 3, g * 3);
        ctx.fillRect(0, 0, W, H);
      },
    ],
    [
      "an image moved part of a pixel from the corner",
      (ctx, f, g) => {
        ctx.drawImage(big, f, g);
      },
    ],
    [
      "an image scaled inside one tile",
      (ctx, f, g) => {
        ctx.drawImage(small, 300 + f, 280 + g, 130.7, 120.2);
      },
    ],
  ];

  /** `scene` read whole, and read tile by tile into one buffer. */
  const reads = (gpu, scene, f, g) => {
    const draw = () => {
      const canvas = new Canvas(W, H, { gpu }),
        ctx = canvas.getContext("2d");
      ctx.fillStyle = "rgb(30 120 60)";
      scene(ctx, f, g);
      return ctx;
    };
    const whole = draw().getImageData(0, 0, W, H).data,
      ctx = draw(),
      pieced = new Uint8ClampedArray(whole.length);
    for (let ty = 0; ty < H; ty += TILE) {
      for (let tx = 0; tx < W; tx += TILE) {
        const part = ctx.getImageData(tx, ty, TILE, TILE).data;
        for (let y = 0; y < TILE; y++) {
          const from = y * TILE * 4;
          pieced.set(
            part.subarray(from, from + TILE * 4),
            ((ty + y) * W + tx) * 4,
          );
        }
      }
    }
    return { whole, pieced };
  };

  const onGpu = new Canvas(1, 1, { gpu: true }).engine?.renderer === "GPU";

  for (const gpu of [false, true]) {
    for (const [name, scene] of SCENES) {
      test(
        `${name}, ${gpu ? "GPU" : "CPU"}`,
        { skip: gpu && !onGpu && "no GPU engine" },
        () => {
          for (const [f, g] of OFFSETS) {
            const { whole, pieced } = reads(gpu, scene, f, g);
            let apart = 0;
            for (let i = 0; i < whole.length; i++)
              apart += whole[i] !== pieced[i] ? 1 : 0;
            assert.equal(apart, 0, `at (${f}, ${g}): bytes apart`);
          }
        },
      );
    }
  }
});
