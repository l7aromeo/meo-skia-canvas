//
// Types for the browser build (lib/browser.js).
//
// Mostly a re-export list rather than a second set of declarations: the shapes
// stay in index.d.ts and cannot drift, and only the membership of this list has
// to be maintained. It must match `module.exports` in browser.js, which
// `tests/static/binary.test.js` checks.
//
// The Node build exports considerably more. App, Window, FontLibrary,
// CanvasTexture, ColorFilter, ImageFilter, MaskFilter, Shader, TextMetrics,
// Paragraph, ParagraphBuilder and backend are all backed by Skia, the
// filesystem or a GUI event loop, so they are absent here rather than declared
// and undefined. TextMetrics is absent for a different reason than the rest:
// `ctx.measureText()` in a page returns the platform's own, so re-exporting
// ours would describe an object nothing here produces.
//
// `Canvas` is narrowed below. Re-exporting it whole told three lies: that
// eleven image formats the browser cannot encode were available, that
// `toBuffer` resolves to a `Buffer`, and that the synchronous export methods
// exist at all.
//
// The narrowing then stopped short of two more. The `.raw`, `.pdf`, `.svg` and
// `.webp` shorthands were still declared, though `browser.js` defines only
// `.png` and `.jpg` -- and those two resolve to an `ArrayBuffer`, so even the
// pair that exists had the wrong type. And `gpu`, `engine`, `colorType` and
// `colorSpace` describe a Skia surface: there is none here, and reading any of
// them gives `undefined` rather than an answer about the browser's renderer.
//
// And `Canvas` was not the only one. Nine further names were re-exported from
// `./index` while `browser.js` takes them straight off `window`:
// CanvasRenderingContext2D, CanvasGradient, CanvasPattern, Image, ImageData,
// Path2D, DOMMatrix, DOMRect and DOMPoint. Nothing patches those globals, so
// every extension this fork adds was declared on a type that does not have it
// -- 19 phantom members on Path2D alone, 19 on the context, and `img.frames`,
// `imageData.colorType`, `gradient.interpolation` and `matrix.skew` besides.
// They are the DOM's types here, and are declared as such below.
//
// `loadImage` and `loadImageData` are defined in `browser.js` rather than
// re-exported, and differ from the Node pair in both directions: they take
// only a URL, and `loadImage` resolves to an `HTMLImageElement`. The Node
// overloads accept a `Buffer` or a Sharp image, neither of which exists in a
// page.
//

import type { Canvas as NodeCanvas, ExportOptions, SaveOptions } from "./index";

/**
 * Values re-exported from the Node build unchanged. The shapes stay in
 * `index.d.ts`, so only the membership of this list is maintained here.
 *
 * @category Shared with the Node Build
 */
export {
  ColorMatrix,
  PlaceholderAlignment,
  RectHeightStyle,
  RectWidthStyle,
  TextBaseline,
  TextDecoration,
  TextDecorationStyle,
} from "./index";

//
// The nine names below are the DOM's own types, which is what `browser.js`
// exports: it reads them off `window` at module scope and nothing patches
// them, so they carry the platform's API and none of this library's
// additions. They are exported by name because a page's bundler resolves this
// module rather than the globals, not because the shapes differ from
// `globalThis` -- and they need `"dom"` in the consuming project's `lib`,
// which a browser build has.
//
// Each is a type and a value, and both halves need documenting: TypeDoc counts
// them separately, and a comment on only one leaves the other undocumented
// with nothing saying so.
//

/**
 * The DOM's 2D context. None of this library's additions are on it -- no
 * `outlineText`, `createTexture`, `imageFilter`, `textWrap` or
 * `fontVariationSettings`.
 *
 * @category Shared with the Browser
 */
export type CanvasRenderingContext2D = globalThis.CanvasRenderingContext2D;
/**
 * The context constructor, which a page has for `instanceof` and nothing else:
 * contexts come from `canvas.getContext("2d")`.
 *
 * @category Shared with the Browser
 */
export const CanvasRenderingContext2D: typeof globalThis.CanvasRenderingContext2D;
/**
 * The DOM's `CanvasGradient`: `addColorStop`, and no `interpolation` or
 * `hueInterpolation`.
 *
 * @category Shared with the Browser
 */
export type CanvasGradient = globalThis.CanvasGradient;
/**
 * The gradient constructor. Gradients come from the context's
 * `createLinearGradient` and friends.
 *
 * @category Shared with the Browser
 */
export const CanvasGradient: typeof globalThis.CanvasGradient;
/**
 * The DOM's `CanvasPattern`.
 *
 * @category Shared with the Browser
 */
export type CanvasPattern = globalThis.CanvasPattern;
/**
 * The pattern constructor. Patterns come from `ctx.createPattern`.
 *
 * @category Shared with the Browser
 */
export const CanvasPattern: typeof globalThis.CanvasPattern;
/**
 * `HTMLImageElement`, which is what a page's `new Image()` produces. It has no
 * `frames`, `delays` or `currentColor`.
 *
 * @category Shared with the Browser
 */
export type Image = globalThis.HTMLImageElement;
/**
 * The `Image` constructor -- `HTMLImageElement`'s, taking optional width and
 * height rather than this library's decoding options.
 *
 * @category Shared with the Browser
 */
export const Image: typeof globalThis.Image;
/**
 * The DOM's `ImageData`: `data`, `width`, `height` and `colorSpace`, with no
 * `colorType`, `bytesPerPixel` or `toSharp`.
 *
 * @category Shared with the Browser
 */
export type ImageData = globalThis.ImageData;
/**
 * The `ImageData` constructor, which takes the DOM's `ImageDataSettings` --
 * a `colorSpace` and nothing else.
 *
 * @category Shared with the Browser
 */
export const ImageData: typeof globalThis.ImageData;
/**
 * The DOM's `Path2D`: the eleven path-building methods, and no `d`, `bounds`,
 * `edges`, `contains`, boolean operations or effects.
 *
 * @category Shared with the Browser
 */
export type Path2D = globalThis.Path2D;
/**
 * The `Path2D` constructor, taking another path or SVG path data.
 *
 * @category Shared with the Browser
 */
export const Path2D: typeof globalThis.Path2D;
/**
 * The DOM's `DOMMatrix`, without this library's `clone`, `skew` and
 * `skewSelf`.
 *
 * @category Shared with the Browser
 */
export type DOMMatrix = globalThis.DOMMatrix;
/**
 * The `DOMMatrix` constructor and its static readers, `fromMatrix`,
 * `fromFloat32Array` and `fromFloat64Array`.
 *
 * @category Shared with the Browser
 */
export const DOMMatrix: typeof globalThis.DOMMatrix;
/**
 * The DOM's `DOMRect`.
 *
 * @category Shared with the Browser
 */
export type DOMRect = globalThis.DOMRect;
/**
 * The `DOMRect` constructor and its static `fromRect`.
 *
 * @category Shared with the Browser
 */
export const DOMRect: typeof globalThis.DOMRect;
/**
 * The DOM's `DOMPoint`.
 *
 * @category Shared with the Browser
 */
export type DOMPoint = globalThis.DOMPoint;
/**
 * The `DOMPoint` constructor and its static `fromPoint`.
 *
 * @category Shared with the Browser
 */
export const DOMPoint: typeof globalThis.DOMPoint;

/**
 * Loads an image from a URL, resolving once it has decoded.
 *
 * `new Image()` with `crossOrigin` set to `"Anonymous"`, then `decode()`. The
 * Node overloads taking a `Buffer` or a Sharp image have no counterpart here,
 * and the result is an `HTMLImageElement` rather than this library's `Image`.
 *
 * @category Images and Pixel Data
 */
export function loadImage(src: string | URL): Promise<HTMLImageElement>;

/**
 * Fetches raw pixels into a DOM `ImageData`.
 *
 * The bytes are treated as pixels rather than as an encoded image, so `width`
 * is required. `settings` is the DOM's `ImageDataSettings`, which carries a
 * `colorSpace` and nothing else -- the Node build's `colorType` has no meaning
 * for an `ImageData` the browser owns.
 *
 * @category Images and Pixel Data
 */
export function loadImageData(
  src: string | URL,
  width: number,
  height?: number,
  settings?: ImageDataSettings,
): Promise<ImageData>;

/**
 * Types shared with the Node build.
 *
 * They carry no runtime weight, but they are not free of it either: a type
 * describing a member of a class this build does not have is unreachable, and
 * reads as a promise that something here produces one. `Path2DBounds`,
 * `Path2DEdge`, `TextMetricsLine` and `TextMetricsRun` were exported that way
 * -- the first two describe `.bounds` and `.edges` on a `Path2D` that is the
 * DOM's here, the last two describe `TextMetrics.lines`, and `measureText`
 * returns the platform's `TextMetrics`. `ExportFormat` is narrowed below
 * rather than dropped, because the browser really does encode three formats.
 *
 * @category Shared with the Node Build
 */
export type {
  Color4fInput,
  ColorSpace,
  ColorType,
  ExportOptions,
  RenderOptions,
  SaveOptions,
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
