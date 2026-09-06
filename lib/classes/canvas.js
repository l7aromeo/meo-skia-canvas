//
// Canvas object & export options
//

"use strict";

const { fileURLToPath } = require("url"),
  {
    RustClass,
    core,
    inspect,
    argc,
    skiaNode,
    REPR,
    ALLOC,
    INIT,
    PROP,
    CALL,
    STRICT,
  } = require("./neon"),
  {
    Image,
    ImageData,
    pixelSize,
    getSharp,
    SHARP_COLOR_TYPE,
    SHARP_CHANNELS,
  } = require("./imagery"),
  { Path2D } = require("./path"),
  { toSkMatrix } = require("./geometry");

// `width` and `height` are IDL `unsigned long`, and an assignment converts
// rather than refuses: `ToNumber` first, then `NaN` and the infinities become
// 0 and anything else truncates toward zero and wraps at 2^32. Only a
// negative value takes the default -- HTML's rule that a dimension which
// cannot be used falls back to it.
//
// Chrome 148, measured rather than inferred: `"abc"`, `NaN`, `Infinity` and
// `4294967296` all give 0, `-0.4` gives 0, `25.7` gives 25, `"0x10"` gives 16,
// `true` gives 1, and `-5` and `-1` give 300. The one row not shared is a
// value above Chrome's own maximum canvas dimension, which it resets to the
// default: `4294967295` gives 300 there and 4294967296 here, the second of
// those being a dimension stored as an `f32` and read back rounded rather
// than anything this conversion does. There is no maximum here to compare
// against.
// The largest dimension Skia's raster geometry holds exactly.
//
// `SkSize` and `SkRect` are `f32`, and a page's bounds go through them, so
// above `f32`'s exact-integer range the stored dimension is not the one that
// was asked for: 16777217 becomes 16777216, and 16777219 becomes 16777220 --
// a column *wider* than requested, whose pixels exist. Clamping here keeps
// `canvas.width`, the value assigned and the raster in agreement, because
// every integer at or below this bound is exact in `f32`.
//
// Skia's own `kMaxDimension` is `SK_MaxS32 >> 2`, or 2^29 - 1, and cannot
// serve: a maximum only keeps the report honest when it sits at or below the
// precision, and that one sits above it. Do not "correct" this to Skia's
// number -- it would reinstate the defect for 520 million reachable values.
//
// Checkable from here, since `Math.fround` is this side's `f32`:
// `Math.fround(2 ** 24)` is `2 ** 24`, and `Math.fround(2 ** 24 + 1)` is not.
const MAX_DIMENSION = 2 ** 24;

const toDimension = (value, fallback) => {
  const number = Math.trunc(Number(value));
  if (!Number.isFinite(number)) return 0;
  return number < 0 ? fallback : Math.min(number >>> 0, MAX_DIMENSION);
};

// The constructor is the markup form rather than the property one. A `<canvas>`
// whose `width` attribute is absent or unparseable is 300 wide, not 0, so an
// argument that cannot be used here is an argument that was not given. That is
// what makes `new Canvas()` 300x150, and it is why the constructor cannot
// simply assign through the setters above.
const toInitialDimension = (value, fallback) => {
  const number = Math.trunc(Number(value));
  // The same bound as the setter. These two implement deliberately different
  // rules for an argument that cannot be used -- the constructor falls back
  // where assignment converts -- but neither rule says anything about a value
  // that is merely too large, so the clamp belongs in both or the two would
  // disagree in a range nothing tests.
  return Number.isFinite(number) && number >= 0
    ? Math.min(number >>> 0, MAX_DIMENSION)
    : fallback;
};

class Canvas extends RustClass {
  #contexts;

  /** @type {WeakMap<Canvas, CanvasRenderingContext2D[]>} */
  static contexts = new WeakMap();

  constructor(
    width,
    height,
    {
      textContrast = 0,
      textGamma = 1.4,
      gpu = true,
      colorType = "rgba",
      colorSpace = "srgb",
    } = {},
  ) {
    super(Canvas)[ALLOC]({
      textContrast,
      textGamma,
      gpu: !!gpu,
      colorType,
      colorSpace,
    });
    this.#contexts = [];
    // Declared in the types but never implemented. The WeakMap holds the
    // same array `#contexts` does, so it tracks pages as they are added
    // rather than snapshotting them.
    Canvas.contexts.set(this, this.#contexts);
    this[PROP]("width", toInitialDimension(width, 300));
    this[PROP]("height", toInitialDimension(height, 150));
  }

  getContext(kind) {
    return kind == "2d" ? this.#contexts[0] || this.newPage() : null;
  }

  get gpu() {
    return this[PROP]("engine") == "gpu";
  }
  set gpu(mode) {
    this[PROP]("engine", mode ? "gpu" : "cpu");
  }

  get engine() {
    return JSON.parse(this[PROP]("engine_status"));
  }

  // The pixel format this canvas was constructed with. Exports and getImageData
  // inherit it unless the call names one of its own.
  get colorType() {
    return this[PROP]("colorType");
  }

  get colorSpace() {
    return this[PROP]("colorSpace");
  }

  get width() {
    return this[PROP]("width");
  }
  set width(w) {
    this[PROP]("width", toDimension(w, 300));
    if (this.#contexts[0]) this.getContext("2d")[CALL]("resetSize", core(this));
  }

  get height() {
    return this[PROP]("height");
  }
  set height(h) {
    this[PROP]("height", toDimension(h, 150));
    if (this.#contexts[0]) this.getContext("2d")[CALL]("resetSize", core(this));
  }

  newPage(width, height) {
    // The dimensions come as a pair or not at all. A lone argument used to be
    // dropped in silence, so `newPage(500)` added a page at the old size and
    // reported nothing.
    if (arguments.length == 1) {
      throw new TypeError(
        "newPage() takes a width and a height together, or neither to keep the current size",
      );
    }

    const { CanvasRenderingContext2D } = require("./context");
    let ctx = new CanvasRenderingContext2D(this);
    this.#contexts.unshift(ctx);
    if (arguments.length >= 2) {
      // Sized the way the constructor is, not the way an assignment is: these
      // are arguments naming a size, so one that cannot be used is one that
      // was not given. `newPage(NaN, NaN)` is 300x150 for the same reason
      // `new Canvas()` is.
      this[PROP]("width", toInitialDimension(width, 300));
      this[PROP]("height", toInitialDimension(height, 150));
      this.getContext("2d")[CALL]("resetSize", core(this));
    }
    return ctx;
  }

  get pages() {
    return this.#contexts.slice().reverse();
  }

  get raw() {
    return this.toBuffer("raw");
  }
  get png() {
    return this.toBuffer("png");
  }
  get jpg() {
    return this.toBuffer("jpg");
  }
  get pdf() {
    return this.toBuffer("pdf");
  }
  get svg() {
    return this.toBuffer("svg");
  }
  get webp() {
    return this.toBuffer("webp");
  }

  // Warn about renamed methods but map them to the new names (for now)
  saveAs() {
    _deprecated("Canvas.saveAs()");
    return this.toFile(...arguments);
  }
  saveAsSync() {
    _deprecated("Canvas.saveAsSync()");
    return this.toFileSync(...arguments);
  }
  toDataURLSync() {
    _deprecated("Canvas.toDataURLSync()");
    return this.toURLSync(...arguments);
  }

  toSharpSync({ page, matte, msaa, density = 1 } = {}) {
    // The async form pipes through a stream because `toBuffer` is a promise.
    // Synchronously we already hold the bytes, so hand them to sharp directly
    // -- same raw layout, same metadata, no stream.
    const sharp = getSharp(),
      buffer = this.toBufferSync("raw", {
        page,
        matte,
        density,
        msaa,
        colorType: SHARP_COLOR_TYPE,
      });

    return sharp(buffer, {
      raw: {
        width: this.width * density,
        height: this.height * density,
        channels: SHARP_CHANNELS,
      },
    }).withMetadata({ density: density * 72 });
  }

  toFile(filename, opts = {}) {
    let { pages, padding, pattern, ...rest } = exportOptions(
        this,
        { filename },
        opts,
      ),
      args = [pages.map(core), pattern, padding, rest];
    return this[CALL]("save", ...args);
  }

  toFileSync(filename, opts = {}) {
    let { pages, padding, pattern, ...rest } = exportOptions(
      this,
      { filename },
      opts,
    );
    this[CALL]("saveSync", pages.map(core), pattern, padding, rest);
  }

  toBuffer(extension = "png", opts = {}) {
    let { pages, ...rest } = exportOptions(this, { extension }, opts);
    return this[CALL]("toBuffer", pages.map(core), rest);
  }

  toBufferSync(extension = "png", opts = {}) {
    let { pages, ...rest } = exportOptions(this, { extension }, opts);
    return this[CALL]("toBufferSync", pages.map(core), rest);
  }

  // Callback-style and returning undefined, as the standard defines it, rather
  // than the promise the rest of this class returns. `type` is a mime type
  // here -- "image/png" -- not the bare extension the other exporters take.
  //
  // Per spec a failure calls back with null instead of raising: the callback
  // has already been handed off by the time the encode runs, so there is
  // nowhere left to throw.
  toBlob(callback, type = "image/png", quality) {
    if (typeof callback != "function") {
      throw new TypeError("toBlob() expects a callback function");
    }

    let extension = String(type).replace(/^image\//, ""),
      opts = quality === undefined ? {} : { quality };

    this.toBuffer(extension, opts).then(
      (data) => callback(new Blob([data], { type })),
      () => callback(null),
    );
  }

  toURL(extension = "png", opts = {}) {
    let { mime } = exportOptions(this, { extension }, opts),
      buffer = this.toBuffer(extension, opts);
    return buffer.then(
      (data) => `data:${mime};base64,${data.toString("base64")}`,
    );
  }

  toURLSync(extension = "png", opts = {}) {
    let { mime } = exportOptions(this, { extension }, opts),
      buffer = this.toBufferSync(extension, opts);
    return `data:${mime};base64,${buffer.toString("base64")}`;
  }

  // Match the browser API in only accepting a single optional quality argument
  toDataURL(extension = "png", quality) {
    if (quality !== undefined && typeof quality !== "number") {
      throw TypeError(
        "Expected a number in the range 0–1 for `quality` (use toURL() for additional rendering options)",
      );
    }
    return this.toURLSync(extension, { quality });
  }

  toSharp({ page, matte, msaa, density = 1 } = {}) {
    const { Readable } = require("node:stream"),
      sharp = getSharp(),
      buffer = this.toBuffer("raw", {
        page,
        matte,
        density,
        msaa,
        colorType: SHARP_COLOR_TYPE,
      });

    return Readable.from(
      (async function* () {
        yield buffer;
      })(),
    ).pipe(
      sharp({
        raw: {
          width: this.width * density,
          height: this.height * density,
          channels: SHARP_CHANNELS,
        },
      }).withMetadata({ density: density * 72 }),
    );
  }

  [REPR](depth, options) {
    let { width, height, gpu, engine, pages } = this;
    return `Canvas ${inspect({ width, height, gpu, engine, pages }, options)}`;
  }
}

class CanvasGradient extends RustClass {
  constructor(style, ...coords) {
    super(CanvasGradient);
    style = (style || "").toLowerCase();
    if (["linear", "radial", "conic"].includes(style))
      this[INIT](style, ...coords);
    else
      throw new Error(
        `Function is not a constructor (use CanvasRenderingContext2D's "createConicGradient", "createLinearGradient", and "createRadialGradient" methods instead)`,
      );
  }

  addColorStop(offset, color) {
    this[CALL]("addColorStop", ...arguments);
  }

  get interpolation() {
    return this[PROP]("interpolation");
  }

  set interpolation(value) {
    const valid = [
      "srgb",
      "srgb-linear",
      "lab",
      "oklab",
      "oklch",
      "lch",
      "hsl",
      "hwb",
    ];
    if (!valid.includes(value)) {
      throw new TypeError(`Invalid interpolation color space: ${value}`);
    }
    this[PROP]("interpolation", value);
  }

  get hueInterpolation() {
    return this[PROP]("hueInterpolation");
  }

  set hueInterpolation(value) {
    const valid = ["shorter", "longer", "increasing", "decreasing"];
    if (!valid.includes(value)) {
      throw new TypeError(`Invalid hue interpolation: ${value}`);
    }
    this[PROP]("hueInterpolation", value);
  }

  [REPR](depth, options) {
    return `CanvasGradient (${this[CALL]("repr")})`;
  }
}

class CanvasPattern extends RustClass {
  constructor(canvas, src, repeat) {
    repeat = [...arguments].slice(2);
    super(CanvasPattern);
    if (src instanceof Image) {
      let { width, height } = canvas;
      this[INIT]("from_image", core(src), width, height, ...repeat);
    } else if (src instanceof ImageData) {
      this[INIT]("from_image_data", src, ...repeat);
    } else if (src instanceof Canvas) {
      let ctx = src.getContext("2d");
      this[INIT]("from_canvas", core(ctx), ...repeat);
    } else {
      throw new Error("CanvasPatterns require a source Image or a Canvas");
    }
  }

  setTransform(matrix) {
    this[CALL]("setTransform", toSkMatrix.apply(null, arguments));
  }

  [REPR](depth, options) {
    return `CanvasPattern (${this[CALL]("repr")})`;
  }
}

class CanvasTexture extends RustClass {
  constructor(
    spacing,
    {
      path,
      color,
      angle,
      line,
      cap = "butt",
      outline = false,
      offset = 0,
    } = {},
  ) {
    super(CanvasTexture);
    argc(arguments, 1);
    let [x, y] = Array.isArray(offset)
      ? offset.concat(offset).slice(0, 2)
      : [offset, offset];
    let [h, v] = Array.isArray(spacing)
      ? spacing.concat(spacing).slice(0, 2)
      : [spacing, spacing];
    if (path !== undefined && !(path instanceof Path2D)) {
      throw TypeError("Expected a Path2D object for `path`");
    }
    path = core(path);
    line = line != null ? line : path ? 0 : 1;
    angle = angle != null ? angle : path ? 0 : -Math.PI / 4;
    this[ALLOC](path, color, line, cap, angle, !!outline, h, v, x, y);
  }

  [REPR](depth, options) {
    return `CanvasTexture (${this[CALL]("repr")})`;
  }
}

//
// Mime type <-> File extension mappings
//

// Asked for rather than remembered. This class used to carry its own copy of
// the extension and media-type maps, the list of names its error message
// offered, and -- the one that would have gone wrong silently -- a bare
// `format == "pdf"` deciding which exports gather every page. The addon knows
// all four from one table, and nothing here can drift from it: add a format
// there and this picks it up, including whether it spans pages.
const DESCRIBED = JSON.parse(skiaNode.formats());

class Format {
  constructor() {
    let formats = {},
      mimes = {},
      spanning = new Set(),
      animates = new Set(),
      depths = {};

    for (let {
      name,
      mime,
      extension,
      aliases,
      spansPages,
      animated,
      inferable,
      bitDepths,
    } of DESCRIBED) {
      // The extension is a key only where a filename may name the format.
      // `raw` is asked for by name and written as `.bin`, and registering
      // that suffix would make `toFile("x.bin")` write pixel bytes nothing
      // can read back, where it used to refuse the extension outright.
      let keys = [name, ...(inferable ? [extension] : []), ...aliases];
      for (let key of keys) formats[key] = mime;
      // First name wins, so a media type maps back to the canonical name
      // rather than to whichever alias came last: `image/jpeg` is `jpg`.
      mimes[mime] ??= name;
      if (spansPages) spanning.add(name);
      if (animated) animates.add(name);
      depths[name] = bitDepths;
    }

    Object.assign(this, {
      toMime: this.toMime.bind(this),
      fromMime: this.fromMime.bind(this),
      spansPages: this.spansPages.bind(this),
      animates: this.animates.bind(this),
      bitDepths: this.bitDepths.bind(this),
      expected: DESCRIBED.map(({ name }) => `"${name}"`)
        .join(", ")
        .replace(/, ([^,]*)$/, ", or $1"),
      formats,
      mimes,
      spanning,
      animating: animates,
      depths,
    });
  }

  toMime(ext) {
    return this.formats[(ext || "").replace(/^\./, "").toLowerCase()];
  }

  fromMime(mime) {
    return this.mimes[mime];
  }

  // Whether one file of this format carries every page. PDF, GIF and APNG
  // do; the last two are raster, which is why this cannot be inferred from
  // "is it a vector format".
  spansPages(format) {
    return this.spanning.has(format);
  }

  // Whether this format's pages are frames with durations. Not the same
  // question as spanning pages: TIFF, ICO and PDF gather every page and
  // none of them has a clock.
  animates(format) {
    return this.animating.has(format);
  }

  // The depths a caller may ask a file of this format to be written at,
  // which is empty for every format that takes its depth from the canvas.
  bitDepths(format) {
    return this.depths[format] || [];
  }
}

//
// Validation of the options dict shared by the `saveAs`, `toBuffer`, and `toDataURL` methods
//

const { basename, extname } = require("path");

function exportOptions(canvas, { filename = "", extension = "" }, opts) {
  // a single number will be interpreted as a quality setting
  if (typeof opts == "number") opts = { quality: opts };

  // unpack common export options
  let {
    page,
    pageRange,
    quality,
    matte,
    density,
    msaa,
    outline,
    downsample,
    colorType,
    bitDepth,
    chromaSampling,
    lossless,
    colorSpace,
    fps,
    frameDelays,
    loop: loops,
  } = opts;

  // Every key the destructure above reads, plus `format`, which is read
  // separately below and honoured only by `toFile`.
  //
  // Refused rather than ignored under `SKIA_CANVAS_STRICT`, and silent
  // otherwise -- the same gating `refuse_unknown_keys` uses for a text style,
  // and a `TypeError` for the same reason that site raises one. Silent by
  // default is how the Canvas API treats a value it does not recognise, and
  // it keeps a caller who threads their own bookkeeping through this object
  // from being broken by the check.
  //
  // It has to live here rather than beside the Rust parser. This function
  // rebuilds what it returns from the named locals above, so a key the caller
  // invented is dropped at this line and never reaches `export_options_arg` --
  // a guard on that side could not fire.
  if (STRICT && opts && typeof opts == "object") {
    const known = [
      "page",
      "pageRange",
      "quality",
      "matte",
      "density",
      "msaa",
      "outline",
      "downsample",
      "colorType",
      "bitDepth",
      "chromaSampling",
      "lossless",
      "colorSpace",
      "fps",
      "frameDelays",
      "loop",
      "format",
    ];
    for (const name of Object.keys(opts)) {
      if (!known.includes(name)) {
        throw new TypeError(`Unknown export option \`${name}\``);
      }
    }
  }

  // only allow format overrides in toFile()
  let imageFormat = filename ? opts.format : undefined;

  if (filename instanceof URL) {
    if (filename.protocol == "file:") filename = fileURLToPath(filename);
    else
      throw Error(
        `URLs must use 'file' protocol (got '${filename.protocol.replace(":", "")}')`,
      );
  }

  // ensure the canvas has a context (so it can at least generate an empty image)
  if (!canvas.pages.length) canvas.getContext("2d");

  var { fromMime, toMime, expected, spansPages, animates, bitDepths } =
      new Format(),
    ext = imageFormat || extension.replace(/@\d+x$/i, "") || extname(filename),
    format = fromMime(toMime(ext) || ext),
    mime = toMime(format),
    pages = canvas.pages,
    pp = pages.length;

  if (!ext)
    throw new Error(
      `Cannot determine image format (use a filename extension or 'format' argument)`,
    );
  if (!format)
    throw new Error(`Unsupported file format "${ext}" (expected ${expected})`);

  let padding,
    isSequence,
    pattern = filename.replace(/{(\d*)}/g, (_, width) => {
      isSequence = true;
      width = parseInt(width, 10);
      padding = isFinite(width) ? width : isFinite(padding) ? padding : -1;
      return "{}";
    });

  // A page number has to be whole before it can be range-checked, because
  // the range check cannot see the difference. `1.5` is greater than zero,
  // so it becomes an index of `0.5`; that is neither negative nor `>= pp`,
  // so it passes, and `pages[0.5]` is `undefined` — leaving an empty list
  // for native code that indexes it. `loop` is validated this way already;
  // this was the one numeric export option that was not. (`density` takes a
  // fraction deliberately, which is written down where it is read.)
  if (page !== undefined && !Number.isInteger(page))
    throw new TypeError(`Expected an integer for \`page\` (got ${page})`);

  // allow negative indexing if a specific page is specified
  let idx = page > 0 ? page - 1 : page < 0 ? pp + page : undefined;

  // Names the number the caller passed, not the index it resolved to. The
  // message speaks in 1-based pages -- "pages 1–2" -- and ended with the
  // 0-based `idx`, so asking for page 9 of a two-page canvas was told that
  // "8 is out of bounds", a number nobody had typed.
  if ((isFinite(idx) && idx < 0) || idx >= pp)
    throw new RangeError(
      pp == 1
        ? `Canvas only has a ‘page 1’ (${page} is out of bounds)`
        : `Canvas has pages 1–${pp} (${page} is out of bounds)`,
    );

  // `pageRange` is the slice `page` makes, over more than one page. It
  // counts the way `page` counts — from 1, negatives from the end — because
  // a range sitting next to that option and numbering differently would be a
  // trap rather than a convenience. Zero belongs to neither scheme and is
  // refused rather than read as one end of the canvas.
  let span;
  if (pageRange !== undefined) {
    if (page !== undefined)
      throw new TypeError(
        "Expected `page` or `pageRange`, not both (they answer the same question differently)",
      );
    if (
      !Array.isArray(pageRange) ||
      pageRange.length != 2 ||
      !Array.from(pageRange).every(Number.isInteger)
    )
      throw new TypeError(
        `Expected two integers for \`pageRange\` (got ${JSON.stringify(pageRange)})`,
      );
    // A single-page format has nothing to gather, unless the filename asks
    // for one file per page, which is a sequence and does.
    if (!spansPages(format) && !isSequence)
      throw new TypeError(
        `"${format}" encodes one page, so \`pageRange\` would do nothing ` +
          "(pass `page`, or a filename template like `frame-{}.png`)",
      );

    span = Array.from(pageRange, (n) => (n > 0 ? n - 1 : n < 0 ? pp + n : NaN));
    for (let end of [0, 1]) {
      if (!(span[end] >= 0 && span[end] < pp))
        throw new RangeError(
          pp == 1
            ? `Canvas only has a ‘page 1’ (${pageRange[end]} is out of bounds)`
            : `Canvas has pages 1–${pp} (${pageRange[end]} is out of bounds)`,
        );
    }
    if (span[0] > span[1])
      throw new RangeError(
        `\`pageRange\` ends before it begins (${pageRange[0]} to ${pageRange[1]})`,
      );
  }

  pages = isFinite(idx)
    ? [pages[idx]]
    : span
      ? pages.slice(span[0], span[1] + 1)
      : isSequence || spansPages(format)
        ? pages
        : pages.slice(-1); // default to the 'current' context

  // inherit text settings from the canvas (since they can't be changed on a per-render basis due to glyph caching)
  const { textContrast, textGamma } = canvas.engine;

  if (quality === undefined) {
    quality = 0.92;
  } else {
    if (
      typeof quality != "number" ||
      !isFinite(quality) ||
      quality < 0 ||
      quality > 1
    ) {
      throw new TypeError("Expected a number between 0.0–1.0 for `quality`");
    }
  }

  if (density === undefined) {
    let m = (extension || basename(filename, ext)).match(/@(\d+)x$/i);
    density = m ? parseInt(m[1], 10) : 1;
  } else if (typeof density != "number" || !isFinite(density) || density <= 0) {
    // Any positive number, not only a whole one. The integer rule came from
    // the `@2x` filename convention just above, which can only produce one
    // -- but that is a convention for naming files, not a constraint on a
    // scale factor, and 1.5 is an ordinary device pixel ratio. The Rust API
    // has always taken it, and the resolution every format records is
    // computed from it correctly.
    //
    // The message said "non-negative" while the check demanded 1 or more, so
    // it named a range containing two values it refused.
    throw new TypeError("Expected a positive number for `density`");
  }

  if (msaa === undefined || msaa === true) {
    msaa = undefined; // use the default 4x msaa
  } else if (!isFinite(+msaa) || +msaa < 0) {
    throw new TypeError("The number of MSAA samples must be an integer ≥0");
  }

  if (colorType !== undefined) {
    pixelSize(colorType); // throw an error if invalid
  }

  // Refused rather than dropped, as the timing options below are. A caller
  // who asked for a depth and got the default back has no way to tell -- the
  // file is a valid AVIF either way, just not the one they asked for.
  if (bitDepth !== undefined) {
    let taken = bitDepths(format);
    if (!taken.length) {
      throw new TypeError(
        `"${format}" takes its depth from the canvas, so \`bitDepth\` would ` +
          "do nothing (pass `colorType` to the canvas or the export instead)",
      );
    }
    if (!taken.includes(bitDepth)) {
      throw new RangeError(
        `Expected ${taken.join(", ").replace(/, ([^,]*)$/, ", or $1")} for ` +
          `\`bitDepth\` of "${format}" (got ${bitDepth})`,
      );
    }
  }

  // Animation timing. Validated here rather than in the addon so a mistake
  // names the argument that was wrong, and so `fps` reaching zero cannot be
  // divided by further down.
  // Timing given to a format with no clock is refused rather than dropped.
  // PNG, TIFF, ICO and the rest either encode one page or gather them all
  // untimed; a caller who asked for twelve frames a second and got a single
  // still image is owed the reason. `undefined` is what makes this
  // possible, so `fps` is only defaulted after the check.
  if (!animates(format)) {
    let named =
      fps !== undefined
        ? "`fps`"
        : frameDelays !== undefined
          ? "`frameDelays`"
          : loops !== undefined
            ? "`loop`"
            : null;
    if (named) {
      throw new TypeError(
        `"${format}" is not an animated format, so ${named} would do nothing ` +
          `(it encodes ${spansPages(format) ? "every page, untimed" : "one page"})`,
      );
    }
  }

  // Left undefined when unasked, rather than defaulted to 30 here: the
  // addon applies the same default, and sending one either way would make
  // "not asked for" indistinguishable from "asked for the default" -- which
  // is the state the check above needs to see.
  if (
    fps !== undefined &&
    (typeof fps != "number" || !isFinite(fps) || fps <= 0)
  ) {
    throw new TypeError("Expected a positive number for `fps`");
  }

  if (frameDelays === undefined) {
    frameDelays = [];
  } else if (!Array.isArray(frameDelays)) {
    throw new TypeError(
      "Expected an array of non-negative numbers for `frameDelays`",
    );
  } else if (
    // `Array.from`, because `some` and every other iteration method skip a
    // sparse array's holes. `new Array(3)` with only `[0]` assigned passed
    // this check and the length check below, and each hole then reached the
    // addon as `undefined` and was read as a zero-length frame -- so the
    // animation was retimed to nothing and nothing said so.
    Array.from(frameDelays).some(
      (ms) => typeof ms != "number" || !isFinite(ms) || ms < 0,
    )
  ) {
    throw new TypeError(
      "Expected an array of non-negative numbers for `frameDelays`",
    );
  } else if (frameDelays.length && frameDelays.length != pages.length) {
    // Silently ignoring a mismatched list would retime the animation without
    // saying so, which looks like the timing argument doing nothing.
    throw new TypeError(
      `Expected one entry in \`frameDelays\` per page (got ${frameDelays.length} for ${pages.length})`,
    );
  }

  if (loops === undefined) {
    loops = 0; // forever, which is what both formats spell as zero
  } else if (
    typeof loops != "number" ||
    !Number.isInteger(loops) ||
    loops < 0
  ) {
    throw new TypeError(
      "Expected a non-negative integer for `loop` (0 repeats forever)",
    );
  }

  // default to false, otherwise detect truthy
  downsample = !!downsample;
  outline = !!outline;

  return {
    filename,
    pattern,
    format,
    mime,
    pages,
    padding,
    quality,
    matte,
    density,
    msaa,
    outline,
    textContrast,
    textGamma,
    downsample,
    colorType,
    bitDepth,
    chromaSampling,
    lossless,
    colorSpace,
    fps,
    frameDelays,
    loop: loops,
  };
}

// emit a deprecation warning, once per API per process
let _warnings = {
  "Canvas.saveAs()": "Canvas.toFile()",
  "Canvas.saveAsSync()": "Canvas.toFileSync()",
  "Canvas.toDataURLSync()":
    "Canvas.toURLSync() (see also Canvas.toDataURL() which is now synchronous)",
};
function _deprecated(oldAPI) {
  let newAPI = _warnings[oldAPI];
  if (newAPI)
    console.error(
      `Deprecation warning: ${oldAPI} has been renamed to ${newAPI} and will stop working in a future release.`,
    );
  delete _warnings[oldAPI];
}

module.exports = {
  Canvas,
  CanvasGradient,
  CanvasPattern,
  CanvasTexture,
  getSharp,
};
