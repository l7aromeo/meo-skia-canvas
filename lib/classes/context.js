//
// The Canvas drawing API
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
    REF,
    PROP,
    CALL,
  } = require("./neon"),
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
      super(CanvasRenderingContext2D)[ALLOC](core(canvas));
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
    this[CALL]("reset");
    // Clear JS-side state stack and refs
    this.#stateStack.length = 0;
    this[REF]("colorFilter", null);
    this[REF]("imageFilter", null);
    this[REF]("maskFilter", null);
    this[REF]("fill", null);
    this[REF]("stroke", null);
  }

  // -- grid state ------------------------------------------------------------
  save() {
    this[CALL]("save");
    // Push current JS refs onto stack (for shaders and filters)
    this.#stateStack.push({
      colorFilter: this[REF]("colorFilter") ?? null,
      imageFilter: this[REF]("imageFilter") ?? null,
      maskFilter: this[REF]("maskFilter") ?? null,
      fill: this[REF]("fill") ?? null,
      stroke: this[REF]("stroke") ?? null,
    });
  }
  restore() {
    this[CALL]("restore");
    // Pop JS refs from stack
    const state = this.#stateStack.pop();
    if (state) {
      this[REF]("colorFilter", state.colorFilter);
      this[REF]("imageFilter", state.imageFilter);
      this[REF]("maskFilter", state.maskFilter);
      this[REF]("fill", state.fill);
      this[REF]("stroke", state.stroke);
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
    else
      this[CALL]("saveLayer", alpha, bounds, backdrop ? core(backdrop) : null);
    // saveLayer opens a save frame that restore() pops -- mirror save()'s
    // JS ref snapshot so the cached filter/style refs unwind in step.
    this.#stateStack.push({
      colorFilter: this[REF]("colorFilter") ?? null,
      imageFilter: this[REF]("imageFilter") ?? null,
      maskFilter: this[REF]("maskFilter") ?? null,
      fill: this[REF]("fill") ?? null,
      stroke: this[REF]("stroke") ?? null,
    });
  }

  get currentTransform() {
    return fromSkMatrix(this[PROP]("currentTransform"));
  }
  set currentTransform(matrix) {
    this.setTransform(matrix);
  }

  resetTransform() {
    this[CALL]("resetTransform");
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
    this[PROP]("currentTransform", toSkMatrix.apply(null, arguments));
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
    this[CALL]("transform", toSkMatrix.apply(null, arguments));
  }
  translate(x, y) {
    this[CALL]("translate", ...arguments);
  }
  scale(x, y) {
    this[CALL]("scale", ...arguments);
  }
  rotate(angle) {
    this[CALL]("rotate", ...arguments);
  }

  createProjection(quad, basis) {
    return fromSkMatrix(
      this[CALL]("createProjection", [quad].flat(), [basis].flat()),
    );
  }

  // -- bézier paths ----------------------------------------------------------
  beginPath() {
    this[CALL]("beginPath");
  }
  rect(x, y, width, height) {
    this[CALL]("rect", ...arguments);
  }
  arc(x, y, radius, startAngle, endAngle, isCCW) {
    this[CALL]("arc", ...arguments);
  }
  ellipse(x, y, xRadius, yRadius, rotation, startAngle, endAngle, isCCW) {
    this[CALL]("ellipse", ...arguments);
  }
  moveTo(x, y) {
    this[CALL]("moveTo", ...arguments);
  }
  lineTo(x, y) {
    this[CALL]("lineTo", ...arguments);
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
  closePath() {
    this[CALL]("closePath");
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
    return this[CALL]("fill", ...arguments);
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
    return this[CALL]("stroke", ...arguments);
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
    return this[CALL]("clip", ...arguments);
  }

  isPointInPath(path, x, y, rule) {
    if (path instanceof Path2D) arguments[0] = core(path);
    return this[CALL]("isPointInPath", ...arguments);
  }
  isPointInStroke(path, x, y) {
    if (path instanceof Path2D) arguments[0] = core(path);
    return this[CALL]("isPointInStroke", ...arguments);
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
    this[CALL]("fillRect", ...arguments);
  }
  strokeRect(x, y, width, height) {
    this[CALL]("strokeRect", ...arguments);
  }
  clearRect(x, y, width, height) {
    this[CALL]("clearRect", ...arguments);
  }

  set fillStyle(style) {
    let isShader =
        style instanceof CanvasPattern ||
        style instanceof CanvasGradient ||
        style instanceof CanvasTexture ||
        style instanceof Shader,
      [ref, val] = isShader ? [style, core(style)] : [null, style];
    this[REF]("fill", ref);
    this[PROP]("fillStyle", val);
  }

  get fillStyle() {
    let style = this[PROP]("fillStyle");
    return style === null ? this[REF]("fill") : style;
  }

  set strokeStyle(style) {
    let isShader =
        style instanceof CanvasPattern ||
        style instanceof CanvasGradient ||
        style instanceof CanvasTexture ||
        style instanceof Shader,
      [ref, val] = isShader ? [style, core(style)] : [null, style];
    this[REF]("stroke", ref);
    this[PROP]("strokeStyle", val);
  }

  get strokeStyle() {
    let style = this[PROP]("strokeStyle");
    return style === null ? this[REF]("stroke") : style;
  }

  // -- line style ------------------------------------------------------------
  getLineDash() {
    return this[CALL]("getLineDash");
  }
  setLineDash(segments) {
    this[CALL]("setLineDash", ...arguments);
  }
  get lineCap() {
    return this[PROP]("lineCap");
  }
  set lineCap(style) {
    this[PROP]("lineCap", style);
  }
  get lineDashFit() {
    return this[PROP]("lineDashFit");
  }
  set lineDashFit(style) {
    this[PROP]("lineDashFit", style);
  }
  get lineDashMarker() {
    return wrap(Path2D, this[PROP]("lineDashMarker"));
  }
  set lineDashMarker(path) {
    this[PROP]("lineDashMarker", path instanceof Path2D ? core(path) : path);
  }
  get lineDashOffset() {
    return this[PROP]("lineDashOffset");
  }
  set lineDashOffset(offset) {
    this[PROP]("lineDashOffset", offset);
  }
  get lineJoin() {
    return this[PROP]("lineJoin");
  }
  set lineJoin(style) {
    this[PROP]("lineJoin", style);
  }
  get lineWidth() {
    return this[PROP]("lineWidth");
  }
  set lineWidth(width) {
    this[PROP]("lineWidth", width);
  }
  get miterLimit() {
    return this[PROP]("miterLimit");
  }
  set miterLimit(limit) {
    this[PROP]("miterLimit", limit);
  }

  // -- imagery ---------------------------------------------------------------
  get imageSmoothingEnabled() {
    return this[PROP]("imageSmoothingEnabled");
  }
  set imageSmoothingEnabled(flag) {
    this[PROP]("imageSmoothingEnabled", !!flag);
  }
  get dither() {
    return this[PROP]("dither");
  }
  set dither(flag) {
    this[PROP]("dither", !!flag);
  }
  get imageSmoothingQuality() {
    return this[PROP]("imageSmoothingQuality");
  }
  set imageSmoothingQuality(level) {
    this[PROP]("imageSmoothingQuality", level);
  }

  createImageData(width, height, settings) {
    // The single-argument form clones an existing ImageData -- same
    // dimensions, blank pixels. `argc` rejects one argument, so the
    // spec-standard call threw.
    if (arguments.length == 1 && width instanceof ImageData) {
      // Both settings, not just the format. The standard says the clone keeps
      // the source's settings, and dropping the colour space here made this
      // the one of the two documented ways to copy an `ImageData` that turned
      // a Display P3 buffer into an sRGB one -- `new ImageData(source)` keeps
      // it, so the two disagreed about the same operation.
      return new ImageData(width.width, width.height, {
        colorType: width.colorType,
        colorSpace: width.colorSpace,
      });
    }
    argc(arguments, 2, 3);

    // Inherit the canvas's space and format, as `getImageData` below does and
    // as the clone above does. The standard fixes the space -- "defaultColorSpace
    // set to this's color space" -- and a `display-p3` canvas handed back an
    // `srgb` buffer is not merely mislabelled: the label decides how
    // `putImageData` reads the bytes, so P3 components written through it were
    // taken as sRGB and converted on the way in.
    //
    // The format comes along for the same reason it does two methods down.
    // Leaving it would have `createImageData` and `getImageData` describe
    // different buffers for one canvas, which is the disagreement this is
    // fixing rather than a second one to introduce.
    let { colorType, colorSpace, ...rest } = settings ?? {};
    return new ImageData(width, height, {
      ...rest,
      colorType: colorType ?? this.canvas.colorType,
      colorSpace: colorSpace ?? this.canvas.colorSpace,
    });
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

    // A whole number, unlike `exportOptions` in `lib/classes/canvas.js`, which
    // takes any positive one. The difference is not a preference: at a
    // fractional density there is no size this can hand `ImageData` that is
    // right, because the width is not a function of the width.
    //
    // The addon rounds the scaled *rect*, edge by edge -- `Rect::round` in
    // `Context2D::get_image_data` -- so the pixel count depends on where the
    // region starts. A 5x5 region at density 1.5 is 8 wide from x=0 (0.0->0,
    // 7.5->8) and 7 wide from x=1 (1.5->2, 9.0->9). The export path scales the
    // *size* instead, `Page::scaled_dimensions`, where the origin cannot enter
    // into it: 5 at 1.5 floors to 7, wherever it starts.
    //
    // The two are not two answers to one question, so there is nothing to
    // converge them on. Rounding each edge is what makes abutting reads tile
    // exactly: a shared edge rounds to the same integer from both sides, so
    // the halves of an eight-wide canvas at density 1.5 sum to the whole.
    // Flooring each region's size independently gives five and six against a
    // whole of twelve -- a gap. And a page has no origin for an origin rule
    // to apply to, which is why the export path scales a size and cannot do
    // otherwise.
    //
    // So a fraction accepted here would need an origin-dependent rule
    // reproduced in JavaScript, and adopting either side's rule for both
    // would break a property the other one is right to have. This refuses
    // instead, by decision rather than by omission. The two agree exactly
    // when the product is whole, which is why `10` at `1.5` is fifteen either
    // way and the seam went unnoticed for years.
    //
    // The message is the one `exportOptions` gives, minus that difference:
    // the wording it replaces named a range containing `0`, which it refused.
    if (
      typeof density != "number" ||
      !Number.isInteger(density) ||
      density < 1
    ) {
      throw new TypeError("Expected a positive integer for `density`");
    }

    if (msaa === undefined || msaa === true) {
      msaa = undefined; // use the default 4x msaa
    } else if (!isFinite(+msaa) || +msaa < 0) {
      throw new TypeError("The number of MSAA samples must be an integer ≥0");
    }

    let opts = { colorType, colorSpace, density, matte, msaa },
      buffer = this[CALL](
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
    this[CALL]("putImageData", imageData, ...coords);
  }

  drawImage(image, ...coords) {
    if (image instanceof Canvas) {
      const source = image.getContext("2d");
      if (!place(this, source, coords))
        this[CALL]("drawImage", core(source), ...coords);
    } else if (image instanceof Image) {
      if (!image.complete)
        throw Error(
          "Image has not completed loading: listen for `load` event or await `decode()` first",
        );
      if (!place(this, image, coords))
        this[CALL]("drawImage", core(image), ...coords);
    } else if (image instanceof ImageData) {
      this[CALL]("drawImage", image, ...coords);
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
      this[CALL]("drawCanvas", core(image.getContext("2d")), ...coords);
    } else {
      this.drawImage(image, ...coords);
    }
  }

  // -- typography ------------------------------------------------------------
  get font() {
    return this[PROP]("font");
  }
  set font(str) {
    // The canonical string first, then the specification it came from.
    // Rust reads the string on its own and only goes on to the object the
    // first time it sees that name -- reading it is thirty times what
    // parsing the CSS costs, and the parse is already memoized here.
    const spec = css.font(str);
    this[PROP]("font", spec && spec.canonical, spec);
  }
  get textAlign() {
    return this[PROP]("textAlign");
  }
  set textAlign(mode) {
    this[PROP]("textAlign", mode);
  }
  get textBaseline() {
    return this[PROP]("textBaseline");
  }
  set textBaseline(mode) {
    this[PROP]("textBaseline", mode);
  }
  get direction() {
    return this[PROP]("direction");
  }
  set direction(mode) {
    this[PROP]("direction", mode);
  }
  get fontStretch() {
    return this[PROP]("fontStretch");
  }
  set fontStretch(str) {
    this[PROP]("fontStretch", css.stretch(str));
  }
  get letterSpacing() {
    return this[PROP]("letterSpacing");
  }
  set letterSpacing(str) {
    this[PROP]("letterSpacing", css.spacing(str));
  }
  get wordSpacing() {
    return this[PROP]("wordSpacing");
  }
  set wordSpacing(str) {
    this[PROP]("wordSpacing", css.spacing(str));
  }

  measureText(text, maxWidth) {
    // A buffer of numbers and an array of family names, assembled into the
    // object here. Building that object in Rust meant about forty property
    // writes, each one a call across the binding -- 4.6 microseconds of a
    // 9.4-microsecond call, against 3.5 for the typesetting it reports.
    return textMetrics(this[CALL]("measureText", toString(text), maxWidth));
  }

  fillText(text, ...geom) {
    const string = toString(text);
    if (!writeText(this, "fill", string, geom))
      this[CALL]("fillText", string, ...geom);
  }

  strokeText(text, ...geom) {
    const string = toString(text);
    if (!writeText(this, "stroke", string, geom))
      this[CALL]("strokeText", string, ...geom);
  }

  outlineText(text, ...geom) {
    let path = this[CALL]("outlineText", toString(text), ...geom);
    return path ? wrap(Path2D, path) : null;
  }

  drawParagraph(paragraph, x, y) {
    if (!(paragraph instanceof Paragraph)) {
      throw new TypeError("Expected a Paragraph as 1st arg");
    }
    this[CALL]("drawParagraph", core(paragraph), x, y);
  }

  // -- non-standard typography extensions --------------------------------------------
  get fontHinting() {
    return this[PROP]("fontHinting");
  }
  set fontHinting(flag) {
    this[PROP]("fontHinting", !!flag);
  }
  get fontVariant() {
    return this[PROP]("fontVariant");
  }
  set fontVariant(str) {
    // Ignored rather than thrown when it will not parse, matching every
    // sibling property and what the Canvas standard asks of an attribute
    // setter.
    const spec = css.variant(str);
    if (spec) this[PROP]("fontVariant", spec);
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
    return this[PROP]("textWrap");
  }
  set textWrap(flag) {
    this[PROP]("textWrap", !!flag);
  }
  get textDecoration() {
    return this[PROP]("textDecoration");
  }
  set textDecoration(str) {
    this[PROP]("textDecoration", css.decoration(str));
  }
  get fontVariationSettings() {
    return this[PROP]("fontVariationSettings");
  }
  set fontVariationSettings(str) {
    let settings = css.variationSettings(str);
    this[PROP]("fontVariationSettings", settings);
  }
  set textTracking(_) {
    process.emitWarning(
      "The .textTracking property has been removed; use the .letterSpacing property instead",
      "PropertyRemoved",
    );
  }

  // -- effects ---------------------------------------------------------------
  get globalCompositeOperation() {
    return this[PROP]("globalCompositeOperation");
  }
  set globalCompositeOperation(blend) {
    this[PROP]("globalCompositeOperation", blend);
  }
  get globalAlpha() {
    return this[PROP]("globalAlpha");
  }
  set globalAlpha(alpha) {
    this[PROP]("globalAlpha", alpha);
  }
  get shadowBlur() {
    return this[PROP]("shadowBlur");
  }
  set shadowBlur(level) {
    this[PROP]("shadowBlur", level);
  }
  get shadowColor() {
    return this[PROP]("shadowColor");
  }
  set shadowColor(color) {
    this[PROP]("shadowColor", color);
  }
  get shadowOffsetX() {
    return this[PROP]("shadowOffsetX");
  }
  set shadowOffsetX(x) {
    this[PROP]("shadowOffsetX", x);
  }
  get shadowOffsetY() {
    return this[PROP]("shadowOffsetY");
  }
  set shadowOffsetY(y) {
    this[PROP]("shadowOffsetY", y);
  }
  get filter() {
    return this[PROP]("filter");
  }
  set filter(str) {
    // Resolve relative lengths against the context font, which is what
    // Chrome does: at 16px `blur(0.5em)` is 8px and at 40px it is 20px.
    // This passed nothing, so `parseSize` fell back to its default of 16
    // and every `em` in a filter meant the same thing whatever the font
    // said. `parseFont` is cached, so re-reading it here is a lookup.
    let { size = 16 } = css.font(this.font) || {};
    this[PROP]("filter", css.filter(str, size));
  }

  // -- Skia filter properties (CanvasKit parity) --------------------------
  get colorFilter() {
    return this[REF]("colorFilter") ?? null;
  }
  set colorFilter(filter) {
    if (filter !== null && !(filter instanceof ColorFilter)) {
      throw new TypeError("Expected ColorFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("ColorFilter has been deleted");
    }
    this[REF]("colorFilter", filter);
    this[PROP]("colorFilter", filter ? core(filter) : null);
  }

  get imageFilter() {
    return this[REF]("imageFilter") ?? null;
  }
  set imageFilter(filter) {
    if (filter !== null && !(filter instanceof ImageFilter)) {
      throw new TypeError("Expected ImageFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("ImageFilter has been deleted");
    }
    this[REF]("imageFilter", filter);
    this[PROP]("skiaImageFilter", filter ? core(filter) : null);
  }

  get maskFilter() {
    return this[REF]("maskFilter") ?? null;
  }
  set maskFilter(filter) {
    if (filter !== null && !(filter instanceof MaskFilter)) {
      throw new TypeError("Expected MaskFilter or null");
    }
    if (filter?._deleted) {
      throw new Error("MaskFilter has been deleted");
    }
    this[REF]("maskFilter", filter);
    this[PROP]("skiaMaskFilter", filter ? core(filter) : null);
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
