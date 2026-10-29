// @ts-check

"use strict";

// A half-float canvas stores the nearest half, subnormals included, on every
// machine. Skia's x86 raster stages differ here: the portable one an x86
// build is compiled with truncates and flushes subnormals, and the module
// selects the AVX2 one at load. tests/half_float_store.rs pins the crate;
// this pins the module, which starts Skia on its own.

const { assert, describe, test } = require("../runner"),
  { Canvas, ImageData } = require("../../lib"),
  fs = require("node:fs");

/**
 * Why the half-float test cannot run here, or `null` when it can. On x64,
 * Skia rounds a half only in its AVX2 stage, which needs AVX2 and F16C; a
 * CPU without them keeps the portable stage, which truncates. Read from
 * /proc/cpuinfo, the one place Node can see the flags; elsewhere on x64 the
 * answer is unknown, and the test skips rather than assume.
 */
function noRoundingStage() {
  if (process.arch !== "x64") return null;
  let flags;
  try {
    flags = fs.readFileSync("/proc/cpuinfo", "utf8").match(/^flags\s*:(.*)$/m);
  } catch {
    return "this x64 machine's CPU flags are unreadable, so AVX2 and F16C are unknown";
  }
  const has = new Set(flags ? flags[1].trim().split(/\s+/) : []);
  return has.has("avx2") && has.has("f16c")
    ? null
    : "this x64 CPU lacks AVX2 or F16C, so Skia keeps its portable half-float stage, which truncates";
}

/** Exactly between two halves; round-to-nearest-even takes the one above. */
const TIE = 0.5 + 1.5 / 2048;

/** The half `TIE` rounds to. */
const TIE_AS_HALF = 0.5 + 2 / 2048;

/** Three quarters of the way to the next half, which it rounds up to. */
const ABOVE_HALF = 0.5 + 0.75 / 2048;

/** The half `ABOVE_HALF` rounds to. */
const ABOVE_HALF_AS_HALF = 0.5 + 1 / 2048;

/** `2^-20`, a subnormal half exactly. */
const SUBNORMAL = 2 ** -20;

/** Floats in and out, linear and premultiplied, so nothing converts them. */
const FLOATS = {
  colorSpace: "srgb-linear",
  colorType: "RGBAF32",
  alphaType: "premultiplied",
};

/** What a 1x1 canvas of `colorType` holds after the three are written. */
function stored(colorType) {
  const canvas = new Canvas(1, 1, { colorSpace: "srgb-linear", colorType });
  const ctx = canvas.getContext("2d");
  const written = new Float32Array([TIE, SUBNORMAL, ABOVE_HALF, 1]);
  ctx.putImageData(
    new ImageData(new Uint8ClampedArray(written.buffer), 1, 1, FLOATS),
    0,
    0,
  );
  const { data } = ctx.getImageData(0, 0, 1, 1, FLOATS);
  return [...new Float32Array(data.buffer, data.byteOffset, 4)];
}

describe("a half-float canvas", () => {
  test("a float canvas holds the values as written", () => {
    assert.deepEqual(stored("RGBAF32"), [
      Math.fround(TIE),
      Math.fround(SUBNORMAL),
      Math.fround(ABOVE_HALF),
      1,
    ]);
  });

  test("stores the nearest half and keeps subnormals", (t) => {
    const reason = noRoundingStage();
    if (reason) return t.skip(reason);
    assert.deepEqual(stored("RGBAF16"), [
      TIE_AS_HALF,
      SUBNORMAL,
      ABOVE_HALF_AS_HALF,
      1,
    ]);
  });
});
