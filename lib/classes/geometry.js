//
// Polyfill for DOMMatrix and friends
//

"use strict";

const { inspect } = require("util");

const isPlainObject = (o) =>
  o !== null &&
  typeof o === "object" &&
  !(o instanceof DOMMatrix) &&
  !Array.isArray(o) &&
  !ArrayBuffer.isView(o);

/*
 * vendored in order to fix its dependence on the window global [@samizdatco 2020/08/04]
 * removed SVGMatrix references that were guaranteed to be undefined on node [@mpaperno 2024/10/20]
 * added support for parsing existing matrices (and matrix-like) objects in constructor [@mpaperno 2024/10/20]
 * added `parseTransform*` helpers to enable CSS-style strings as constructor args [@samizdatco 2024/10/29]
 * otherwise unchanged from https://github.com/jarek-foksa/geometry-polyfill/tree/f36bbc8f4bc43539d980687904ce46c8e915543d
 */

// @info
//   DOMPoint polyfill
// @src
//   https://drafts.fxtf.org/geometry/#DOMPoint
//   https://github.com/chromium/chromium/blob/master/third_party/blink/renderer/core/geometry/dom_point_read_only.cc
class DOMPoint {
  constructor(x = 0, y = 0, z = 0, w = 1) {
    this.x = x;
    this.y = y;
    this.z = z;
    this.w = w;
  }

  static fromPoint(otherPoint = {}) {
    return new DOMPoint(
      otherPoint.x ?? 0,
      otherPoint.y ?? 0,
      otherPoint.z !== undefined ? otherPoint.z : 0,
      otherPoint.w !== undefined ? otherPoint.w : 1,
    );
  }

  matrixTransform(matrix) {
    if (matrix.is2D && this.z === 0 && this.w === 1) {
      return new DOMPoint(
        this.x * matrix.a + this.y * matrix.c + matrix.e,
        this.x * matrix.b + this.y * matrix.d + matrix.f,
        0,
        1,
      );
    } else {
      return new DOMPoint(
        this.x * matrix.m11 +
          this.y * matrix.m21 +
          this.z * matrix.m31 +
          this.w * matrix.m41,
        this.x * matrix.m12 +
          this.y * matrix.m22 +
          this.z * matrix.m32 +
          this.w * matrix.m42,
        this.x * matrix.m13 +
          this.y * matrix.m23 +
          this.z * matrix.m33 +
          this.w * matrix.m43,
        this.x * matrix.m14 +
          this.y * matrix.m24 +
          this.z * matrix.m34 +
          this.w * matrix.m44,
      );
    }
  }

  toJSON() {
    return {
      x: this.x,
      y: this.y,
      z: this.z,
      w: this.w,
    };
  }
}

// @info
//   DOMRect polyfill
// @src
//   https://drafts.fxtf.org/geometry/#DOMRect
//   https://github.com/chromium/chromium/blob/master/third_party/blink/renderer/core/geometry/dom_rect_read_only.cc

class DOMRect {
  constructor(x = 0, y = 0, width = 0, height = 0) {
    this.x = x;
    this.y = y;
    this.width = width;
    this.height = height;
  }

  static fromRect(otherRect = {}) {
    return new DOMRect(
      otherRect.x ?? 0,
      otherRect.y ?? 0,
      otherRect.width ?? 0,
      otherRect.height ?? 0,
    );
  }

  // Each edge is the NaN-safe min/max of the coordinate and coordinate+extent,
  // per https://drafts.csswg.org/geometry/#dom-domrectreadonly-top. Returning
  // `y` and `x + width` directly is only right for non-negative extents:
  // DOMRect(10, 10, -6, -4) reported left=10 right=4, a rect inside-out.
  // `Math.min`/`Math.max` propagate NaN, which is the NaN-safe part.
  get top() {
    return Math.min(this.y, this.y + this.height);
  }

  get left() {
    return Math.min(this.x, this.x + this.width);
  }

  get right() {
    return Math.max(this.x, this.x + this.width);
  }

  get bottom() {
    return Math.max(this.y, this.y + this.height);
  }

  toJSON() {
    return {
      x: this.x,
      y: this.y,
      width: this.width,
      height: this.height,
      top: this.top,
      left: this.left,
      right: this.right,
      bottom: this.bottom,
    };
  }
}

for (let propertyName of ["top", "right", "bottom", "left"]) {
  let propertyDescriptor = Object.getOwnPropertyDescriptor(
    DOMRect.prototype,
    propertyName,
  );
  propertyDescriptor.enumerable = true;
  Object.defineProperty(DOMRect.prototype, propertyName, propertyDescriptor);
}

// @info
//   DOMMatrix polyfill (SVG 2)
// @src
//   https://github.com/chromium/chromium/blob/master/third_party/blink/renderer/core/geometry/dom_matrix_read_only.cc
//   https://github.com/tocharomera/generativecanvas/blob/master/node-canvas/lib/DOMMatrix.js

const M11 = 0,
  M12 = 1,
  M13 = 2,
  M14 = 3;
const M21 = 4,
  M22 = 5,
  M23 = 6,
  M24 = 7;
const M31 = 8,
  M32 = 9,
  M33 = 10,
  M34 = 11;
const M41 = 12,
  M42 = 13,
  M43 = 14,
  M44 = 15;

const A = M11,
  B = M12;
const C = M21,
  D = M22;
const E = M41,
  F = M42;

const DEGREE_PER_RAD = 180 / Math.PI;
const RAD_PER_DEGREE = Math.PI / 180;

// `DOMMatrixInit` is a plain object in the spec, and the multiply family is
// specified to take one. The internal value slot only exists on a real
// DOMMatrix, so coerce first; an omitted argument means identity.
const asMatrix = (m) => (m instanceof DOMMatrix ? m : new DOMMatrix(m));

const $values = Symbol();
const $is2D = Symbol();

// Parsers for CSS-style string initializers
const parseTransformName = (name) =>
  name.match(/^(matrix(3d)?|(rotate|translate|scale)(3d|X|Y|Z)?|skew(X|Y)?)$/);

const parseAngle = (value) => {
  if (value.endsWith("deg")) return parseFloat(value);
  if (value.endsWith("rad")) return parseFloat(value) * DEGREE_PER_RAD;
  if (value.endsWith("turn")) return parseFloat(value) * 360;
  throw new TypeError(
    `Angles must be in 'deg', 'rad', or 'turn' units (got: "${value}")`,
  );
};

const parseLength = (value) => {
  if (value.endsWith("px")) return parseFloat(value);
  if (!isNaN(value) && !isNaN(parseFloat(value))) return parseFloat(value);
  throw new TypeError(
    `Lengths must be in 'px' or numeric units (got: "${value}")`,
  );
};

const parseScalar = (value) => {
  if (value.endsWith("%")) return parseFloat(value) / 100;
  if (!isNaN(value) && !isNaN(parseFloat(value))) return parseFloat(value);
  throw new TypeError(
    `Scales must be in '%' or numeric units (got: "${value}")`,
  );
};

const parseNumeric = (value) => {
  if (!isNaN(value) && !isNaN(parseFloat(value))) return parseFloat(value);
  throw new TypeError(
    `Matrix values must be in plain, numeric units (got: "${value}")`,
  );
};

const parseTransformString = (transformString) => {
  return transformString
    .split(/\)\s*?/)
    .filter((s) => !!s.trim())
    .map((transform) => {
      let [name, transformValue] = transform.split("(").map((s) => s.trim());

      // catch single-word initializers
      if (!transformValue) {
        if (name.match(/^(inherit|initial|revert(-layer)?|unset|none)$/))
          return { op: "matrix", vals: [1, 0, 0, 1, 0, 0] };
        throw new SyntaxError("The string did not match the expected pattern");
      }

      // otherwise check that the last term was well formed before splitting based on `)`
      if (!transformString.trim().endsWith(")")) {
        throw new SyntaxError("Expected a closing ')'");
      }

      // validate & normalize op names
      if (!parseTransformName(name)) {
        throw new SyntaxError(`Unknown transform operation: ${name}`);
      } else if (name == "rotate3d") {
        name = "rotateAxisAngle";
      }

      // validate the individual values & units
      const rawVals = transformValue.split(",").map((s) => s.trim());
      const values = name.startsWith("rotate")
        ? [...rawVals.slice(0, -1).map(parseLength), parseAngle(rawVals.at(-1))]
        : name.startsWith("skew")
          ? rawVals.map(parseAngle)
          : name.startsWith("scale")
            ? rawVals.map(parseScalar)
            : name.startsWith("matrix")
              ? rawVals.map(parseNumeric)
              : rawVals.map(parseLength);

      // special case validation for the matrix/3d ops
      for (const [form, len] of [
        ["matrix", 6],
        ["matrix3d", 16],
      ]) {
        if (name == form && values.length != len) {
          throw new TypeError(
            `${name}() requires 6 numeric values (got ${values.length})`,
          );
        }
      }

      // catch single-dimension ops and route them to the corresponding 3D matrix method
      const parts = name.match(/^(rotate|translate|scale)(3d|X|Y|Z)$/);
      if (parts) {
        const [_, op, dim] = parts;
        const fill = op == "scale" ? 1 : 0;
        return {
          op,
          vals:
            dim == "X"
              ? [values[0], fill, fill]
              : dim == "Y"
                ? [fill, values[0], fill]
                : dim == "Z"
                  ? [fill, fill, values[0]]
                  : values,
        };
      } else {
        return { op: name, vals: values };
      }
    })
    .flat();
};

let setNumber2D = (receiver, index, value) => {
  if (typeof value !== "number") {
    throw new TypeError("Expected number");
  }

  receiver[$values][index] = value;
};

let setNumber3D = (receiver, index, value) => {
  if (typeof value !== "number") {
    throw new TypeError("Expected number");
  }

  if (index === M33 || index === M44) {
    if (value !== 1) {
      receiver[$is2D] = false;
    }
  } else if (value !== 0) {
    receiver[$is2D] = false;
  }

  receiver[$values][index] = value;
};

// The 2D aliases and the 4x4 cells they name. A dictionary may carry either
// spelling, and if it carries both they have to agree.
const MATRIX_ALIASES = [
  ["a", "m11"],
  ["b", "m12"],
  ["c", "m21"],
  ["d", "m22"],
  ["e", "m41"],
  ["f", "m42"],
];

// The cells that make a matrix 3D: eight that do so by departing from zero,
// and two on the diagonal that do so by departing from one.
const CELLS_3D_FROM_ZERO = [
  "m13",
  "m14",
  "m23",
  "m24",
  "m31",
  "m32",
  "m34",
  "m43",
];
const CELLS_3D_FROM_ONE = ["m33", "m44"];

/**
 * A matrix from a `DOMMatrixInit`, reading every cell it names.
 *
 * `fromMatrix` used to route a dictionary two ways: one requiring all sixteen
 * cells, and otherwise a branch that destructured `a, b, c, d, e, f` and
 * nothing else. A *partial* dictionary -- which is the ordinary case, and
 * what `lib/index.d.ts` describes when it says a cell left out takes its
 * value from the identity -- failed the first and lost every 3D cell to the
 * second. `{m13: 5}` read back as `0`, silently, and `is2D` then reported
 * true because the content it was describing had already been discarded.
 *
 * That is also why the `TypeError` the declarations promise for `is2D: true`
 * beside a 3D cell never fired: the contradiction was erased before anything
 * could see it.
 */
const fromDictionary = (init) => {
  for (const [short, long] of MATRIX_ALIASES) {
    const a = init[short];
    const b = init[long];
    // NaN is allowed and never equals itself, so the two spellings agree if
    // both are NaN. Chrome refuses a genuine mismatch rather than choosing.
    const agree = a === b || (Number.isNaN(a) && Number.isNaN(b));
    if (a !== undefined && b !== undefined && !agree) {
      throw new TypeError(
        `Matrix init names ${short} = ${a} and ${long} = ${b}, which disagree`,
      );
    }
  }

  const departs = (key, identity) =>
    init[key] !== undefined && init[key] !== identity;
  const is3D =
    CELLS_3D_FROM_ZERO.some((key) => departs(key, 0)) ||
    CELLS_3D_FROM_ONE.some((key) => departs(key, 1));

  if (init.is2D === true && is3D) {
    throw new TypeError(
      "Matrix init sets is2D true beside a 3D cell that is not the identity",
    );
  }

  const cell = (long, short, identity) => {
    const named = init[long] !== undefined ? init[long] : init[short];
    return named === undefined ? identity : named;
  };

  // Built from all sixteen, so the constructor's own `setNumber3D` infers
  // `is2D` from what is actually there rather than from which branch ran.
  const matrix = new DOMMatrix([
    cell("m11", "a", 1),
    cell("m12", "b", 0),
    cell("m13", undefined, 0),
    cell("m14", undefined, 0),
    cell("m21", "c", 0),
    cell("m22", "d", 1),
    cell("m23", undefined, 0),
    cell("m24", undefined, 0),
    cell("m31", undefined, 0),
    cell("m32", undefined, 0),
    cell("m33", undefined, 1),
    cell("m34", undefined, 0),
    cell("m41", "e", 0),
    cell("m42", "f", 0),
    cell("m43", undefined, 0),
    cell("m44", undefined, 1),
  ]);

  // `is2D: false` on a matrix with no 3D cell is the caller saying to treat
  // it as 3D anyway, which the inference cannot know.
  if (init.is2D === false) matrix[$is2D] = false;

  return matrix;
};

let newInstance = (values) => {
  let instance = Object.create(DOMMatrix.prototype);
  instance.constructor = DOMMatrix;
  instance[$is2D] = true;
  instance[$values] = values;

  return instance;
};

let multiply = (first, second) => {
  let dest = new Float64Array(16);

  for (let i = 0; i < 4; i++) {
    for (let j = 0; j < 4; j++) {
      let sum = 0;

      for (let k = 0; k < 4; k++) {
        sum += first[i * 4 + k] * second[k * 4 + j];
      }

      dest[i * 4 + j] = sum;
    }
  }

  return dest;
};

class DOMMatrix {
  get m11() {
    return this[$values][M11];
  }
  set m11(value) {
    setNumber2D(this, M11, value);
  }
  get m12() {
    return this[$values][M12];
  }
  set m12(value) {
    setNumber2D(this, M12, value);
  }
  get m13() {
    return this[$values][M13];
  }
  set m13(value) {
    setNumber3D(this, M13, value);
  }
  get m14() {
    return this[$values][M14];
  }
  set m14(value) {
    setNumber3D(this, M14, value);
  }
  get m21() {
    return this[$values][M21];
  }
  set m21(value) {
    setNumber2D(this, M21, value);
  }
  get m22() {
    return this[$values][M22];
  }
  set m22(value) {
    setNumber2D(this, M22, value);
  }
  get m23() {
    return this[$values][M23];
  }
  set m23(value) {
    setNumber3D(this, M23, value);
  }
  get m24() {
    return this[$values][M24];
  }
  set m24(value) {
    setNumber3D(this, M24, value);
  }
  get m31() {
    return this[$values][M31];
  }
  set m31(value) {
    setNumber3D(this, M31, value);
  }
  get m32() {
    return this[$values][M32];
  }
  set m32(value) {
    setNumber3D(this, M32, value);
  }
  get m33() {
    return this[$values][M33];
  }
  set m33(value) {
    setNumber3D(this, M33, value);
  }
  get m34() {
    return this[$values][M34];
  }
  set m34(value) {
    setNumber3D(this, M34, value);
  }
  get m41() {
    return this[$values][M41];
  }
  set m41(value) {
    setNumber2D(this, M41, value);
  }
  get m42() {
    return this[$values][M42];
  }
  set m42(value) {
    setNumber2D(this, M42, value);
  }
  get m43() {
    return this[$values][M43];
  }
  set m43(value) {
    setNumber3D(this, M43, value);
  }
  get m44() {
    return this[$values][M44];
  }
  set m44(value) {
    setNumber3D(this, M44, value);
  }

  get a() {
    return this[$values][A];
  }
  set a(value) {
    setNumber2D(this, A, value);
  }
  get b() {
    return this[$values][B];
  }
  set b(value) {
    setNumber2D(this, B, value);
  }
  get c() {
    return this[$values][C];
  }
  set c(value) {
    setNumber2D(this, C, value);
  }
  get d() {
    return this[$values][D];
  }
  set d(value) {
    setNumber2D(this, D, value);
  }
  get e() {
    return this[$values][E];
  }
  set e(value) {
    setNumber2D(this, E, value);
  }
  get f() {
    return this[$values][F];
  }
  set f(value) {
    setNumber2D(this, F, value);
  }

  get is2D() {
    return this[$is2D];
  }

  get isIdentity() {
    let values = this[$values];

    return (
      values[M11] === 1 &&
      values[M12] === 0 &&
      values[M13] === 0 &&
      values[M14] === 0 &&
      values[M21] === 0 &&
      values[M22] === 1 &&
      values[M23] === 0 &&
      values[M24] === 0 &&
      values[M31] === 0 &&
      values[M32] === 0 &&
      values[M33] === 1 &&
      values[M34] === 0 &&
      values[M41] === 0 &&
      values[M42] === 0 &&
      values[M43] === 0 &&
      values[M44] === 1
    );
  }

  static fromMatrix(init = {}) {
    if (init instanceof DOMMatrix) return new DOMMatrix(init[$values]);
    if (DOMMatrix.isMatrix4(init))
      return new DOMMatrix([
        init.m11,
        init.m12,
        init.m13,
        init.m14,
        init.m21,
        init.m22,
        init.m23,
        init.m24,
        init.m31,
        init.m32,
        init.m33,
        init.m34,
        init.m41,
        init.m42,
        init.m43,
        init.m44,
      ]);
    if (DOMMatrix.isMatrix3(init) || isPlainObject(init))
      return fromDictionary(init);
    throw new TypeError(`Expected DOMMatrix, got: '${init}'`);
  }

  static fromFloat32Array(init) {
    if (!(init instanceof Float32Array))
      throw new TypeError("Expected Float32Array");
    return new DOMMatrix(init);
  }

  static fromFloat64Array(init) {
    if (!(init instanceof Float64Array))
      throw new TypeError("Expected Float64Array");
    return new DOMMatrix(init);
  }

  static isMatrix3(matrix) {
    if (matrix instanceof DOMMatrix) return true;
    if (typeof matrix != "object") return false;
    for (const p of ["a", "b", "c", "d", "e", "f"])
      if (typeof matrix[p] != "number") return false;
    return true;
  }

  static isMatrix4(matrix) {
    if (matrix instanceof DOMMatrix) return true;
    if (typeof matrix != "object") return false;
    for (const p of [
      "m11",
      "m12",
      "m13",
      "m14",
      "m21",
      "m22",
      "m23",
      "m24",
      "m31",
      "m32",
      "m33",
      "m34",
      "m41",
      "m42",
      "m43",
      "m44",
    ]) {
      if (typeof matrix[p] != "number") return false;
    }
    return true;
  }

  // @type
  // (Float64Array) => void
  constructor(init) {
    if (init instanceof DOMMatrix || isPlainObject(init))
      return DOMMatrix.fromMatrix(init);

    if (arguments.length > 1) init = [...arguments];

    this[$is2D] = true;

    this[$values] = new Float64Array([
      1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1,
    ]);

    // Parse CSS transformList and accumulate transforms sequentially
    if (typeof init === "string") {
      if (init === "") return;

      let acc = new DOMMatrix();
      for (const { op, vals } of parseTransformString(init)) {
        acc = op.startsWith("matrix")
          ? acc.multiply(new DOMMatrix(vals))
          : acc[op]
            ? acc[op](...vals)
            : acc;
      }

      init = acc[$values];
    }

    let i = 0;

    if (init && init.length === 6) {
      setNumber2D(this, A, init[i++]);
      setNumber2D(this, B, init[i++]);
      setNumber2D(this, C, init[i++]);
      setNumber2D(this, D, init[i++]);
      setNumber2D(this, E, init[i++]);
      setNumber2D(this, F, init[i]);
    } else if (init && init.length === 16) {
      setNumber2D(this, M11, init[i++]);
      setNumber2D(this, M12, init[i++]);
      setNumber3D(this, M13, init[i++]);
      setNumber3D(this, M14, init[i++]);
      setNumber2D(this, M21, init[i++]);
      setNumber2D(this, M22, init[i++]);
      setNumber3D(this, M23, init[i++]);
      setNumber3D(this, M24, init[i++]);
      setNumber3D(this, M31, init[i++]);
      setNumber3D(this, M32, init[i++]);
      setNumber3D(this, M33, init[i++]);
      setNumber3D(this, M34, init[i++]);
      setNumber2D(this, M41, init[i++]);
      setNumber2D(this, M42, init[i++]);
      setNumber3D(this, M43, init[i++]);
      setNumber3D(this, M44, init[i]);
    } else if (init !== undefined) {
      throw new TypeError("Expected string, array, or matrix object.");
    }
  }

  dump() {
    let mat = this[$values];
    console.log([
      mat.slice(0, 4),
      mat.slice(4, 8),
      mat.slice(8, 12),
      mat.slice(12, 16),
    ]);
  }

  [inspect.custom](depth, options) {
    if (depth < 0) return "[DOMMatrix]";

    let { a, b, c, d, e, f, is2D, isIdentity } = this;
    if (this.is2D) {
      return `DOMMatrix ${inspect({ a, b, c, d, e, f, is2D, isIdentity }, { colors: true })}`;
    } else {
      let {
        m11,
        m12,
        m13,
        m14,
        m21,
        m22,
        m23,
        m24,
        m31,
        m32,
        m33,
        m34,
        m41,
        m42,
        m43,
        m44,
        is2D,
        isIdentity,
      } = this;
      return `DOMMatrix ${inspect({ a, b, c, d, e, f, m11, m12, m13, m14, m21, m22, m23, m24, m31, m32, m33, m34, m41, m42, m43, m44, is2D, isIdentity }, { colors: true })}`;
    }
  }

  multiply(other) {
    return newInstance(this[$values]).multiplySelf(other);
  }

  multiplySelf(other) {
    other = asMatrix(other);
    this[$values] = multiply(other[$values], this[$values]);

    if (!other.is2D) {
      this[$is2D] = false;
    }

    return this;
  }

  preMultiplySelf(other) {
    other = asMatrix(other);
    this[$values] = multiply(this[$values], other[$values]);

    if (!other.is2D) {
      this[$is2D] = false;
    }

    return this;
  }

  translate(tx, ty, tz) {
    return newInstance(this[$values]).translateSelf(tx, ty, tz);
  }

  translateSelf(tx = 0, ty = 0, tz = 0) {
    this[$values] = multiply(
      [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, tx, ty, tz, 1],
      this[$values],
    );

    if (tz !== 0) {
      this[$is2D] = false;
    }

    return this;
  }

  scale(scaleX, scaleY, scaleZ, originX, originY, originZ) {
    return newInstance(this[$values]).scaleSelf(
      scaleX,
      scaleY,
      scaleZ,
      originX,
      originY,
      originZ,
    );
  }

  scale3d(scale, originX, originY, originZ) {
    return newInstance(this[$values]).scale3dSelf(
      scale,
      originX,
      originY,
      originZ,
    );
  }

  scale3dSelf(scale, originX, originY, originZ) {
    return this.scaleSelf(scale, scale, scale, originX, originY, originZ);
  }

  scaleSelf(scaleX, scaleY, scaleZ, originX, originY, originZ) {
    // Not redundant with translate's checks because we need to negate the values later.
    if (typeof originX !== "number") originX = 0;
    if (typeof originY !== "number") originY = 0;
    if (typeof originZ !== "number") originZ = 0;

    this.translateSelf(originX, originY, originZ);

    if (typeof scaleX !== "number") scaleX = 1;
    if (typeof scaleY !== "number") scaleY = scaleX;
    if (typeof scaleZ !== "number") scaleZ = 1;

    this[$values] = multiply(
      [scaleX, 0, 0, 0, 0, scaleY, 0, 0, 0, 0, scaleZ, 0, 0, 0, 0, 1],
      this[$values],
    );

    this.translateSelf(-originX, -originY, -originZ);

    if (scaleZ !== 1 || originZ !== 0) {
      this[$is2D] = false;
    }

    return this;
  }

  rotateFromVector(x, y) {
    return newInstance(this[$values]).rotateFromVectorSelf(x, y);
  }

  rotateFromVectorSelf(x = 0, y = 0) {
    let theta = x === 0 && y === 0 ? 0 : Math.atan2(y, x) * DEGREE_PER_RAD;
    return this.rotateSelf(theta);
  }

  rotate(rotX, rotY, rotZ) {
    return newInstance(this[$values]).rotateSelf(rotX, rotY, rotZ);
  }

  rotateSelf(rotX, rotY, rotZ) {
    if (rotY === undefined && rotZ === undefined) {
      rotZ = rotX;
      rotX = rotY = 0;
    }

    if (typeof rotY !== "number") rotY = 0;
    if (typeof rotZ !== "number") rotZ = 0;

    if (rotX !== 0 || rotY !== 0) {
      this[$is2D] = false;
    }

    rotX *= RAD_PER_DEGREE;
    rotY *= RAD_PER_DEGREE;
    rotZ *= RAD_PER_DEGREE;

    let c = Math.cos(rotZ);
    let s = Math.sin(rotZ);

    this[$values] = multiply(
      [c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
      this[$values],
    );

    c = Math.cos(rotY);
    s = Math.sin(rotY);

    this[$values] = multiply(
      [c, 0, -s, 0, 0, 1, 0, 0, s, 0, c, 0, 0, 0, 0, 1],
      this[$values],
    );

    c = Math.cos(rotX);
    s = Math.sin(rotX);

    this[$values] = multiply(
      [1, 0, 0, 0, 0, c, s, 0, 0, -s, c, 0, 0, 0, 0, 1],
      this[$values],
    );

    return this;
  }

  rotateAxisAngle(x, y, z, angle) {
    return newInstance(this[$values]).rotateAxisAngleSelf(x, y, z, angle);
  }

  rotateAxisAngleSelf(x = 0, y = 0, z = 0, angle = 0) {
    let length = Math.sqrt(x * x + y * y + z * z);

    if (length === 0) {
      return this;
    }

    if (length !== 1) {
      x /= length;
      y /= length;
      z /= length;
    }

    angle *= RAD_PER_DEGREE;

    let c = Math.cos(angle);
    let s = Math.sin(angle);
    let t = 1 - c;
    let tx = t * x;
    let ty = t * y;

    this[$values] = multiply(
      [
        tx * x + c,
        tx * y + s * z,
        tx * z - s * y,
        0,
        tx * y - s * z,
        ty * y + c,
        ty * z + s * x,
        0,
        tx * z + s * y,
        ty * z - s * x,
        t * z * z + c,
        0,
        0,
        0,
        0,
        1,
      ],
      this[$values],
    );

    if (x !== 0 || y !== 0) {
      this[$is2D] = false;
    }

    return this;
  }

  skew(sx, sy) {
    return newInstance(this[$values]).skewSelf(sx, sy);
  }

  skewSelf(sx, sy) {
    if (typeof sx !== "number" && typeof sy !== "number") {
      return this;
    }

    let x = isNaN(sx) ? 0 : Math.tan(sx * RAD_PER_DEGREE);
    let y = isNaN(sy) ? 0 : Math.tan(sy * RAD_PER_DEGREE);

    this[$values] = multiply(
      [1, y, 0, 0, x, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
      this[$values],
    );

    return this;
  }

  skewX(sx) {
    return newInstance(this[$values]).skewXSelf(sx);
  }

  skewXSelf(sx) {
    if (typeof sx !== "number") {
      return this;
    }

    let t = Math.tan(sx * RAD_PER_DEGREE);

    this[$values] = multiply(
      [1, 0, 0, 0, t, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
      this[$values],
    );

    return this;
  }

  skewY(sy) {
    return newInstance(this[$values]).skewYSelf(sy);
  }

  skewYSelf(sy) {
    if (typeof sy !== "number") {
      return this;
    }

    let t = Math.tan(sy * RAD_PER_DEGREE);

    this[$values] = multiply(
      [1, t, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
      this[$values],
    );

    return this;
  }

  flipX() {
    return newInstance(
      multiply(
        [-1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
        this[$values],
      ),
    );
  }

  flipY() {
    return newInstance(
      multiply(
        [1, 0, 0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
        this[$values],
      ),
    );
  }

  inverse() {
    return newInstance(this[$values]).invertSelf();
  }

  invertSelf() {
    if (this[$is2D]) {
      let det =
        this[$values][A] * this[$values][D] -
        this[$values][B] * this[$values][C];

      // Invertable
      if (det !== 0) {
        // Computed into locals first: every term reads the pre-inversion
        // values, so assigning as we go would feed already-overwritten
        // numbers into the later terms.
        let [a, b, c, d, e, f] = [
          this[$values][D] / det,
          -this[$values][B] / det,
          -this[$values][C] / det,
          this[$values][A] / det,
          (this[$values][C] * this[$values][F] -
            this[$values][D] * this[$values][E]) /
            det,
          (this[$values][B] * this[$values][E] -
            this[$values][A] * this[$values][F]) /
            det,
        ];

        // Replace the values array rather than writing through the a/b/c/d
        // setters: `newInstance` shares its argument by reference, so an
        // in-place write here would reach back into the matrix `inverse()`
        // was called on. Every other *Self method replaces the array too.
        let values = this[$values].slice();
        [values[A], values[B], values[C], values[D], values[E], values[F]] = [
          a,
          b,
          c,
          d,
          e,
          f,
        ];
        this[$values] = values;

        return this;
      }

      // Not invertable
      else {
        this[$is2D] = false;

        this[$values] = [
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
          NaN,
        ];

        return this;
      }
    } else {
      throw new Error("3D matrix inversion is not implemented.");
    }
  }

  setMatrixValue(transformList) {
    let temp = new DOMMatrix(transformList);

    this[$values] = temp[$values];
    this[$is2D] = temp[$is2D];

    return this;
  }

  transformPoint(point = {}) {
    // Every DOMPointInit field is optional: x/y/z default to 0 and w to 1.
    // Reading them raw turned `{x, y}` -- the overwhelmingly common call --
    // into an all-NaN point.
    let { x = 0, y = 0, z = 0, w = 1 } = point;

    let values = this[$values];

    let nx =
      values[M11] * x + values[M21] * y + values[M31] * z + values[M41] * w;
    let ny =
      values[M12] * x + values[M22] * y + values[M32] * z + values[M42] * w;
    let nz =
      values[M13] * x + values[M23] * y + values[M33] * z + values[M43] * w;
    let nw =
      values[M14] * x + values[M24] * y + values[M34] * z + values[M44] * w;

    return new DOMPoint(nx, ny, nz, nw);
  }

  toFloat32Array() {
    return Float32Array.from(this[$values]);
  }

  toFloat64Array() {
    return this[$values].slice(0);
  }

  toJSON() {
    return {
      a: this.a,
      b: this.b,
      c: this.c,
      d: this.d,
      e: this.e,
      f: this.f,
      m11: this.m11,
      m12: this.m12,
      m13: this.m13,
      m14: this.m14,
      m21: this.m21,
      m22: this.m22,
      m23: this.m23,
      m24: this.m24,
      m31: this.m31,
      m32: this.m32,
      m33: this.m33,
      m34: this.m34,
      m41: this.m41,
      m42: this.m42,
      m43: this.m43,
      m44: this.m44,
      is2D: this.is2D,
      isIdentity: this.isIdentity,
    };
  }

  toString() {
    let name = this.is2D ? "matrix" : "matrix3d";
    let values = this.is2D
      ? [this.a, this.b, this.c, this.d, this.e, this.f]
      : this[$values];
    let simplify = (n) =>
      n
        .toFixed(12)
        .replace(/\.([^0])?0*$/, ".$1")
        .replace(/\.$/, "")
        .replace(/^-0$/, "0");
    return `${name}(${values.map(simplify).join(", ")})`;
  }

  clone() {
    return new DOMMatrix(this);
  }
}

for (let propertyName of [
  "a",
  "b",
  "c",
  "d",
  "e",
  "f",
  "m11",
  "m12",
  "m13",
  "m14",
  "m21",
  "m22",
  "m23",
  "m24",
  "m31",
  "m32",
  "m33",
  "m34",
  "m41",
  "m42",
  "m43",
  "m44",
  "is2D",
  "isIdentity",
]) {
  let propertyDescriptor = Object.getOwnPropertyDescriptor(
    DOMMatrix.prototype,
    propertyName,
  );
  propertyDescriptor.enumerable = true;
  Object.defineProperty(DOMMatrix.prototype, propertyName, propertyDescriptor);
}

//
// Helpers to reconcile Skia and DOMMatrix’s disagreement about row/col orientation
//

function toSkMatrix() {
  if (arguments.length != 1 && arguments.length < 6) {
    throw new TypeError("not enough arguments");
  }
  try {
    const m = new DOMMatrix(...arguments);
    return [m.a, m.c, m.e, m.b, m.d, m.f, m.m14, m.m24, m.m44];
  } catch (e) {
    throw new TypeError(`Invalid transform matrix argument(s): ` + e, {
      cause: e,
    });
  }
}

function fromSkMatrix(skMatrix) {
  let [a, b, c, d, e, f, p0, p1, p2] = skMatrix;
  return new DOMMatrix([a, d, 0, p0, b, e, 0, p1, 0, 0, 1, 0, c, f, 0, p2]);
}

module.exports = { DOMPoint, DOMMatrix, DOMRect, toSkMatrix, fromSkMatrix };
