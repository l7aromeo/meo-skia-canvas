//
// Types for the browser build (lib/browser.js).
//
// Mostly a re-export list rather than a second set of declarations: the shapes
// stay in index.d.ts and cannot drift, and only the membership of this list has
// to be maintained. It must match `module.exports` in browser.js, which
// `tests/static/binary.test.js` checks.
//
// The Node build exports considerably more. App, Window, FontLibrary,
// CanvasTexture, ColorFilter, ImageFilter, MaskFilter, Shader, Paragraph,
// ParagraphBuilder and backend are all backed by Skia, the filesystem or a GUI
// event loop, so they are absent here rather than declared and undefined.
//
// `Canvas` is the exception, and is narrowed below. Re-exporting it whole told
// three lies: that eleven image formats the browser cannot encode were
// available, that `toBuffer` resolves to a `Buffer`, and that the synchronous
// export methods exist at all.
//
// The narrowing then stopped short of two more. The `.raw`, `.pdf`, `.svg` and
// `.webp` shorthands were still declared, though `browser.js` defines only
// `.png` and `.jpg` -- and those two resolve to an `ArrayBuffer`, so even the
// pair that exists had the wrong type. And `gpu`, `engine`, `colorType` and
// `colorSpace` describe a Skia surface: there is none here, and reading any of
// them gives `undefined` rather than an answer about the browser's renderer.
//

import type { Canvas as NodeCanvas, ExportOptions, SaveOptions } from "./index";

/**
 * Values re-exported from the Node build unchanged. The shapes stay in
 * `index.d.ts`, so only the membership of this list is maintained here.
 *
 * @category Shared with the Node Build
 */
export {
  CanvasGradient,
  CanvasPattern,
  CanvasRenderingContext2D,
  ColorMatrix,
  DOMMatrix,
  DOMPoint,
  DOMRect,
  Image,
  ImageData,
  Path2D,
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
  loadImage,
  loadImageData,
} from "./index";

/**
 * Types shared with the Node build. They carry no runtime weight, so there is
 * nothing to leave out. `ExportFormat` is the exception, and is narrowed
 * below.
 *
 * @category Shared with the Node Build
 */
export type {
  Color4fInput,
  ColorSpace,
  ColorType,
  ExportOptions,
  Path2DBounds,
  Path2DEdge,
  RenderOptions,
  SaveOptions,
  TextMetricsLine,
  TextMetricsRun,
} from "./index";

/**
 * The formats a browser can encode.
 *
 * Three, and not because of anything this library chose:
 * `HTMLCanvasElement.toBlob` and `toDataURL` are what produce the bytes here,
 * and they write PNG, JPEG and WebP. GIF, APNG, TIFF, ICO, BMP, AVIF, PDF and
 * SVG are encoders in this project's Rust, which a page does not have — so
 * asking for one throws `Unsupported file format`.
 *
 * Reach for them by drawing in the browser and encoding in Node, or by
 * feeding `getImageData` to a WebAssembly encoder in your own bundle.
 *
 * @category Exporting
 */
export type ExportFormat = "png" | "jpg" | "jpeg" | "webp";

/**
 * Members of the Node `Canvas` that `browser.js` never defines.
 *
 * The synchronous exports have no counterpart: every path to bytes in a page
 * goes through `toBlob`, which is asynchronous. `toSharp` needs a Node image
 * pipeline. `saveAs` is defined, but only to throw the message telling you it
 * was renamed to `toFile`, which is not worth a signature.
 *
 * The rest are the parts that describe a Skia surface. `gpu`, `engine`,
 * `colorType` and `colorSpace` report on a renderer this build does not have:
 * the pixels belong to the element, and the browser decides how they are
 * composited. The `raw`, `pdf` and `svg` shorthands are encoders that live in
 * this project's Rust, and `webp` -- which the browser *can* encode -- simply
 * has no getter in the shim, so reach it through `toBuffer("webp")`.
 */
type Absent =
  | "colorSpace"
  | "colorType"
  | "engine"
  | "gpu"
  | "pdf"
  | "raw"
  | "saveAs"
  | "saveAsSync"
  | "svg"
  | "toBufferSync"
  | "toDataURLSync"
  | "toFileSync"
  | "toSharp"
  | "toSharpSync"
  | "toURLSync"
  | "webp";

/** Members the browser build defines with a different shape. */
type Narrowed = "jpg" | "png" | "toBuffer" | "toFile" | "toURL";

/**
 * Compile-time proof that every name above is really a member of the Node
 * `Canvas`. `Omit` accepts keys that do not exist and silently does nothing,
 * so a rename in `Canvas` would quietly stop narrowing anything -- which is
 * how the declarations this replaced came to describe a class that had moved
 * on.
 */
type MemberOfCanvas<K extends keyof NodeCanvas> = K;
type _AssertNamesExist = MemberOfCanvas<Absent | Narrowed>;

/**
 * A canvas in the browser: a real `<canvas>` element with this library's
 * drawing and export API assigned onto it.
 *
 * Because it *is* the element, everything `HTMLCanvasElement` offers is there
 * at runtime — `toBlob`, `getContext`, `width`, `height` — and the DOM types
 * describe those. What this adds is the parts of the Node API the shim
 * implements, with the return types the browser actually produces.
 *
 * @category Canvas
 */
export interface Canvas extends Omit<NodeCanvas, Absent | Narrowed> {
  /**
   * The encoded bytes, as an `ArrayBuffer` rather than Node's `Buffer`: this
   * comes from `Blob.arrayBuffer()`, and there is no `Buffer` in a page.
   *
   * `format` defaults to PNG. A number in place of the options is read as
   * `quality`, as it is in the Node build.
   */
  toBuffer(
    format?: ExportFormat,
    options?: ExportOptions | number,
  ): Promise<ArrayBuffer>;

  /** A `data:` URL. `format` defaults to PNG. */
  toURL(
    format?: ExportFormat,
    options?: ExportOptions | number,
  ): Promise<string>;

  /**
   * Downloads the canvas rather than writing a file: there is no filesystem
   * here, so `filename` names the download and the browser puts it wherever
   * downloads go.
   *
   * A filename containing `"{}"` downloads every page as a zip archive
   * instead, one numbered file inside it -- the same `"{}"` that writes a
   * numbered sequence in Node. Page count alone does not trigger it: without
   * the braces this downloads the current page, as the other exporters do.
   * The archive is named by the `archive` option, defaulting to
   * `"canvas.zip"`.
   *
   * Zipping needs [JSZip](https://www.npmjs.com/package/jszip) in your
   * bundle. Without it the promise still resolves and nothing downloads --
   * the reason is logged to the console rather than thrown.
   */
  toFile(
    filename: string,
    options?: (SaveOptions & { archive?: string }) | number,
  ): Promise<void>;

  /**
   * The canvas as PNG bytes -- `toBuffer("png")` with no arguments, and an
   * `ArrayBuffer` rather than Node's `Buffer`.
   */
  readonly png: Promise<ArrayBuffer>;

  /**
   * The canvas as JPEG bytes, as an `ArrayBuffer`.
   *
   * No quality is passed, so the browser's own default applies rather than
   * the 0.92 the Node build uses. Call `toBuffer("jpg", 0.8)` to choose one.
   */
  readonly jpg: Promise<ArrayBuffer>;
}

/**
 * The browser `Canvas` constructor.
 *
 * `new Canvas(w, h)` returns a `<canvas>` element with the export methods
 * assigned onto it, so the result is a DOM node: it can be appended to the
 * document, and `instanceof HTMLCanvasElement` holds where
 * `instanceof Canvas` does not.
 *
 * Unlike the Node constructor it takes no options object: pixel format,
 * color space and renderer are the browser's to choose. Both dimensions are
 * assigned onto the element as given, so `new Canvas()` is 0 x 0 rather than
 * the 300 x 150 an empty `<canvas>` would be -- pass a size, or set `width`
 * and `height` afterwards.
 *
 * @category Canvas
 */
export const Canvas: {
  /** Create a canvas element of `width` x `height` pixels. */
  new (width?: number, height?: number): Canvas;
  /** The shape instances share. Assigning to it affects nothing: the
   * constructor returns an element, not an object built from this. */
  prototype: Canvas;
};
