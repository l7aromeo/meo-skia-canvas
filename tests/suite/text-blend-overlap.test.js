// @ts-check

"use strict";

// A file of its own, so that a failure here -- the GPU canvas drawing glyphs
// a draw at a time under a blend -- is reported as this file's and does not
// take the rest of the text suite with it: node runs each test file in a
// process of its own.

const { assert, describe, test, beforeEach } = require("../runner"),
  { Canvas, FontLibrary } = require("../../lib");

describe("Glyphs drawn into one another under a blend that reads the page", () => {
  // Where the glyphs of one run overlap, the CPU canvas composites each over
  // what the glyphs before it left, as Chrome's CPU and GPU canvases do. The
  // GPU canvas kept one glyph's blend over the page as it stood before the
  // text under the 14 blends Skia does in the shader: Oswald's "Hello Wq" at
  // 48px with letterSpacing -12px stood 25 to 218 levels from the CPU canvas,
  // and the COLR face's boxes 10 to 195, sharing their edge pixels even
  // with no letter spacing.
  const OSWALD = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
  // A face with a COLR table: its "A" a colour glyph, its lowercase letters
  // plain boxes drawn between the colour glyphs.
  const PALETTES = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";
  const READ_IN_THE_SHADER = [
    "multiply",
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
    { font: "48px OverlapOswald", text: "Hello Wq" },
    { font: "40px OverlapPalettes", text: "Abcdef" },
  ];
  // letterSpacing, globalAlpha, the ink's alpha, and whether it is stroked.
  const SCENES = [
    [-12, 1, 1, false],
    [-12, 0.5, 1, false],
    [-12, 1, 0.5, false],
    [0, 1, 1, false],
    [-12, 1, 1, true],
    [-12, 0.5, 1, true],
  ];
  // As far as source-over itself stands for the same scenes on Metal: 3
  // levels filled and 5 stroked. Elsewhere -- Mesa's llvmpipe, not measured
  // -- the threshold the issue counts a channel as differing past, under
  // the 10 levels the least of the scenes stood drawn whole on Metal.
  const bound = (stroke, device) =>
    !/llvmpipe/.test(device) ? (stroke ? 5 : 3) : 8;
  // How far the Palettes face's em may measure from 40px at 40px: a 64th
  // of a pixel, the unit FreeType lays text out in. FreeType on Linux
  // reports it 2^-15 short, 39.999969482421875; macOS reports 40.
  const EM_TOLERANCE = 1 / 64;
  // How many tolerances the fallback face must stand from the em, so that
  // no face the check could mistake for the Palettes one passes it.
  const GAP_OVER_TOLERANCE = 100;

  beforeEach(() => {
    FontLibrary.use("OverlapOswald", [OSWALD]);
    FontLibrary.use("OverlapPalettes", [PALETTES]);
  });

  /** The page after the run is drawn twice in `blend`, and its engine. */
  const drawn = (gpu, { font, text }, blend, [spacing, alpha, ink, stroke]) => {
    const canvas = new Canvas(320, 160);
    canvas.gpu = gpu;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "rgb(220,180,60)";
    ctx.fillRect(0, 0, 160, 160);
    ctx.fillStyle = "rgb(30,120,90)";
    ctx.fillRect(160, 0, 160, 160);
    ctx.globalCompositeOperation = blend;
    ctx.globalAlpha = alpha;
    ctx.font = font;
    ctx.letterSpacing = `${spacing}px`;
    ctx.fillStyle = ctx.strokeStyle = `rgba(40,90,160,${ink})`;
    ctx.lineWidth = 3;
    for (const y of [70, 140])
      stroke ? ctx.strokeText(text, 10, y) : ctx.fillText(text, 10, y);
    return {
      data: ctx.getImageData(0, 0, 320, 160).data,
      engine: canvas.engine,
    };
  };

  test("the faces the scenes name are the ones drawn", () => {
    // A family not found falls back to the platform's face, and the
    // scenes would then compare text with no overlap the test means.
    const advance = (font) => {
      const ctx = new Canvas(1, 1).getContext("2d");
      ctx.font = font;
      return ctx.measureText("A").width;
    };
    // The Palettes face's "A" advances one em. The fallback's stands far
    // from it -- 26.68px at 40px on macOS -- and the check is good only
    // while it does, so the gap is measured here rather than assumed.
    const palettes = advance("40px OverlapPalettes");
    const fallback = advance("40px OverlapNowhere");
    assert.ok(
      Math.abs(fallback - 40) > GAP_OVER_TOLERANCE * EM_TOLERANCE,
      `the fallback's "A" is ${fallback}px, too near 40 to tell the faces apart`,
    );
    assert.ok(
      Math.abs(palettes - 40) <= EM_TOLERANCE,
      `"A" is ${palettes}px, not the Palettes face's 40px em`,
    );
    assert.notEqual(
      advance("48px OverlapOswald"),
      advance("48px OverlapNowhere"),
    );
  });

  test("the GPU canvas composites each glyph as the CPU canvas does", (t) => {
    if (new Canvas(1, 1).engine.renderer != "GPU") return t.skip("no GPU here");
    const apart = [];
    for (const blend of [...READ_IN_THE_SHADER, "source-over", "screen"])
      for (const run of RUNS)
        for (const scene of SCENES) {
          const cpu = drawn(false, run, blend, scene);
          const gpu = drawn(true, run, blend, scene);
          assert.equal(gpu.engine.renderer, "GPU", "the canvas is on the GPU");
          // Every pixel is opaque, so the straight bytes compared are the
          // premultiplied ones.
          for (let i = 3; i < cpu.data.length; i += 4)
            assert.equal(cpu.data[i], 255, `${blend}: the page stays opaque`);
          let most = 0;
          for (let i = 0; i < cpu.data.length; i++)
            most = Math.max(most, Math.abs(cpu.data[i] - gpu.data[i]));
          const limit = bound(scene[3], gpu.engine.device);
          if (most > limit)
            apart.push(
              `${run.text} ${blend} ${scene}: ${most} levels, past ${limit}`,
            );
        }
    assert.deepEqual(apart, []);
  });
});
