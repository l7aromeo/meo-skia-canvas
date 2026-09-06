//
// Bézier paths
//

"use strict";

const {
    RustClass,
    core,
    wrap,
    inspect,
    argc,
    REPR,
    neon,
    ALLOC,
    INIT,
    PROP,
    CALL,
  } = require("./neon"),
  drawlist = require("./drawlist"),
  { toSkMatrix } = require("./geometry"),
  css = require("./css");

/** Whether every value is a number a record can hold. */
const everyFinite = (values) =>
  Array.prototype.every.call(
    values,
    (v) => typeof v === "number" && isFinite(v),
  );

// The dispatch behind the boolean operations and the path effects. Module
// scope rather than statics on the class: they were reachable as
// `Path2D.op(...)`, `Path2D.interpolate(...)` and `Path2D.effect(...)` while
// being declared nowhere, so a caller could find them by introspection and
// depend on a shape nothing promises. The instance methods below are the
// surface.
const op = (operation, path, other) => {
  let args = other ? [core(other), operation] : [];
  return wrap(Path2D, path[CALL]("op", ...args));
};

const interpolate = (path, other, weight) => {
  let args = other ? [core(other), weight] : [];
  return wrap(Path2D, path[CALL]("interpolate", ...args));
};

const effect = (kind, path, ...args) => wrap(Path2D, path[CALL](kind, ...args));

class Path2D extends RustClass {
  constructor(source) {
    super(Path2D);
    if (source instanceof Path2D) this[INIT]("from_path", core(source));
    else if (typeof source == "string") this[INIT]("from_svg", source);
    else this[ALLOC]();
  }

  // dimensions & contents
  get bounds() {
    return this[CALL]("bounds");
  }
  get edges() {
    return this[CALL]("edges");
  }
  get d() {
    return this[PROP]("d");
  }
  set d(svg) {
    this[PROP]("d", svg);
  }
  contains(x, y) {
    return this[CALL]("contains", ...arguments);
  }

  points(step = 1) {
    return this.jitter(step, 0)
      .edges.map(([verb, ...pts]) => pts.slice(-2))
      .filter((pt) => pt.length);
  }

  // concatenation
  addPath(path, matrix) {
    if (path instanceof Path2D && !matrix) {
      const write = drawlist.writerFor(Path2D, "appendPath");
      if (write) return write.call(this, path);
    }
    let args = path instanceof Path2D ? [core(path)] : [];
    if (matrix) args.push(toSkMatrix(matrix));
    this[CALL]("addPath", ...args);
  }

  // line segments
  moveTo(x, y) {
    this[CALL]("moveTo", ...arguments);
  }
  lineTo(x, y) {
    this[CALL]("lineTo", ...arguments);
  }
  closePath() {
    this[CALL]("closePath");
  }
  arcTo(x1, y1, x2, y2, radius) {
    this[CALL]("arcTo", ...arguments);
  }
  bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y) {
    this[CALL]("bezierCurveTo", ...arguments);
  }
  quadraticCurveTo(cpx, cpy, x, y) {
    this[CALL]("quadraticCurveTo", ...arguments);
  }
  conicCurveTo(cpx, cpy, x, y, weight) {
    this[CALL]("conicCurveTo", ...arguments);
  }

  // shape primitives
  ellipse(x, y, radiusX, radiusY, rotation, startAngle, endAngle, isCCW) {
    this[CALL]("ellipse", ...arguments);
  }
  rect(x, y, width, height) {
    this[CALL]("rect", ...arguments);
  }
  arc(x, y, radius, startAngle, endAngle) {
    this[CALL]("arc", ...arguments);
  }
  roundRect(x, y, w, h, r = 0) {
    argc(arguments, 4, 5);
    // One radius for all four corners is the shape a record can hold, and
    // flipping the corners for a negative width or height below is a no-op
    // when they are all the same. A negative radius goes the long way so
    // that it is refused by name rather than by the record being dropped.
    if (typeof r === "number" && r >= 0 && everyFinite([x, y, w, h, r])) {
      const write = drawlist.writerFor(Path2D, "roundRectUniform");
      if (write) return write.call(this, x, y, w, h, r);
    }
    let radii = css.radii(r);
    if (radii) {
      if (w < 0) radii = [radii[1], radii[0], radii[3], radii[2]];
      if (h < 0) radii = [radii[3], radii[2], radii[1], radii[0]];
      this[CALL](
        "roundRect",
        x,
        y,
        w,
        h,
        ...radii.map(({ x, y }) => [x, y]).flat(),
      );
    }
  }

  // tween similar paths
  interpolate(path, weight) {
    return interpolate(this, ...arguments);
  }

  // boolean operations
  complement(path) {
    return op("complement", this, ...arguments);
  }
  difference(path) {
    return op("difference", this, ...arguments);
  }
  intersect(path) {
    return op("intersect", this, ...arguments);
  }
  union(path) {
    return op("union", this, ...arguments);
  }
  xor(path) {
    return op("xor", this, ...arguments);
  }

  // path effects
  jitter(len, amt, seed) {
    return effect("jitter", this, ...arguments);
  }
  simplify(rule) {
    return effect("simplify", this, ...arguments);
  }
  unwind() {
    return effect("unwind", this);
  }
  round(radius) {
    return effect("round", this, ...arguments);
  }
  offset(dx, dy) {
    return effect("offset", this, ...arguments);
  }

  transform(matrix) {
    return effect("transform", this, toSkMatrix.apply(null, arguments));
  }

  trim(...rng) {
    if (typeof rng[1] != "number") {
      if (rng[0] > 0) rng.unshift(0);
      else if (rng[0] < 0) rng.splice(1, 0, 1);
    }
    if (rng[0] < 0) rng[0] = Math.max(-1, rng[0]) + 1;
    if (rng[1] < 0) rng[1] = Math.max(-1, rng[1]) + 1;
    return effect("trim", this, ...rng);
  }

  [REPR](depth, options) {
    let { d, bounds, edges } = this;
    return `Path2D ${inspect({ d, bounds, edges }, options)}`;
  }
}

// The verbs Rust declares, recorded rather than called. Everything else on
// this class still crosses immediately: a verb taking a path, a string or a
// sequence has nowhere to go in a buffer of numbers, and a read has to be
// answered now.
drawlist.install(
  Path2D,
  neon.Path2D.verbTable(),
  // Three arguments where the batch named nothing a number cannot
  // hold, which is most of them. The fourth is walked on arrival
  // whether or not it holds anything, and the crossing is what a
  // flush costs -- see `plot` in `node::verbs`.
  (target, buffer, length, slots) =>
    slots.length
      ? neon.Path2D.plot(drawlist.rawHandle(target), buffer, length, slots)
      : neon.Path2D.plot(drawlist.rawHandle(target), buffer, length),
);

module.exports = { Path2D };
