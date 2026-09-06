//
// Image & ImageData
//

"use strict";

const {
    RustClass,
    core,
    readOnly,
    inspect,
    neon,
    argc,
    skiaNode,
    REPR,
    ALLOC,
    PROP,
    CALL,
  } = require("./neon"),
  drawlist = require("./drawlist"),
  { fetchURL, decodeDataURL, expandURL } = require("../urls"),
  { EventEmitter } = require("events"),
  { readFile } = require("fs/promises");

//
// Image
//

const DecodingError = () => new Error("Could not decode image data");

// Every name `ColorSpace` accepts. Checked at construction so
// `new ImageData(w, h, { colorSpace })` fails where the mistake is, as it does
// in Chrome, rather than later inside a draw.
//
// Mirrors the parser in `src/node/utils.rs`; a test round-trips every name so
// the two cannot drift apart silently.
const COLOR_SPACES = new Set([
  "srgb",
  "srgb-linear",
  "linear",
  "display-p3",
  "p3",
  "display-p3-linear",
  "p3-linear",
  "rec2020",
  "bt2020",
  "rec2020-linear",
  "bt2020-linear",
  "rec2020-pq",
  "hdr10",
  "rec2020-hlg",
  "hlg",
]);

// `currentColor` is peeled off the way `loadImageData` peels `colorType` and
// `colorSpace`: what reaches `fetchData` is the fetch options and nothing
// else. Setting it before `data` is what makes this the cheap path -- the
// document is recorded once, with the colour already applied, where setting
// the property afterwards records a second time.
const loadImage = (src, allOptions) =>
  new Promise((res, rej) => {
    let { currentColor, ...options } = allOptions || {};
    fetchData(
      src,
      options,
      (data, src, raw) => {
        let img = new Image();
        img[PROP]("src", src);
        if (currentColor != null) img[PROP]("currentColor", currentColor);
        if (img[PROP]("data", data, raw)) res(img);
        else rej(DecodingError());
      },
      rej,
    );
  });

class Image extends RustClass {
  #fetch;
  #err;

  /**
   * Replaces the pixels, after anything queued to draw the old ones.
   *
   * A recorded `drawImage` keeps this object and reads its content when the
   * batch lands, so this is the moment that content must not change under a
   * record still pointing at it. It is also the only moment it does change,
   * which is why the drain is written here by name rather than left to the
   * accessor that does it for a class recording its own verbs: `complete` is
   * read once per `drawImage`, and draining there would hand over a batch of
   * one every time and leave the recording worth nothing.
   */
  #decode(...data) {
    drawlist.drain(this);
    return this[PROP]("data", ...data);
  }

  constructor(data, src = "") {
    super(Image)[ALLOC]();

    data = expandURL(data);
    this[PROP]("src", "" + src || "::Buffer::");

    if (Buffer.isBuffer(data)) {
      if (!this.#decode(data)) throw DecodingError();
    } else if (typeof data == "string") {
      decodeDataURL(
        data,
        (buffer) => {
          if (!this.#decode(buffer)) throw DecodingError();
          if (!src) this[PROP]("src", data);
        },
        (err) => {
          throw err;
        },
      );
    } else if (data) {
      throw TypeError(
        `Expected a Buffer or a String containing a data URL (got: ${data})`,
      );
    }
  }

  get complete() {
    return this[PROP]("complete");
  }
  get height() {
    return this[PROP]("height");
  }
  get width() {
    return this[PROP]("width");
  }

  // What `currentColor` in an SVG source resolves to. `null` until set, and
  // the value that was set rather than what any particular shape ended up
  // painted -- a subtree declaring its own `color` resolves against that
  // instead, so there is no one colour to report.
  //
  // Assigning before `src` costs nothing: the document is recorded once with
  // the colour applied. Assigning after re-records it.
  get currentColor() {
    return this[PROP]("currentColor");
  }
  set currentColor(color) {
    this[PROP]("currentColor", color);
  }

  // How many frames the source holds, and how long each is shown for in
  // milliseconds. A still image is one frame of no duration, so `delays`
  // is always `frames` long and the two cannot disagree.
  get frames() {
    return this[PROP]("frames");
  }
  get delays() {
    return this[PROP]("delays");
  }

  // One frame, as an Image of its own, composited against the frames before
  // it so it can be drawn on its own and asked for in any order.
  //
  // A negative index counts from the end, so `frame(-1)` is the last one --
  // the same rule `page` follows in the export options, and the one
  // `Array.prototype.at` follows.
  //
  // Nothing advances a frame by itself -- there is no clock here. An
  // animation plays because the caller picks the frame each output frame
  // shows.
  frame(index = 0) {
    let img = new Image();
    img[PROP]("src", this[PROP]("src"));
    img[CALL]("takeFrame", core(this), index);
    return img;
  }

  // The intrinsic dimensions. In a browser these differ from width/height
  // because an `<img>` can be resized by attribute or by CSS; there is no
  // layout here, so width/height are already intrinsic and these are the same
  // measurement under the name the standard gives it.
  get naturalHeight() {
    return this[PROP]("height");
  }
  get naturalWidth() {
    return this[PROP]("width");
  }

  #onload;
  get onload() {
    return this.#onload;
  }
  set onload(cb) {
    if (this.#onload) this.off("load", this.#onload);
    this.#onload = typeof cb == "function" ? cb : null;
    if (this.#onload) this.on("load", this.#onload);
  }

  #onerror;
  get onerror() {
    return this.#onerror;
  }
  set onerror(cb) {
    if (this.#onerror) this.off("error", this.#onerror);
    this.#onerror = typeof cb == "function" ? cb : null;
    if (this.#onerror) this.on("error", this.#onerror);
  }

  get src() {
    return this[PROP]("src");
  }
  set src(src) {
    const request = (this.#fetch = {}); // use an empty object as a unique token
    const loaded = (data, imgSrc, raw) => {
      if (request === this.#fetch) {
        // confirm this is the most recent request with ===
        this.#fetch = undefined;
        this[PROP]("src", imgSrc);
        this.#err = this.#decode(data, raw) ? null : DecodingError();
        if (this.#err) this.emit("error", this.#err);
        else this.emit("load", this);
      }
    };
    const failed = (err) => {
      if (request === this.#fetch) {
        // confirm this is the most recent request with ===
        this.#fetch = undefined;
        this.#err = err;
        this.#decode(Buffer.alloc(0));
        this.emit("error", err);
      }
    };

    src = expandURL(src);
    this[PROP]("src", typeof src == "string" ? src : "");

    fetchData(src, undefined, loaded, failed);
  }

  decode() {
    return this.#fetch
      ? new Promise((res, rej) => this.once("load", res).once("error", rej))
      : this.#err
        ? Promise.reject(this.#err)
        : this.complete
          ? Promise.resolve(this)
          : Promise.reject(new Error("Image source not set"));
  }

  [REPR](depth, options) {
    let { width, height, complete, src } = this;
    options.maxStringLength = src.match(/^data:/) ? 128 : Infinity;
    return `Image ${inspect({ width, height, complete, src }, options)}`;
  }
}

// Mix the EventEmitter properties into Image
Object.assign(Image.prototype, EventEmitter.prototype);

//
// ImageData
//

const loadImageData = (src, ...args) =>
  new Promise((res, rej) => {
    // The two the `ImageData` constructor reads are split off here so what
    // reaches `fetchData` is the fetch options and nothing else.
    let { colorType, colorSpace, ...options } = args[2] || {};
    fetchData(
      src,
      options,
      (data, src, raw) => {
        // The decoded path hands the constructor the caller's own options
        // object, so both settings arrive there already.
        if (!raw) return res(new ImageData(data, ...args));

        // `raw` means a sharp source, which `fetchData` takes through
        // `.ensureAlpha().raw()` -- eight-bit RGBA by construction. A
        // `colorType` naming anything else describes those bytes wrongly and
        // fails the constructor's length check, so it is refused by name
        // rather than dropped, the way `exportOptions` refuses an option it
        // cannot honour.
        if (colorType !== undefined && colorType !== "rgba")
          return rej(
            TypeError(
              `Cannot honor colorType "${colorType}" here: a sharp source arrives as eight-bit rgba`,
            ),
          );

        // `colorSpace` is a tag rather than a conversion -- the bytes are
        // identical either way -- so the caller's assertion about the pixels
        // carries through unchanged, exactly as it does on the decoded path.
        // Dropping it made the same call honor the setting or discard it
        // depending on what the fetch returned rather than on how it was
        // written.
        res(new ImageData(data, raw.width, raw.height, { colorSpace }));
      },
      rej,
    );
  });

class ImageData {
  constructor(...args) {
    // One binding each for the whole dispatch below. Every arm reads a
    // different argument shape and fills the same six, and everything after
    // the chain reads them without caring which arm ran. An arm has to fill
    // all six: nothing here enforces that, and the checks below catch only
    // `colorSpace` and the dimensions -- a missing `bytesPerPixel` or `data`
    // reaches `readOnly` as `undefined`.
    let data, width, height, colorSpace, colorType, bytesPerPixel;

    if (args[0] instanceof ImageData) {
      argc(arguments, 1);
      ({ data, width, height, colorSpace, colorType, bytesPerPixel } = args[0]);
    } else if (args[0] instanceof Image) {
      argc(arguments, 1);
      const [image, options = {}] = args;
      ({ colorSpace = "srgb", colorType = "rgba" } = options);
      ({ width, height } = image);
      bytesPerPixel = pixelSize(colorType);
      const buffer = neon.Image.pixels(core(image), { colorType });
      data = new Uint8ClampedArray(buffer);
    } else if (
      args[0] instanceof Uint8ClampedArray ||
      args[0] instanceof Buffer
    ) {
      argc(arguments, 2);
      const [source, rawWidth, rawHeight, options = {}] = args;
      ({ colorSpace = "srgb", colorType = "rgba" } = options);
      bytesPerPixel = pixelSize(colorType); // validates the string as side effect

      width = Math.floor(Math.abs(rawWidth));
      // A height of zero or `undefined` is read off the buffer instead, so
      // it needs the source length rather than the converted array's.
      height = Math.floor(
        Math.abs(rawHeight || source.length / width / bytesPerPixel),
      );
      data =
        source instanceof Uint8ClampedArray
          ? source
          : new Uint8ClampedArray(source);
      // Two refusals, not one. The standard separates a buffer that cannot
      // describe whole pixels at all -- an `InvalidStateError` -- from one
      // that describes a different number of them than the dimensions ask
      // for, which is an `IndexSizeError`. Both were a `TypeError` naming
      // "buffer length", which is this class's internal arithmetic rather
      // than anything the caller wrote.
      // Both comparisons are skipped when the geometry is not a number,
      // because the dimension check below names that complaint properly. A
      // zero width makes the derived height NaN, and comparing a buffer
      // against it reported "0 pixels, which is not 0xNaN".
      if (Number.isFinite(width) && Number.isFinite(height)) {
        if (data.length % bytesPerPixel != 0) {
          throw new DOMException(
            `The data length (${data.length}) is not a multiple of ` +
              `${bytesPerPixel}, the size of one "${colorType}" pixel`,
            "InvalidStateError",
          );
        }
        if (data.length / bytesPerPixel != width * height) {
          throw new DOMException(
            `The data length (${data.length}) describes ` +
              `${data.length / bytesPerPixel} pixels, not the ` +
              `${width}x${height} asked for`,
            "IndexSizeError",
          );
        }
      }
    } else {
      argc(arguments, 2);
      const [rawWidth, rawHeight, options = {}] = args;
      ({ colorSpace = "srgb", colorType = "rgba" } = options);
      bytesPerPixel = pixelSize(colorType);

      width = Math.floor(Math.abs(rawWidth));
      height = Math.floor(Math.abs(rawHeight));
    }

    if (!COLOR_SPACES.has(colorSpace)) {
      throw TypeError(`Unsupported colorSpace: ${colorSpace}`);
    }

    // `IndexSizeError` rather than a `RangeError`, because the standard names
    // it: "If one or both of sw and sh are zero, then throw an
    // "IndexSizeError" DOMException." Every entry point that builds an
    // `ImageData` funnels through here, so `getImageData(0, 0, 0, 0)`,
    // `createImageData(0, 0)` and `new ImageData(0, 0)` answer alike.
    if (
      !Number.isInteger(width) ||
      !Number.isInteger(height) ||
      width <= 0 ||
      height <= 0
    ) {
      throw new DOMException(
        "The source width or height is zero, negative or not a number " +
          `(got ${width}x${height})`,
        "IndexSizeError",
      );
    }

    // The same limit `checked_byte_size` applies in `src/context/page.rs`, and
    // for the same reason: Skia measures a pixel buffer with a signed 32-bit
    // byte count. The addon refuses a readback past it with this message; the
    // allocation below had no guard at all, and V8 does not raise an exception
    // for an oversized typed array -- it aborts the process with "Check
    // failed: change_in_bytes < kMaxReasonableBytes", which no `catch` can
    // reach. `ctx.createImageData(100000, 100000)` was enough.
    const bytes = width * height * bytesPerPixel;
    if (bytes > MAX_PIXEL_BYTES) {
      throw TypeError(
        `Requested image data is too large: ${width}x${height} at ` +
          `${colorType} exceeds the ${MAX_PIXEL_BYTES} byte limit Skia can address`,
      );
    }

    readOnly(this, "colorSpace", colorSpace);
    readOnly(this, "colorType", colorType);
    readOnly(this, "width", width);
    readOnly(this, "height", height);
    readOnly(this, "bytesPerPixel", bytesPerPixel);
    readOnly(
      this,
      "data",
      data || new Uint8ClampedArray(width * height * bytesPerPixel),
    );
  }

  toSharp() {
    const sharp = getSharp();
    let { width, height, colorType } = this,
      channels = sharpChannels(colorType);
    return sharp(this.data, { raw: { width, height, channels } }).withMetadata({
      density: 72,
    });
  }

  [REPR](depth, options) {
    let { width, height, colorType, bytesPerPixel, data } = this;
    return `ImageData ${inspect({ width, height, colorType, bytesPerPixel, data }, options)}`;
  }
}

//
// Utilities
//

// Bytes a pixel for each `colorType`, from the addon rather than from a copy
// of the list. The copy is what this replaced: it and the addon's own table
// had drifted, so `"N32"` was a type the addon accepted and this threw on,
// and `"RGBA8888"` appeared twice in the four-byte row -- as it did in the
// `ColorType` union the row was written from.
const PIXEL_SIZES = Object.fromEntries(
  JSON.parse(skiaNode.colorTypes()).map(({ name, bytes }) => [name, bytes]),
);

// The largest pixel buffer Skia can address, which is what bounds an
// `ImageData` rather than anything about JavaScript. Skia measures a buffer
// with a signed 32-bit byte count, so this is `i32::MAX` -- the same value
// `checked_byte_size` compares against in `src/context/page.rs`, written the
// same way it is derived rather than as the decimal it comes to.
const MAX_PIXEL_BYTES = 2 ** 31 - 1;

function pixelSize(colorType) {
  const bpp = PIXEL_SIZES[colorType];
  if (!bpp) throw new TypeError(`Unknown colorType: ${colorType}`);
  return bpp;
}

// What sharp's raw reader can be told about a buffer: a channel count, and
// nothing else. Its `raw` input option is `{width, height, channels}` -- there
// is no depth and no channel order, so the bytes have to already be 8-bit
// unsigned, one byte per channel, in sharp's own order. (`depth` exists on
// sharp's raw *output*, which is a different option and does not help here.)
//
// So `bytesPerPixel` is the wrong number to hand over. It equals the channel
// count only for the 8-bit types, which is why passing one as the other
// survived: `rgba` is 4 of each and `Gray8` is 1 of each. `RGB565` is two
// bytes and three channels, and sharp reading it as two 8-bit channels turns a
// red fill black.
const SHARP_CHANNELS_BY_TYPE = {
  // One byte, one channel. sharp reads these as greyscale, which is what the
  // stored byte is for `Gray8`, and is at least a faithful copy of the byte
  // for the other two.
  Alpha8: 1,
  Gray8: 1,
  R8UNorm: 1,
  // Two bytes, two channels, read as greyscale plus alpha.
  R8G8UNorm: 2,
  // Four bytes in red-green-blue-alpha order, which is the order sharp
  // assumes. `RGB888x` and `rgb` carry an ignored fourth byte rather than an
  // alpha, and sharp reading it as alpha is harmless: the addon writes 255
  // there.
  rgb: 4,
  RGB888x: 4,
  rgba: 4,
  RGBA8888: 4,
  SRGBA8888: 4,
};

// The format `Canvas.toSharp` asks the raw exporter for, rather than passing
// whatever the canvas happens to hold. The exporter converts on the way out --
// verified across every published `colorType`, packed and float included --
// so this makes the channel count below a fact rather than an assumption, and
// costs a conversion only for a canvas that was not already 8-bit RGBA.
const SHARP_COLOR_TYPE = "rgba";
const SHARP_CHANNELS = SHARP_CHANNELS_BY_TYPE[SHARP_COLOR_TYPE];

/**
 * How many 8-bit channels sharp should be told `colorType` has.
 *
 * Throws for a layout sharp cannot express -- anything packed, wider than a
 * byte per channel, or ordered blue-first. `Canvas.toSharp()` has no such
 * restriction: it converts on the way out.
 */
function sharpChannels(colorType) {
  const channels = SHARP_CHANNELS_BY_TYPE[colorType];
  if (channels) return channels;

  // Named so the message says which of the two problems it is. A blue-first
  // buffer is the dangerous one: sharp accepts it, the byte count is right,
  // and only the colours are wrong. The addon canonicalises its aliases before
  // an `ImageData` carries one -- `BGRA8888` and, where it is native, `N32`
  // both arrive as `bgra` -- so matching that one name covers every spelling.
  const swapped = ["bgra", "BGRA8888"].includes(colorType);
  throw new TypeError(
    `sharp cannot read \`${colorType}\` pixels directly ` +
      (swapped
        ? "(its channels are ordered blue-first, and sharp's raw reader has no way to be told that)"
        : `(sharp's raw reader takes 8-bit channels, and \`${colorType}\` is ${pixelSize(colorType)} bytes a pixel, packed or wider)`) +
      ". Use `canvas.toSharp()`, which converts, or read the pixels with a " +
      "`colorType` of `rgba`.",
  );
}

function getSharp() {
  try {
    return require("sharp");
  } catch (e) {
    throw Error(
      "Cannot find module 'sharp' (try running `npm install sharp` first)",
      { cause: e },
    );
  }
}

function isSharpImage(obj) {
  try {
    return obj instanceof require("sharp");
  } catch {
    return false;
  }
}

const fetchData = (src, reqOpts, loaded, failed) => {
  src = expandURL(src);
  if (Buffer.isBuffer(src)) {
    loaded(src, "::Buffer::");
  } else if (isSharpImage(src)) {
    src
      .ensureAlpha()
      .raw()
      .toBuffer((err, buf, info) => {
        let {
          options: {
            input: { file, buffer },
          },
        } = src;
        if (err) failed(err);
        else loaded(buf, buffer ? "::Sharp::" : file, info);
      });
  } else {
    src = typeof src == "string" ? src : "" + src;
    if (src.startsWith("data:")) {
      decodeDataURL(
        src,
        (buffer) => loaded(buffer, src),
        (err) => failed(err),
      );
    } else if (/^\s*https?:\/\//.test(src)) {
      fetchURL(
        src,
        reqOpts,
        (buffer) => loaded(buffer, src),
        (err) => failed(err),
      );
    } else {
      readFile(src)
        .then((data) => loaded(data, src))
        .catch((e) => failed(e));
    }
  }
};

module.exports = {
  Image,
  ImageData,
  loadImage,
  loadImageData,
  pixelSize,
  getSharp,
  sharpChannels,
  SHARP_COLOR_TYPE,
  SHARP_CHANNELS,
};
