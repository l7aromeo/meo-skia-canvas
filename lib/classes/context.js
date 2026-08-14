//
// The Canvas drawing API
//

"use strict";

const { RustClass, core, wrap, inspect, argc, REPR } = require("./neon"),
  {
    Canvas,
    CanvasGradient,
    CanvasPattern,
    CanvasTexture,
  } = require("./canvas"),
  { fromSkMatrix, toSkMatrix } = require("./geometry"),
  { Image, ImageData } = require("./imagery"),
  { TextMetrics, Paragraph } = require("./typography"),
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
    this.ƒ("saveLayer", alpha, bounds, backdrop ? core(backdrop) : null);
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
    this.prop("currentTransform", toSkMatrix.apply(null, arguments));
  }

  transform(matrix) {
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
    if (path instanceof Path2D) arguments[0] = core(path);
    return this.ƒ("fill", ...arguments);
  }

  stroke(path) {
    if (path instanceof Path2D) arguments[0] = core(path);
    return this.ƒ("stroke", ...arguments);
  }

  clip(path, rule) {
    if (path instanceof Path2D) arguments[0] = core(path);
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
  createConicGradient(startAngle, x, y) {
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
      this.ƒ("drawImage", core(image.getContext("2d")), ...coords);
    } else if (image instanceof Image) {
      if (image.complete) this.ƒ("drawImage", core(image), ...coords);
      else
        throw Error(
          "Image has not completed loading: listen for `load` event or await `decode()` first",
        );
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
    this.prop("font", css.font(str));
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
    let metrics = JSON.parse(this.ƒ("measureText", toString(text), maxWidth));
    return new TextMetrics(metrics);
  }

  fillText(text, ...geom) {
    this.ƒ("fillText", toString(text), ...geom);
  }

  strokeText(text, ...geom) {
    this.ƒ("strokeText", toString(text), ...geom);
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

module.exports = { CanvasRenderingContext2D };
