// @ts-check

// The colour space a file states has to survive the addon's own decode path,
// and this suite exists because a Rust test cannot say so. `Image::from_encoded`
// and the binding's loader were two copies of one decode, and only the crate's
// relabelled a BMP -- so a Display P3 BMP read here came back sRGB while the
// same bytes read from Rust did not. No Rust test executes `lib/`, which is
// the structural hole that let it ship rather than an oversight in the matrix.

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, loadImage } = require("../../lib");

// Every space a canvas can be built in. PQ and HLG are absent from the BMP
// rows below rather than from here: a BMP describes a transfer function with
// one exponent per channel and neither of those is a power law, so the
// encoder refuses them. That refusal is the correct answer and is asserted.
// sRGB is deliberately absent. Reading an sRGB canvas as sRGB is the same
// read twice, so both answers are one value and the cell passes whatever the
// code does -- the separation guard below refuses it rather than letting it
// count. PQ and HLG are absent for a different reason and are covered by
// their own tests further down.
const SPACES = [
  "srgb-linear",
  "display-p3",
  "display-p3-linear",
  "rec2020",
  "rec2020-linear",
];

// In every gamut, so nothing clips. A saturated fill is the trap this whole
// area keeps setting: Display P3 red is also sRGB red, so a cell measured in
// it passes whether or not the tag survived.
const FILL = "color(srgb 0.80 0.35 0.20)";

// How far apart the two answers must be before a cell can tell them apart.
// Below this the comparison proves nothing, so the test says so rather than
// passing quietly.
const SEPARATION = 8;

/** The pixel a canvas in `space` holds, read in `readAs`. */
function pixel(space, readAs) {
  const canvas = new Canvas(8, 8, { colorSpace: space });
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = FILL;
  ctx.fillRect(0, 0, 8, 8);
  const data = ctx.getImageData(0, 0, 1, 1, { colorSpace: readAs }).data;
  return [data[0], data[1], data[2]];
}

/** The largest per-channel difference between two pixels. */
function spread(a, b) {
  return Math.max(...[0, 1, 2].map((i) => Math.abs(a[i] - b[i])));
}

describe("colour space survives a round trip through the addon", () => {
  for (const space of SPACES) {
    for (const format of ["png", "bmp", "webp", "avif"]) {
      test(`${format} from a ${space} canvas`, async () => {
        // What the two outcomes look like: converted on the way out if the
        // file's space was honoured, handed through untouched if it was lost.
        const honoured = pixel(space, "srgb");
        const lost = pixel(space, space);
        assert.ok(
          spread(honoured, lost) >= SEPARATION,
          `${space}/${format}: the two answers are only ` +
            `${spread(honoured, lost)} apart, so this cell cannot tell a kept ` +
            `tag from a lost one -- pick a fill that separates them`,
        );

        const canvas = new Canvas(8, 8, { colorSpace: space });
        const ctx = canvas.getContext("2d");
        ctx.fillStyle = FILL;
        ctx.fillRect(0, 0, 8, 8);
        const image = await loadImage(await canvas.toBuffer(format));

        const back = new Canvas(8, 8, { colorSpace: "srgb" });
        back.getContext("2d").drawImage(image, 0, 0);
        const got = Array.from(
          back.getContext("2d").getImageData(0, 0, 1, 1).data,
        ).slice(0, 3);

        // Nearer one answer than the other, rather than within a fixed
        // distance of it. JPEG, WebP and AVIF are lossy, and a linear space
        // spends its eight bits unevenly, so an honoured cell can sit several
        // levels off while still being unmistakably the converted answer --
        // `display-p3-linear` through WebP lands 7 away from it and 50 from
        // the other. A fixed tolerance called that a failure.
        const toHonoured = spread(got, honoured),
          toLost = spread(got, lost);
        assert.ok(
          toHonoured * 2 < toLost,
          `${space}/${format}: read back as ${got}, which is ${toHonoured} ` +
            `from the converted ${honoured} and ${toLost} from the untagged ` +
            `${lost} -- not clearly the converted one`,
        );
      });
    }
  }

  // PQ and HLG reach a file only through a container that names a transfer
  // function by code point rather than describing it as a curve. AVIF does;
  // this is the pair that matters for HDR and is why it is tested apart from
  // the rows above rather than folded into them.
  for (const space of ["rec2020-pq", "rec2020-hlg"]) {
    test(`avif carries ${space}`, async () => {
      const honoured = pixel(space, "srgb"),
        lost = pixel(space, space);
      assert.ok(
        spread(honoured, lost) >= SEPARATION,
        `${space}: the two answers are only ${spread(honoured, lost)} apart`,
      );

      const canvas = new Canvas(8, 8, { colorSpace: space });
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = FILL;
      ctx.fillRect(0, 0, 8, 8);
      const image = await loadImage(await canvas.toBuffer("avif"));

      const back = new Canvas(8, 8, { colorSpace: "srgb" });
      back.getContext("2d").drawImage(image, 0, 0);
      const got = Array.from(
        back.getContext("2d").getImageData(0, 0, 1, 1).data,
      ).slice(0, 3);

      assert.ok(
        spread(got, honoured) * 2 < spread(got, lost),
        `${space}: read back as ${got} rather than the converted ${honoured}`,
      );
    });
  }

  test("a BMP refuses a transfer function it cannot describe", async () => {
    // Not a gap in the rows above. A `BITMAPV4HEADER` carries one exponent per
    // channel, and PQ and HLG are not power laws at all, so a BMP claiming one
    // would misdescribe every pixel rather than approximate them.
    for (const space of ["rec2020-pq", "rec2020-hlg"]) {
      const canvas = new Canvas(8, 8, { colorSpace: space });
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = FILL;
      ctx.fillRect(0, 0, 8, 8);
      await assert.rejects(
        canvas.toBuffer("bmp"),
        /gamma value per channel/,
        `${space} should be refused by the BMP encoder, not approximated`,
      );
    }
  });
});
