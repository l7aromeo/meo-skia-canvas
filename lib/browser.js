//
// Browser equivalents of the skia-canvas convenience initializers and polyfills for
// the Canvas object’s newPage & export methods
//
// OPTIONAL DEPENDENCY: be sure to include JSZip in your project bundle if you want to
// make use of multi-page toFile() downloads
//

"use strict";

const _toURL_ = Symbol.for("toDataURL");

const loadImage = (src) => {
  let img = Object.assign(new Image(), { crossOrigin: "Anonymous", src });
  return img.decode().then(() => img);
};

const loadImageData = (src, width, height, settings) =>
  fetch(src)
    .then((resp) => resp.arrayBuffer())
    .then(
      (buf) =>
        new ImageData(new Uint8ClampedArray(buf), width, height, settings),
    );

class Canvas {
  constructor(width, height) {
    let elt = document.createElement("canvas"),
      pages = [];

    for (var [prop, get] of Object.entries({
      png: () => asBuffer(elt, "image/png"),
      jpg: () => asBuffer(elt, "image/jpeg"),
      pages: () => pages.concat(elt).map((c) => c.getContext("2d")),
    }))
      Object.defineProperty(elt, prop, { get });

    return Object.assign(elt, {
      width,
      height,

      newPage(...size) {
        let { width, height } = elt,
          page = Object.assign(document.createElement("canvas"), {
            width,
            height,
          });
        page.getContext("2d").drawImage(elt, 0, 0);
        pages.push(page);

        // The snapshot above is taken at the size the canvas already has,
        // whatever this call goes on to resize it to. No argument leaves that
        // size in place; `newPage(w, h)` replaces it.
        if (size.length) [width, height] = size;
        return Object.assign(elt, { width, height }).getContext("2d");
      },

      saveAs() {
        throw Error("Canvas.saveAs() has been renamed to Canvas.toFile");
      },

      toFile(filename, args) {
        args = typeof args == "number" ? { quality: args } : args;
        let opts = exportOptions(this.pages, { filename, ...args }),
          { pattern, padding, mime, quality, matte, density, archive } = opts,
          pages = atScale(opts.pages, density);
        return padding == undefined
          ? asDownload(pages[0], mime, quality, matte, filename)
          : asZipDownload(
              pages,
              mime,
              quality,
              matte,
              archive,
              pattern,
              padding,
            );
      },

      toBuffer(extension = "png", args = {}) {
        args = typeof args == "number" ? { quality: args } : args;
        let opts = exportOptions(this.pages, { extension, ...args }),
          { mime, quality, matte, pages, density } = opts,
          canvas = atScale(pages, density, matte)[0];
        return asBuffer(canvas, mime, quality, matte);
      },

      toURL(extension = "png", args = {}) {
        args = typeof args == "number" ? { quality: args } : args;
        let opts = exportOptions(this.pages, { extension, ...args }),
          { mime, quality, matte, pages, density } = opts,
          canvas = atScale(pages, density, matte)[0],
          url = canvas.toDataURL(mime, quality);
        return Promise.resolve(url);
      },
    });
  }
}

//
// Browser helpers for converting canvas elements to blobs/buffers/files/zips
//

const asBlob = (canvas, mime, quality, matte) => {
  if (matte) {
    let { width, height } = canvas,
      comp = Object.assign(document.createElement("canvas"), { width, height }),
      ctx = comp.getContext("2d");
    ctx.fillStyle = matte;
    ctx.fillRect(0, 0, width, height);
    ctx.drawImage(canvas, 0, 0);
    canvas = comp;
  }

  return new Promise((res, rej) => canvas.toBlob(res, mime, quality));
};

const asBuffer = (...args) => asBlob(...args).then((b) => b.arrayBuffer());

const asDownload = async (canvas, mime, quality, matte, filename) => {
  _download(filename, await asBlob(canvas, mime, quality, matte));
};

const asZipDownload = async (
  pages,
  mime,
  quality,
  matte,
  archive,
  pattern,
  padding,
) => {
  // Only the import is caught. The `catch` used to sit on the whole chain, so
  // a failure inside `generateAsync`, `payload.file` or any `toBlob` was
  // reported as a missing JSZip -- telling a caller who had bundled it to
  // bundle it, and resolving as though the download had happened.
  let JSZip;
  try {
    ({ default: JSZip } = await import("jszip"));
  } catch {
    console.log(
      "Multi-page downloads require JSZip to be bundled: https://www.npmjs.com/package/jszip",
    );
    return;
  }

  let filenames = (i) =>
      pattern.replace("{}", String(i + 1).padStart(padding, "0")),
    zip = new JSZip(),
    folder = basename(archive, ".zip") || "archive",
    payload = zip.folder(folder);

  await Promise.all(
    pages.map(async (page, i) => {
      let filename = filenames(i); // serialize filename(s) before awaiting
      payload.file(filename, await asBlob(page, mime, quality, matte));
    }),
  );

  _download(`${folder}.zip`, await zip.generateAsync({ type: "blob" }));
};

const _download = (filename, blob) => {
  const href = window.URL.createObjectURL(blob),
    link = document.createElement("a");
  link.style.display = "none";
  link.href = href;
  link.setAttribute("download", filename);
  if (typeof link.download === "undefined") {
    link.setAttribute("target", "_blank");
  }
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  setTimeout(() => window.URL.revokeObjectURL(href), 100);
};

const atScale = (pages, density, matte) =>
  pages.map((page) => {
    if (density == 1 && !matte) return page.canvas;

    let scaled = document.createElement("canvas"),
      ctx = scaled.getContext("2d"),
      src = page.canvas ? page.canvas : page;
    scaled.width = src.width * density;
    scaled.height = src.height * density;
    if (matte) {
      ctx.fillStyle = matte;
      ctx.fillRect(0, 0, scaled.width, scaled.height);
    }
    ctx.scale(density, density);
    ctx.drawImage(src, 0, 0);
    return scaled;
  });

//
// Mime type <-> File extension mappings
//

class Format {
  constructor() {
    let png = "image/png",
      jpg = "image/jpeg",
      jpeg = "image/jpeg",
      webp = "image/webp";

    Object.assign(this, {
      toMime: this.toMime.bind(this),
      fromMime: this.fromMime.bind(this),
      expected: `"png", "jpg", or "webp"`,
      formats: { png, jpg, jpeg, webp },
      mimes: { [png]: "png", [jpg]: "jpg", [webp]: "webp" },
    });
  }

  toMime(ext) {
    return this.formats[(ext || "").replace(/^\./, "").toLowerCase()];
  }

  fromMime(mime) {
    return this.mimes[mime];
  }
}

//
// Validation of the options dict shared by the Canvas saveAs, toBuffer, and toDataURL methods
//

function basename(str, ext) {
  let stub = str.substring(str.lastIndexOf("/") + 1);
  return ext && stub.endsWith(ext) ? stub.slice(0, -ext.length) : stub;
}

function extname(str) {
  return str.substring(str.lastIndexOf("."));
}

function exportOptions(
  pages,
  {
    filename = "",
    extension = "",
    format,
    page,
    quality,
    matte,
    density,
    archive,
  } = {},
) {
  let { fromMime, toMime, expected } = new Format();
  archive = "" + (archive || "canvas");
  // Read before `format` is normalized, so an explicit `format` argument
  // still names the extension a filename pattern is built from.
  let ext = format || extension.replace(/@\d+x$/i, "") || extname(filename);
  format = fromMime(toMime(ext) || ext);
  let mime = toMime(format),
    pp = pages.length;

  if (!ext)
    throw new Error(
      `Cannot determine image format (use a filename extension or 'format' argument)`,
    );
  if (!format)
    throw new Error(`Unsupported file format "${ext}" (expected ${expected})`);
  if (!pp)
    throw new RangeError(
      `Canvas has no associated contexts (try calling getContext or newPage first)`,
    );

  let padding,
    isSequence,
    pattern = filename.replace(/{(\d*)}/g, (_, width) => {
      isSequence = true;
      width = parseInt(width, 10);
      padding = isFinite(width) ? width : isFinite(padding) ? padding : -1;
      return "{}";
    });

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

  // A filename template is the only thing that gathers pages here. There is no
  // format in this build that spans them: `Format` above holds png, jpg and
  // webp, so a `.pdf` name is refused as an unsupported format before reaching
  // this line, and `browser.d.ts` lists `pdf` among the members the browser
  // build does not have.
  pages = isFinite(idx) ? [pages[idx]] : isSequence ? pages : pages.slice(-1); // default to the 'current' context

  if (quality === undefined) {
    quality = 0.92;
  } else {
    if (
      typeof quality != "number" ||
      !isFinite(quality) ||
      quality < 0 ||
      quality > 1
    ) {
      throw new TypeError(
        "The quality option must be an number in the 0.0–1.0 range",
      );
    }
  }

  if (density === undefined) {
    let m = (extension || basename(filename, ext)).match(/@(\d+)x$/i);
    density = m ? parseInt(m[1], 10) : 1;
  } else if (typeof density != "number" || !isFinite(density) || density <= 0) {
    // Any positive number, not only a whole one, and the same message the Node
    // build gives. The integer rule came from the `@2x` filename convention
    // just above, which can only produce one -- but that is a convention for
    // naming files, not a constraint on a scale factor, and 1.5 is an ordinary
    // device pixel ratio. The message said "non-negative" while the check
    // demanded 1 or more, so it named a range containing two values it
    // refused.
    throw new TypeError("Expected a positive number for `density`");
  }

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
    archive,
  };
}

const {
  CanvasRenderingContext2D,
  CanvasGradient,
  CanvasPattern,
  Image,
  ImageData,
  Path2D,
  DOMMatrix,
  DOMRect,
  DOMPoint,
} = window;

// The only two modules worth requiring here: both are plain data with no native
// dependency. Everything else this package exports is backed by Skia, the
// filesystem or a GUI event loop, and cannot exist in a browser.
const { ColorMatrix } = require("./classes/color_matrix"),
  {
    PlaceholderAlignment,
    RectHeightStyle,
    RectWidthStyle,
    TextBaseline,
    TextDecoration,
    TextDecorationStyle,
  } = require("./classes/text_decoration");

module.exports = {
  Canvas,
  loadImage,
  loadImageData,
  CanvasRenderingContext2D,
  CanvasGradient,
  CanvasPattern,
  Image,
  ImageData,
  Path2D,
  DOMMatrix,
  DOMRect,
  DOMPoint,
  ColorMatrix,
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
};
