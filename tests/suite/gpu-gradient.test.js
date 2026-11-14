// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

// A gradient on a GPU canvas is drawn in the bytes the CPU canvas draws it
// in (#667): by an effect computing the CPU's arithmetic on the GPU, or,
// where it cannot, by the CPU over the GPU's pixels. Ganesh's own gradient
// moved a hard stop by up to a tenth of a degree or half a pixel and landed
// a smooth ramp up to five levels away.
describe("a gradient on a GPU canvas", () => {
  const onGpu = new Canvas(1, 1, { gpu: true }).engine?.renderer === "GPU";
  const skip = !onGpu && "no GPU engine";

  /** The issue's conic: a period of 45 degrees from 292.5, a hard stop at each wrap. */
  const issueConic = (ctx) => {
    const g = ctx.createConicGradient(0, 28, 21.2);
    for (let k = -7; k <= 1; k++) {
      const at = (t) => (292.5 + (k + t) * 45) / 360;
      const add = (t, c) => {
        const u = at(t);
        if (u >= 0 && u <= 1) g.addColorStop(u, c);
      };
      add(0, "rgb(47 79 79)");
      add(1 / 3, "red");
      add(2 / 3, "blue");
      add(1, "rgb(250 240 230)");
    }
    return g;
  };

  /** A smooth sweep, steep enough that Ganesh's half-precision angle shows. */
  const sweep = (ctx) => {
    const g = ctx.createConicGradient(0.7, 30.25, 33.75);
    for (let k = 0; k < 6; k++) {
      g.addColorStop(k / 6, "rgb(250 200 0)");
      g.addColorStop((k + 0.5) / 6, "rgb(0 60 200)");
    }
    g.addColorStop(1, "rgb(250 200 0)");
    return g;
  };

  /** A translucent two-point conical gradient, which the CPU draws in floats. */
  const radial = (ctx, space) => {
    const g = ctx.createRadialGradient(40, 50, 3, 70, 60, 90);
    if (space) g.colorInterpolationSpace = space;
    for (let k = 0; k < 12; k++) {
      g.addColorStop(k / 12, "rgb(200 30 90 / 0.7)");
      g.addColorStop((k + 0.5) / 12, "lime");
    }
    return g;
  };

  /** `scene` drawn on a `width` by `height` canvas on the GPU or the CPU. */
  const drawn = (gpu, width, height, scene) => {
    const canvas = new Canvas(width, height);
    canvas.gpu = gpu;
    const ctx = canvas.getContext("2d");
    scene(ctx);
    return ctx.getImageData(0, 0, width, height).data;
  };

  /** How many bytes of `a` and `b` differ. */
  const apart = (a, b) => {
    let differ = 0;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) differ++;
    return differ;
  };

  test(
    "takes the CPU's side of each hard stop of the issue's conic",
    { skip },
    () => {
      // Ganesh put 8 of these pixels on the other side of a wrap, 203 levels
      // off. One byte still parts, on a tie: the CPU's own value there is
      // 121.50000.
      const scene = (ctx) => {
        ctx.fillStyle = issueConic(ctx);
        ctx.fillRect(0, 0, 64, 64);
      };
      const cpu = drawn(false, 64, 64, scene),
        gpu = drawn(true, 64, 64, scene);
      let past = 0;
      for (let i = 0; i < cpu.length; i += 4) {
        const gap = Math.max(
          ...[0, 1, 2].map((c) => Math.abs(cpu[i + c] - gpu[i + c])),
        );
        if (gap > 1) past++;
      }
      assert.equal(past, 0, `${past} pixels more than a level from the CPU's`);
    },
  );

  const exact = {
    // 340 hard stops, past the 128 colours Ganesh keeps in uniforms: it
    // holds their offsets in half floats beyond that.
    "stripes, each hard stop 0.2px past a pixel's centre": [
      1024,
      2,
      (ctx) => {
        const g = ctx.createLinearGradient(0, 0, 1024, 0);
        let flip = false;
        for (let j = 3; j < 1020; j += 3) {
          const at = (j + 0.5 + 0.2) / 1024;
          g.addColorStop(at, flip ? "white" : "black");
          g.addColorStop(at, flip ? "black" : "white");
          flip = !flip;
        }
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, 1024, 2);
      },
    ],
    "a smooth sweep": [
      64,
      64,
      (ctx) => {
        ctx.fillStyle = sweep(ctx);
        ctx.fillRect(0, 0, 64, 64);
      },
    ],
    "a stroke at a globalAlpha": [
      64,
      64,
      (ctx) => {
        ctx.globalAlpha = 0.6;
        ctx.lineWidth = 10;
        ctx.strokeStyle = sweep(ctx);
        ctx.strokeRect(10, 10, 40, 40);
      },
    ],
    "a translucent radial gradient over a fill": [
      96,
      96,
      (ctx) => {
        ctx.fillStyle = "rgb(20 40 200)";
        ctx.fillRect(0, 0, 96, 96);
        ctx.fillStyle = radial(ctx);
        ctx.fillRect(0, 0, 96, 96);
      },
    ],
    "a gradient interpolated in Oklab": [
      96,
      96,
      (ctx) => {
        ctx.fillStyle = "rgb(20 40 200)";
        ctx.fillRect(0, 0, 96, 96);
        ctx.fillStyle = radial(ctx, "oklab");
        ctx.fillRect(0, 0, 96, 96);
      },
    ],
    // Blends that change what lies beyond the draw clear the page outside it.
    "a gradient through copy": [
      64,
      64,
      (ctx) => {
        ctx.fillStyle = "rgb(50 100 150)";
        ctx.fillRect(0, 0, 64, 64);
        ctx.globalCompositeOperation = "copy";
        ctx.fillStyle = sweep(ctx);
        ctx.fillRect(10, 12, 30, 25);
      },
    ],
    "a gradient through source-in": [
      64,
      64,
      (ctx) => {
        ctx.fillStyle = "rgb(50 100 150)";
        ctx.fillRect(0, 0, 64, 64);
        ctx.globalCompositeOperation = "source-in";
        ctx.fillStyle = sweep(ctx);
        ctx.fillRect(10, 12, 30, 25);
      },
    ],
    // Drawn by the CPU over the GPU's pixels, read back once for the run:
    // each over what the draws before it left.
    "a run of overlapping translucent Oklab radials": [
      96,
      96,
      (ctx) => {
        ctx.fillStyle = "rgb(20 40 200)";
        ctx.fillRect(0, 0, 96, 96);
        ctx.fillStyle = radial(ctx, "oklab");
        for (let k = 0; k < 40; k++)
          ctx.fillRect((k * 7.3) % 70, (k * 4.1) % 70, 26, 22);
      },
    ],
    "a gradient through color-dodge": [
      64,
      64,
      (ctx) => {
        ctx.fillStyle = "rgb(50 100 150)";
        ctx.fillRect(0, 0, 64, 64);
        ctx.globalCompositeOperation = "color-dodge";
        ctx.fillStyle = issueConic(ctx);
        ctx.fillRect(0, 0, 64, 64);
      },
    ],
  };
  for (const [name, [width, height, scene]] of Object.entries(exact))
    test(`stores the CPU canvas's bytes for ${name}`, { skip }, () => {
      const cpu = drawn(false, width, height, scene),
        gpu = drawn(true, width, height, scene);
      assert.equal(apart(cpu, gpu), 0);
      // The Canvas standard's result, which both engines could miss alike:
      // these blends clear the fill outside the draw.
      if (/copy|source-in/.test(name))
        assert.deepEqual([...cpu.slice(0, 4)], [0, 0, 0, 0]);
    });

  /**
   * How many pixels apart `a` and `b` are among those `a` inks whole, with
   * all eight neighbours: the edge of a shape is the GPU's own coverage, as
   * it is for a flat colour, and is not counted.
   */
  const interiorApart = (a, width, height) => (b) => {
    const whole = (x, y) =>
      x >= 0 &&
      y >= 0 &&
      x < width &&
      y < height &&
      a[(y * width + x) * 4 + 3] === 255;
    let differ = 0;
    for (let y = 0; y < height; y++)
      for (let x = 0; x < width; x++) {
        const i = (y * width + x) * 4;
        if ([0, 1, 2, 3].every((k) => a[i + k] === b[i + k])) continue;
        let inside = true;
        for (let dy = -1; dy <= 1; dy++)
          for (let dx = -1; dx <= 1; dx++) inside &&= whole(x + dx, y + dy);
        if (inside) differ++;
      }
    return differ;
  };

  /** The byte offsets of the pixels `a` inks whole, with all eight neighbours. */
  const inside = function* (a, width, height) {
    const whole = (x, y) =>
      x >= 0 &&
      y >= 0 &&
      x < width &&
      y < height &&
      a[(y * width + x) * 4 + 3] === 255;
    for (let y = 0; y < height; y++)
      for (let x = 0; x < width; x++) {
        let all = true;
        for (let dy = -1; dy <= 1; dy++)
          for (let dx = -1; dx <= 1; dx++) all &&= whole(x + dx, y + dy);
        if (all) yield (y * width + x) * 4;
      }
  };

  const clipped = (ctx) => {
    ctx.beginPath();
    ctx.arc(32, 32, 22, 0, 2 * Math.PI);
    ctx.clip();
  };
  const shapes = {
    // Ganesh draws each of these from geometry it derives the shader's
    // coordinates for only nearly: a rectangle whose edges fall inside
    // pixels, a path, a fill and a stroke the clip cuts, and glyphs.
    "a rectangle whose edges fall inside pixels": (ctx) => {
      ctx.fillStyle = sweep(ctx);
      ctx.fillRect(3.37, 2.63, 57.5, 58.25);
    },
    "a triangle": (ctx) => {
      ctx.fillStyle = sweep(ctx);
      ctx.beginPath();
      ctx.moveTo(0, 0);
      ctx.lineTo(64, 10);
      ctx.lineTo(10, 64);
      ctx.fill();
    },
    "a triangle the clip cuts": (ctx) => {
      clipped(ctx);
      ctx.fillStyle = sweep(ctx);
      ctx.beginPath();
      ctx.moveTo(0, 0);
      ctx.lineTo(64, 10);
      ctx.lineTo(10, 64);
      ctx.fill();
    },
    "a stroke the clip cuts": (ctx) => {
      clipped(ctx);
      ctx.lineWidth = 8;
      ctx.strokeStyle = sweep(ctx);
      ctx.strokeRect(10, 10, 40, 40);
    },
    text: (ctx) => {
      ctx.font = "bold 40px sans-serif";
      ctx.fillStyle = sweep(ctx);
      ctx.fillText("Wg", 2, 48);
    },
  };
  for (const [name, scene] of Object.entries(shapes))
    test(`stores the CPU canvas's bytes inside ${name}`, { skip }, () => {
      const cpu = drawn(false, 64, 64, scene);
      assert.equal(interiorApart(cpu, 64, 64)(drawn(true, 64, 64, scene)), 0);
    });

  const sweepScene = (ctx) => {
    ctx.fillStyle = sweep(ctx);
    ctx.fillRect(0, 0, 64, 64);
  };

  test(
    "drawn on the CPU and then switched to the GPU, as drawn on the CPU",
    { skip },
    () => {
      // Chosen where the page is played, not where it is drawn.
      const canvas = new Canvas(64, 64);
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      sweepScene(ctx);
      canvas.gpu = true;
      assert.equal(canvas.engine.renderer, "GPU");
      assert.equal(
        apart(
          drawn(false, 64, 64, sweepScene),
          ctx.getImageData(0, 0, 64, 64).data,
        ),
        0,
      );
    },
  );

  const transforms = {
    "scaled by 2": (ctx) => ctx.scale(2, 2),
    "scaled by 0.5": (ctx) => ctx.scale(0.5, 0.5),
    "moved part of a pixel": (ctx) => ctx.translate(0.5, 0.25),
    rotated: (ctx) => {
      ctx.translate(32, 0);
      ctx.rotate(0.3);
    },
  };
  for (const [name, transform] of Object.entries(transforms))
    test(
      `drawn with drawCanvas into a GPU canvas ${name}, as on the CPU inside`,
      { skip },
      () => {
        // The image the effect is sampled from has the source's device pixels
        // for texels; under any other mapping than whole pixels the draw is
        // made on the CPU at the destination's pixels instead.
        const into = (gpu) =>
          drawn(gpu, 128, 128, (ctx) => {
            const source = new Canvas(64, 64);
            source.gpu = true;
            sweepScene(source.getContext("2d"));
            transform(ctx);
            ctx.drawCanvas(source, 0, 0);
          });
        // The CPU raster is made at the destination's pixels from an origin
        // of its own, and the CPU canvas draws into the layer from another:
        // the two invert the transform with different translations, which
        // can move a value on a half level to the next. Nothing further.
        {
          const cpu = into(false),
            gpu = into(true);
          let most = 0;
          for (const i of inside(cpu, 128, 128))
            for (let k = 0; k < 4; k++)
              most = Math.max(most, Math.abs(cpu[i + k] - gpu[i + k]));
          assert.ok(most <= 1, `${most} levels apart`);
        }
        // Through another blend, inside the layer the source is drawn
        // through, the draw made on the CPU over transparent is composited
        // through the blend by the GPU: a level apart where its rounding and
        // the CPU's differ. `source-atop` inks nothing over transparent, so
        // the draw is made there with `source-over`.
        for (const blend of ["multiply", "source-atop"]) {
          const blended = (gpu) =>
            drawn(gpu, 128, 128, (ctx) => {
              const source = new Canvas(64, 64);
              source.gpu = true;
              const drawing = source.getContext("2d");
              drawing.fillStyle = "rgb(20 40 200)";
              drawing.fillRect(0, 0, 64, 64);
              drawing.globalCompositeOperation = blend;
              sweepScene(drawing);
              transform(ctx);
              ctx.drawCanvas(source, 0, 0);
            });
          const cpu = blended(false),
            gpu = blended(true);
          let most = 0;
          for (const i of inside(cpu, 128, 128))
            for (let k = 0; k < 4; k++)
              most = Math.max(most, Math.abs(cpu[i + k] - gpu[i + k]));
          assert.ok(most <= 1, `${most} levels apart through ${blend}`);
        }
      },
    );

  test(
    "stores the CPU canvas's bytes inside a run of many gradient draws",
    { skip },
    () => {
      // Each draw takes its effect from one atlas, in rows the atlas packs.
      // The triangles lie apart on a grid, so that every edge lies over the
      // transparent page and only their insides are compared.
      const scene = (ctx) => {
        // Two sweeps, which ask a vector backend for the same, so that the
        // draws are one run, and the atlas holds them all.
        const one = sweep(ctx),
          other = ctx.createConicGradient(0.7, 190.5, 70.25);
        for (let k = 0; k < 6; k++) {
          other.addColorStop(k / 6, "rgb(250 200 0)");
          other.addColorStop((k + 0.5) / 6, "rgb(0 60 200)");
        }
        other.addColorStop(1, "rgb(250 200 0)");
        for (let k = 0; k < 400; k++) {
          const x = (k % 20) * 12.8,
            y = Math.floor(k / 20) * 12.8;
          ctx.fillStyle = k % 2 === 0 ? one : other;
          ctx.beginPath();
          ctx.moveTo(x + 0.3, y + 0.6);
          ctx.lineTo(x + 11.4, y + 1.1);
          ctx.lineTo(x + 2.2, y + 11.7);
          ctx.fill();
        }
      };
      // Each slot maps the effect through a translation of its own, which can
      // move a value on a half level to the next: no further.
      const cpu = drawn(false, 256, 256, scene),
        gpu = drawn(true, 256, 256, scene);
      let most = 0;
      for (const i of inside(cpu, 256, 256))
        for (let k = 0; k < 4; k++)
          most = Math.max(most, Math.abs(cpu[i + k] - gpu[i + k]));
      assert.ok(most <= 1, `${most} levels apart`);
    },
  );

  for (const density of [2, 1.5])
    test(
      `read at a density of ${density}, as on the CPU inside`,
      { skip },
      () => {
        // The page is played onto the canvas's own pixels under the scale,
        // outside any layer: the draw is made on the CPU over them instead of
        // resampling the effect's image.
        // Through a blend, or translucent, the draw made over transparent and
        // composited rounds twice: only the pixels read back give the CPU's
        // bytes.
        const over = (setup) => (ctx) => {
          ctx.fillStyle = "rgb(20 40 200)";
          ctx.fillRect(0, 0, 64, 64);
          setup(ctx);
          ctx.fillStyle = sweep(ctx);
          ctx.fillRect(0, 0, 64, 64);
        };
        const multiplied = over(
            (ctx) => (ctx.globalCompositeOperation = "multiply"),
          ),
          translucent = over((ctx) => (ctx.globalAlpha = 0.6));
        for (const scene of [shapes["a triangle"], multiplied, translucent]) {
          const read = (gpu) => {
            const canvas = new Canvas(64, 64);
            canvas.gpu = gpu;
            scene(canvas.getContext("2d"));
            return canvas.toBufferSync("raw", { density });
          };
          const side = 64 * density;
          const cpu = read(false);
          assert.equal(cpu.length, side * side * 4);
          assert.equal(interiorApart(cpu, side, side)(read(true)), 0);
        }
      },
    );

  for (const into of [false, true])
    test(
      `drawn with drawCanvas into a ${into ? "GPU" : "CPU"} canvas, as drawn on the CPU`,
      { skip },
      () => {
        const source = new Canvas(64, 64);
        source.gpu = true;
        sweepScene(source.getContext("2d"));
        const canvas = new Canvas(64, 64);
        canvas.gpu = into;
        const ctx = canvas.getContext("2d");
        ctx.drawCanvas(source, 0, 0);
        assert.equal(
          apart(
            drawn(false, 64, 64, sweepScene),
            ctx.getImageData(0, 0, 64, 64).data,
          ),
          0,
        );
      },
    );
});
