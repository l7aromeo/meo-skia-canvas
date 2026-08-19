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

const loadImage = (src, options) =>
  new Promise((res, rej) =>
    fetchData(
      src,
      options,
      (data, src, raw) => {
        let img = new Image();
        img.prop("src", src);
        if (img.prop("data", data, raw)) res(img);
        else rej(DecodingError());
      },
      rej,
    ),
  );

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
    return this.prop("data", ...data);
  }

  constructor(data, src = "") {
    super(Image).alloc();

    data = expandURL(data);
    this.prop("src", "" + src || "::Buffer::");

    if (Buffer.isBuffer(data)) {
      if (!this.#decode(data)) throw DecodingError();
    } else if (typeof data == "string") {
      decodeDataURL(
        data,
        (buffer) => {
          if (!this.#decode(buffer)) throw DecodingError();
          if (!src) this.prop("src", data);
        },
        (err) => {
          throw err;
        },
      );
    } else if (data) {
      throw TypeError(
        `Exptected a Buffer or a String containing a data URL (got: ${data})`,
      );
    }
  }

  get complete() {
    return this.prop("complete");
  }
  get height() {
    return this.prop("height");
  }
  get width() {
    return this.prop("width");
  }

  // How many frames the source holds, and how long each is shown for in
  // milliseconds. A still image is one frame of no duration, so `delays`
  // is always `frames` long and the two cannot disagree.
  get frames() {
    return this.prop("frames");
  }
  get delays() {
    return this.prop("delays");
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
    img.prop("src", this.prop("src"));
    img.ƒ("takeFrame", core(this), index);
    return img;
  }

  // The intrinsic dimensions. In a browser these differ from width/height
  // because an `<img>` can be resized by attribute or by CSS; there is no
  // layout here, so width/height are already intrinsic and these are the same
  // measurement under the name the standard gives it.
  get naturalHeight() {
    return this.prop("height");
  }
  get naturalWidth() {
    return this.prop("width");
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
    return this.prop("src");
  }
  set src(src) {
    const request = (this.#fetch = {}); // use an empty object as a unique token
    const loaded = (data, imgSrc, raw) => {
      if (request === this.#fetch) {
        // confirm this is the most recent request with ===
        this.#fetch = undefined;
        this.prop("src", imgSrc);
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
    this.prop("src", typeof src == "string" ? src : "");

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
    let { colorType, colorSpace, ...options } = args[2] || {};
    fetchData(
      src,
      options,
      (data, src, raw) =>
        res(
          raw
            ? new ImageData(data, raw.width, raw.height)
            : new ImageData(data, ...args),
        ),
      rej,
    );
  });

class ImageData {
  constructor(...args) {
    if (args[0] instanceof ImageData) {
      argc(arguments, 1);
      var { data, width, height, colorSpace, colorType, bytesPerPixel } =
        args[0];
    } else if (args[0] instanceof Image) {
      argc(arguments, 1);
      var [image, { colorSpace = "srgb", colorType = "rgba" } = {}] = args,
        { width, height } = image,
        bytesPerPixel = pixelSize(colorType),
        buffer = neon.Image.pixels(core(image), { colorType }),
        data = new Uint8ClampedArray(buffer);
    } else if (
      args[0] instanceof Uint8ClampedArray ||
      args[0] instanceof Buffer
    ) {
      argc(arguments, 2);
      var [
          data,
          width,
          height,
          { colorSpace = "srgb", colorType = "rgba" } = {},
        ] = args,
        bytesPerPixel = pixelSize(colorType); // validates the string as side effect

      width = Math.floor(Math.abs(width));
      height = Math.floor(
        Math.abs(height || data.length / width / bytesPerPixel),
      );
      data =
        data instanceof Uint8ClampedArray ? data : new Uint8ClampedArray(data);
      if (data.length / bytesPerPixel != width * height) {
        throw new TypeError("ImageData dimensions must match buffer length");
      }
    } else {
      argc(arguments, 2);
      var [width, height, { colorSpace = "srgb", colorType = "rgba" } = {}] =
          args,
        bytesPerPixel = pixelSize(colorType);

      width = Math.floor(Math.abs(width));
      height = Math.floor(Math.abs(height));
    }

    if (!COLOR_SPACES.has(colorSpace)) {
      throw TypeError(`Unsupported colorSpace: ${colorSpace}`);
    }

    if (
      !Number.isInteger(width) ||
      !Number.isInteger(height) ||
      width <= 0 ||
      height <= 0
    ) {
      throw RangeError("Dimensions must be non-zero");
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
    let { width, height, bytesPerPixel: channels } = this;
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

function pixelSize(colorType) {
  const bpp = PIXEL_SIZES[colorType];
  if (!bpp) throw new TypeError(`Unknown colorType: ${colorType}`);
  return bpp;
}

function getSharp() {
  try {
    return require("sharp");
  } catch (e) {
    throw Error(
      "Cannot find module 'sharp' (try running `npm install sharp` first)",
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
};
