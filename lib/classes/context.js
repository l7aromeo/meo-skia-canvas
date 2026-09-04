//
// The Canvas drawing API
//

"use strict";

const { RustClass, core, wrap, inspect, argc, REPR, neon } = require("./neon"),
  drawlist = require("./drawlist"),
  {
    Canvas,
    CanvasGradient,
    CanvasPattern,
    CanvasTexture,
  } = require("./canvas"),
  { DOMMatrix, fromSkMatrix, toSkMatrix } = require("./geometry"),
  { Image, ImageData } = require("./imagery"),
  { textMetrics, Paragraph } = require("./typography"),
  { Path2D } = require("./path"),
  { ColorFilter, ImageFilter, MaskFilter, Shader } = require("./filter"),
  css = require("./css");

const toString = (val) =>
  typeof val == "string" ? val : new String(val).toString();

// The caps axis of `font-variant`, which `fontVariantCaps` reads and writes.
// "normal" is deliberately absent: it means "no caps token", so it is handled
// by removing one rather than by matching.
const CAPS = new Set([
  "small-caps",
  "all-small-caps",
  "petite-caps",
  "all-petite-caps",
  "unicase",
  "titling-caps",
]);

/** Whether every value is a number a record can hold. */
const everyFinite = (values) =>
  Array.prototype.every.call(
    values,
    (v) => typeof v === "number" && isFinite(v),
  );

// The six numbers a matrix is worth, or `null` if it is not that kind of
// matrix.
//
// A batch carries numbers, so a `DOMMatrix` used to cross as an object even
// though almost every one is a plain 2D transform that six of its fields
// describe completely. `setTransformNumbers` and `transformNumbers` already
// build `Matrix::new_all(a, c, e, b, d, f, 0, 0, 1)`, so a matrix whose
// projective row is that can go the same way -- and `toSkMatrix` is skipped
// with it, which was constructing a second `DOMMatrix` from the one it had
// just been handed.
//
// Anything else -- a projective matrix, a matrix-like object, a CSS string,
// a non-finite field -- falls through to the crossing it always took.
const finiteSix = (a, b, c, d, e, f) =>
  typeof a === "number" &&
  typeof b === "number" &&
  typeof c === "number" &&
  typeof d === "number" &&
  typeof e === "number" &&
  typeof f === "number" &&
  isFinite(a) &&
  isFinite(b) &&
  isFinite(c) &&
  isFinite(d) &&
  isFinite(e) &&
  isFinite(f);

const affineSix = (m) =>
  m instanceof DOMMatrix &&
  m.m14 === 0 &&
  m.m24 === 0 &&
  m.m44 === 1 &&
  finiteSix(m.a, m.b, m.c, m.d, m.e, m.f)
    ? [m.a, m.b, m.c, m.d, m.e, m.f]
    : null;

// The recorded `drawImage` for each count of coordinates that places a
// source. Any other count has an error of its own to raise, which only the
// hand-written call can raise, so it is left to go the long way.
const PLACEMENTS = {
  2: "drawImageAt",
  4: "drawImageIn",
  8: "drawImageCropped",
};

/**
 * Records a `drawImage`, or answers false when this shape cannot be recorded.
 *
 * An `ImageData` never reaches here: its pixels are a JavaScript array, and
 * a record resolves what it points at when the batch lands, so a caller
 * could change them in between without crossing anything that would hand the
 * batch over first.
 */
function place(ctx, source, coords) {
  const write =
    PLACEMENTS[coords.length] &&
    drawlist.writerFor(CanvasRenderingContext2D, PLACEMENTS[coords.length]);
  if (!write) return false;
  write.call(ctx, source, ...coords);
  return true;
}

/**
 * Records a `fillText` or a `strokeText`, or answers false for a shape that
 * cannot be recorded.
 *
 * Two shapes: a position, and a position with a width to fit into. A fourth
 * argument of `undefined` is the one the call treats as absent, and a record
 * cannot -- an unusable number makes the decoder drop the whole record, so
 * the text would not be drawn at all. That one goes the long way, as does
 * anything longer, which the call reads past.
 */
function writeText(ctx, style, string, geom) {
  const shape =
    geom.length === 2
      ? "At"
      : geom.length === 3 && geom[2] !== undefined
        ? "In"
        : null;
  const write =
    shape &&
    drawlist.writerFor(CanvasRenderingContext2D, style + "Text" + shape);
  if (!write) return false;
  write.call(ctx, string, ...geom);
  return true;
}

class CanvasRenderingContext2D extends RustClass {
  #canvas;
  #stateStack = []; // Track JS-side refs for save/restore

  constructor(canvas) {
    try {
      super(CanvasRenderingContext2D).alloc(core(canvas));
      this.#canvas = new WeakRef(canvas);
    } catch (e) {
      throw new TypeError(
        `Function is not a constructor (use Canvas's "getContext" method instead)`,
        { cause: e },
      );
    }
  }

  get canvas() {
    return this.#canvas.deref();
  }

  // Always false. Context loss is a GPU-compositor event -- the browser
  // reclaiming a backing store from a backgrounded tab -- and there is no
  // compositor here. A canvas either has its surface or construction failed.
  isContextLost() {
    return false;
  }

  // -- global state & content reset ------------------------------------------
  reset() {
    this.ƒ("reset");
    // Clear JS-side state stack and refs
    this.#stateStack.length = 0;
    this.ref("colorFilter", null);
    this.ref("imageFilter", null);
    this.ref("maskFilter", null);
    this.ref("fill", null);
    this.ref("stroke", null);
  }

  // -- grid state ------------------------------------------------------------
  save() {
    this.ƒ("save");
    // Push current JS refs onto stack (for shaders and filters)
    this.#stateStack.push({
      colorFilter: this.ref("colorFilter") ?? null,
      imageFilter: this.ref("imageFilter") ?? null,
      maskFilter: this.ref("maskFilter") ?? null,
      fill: this.ref("fill") ?? null,
      stroke: this.ref("stroke") ?? null,
    });
  }
  restore() {
    this.ƒ("restore");
    // Pop JS refs from stack
    const state = this.#stateStack.pop();
    if (state) {
      this.ref("colorFilter", state.colorFilter);
      this.ref("imageFilter", state.imageFilter);
      this.ref("maskFilter", state.maskFilter);
      this.ref("fill", state.fill);
      this.ref("stroke", state.stroke);
    }
  }
  saveLayer(alpha = 1, bounds = null, backdrop = null) {
    if (backdrop !== null && !(backdrop instanceof ImageFilter)) {
      throw new TypeError("saveLayer backdrop must be an ImageFilter or null");
    }
    if (backdrop?._deleted) {
      throw new Error("ImageFilter has been deleted");
    }
    // Only the shape with nothing but an alpha, and only when that alpha is
    // a number: a dropped record here would leave no layer for the matching
    // `restore` below to pop, where a dropped drawing verb only fails to
    // paint.
    const write =
      bounds === null &&
      backdrop === null &&
      everyFinite([alpha]) &&
      drawlist.writerFor(CanvasRenderingContext2D, "saveLayerAlpha");
    if (write) write.call(this, alpha);
    else this.ƒ("saveLayer", alpha, bounds, backdrop ? core(backdrop) : null);
    // saveLayer opens a save frame that restore() pops -- mirror save()'s
    // JS ref snapshot so the cached filter/style refs unwind in step.
    this.#stateStack.push({
      colorFilter: this.ref("colorFilter") ?? null,
      imageFilter: this.ref("imageFilter") ?? null,
      maskFilter: this.ref("maskFilter") ?? null,
      fill: this.ref("fill") ?? null,
      stroke: this.ref("stroke") ?? null,
    });
  }

  get currentTransform() {
    return fromSkMatrix(this.prop("currentTransform"));
  }
  set currentTransform(matrix) {
    this.setTransform(matrix);
  }

  resetTransform() {
    this.ƒ("resetTransform");
  }
  getTransform() {
    return this.currentTransform;
  }
  setTransform(matrix) {
    // Per spec a bare `setTransform()` resets to the identity matrix. Passing
    // zero arguments straight to `toSkMatrix` threw "not enough arguments".
    if (arguments.length == 0) return this.resetTransform();
    if (arguments.length === 6) {
      // Indexed rather than destructured or spread: both of those walk
      // `arguments` through its iterator, and that cost more than the
      // recorded write they were guarding.
      const a = arguments[0],
        b = arguments[1],
        c = arguments[2],
        d = arguments[3],
        e = arguments[4],
        f = arguments[5];
      if (finiteSix(a, b, c, d, e, f)) {
        const write = drawlist.writerFor(
          CanvasRenderingContext2D,
          "setTransformNumbers",
        );
        if (write) return write.call(this, a, b, c, d, e, f);
      }
    } else if (arguments.length === 1) {
      const six = affineSix(matrix);
      if (six) {
        const write = drawlist.writerFor(
          CanvasRenderingContext2D,
          "setTransformNumbers",
        );
        if (write)
          return write.call(
            this,
            six[0],
            six[1],
            six[2],
            six[3],
            six[4],
            six[5],
          );
      }
    }
    this.prop("currentTransform", toSkMatrix.apply(null, arguments));
  }

  transform(matrix) {
    // Six numbers is the form a batch can hold; a `DOMMatrix` or an array is
    // an object, and crosses as it always did.
    if (arguments.length === 6) {
      // Indexed rather than destructured or spread: both of those walk
      // `arguments` through its iterator, and that cost more than the
      // recorded write they were guarding.
      const a = arguments[0],
        b = arguments[1],
        c = arguments[2],
        d = arguments[3],
        e = arguments[4],
        f = arguments[5];
      if (finiteSix(a, b, c, d, e, f)) {
        const write = drawlist.writerFor(
          CanvasRenderingContext2D,
          "transformNumbers",
        );
        if (write) return write.call(this, a, b, c, d, e, f);
      }
    } else if (arguments.length === 1) {
      const six = affineSix(matrix);
      if (six) {
        const write = drawlist.writerFor(
          CanvasRenderingContext2D,
          "transformNumbers",
        );
        if (write)
          return write.call(
            this,
            six[0],
            six[1],
            six[2],
            six[3],
            six[4],
            six[5],
          );
      }
    }
    this.ƒ("transform", toSkMatrix.apply(null, arguments));
  }
  translate(x, y) {
    this.ƒ("translate", ...arguments);
  }
  scale(x, y) {
    this.ƒ("scale", ...arguments);
  }
  rotate(angle) {
    this.ƒ("rotate", ...arguments);
  }

  createProjection(quad, basis) {
    return fromSkMatrix(
      this.ƒ("createProjection", [quad].flat(), [basis].flat()),
    );
  }

  // -- bézier paths ----------------------------------------------------------
  beginPath() {
    this.ƒ("beginPath");
  }
  rect(x, y, width, height) {
    this.ƒ("rect", ...arguments);
  }
  arc(x, y, radius, startAngle, endAngle, isCCW) {
    this.ƒ("arc", ...arguments);
  }
  ellipse(x, y, xRadius, yRadius, rotation, startAngle, endAngle, isCCW) {
    this.ƒ("ellipse", ...arguments);
  }
  moveTo(x, y) {
    this.ƒ("moveTo", ...arguments);
  }
  lineTo(x, y) {
    this.ƒ("lineTo", ...arguments);
  }
  arcTo(x1, y1, x2, y2, radius) {
    this.ƒ("arcTo", ...arguments);
  }
  bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y) {
    this.ƒ("bezierCurveTo", ...arguments);
  }
  quadraticCurveTo(cpx, cpy, x, y) {
    this.ƒ("quadraticCurveTo", ...arguments);
  }
  conicCurveTo(cpx, cpy, x, y, weight) {
    this.ƒ("conicCurveTo", ...arguments);
  }
  closePath() {
    this.ƒ("closePath");
  }
  roundRect(x, y, w, h, r = 0) {
    argc(arguments, 4, 5);
    // One radius for all four corners is the shape a record can hold. An
    // array of radii, or a negative one, goes the long way -- the negative so
    // that it is refused by name rather than by the record being dropped.
    if (typeof r === "number" && r >= 0 && everyFinite([x, y, w, h, r])) {
      const write = drawlist.writerFor(
        CanvasRenderingContext2D,
        "roundRectUniform",
      );
      if (write) return write.call(this, x, y, w, h, r);
    }
    let radii = css.radii(r);
    if (radii) {
      if (w < 0) radii = [radii[1], radii[0], radii[3], radii[2]];
      if (h < 0) radii = [radii[3], radii[2], radii[1], radii[0]];
      this.ƒ(
        "roundRect",
        x,
        y,
        w,
        h,
        ...radii.map(({ x, y }) => [x, y]).flat(),
      );
    }
  }

  // -- using paths -----------------------------------------------------------
  fill(path, rule) {
    // Four call shapes and one record shape each, so the wrapper picks the
    // verb rather than the declaration trying to describe all of them.
    if (path instanceof Path2D) {
      // Only the two rules the API defines. Anything else has to reach the
      // hand-written path, which refuses it by name -- recording it would
      // make `fill(path, "bogus")` a silent winding fill.
      const write =
        (rule === undefined || rule === "nonzero" || rule === "evenodd") &&
        drawlist.writerFor(CanvasRenderingContext2D, "fillPath2D");
      if (write) return write.call(this, path, rule ?? "nonzero");
      arguments[0] = core(path);
    } else if (arguments.length === 0 || path === "nonzero") {
      const write = drawlist.writerFor(CanvasRenderingContext2D, "fillPage");
      if (write) return write.call(this);
    } else if (path === "evenodd") {
      const write = drawlist.writerFor(
        CanvasRenderingContext2D,
        "fillPageEvenOdd",
      );
      if (write) return write.call(this);
    }
    return this.ƒ("fill", ...arguments);
  }

  stroke(path) {
    if (path instanceof Path2D) {
      const write = drawlist.writerFor(
        CanvasRenderingContext2D,
        "strokePath2D",
      );
      if (write) return write.call(this, path);
      arguments[0] = core(path);
    } else if (arguments.length === 0) {
      const write = drawlist.writerFor(CanvasRenderingContext2D, "strokePage");
      if (write) return write.call(this);
    }
    return this.ƒ("stroke", ...arguments);
  }

  clip(path, rule) {
    // The same three shapes as `fill`, and the same rule about them: a
    // wrapper choosing a verb may narrow what a call accepts, never widen it.
    const defined =
      rule === undefined || rule === "nonzero" || rule === "evenodd";
    if (path instanceof Path2D) {
      const write =
        defined && drawlist.writerFor(CanvasRenderingContext2D, "clipPath2D");
      if (write) return write.call(this, path, rule ?? "nonzero");
      arguments[0] = core(path);
    } else if (arguments.length === 0 || path === "nonzero") {
      const write = drawlist.writerFor(CanvasRenderingContext2D, "clipPage");
      if (write) return write.call(this);
    } else if (path === "evenodd") {
      const write = drawlist.writerFor(
        CanvasRenderingContext2D,
        "clipPageEvenOdd",
      );
      if (write) return write.call(this);
    }
    return this.ƒ("clip", ...arguments);
  }

  isPointInPath(path, x, y, rule) {
    if (path instanceof Path2D) arguments[0] = core(path);
    return this.ƒ("isPointInPath", ...arguments);
  }
  isPointInStroke(path, x, y) {
    if (path instanceof Path2D) arguments[0] = core(path);
    return this.ƒ("isPointInStroke", ...arguments);
  }

  // -- shaders ---------------------------------------------------------------
  createPattern(image, repetition) {
    return new CanvasPattern(this.canvas, ...arguments);
  }
  createLinearGradient(x0, y0, x1, y1) {
    return new CanvasGradient("Linear", ...arguments);
  }
  createRadialGradient(x0, y0, r0, x1, y1, r1) {
    return new CanvasGradient("Radial", ...arguments);
  }
  // `endAngle` is this library's own, past the three arguments the Canvas
  // API defines: Skia sweeps any arc and the Rust API has always taken one,
  // so a partial sweep was reachable from Rust and not from here. Omitted,
  // it is the full turn a browser draws.
  createConicGradient(startAngle, x, y, endAngle) {
    return new CanvasGradient("Conic", ...arguments);
  }

  createTexture(spacing, options) {
    return new CanvasTexture(...arguments);
  }

  // -- fill & stroke ---------------------------------------------------------
  fillRect(x, y, width, height) {
    this.ƒ("fillRect", ...arguments);
  }
  strokeRect(x, y, width, height) {
    this.ƒ("strokeRect", ...arguments);
  }
  clearRect(x, y, width, height) {
    this.ƒ("clearRect", ...arguments);
  }

  set fillStyle(style) {
    let isShader =
        style instanceof CanvasPattern ||
        style instanceof CanvasGradient ||
        style instanceof CanvasTexture ||
        style instanceof Shader,
      [ref, val] = isShader ? [style, core(style)] : [null, style];
    this.ref("fill", ref);
    this.prop("fillStyle", val);
  }

  get fillStyle() {
    let style = this.prop("fillStyle");
    return style === null ? this.ref("fill") : style;
  }

  set strokeStyle(style) {
    let isShader =
        style instanceof CanvasPattern ||
        style instanceof CanvasGradient ||
        style instanceof CanvasTexture ||
        style instanceof Shader,
      [ref, val] = isShader ? [style, core(style)] : [null, style];
    this.ref("stroke", ref);
    this.prop("strokeStyle", val);
  }

  get strokeStyle() {
    let style = this.prop("strokeStyle");
    return style === null ? this.ref("stroke") : style;
  }

  // -- line style ------------------------------------------------------------
  getLineDash() {
    return this.ƒ("getLineDash");
  }
  setLineDash(segments) {
    this.ƒ("setLineDash", ...arguments);
  }
  get lineCap() {
    return this.prop("lineCap");
  }
  set lineCap(style) {
    this.prop("lineCap", style);
  }
  get lineDashFit() {
    return this.prop("lineDashFit");
  }
  set lineDashFit(style) {
    this.prop("lineDashFit", style);
  }
  get lineDashMarker() {
    return wrap(Path2D, this.prop("lineDashMarker"));
  }
  set lineDashMarker(path) {
    this.prop("lineDashMarker", path instanceof Path2D ? core(path) : path);
  }
  get lineDashOffset() {
    return this.prop("lineDashOffset");
  }
  set lineDashOffset(offset) {
    this.prop("lineDashOffset", offset);
  }
  get lineJoin() {
    return this.prop("lineJoin");
  }
  set lineJoin(style) {
    this.prop("lineJoin", style);
  }
  get lineWidth() {
    return this.prop("lineWidth");
  }
  set lineWidth(width) {
    this.prop("lineWidth", width);
  }
  get miterLimit() {
    return this.prop("miterLimit");
  }
  set miterLimit(limit) {
    this.prop("miterLimit", limit);
  }

  // -- imagery ---------------------------------------------------------------
  get imageSmoothingEnabled() {
    return this.prop("imageSmoothingEnabled");
  }
  set imageSmoothingEnabled(flag) {
    this.prop("imageSmoothingEnabled", !!flag);
  }
  get dither() {
    return this.prop("dither");
  }
  set dither(flag) {
    this.prop("dither", !!flag);
  }
  get imageSmoothingQuality() {
    return this.prop("imageSmoothingQuality");
  }
  set imageSmoothingQuality(level) {
    this.prop("imageSmoothingQuality", level);
  }

  createImageData(width, height, settings) {
    // The single-argument form clones an existing ImageData -- same
    // dimensions, blank pixels. `argc` rejects one argument, so the
    // spec-standard call threw.
    if (arguments.length == 1 && width instanceof ImageData) {
      return new ImageData(width.width, width.height, {
        colorType: width.colorType,
      });
    }
    argc(arguments, 2, 3);
    return new ImageData(width, height, settings);
  }

  getImageData(
    x,
    y,
    width,
    height,
    { colorType, colorSpace, density = 1, matte, msaa } = {},
  ) {
    argc(arguments, 4, 5);

    // Inherit the canvas's own format and space rather than hard-coding
    // "rgba" and "srgb", so a canvas built with either is honoured -- which
    // is what a browser does: `getImageData()` on a Display P3 canvas hands
    // back P3 data, and says so through `ImageData.colorSpace`. Resolved here
    // rather than passed through as undefined because the ImageData below
    // needs both to know the buffer's layout.
    colorType ??= this.canvas.colorType;
    colorSpace ??= this.canvas.colorSpace;

    if (
      typeof density != "number" ||
      !Number.isInteger(density) ||
      density < 1
    ) {
      throw new TypeError("Expected a non-negative integer for `density`");
    }

    if (msaa === undefined || msaa === true) {
      msaa = undefined; // use the default 4x msaa
    } else if (!isFinite(+msaa) || +msaa < 0) {
      throw new TypeError("The number of MSAA samples must be an integer ≥0");
    }

    let opts = { colorType, colorSpace, density, matte, msaa },
      buffer = this.ƒ(
        "getImageData",
        x,
        y,
        width,
        height,
        opts,
        core(this.canvas),
      );
    return new ImageData(buffer, width * density, height * density, {
      colorType,
      colorSpace,
    });
  }

  putImageData(imageData, ...coords) {
    argc(arguments, 3, 7);
    if (!(imageData instanceof ImageData))
      throw TypeError("Expected an ImageData as 1st arg");
    this.ƒ("putImageData", imageData, ...coords);
  }

  drawImage(image, ...coords) {
    if (image instanceof Canvas) {
      const source = image.getContext("2d");
      if (!place(this, source, coords))
        this.ƒ("drawImage", core(source), ...coords);
    } else if (image instanceof Image) {
      if (!image.complete)
        throw Error(
          "Image has not completed loading: listen for `load` event or await `decode()` first",
        );
      if (!place(this, image, coords))
        this.ƒ("drawImage", core(image), ...coords);
    } else if (image instanceof ImageData) {
      this.ƒ("drawImage", image, ...coords);
    } else if (image instanceof Promise) {
      throw Error(
        "Promise has not yet resolved: `await` image loading before drawing",
      );
    } else {
      let nonimage = inspect(image, { depth: 1 });
      throw Error(`Expected an Image or a Canvas argument (got: ${nonimage})`);
    }
  }

  drawCanvas(image, ...coords) {
    if (image instanceof Canvas) {
      this.ƒ("drawCanvas", core(image.getContext("2d")), ...coords);
    } else {
      this.drawImage(image, ...coords);
    }
  }

  // -- typography ------------------------------------------------------------
  get font() {
    return this.prop("font");
  }
  set font(str) {
    // The canonical string first, then the specification it came from.
    // Rust reads the string on its own and only goes on to the object the
    // first time it sees that name -- reading it is thirty times what
    // parsing the CSS costs, and the parse is already memoized here.
    const spec = css.font(str);
    this.prop("font", spec && spec.canonical, spec);
  }
  get textAlign() {
    return this.prop("textAlign");
  }
  set textAlign(mode) {
    this.prop("textAlign", mode);
  }
  get textBaseline() {
    return this.prop("textBaseline");
  }
  set textBaseline(mode) {
    this.prop("textBaseline", mode);
  }
  get direction() {
    return this.prop("direction");
  }
  set direction(mode) {
    this.prop("direction", mode);
  }
  get fontStretch() {
    return this.prop("fontStretch");
  }
  set fontStretch(str) {
    this.prop("fontStretch", css.stretch(str));
  }
  get letterSpacing() {
    return this.prop("letterSpacing");
  }
  set letterSpacing(str) {
    this.prop("letterSpacing", css.spacing(str));
  }
  get wordSpacing() {
    return this.prop("wordSpacing");
  }
  set wordSpacing(str) {
    this.prop("wordSpacing", css.spacing(str));
  }

  measureText(text, maxWidth) {
    // A buffer of numbers and an array of family names, assembled into the
    // object here. Building that object in Rust meant about forty property
    // writes, each one a call across the binding -- 4.6 microseconds of a
    // 9.4-microsecond call, against 3.5 for the typesetting it reports.
    return textMetrics(this.ƒ("measureText", toString(text), maxWidth));
  }

  fillText(text, ...geom) {
    const string = toString(text);
    if (!writeText(this, "fill", string, geom))
      this.ƒ("fillText", string, ...geom);
  }

  strokeText(text, ...geom) {
    const string = toString(text);
    if (!writeText(this, "stroke", string, geom))
      this.ƒ("strokeText", string, ...geom);
  }

  outlineText(text, ...geom) {
    let path = this.ƒ("outlineText", toString(text), ...geom);
    return path ? wrap(Path2D, path) : null;
  }

  drawParagraph(paragraph, x, y) {
    if (!(paragraph instanceof Paragraph)) {
      throw new TypeError("Expected a Paragraph as 1st arg");
    }
    this.ƒ("drawParagraph", core(paragraph), x, y);
  }

  // -- non-standard typography extensions --------------------------------------------
  get fontHinting() {
    return this.prop("fontHinting");
  }
  set fontHinting(flag) {
    this.prop("fontHinting", !!flag);
  }
  get fontVariant() {
    return this.prop("fontVariant");
  }
  set fontVariant(str) {
    this.prop("fontVariant", css.variant(str));
  }

  // The CSS longhand of `fontVariant`, which is the shorthand: reading returns
  // just the caps token, and writing replaces it while leaving the other axes
  // (numeric figures, ligatures, alternates) as they were.
  //
  // Invalid values are ignored rather than thrown, which is what the Canvas
  // standard requires of an attribute setter and what browsers do.
  get fontVariantCaps() {
    let caps = this.fontVariant.split(/\s+/).find((tok) => CAPS.has(tok));
    return caps || "normal";
  }
  set fontVariantCaps(str) {
    if (!CAPS.has(str) && str !== "normal") return;

    let rest = this.fontVariant
      .split(/\s+/)
      .filter((tok) => tok !== "normal" && !CAPS.has(tok));
    let next = (str === "normal" ? rest : [str, ...rest]).join(" ");
    this.fontVariant = next || "normal";
  }
  get textWrap() {
    return this.prop("textWrap");
  }
  set textWrap(flag) {
    this.prop("textWrap", !!flag);
  }
  get textDecoration() {
    return this.prop("textDecoration");
  }
  set textDecoration(str) {
    this.prop("textDecoration", css.decoration(str));
  }
  get fontVariationSettings() {
    return this.prop("fontVariationSettings");
  }
  set fontVariationSettings(str) {
    let settings = css.variationSettings(str);
    this.prop("fontVariationSettings", settings);
  }
  set textTracking(_) {
    process.emitWarning(
      "The .textTracking property has been removed; use the .letterSpacing property instead",
      "PropertyRemoved",
    );
  }

  // -- effects ---------------------------------------------------------------
  get globalCompositeOperation() {
    return this.prop("globalCompositeOperation");
  }
  set globalCompositeOperation(blend) {
    this.prop("globalCompositeOperation", blend);
  }
  get globalAlpha() {
    return this.prop("globalAlpha");
  }
  set globalAlpha(alpha) {
    this.prop("globalAlpha", alpha);
  }
  get shadowBlur() {
    return this.prop("shadowBlur");
  }
  set shadowBlur(level) {
    this.prop("shadowBlur", level);
  }
  get shadowColor() {
    return this.prop("shadowColor");
  }
  set shadowColor(color) {
    this.prop("shadowColor", color);
  }
  get shadowOffsetX() {
    return this.prop("shadowOffsetX");
  }
  set shadowOffsetX(x) {
    this.prop("shadowOffsetX", x);
  }
  get shadowOffsetY() {
    return this.prop("shadowOffsetY");
  }
  set shadowOffsetY(y) {
    this.prop("shadowOffsetY", y);
  }
  get filter() {
    return this.prop("filter");
  }
  set filter(str) {
    // Resolve relative lengths against the context font, which is what
    // Chrome does: at 16px `blur(0.5em)` is 8px and at 40px it is 20px.
    // This passed nothing, so `parseSize` fell back to its default of 16
    // and every `em` in a filter meant the same thing whatever the font
    // said. `parseFont` is cached, so re-reading it here is a lookup.
    let { size = 16 } = css.font(this.font) || {};
    this.prop("filter", css.filter(str, size));
  }

  // -- Skia filter properties (CanvasKit parity) --------------------------
  get colorFilter() {
    return this.ref("colorFilter") ?? null;
  }
  set colorFilter(filter) {
    if (filter !== null && !(filter instanceof ColorFilter)) {
      throw new TypeError("Expected ColorFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("ColorFilter has been deleted");
    }
    this.ref("colorFilter", filter);
    this.prop("colorFilter", filter ? core(filter) : null);
  }

  get imageFilter() {
    return this.ref("imageFilter") ?? null;
  }
  set imageFilter(filter) {
    if (filter !== null && !(filter instanceof ImageFilter)) {
      throw new TypeError("Expected ImageFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("ImageFilter has been deleted");
    }
    this.ref("imageFilter", filter);
    this.prop("skiaImageFilter", filter ? core(filter) : null);
  }

  get maskFilter() {
    return this.ref("maskFilter") ?? null;
  }
  set maskFilter(filter) {
    if (filter !== null && !(filter instanceof MaskFilter)) {
      throw new TypeError("Expected MaskFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("MaskFilter has been deleted");
    }
    this.ref("maskFilter", filter);
    this.prop("skiaMaskFilter", filter ? core(filter) : null);
  }

  [REPR](depth, options) {
    let props = [
      "canvas",
      "currentTransform",
      "fillStyle",
      "strokeStyle",
      "font",
      "fontStretch",
      "fontVariant",
      "direction",
      "textAlign",
      "textBaseline",
      "textWrap",
      "letterSpacing",
      "wordSpacing",
      "globalAlpha",
      "globalCompositeOperation",
      "imageSmoothingEnabled",
      "imageSmoothingQuality",
      "filter",
      "shadowBlur",
      "shadowColor",
      "shadowOffsetX",
      "shadowOffsetY",
      "lineCap",
      "lineDashOffset",
      "lineJoin",
      "lineWidth",
      "miterLimit",
    ];
    let info = {};
    if (depth > 0) {
      for (var prop of props) {
        try {
          info[prop] = this[prop];
        } catch {
          info[prop] = undefined;
        }
      }
    }
    return `CanvasRenderingContext2D ${inspect(info, options)}`;
  }
}

// The verbs and property writes Rust declares, recorded rather than called.
//
// `save` and `restore` keep their own wrappers: both carry JavaScript-side
// state -- the shader and filter references a `save` has to remember -- and a
// generated writer would drop it. They still cross immediately, and that stays
// correct without any care being taken, because crossing reads the handle and
// reading the handle hands over everything recorded before it.
drawlist.install(
  CanvasRenderingContext2D,
  neon.CanvasRenderingContext2D.verbTable(),
  // Three arguments where the batch named nothing a number cannot hold, which
  // is most of them. The fourth is walked on arrival whether or not it holds
  // anything, and the crossing is what a flush costs -- see `plot` in
  // `node::verbs`.
  (target, buffer, length, slots) =>
    slots.length
      ? neon.CanvasRenderingContext2D.plot(
          drawlist.rawHandle(target),
          buffer,
          length,
          slots,
        )
      : neon.CanvasRenderingContext2D.plot(
          drawlist.rawHandle(target),
          buffer,
          length,
        ),
);

module.exports = { CanvasRenderingContext2D };
