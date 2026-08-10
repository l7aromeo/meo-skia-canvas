//
// ColorMatrix - CanvasKit-compatible 4x5 color-matrix helpers
//
// Pure math producing a 20-element row-major matrix suitable for
// `ColorFilter.MakeMatrix(...)`. Mirrors CanvasKit's `ColorMatrixHelpers`
// (exposed there as `CanvasKit.ColorMatrix`).
//
//   | r_r r_g r_b r_a r_t |
//   | g_r g_g g_b g_a g_t |
//   | b_r b_g b_b b_a b_t |
//   | a_r a_g a_b a_a a_t |
//
// Output channel c = c_r*R + c_g*G + c_b*B + c_a*A + c_t.

"use strict";

const ColorMatrix = {
  /** The 4x5 identity matrix (no color change). */
  identity() {
    return [1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0];
  },

  /**
   * Concatenate two color matrices: the result applies `inner` then
   * `outer` (i.e. `outer * inner`). Both are 20-element row-major.
   * @param {number[]} outer
   * @param {number[]} inner
   * @returns {number[]}
   */
  concat(outer, inner) {
    const out = new Array(20).fill(0);
    for (let row = 0; row < 4; row++) {
      for (let col = 0; col < 5; col++) {
        // Sum over the 4 color channels of `inner`'s column...
        let sum = 0;
        for (let k = 0; k < 4; k++) {
          sum += outer[row * 5 + k] * inner[k * 5 + col];
        }
        // ...plus `outer`'s translation column contributes only to the
        // result's translation column (col 4), via the implicit 1.
        if (col === 4) {
          sum += outer[row * 5 + 4];
        }
        out[row * 5 + col] = sum;
      }
    }
    return out;
  },

  /**
   * Add a per-channel translation (offset) in place and return the
   * matrix. Offsets are in the 0..1 range for normalized colors.
   * @param {number[]} m
   */
  postTranslate(m, dr, dg, db, da) {
    m[4] += dr;
    m[9] += dg;
    m[14] += db;
    m[19] += da;
    return m;
  },

  /**
   * Rotate hue around a color axis (0 = red, 1 = green, 2 = blue) by the
   * given sine/cosine. Used to build hue-rotation grades.
   * @param {0|1|2} axis
   * @param {number} sine
   * @param {number} cosine
   * @returns {number[]}
   */
  rotated(axis, sine, cosine) {
    const m = ColorMatrix.identity();
    const a = (axis + 1) % 3;
    const b = (axis + 2) % 3;
    m[a * 5 + a] = cosine;
    m[a * 5 + b] = sine;
    m[b * 5 + a] = -sine;
    m[b * 5 + b] = cosine;
    return m;
  },

  /**
   * Scale each channel independently (1 = unchanged).
   * @returns {number[]}
   */
  scaled(redScale, greenScale, blueScale, alphaScale) {
    const m = ColorMatrix.identity();
    m[0] = redScale;
    m[6] = greenScale;
    m[12] = blueScale;
    m[18] = alphaScale;
    return m;
  },
};

module.exports = { ColorMatrix };
