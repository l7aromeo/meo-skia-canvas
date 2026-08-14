// Type-only, and `sharp` is an optional peer: the runtime never requires it.
// A consumer who wants these signatures to resolve has to install it, which
// is what the optional peer declaration asks for.
import type { Sharp } from "sharp";

// `DOMPointReadOnly` and `DOMRectReadOnly` are kept as interfaces because
// `DOMPoint` and `DOMRect` genuinely extend them, and both satisfy the
// contract. Their constructors are deliberately absent: this package never
// implemented either as a runtime value, and declaring them let
// `new DOMPointReadOnly()` typecheck and then throw.
//
// Their purpose does not carry over regardless. In a browser these exist so
// an observer can hand you geometry you cannot mutate -- `contentRect`,
// `boundingClientRect`, `currentTranslate`. Every one of those producers is a
// DOM or layout API, and none can exist here, so nothing will ever return one.
//
// `DOMRectList` is gone entirely, interface and all: it only exists in
// lib.dom because `Element.getClientRects()` returns one, nothing extends it,
// and no signature here mentions it.

//
// Geometry
//

interface DOMPointInit {
  x?: number;
  y?: number;
  z?: number;
  w?: number;
}

/** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint) */
interface DOMPoint extends DOMPointReadOnly {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint/x) */
  x: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint/y) */
  y: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint/z) */
  z: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint/w) */
  w: number;
}

declare var DOMPoint: {
  prototype: DOMPoint;
  new (x?: number, y?: number, z?: number, w?: number): DOMPoint;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPoint/fromPoint_static) */
  fromPoint(other?: DOMPointInit): DOMPoint;
};

/** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly) */
interface DOMPointReadOnly {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/x) */
  readonly x: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/y) */
  readonly y: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/z) */
  readonly z: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/w) */
  readonly w: number;
  matrixTransform(matrix?: DOMMatrixInit): DOMPoint;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/toJSON) */
  toJSON(): any;
}

/** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRect) */
interface DOMRect extends DOMRectReadOnly {
  height: number;
  width: number;
  x: number;
  y: number;
}

interface DOMRectInit {
  height?: number;
  width?: number;
  x?: number;
  y?: number;
}

declare var DOMRect: {
  prototype: DOMRect;
  new (x?: number, y?: number, width?: number, height?: number): DOMRect;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRect/fromRect_static) */
  fromRect(other?: DOMRectInit): DOMRect;
};

/** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly) */
interface DOMRectReadOnly {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/bottom) */
  readonly bottom: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/height) */
  readonly height: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/left) */
  readonly left: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/right) */
  readonly right: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/top) */
  readonly top: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/width) */
  readonly width: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/x) */
  readonly x: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/y) */
  readonly y: number;
  toJSON(): any;
}

//
// Images
//

export function loadImage(
  src: string | URL,
  options?: RequestInit,
): Promise<Image>;
export function loadImage(src: Sharp | Buffer): Promise<Image>;

export function loadImageData(
  src: string | Buffer | URL,
  width: number,
  height?: number,
): Promise<ImageData>;
export function loadImageData(
  src: string | Buffer | URL,
  width: number,
  height: number,
  settings?: ImageDataSettings & RequestInit,
): Promise<ImageData>;
export function loadImageData(src: Sharp): Promise<ImageData>;

/**
 * The color space a surface composites in, and that its exports are tagged
 * with. Wide-gamut and HDR output is the main thing available here that a
 * browser `<canvas>` cannot do.
 *
 * A space is a pair: **primaries** (which colors the extremes of the range
 * mean) and a **transfer function** (how numbers map to light). The names
 * below combine the two. Fourteen names, seven spaces -- each has an alias,
 * listed together.
 *
 * | Name | Primaries | Transfer | Use |
 * | --- | --- | --- | --- |
 * | `srgb` | sRGB | sRGB | The default, and what CSS colors mean. |
 * | `srgb-linear`, `linear` | sRGB | linear | Compositing in linear light. |
 * | `display-p3`, `p3` | Display P3 | sRGB | Wide gamut; standard on Apple displays. |
 * | `display-p3-linear`, `p3-linear` | Display P3 | linear | P3 in linear light. |
 * | `rec2020`, `bt2020` | Rec. 2020 | Rec. 709 | The widest SDR gamut, used by UHD. |
 * | `rec2020-linear`, `bt2020-linear` | Rec. 2020 | linear | Rec. 2020 in linear light. |
 * | `rec2020-pq`, `hdr10` | Rec. 2020 | PQ | **HDR10.** |
 * | `rec2020-hlg`, `hlg` | Rec. 2020 | HLG | Broadcast HDR. |
 *
 * Exports carry the space: a PNG written from a `display-p3` or `rec2020`
 * canvas embeds an ICC profile, so a viewer that understands one renders the
 * wider gamut rather than clipping it to sRGB.
 *
 * A colour outside the canvas's gamut is clipped as it is drawn, not at the
 * export: a Display P3 image drawn onto an `srgb` canvas loses the extra gamut
 * there and cannot get it back by exporting as P3. Name the space you want to
 * *keep* on the canvas.
 *
 * The two HDR rows build a canvas with that transfer function, and an export
 * carries the profile -- but the content is still SDR. Compositing is eight
 * bits per channel, `putImageData` clamps to 1.0, and none of the encoders
 * (PNG, JPEG, WebP) is an HDR container, so there is no way to get a value
 * above white into the picture or out of it.
 *
 * @example
 * const canvas = new Canvas(1920, 1080, { colorSpace: "display-p3" });
 * const ctx = canvas.getContext("2d");
 * ctx.fillStyle = "color(display-p3 1 0 0)"; // outside sRGB, and kept
 */
export type ColorSpace =
  | "srgb"
  | "srgb-linear"
  | "linear"
  | "display-p3"
  | "p3"
  | "display-p3-linear"
  | "p3-linear"
  | "rec2020"
  | "bt2020"
  | "rec2020-linear"
  | "bt2020-linear"
  | "rec2020-pq"
  | "hdr10"
  | "rec2020-hlg"
  | "hlg";
export type ColorType =
  | "Alpha8"
  | "Gray8"
  | "R8UNorm" // 1 byte/px
  | "A16Float"
  | "A16UNorm"
  | "ARGB4444"
  | "R8G8UNorm"
  | "RGB565" // 2 bytes/px
  | "rgb"
  | "RGB888x"
  | "rgba"
  | "RGBA8888"
  | "bgra"
  | "BGRA8888"
  | "BGR101010x"
  | "BGRA1010102" // 4 bytes/px
  | "R16G16Float"
  | "R16G16UNorm"
  | "RGB101010x"
  | "RGBA1010102"
  | "SRGBA8888"
  /**
   * Whichever 32-bit order this platform composites in — BGRA on Apple and
   * Windows, RGBA elsewhere. Naming it asks for a readback that needs no
   * swizzle. Reading the type back reports the concrete layout, since that
   * is what the pixels turned out to be.
   */
  | "N32" // 4 bytes/px
  | "R16G16B16A16UNorm"
  | "RGBAF16"
  | "RGBAF16Norm" // 8 bytes/px
  | "RGBAF32"; // 16 bytes/px

interface ImageDataSettings {
  /**
   * Color space the pixel data is in, defaulting to `"srgb"`.
   *
   * Every name in the {@link ColorSpace} union is accepted, and one that is
   * not throws a `TypeError` at construction rather than later inside a
   * draw. The space is metadata about the buffer, not a conversion: nothing
   * is resampled to match it.
   */
  colorSpace?: ColorSpace;
  /**
   * Layout of the bytes in `data`, defaulting to `"rgba"`.
   *
   * Determines {@link ImageData.bytesPerPixel} and so how `data` is walked.
   * A buffer passed to the constructor must be long enough for the
   * dimensions at that format, or the call throws.
   */
  colorType?: ColorType;
}

interface ImageDataExportSettings {
  /** Background color to draw beneath transparent parts of the canvas */
  matte?: string;

  /** Number of pixels per grid ‘point’ (defaults to 1) */
  density?: number;

  /** Number of samples used for antialiasing each pixel. `0` and `1` both
   * mean one sample a pixel -- no multisampling. */
  msaa?: number | boolean;

  /**
   * Color space the pixels are converted to on the way out, defaulting to
   * the canvas's own.
   *
   * A browser makes the same distinction: `getImageData()` on a
   * `display-p3` canvas hands back P3 components, and the `ImageData`
   * returned reports the space through its own `colorSpace`. The same red
   * reads as `255,0,0` in sRGB and `234,51,35` in `display-p3`.
   */
  colorSpace?: ColorSpace;

  /**
   * Pixel format the export is handed back in, defaulting to the canvas's
   * own.
   *
   * This is what makes `toBuffer("raw", {colorType: "RGBAF32"})` differ from
   * the canvas it came from. Compositing still follows the canvas: a
   * readback format has no business choosing the precision a page is drawn
   * at.
   */
  colorType?: ColorType;
}

/**
 * A rectangle of raw pixel data, as
 * {@link CanvasRenderingContext2D.getImageData} returns and
 * {@link CanvasRenderingContext2D.putImageData} takes.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/ImageData)
 */
export class ImageData {
  prototype: ImageData;
  /** Allocate transparent-black pixels of the given size. */
  constructor(sw: number, sh: number, settings?: ImageDataSettings);
  /**
   * Wrap an existing buffer. Its length must match the dimensions at the
   * chosen {@link ImageDataSettings.colorType}, or the call throws; the
   * height may be left out and is then derived from the length.
   */
  constructor(
    data: Uint8ClampedArray | Buffer,
    sw: number,
    sh?: number,
    settings?: ImageDataSettings,
  );
  /**
   * Copy a decoded {@link Image} into pixel data. An overload this library
   * adds; a browser has no constructor taking an image.
   */
  constructor(image: Image, settings?: ImageDataSettings);
  /**
   * Copy another `ImageData`, keeping its dimensions, color space and
   * format. An overload this library adds.
   */
  constructor(imageData: ImageData);

  /**
   * Color space the components are in. Unlike a browser's, which only ever
   * reports `"srgb"` or `"display-p3"`, this carries whichever
   * {@link ColorSpace} the data was read in.
   */
  readonly colorSpace: ColorSpace;
  /** 🧪 Not in the HTML Canvas standard. */
  readonly colorType: ColorType;
  /**
   * Bytes each pixel occupies under {@link ImageData.colorType}. The only way
   * to walk `data` correctly for a non-`rgba` format.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly bytesPerPixel: number;
  /**
   * The pixels themselves, row by row from the top left.
   *
   * Typed as `Uint8ClampedArray` for compatibility, and it is one for the
   * eight-bit formats. For a float {@link ImageData.colorType} the same
   * bytes are the encoding of wider components, so walk them by
   * {@link ImageData.bytesPerPixel} rather than four at a time.
   */
  readonly data: Uint8ClampedArray;
  /** Number of rows. */
  readonly height: number;
  /** Number of pixels per row. */
  readonly width: number;
  /**
   * Copy the pixels into a [Sharp](https://sharp.pixelplumbing.com) image.
   * Sharp is an optional peer dependency and must be installed separately.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toSharp(): Sharp;
}

/**
 * A decoded image, ready to be drawn with
 * {@link CanvasRenderingContext2D.drawImage}.
 *
 * Loading is asynchronous unless the data is already in hand: pass a
 * `Buffer` or a data URL to the constructor and the image is
 * {@link Image.complete} immediately. Otherwise assign {@link Image.src} and
 * wait for the `load` event, {@link Image.decode}, or use {@link loadImage}.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement)
 */
export class Image extends EventEmitter {
  /**
   * Decode image data synchronously.
   *
   * The data must be a `Buffer` of an encoded image -- an `ArrayBuffer` or
   * typed array throws -- or a `data:` URL. The optional second argument
   * sets {@link Image.src} for identification only; it is never fetched, so
   * it need not be a valid URL.
   *
   * Constructing from data is this library's addition: a browser's `Image`
   * constructor takes only a width and a height.
   */
  constructor(data?: Buffer | URL | string, src?: string);
  /**
   * Where the image was loaded from. Assigning starts a load and, when it
   * finishes, fires `load` or `error`.
   *
   * The setter takes more than the standard's URL string: an http(s) URL, a
   * local file path, a `data:` URL, a `Buffer` of encoded bytes, or a Sharp
   * image. Assigning again abandons a load already in flight rather than
   * racing it.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/src)
   */
  get src(): string;
  set src(src: string | URL | Buffer | Sharp);
  /**
   * Width of the decoded image in pixels, and read-only: `drawImage` uses
   * the intrinsic size, so assigning could not have meant anything. `0`
   * until the image loads.
   *
   * An SVG with no intrinsic size is rasterized at a height of 150 with the
   * width taken from its `viewBox` aspect.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/width)
   */
  get width(): number;
  /**
   * Height of the decoded image in pixels, read-only for the same reason
   * {@link Image.width} is.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/height)
   */
  get height(): number;
  /**
   * The image's intrinsic width.
   *
   * The same number as `width` here. They differ in a browser only because an
   * `<img>` can be resized by attribute or by CSS, and there is no layout in
   * this environment for that to happen in.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/naturalWidth)
   */
  get naturalWidth(): number;
  /**
   * The image's intrinsic height. As `naturalWidth`, this equals `height`.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/naturalHeight)
   */
  get naturalHeight(): number;
  /**
   * 🧪 Not in the HTML Canvas standard.
   *
   * How many frames the image holds.
   *
   * `1` for a still image. Animated GIF, WebP and APNG report every frame
   * they contain -- the last of them demuxed by this library, since Skia
   * opens an APNG as the still image its `IDAT` holds and reports one frame.
   */
  get frames(): number;
  /**
   * 🧪 Not in the HTML Canvas standard.
   *
   * How long each frame is shown, in milliseconds.
   *
   * One entry per frame, so this array is always `frames` long. A still image
   * reports a single `0`: it is shown until something else is drawn, which is
   * not a duration.
   *
   * A `0` on an animated frame is reported as stored, and does not mean the
   * frame is shown instantly. Viewers clamp a very short GIF delay upward --
   * Firefox renders anything of 10ms or less at 100ms -- so a zero-delay
   * frame is the slowest one, not the fastest.
   */
  get delays(): number[];
  /**
   * 🧪 Not in the HTML Canvas standard.
   *
   * One frame of the image, as an `Image` of its own.
   *
   * Frames that cover only part of the canvas are composited against the ones
   * before them, so every frame comes back whole and drawable, and they may be
   * asked for in any order.
   *
   * Nothing advances a frame on its own -- there is no clock here. An
   * animation plays because the caller picks the frame each of its own output
   * frames shows:
   *
   * ```js
   * const spinner = await loadImage("spinner.gif")
   * for (let i = 0; i < 24; i++) {
   *   ctx.drawImage(spinner.frame(i % spinner.frames), 0, 0)
   *   canvas.newPage()
   * }
   * ```
   *
   * A negative index counts from the end, so `frame(-1)` is the last one --
   * the rule `page` follows in the export options, and the one
   * `Array.prototype.at` follows. A fractional index truncates toward zero
   * before the end is counted from, as `at` does.
   *
   * @throws RangeError if `index` names no frame the image has.
   */
  frame(index?: number): Image;
  /**
   * Called once the image has loaded and decoded. The image is passed as the
   * argument and is also `this`, so a non-arrow function can use either.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/load_event)
   */
  onload: ((this: Image, image: Image) => any) | null;
  /**
   * Called with the `Error` if loading or decoding failed. Assigning
   * replaces the previous handler rather than adding a second one; use
   * `on("error", ...)` to stack listeners.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/error_event)
   */
  onerror: ((this: Image, error: Error) => any) | null;
  /**
   * Whether the image has finished loading, successfully or not.
   *
   * Derived from load state, as in the browser: assigning to it has never done
   * anything, here or there.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/complete)
   */
  readonly complete: boolean;
  /**
   * Resolves once the image is ready to draw, and rejects if it failed.
   *
   * Unlike the browser's, which resolves with `undefined`, this resolves
   * with the image itself. On an image whose `src` was never set it rejects
   * rather than waiting forever.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLImageElement/decode)
   */
  decode(): Promise<Image>;
}

//
// DOMMatrix
//

interface DOMMatrix2DInit {
  a?: number;
  b?: number;
  c?: number;
  d?: number;
  e?: number;
  f?: number;
  m11?: number;
  m12?: number;
  m21?: number;
  m22?: number;
  m41?: number;
  m42?: number;
}

interface DOMMatrixInit extends DOMMatrix2DInit {
  is2D?: boolean;
  m13?: number;
  m14?: number;
  m23?: number;
  m24?: number;
  m31?: number;
  m32?: number;
  m33?: number;
  m34?: number;
  m43?: number;
  m44?: number;
}

interface DOMMatrix {
  /** 2D component; the same value as `m11`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  a: number;
  /** 2D component; the same value as `m12`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  b: number;
  /** 2D component; the same value as `m21`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  c: number;
  /** 2D component; the same value as `m22`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  d: number;
  /** 2D component; the same value as `m41`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  e: number;
  /** 2D component; the same value as `m42`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  f: number;
  /** 4x4 component; the same value as `a`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m11: number;
  /** 4x4 component; the same value as `b`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m12: number;
  /** 4x4 component, row 1 column 3. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m13: number;
  /** 4x4 component, row 1 column 4. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m14: number;
  /** 4x4 component; the same value as `c`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m21: number;
  /** 4x4 component; the same value as `d`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m22: number;
  /** 4x4 component, row 2 column 3. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m23: number;
  /** 4x4 component, row 2 column 4. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m24: number;
  /** 4x4 component, row 3 column 1. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m31: number;
  /** 4x4 component, row 3 column 2. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m32: number;
  /** 4x4 component, row 3 column 3. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m33: number;
  /** 4x4 component, row 3 column 4. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m34: number;
  /** 4x4 component; the same value as `e`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m41: number;
  /** 4x4 component; the same value as `f`. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m42: number;
  /** 4x4 component, row 4 column 3. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m43: number;
  /** 4x4 component, row 4 column 4. [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix#instance_properties) */
  m44: number;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/flipX) */
  flipX(): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/flipY) */
  flipY(): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/inverse) */
  inverse(): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/invertSelf) */
  invertSelf(): DOMMatrix;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/multiply) */
  multiply(other?: DOMMatrixInit): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/multiplySelf) */
  multiplySelf(other?: DOMMatrixInit): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/preMultiplySelf) */
  preMultiplySelf(other?: DOMMatrixInit): DOMMatrix;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/rotate) */
  rotate(rotX?: number, rotY?: number, rotZ?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/rotateSelf) */
  rotateSelf(rotX?: number, rotY?: number, rotZ?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/rotateAxisAngle) */
  rotateAxisAngle(
    x?: number,
    y?: number,
    z?: number,
    angle?: number,
  ): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/rotateAxisAngleSelf) */
  rotateAxisAngleSelf(
    x?: number,
    y?: number,
    z?: number,
    angle?: number,
  ): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/rotateFromVector) */
  rotateFromVector(x?: number, y?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/rotateFromVectorSelf) */
  rotateFromVectorSelf(x?: number, y?: number): DOMMatrix;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/scale) */
  scale(
    scaleX?: number,
    scaleY?: number,
    scaleZ?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/scaleSelf) */
  scaleSelf(
    scaleX?: number,
    scaleY?: number,
    scaleZ?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/scale3d) */
  scale3d(
    scale?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/scale3dSelf) */
  scale3dSelf(
    scale?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;

  /**
   * Skew on both axes at once, equivalent to `skewX(sx).skewY(sy)`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  skew(sx?: number, sy?: number): DOMMatrix;
  /**
   * Skew this matrix on both axes at once, in place.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  skewSelf(sx?: number, sy?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/skewX) */
  skewX(sx?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/skewXSelf) */
  skewXSelf(sx?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/skewY) */
  skewY(sy?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/skewYSelf) */
  skewYSelf(sy?: number): DOMMatrix;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/translate) */
  translate(tx?: number, ty?: number, tz?: number): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/translateSelf) */
  translateSelf(tx?: number, ty?: number, tz?: number): DOMMatrix;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrix/setMatrixValue) */
  setMatrixValue(transformList: string): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/transformPoint) */
  transformPoint(point?: DOMPointInit): DOMPoint;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/is2D) */
  readonly is2D: boolean;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/isIdentity) */
  readonly isIdentity: boolean;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/toFloat32Array) */
  toFloat32Array(): Float32Array;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/toFloat64Array) */
  toFloat64Array(): Float64Array;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/toJSON) */
  toJSON(): any;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMMatrixReadOnly/toString) */
  toString(): string;
  /**
   * A copy that can be mutated without touching this one.
   *
   * 🧪 Not in the HTML Canvas standard. `DOMMatrix.fromMatrix(m)` is the
   * standard spelling and does the same thing.
   */
  clone(): DOMMatrix;
}

type FixedLenArray<T, L extends number> = T[] & { length: L };
type Matrix =
  | string
  | DOMMatrix
  | { a: number; b: number; c: number; d: number; e: number; f: number }
  | FixedLenArray<number, 6>
  | FixedLenArray<number, 16>;

declare var DOMMatrix: {
  prototype: DOMMatrix;
  new (init?: Matrix): DOMMatrix;
  fromFloat32Array(array32: Float32Array): DOMMatrix;
  fromFloat64Array(array64: Float64Array): DOMMatrix;
  fromMatrix(other?: DOMMatrixInit): DOMMatrix;
};

//
// Canvas
//

export type ExportFormat =
  | "png"
  | "jpg"
  | "jpeg"
  | "webp"
  | "gif"
  | "apng"
  | "tiff"
  | "tif"
  | "ico"
  | "bmp"
  | "avif"
  | "raw"
  | "pdf"
  | "svg";

/** 🧪 Not in the HTML Canvas standard. */
export interface RenderOptions {
  /**
   * Which page to export, numbered from `1`.
   *
   * Left out, a single-page format encodes the **current** page -- the most
   * recently added one, which is what `canvas.getContext()` hands back --
   * and a format that gathers pages takes all of them. A negative number
   * counts from the end, so `-1` is that same current page named explicitly.
   *
   * A number past either end is a `RangeError` that names the page asked
   * for, not the index it resolved to.
   */
  page?: number;

  /** Background color to draw beneath transparent parts of the canvas */
  matte?: string;

  /** Number of pixels per grid ‘point’ (defaults to 1) */
  density?: number;

  /** Number of samples used for antialiasing each pixel. `0` and `1` both
   * mean one sample a pixel -- no multisampling. */
  msaa?: number | boolean;
}

/** 🧪 Not in the HTML Canvas standard. */
export interface ExportOptions extends RenderOptions {
  /** Quality for lossy encodings like JPEG & WEBP (0.0–1.0) */
  quality?: number;

  /** Optionally convert text to bézier paths (SVG only) */
  outline?: boolean;

  /** Optionally use 4:2:0 chroma subsampling (JPEG only) */
  downsample?: boolean;

  /**
   * Pixel format the export is handed back in, defaulting to the canvas's
   * own.
   *
   * This is what makes `toBuffer("raw", {colorType: "RGBAF32"})` differ from
   * the canvas it came from, and it is also where the encoded formats read
   * their own depth: a float type writes a sixteen-bit `"png"`, `"apng"` or
   * `"tiff"`, and `"RGBA8888"` on a float canvas writes eight. `"avif"` is
   * the exception and has {@link ExportOptions.bitDepth} of its own.
   *
   * Compositing still follows the canvas: a readback format has no business
   * choosing the precision a page is drawn at.
   */
  colorType?: ColorType;

  /**
   * Bits a channel `"avif"` codes its pixels at, defaulting to whatever the
   * canvas has to give.
   *
   * AV1 codes 8, 10 and 12 and AVIF carries all three. Unasked, an eight-bit
   * canvas is written at 10 -- AV1's transforms work above the input depth
   * anyway, and the headroom keeps quantisation from banding a gradient that
   * eight bits would step through -- and a float canvas at 12.
   *
   * The reason to name one is reach. 8 and 10 at 4:4:4 are AV1's High
   * profile; 12 is Professional, which fewer decoders implement. So a float
   * canvas whose file has to open anywhere asks for 10, while 8 is both the
   * smallest file and the one depth that reaches the encoder as the bytes
   * the canvas already holds.
   *
   * Naming one for any other format is a `TypeError`: their depths are the
   * ones {@link colorType} already names. JPEG, WebP, GIF, ICO and BMP have
   * no deeper form at all -- eight bits a channel is what those formats are.
   */
  bitDepth?: 8 | 10 | 12;

  /**
   * Color space the exported image is converted to, defaulting to the
   * canvas's own.
   *
   * A `display-p3` canvas therefore writes a Display P3 file with the
   * matching ICC profile embedded, without being asked. Naming a space here
   * converts; it cannot recover. A colour outside the canvas's gamut was
   * already clipped when it was drawn, so exporting an `srgb` canvas as
   * `"display-p3"` writes a P3 file holding sRGB colours.
   */
  colorSpace?: ColorSpace;

  /**
   * Frames per second for the animated formats -- `"webp"`, `"gif"`,
   * `"apng"` and `"avif"`. One page is one frame, so this is the rate the
   * pages play at.
   *
   * Defaults to 30. GIF stores hundredths of a second, so its frame times
   * round to the nearest 10ms.
   *
   * Naming one for any other format is a `TypeError`, not something quietly
   * dropped -- and that includes `"tiff"`, `"ico"` and `"pdf"`, which gather
   * every page without any of them having a duration.
   */
  fps?: number;

  /**
   * Per-frame durations in milliseconds, overriding `fps`.
   *
   * Must have one entry per page, which is what makes re-encoding an
   * animation possible: the `delays` an `Image` reports can be handed
   * straight back. A list of any other length is a `RangeError` rather than
   * a silent retiming, and a list given to a format that cannot animate is
   * a `TypeError`.
   */
  frameDelays?: number[];

  /**
   * How many times an animation plays. `0` -- the default -- repeats it
   * forever, which is how both GIF and APNG spell it. Naming one for a
   * format that cannot animate is a `TypeError`.
   *
   * `1` plays it once, which APNG states outright and GIF cannot: GIF's loop
   * count lives in a block whose zero already means "forever", so no number
   * means "once" and the convention is to omit the block. A GIF asking for a
   * single play therefore declares nothing, and a decoder may report either
   * answer depending on when it is asked. Every other count is stated
   * plainly by both.
   */
  loop?: number;
}

/** 🧪 Not in the HTML Canvas standard. */
export interface SaveOptions extends ExportOptions {
  /** Image format to use (either as a file extension or a mime-type string) */
  format?: ExportFormat;
}

/**
 * What one canvas is actually rendering on, as reported by
 * {@link Canvas.engine}.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface EngineDetails {
  /**
   * Which backend took this canvas -- not what the machine has. A canvas
   * built with `gpu: false`, and a float canvas that no GPU can composite,
   * both report `"CPU"` on a machine with a working GPU.
   */
  renderer: "CPU" | "GPU";
  /**
   * The graphics API this build was compiled against, or `null` when it was
   * compiled without GPU support. Names the API even where `renderer` is
   * `"CPU"`, since the fallback happened inside that API.
   */
  api: "Vulkan" | "Metal" | null;
  /**
   * The adapter as the driver names it -- `"Discrete GPU (…)"` -- or, on the
   * CPU, a phrase saying which reason put it there: manually disabled, the
   * pixel format needing it, a failed initialization, or a build without GPU
   * support.
   */
  device: string;
  /**
   * Driver name and version, where the API reports them. Vulkan does; Metal
   * omits the field on the GPU, and every CPU path reports the string
   * `"N/A"`.
   */
  driver?: string;
  /** Threads in the rasterizing pool. */
  threads: number;
  /**
   * Why the GPU was not used, when that is a failure rather than a choice.
   * Absent on a working GPU, and `null` on a build compiled without support.
   */
  error?: string | null;
  /** The canvas's own {@link TextOptions.textContrast}. */
  textContrast: number;
  /** The canvas's own {@link TextOptions.textGamma}. */
  textGamma: number;
}

/**
 * What this build on this machine offers, as reported by {@link backend}.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface BackendInfo {
  /** Whether a canvas built right now would rasterize on the GPU or the CPU. */
  renderer: "CPU" | "GPU";
  /**
   * The graphics API this build was compiled against, or `null` when it was
   * compiled without GPU support.
   */
  api: "Vulkan" | "Metal" | null;
  /** The adapter as the driver names it, or why the CPU is being used. */
  device: string;
  /**
   * Driver name and version, where the API reports them -- Vulkan does,
   * Metal does not. `"N/A"` where the CPU is rendering.
   */
  driver?: string;
  /** Threads in the rasterizing pool. */
  threads: number;
  /**
   * Whether a canvas may choose the GPU at all. False on a build without GPU
   * support and on a machine whose driver declined, which `error` tells
   * apart.
   */
  gpuAvailable: boolean;
  /**
   * Why GPU initialization failed, where it failed. `null` on a build
   * compiled without GPU support -- there was nothing to initialize.
   */
  error?: string | null;
}

/**
 * What this build and this machine offer, without constructing a canvas.
 *
 * The device and driver strings come from the driver: they are for logging,
 * not for matching on, since their wording is the platform's and changes
 * with it. To find out what one canvas settled on -- which is a different
 * question, since a float canvas rasterizes whatever the machine has --
 * read {@link Canvas.engine}.
 */
export function backend(): BackendInfo;

/** 🧪 Not in the HTML Canvas standard. */
export interface TextOptions {
  /** Amount of additional contrast to add when rendering text (defaults to 0) */
  textContrast?: number;

  /** Gamma value for blending the edges of letterforms (defaults to 1.4) */
  textGamma?: number;

  /**
   * Pixel format exports and `getImageData` hand pixels back in (defaults to
   * `"rgba"`).
   *
   * On a canvas, a float format (`"RGBAF16"`, `"RGBAF32"`) also selects what
   * the page composites in, so blending keeps the fractions eight bits round
   * away: sixty layers at 0.6% alpha land on 0.30308 (`RGBAF32`) and 0.30298
   * (`RGBAF16`) against an arithmetic answer of 0.30308, where eight bits
   * compound their rounding into 0.23922. It costs twice the memory for
   * `RGBAF16` and four times for `RGBAF32`. The time cost depends on what is
   * drawn rather than on the pixel width: 120 translucent layers are
   * *faster* in float (0.74x and 0.77x, since an eight-bit surface converts
   * through its transfer function on every layer and a float one does not),
   * while 120 opaque fills cost 1.29x in `RGBAF16` and 7.58x in `RGBAF32`.
   * Every other format composites at eight bits and converts on the way out
   * -- an opaque or narrower one would lose more inside the page than it
   * saves.
   *
   * On a `toBuffer` or `getImageData` call it means only the layout of the
   * buffer you receive; the page keeps the format its canvas was built with.
   *
   * A float canvas renders on the raster backend, whatever `gpu` says, and
   * `canvas.engine` reports which one took it. No GPU can currently deliver
   * the precision: Skia's Metal and Vulkan backends implement no 32-bit float
   * surface at all, and while both provide `RGBAF16`, a GPU quantises the
   * paint colour to eight bits before compositing -- the same sixty layers
   * land on 0.235 there, further from 0.303 than the eight-bit answer of
   * 0.361. Asking for float and being handed eight bits would be the worse
   * trade, so the canvas changes engine instead.
   *
   * This is probed at runtime rather than assumed, so a Skia that grows the
   * support keeps such canvases on the GPU with no change here.
   */
  colorType?: ColorType;

  /**
   * The space the canvas composites in (defaults to `"srgb"`).
   *
   * Fixed here rather than per export: colours are interpreted in it, and one
   * outside its gamut is clipped as it is drawn. Exports and readbacks
   * default to it and convert out of it when asked for another.
   */
  colorSpace?: ColorSpace;

  /**
   * Whether to rasterize on the GPU when one is available (defaults to
   * `true`). Set `false` to force the CPU backend.
   *
   * Asking is not getting: {@link Canvas.gpu} reports the engine the canvas
   * settled on, so it reads `false` on a build with no GPU support, on a
   * machine whose driver declined, and on a float canvas, which no GPU can
   * composite. {@link Canvas.engine} says which of those it was.
   */
  gpu?: boolean;
}

/**
 * A stand-in for the HTML `<canvas>` element: it holds the image dimensions,
 * hands out a {@link CanvasRenderingContext2D} to draw with, and encodes what
 * was drawn to a file, a buffer, or a string.
 *
 * Rendering is deferred until an export is asked for, and runs on a
 * background thread, so the asynchronous exporters are the ones to prefer
 * where several images are being produced at once.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement)
 */
export class Canvas {
  /**
   * Every canvas's page list, keyed by canvas. This is the live array rather
   * than a copy, so it grows as pages are added -- and it holds them newest
   * first, the reverse of the order {@link Canvas.pages} hands back.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  static contexts: WeakMap<Canvas, readonly CanvasRenderingContext2D[]>;
  /**
   * Gets or sets the height of a canvas element on a document.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement/height)
   */
  height: number;
  /**
   * Gets or sets the width of a canvas element on a document.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement/width)
   */
  width: number;

  /**
   * Build a canvas of `width` x `height` points, defaulting to the browser's
   * own 300 x 150.
   *
   * The third argument is this library's: it fixes the pixel format, the
   * color space and the renderer for the canvas's whole life, none of which
   * an export can change afterwards. See {@link TextOptions}.
   *
   * ```ts
   * const canvas = new Canvas(512, 512, { colorSpace: "display-p3" })
   * ```
   *
   * The options argument is this library's own; a browser configures none of
   * this on the element.
   */
  constructor(width?: number, height?: number, options?: TextOptions);

  /**
   * Returns an object that provides methods and properties for drawing and manipulating images and graphics on a canvas element in a document. A context object includes information about colors, line widths, fonts, and other graphic parameters that can be drawn on a canvas.
   * @param type The type of canvas to create. Skia Canvas only supports a 2-D context using canvas.getContext("2d")
   *
   * The argument is required: the runtime returns `null` for anything other
   * than `"2d"`, including no argument at all, so declaring it optional
   * promised a context that would not arrive.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement/getContext)
   */
  getContext(type: "2d"): CanvasRenderingContext2D;
  /**
   * Add a page, and return its drawing context.
   *
   * Pages stay drawable once added, and which of them an export takes
   * depends on the format and the filename. PDF, TIFF, ICO and the three
   * animated formats gather every page into one file; the rest write the
   * current page alone, unless the filename passed to `toFile` contains
   * `"{}"`, which writes one numbered file per page.
   *
   * The size is a pair or nothing: omit both to keep the canvas's current
   * size, or give both to resize the canvas for this page onward. Earlier
   * pages keep the size they were created at. Passing only a width throws,
   * rather than adding a page at a size nobody asked for.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  newPage(): CanvasRenderingContext2D;
  newPage(width: number, height: number): CanvasRenderingContext2D;
  /**
   * Every page added so far, oldest first, each as the context that draws
   * it. The last entry is the current page.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly pages: CanvasRenderingContext2D[];

  /**
   * Whether this canvas is rasterizing on the GPU.
   *
   * Reports the engine it settled on, not the one asked for, so a float
   * canvas reads `false` on a machine with a working GPU. Assigning moves an
   * existing canvas between the two, and is ignored where the engine asked
   * for is not available.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get gpu(): boolean;
  set gpu(enabled: boolean);
  /**
   * Which backend took this canvas, and what it is -- see
   * {@link EngineDetails}. `backend()` answers the same question about the
   * machine rather than about one canvas.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly engine: EngineDetails;

  /**
   * The pixel format this canvas was constructed with (`"rgba"` by default).
   * Exports and `getImageData` inherit it unless the call names its own.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly colorType: ColorType;

  /**
   * The color space this canvas composites in, as passed to the constructor
   * and normalized to its canonical name -- `"p3"` reads back as
   * `"display-p3"`. Exports and `getImageData` inherit it unless the call
   * names its own, which is what a browser does.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly colorSpace: ColorSpace;

  /**
   * @deprecated Use {@link Canvas.toFile()} instead
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  saveAs(filename: string, options?: SaveOptions): Promise<void>;
  /**
   * Render the canvas and write it to disk, resolving once the file is
   * closed.
   *
   * The format comes from the filename's extension unless `options.format`
   * names one, which is what to use when the name cannot carry an extension.
   * A `URL` is accepted in place of a path, and must use the `file:`
   * protocol.
   *
   * A filename containing `"{}"` writes one numbered file per page --
   * `"page-{}.png"` gives `page-1.png` onward -- and a number between the
   * braces is the zero-padded width, so `"frame-{4}.png"` gives
   * `frame-0001.png`. Without the braces, a format that gathers pages writes
   * all of them into the one file and the rest write the current page alone.
   *
   * An `@2x` suffix on the filename sets {@link RenderOptions.density}, so
   * `"chart@2x.png"` is the same call as `{ density: 2 }`.
   *
   * ```ts
   * await canvas.toFile("chart.png")
   * await canvas.toFile("frames-{4}.png", { density: 2 })
   * ```
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toFile(filename: string | URL, options?: SaveOptions): Promise<void>;
  /**
   * Render the canvas and resolve with the encoded bytes.
   *
   * `format` is an extension (`"png"`) or a mime type (`"image/png"`), and
   * may carry an `@2x` suffix to set {@link RenderOptions.density}. `"raw"`
   * returns the pixels themselves, laid out as
   * {@link ExportOptions.colorType} says.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toBuffer(format: ExportFormat, options?: ExportOptions): Promise<Buffer>;
  /**
   * Encode the canvas and hand the result to a callback as a `Blob`.
   *
   * Callback-style and returning `void`, as the standard defines it, rather
   * than the promise the other exporters on this class return. `type` is a
   * mime type -- `"image/png"` -- not the bare format name they take.
   *
   * A failed encode calls back with `null` rather than raising: the callback
   * has already been handed off by the time the encode runs.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement/toBlob)
   */
  toBlob(
    callback: (blob: Blob | null) => void,
    type?: string,
    quality?: number,
  ): void;
  /**
   * Render the canvas and resolve with a `data:` URL -- the same bytes
   * {@link Canvas.toBuffer} returns, base64-encoded behind the format's mime
   * type, ready for an `<img src>` or a CSS `url()`.
   *
   * Base64 costs a third more bytes than the buffer it wraps.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toURL(format: ExportFormat, options?: ExportOptions): Promise<string>;
  /**
   * Hand the canvas's pixels to a [Sharp](https://sharp.pixelplumbing.com)
   * image, for the processing and optimization that library offers.
   *
   * Sharp is an optional peer dependency and must be installed separately;
   * this throws if it is missing. The returned object is ready
   * synchronously, but most operations on it are themselves asynchronous.
   * The image carries a density of `72 * density` dpi.
   *
   * ```ts
   * await canvas.toSharp().heif({ compression: "hevc" }).toFile("out.heif")
   * ```
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toSharp(options?: RenderOptions): Sharp;

  /**
   * @deprecated Use {@link Canvas.toFileSync()} instead
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  saveAsSync(filename: string, options?: SaveOptions): void;
  /**
   * {@link Canvas.toFile} without the promise: it blocks until the file is
   * written.
   *
   * Identical arguments and identical rules for formats, page selection and
   * `"{}"` numbering. Rendering no longer overlaps with anything else, which
   * is what the asynchronous form buys.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toFileSync(filename: string | URL, options?: SaveOptions): void;
  /**
   * {@link Canvas.toBuffer} without the promise: it blocks and returns the
   * encoded bytes.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toBufferSync(format: ExportFormat, options?: ExportOptions): Buffer;
  /**
   * @deprecated {@link Canvas.toDataURL()} is now synchronous; use it instead
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toDataURLSync(format: ExportFormat, options?: ExportOptions): string;
  /**
   * {@link Canvas.toURL} without the promise: it blocks and returns the
   * `data:` URL.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toURLSync(format: ExportFormat, options?: ExportOptions): string;
  /**
   * {@link Canvas.toSharp} without the intermediate stream: the pixels are
   * already in hand, so they are handed to Sharp directly. Same arguments,
   * same result.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  toSharpSync(options?: RenderOptions): Sharp;

  /**
   * `format` accepts a bare extension (`"png"`) or a mime type
   * (`"image/png"`), and defaults to PNG as in the browser.
   *
   * [MDN Reference](https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/toDataURL)
   */
  toDataURL(format?: ExportFormat | string, quality?: number): string;

  /**
   * The canvas's pixels, unencoded, laid out as its own
   * {@link Canvas.colorType} says. Shorthand for `toBuffer("raw")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get raw(): Promise<Buffer>;
  /**
   * The canvas as a PDF, every page included. Shorthand for
   * `toBuffer("pdf")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get pdf(): Promise<Buffer>;
  /**
   * The current page as an SVG document. Shorthand for `toBuffer("svg")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get svg(): Promise<Buffer>;
  /**
   * The current page as a JPEG at the default quality of 0.92. Shorthand for
   * `toBuffer("jpg")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get jpg(): Promise<Buffer>;
  /**
   * The current page as a PNG. Shorthand for `toBuffer("png")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get png(): Promise<Buffer>;
  /**
   * The canvas as a WebP -- an animation where it has more than one page,
   * timed at the default 30fps. Shorthand for `toBuffer("webp")`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get webp(): Promise<Buffer>;
}

//
// Patterns
//

/**
 * An opaque object describing a pattern, based on an image, a canvas, or a video, created by the CanvasRenderingContext2D.createPattern() method.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasPattern)
 */
export class CanvasPattern {
  /**
   * Instances come from `CanvasRenderingContext2D.createPattern()`. Constructing one directly
   * leaves it without its native state: the call appears to succeed and
   * the first method then fails inside Neon.
   */
  private constructor();
  /**
   * Set the transform the tiled image is drawn under, replacing any
   * previous one.
   *
   * Takes a `DOMMatrix`, anything `new DOMMatrix()` accepts -- a CSS
   * transform string, an `{a, b, c, d, e, f}` object, a 6- or 16-element
   * array -- or the six numbers themselves. The transform applies to the
   * pattern, not to the context, so it survives `save()` and `restore()`.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasPattern/setTransform)
   */
  setTransform(transform: Matrix): void;
  setTransform(
    a: number,
    b: number,
    c: number,
    d: number,
    e: number,
    f: number,
  ): void;
}

/** Color space for gradient interpolation */
type GradientColorSpace =
  "srgb" | "srgb-linear" | "lab" | "oklab" | "oklch" | "lch" | "hsl" | "hwb";

/** Hue interpolation method for cylindrical color spaces (oklch, lch, hsl, hwb) */
type HueInterpolation = "shorter" | "longer" | "increasing" | "decreasing";

/**
 * An opaque object describing a gradient. It is returned by the methods CanvasRenderingContext2D.createLinearGradient() or CanvasRenderingContext2D.createRadialGradient().
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasGradient)
 */
interface CanvasGradient {
  /**
   * Adds a color stop with the given color to the gradient at the given
   * offset. 0.0 is the offset at one end of the gradient, 1.0 is the offset
   * at the other end.
   *
   * An offset outside `0.0..=1.0` throws a `RangeError`, and a color that
   * will not parse throws a `TypeError` -- plain JavaScript errors rather
   * than the `DOMException`s a browser raises, there being no DOM here.
   *
   * The color may also be a `[r, g, b, a]` array of premultiplied
   * linear-light floats, which no browser accepts.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasGradient/addColorStop)
   */
  addColorStop(offset: number, color: Color4fInput): void;

  /**
   * Color space the gradient's stops are blended in. Default: `"srgb"`.
   *
   * The default is the canvas's own space under another name: it reads back
   * as `"srgb"`, and on an sRGB canvas -- the default -- the two are the
   * same thing. The perceptual spaces are what to reach for when a two-color
   * ramp goes muddy in the middle; `oklab` and `oklch` hold lightness even
   * across the blend where sRGB's midpoint darkens.
   *
   * An unrecognized name is ignored and the current setting kept, as an
   * attribute setter is expected to do.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  interpolation: GradientColorSpace;

  /**
   * Which way hue travels in the cylindrical spaces -- `oklch`, `lch`,
   * `hsl`, `hwb`. Default: `"shorter"`, and no effect on the other spaces.
   *
   * `"longer"` takes the other way round the hue circle, so red to green
   * passes through blue; `"increasing"` always ascends, wrapping past 360
   * degrees, and `"decreasing"` always descends. An unrecognized name is
   * ignored, as with {@link CanvasGradient.interpolation}.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  hueInterpolation: HueInterpolation;
}

/**
 * The constructor object, exported so `x instanceof CanvasGradient` works.
 * Gradients come from `CanvasRenderingContext2D.createLinearGradient()` and
 * its siblings; calling this directly throws, so no construct signature is
 * declared even though `lib.dom.d.ts` has one.
 */
declare var CanvasGradient: {
  prototype: CanvasGradient;
};

/**
 * A repeating pattern drawn from a path, used as a fill or stroke style.
 *
 * Unlike {@link CanvasPattern}, which tiles a bitmap, a texture redraws its
 * path at each grid position, so it stays sharp at any scale.
 *
 * Reachable either way: {@link CanvasRenderingContext2D.createTexture} and
 * this constructor are the same call.
 *
 * ```ts
 * ctx.fillStyle = new CanvasTexture(8, { color: "red", line: 2 })
 * ```
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class CanvasTexture {
  /**
   * @param spacing - grid pitch, either `[x, y]` or one number for both
   * @param options - what to draw at each grid position; parallel lines if no
   *   `path` is given
   */
  constructor(spacing: Offset, options?: CreateTextureOptions);
}

//
// ColorFilter & ImageFilter
//

/** 4x5 row-major color matrix (20 elements) */
export type ColorMatrix = Float32Array | ArrayLike<number>;

//
// Filter Types
//

/** 3D point for lighting effects [x, y, z] */
export type Point3 = [number, number, number];

/** Color channel selector for displacement maps */
export type ColorChannel = "R" | "G" | "B" | "A";

/** Tile mode for edge handling */
export type TileMode = "clamp" | "repeat" | "mirror" | "decal";

/**
 * Sampling mode for image transformations.
 *
 * `"nearest"` and `"linear"` are the two filter modes. `"mipmap"` adds a
 * mipmap chain, which is better under heavy minification, and `"cubic"` is
 * Mitchell-Netravali bicubic — the highest quality of the four for scaled or
 * moving imagery, and the one that ignores mipmaps entirely.
 */
export type SamplingMode = "nearest" | "linear" | "mipmap" | "cubic";

/** Blend modes for image compositing */
export type BlendMode =
  | "clear"
  | "src"
  | "source"
  | "dst"
  | "destination"
  | "srcOver"
  | "src-over"
  | "source-over"
  | "dstOver"
  | "dst-over"
  | "destination-over"
  | "srcIn"
  | "src-in"
  | "source-in"
  | "dstIn"
  | "dst-in"
  | "destination-in"
  | "srcOut"
  | "src-out"
  | "source-out"
  | "dstOut"
  | "dst-out"
  | "destination-out"
  | "srcATop"
  | "src-atop"
  | "source-atop"
  | "dstATop"
  | "dst-atop"
  | "destination-atop"
  | "xor"
  | "plus"
  | "lighter"
  | "modulate"
  | "screen"
  | "overlay"
  | "darken"
  | "lighten"
  | "colorDodge"
  | "color-dodge"
  | "colorBurn"
  | "color-burn"
  | "hardLight"
  | "hard-light"
  | "softLight"
  | "soft-light"
  | "difference"
  | "exclusion"
  | "multiply"
  | "hue"
  | "saturation"
  | "color"
  | "luminosity";

/**
 * ColorFilter for color transformations.
 * Mirrors CanvasKit.ColorFilter API.
 *
 * @remarks
 * - Matrices operate in the canvas's working color space (sRGB, P3, or linear)
 * - Filters are immutable and safe to reuse across draws
 * - Input arrays are copied - safe to mutate after creation
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class ColorFilter {
  /**
   * Create a color filter of the given kind.
   *
   * The kind names the transform and the remaining arguments are the ones that
   * kind takes, matching the `Make` method of the same name. Where those return
   * `null` for arguments Skia rejects, the constructor throws instead.
   *
   * ```ts
   * ctx.colorFilter = new ColorFilter("blend", "red", "multiply")
   * ```
   *
   * @throws TypeError if the kind is unknown or the arguments describe no filter
   */
  constructor(kind: "matrix", matrix: ColorMatrix);
  /** Convert sRGB gamma to linear. See {@link ColorFilter.MakeSRGBToLinearGamma}. */
  constructor(kind: "srgb-to-linear-gamma");
  /** Convert linear gamma to sRGB. See {@link ColorFilter.MakeLinearToSRGBGamma}. */
  constructor(kind: "linear-to-srgb-gamma");
  /** Blend with a solid color. See {@link ColorFilter.MakeBlend}. */
  constructor(kind: "blend", color: string, mode: string);
  /** Apply `inner`, then `outer`. See {@link ColorFilter.MakeCompose}. */
  constructor(kind: "compose", outer: ColorFilter, inner: ColorFilter);
  /** Interpolate between two filters. See {@link ColorFilter.MakeLerp}. */
  constructor(kind: "lerp", t: number, dst: ColorFilter, src: ColorFilter);
  /** Color matrix applied in HSL space. See {@link ColorFilter.MakeHSLAMatrix}. */
  constructor(kind: "hsla-matrix", matrix: ColorMatrix);
  /** Multiply-then-add lighting. See {@link ColorFilter.MakeLighting}. */
  constructor(kind: "lighting", multiply: string, add: string);
  /** Extract brightness to alpha. See {@link ColorFilter.MakeLumaColorFilter}. */
  constructor(kind: "luma");
  /** One lookup table for every channel. See {@link ColorFilter.MakeTable}. */
  constructor(kind: "table", table: Uint8Array | number[]);
  /** A lookup table per channel. See {@link ColorFilter.MakeTableARGB}. */
  constructor(
    kind: "table-argb",
    tableA: Uint8Array | number[] | null,
    tableR: Uint8Array | number[] | null,
    tableG: Uint8Array | number[] | null,
    tableB: Uint8Array | number[] | null,
  );

  /**
   * Create ColorFilter from 4x5 row-major matrix.
   * @param matrix - 20 elements: [R_scale, R_G, R_B, R_A, R_offset, G_R, G_scale, G_B, G_A, G_offset, ...]
   * @returns ColorFilter (never null for valid input)
   * @throws RangeError if matrix.length !== 20
   * @throws TypeError if matrix contains non-finite numbers
   */
  static MakeMatrix(matrix: ColorMatrix): ColorFilter;

  /**
   * Create ColorFilter that converts sRGB gamma to linear.
   */
  static MakeSRGBToLinearGamma(): ColorFilter;

  /**
   * Create ColorFilter that converts linear gamma to sRGB.
   */
  static MakeLinearToSRGBGamma(): ColorFilter;

  /**
   * Create ColorFilter that blends with a solid color.
   * @param color - CSS color string
   * @param mode - blend mode (e.g., "multiply", "screen", "overlay")
   */
  static MakeBlend(color: string, mode: string): ColorFilter | null;

  /**
   * Compose two ColorFilters (outer applied after inner).
   */
  static MakeCompose(
    outer: ColorFilter,
    inner: ColorFilter,
  ): ColorFilter | null;

  /**
   * Interpolate between two ColorFilters.
   * @param t - interpolation factor (0 = dst, 1 = src)
   * @param dst - destination filter
   * @param src - source filter
   */
  static MakeLerp(
    t: number,
    dst: ColorFilter,
    src: ColorFilter,
  ): ColorFilter | null;

  /**
   * Create HSLA color matrix filter (operates in HSL space).
   * @param matrix - 20 elements (4x5 row-major)
   */
  static MakeHSLAMatrix(matrix: ColorMatrix): ColorFilter;

  /**
   * Create lighting effect filter.
   * @param multiply - multiply color (CSS string)
   * @param add - add color (CSS string)
   */
  static MakeLighting(multiply: string, add: string): ColorFilter | null;

  /**
   * Create luma (luminance) color filter - extracts brightness to alpha.
   */
  static MakeLumaColorFilter(): ColorFilter;

  /**
   * Create table-based color filter (same table for all channels).
   * @param table - 256 elements mapping input values to output values
   */
  static MakeTable(table: Uint8Array | number[]): ColorFilter | null;

  /**
   * Create table-based color filter with separate tables per channel.
   * Pass null for any channel to leave it unchanged.
   */
  static MakeTableARGB(
    tableA: Uint8Array | number[] | null,
    tableR: Uint8Array | number[] | null,
    tableG: Uint8Array | number[] | null,
    tableB: Uint8Array | number[] | null,
  ): ColorFilter | null;

  /**
   * Mark filter as deleted. Use-after-delete throws Error.
   */
  delete(): void;
}

/**
 * ImageFilter for composable effects.
 * Mirrors CanvasKit.ImageFilter API.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class ImageFilter {
  /**
   * Create an image filter of the given kind.
   *
   * The kind names the effect and the remaining arguments are the ones that
   * kind takes, matching the `Make` method of the same name. Where those return
   * `null` for arguments Skia rejects, the constructor throws instead.
   *
   * Most kinds end with an optional `input` filter, which is how they chain;
   * omitting it reads from the layer being drawn.
   *
   * ```ts
   * ctx.imageFilter = new ImageFilter("blur", 4, 4)
   * ```
   *
   * @throws TypeError if the kind is unknown or the arguments describe no filter
   */
  constructor(
    kind: "color-filter",
    colorFilter: ColorFilter,
    input?: ImageFilter | null,
  );
  /** Apply `inner`, then `outer`. See {@link ImageFilter.MakeCompose}. */
  constructor(kind: "compose", outer: ImageFilter, inner: ImageFilter);
  /** Gaussian blur. See {@link ImageFilter.MakeBlur}. */
  constructor(
    kind: "blur",
    sigmaX: number,
    sigmaY: number,
    tileMode?: "clamp" | "repeat" | "mirror" | "decal",
    input?: ImageFilter | null,
  );
  /** Source plus its shadow. See {@link ImageFilter.MakeDropShadow}. */
  constructor(
    kind: "drop-shadow",
    dx: number,
    dy: number,
    sigmaX: number,
    sigmaY: number,
    color: string | [number, number, number, number],
    input?: ImageFilter | null,
  );
  /** The shadow alone. See {@link ImageFilter.MakeDropShadowOnly}. */
  constructor(
    kind: "drop-shadow-only",
    dx: number,
    dy: number,
    sigmaX: number,
    sigmaY: number,
    color: string | [number, number, number, number],
    input?: ImageFilter | null,
  );
  /** Translate. See {@link ImageFilter.MakeOffset}. */
  constructor(
    kind: "offset",
    dx: number,
    dy: number,
    input?: ImageFilter | null,
  );
  /** Morphological dilation. See {@link ImageFilter.MakeDilate}. */
  constructor(
    kind: "dilate",
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
    /** 🧪 Bounds the kernel's domain as well as clipping the output. */
    crop?: [number, number, number, number] | null,
  );
  /** Morphological erosion. See {@link ImageFilter.MakeErode}. */
  constructor(
    kind: "erode",
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
    /** 🧪 Bounds the kernel's domain as well as clipping the output. */
    crop?: [number, number, number, number] | null,
  );
  /** Draw several filters together. See {@link ImageFilter.MakeMerge}. */
  constructor(kind: "merge", filters: (ImageFilter | null)[]);
  /** Transparent black. See {@link ImageFilter.MakeEmpty}. */
  constructor(kind: "empty");
  /** Repeat a source rect across a destination. See {@link ImageFilter.MakeTile}. */
  constructor(
    kind: "tile",
    src: [number, number, number, number],
    dst: [number, number, number, number],
    input?: ImageFilter | null,
  );
  /** Blend two filters. See {@link ImageFilter.MakeBlend}. */
  constructor(
    kind: "blend",
    mode: BlendMode,
    background?: ImageFilter | null,
    foreground?: ImageFilter | null,
  );
  /** k1*fg*bg + k2*fg + k3*bg + k4. See {@link ImageFilter.MakeArithmetic}. */
  constructor(
    kind: "arithmetic",
    k1: number,
    k2: number,
    k3: number,
    k4: number,
    enforcePMColor?: boolean,
    background?: ImageFilter | null,
    foreground?: ImageFilter | null,
  );
  /** Displace pixels by a map. See {@link ImageFilter.MakeDisplacementMap}. */
  constructor(
    kind: "displacement-map",
    xChannel: ColorChannel,
    yChannel: ColorChannel,
    scale: number,
    displacement?: ImageFilter | null,
    color?: ImageFilter | null,
  );
  /** Convolution kernel. See {@link ImageFilter.MakeMatrixConvolution}. */
  constructor(
    kind: "matrix-convolution",
    kernelSize: [number, number],
    kernel: number[],
    gain: number,
    bias: number,
    kernelOffset: [number, number],
    tileMode?: TileMode,
    convolveAlpha?: boolean,
    input?: ImageFilter | null,
    /** 🧪 Bounds the kernel's domain as well as clipping the output. */
    crop?: [number, number, number, number] | null,
  );
  /** Affine or 3x3 transform. See {@link ImageFilter.MakeMatrixTransform}. */
  constructor(
    kind: "matrix-transform",
    matrix: number[],
    sampling?: SamplingMode,
    input?: ImageFilter | null,
  );
  /** Fisheye lens. See {@link ImageFilter.MakeMagnifier}. */
  constructor(
    kind: "magnifier",
    lensBounds: [number, number, number, number],
    zoomAmount: number,
    inset: number,
    sampling?: SamplingMode,
    input?: ImageFilter | null,
  );
  /** Restrict to a rect. See {@link ImageFilter.MakeCrop}. */
  constructor(
    kind: "crop",
    rect: [number, number, number, number],
    tileMode?: TileMode,
    input?: ImageFilter | null,
  );
  /** Diffuse light from a direction. See {@link ImageFilter.MakeDistantLitDiffuse}. */
  constructor(
    kind: "distant-lit-diffuse",
    direction: Point3,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  );
  /** Diffuse light from a point. See {@link ImageFilter.MakePointLitDiffuse}. */
  constructor(
    kind: "point-lit-diffuse",
    location: Point3,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  );
  /** Diffuse light from a spot. See {@link ImageFilter.MakeSpotLitDiffuse}. */
  constructor(
    kind: "spot-lit-diffuse",
    location: Point3,
    target: Point3,
    falloffExponent: number,
    cutoffAngle: number,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  );
  /** Specular light from a direction. See {@link ImageFilter.MakeDistantLitSpecular}. */
  constructor(
    kind: "distant-lit-specular",
    direction: Point3,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  );
  /** Specular light from a point. See {@link ImageFilter.MakePointLitSpecular}. */
  constructor(
    kind: "point-lit-specular",
    location: Point3,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  );
  /** Specular light from a spot. See {@link ImageFilter.MakeSpotLitSpecular}. */
  constructor(
    kind: "spot-lit-specular",
    location: Point3,
    target: Point3,
    falloffExponent: number,
    cutoffAngle: number,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  );

  /**
   * Create ImageFilter from ColorFilter.
   * @param colorFilter - The color filter to wrap
   * @param input - Optional previous filter for chaining
   * @returns ImageFilter or null on Skia internal failure
   * @throws Error if colorFilter has been deleted
   */
  static MakeColorFilter(
    colorFilter: ColorFilter,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Compose two ImageFilters (outer applied after inner).
   * @returns ImageFilter or null on Skia internal failure
   * @throws Error if either filter has been deleted
   */
  static MakeCompose(
    outer: ImageFilter,
    inner: ImageFilter,
  ): ImageFilter | null;

  /**
   * Create blur ImageFilter.
   * @param sigmaX - horizontal blur radius
   * @param sigmaY - vertical blur radius
   * @param tileMode - edge behavior: "clamp" | "repeat" | "mirror" | "decal"
   * @param input - optional input filter for chaining
   */
  static MakeBlur(
    sigmaX: number,
    sigmaY: number,
    tileMode?: "clamp" | "repeat" | "mirror" | "decal",
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Create drop shadow ImageFilter.
   * @param dx - shadow x offset
   * @param dy - shadow y offset
   * @param sigmaX - horizontal blur radius
   * @param sigmaY - vertical blur radius
   * @param color - CSS color string or [r,g,b,a] array (0-1 floats)
   * @param input - optional input filter for chaining
   */
  static MakeDropShadow(
    dx: number,
    dy: number,
    sigmaX: number,
    sigmaY: number,
    color: string | [number, number, number, number],
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Create drop shadow ImageFilter (shadow only, no source image).
   * @param dx - shadow x offset
   * @param dy - shadow y offset
   * @param sigmaX - horizontal blur radius
   * @param sigmaY - vertical blur radius
   * @param color - CSS color string or [r,g,b,a] array (0-1 floats)
   * @param input - optional input filter for chaining
   */
  static MakeDropShadowOnly(
    dx: number,
    dy: number,
    sigmaX: number,
    sigmaY: number,
    color: string | [number, number, number, number],
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Create offset ImageFilter.
   * @param dx - x offset
   * @param dy - y offset
   * @param input - optional input filter for chaining
   */
  static MakeOffset(
    dx: number,
    dy: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Create morphological dilation ImageFilter.
   * @param radiusX - horizontal radius
   * @param radiusY - vertical radius
   * @param input - optional input filter for chaining
   * @param crop - 🧪 `[x, y, width, height]` bounding the kernel's domain as
   *   well as clipping the output, so the dilation stops spreading at the
   *   edge rather than spreading and then being cut. Not the same as
   *   composing a separate `"crop"` filter afterwards.
   * @throws TypeError if `crop` is given and is not four finite numbers
   */
  static MakeDilate(
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
    crop?: [number, number, number, number] | null,
  ): ImageFilter | null;

  /**
   * Create morphological erosion ImageFilter.
   * @param radiusX - horizontal radius
   * @param radiusY - vertical radius
   * @param input - optional input filter for chaining
   * @param crop - 🧪 `[x, y, width, height]` bounding the kernel's domain as
   *   well as clipping the output. See {@link ImageFilter.MakeDilate}.
   * @throws TypeError if `crop` is given and is not four finite numbers
   */
  static MakeErode(
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
    crop?: [number, number, number, number] | null,
  ): ImageFilter | null;

  /**
   * Merge multiple ImageFilters into one.
   * @param filters - array of filters to merge (null entries allowed)
   */
  static MakeMerge(filters: (ImageFilter | null)[]): ImageFilter | null;

  /**
   * A filter that produces no output.
   *
   * Empty means an empty result, not an absent filter: anything drawn through
   * it disappears. Assign `null` to `ctx.imageFilter` to draw unfiltered.
   */
  static MakeEmpty(): ImageFilter;

  /**
   * Create tile ImageFilter.
   * @param src - source rect [x, y, width, height]
   * @param dst - destination rect [x, y, width, height]
   * @param input - optional input filter for chaining
   */
  static MakeTile(
    src: [number, number, number, number],
    dst: [number, number, number, number],
    input?: ImageFilter | null,
  ): ImageFilter | null;

  // ==================== Advanced ImageFilter methods ====================

  /**
   * Blend two image filters using a blend mode.
   * @param mode - blend mode ("srcOver", "multiply", "screen", etc.)
   * @param background - background filter (or null for source)
   * @param foreground - foreground filter (or null for source)
   */
  static MakeBlend(
    mode: BlendMode,
    background?: ImageFilter | null,
    foreground?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Arithmetic blend: k1*fg*bg + k2*fg + k3*bg + k4.
   * @param k1 - coefficient for fg*bg
   * @param k2 - coefficient for fg
   * @param k3 - coefficient for bg
   * @param k4 - constant offset
   * @param enforcePMColor - enforce premultiplied color (default true)
   * @param background - background filter (or null for source)
   * @param foreground - foreground filter (or null for source)
   */
  static MakeArithmetic(
    k1: number,
    k2: number,
    k3: number,
    k4: number,
    enforcePMColor?: boolean,
    background?: ImageFilter | null,
    foreground?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Displacement map filter.
   * @param xChannel - color channel for x displacement ("R", "G", "B", "A")
   * @param yChannel - color channel for y displacement ("R", "G", "B", "A")
   * @param scale - displacement scale
   * @param displacement - displacement map filter (or null for source)
   * @param color - color source filter (or null for source)
   */
  static MakeDisplacementMap(
    xChannel: ColorChannel,
    yChannel: ColorChannel,
    scale: number,
    displacement?: ImageFilter | null,
    color?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Matrix convolution filter (e.g., sharpen, edge detect).
   * @param kernelSize - [width, height] of kernel
   * @param kernel - convolution kernel (width*height elements)
   * @param gain - scale factor applied to result
   * @param bias - bias added to result
   * @param kernelOffset - [x, y] offset for kernel center
   * @param tileMode - tile mode for edge handling (default "decal")
   * @param convolveAlpha - whether to convolve alpha channel (default true)
   * @param input - optional input filter for chaining
   * @param crop - 🧪 `[x, y, width, height]` bounding the kernel's domain as
   *   well as clipping the output. See {@link ImageFilter.MakeDilate}.
   */
  static MakeMatrixConvolution(
    kernelSize: [number, number],
    kernel: number[],
    gain: number,
    bias: number,
    kernelOffset: [number, number],
    tileMode?: TileMode,
    convolveAlpha?: boolean,
    input?: ImageFilter | null,
    crop?: [number, number, number, number] | null,
  ): ImageFilter | null;

  /**
   * Apply a matrix transformation to the image.
   *
   * The two lengths are read in different orders, so they are not the same
   * matrix written two ways. Six elements are `[a, b, c, d, e, f]` in canvas
   * `transform()` order -- `b` and `c` are the skews, `e` and `f` the
   * translation -- while nine are plain row-major. A uniform 2x scale is
   * therefore `[2, 0, 0, 2, 0, 0]`; the row-major-looking `[2, 0, 0, 0, 2, 0]`
   * sets the vertical scale to zero and Skia returns `null`.
   *
   * @param matrix - 6 elements in `transform()` order, or 9 row-major
   * @param sampling - one of the four {@link SamplingMode} names (default
   *   "linear")
   * @param input - optional input filter for chaining
   */
  static MakeMatrixTransform(
    matrix: number[],
    sampling?: SamplingMode,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Magnifier (fisheye) effect.
   * @param lensBounds - [x, y, width, height] of lens area
   * @param zoomAmount - magnification factor
   * @param inset - edge distortion width
   * @param sampling - sampling mode (default "linear")
   * @param input - optional input filter for chaining
   */
  static MakeMagnifier(
    lensBounds: [number, number, number, number],
    zoomAmount: number,
    inset: number,
    sampling?: SamplingMode,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Crop filter with optional tile mode.
   * @param rect - [x, y, width, height] crop rectangle
   * @param tileMode - tile mode for pixels outside rect (default "decal")
   * @param input - optional input filter for chaining
   */
  static MakeCrop(
    rect: [number, number, number, number],
    tileMode?: TileMode,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  // ==================== Lighting ImageFilter methods ====================

  /**
   * Diffuse lighting from a distant light source.
   * @param direction - [x, y, z] light direction
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param kd - diffuse reflectance coefficient
   * @param input - optional input filter (alpha as height map)
   */
  static MakeDistantLitDiffuse(
    direction: Point3,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Diffuse lighting from a point light source.
   * @param location - [x, y, z] light position
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param kd - diffuse reflectance coefficient
   * @param input - optional input filter
   */
  static MakePointLitDiffuse(
    location: Point3,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Diffuse lighting from a spot light source.
   * @param location - [x, y, z] light position
   * @param target - [x, y, z] spot target
   * @param falloffExponent - how sharply the light fades toward the edge of
   *   the cone
   * @param cutoffAngle - the cone's half-angle, in degrees
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param kd - diffuse reflectance coefficient
   * @param input - optional input filter
   */
  static MakeSpotLitDiffuse(
    location: Point3,
    target: Point3,
    falloffExponent: number,
    cutoffAngle: number,
    lightColor: string,
    surfaceScale: number,
    kd: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Specular lighting from a distant light source.
   * @param direction - [x, y, z] light direction
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param ks - specular reflectance coefficient
   * @param shininess - specular exponent
   * @param input - optional input filter
   */
  static MakeDistantLitSpecular(
    direction: Point3,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Specular lighting from a point light source.
   * @param location - [x, y, z] light position
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param ks - specular reflectance coefficient
   * @param shininess - specular exponent
   * @param input - optional input filter
   */
  static MakePointLitSpecular(
    location: Point3,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Specular lighting from a spot light source.
   * @param location - [x, y, z] light position
   * @param target - [x, y, z] spot target
   * @param falloffExponent - how sharply the light fades toward the edge of
   *   the cone
   * @param cutoffAngle - the cone's half-angle, in degrees
   * @param lightColor - CSS color of the light
   * @param surfaceScale - height scale factor
   * @param ks - specular reflectance coefficient
   * @param shininess - specular exponent
   * @param input - optional input filter
   */
  static MakeSpotLitSpecular(
    location: Point3,
    target: Point3,
    falloffExponent: number,
    cutoffAngle: number,
    lightColor: string,
    surfaceScale: number,
    ks: number,
    shininess: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Mark filter as deleted. Use-after-delete throws Error.
   */
  delete(): void;
}

/**
 * Coverage-mask filter (styled Gaussian blur). Unlike an `ImageFilter`
 * blur, the `BlurStyle` controls how the blur relates to the geometry:
 * glow, halo, inner shadow, feathered fill. Set on a context via
 * `ctx.maskFilter`. Mirrors CanvasKit's `MaskFilter`.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class MaskFilter {
  /**
   * Create a coverage-mask blur.
   *
   * A blur is the only kind of mask filter Skia offers, so the first argument
   * is the blur style rather than a kind. Where {@link MaskFilter.MakeBlur}
   * returns `null` for a sigma Skia rejects, this throws instead.
   *
   * ```ts
   * ctx.maskFilter = new MaskFilter("outer", 6)
   * ```
   *
   * @param style - "normal" (both sides), "solid" (glow keeping the
   *   shape), "outer" (halo only), "inner" (inner shadow only)
   * @param sigma - blur standard deviation in pixels, greater than 0
   * @param respectCTM - scale the blur with the canvas transform
   *   (default true); pass false to keep it screen-fixed
   * @throws TypeError if the arguments describe no filter
   */
  constructor(
    style: "normal" | "solid" | "outer" | "inner",
    sigma: number,
    respectCTM?: boolean,
  );
  /**
   * Gaussian coverage blur.
   * @param style - "normal" (both sides), "solid" (glow keeping the
   *   shape), "outer" (halo only), "inner" (inner shadow only)
   * @param sigma - blur standard deviation in pixels
   * @param respectCTM - scale the blur with the canvas transform
   *   (default true); pass false to keep it screen-fixed
   */
  static MakeBlur(
    style: "normal" | "solid" | "outer" | "inner",
    sigma: number,
    respectCTM?: boolean,
  ): MaskFilter | null;
  /** Mark filter as deleted. Use-after-delete throws Error. */
  delete(): void;
}

/**
 * A reusable shader, settable as `ctx.fillStyle` / `strokeStyle`.
 * Currently the procedural-noise factories; gradient shaders are
 * reachable via `createLinear/Radial/ConicGradient`. Mirrors CanvasKit's
 * `Shader`.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class Shader {
  /**
   * Create a procedural noise shader, for use as a fill or stroke style.
   *
   * Both kinds take the same four arguments and differ in how the noise is
   * summed: `"fractal-noise"` is soft and cloud-like, `"turbulence"` sharper
   * and more chaotic. Where the `Make` methods return `null` for arguments
   * Skia rejects, the constructor throws instead.
   *
   * ```ts
   * ctx.fillStyle = new Shader("turbulence", 0.08, 0.08, 4, 0)
   * ```
   *
   * @param kind - which noise function to sum
   * @param baseFreqX - noise frequency along x (small = larger features)
   * @param baseFreqY - noise frequency along y
   * @param octaves - detail levels
   * @param seed - pattern seed
   * @throws TypeError if the kind is unknown or the arguments describe no shader
   */
  constructor(
    kind: "fractal-noise" | "turbulence",
    baseFreqX: number,
    baseFreqY: number,
    octaves: number,
    seed: number,
  );
  /**
   * Fractal (Perlin) noise -- film grain, clouds, organic texture.
   * @param baseFreqX - noise frequency along x (small = larger features)
   * @param baseFreqY - noise frequency along y
   * @param octaves - detail levels
   * @param seed - pattern seed
   */
  static MakeFractalNoise(
    baseFreqX: number,
    baseFreqY: number,
    octaves: number,
    seed: number,
  ): Shader | null;
  /** Turbulence (absolute-value Perlin noise) -- sharper than fractal. */
  static MakeTurbulence(
    baseFreqX: number,
    baseFreqY: number,
    octaves: number,
    seed: number,
  ): Shader | null;
  /** Mark shader as deleted. Use-after-delete throws Error. */
  delete(): void;
}

/**
 * 4x5 color-matrix helpers (CanvasKit `ColorMatrixHelpers`). Each method
 * returns a 20-element row-major matrix for `ColorFilter.MakeMatrix`.
 * Use to build hue-rotate / saturation / brightness grades.
 */
export const ColorMatrix: {
  /** The identity matrix (no color change). */
  identity(): number[];
  /** Concatenate two matrices: applies `inner`, then `outer`. */
  concat(outer: number[], inner: number[]): number[];
  /**
   * Add a per-channel offset in place, and return the same array. Offsets
   * are in the 0-1 range that normalized colors use.
   */
  postTranslate(
    m: number[],
    dr: number,
    dg: number,
    db: number,
    da: number,
  ): number[];
  /** Hue rotation around a color axis (0=red, 1=green, 2=blue). */
  rotated(axis: 0 | 1 | 2, sine: number, cosine: number): number[];
  /** Per-channel scale (1 = unchanged). */
  scaled(
    redScale: number,
    greenScale: number,
    blueScale: number,
    alphaScale: number,
  ): number[];
};

//
// Context
//

type CanvasDrawable = Canvas | Image | ImageData;
type CanvasPatternSource = Canvas | Image | ImageData;
type CanvasDirection = "inherit" | "ltr" | "rtl";
type CanvasFillRule = "evenodd" | "nonzero";
type CanvasFontStretch =
  | "condensed"
  | "expanded"
  | "extra-condensed"
  | "extra-expanded"
  | "normal"
  | "semi-condensed"
  | "semi-expanded"
  | "ultra-condensed"
  | "ultra-expanded";
type CanvasTextAlign =
  "center" | "end" | "left" | "right" | "start" | "justify";
type CanvasTextBaseline =
  "alphabetic" | "bottom" | "hanging" | "ideographic" | "middle" | "top";
type CanvasLineCap = "butt" | "round" | "square";
type CanvasLineJoin = "bevel" | "miter" | "round";
// type CanvasFontKerning = "auto" | "none" | "normal";
type CanvasFontVariantCaps =
  | "all-petite-caps"
  | "all-small-caps"
  | "normal"
  | "petite-caps"
  | "small-caps"
  | "titling-caps"
  | "unicase";
// type CanvasTextRendering = "auto" | "geometricPrecision" | "optimizeLegibility" | "optimizeSpeed";

type Offset = [x: number, y: number] | number;
type QuadOrRect =
  | [
      x1: number,
      y1: number,
      x2: number,
      y2: number,
      x3: number,
      y3: number,
      x4: number,
      y4: number,
    ]
  | [left: number, top: number, right: number, bottom: number]
  | [width: number, height: number];
type GlobalCompositeOperation =
  | "color"
  | "color-burn"
  | "color-dodge"
  | "copy"
  | "darken"
  | "destination-atop"
  | "destination-in"
  | "destination-out"
  | "destination-over"
  | "difference"
  | "exclusion"
  | "hard-light"
  | "hue"
  | "lighten"
  | "lighter"
  | "luminosity"
  | "multiply"
  | "overlay"
  | "saturation"
  | "screen"
  | "soft-light"
  | "source-atop"
  | "source-in"
  | "source-out"
  | "source-over"
  | "xor";
type ImageSmoothingQuality = "high" | "low" | "medium";

type FontVariantSetting =
  | "normal"
  /* alternates */
  | "historical-forms"
  /* caps */
  | "small-caps"
  | "all-small-caps"
  | "petite-caps"
  | "all-petite-caps"
  | "unicase"
  | "titling-caps"
  /* numeric */
  | "lining-nums"
  | "oldstyle-nums"
  | "proportional-nums"
  | "tabular-nums"
  | "diagonal-fractions"
  | "stacked-fractions"
  | "ordinal"
  | "slashed-zero"
  /* ligatures */
  | "common-ligatures"
  | "no-common-ligatures"
  | "discretionary-ligatures"
  | "no-discretionary-ligatures"
  | "historical-ligatures"
  | "no-historical-ligatures"
  | "contextual"
  | "no-contextual"
  /* east-asian */
  | "jis78"
  | "jis83"
  | "jis90"
  | "jis04"
  | "simplified"
  | "traditional"
  | "full-width"
  | "proportional-width"
  | "ruby"
  /* position */
  | "super"
  | "sub";

/** 🧪 Not in the HTML Canvas standard. */
export interface CreateTextureOptions {
  /** The 2D shape to be drawn in a repeating grid with the specified spacing (if omitted, parallel lines will be used) */
  path?: Path2D;

  /** The lineWidth with which to stroke the path (if omitted, the path will be filled instead) */
  line?: number;

  /** The lineCap style to use if stroking the path */
  cap?: CanvasLineCap;

  /** The color to use for stroking/filling the path */
  color?: string;

  /** The orientation of the pattern grid in radians */
  angle?: number;

  /** The amount by which to shift the pattern relative to the canvas origin */
  offset?: Offset;

  /** Whether to render the texture as a single path (rather than as a repeating pattern within a clipping mask) */
  outline?: boolean;
}

interface CanvasCompositing {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/globalAlpha) */
  globalAlpha: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/globalCompositeOperation) */
  globalCompositeOperation: GlobalCompositeOperation;
}

interface CanvasDrawImage {
  drawImage(image: CanvasDrawable, dx: number, dy: number): void;
  drawImage(
    image: CanvasDrawable,
    dx: number,
    dy: number,
    dw: number,
    dh: number,
  ): void;
  drawImage(
    image: CanvasDrawable,
    sx: number,
    sy: number,
    sw: number,
    sh: number,
    dx: number,
    dy: number,
    dw: number,
    dh: number,
  ): void;
  /**
   * Draw another canvas, replaying its contents as vectors.
   *
   * Unlike {@link CanvasRenderingContext2D.drawImage}, which rasterizes the
   * source first, this keeps text as text and paths as paths -- so an SVG or
   * PDF export of the result stays selectable and scalable.
   *
   * The replay is clipped to the destination rectangle, so anything that
   * spreads -- an `imageFilter` blur, a shadow -- stops at its edge instead of
   * bleeding past it the way it does through `drawImage`. Give the destination
   * room if you want the spread. This is inherited behaviour, matching
   * upstream, not a rule the Canvas standard sets: `drawCanvas` has no
   * standard counterpart, and `drawImage` does let a filter spread.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  drawCanvas(image: Canvas, dx: number, dy: number): void;
  drawCanvas(
    image: Canvas,
    dx: number,
    dy: number,
    dw: number,
    dh: number,
  ): void;
  drawCanvas(
    image: Canvas,
    sx: number,
    sy: number,
    sw: number,
    sh: number,
    dx: number,
    dy: number,
    dw: number,
    dh: number,
  ): void;
}

interface CanvasDrawPath {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/beginPath) */
  beginPath(): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/clip) */
  clip(fillRule?: CanvasFillRule): void;
  clip(path: Path2D, fillRule?: CanvasFillRule): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fill) */
  fill(fillRule?: CanvasFillRule): void;
  fill(path: Path2D, fillRule?: CanvasFillRule): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/isPointInPath) */
  isPointInPath(x: number, y: number, fillRule?: CanvasFillRule): boolean;
  isPointInPath(
    path: Path2D,
    x: number,
    y: number,
    fillRule?: CanvasFillRule,
  ): boolean;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/isPointInStroke) */
  isPointInStroke(x: number, y: number): boolean;
  isPointInStroke(path: Path2D, x: number, y: number): boolean;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/stroke) */
  stroke(): void;
  stroke(path: Path2D): void;
}

interface CanvasFillStrokeStyles {
  /**
   * Solid color, gradient, pattern, or texture used for fills.
   *
   * Color inputs follow the [`Color4fInput`] convention:
   *
   * - A **CSS string** (`"#ff8800"`, `"rgb(...)"`, named colors) is parsed as
   *   sRGB-gamma.
   * - A **`[r, g, b, a]` array** carries premultiplied **linear-light** floats
   *   interpreted in the surface's working color space; pass these to avoid
   *   the lossy sRGB-encoding round-trip used by the CSS form (the shape
   *   mirrors `TextStyleInput.color` and CanvasKit's `Paint.setColor`).
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fillStyle)
   */
  fillStyle:
    Color4fInput | CanvasGradient | CanvasPattern | CanvasTexture | Shader;
  /**
   * Solid color, gradient, pattern, texture, or shader used for strokes. See
   * `fillStyle` for the color-input contract.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/strokeStyle)
   */
  strokeStyle:
    Color4fInput | CanvasGradient | CanvasPattern | CanvasTexture | Shader;
  /**
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createConicGradient)
   *
   * The optional fourth argument is this library's own: the Canvas API
   * always sweeps a full turn, and Skia can sweep any arc. Omitting it
   * draws exactly what a browser draws.
   */
  createConicGradient(
    startAngle: number,
    x: number,
    y: number,
    endAngle?: number,
  ): CanvasGradient;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createLinearGradient) */
  createLinearGradient(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
  ): CanvasGradient;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createPattern) */
  createPattern(
    image: CanvasPatternSource,
    repetition: string | null,
  ): CanvasPattern | null;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createRadialGradient) */
  createRadialGradient(
    x0: number,
    y0: number,
    r0: number,
    x1: number,
    y1: number,
    r1: number,
  ): CanvasGradient;

  /**
   * [Guide](https://github.com/l7aromeo/meo-skia-canvas/blob/main/docs/api/context.md#createtexture)
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  createTexture(spacing: Offset, options?: CreateTextureOptions): CanvasTexture;
}

interface CanvasFilters {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/filter) */
  filter: string;
}

interface CanvasImageData {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createImageData) */
  createImageData(
    width: number,
    height: number,
    settings?: ImageDataSettings,
  ): ImageData;
  createImageData(imagedata: ImageData): ImageData;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/getImageData) */
  getImageData(
    x: number,
    y: number,
    width: number,
    height: number,
    settings?: ImageDataExportSettings,
  ): ImageData;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/putImageData) */
  putImageData(imagedata: ImageData, dx: number, dy: number): void;
  putImageData(
    imagedata: ImageData,
    dx: number,
    dy: number,
    dirtyX: number,
    dirtyY: number,
    dirtyWidth: number,
    dirtyHeight: number,
  ): void;
}

interface CanvasImageSmoothing {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/imageSmoothingEnabled) */
  imageSmoothingEnabled: boolean;
  /**
   * Dither draws to break up banding in gradients and dark frames on
   * 8-bit surfaces. Mirrors CanvasKit's `Paint.setDither`. Not part of
   * the HTML Canvas standard. Default `false`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  dither: boolean;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/imageSmoothingQuality) */
  imageSmoothingQuality: ImageSmoothingQuality;
}

interface CanvasPath {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/arc) */
  arc(
    x: number,
    y: number,
    radius: number,
    startAngle: number,
    endAngle: number,
    counterclockwise?: boolean,
  ): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/arcTo) */
  arcTo(x1: number, y1: number, x2: number, y2: number, radius: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/bezierCurveTo) */
  bezierCurveTo(
    cp1x: number,
    cp1y: number,
    cp2x: number,
    cp2y: number,
    x: number,
    y: number,
  ): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/closePath) */
  closePath(): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/ellipse) */
  ellipse(
    x: number,
    y: number,
    radiusX: number,
    radiusY: number,
    rotation: number,
    startAngle: number,
    endAngle: number,
    counterclockwise?: boolean,
  ): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/lineTo) */
  lineTo(x: number, y: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/moveTo) */
  moveTo(x: number, y: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/quadraticCurveTo) */
  quadraticCurveTo(cpx: number, cpy: number, x: number, y: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/rect) */
  rect(x: number, y: number, w: number, h: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/roundRect) */
  roundRect(
    x: number,
    y: number,
    w: number,
    h: number,
    radii?: number | DOMPointInit | (number | DOMPointInit)[],
  ): void;
}

interface CanvasPathDrawingStyles {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/lineCap) */
  lineCap: CanvasLineCap;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/lineDashOffset) */
  lineDashOffset: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/lineJoin) */
  lineJoin: CanvasLineJoin;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/lineWidth) */
  lineWidth: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/miterLimit) */
  miterLimit: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/getLineDash) */
  getLineDash(): number[];
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/setLineDash) */
  setLineDash(segments: Iterable<number>): void;
}

interface CanvasRect {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/clearRect) */
  clearRect(x: number, y: number, w: number, h: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fillRect) */
  fillRect(x: number, y: number, w: number, h: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/strokeRect) */
  strokeRect(x: number, y: number, w: number, h: number): void;
}

interface CanvasShadowStyles {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/shadowBlur) */
  shadowBlur: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/shadowColor) */
  shadowColor: string;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/shadowOffsetX) */
  shadowOffsetX: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/shadowOffsetY) */
  shadowOffsetY: number;
}

interface CanvasState {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/reset) */
  reset(): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/restore) */
  restore(): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/save) */
  save(): void;
  /**
   * Push an isolated compositing layer (CanvasKit `saveLayer`). Draws
   * until the matching `restore()` accumulate into the layer, which is
   * then composited onto the canvas at `alpha` (default 1) with the
   * current `globalCompositeOperation`. `bounds` is an optional
   * `[x, y, w, h]` that **clips** the layer -- Skia describes it as a
   * sizing hint for the offscreen, but nothing outside it is drawn.
   * `backdrop` applies an ImageFilter to the content behind the layer
   * (blur-behind / frosted glass). Not part of the HTML Canvas standard.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  saveLayer(
    alpha?: number,
    bounds?: [number, number, number, number] | null,
    backdrop?: ImageFilter | null,
  ): void;

  /**
   * Always `false`.
   *
   * Context loss is a GPU-compositor event -- a browser reclaiming the backing
   * store of a backgrounded tab -- and there is no compositor here. A canvas
   * either has its surface or its construction failed.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/isContextLost)
   */
  isContextLost(): boolean;
}

interface CanvasText {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fillText) */
  fillText(text: string, x: number, y: number, maxWidth?: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/measureText) */
  measureText(text: string): TextMetrics;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/strokeText) */
  strokeText(text: string, x: number, y: number, maxWidth?: number): void;
}

interface CanvasTextDrawingStyles {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/direction) */
  direction: CanvasDirection;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/font) */
  font: string;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fontStretch) */
  fontStretch: CanvasFontStretch;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/letterSpacing) */
  letterSpacing: string;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/textAlign) */
  textAlign: CanvasTextAlign;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/textBaseline) */
  textBaseline: CanvasTextBaseline;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/wordSpacing) */
  wordSpacing: string;

  /**
   * The capitalization axis of {@link CanvasRenderingContext2D.fontVariant}.
   *
   * This is the CSS longhand and `fontVariant` the shorthand, so writing here
   * replaces only the caps token and leaves the other axes -- figures,
   * ligatures, alternates -- as they were. An unrecognised value is ignored,
   * as the standard requires of an attribute setter.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fontVariantCaps)
   */
  fontVariantCaps: CanvasFontVariantCaps;

  // UNIMPLEMENTED
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/textRendering) */
  // textRendering: CanvasTextRendering;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fontKerning) */
  // fontKerning: CanvasFontKerning;
}

interface CanvasTransform {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/getTransform) */
  getTransform(): DOMMatrix;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/resetTransform) */
  resetTransform(): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/rotate) */
  rotate(angle: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/scale) */
  scale(x: number, y: number): void;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/setTransform) */
  setTransform(
    a: number,
    b: number,
    c: number,
    d: number,
    e: number,
    f: number,
  ): void;

  /** transform argument extensions (accept DOMMatrix & matrix-like objectx, not just param lists) */
  setTransform(transform?: Matrix): void;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/transform) */
  transform(
    a: number,
    b: number,
    c: number,
    d: number,
    e: number,
    f: number,
  ): void;
  transform(transform: Matrix): void;

  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/translate) */
  translate(x: number, y: number): void;
}

/**
 * The CanvasRenderingContext2D interface, part of the Canvas API, provides the 2D rendering context for the drawing surface of a <canvas> element. It is used for drawing shapes, text, images, and other objects.
 *
 * - [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D)
 * - [Guide](https://github.com/l7aromeo/meo-skia-canvas/blob/main/docs/api/context.md)
 */
export interface CanvasRenderingContext2D
  extends
    CanvasCompositing,
    CanvasDrawImage,
    CanvasDrawPath,
    CanvasFillStrokeStyles,
    CanvasFilters,
    CanvasImageData,
    CanvasImageSmoothing,
    CanvasPath,
    CanvasPathDrawingStyles,
    CanvasRect,
    CanvasShadowStyles,
    CanvasState,
    CanvasText,
    CanvasTextDrawingStyles,
    CanvasTransform {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/canvas) */
  readonly canvas: Canvas;
  /** 🧪 Not in the HTML Canvas standard. */
  fontVariant: FontVariantSetting;
  /** 🧪 Not in the HTML Canvas standard. */
  fontVariationSettings: string;
  /** 🧪 Not in the HTML Canvas standard. */
  fontHinting: boolean;
  /** 🧪 Not in the HTML Canvas standard. */
  textWrap: boolean;
  /** 🧪 Not in the HTML Canvas standard. */
  textDecoration: string;
  /** 🧪 Not in the HTML Canvas standard. */
  lineDashMarker: Path2D | null;
  /** 🧪 Not in the HTML Canvas standard. */
  lineDashFit: "move" | "turn" | "follow";

  // skia/chrome beziers & convenience methods
  /**
   * The context's current transformation matrix, as an alternative to
   * `getTransform()` and `setTransform()`.
   *
   * The setter takes anything `new DOMMatrix()` does -- a matrix, a CSS
   * transform string, an `{a, b, c, d, e, f}` object, or a 6- or 16-element
   * array -- and replaces the transform rather than multiplying into it.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  get currentTransform(): DOMMatrix;
  set currentTransform(matrix: Matrix);
  /** 🧪 Not in the HTML Canvas standard. */
  createProjection(quad: QuadOrRect, basis?: QuadOrRect): DOMMatrix;
  /** 🧪 Not in the HTML Canvas standard. */
  /**
   * Curve to `(x, y)`, pulled toward the control point.
   *
   * `weight` sets how close the curve comes: `0` draws a straight line, `1`
   * matches `quadraticCurveTo`, and larger values pull it nearer the control
   * point.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  conicCurveTo(
    cpx: number,
    cpy: number,
    x: number,
    y: number,
    weight: number,
  ): void;
  // getContextAttributes(): CanvasRenderingContext2DSettings;

  // add optional maxWidth to work in conjunction with textWrap
  measureText(text: string, maxWidth?: number): TextMetrics;
  /** 🧪 Not in the HTML Canvas standard. */
  outlineText(text: string, maxWidth?: number): Path2D;

  // Skia filter properties (CanvasKit parity)
  /**
   * Color filter applied during drawing. Set null to disable.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  colorFilter: ColorFilter | null;
  /**
   * Image filter applied during drawing. Set null to disable.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  imageFilter: ImageFilter | null;
  /**
   * Coverage-mask filter (styled blur) applied during drawing -- glows,
   * feathered edges, outline blur. Set null to disable.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  maskFilter: MaskFilter | null;

  /**
   * Paints a laid-out {@link Paragraph} with its top-left corner at `(x, y)`.
   *
   * This is the only way to render a `Paragraph`; the rest of that API builds
   * and measures one. Rich text, per-span styling, strut-controlled leading
   * and inline placeholders all arrive through here rather than through
   * {@link CanvasRenderingContext2D.fillText}.
   *
   * Three differences from `fillText` that are easy to trip over:
   *
   * - **{@link Paragraph.layout} must have been called first.** A paragraph
   *   that has never been laid out has no line breaks to draw, and this
   *   silently paints nothing rather than throwing.
   * - **`(x, y)` is the top-left of the text block**, not a baseline. The
   *   same coordinates passed to `fillText` put the text roughly one line
   *   higher.
   * - **Colour comes from the text style, not from `fillStyle`.** Setting
   *   `fillStyle` before the call changes nothing; colour, opacity and
   *   decoration are whatever the `TextStyleInput` carried.
   * - **The compositing state does apply**, along with the transform and clip:
   *   the paragraph is drawn as a group, so `globalAlpha` fades it and
   *   `globalCompositeOperation` composites it. Before 4.2.0 both were
   *   dropped, and every blend mode behaved as `source-over`.
   *
   * @example
   * const builder = ParagraphBuilder.Make({
   *   textAlign: "center",
   *   textStyle: { fontSize: 18, color: [0, 0, 0, 1] },
   * });
   * builder.addText("Wrapped, styled, measured text.");
   *
   * const paragraph = builder.build();
   * paragraph.layout(320); // wrap width, in pixels
   * ctx.drawParagraph(paragraph, 20, 20);
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  drawParagraph(paragraph: Paragraph, x: number, y: number): void;
}

/**
 * The constructor object, exported so `ctx instanceof CanvasRenderingContext2D`
 * works. Contexts come from
 * {@link Canvas.getContext}; calling this directly throws, which is why no
 * construct signature is declared even though `lib.dom.d.ts` has one.
 */
declare var CanvasRenderingContext2D: {
  prototype: CanvasRenderingContext2D;
};

//
// Bézier Paths
//

/** 🧪 Not in the HTML Canvas standard. */
/**
 * The rectangle enclosing a path, from {@link Path2D.bounds}. Edges and
 * dimensions both, so neither has to be derived.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface Path2DBounds {
  /** Smallest y coordinate the path reaches. */
  readonly top: number;
  /** Smallest x coordinate the path reaches. */
  readonly left: number;
  /** Largest y coordinate the path reaches. */
  readonly bottom: number;
  /** Largest x coordinate the path reaches. */
  readonly right: number;
  /** `right - left`. */
  readonly width: number;
  /** `bottom - top`. */
  readonly height: number;
}

export type Path2DEdge = [verb: string, ...args: number[]];

/**
 * This Canvas 2D API interface is used to declare a path that can then be used on a CanvasRenderingContext2D object. The path methods of the CanvasRenderingContext2D interface are also present on this interface, which gives you the convenience of being able to retain and replay your path whenever desired.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/Path2D)
 */
interface Path2D extends CanvasPath {
  /**
   * The smallest rectangle containing the path.
   *
   * Does not account for `lineWidth`, so a stroked path covers more than this.
   * A browser `Path2D` is an opaque recorder of drawing commands with no way
   * to measure it.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly bounds: Path2DBounds;
  /**
   * Every segment added so far, as `["verb", ...points]` entries.
   *
   * The verbs and their arguments match the method names, so the list can be
   * replayed onto another path or onto a context.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly edges: readonly Path2DEdge[];
  /**
   * The path as SVG path data. Readable, writable, and appendable with `+=`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  d: string;

  /**
   * Adds the path given by the argument to the path
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/Path2D/addPath)
   */
  addPath(path: Path2D, transform?: DOMMatrix2DInit): void;

  /**
   * Whether the point is inside the path or on one of its contours.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  contains(x: number, y: number): boolean;
  /**
   * Curve to `(x, y)`, pulled toward the control point.
   *
   * `weight` sets how close the curve comes: `0` draws a straight line, `1`
   * matches `quadraticCurveTo`, and larger values pull it nearer the control
   * point.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  conicCurveTo(
    cpx: number,
    cpy: number,
    x: number,
    y: number,
    weight: number,
  ): void;

  /**
   * The area of `otherPath` that this path does not cover.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  complement(otherPath: Path2D): Path2D;
  /**
   * The area of this path that `otherPath` does not cover.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  difference(otherPath: Path2D): Path2D;
  /**
   * The area both paths cover.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  intersect(otherPath: Path2D): Path2D;
  /**
   * The area either path covers.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  union(otherPath: Path2D): Path2D;
  /**
   * The area exactly one of the paths covers.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  xor(otherPath: Path2D): Path2D;
  /**
   * A blend of two paths that share a sequence of verbs and differ only in
   * their points.
   *
   * `weight` picks the mix: `0` is this path, `1` is `otherPath`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  interpolate(otherPath: Path2D, weight: number): Path2D;

  /**
   * A copy broken into segments of `segmentLength` with each point displaced
   * at random by up to `amount`.
   *
   * The displacement is random but reproducible: the same `seed` gives the
   * same path every run.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  jitter(segmentLength: number, amount: number, seed?: number): Path2D;
  /**
   * A copy shifted by `dx` horizontally and `dy` vertically.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  offset(dx: number, dy: number): Path2D;
  /**
   * The positions along the path at every `step` pixels, `1` by default.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  points(step?: number): readonly [x: number, y: number][];
  /**
   * A copy with its corners rounded off to `radius`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  round(radius: number): Path2D;
  /**
   * A copy with overlapping segments within the path removed, as though the
   * path had been unioned with itself.
   *
   * `"evenodd"` keeps the overlap regions as holes while still removing the
   * edge crossings.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  simplify(rule?: "nonzero" | "evenodd"): Path2D;
  /**
   * A copy with its points transformed. The original is unmodified.
   *
   * Takes a `DOMMatrix`, a CSS transform string such as `"rotate(20deg)"`,
   * or the six numbers of a 2D matrix.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  transform(transform: Matrix): Path2D;
  transform(
    a: number,
    b: number,
    c: number,
    d: number,
    e: number,
    f: number,
  ): Path2D;
  /**
   * The portion of the path between two points along its contour, each given
   * as a fraction from `0` to `1`.
   *
   * `inverted` takes everything except that span instead. With one number,
   * a positive value trims from the start and a negative one trims to the
   * end.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  trim(start: number, end: number, inverted?: boolean): Path2D;
  trim(start: number, inverted?: boolean): Path2D;

  /**
   * A copy that covers the same area under the `"nonzero"` rule as this path
   * does under `"evenodd"`.
   *
   * Useful when one path holds several overlapping contours, where the filled
   * shape otherwise depends on their nesting and direction.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  unwind(): Path2D;
}

declare var Path2D: {
  prototype: Path2D;
  new (path?: Path2D | string): Path2D;
};

//
// Typography
//

/**
 * The dimensions of a piece of text in the canvas, as created by the CanvasRenderingContext2D.measureText() method.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics)
 */
interface TextMetrics {
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/actualBoundingBoxAscent) */
  readonly actualBoundingBoxAscent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/actualBoundingBoxDescent) */
  readonly actualBoundingBoxDescent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/actualBoundingBoxLeft) */
  readonly actualBoundingBoxLeft: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/actualBoundingBoxRight) */
  readonly actualBoundingBoxRight: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/alphabeticBaseline) */
  readonly alphabeticBaseline: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/emHeightAscent) */
  readonly emHeightAscent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/emHeightDescent) */
  readonly emHeightDescent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/fontBoundingBoxAscent) */
  readonly fontBoundingBoxAscent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/fontBoundingBoxDescent) */
  readonly fontBoundingBoxDescent: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/hangingBaseline) */
  readonly hangingBaseline: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/ideographicBaseline) */
  readonly ideographicBaseline: number;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/TextMetrics/width) */
  readonly width: number;

  /**
   * Metrics for each line separately, populated only when the context's
   * `textWrap` is `true`.
   *
   * 🧪 Not in the HTML Canvas standard.
   */
  readonly lines: TextMetricsLine[];
}

// No construct signature: measurements come from
// {@link CanvasRenderingContext2D.measureText}, and the browser has no
// `TextMetrics` constructor either. `prototype` stays so `instanceof` works.
declare var TextMetrics: {
  prototype: TextMetrics;
};

/** 🧪 Not in the HTML Canvas standard. */
export interface TextMetricsLine {
  /** Left edge of line bounding box */
  readonly x: number;
  /** Top edge of line bounding box */
  readonly y: number;
  /** Width of line bounding box */
  readonly width: number;
  /** Height of line bounding box */
  readonly height: number;
  /** Vertical position of currently selected textBaseline */
  readonly baseline: number;
  /** Vertical position of highest ascent for all fonts used in line */
  readonly ascent: number;
  /** Vertical position of lowest descent for all fonts used in line */
  readonly descent: number;
  /** Vertical position of hanging baseline (irrespective of current textBaseline setting) */
  readonly hangingBaseline: number;
  /** Vertical position of alphabetic baseline (irrespective of current textBaseline setting) */
  readonly alphabeticBaseline: number;
  /** Vertical position of ideographic baseline (irrespective of current textBaseline setting) */
  readonly ideographicBaseline: number;
  /** Character index into source string of the beginning of this line */
  readonly startIndex: number;
  /** Character index into source string of the end of this line */
  readonly endIndex: number;
  /** Array of dimensions & metrics for each single-font subsection of the line */
  readonly runs: TextMetricsRun[];
}

/** 🧪 Not in the HTML Canvas standard. */
export interface TextMetricsRun {
  /** Left edge of single-font run of characters */
  readonly x: number;
  /** Top edge of single-font run of characters */
  readonly y: number;
  /** Width of single-font run of characters */
  readonly width: number;
  /** Height of single-font run of characters */
  readonly height: number;
  /** Name of font family used in this run */
  readonly family: string;
  /** Vertical position of this font's ascent metric */
  readonly ascent: number;
  /** Vertical position of this font's descent metric */
  readonly descent: number;
  /** Vertical position of this font's capital letters */
  readonly capHeight: number;
  /** Vertical position of this font's ascender-less letters */
  readonly xHeight: number;
  /** Vertical position of the stroke used for underlines */
  readonly underline: number;
  /** Vertical position of the stroke used for strikethroughs */
  readonly strikethrough: number;
}

/**
 * What one family offers, as reported by {@link FontLibrary.family}. The
 * three lists describe the faces found under that name, system fonts and
 * registered ones together.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface FontFamily {
  /** The name asked for. */
  family: string;
  /** CSS numeric weights available, e.g. `[400, 700]`. */
  weights: number[];
  /** CSS width keywords available, e.g. `["normal", "condensed"]`. */
  widths: string[];
  /** Slants available, e.g. `["normal", "italic"]`. */
  styles: string[];
}

/**
 * One face registered by {@link FontLibrary.use}, described as the file it
 * was read from says.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface Font {
  /**
   * The name this face is filed under: the alias where one was given, and
   * the family name the file itself declares otherwise.
   */
  family: string;
  /** CSS numeric weight. */
  weight: number;
  /** Slant: `"normal"`, `"italic"`, or `"oblique"`. */
  style: string;
  /** CSS width keyword. */
  width: string;
  /**
   * Path the face was read from, or the literal `"<buffer>"` when it was
   * registered from font data rather than a file.
   */
  file: string;
}

/**
 * The process-wide font registry: what a draw can match by name, and how to
 * add faces that are not installed on the system.
 *
 * Its state is global rather than per-canvas, so a family registered here is
 * visible to every canvas, window and paragraph in the process.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
interface FontLibrary {
  /**
   * Every family a draw can match, sorted and de-duplicated -- the
   * platform's own plus anything {@link FontLibrary.use} has added.
   */
  families: readonly string[];
  /**
   * The weights, widths and styles available under `name`, or `undefined`
   * when nothing resolves under it.
   */
  family(name: string): FontFamily | undefined;
  /** Whether {@link FontLibrary.families} contains `familyName`. */
  has(familyName: string): boolean;

  /**
   * Register one or more font files, optionally under an alias of your own.
   *
   * Naming an alias files every face given under that family name, whatever
   * the file itself declares, which is how a face is reached from
   * `ctx.font` by a name of your choosing. Without one, each face keeps the
   * family name it declares.
   *
   * Paths, `Buffer`s and `ArrayBuffer`s are all accepted, as is an object
   * mapping several aliases to their files in one call. The return value
   * describes the faces that were read -- an empty array means the file
   * held no usable font.
   *
   * ```ts
   * FontLibrary.use("Colorfont", "./fonts/Colorfont-Regular.ttf")
   * FontLibrary.use(["./fonts/Inter-Regular.ttf", "./fonts/Inter-Bold.ttf"])
   * FontLibrary.use({ Headline: "./fonts/Playfair.ttf" })
   * ```
   */
  use(familyName: string, fontPaths?: string | readonly string[]): Font[];
  use(familyName: string, fontData: Buffer | ArrayBuffer): Font[];
  use(familyName: string, fontData: readonly (Buffer | ArrayBuffer)[]): Font[];
  use(fontPaths: readonly string[]): Font[];
  use(
    families: Record<string, readonly string[] | string>,
  ): Record<string, Font[]>;

  /**
   * Forget every face registered with {@link FontLibrary.use} and drop the
   * cached font collections. System fonts are unaffected.
   */
  reset(): void;
}

/**
 * The process-wide font registry.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const FontLibrary: FontLibrary;

/**
 * Decoration lines for {@link TextStyleInput.decoration}, as a bit mask:
 * combine them with `|` to underline and strike through at once.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const TextDecoration: {
  /** No line. */
  readonly NoDecoration: 0x0;
  /** A line below the text. */
  readonly Underline: 0x1;
  /** A line above the text. */
  readonly Overline: 0x2;
  /** A line through the text. */
  readonly LineThrough: 0x4;
};

/**
 * How a decoration line is drawn, for
 * {@link TextStyleInput.decorationStyle}.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const TextDecorationStyle: {
  /** One unbroken line. */
  readonly Solid: 0;
  /** Two parallel lines. */
  readonly Double: 1;
  /** A dotted line. */
  readonly Dotted: 2;
  /** A dashed line. */
  readonly Dashed: 3;
  /** A wavy line, as a spell-checker draws. */
  readonly Wavy: 4;
};

/**
 * How a placeholder sits against the line it interrupts.
 *
 * Passed to {@link ParagraphBuilder.addPlaceholder}; the numbering is
 * CanvasKit's.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const PlaceholderAlignment: {
  /** Line the placeholder's own baseline up with the text's. */
  readonly Baseline: 0;
  /** Sit the placeholder on top of the baseline. */
  readonly AboveBaseline: 1;
  /** Hang the placeholder below the baseline. */
  readonly BelowBaseline: 2;
  /** Align its top edge with the line's top. */
  readonly Top: 3;
  /** Align its bottom edge with the line's bottom. */
  readonly Bottom: 4;
  /** Centre it against the line. */
  readonly Middle: 5;
};

/**
 * Which baseline {@link PlaceholderAlignment.Baseline} aligns against.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const TextBaseline: {
  readonly Alphabetic: 0;
  readonly Ideographic: 1;
};

/**
 * One of the {@link PlaceholderAlignment} values.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type PlaceholderAlignmentValue =
  (typeof PlaceholderAlignment)[keyof typeof PlaceholderAlignment];

/**
 * One of the {@link TextBaseline} values.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type TextBaselineValue =
  (typeof TextBaseline)[keyof typeof TextBaseline];

/**
 * A bitmask of {@link TextDecoration} values.
 *
 * `number` rather than a union of the flags, deliberately: they combine, so
 * `TextDecoration.Underline | TextDecoration.LineThrough` is `0x5`, and a
 * union of the individual values would reject every combination -- which is
 * the only reason to have flags.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type TextDecorationMask = number;

/**
 * One of the {@link TextDecorationStyle} values.
 *
 * A union here, unlike {@link TextDecorationMask}: these do not combine, and
 * anything outside the set draws as `Solid`.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type TextDecorationStyleValue =
  (typeof TextDecorationStyle)[keyof typeof TextDecorationStyle];

//
// ParagraphBuilder & Paragraph
//

/**
 * Color input shared across the paint, gradient-stop, and text APIs.
 *
 * - A **CSS string** (`"#ff8800"`, `"rgb(...)"`, named colors): interpreted
 *   as sRGB-gamma; alpha taken from the CSS string when present (e.g.
 *   `"#ff8800ff"` or `"rgba(...)"`).
 * - A `[r, g, b, a]` **array** of premultiplied, **linear-light**
 *   sRGB-primaries floats (CanvasKit's `Paint.setColor4f` convention).
 *   Skia converts the linear value to the destination surface's working
 *   color space at paint time, so HDR (`>1.0`) and out-of-gamut values
 *   survive the round trip. Use this form when you have already done a
 *   perceptual-uniform conversion (e.g. OkLCH -> linear sRGB) and want
 *   to avoid the alpha-dropping `oklchToSrgbHex`-style shortcut.
 *
 * Accepted by:
 * - `CanvasRenderingContext2D.fillStyle` / `strokeStyle`
 * - `CanvasGradient.addColorStop(offset, color)`
 * - `TextStyleInput.color` / `foregroundColor` / `backgroundColor` /
 *   `decorationColor`, `TextShadowInput.color`
 */
export type Color4fInput = string | [number, number, number, number];

/**
 * @deprecated Use `Color4fInput`. Kept as an alias for backwards
 * compatibility with v3.5.0/3.5.1 consumers.
 */
export type TextColorInput = Color4fInput;

/** 🧪 Not in the HTML Canvas standard. */
export interface TextShadowInput {
  /** Shadow color, defaulting to black. */
  color?: TextColorInput;
  /** `[dx, dy]` displacement in pixels, defaulting to no offset. */
  offset?: [number, number];
  /** Blur radius in pixels, defaulting to `0` for a hard-edged shadow. */
  blurRadius?: number;
}

/**
 * Variable-font axis position. `axis` is a 4-character OpenType axis
 * tag (e.g. "wght", "wdth", "ital", "opsz"). `value` is a float in the
 * font's design space, clamped to the typeface's declared min/max for
 * that axis. Mirrors CanvasKit's `fontVariations` shape.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface FontVariationInput {
  /** Four-character OpenType axis tag, such as `"wght"` or `"opsz"`. */
  axis: string;
  /** Position on that axis, clamped to the range the typeface declares. */
  value: number;
}

/**
 * One OpenType feature applied to a text run. `name` is an OpenType
 * feature tag ("smcp", "liga", "onum", "tnum", "ss01", "zero", ...);
 * `value` is the feature selector -- `1`/`0` to enable/disable, or an
 * index for features with multiple alternates. Defaults to `1` (enable)
 * when omitted. Mirrors CanvasKit's `TextFontFeatures`.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface TextFontFeatures {
  /** Four-character OpenType feature tag, such as `"smcp"` or `"ss01"`. */
  name: string;
  /**
   * Feature selector: `1` to enable, `0` to disable, or an index where the
   * feature offers alternates.
   */
  value?: number;
}

/** 🧪 Not in the HTML Canvas standard. */
export interface TextStyleInput {
  /** Type size in pixels. */
  fontSize?: number;
  /** Families to match, in preference order, as `ctx.font` lists them. */
  fontFamilies?: string[];
  /** Fill color for the glyphs. Defaults to black. */
  color?: TextColorInput;
  /**
   * Fill color for the glyphs, taking precedence over `color` where both are
   * given. The two are separate in CanvasKit and kept separate here.
   */
  foregroundColor?: TextColorInput;
  /** Color painted behind the run's glyphs. */
  backgroundColor?: TextColorInput;
  /**
   * Face selection within the families: CSS numeric `weight` (400 normal,
   * 700 bold), CSS numeric `width` (1 ultra-condensed through 9
   * ultra-expanded), and `slant` as `0` upright, `1` italic, `2` oblique.
   * Each defaults to normal when left out.
   */
  fontStyle?: { weight?: number; width?: number; slant?: number };
  /** Extra space added after each glyph, in pixels. */
  letterSpacing?: number;
  /** Extra space added at each word boundary, in pixels. */
  wordSpacing?: number;
  /**
   * Line height as a multiple of `fontSize`, replacing the font's own
   * metrics. Setting it at all turns on the override.
   */
  heightMultiplier?: number;
  /** Which lines to draw. Combine with `|`: `Underline | LineThrough`. */
  decoration?: TextDecorationMask;
  /** How those lines are drawn. Anything outside the set draws as `Solid`. */
  decorationStyle?: TextDecorationStyleValue;
  /** Color of the decoration lines. Defaults to the text color. */
  decorationColor?: TextColorInput;
  /** Line thickness as a multiple of the font's own decoration thickness. */
  decorationThickness?: number;
  /**
   * Shadows painted under the run, in the order given -- several are
   * allowed, unlike the context's single `shadowColor`.
   */
  shadows?: TextShadowInput[];
  /**
   * Explicit variable-font axis positions. When set, the paragraph
   * engine instantiates the matched typeface at these axis values
   * instead of relying on the nominal weight match -- match CanvasKit's
   * behaviour where the variable axes are honoured precisely.
   *
   * Each entry must carry a 4-character ASCII `axis` tag; values are
   * clamped to the typeface's declared min/max for that axis. Entries
   * referring to axes the typeface doesn't expose are silently dropped.
   */
  fontVariations?: FontVariationInput[];
  /**
   * OpenType features applied to the run: small caps (`smcp`/`c2sc`),
   * ligatures (`liga`/`dlig`), oldstyle/tabular/proportional figures
   * (`onum`/`lnum`/`tnum`/`pnum`), slashed zero (`zero`), stylistic
   * sets (`ss01`...`ss20`), and so on. The Canvas2D `fontVariant` path
   * exposes these too; this is the rich-text/paragraph equivalent.
   */
  fontFeatures?: TextFontFeatures[];
  /**
   * Distribute the run's leading half above and half below the text,
   * centring it within the line box. Mirrors CanvasKit's
   * `TextStyle.halfLeading`.
   */
  halfLeading?: boolean;
}

/**
 * A fixed line box independent of the per-run fonts, for deterministic
 * leading (captions, subtitles, vertically-aligned blocks). Mirrors
 * CanvasKit's `StrutStyle`. Presence on a paragraph style enables the
 * strut unless `enabled` is explicitly `false`.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface StrutStyleInput {
  /** Whether the strut takes effect at all. */
  enabled?: boolean;
  /** Families whose metrics define the strut, in preference order. */
  fontFamilies?: string[];
  /** Type size the strut's metrics are computed at, in pixels. */
  fontSize?: number;
  /** Line-height multiplier for the strut line box. */
  heightMultiplier?: number;
  /** Extra leading as a multiple of the strut font size. */
  leading?: number;
  /** Clamp every line to the strut height (vs. treat it as a minimum). */
  forceStrutHeight?: boolean;
  /** Distribute leading half above / half below the text. */
  halfLeading?: boolean;
}

/** 🧪 Not in the HTML Canvas standard. */
export interface ParagraphStyleInput {
  /**
   * Matched case-insensitively. An unrecognised value is ignored and the
   * default alignment stands, so a typo fails silently.
   */
  textAlign?:
    "left" | "right" | "center" | "justify" | "start" | "end" | (string & {});

  /**
   * Base direction for the paragraph. Matched case-insensitively; an
   * unrecognised value is ignored.
   */
  textDirection?: "ltr" | "rtl" | (string & {});
  /**
   * Cap on the number of lines. Anything past it is dropped, which
   * {@link Paragraph.didExceedMaxLines} then reports.
   */
  maxLines?: number;
  /** String appended to the last line when `maxLines` truncated the text. */
  ellipsis?: string;
  /**
   * The style text starts in, before any {@link ParagraphBuilder.pushStyle}.
   * Font variations are read from here and nowhere else.
   */
  textStyle?: TextStyleInput;
  /** Fixed line box for deterministic leading. */
  strutStyle?: StrutStyleInput;
  /**
   * First/last-line leading trim: `0` All, `1` DisableFirstAscent,
   * `2` DisableLastDescent, `3` DisableAll. Mirrors CanvasKit's
   * `TextHeightBehavior`.
   */
  textHeightBehavior?: number;
}

/**
 * A text position, as returned by
 * {@link Paragraph.getGlyphPositionAtCoordinate}.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface GlyphPosition {
  /** Offset into the paragraph's text, in UTF-16 code units. */
  pos: number;
  /**
   * Which side of `pos` the query point fell on: `0` upstream (the end of
   * the preceding glyph), `1` downstream (the start of the following one).
   * It is what decides where a caret sits at a line wrap.
   */
  affinity: number;
}

/**
 * One rectangle covering a run of text, from
 * {@link Paragraph.getRectsForRange} or
 * {@link Paragraph.getRectsForPlaceholders}.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface TextBox {
  /**
   * `[left, top, right, bottom]` relative to the paragraph's top-left
   * corner -- edges, not the `[x, y, width, height]` that the filter
   * classes take.
   */
  rect: [number, number, number, number];
  /** Direction of the run: `0` right-to-left, `1` left-to-right. */
  direction: number;
}

/**
 * Measurements for one laid-out line, from {@link Paragraph.getLineMetrics}.
 *
 * All offsets index the paragraph's text in UTF-16 code units; all distances
 * are in pixels.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface LineMetrics {
  /** Offset of the line's first character. */
  startIndex: number;
  /** Offset just past the line's last character. */
  endIndex: number;
  /** As `endIndex`, with trailing whitespace trimmed. */
  endExcludingWhitespaces: number;
  /** As `endIndex`, including the line break that ended the line, if any. */
  endIncludingNewline: number;
  /** Whether the line ended at a newline rather than at the wrap. */
  isHardBreak: boolean;
  /** Distance from this line's baseline up to its top edge. */
  ascent: number;
  /** Distance from this line's baseline down to its bottom edge. */
  descent: number;
  /** Total height of the line box. */
  height: number;
  /** Width of the text on this line. */
  width: number;
  /** Left edge of the line, which is where alignment shows up. */
  left: number;
  /** Distance from the top of the **paragraph** down to this line's baseline. */
  baseline: number;
  /** Zero-based index of the line. */
  lineNumber: number;
}

/** 🧪 Not in the HTML Canvas standard. */
export class ParagraphBuilder {
  /**
   * Create a builder for laying out styled text.
   *
   * Text is shaped with the process-global font library. CanvasKit takes a
   * `FontMgr` here; this build has no per-builder equivalent, so the parameter
   * is omitted rather than accepted and ignored.
   *
   * ```ts
   * const para = new ParagraphBuilder({ textStyle: { fontSize: 16 } })
   *   .addText("hello")
   *   .build()
   * ```
   */
  constructor(style?: ParagraphStyleInput);
  /**
   * Text is shaped with the process-global font library. CanvasKit takes a
   * `FontMgr` here; this build has no per-builder equivalent, so the parameter
   * is omitted rather than accepted and ignored.
   */
  static Make(style?: ParagraphStyleInput): ParagraphBuilder;
  /**
   * Begin a run in `style`, stacked on top of whatever is already pushed.
   * Text added from here on uses it until the matching {@link pop}.
   *
   * Font variations are the exception: the paragraph's font collection is
   * fixed when the builder is created, so an axis position pushed here has
   * no effect. Set those on the constructor's `textStyle`.
   */
  pushStyle(style: TextStyleInput): this;
  /**
   * End the innermost {@link pushStyle}, returning to the style beneath it.
   * Popping with nothing pushed is harmless.
   */
  pop(): this;
  /** Append text in the current style. */
  addText(text: string): this;
  /**
   * Reserve a rectangle in the text flow for something drawn separately.
   *
   * `align` and `baseline` were accepted and discarded until 4.2.0, so every
   * placeholder laid out on the baseline whatever was passed. A value outside
   * either set now throws rather than silently reverting to the default.
   *
   * @param align - see {@link PlaceholderAlignment}
   * @param baseline - see {@link TextBaseline}; consulted by the three
   *   baseline-relative alignments -- `Baseline`, `AboveBaseline` and
   *   `BelowBaseline` -- and ignored by `Top`, `Bottom` and `Middle`
   * @param offset - distance from the placeholder's top edge to its baseline
   */
  addPlaceholder(
    width: number,
    height: number,
    align?: PlaceholderAlignmentValue,
    baseline?: TextBaselineValue,
    offset?: number,
  ): this;
  /**
   * Finishes the paragraph. Call {@link Paragraph.layout} on the result
   * before measuring or drawing it.
   */
  build(): Paragraph;
}

/**
 * A shaped block of text, built by {@link ParagraphBuilder} and painted with
 * {@link CanvasRenderingContext2D.drawParagraph}.
 *
 * Nothing here reports anything useful until {@link Paragraph.layout} has run.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class Paragraph {
  /**
   * A laid-out paragraph comes from {@link ParagraphBuilder.build}, which is
   * where its text and styles are assembled. There is no set of arguments that
   * describes one: a builder can carry several styled runs, so a constructor
   * taking text and a single style could not express what it produces.
   */
  private constructor();
  /**
   * Breaks the text into lines at `width` pixels.
   *
   * Required before drawing or measuring: every getter below, and
   * {@link CanvasRenderingContext2D.drawParagraph}, depends on it. Drawing an
   * un-laid-out paragraph is a silent no-op. Safe to call again with a
   * different width to re-wrap.
   */
  layout(width: number): void;
  /** Total height of the laid-out lines, in pixels. */
  getHeight(): number;
  /**
   * Width of the widest line actually produced -- the tight bounding box,
   * normally narrower than the width {@link layout} was given.
   */
  getLongestLine(): number;
  /** The width passed to {@link layout}, not the width the text came out at. */
  getMaxWidth(): number;
  /**
   * The width that would fit every line without wrapping. Laying out any
   * wider changes nothing.
   */
  getMaxIntrinsicWidth(): number;
  /**
   * The width of the widest unbreakable word. Laying out any narrower
   * changes nothing, since there is nowhere left to break.
   */
  getMinIntrinsicWidth(): number;
  /**
   * Distance from the top of the paragraph down to the first line's
   * alphabetic baseline -- the line Latin glyphs rest on.
   */
  getAlphabeticBaseline(): number;
  /**
   * Distance from the top of the paragraph down to the first line's
   * ideographic baseline, which sits below the alphabetic one.
   */
  getIdeographicBaseline(): number;
  /**
   * The text position nearest `(x, y)`, in coordinates relative to the
   * paragraph's top-left corner -- what a click maps to when placing a
   * caret. `affinity` says which side of the offset the point fell on.
   */
  getGlyphPositionAtCoordinate(x: number, y: number): GlyphPosition;
  /**
   * The boxes covering text positions `start` up to `end`, as a selection
   * highlight would draw them. One box per line the range touches, or per
   * direction run within a line in bidirectional text.
   *
   * `hStyle` chooses how tall each box is: `0` tight to the glyphs (the
   * default), `1` the full line height, `2`, `3` and `4` distributing line
   * spacing to the middle, top and bottom, and `5` the strut's height.
   * `wStyle` is `0` for tight or `1` to stretch the last box of a line to
   * the layout width.
   */
  getRectsForRange(
    start: number,
    end: number,
    hStyle?: number,
    wStyle?: number,
  ): TextBox[];
  /** One {@link LineMetrics} entry per laid-out line, in order. */
  getLineMetrics(): LineMetrics[];
  /**
   * 🧪 Not in CanvasKit.
   *
   * The distance from the paragraph's top edge to the first line's
   * baseline, which is what to add to a y coordinate to place text by its
   * baseline rather than by its top.
   *
   * The same number as `getLineMetrics()[0].ascent`, and `0` for an empty
   * paragraph, which has no first line to measure.
   */
  getFirstLineAscent(): number;
  /** Whether layout dropped content past the style's `maxLines`. */
  didExceedMaxLines(): boolean;
  /** Number of laid-out lines. */
  getNumberOfLines(): number;
  /**
   * Bounding boxes of the inline placeholders, in insertion order --
   * the readback counterpart to `ParagraphBuilder.addPlaceholder`.
   */
  getRectsForPlaceholders(): TextBox[];
  /**
   * Codepoints no font in the collection could resolve (tofu / missing
   * glyphs), for validating automated multi-language renders.
   */
  getUnresolvedCodepoints(): number[];
}

//
// Window & App
//

import { EventEmitter } from "stream";
/**
 * Which loop drives the windows.
 *
 * `"node"` runs the UI on Node's own event loop, so timers, promises and I/O
 * keep working while windows are open. `"native"` hands the thread to the
 * platform's loop instead, which is smoother under heavy drawing but starves
 * anything else the script was doing.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type EventLoopMode = "node" | "native";
/**
 * What an `input` event did to the text, using the names the DOM
 * `InputEvent.inputType` uses.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type TextInputType =
  | "insertText"
  | "deleteContentBackward"
  | "deleteContentForward"
  | "insertLineBreak"
  | "insertCompositionText";
/**
 * How a canvas is scaled into a window whose size or aspect it does not
 * match. The canvas is centred under every mode but `"resize"`, and the
 * uncovered area is painted with {@link Window.background}.
 *
 * - `"contain"` -- the default: scale until one axis fits, keeping the aspect
 * - `"contain-x"` / `"contain-y"` -- scale to fit that axis alone, letting
 *   the other overflow or fall short
 * - `"cover"` -- scale until both axes are covered, cropping the overflow
 * - `"scale-down"` -- as `"contain"`, but never scaling above 1:1
 * - `"fill"` -- stretch both axes independently, distorting the aspect
 * - `"none"` -- draw at 1:1
 * - `"resize"` -- do not scale; resize the canvas itself to the window
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type FitStyle =
  | "none"
  | "contain-x"
  | "contain-y"
  | "contain"
  | "cover"
  | "fill"
  | "scale-down"
  | "resize";
export type CursorStyle =
  | "default"
  | "crosshair"
  | "hand"
  | "arrow"
  | "move"
  | "text"
  | "wait"
  | "help"
  | "progress"
  | "not-allowed"
  | "context-menu"
  | "cell"
  | "vertical-text"
  | "alias"
  | "copy"
  | "no-drop"
  | "grab"
  | "grabbing"
  | "all-scroll"
  | "zoom-in"
  | "zoom-out"
  | "e-resize"
  | "n-resize"
  | "ne-resize"
  | "nw-resize"
  | "s-resize"
  | "se-resize"
  | "sw-resize"
  | "w-resize"
  | "ew-resize"
  | "ns-resize"
  | "nesw-resize"
  | "nwse-resize"
  | "col-resize"
  | "row-resize"
  | "none";

/**
 * Initial state for a {@link Window}. Every field is also a property on the
 * window itself and can be changed after it opens.
 *
 * The {@link TextOptions} it extends are the canvas settings used when the
 * window creates a canvas of its own; they are ignored when `canvas` hands
 * it one that already exists.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export type WindowOptions = {
  /** Title-bar text. */
  title?: string;
  /** Position of the window's left edge on screen, in points. */
  left?: number;
  /** Position of the window's top edge on screen, in points. */
  top?: number;
  /** Width of the window, in points, defaulting to the canvas's. */
  width?: number;
  /** Height of the window, in points, defaulting to the canvas's. */
  height?: number;
  /** How the canvas is scaled into the window. Defaults to `"contain"`. */
  fit?: FitStyle;
  /** Which canvas page to display, numbered from `1`. */
  page?: number;
  /** Color drawn where the canvas does not cover the window. */
  background?: string;
  /** Open occupying the whole screen. */
  fullscreen?: boolean;
  /** Open without a title bar or frame. */
  borderless?: boolean;
  /** Whether the user may resize the window. */
  resizable?: boolean;
  /** Open hidden, to be shown later by setting `visible`. */
  visible?: boolean;
  /** Pointer shape over the window. */
  cursor?: CursorStyle;
  /** An existing canvas to display, instead of creating one. */
  canvas?: Canvas;
} & TextOptions;

/**
 * Payload shared by the mouse events, following the DOM's names.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
type MouseEventProps = {
  /** Cursor position in **canvas** coordinates, with the window's fit undone. */
  x: number;
  /** Cursor position in canvas coordinates, vertically. */
  y: number;
  /**
   * Cursor position in untransformed **window** coordinates, which differs
   * from `x` whenever the canvas is being scaled to fit.
   */
  pageX: number;
  /** Cursor position in window coordinates, vertically. */
  pageY: number;
  /** Which button changed state: `0` left, `1` middle, `2` right. */
  button: number;
  /** Bitmask of the buttons currently held. */
  buttons: number;
  /** Whether Control was held. */
  ctrlKey: boolean;
  /** Whether Alt / Option was held. */
  altKey: boolean;
  /** Whether Command / Windows was held. */
  metaKey: boolean;
  /** Whether Shift was held. */
  shiftKey: boolean;
};

/**
 * Payload shared by `keydown` and `keyup`, following the DOM's names.
 *
 * The handler is also passed a `preventDefault()` which suppresses the
 * built-in shortcuts -- Command-W to close, Command-F to toggle fullscreen.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
type KeyboardEventProps = {
  /** The character or named key produced, e.g. `"a"` or `"ArrowLeft"`. */
  key: string;
  /** The physical key, independent of layout, e.g. `"KeyA"`. */
  code: string;
  /** Which of a duplicated key it was: `0` standard, `1` left, `2` right, `3` numpad. */
  location: number;
  /** Whether this is an auto-repeat while the key is held. */
  repeat: boolean;
  /** Whether Control was held. */
  ctrlKey: boolean;
  /** Whether Alt / Option was held. */
  altKey: boolean;
  /** Whether Command / Windows was held. */
  metaKey: boolean;
  /** Whether Shift was held. */
  shiftKey: boolean;
};

/**
 * Every event a {@link Window} emits, and what each one carries. The object
 * a handler receives also has `type` and `target` alongside these fields.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
type WindowEvents = {
  /** A mouse button went down. */
  mousedown: MouseEventProps;
  /** A mouse button came up. */
  mouseup: MouseEventProps;
  /** The cursor moved over the window. */
  mousemove: MouseEventProps;
  /** A key went down, auto-repeats included. */
  keydown: KeyboardEventProps;
  /** A key came up. */
  keyup: KeyboardEventProps;
  /**
   * Text was entered, by keystroke or by input method. `data` is what was
   * inserted, and `inputType` what kind of edit it was.
   */
  input: {
    data: string;
    inputType: TextInputType;
  };
  /** The scroll wheel or trackpad moved, in pixels. */
  wheel: { deltaX: number; deltaY: number };
  /** The window entered or left fullscreen. */
  fullscreen: { enabled: boolean };
  /** The window was moved, reporting its new screen position. */
  move: { left: number; top: number };
  /** The window was resized, reporting its new size in points. */
  resize: { height: number; width: number };
  /**
   * A new frame is due, numbered from `0`. This is where to draw an
   * animation: the canvas is presented after the handler returns.
   */
  frame: { frame: number };
  /**
   * As `frame`, but the context is reset first, so each handler starts from
   * a blank canvas with default state. Listening for this at all is what
   * turns the clearing on; `frame` alone leaves the previous frame in place
   * to be drawn over.
   */
  draw: { frame: number };
  /** The window lost keyboard focus. */
  blur: {};
  /** The window gained keyboard focus. */
  focus: {};
  /**
   * Emitted once, immediately before the first `frame` event, for one-time
   * initialization.
   */
  setup: {};
  /** The window has closed. */
  close: {};
};

/**
 * An on-screen window displaying a {@link Canvas}, with mouse and keyboard
 * events delivered as they arrive.
 *
 * A window draws its canvas's current page every frame, so animating means
 * redrawing in a `frame` handler rather than pushing images anywhere. Opening
 * one schedules {@link App.launch} on the next tick, so a script that creates
 * a window keeps running until every window is closed.
 *
 * ```ts
 * const win = new Window(400, 300, { title: "hello" })
 * win.on("frame", ({ frame }) => {
 *   let { ctx } = win
 *   ctx.fillStyle = "skyblue"
 *   ctx.fillRect(0, 0, 100 + frame, 100)
 * })
 * ```
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export class Window extends EventEmitter<{
  [EventName in keyof WindowEvents]: [
    {
      target: Window;
      type: EventName;
    } & WindowEvents[EventName],
  ];
}> {
  constructor(width: number, height: number, options?: WindowOptions);
  constructor(options?: WindowOptions);

  /** The drawing context of the canvas page this window is showing. */
  readonly ctx: CanvasRenderingContext2D;
  /**
   * The canvas being displayed. Assigning another one swaps what the window
   * shows without reopening it.
   */
  canvas: Canvas;
  /** Whether the window is mapped on screen. Set `false` to hide it. */
  visible: boolean;
  /** Whether the window occupies the whole screen. */
  fullscreen: boolean;
  /** Whether the title bar and frame are hidden. */
  borderless: boolean;
  /** Whether the user may resize the window. */
  resizable: boolean;
  /** Title-bar text. Assigning `null` clears it rather than printing "null". */
  title: string;
  /**
   * Pointer shape over the window, using the CSS `cursor` keywords. An
   * unrecognized name is ignored and the current cursor kept.
   */
  cursor: CursorStyle;
  /**
   * How the canvas is scaled into the window when the two disagree in size
   * or aspect -- see {@link FitStyle}. An unrecognized name is ignored.
   */
  fit: FitStyle;
  /** Position of the window's left edge on screen, in points. */
  left: number;
  /** Position of the window's top edge on screen, in points. */
  top: number;
  /** Width of the window, in points. Non-finite assignments are ignored. */
  width: number;
  /** Height of the window, in points. Non-finite assignments are ignored. */
  height: number;
  /**
   * Which page of the canvas is on display, numbered from `1`. A negative
   * number counts from the end, and the window resizes its canvas to that
   * page's dimensions. Assigning a page that does not exist is ignored.
   */
  page: number;
  /**
   * Color drawn behind the canvas, filling any part of the window the
   * canvas does not cover under the current {@link Window.fit}.
   */
  background: string;
  /** Whether {@link Window.close} has run. */
  readonly closed: boolean;

  /**
   * Queue the window for display. Nothing appears until the event loop
   * starts, which {@link App.launch} does -- scheduled automatically on the
   * next tick, so this is usually all a script has to call.
   */
  open(): void;
  /**
   * Close the window and emit its `close` event. When the last window
   * closes, the event loop ends and {@link App.launch}'s promise resolves.
   */
  close(): void;
}

/**
 * The process-wide window manager: the event loop every {@link Window} runs
 * on, and the frame clock that drives their `frame` events.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export interface App extends EventEmitter<{
  idle: [{ type: "idle"; target: App }];
}> {
  /** Every window that is currently open, in the order they opened. */
  readonly windows: Window[];
  /**
   * Whether {@link App.launch} has been called. It stays `true` once the
   * loop has started, including after the last window closes, since
   * {@link App.eventLoop} can no longer be changed either way.
   */
  readonly running: boolean;
  /**
   * Which loop drives the windows -- see {@link EventLoopMode}. It can only
   * be set before the loop starts; assigning afterwards throws.
   */
  eventLoop: EventLoopMode;
  /**
   * Target frames per second for every window, defaulting to 60. Values
   * below `1` are ignored.
   */
  fps: number;

  /**
   * Start the event loop, resolving once the last window has closed.
   *
   * Opening a window schedules this on the next tick, so calling it is only
   * necessary to hold the promise -- calling it twice returns the same one.
   */
  launch(): Promise<undefined>;
  /** Close every window and end the event loop. */
  quit(): void;
}

/**
 * The process-wide window manager.
 *
 * 🧪 Not in the HTML Canvas standard.
 */
export const App: App;
