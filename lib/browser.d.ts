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

import type { Canvas as NodeCanvas, ExportOptions, SaveOptions } from "./index";

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
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
  loadImage,
  loadImageData,
} from "./index";

// Types carry no runtime weight, so the browser build can use the same ones.
// `ExportFormat` is the exception -- see below.
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
 */
export type ExportFormat = "png" | "jpg" | "jpeg" | "webp";

/**
 * Members of the Node `Canvas` that `browser.js` never defines.
 *
 * The synchronous exports have no counterpart: every path to bytes in a page
 * goes through `toBlob`, which is asynchronous. `toSharp` needs a Node image
 * pipeline. `saveAs` is defined, but only to throw the message telling you it
 * was renamed to `toFile`, which is not worth a signature.
 */
type Absent =
  | "saveAs"
  | "saveAsSync"
  | "toBufferSync"
  | "toDataURLSync"
  | "toFileSync"
  | "toSharp"
  | "toSharpSync"
  | "toURLSync";

/** Members the browser build defines with a different shape. */
type Narrowed = "toBuffer" | "toFile" | "toURL";

/**
 * Compile-time proof that every name above is really a member of the Node
 * `Canvas`. `Omit` accepts keys that do not exist and silently does nothing,
 * so a rename upstream would quietly stop narrowing anything -- which is how
 * the declarations this replaced came to describe a class that had moved on.
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
   * here, so `filename` names the download. A multi-page canvas is zipped,
   * which needs JSZip in your bundle.
   */
  toFile(filename: string, options?: SaveOptions | number): Promise<void>;
}

export const Canvas: {
  new (width?: number, height?: number): Canvas;
  prototype: Canvas;
};
