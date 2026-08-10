// Type-only, and `sharp` is an optional peer: the runtime never requires it.
// A consumer who wants these signatures to resolve has to install it, which
// is what the optional peer declaration asks for.
import type { Sharp } from "sharp";

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

declare var DOMPointReadOnly: {
  prototype: DOMPointReadOnly;
  new (x?: number, y?: number, z?: number, w?: number): DOMPointReadOnly;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMPointReadOnly/fromPoint_static) */
  fromPoint(other?: DOMPointInit): DOMPointReadOnly;
};

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

interface DOMRectList {
  readonly length: number;
  item(index: number): DOMRect | null;
  [index: number]: DOMRect;
}

declare var DOMRectList: {
  prototype: DOMRectList;
  new (): DOMRectList;
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

declare var DOMRectReadOnly: {
  prototype: DOMRectReadOnly;
  new (
    x?: number,
    y?: number,
    width?: number,
    height?: number,
  ): DOMRectReadOnly;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/DOMRectReadOnly/fromRect_static) */
  fromRect(other?: DOMRectInit): DOMRectReadOnly;
};

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

// Color spaces for rendering surfaces
export type ColorSpace =
  | "srgb" // Standard sRGB with gamma (default)
  | "srgb-linear"
  | "linear" // Linear sRGB for HDR compositing
  | "display-p3"
  | "p3" // Display P3 (wide gamut, Apple devices)
  | "display-p3-linear"
  | "p3-linear" // Display P3 with linear transfer
  | "rec2020"
  | "bt2020" // Rec. 2020 (wide gamut for UHD)
  | "rec2020-linear"
  | "bt2020-linear" // Rec. 2020 with linear transfer
  | "rec2020-pq"
  | "hdr10" // Rec. 2020 + PQ transfer (HDR10)
  | "rec2020-hlg"
  | "hlg"; // Rec. 2020 + HLG transfer (broadcast HDR)
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
  | "RGBA8888"
  | "SRGBA8888" // 4 bytes/px
  | "R16G16B16A16UNorm"
  | "RGBAF16"
  | "RGBAF16Norm" // 8 bytes/px
  | "RGBAF32"; // 16 bytes/px

interface ImageDataSettings {
  /**
   * Only `"srgb"` is accepted; the `ImageData` constructor throws on anything
   * else. The wider {@link ColorSpace} union applies to the `Canvas`
   * constructor and to export options, which do honour it.
   */
  colorSpace?: "srgb";
  colorType?: ColorType;
}

interface ImageDataExportSettings {
  /** Background color to draw beneath transparent parts of the canvas */
  matte?: string;

  /** Number of pixels per grid ‘point’ (defaults to 1) */
  density?: number;

  /** Number of samples used for antialising each pixel */
  msaa?: number | boolean;

  /** Color space (must be "srgb") */
  colorSpace?: ColorSpace;

  /** Color type to use when exporting in "raw" format */
  colorType?: ColorType;
}

export class ImageData {
  prototype: ImageData;
  constructor(sw: number, sh: number, settings?: ImageDataSettings);
  constructor(
    data: Uint8ClampedArray | Buffer,
    sw: number,
    sh?: number,
    settings?: ImageDataSettings,
  );
  constructor(image: Image, settings?: ImageDataSettings);
  constructor(imageData: ImageData);

  readonly colorSpace: ColorSpace;
  readonly colorType: ColorType;
  readonly data: Uint8ClampedArray;
  readonly height: number;
  readonly width: number;
  toSharp(): Sharp;
}

export class Image extends EventEmitter {
  constructor(data?: Buffer | URL | string, src?: string);
  get src(): string;
  set src(src: string | URL | Buffer | Sharp);
  get width(): number;
  get height(): number;
  onload: ((this: Image, image: Image) => any) | null;
  onerror: ((this: Image, error: Error) => any) | null;
  complete: boolean;
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
  a: number;
  b: number;
  c: number;
  d: number;
  e: number;
  f: number;
  m11: number;
  m12: number;
  m13: number;
  m14: number;
  m21: number;
  m22: number;
  m23: number;
  m24: number;
  m31: number;
  m32: number;
  m33: number;
  m34: number;
  m41: number;
  m42: number;
  m43: number;
  m44: number;

  flipX(): DOMMatrix;
  flipY(): DOMMatrix;
  inverse(): DOMMatrix;
  invertSelf(): DOMMatrix;

  multiply(other?: DOMMatrixInit): DOMMatrix;
  multiplySelf(other?: DOMMatrixInit): DOMMatrix;
  preMultiplySelf(other?: DOMMatrixInit): DOMMatrix;

  rotate(rotX?: number, rotY?: number, rotZ?: number): DOMMatrix;
  rotateSelf(rotX?: number, rotY?: number, rotZ?: number): DOMMatrix;
  rotateAxisAngle(
    x?: number,
    y?: number,
    z?: number,
    angle?: number,
  ): DOMMatrix;
  rotateAxisAngleSelf(
    x?: number,
    y?: number,
    z?: number,
    angle?: number,
  ): DOMMatrix;
  rotateFromVector(x?: number, y?: number): DOMMatrix;
  rotateFromVectorSelf(x?: number, y?: number): DOMMatrix;

  scale(
    scaleX?: number,
    scaleY?: number,
    scaleZ?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  scaleSelf(
    scaleX?: number,
    scaleY?: number,
    scaleZ?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  scale3d(
    scale?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;
  scale3dSelf(
    scale?: number,
    originX?: number,
    originY?: number,
    originZ?: number,
  ): DOMMatrix;

  skew(sx?: number, sy?: number): DOMMatrix;
  skewSelf(sx?: number, sy?: number): DOMMatrix;
  skewX(sx?: number): DOMMatrix;
  skewXSelf(sx?: number): DOMMatrix;
  skewY(sy?: number): DOMMatrix;
  skewYSelf(sy?: number): DOMMatrix;

  translate(tx?: number, ty?: number, tz?: number): DOMMatrix;
  translateSelf(tx?: number, ty?: number, tz?: number): DOMMatrix;

  setMatrixValue(transformList: string): DOMMatrix;
  transformPoint(point?: DOMPointInit): DOMPoint;

  toFloat32Array(): Float32Array;
  toFloat64Array(): Float64Array;
  toJSON(): any;
  toString(): string;
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
  "png" | "jpg" | "jpeg" | "webp" | "raw" | "pdf" | "svg";
export type FontOptions = "outline" | "device-independent";

export interface RenderOptions {
  /** Page to export: Defaults to 1 (i.e., first page) */
  page?: number;

  /** Background color to draw beneath transparent parts of the canvas */
  matte?: string;

  /** Number of pixels per grid ‘point’ (defaults to 1) */
  density?: number;

  /** Number of samples used for antialising each pixel */
  msaa?: number | boolean;
}

export interface ExportOptions extends RenderOptions {
  /** Quality for lossy encodings like JPEG & WEBP (0.0–1.0) */
  quality?: number;

  /** Optionally convert text to bézier paths (SVG only) */
  outline?: boolean;

  /** Optionally use 4:2:0 chroma subsampling (JPEG only) */
  downsample?: boolean;

  /** Color type to use when exporting in "raw" format */
  colorType?: ColorType;

  /** Color space for the output image (defaults to "srgb") */
  colorSpace?: ColorSpace;
}

export interface SaveOptions extends ExportOptions {
  /** Image format to use (either as a file extension or a mime-type string) */
  format?: ExportFormat;
}

export interface EngineDetails {
  renderer: "CPU" | "GPU";
  api: "Vulkan" | "Metal";
  device: string;
  driver?: string;
  threads: number;
  error?: string;
}

export interface BackendInfo {
  /** Whether GPU or CPU renderer is being used. */
  renderer: "CPU" | "GPU";
  /** Graphics API used (Vulkan, Metal, or null for CPU). */
  api: "Vulkan" | "Metal" | null;
  /** Description of the rendering device. */
  device: string;
  /** Driver version (GPU only). */
  driver?: string;
  /** Number of CPU threads available for rendering. */
  threads: number;
  /** Whether GPU rendering is available. */
  gpuAvailable: boolean;
  /** Error message if GPU initialization failed. */
  error?: string;
}

/**
 * Get backend information without creating a canvas.
 * Useful for determining optimal color type (F16 for GPU, F32 for CPU).
 */
export function backend(): BackendInfo;

export interface TextOptions {
  /** Amount of additional contrast to add when rendering text (defaults to 0) */
  textContrast?: number;

  /** Gamma value for blending the edges of letterforms (defaults to 1.4) */
  textGamma?: number;

  /** Surface pixel format for high-precision/HDR rendering (defaults to "rgba") */
  colorType?: ColorType;

  /** Color space for rendering (defaults to "srgb", use "srgb-linear" for HDR workflows) */
  colorSpace?: ColorSpace;
}

/** [Skia Canvas Docs](https://skia-canvas.org/api/canvas) */
export class Canvas {
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

  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#creating-new-canvas-objects) */
  constructor(width?: number, height?: number, options?: TextOptions);

  /**
   * Returns an object that provides methods and properties for drawing and manipulating images and graphics on a canvas element in a document. A context object includes information about colors, line widths, fonts, and other graphic parameters that can be drawn on a canvas.
   * @param type The type of canvas to create. Skia Canvas only supports a 2-D context using canvas.getContext("2d")
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/HTMLCanvasElement/getContext)
   */
  getContext(type?: "2d"): CanvasRenderingContext2D;
  newPage(width?: number, height?: number): CanvasRenderingContext2D;
  readonly pages: CanvasRenderingContext2D[];

  get gpu(): boolean;
  set gpu(enabled: boolean);
  readonly engine: EngineDetails;

  /**
   * The pixel format this canvas was constructed with (`"rgba"` by default).
   * Exports and `getImageData` inherit it unless the call names its own.
   */
  readonly colorType: ColorType;

  /** @deprecated Use {@link Canvas.toFile()} instead */
  saveAs(filename: string, options?: SaveOptions): Promise<void>;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tofile): toFile() */
  toFile(filename: string, options?: SaveOptions): Promise<void>;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tobuffer) */
  toBuffer(format: ExportFormat, options?: ExportOptions): Promise<Buffer>;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tourl) */
  toURL(format: ExportFormat, options?: ExportOptions): Promise<string>;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tosharp) */
  toSharp(options?: RenderOptions): Sharp;

  /** @deprecated Use {@link Canvas.toFileSync()} instead */
  saveAsSync(filename: string, options?: SaveOptions): void;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tobuffer) */
  toBufferSync(format: ExportFormat, options?: ExportOptions): Buffer;
  /** @deprecated {@link Canvas.toDataURL()} is now synchronous; use it instead */
  toDataURLSync(format: ExportFormat, options?: ExportOptions): string;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tourl) */
  toURLSync(format: ExportFormat, options?: ExportOptions): string;
  /** [Skia Canvas Docs](https://skia-canvas.org/api/canvas#tosharp) */
  toSharpSync(options?: RenderOptions): Sharp;

  /** [MDN Reference](https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/toDataURL) */
  toDataURL(format: ExportFormat, quality?: number): string;

  get raw(): Promise<Buffer>;
  get pdf(): Promise<Buffer>;
  get svg(): Promise<Buffer>;
  get jpg(): Promise<Buffer>;
  get png(): Promise<Buffer>;
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
   * Adds a color stop with the given color to the gradient at the given offset. 0.0 is the offset at one end of the gradient, 1.0 is the offset at the other end.
   *
   * Throws an "IndexSizeError" DOMException if the offset is out of range. Throws a "SyntaxError" DOMException if the color cannot be parsed.
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasGradient/addColorStop)
   */
  addColorStop(offset: number, color: Color4fInput): void;

  /** Color space for gradient interpolation. Default: "srgb" */
  interpolation: GradientColorSpace;

  /** Hue direction for cylindrical spaces (oklch, lch, hsl, hwb). Default: "shorter" */
  hueInterpolation: HueInterpolation;
}

declare var CanvasGradient: {
  prototype: CanvasGradient;
  new (): CanvasGradient;
};

export class CanvasTexture {}

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

/** Sampling mode for image transformations */
export type SamplingMode = "nearest" | "linear";

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
 */
export class ColorFilter {
  private constructor();

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
 */
export class ImageFilter {
  private constructor();

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
   */
  static MakeDilate(
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Create morphological erosion ImageFilter.
   * @param radiusX - horizontal radius
   * @param radiusY - vertical radius
   * @param input - optional input filter for chaining
   */
  static MakeErode(
    radiusX: number,
    radiusY: number,
    input?: ImageFilter | null,
  ): ImageFilter | null;

  /**
   * Merge multiple ImageFilters into one.
   * @param filters - array of filters to merge (null entries allowed)
   */
  static MakeMerge(filters: (ImageFilter | null)[]): ImageFilter | null;

  /**
   * Create empty (no-op) ImageFilter.
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
  ): ImageFilter | null;

  /**
   * Apply a matrix transformation to the image.
   * @param matrix - 6 elements (2D affine) or 9 elements (3x3)
   * @param sampling - sampling mode ("nearest" or "linear", default "linear")
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
   * @param falloffExponent - falloff exponent
   * @param cutoffAngle - cutoff angle in degrees
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
   * @param falloffExponent - falloff exponent
   * @param cutoffAngle - cutoff angle in degrees
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
 */
export class MaskFilter {
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
 */
export class Shader {
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
  /** Add a per-channel offset in place; returns the same matrix. */
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
type CanvasPatternSource = Canvas | Image;
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
// type CanvasFontVariantCaps = "all-petite-caps" | "all-small-caps" | "normal" | "petite-caps" | "small-caps" | "titling-caps" | "unicase";
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
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/createConicGradient) */
  createConicGradient(startAngle: number, x: number, y: number): CanvasGradient;
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

  /** [Skia Canvas Docs](https://skia-canvas.org/api/context#createtexture) */
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
   * `[x, y, w, h]` hint; `backdrop` applies an ImageFilter to the
   * content behind the layer (blur-behind / frosted glass). Not part of
   * the HTML Canvas standard.
   */
  saveLayer(
    alpha?: number,
    bounds?: [number, number, number, number] | null,
    backdrop?: ImageFilter | null,
  ): void;

  // UNIMPLEMENTED
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/isContextLost) */
  // isContextLost(): boolean;
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

  // UNIMPLEMENTED
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/textRendering) */
  // textRendering: CanvasTextRendering;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fontKerning) */
  // fontKerning: CanvasFontKerning;
  /** [MDN Reference](https://developer.mozilla.org/docs/Web/API/CanvasRenderingContext2D/fontVariantCaps) */
  // fontVariantCaps: CanvasFontVariantCaps;
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
 * - [Skia Canvas Docs](https://skia-canvas.org/api/context)
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
  fontVariant: FontVariantSetting;
  fontVariationSettings: string;
  fontHinting: boolean;
  textWrap: boolean;
  textDecoration: string;
  lineDashMarker: Path2D | null;
  lineDashFit: "move" | "turn" | "follow";

  // skia/chrome beziers & convenience methods
  get currentTransform(): DOMMatrix;
  set currentTransform(matrix: Matrix);
  createProjection(quad: QuadOrRect, basis?: QuadOrRect): DOMMatrix;
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
  outlineText(text: string, maxWidth?: number): Path2D;

  // Skia filter properties (CanvasKit parity)
  /** Color filter applied during drawing. Set null to disable. */
  colorFilter: ColorFilter | null;
  /** Image filter applied during drawing. Set null to disable. */
  imageFilter: ImageFilter | null;
  /**
   * Coverage-mask filter (styled blur) applied during drawing -- glows,
   * feathered edges, outline blur. Set null to disable.
   */
  maskFilter: MaskFilter | null;

  drawParagraph(paragraph: Paragraph, x: number, y: number): void;
}

//
// Bézier Paths
//

export interface Path2DBounds {
  readonly top: number;
  readonly left: number;
  readonly bottom: number;
  readonly right: number;
  readonly width: number;
  readonly height: number;
}

export type Path2DEdge = [verb: string, ...args: number[]];

/**
 * This Canvas 2D API interface is used to declare a path that can then be used on a CanvasRenderingContext2D object. The path methods of the CanvasRenderingContext2D interface are also present on this interface, which gives you the convenience of being able to retain and replay your path whenever desired.
 *
 * [MDN Reference](https://developer.mozilla.org/docs/Web/API/Path2D)
 */
interface Path2D extends CanvasPath {
  readonly bounds: Path2DBounds;
  readonly edges: readonly Path2DEdge[];
  d: string;

  /**
   * Adds the path given by the argument to the path
   *
   * [MDN Reference](https://developer.mozilla.org/docs/Web/API/Path2D/addPath)
   */
  addPath(path: Path2D, transform?: DOMMatrix2DInit): void;

  contains(x: number, y: number): boolean;
  conicCurveTo(
    cpx: number,
    cpy: number,
    x: number,
    y: number,
    weight: number,
  ): void;

  complement(otherPath: Path2D): Path2D;
  difference(otherPath: Path2D): Path2D;
  intersect(otherPath: Path2D): Path2D;
  union(otherPath: Path2D): Path2D;
  xor(otherPath: Path2D): Path2D;
  interpolate(otherPath: Path2D, weight: number): Path2D;

  jitter(segmentLength: number, amount: number, seed?: number): Path2D;
  offset(dx: number, dy: number): Path2D;
  points(step?: number): readonly [x: number, y: number][];
  round(radius: number): Path2D;
  simplify(rule?: "nonzero" | "evenodd"): Path2D;
  transform(transform: Matrix): Path2D;
  transform(
    a: number,
    b: number,
    c: number,
    d: number,
    e: number,
    f: number,
  ): Path2D;
  trim(start: number, end: number, inverted?: boolean): Path2D;
  trim(start: number, inverted?: boolean): Path2D;

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

  /** Individual metrics for each line (only applicable when context's textWrap is set to `true` ) */
  readonly lines: TextMetricsLine[];
}

declare var TextMetrics: {
  prototype: TextMetrics;
  new (): TextMetrics;
};

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

export interface FontFamily {
  family: string;
  weights: number[];
  widths: string[];
  styles: string[];
}

export interface Font {
  family: string;
  weight: number;
  style: string;
  width: string;
  file: string;
}

interface FontLibrary {
  families: readonly string[];
  family(name: string): FontFamily | undefined;
  has(familyName: string): boolean;

  use(familyName: string, fontPaths?: string | readonly string[]): Font[];
  use(familyName: string, fontData: Buffer | ArrayBuffer): Font[];
  use(familyName: string, fontData: readonly (Buffer | ArrayBuffer)[]): Font[];
  use(fontPaths: readonly string[]): Font[];
  use(
    families: Record<string, readonly string[] | string>,
  ): Record<string, Font[]>;

  reset(): void;
}

export const FontLibrary: FontLibrary;

export const TextDecoration: {
  readonly NoDecoration: 0x0;
  readonly Underline: 0x1;
  readonly Overline: 0x2;
  readonly LineThrough: 0x4;
};

export const TextDecorationStyle: {
  readonly Solid: 0;
  readonly Double: 1;
  readonly Dotted: 2;
  readonly Dashed: 3;
  readonly Wavy: 4;
};

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

export interface TextShadowInput {
  color?: TextColorInput;
  offset?: [number, number];
  blurRadius?: number;
}

/**
 * Variable-font axis position. `axis` is a 4-character OpenType axis
 * tag (e.g. "wght", "wdth", "ital", "opsz"). `value` is a float in the
 * font's design space, clamped to the typeface's declared min/max for
 * that axis. Mirrors CanvasKit's `fontVariations` shape.
 */
export interface FontVariationInput {
  axis: string;
  value: number;
}

/**
 * One OpenType feature applied to a text run. `name` is an OpenType
 * feature tag ("smcp", "liga", "onum", "tnum", "ss01", "zero", ...);
 * `value` is the feature selector -- `1`/`0` to enable/disable, or an
 * index for features with multiple alternates. Defaults to `1` (enable)
 * when omitted. Mirrors CanvasKit's `TextFontFeatures`.
 */
export interface TextFontFeatures {
  name: string;
  value?: number;
}

export interface TextStyleInput {
  fontSize?: number;
  fontFamilies?: string[];
  color?: TextColorInput;
  foregroundColor?: TextColorInput;
  backgroundColor?: TextColorInput;
  fontStyle?: { weight?: number; width?: number; slant?: number };
  letterSpacing?: number;
  wordSpacing?: number;
  heightMultiplier?: number;
  decoration?: number;
  decorationStyle?: number;
  decorationColor?: TextColorInput;
  decorationThickness?: number;
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
 */
export interface StrutStyleInput {
  enabled?: boolean;
  fontFamilies?: string[];
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

export interface ParagraphStyleInput {
  textAlign?: string;
  textDirection?: string;
  maxLines?: number;
  ellipsis?: string;
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

export interface GlyphPosition {
  pos: number;
  affinity: number;
}

export interface TextBox {
  rect: [number, number, number, number];
  direction: number;
}

export interface LineMetrics {
  startIndex: number;
  endIndex: number;
  endExcludingWhitespaces: number;
  endIncludingNewline: number;
  isHardBreak: boolean;
  ascent: number;
  descent: number;
  height: number;
  width: number;
  left: number;
  baseline: number;
  lineNumber: number;
}

export class ParagraphBuilder {
  /**
   * Text is shaped with the process-global font library. CanvasKit takes a
   * `FontMgr` here; this build has no per-builder equivalent, so the parameter
   * is omitted rather than accepted and ignored.
   */
  static Make(style?: ParagraphStyleInput): ParagraphBuilder;
  pushStyle(style: TextStyleInput): this;
  pop(): this;
  addText(text: string): this;
  addPlaceholder(
    width: number,
    height: number,
    align?: number,
    baseline?: number,
    offset?: number,
  ): this;
  build(): Paragraph;
}

export class Paragraph {
  layout(width: number): void;
  getHeight(): number;
  getLongestLine(): number;
  getMaxWidth(): number;
  getMaxIntrinsicWidth(): number;
  getMinIntrinsicWidth(): number;
  getAlphabeticBaseline(): number;
  getIdeographicBaseline(): number;
  getGlyphPositionAtCoordinate(x: number, y: number): GlyphPosition;
  getRectsForRange(
    start: number,
    end: number,
    hStyle?: number,
    wStyle?: number,
  ): TextBox[];
  getLineMetrics(): LineMetrics[];
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
export type EventLoopMode = "node" | "native";
export type TextInputType =
  | "insertText"
  | "deleteContentBackward"
  | "deleteContentForward"
  | "insertLineBreak"
  | "insertCompositionText";
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

export type WindowOptions = {
  title?: string;
  left?: number;
  top?: number;
  width?: number;
  height?: number;
  fit?: FitStyle;
  page?: number;
  background?: string;
  fullscreen?: boolean;
  borderless?: boolean;
  resizable?: boolean;
  visible?: boolean;
  cursor?: CursorStyle;
  canvas?: Canvas;
} & TextOptions;

type MouseEventProps = {
  x: number;
  y: number;
  pageX: number;
  pageY: number;
  button: number;
  buttons: number;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
};

type KeyboardEventProps = {
  key: string;
  code: string;
  location: number;
  repeat: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
};

type WindowEvents = {
  mousedown: MouseEventProps;
  mouseup: MouseEventProps;
  mousemove: MouseEventProps;
  keydown: KeyboardEventProps;
  keyup: KeyboardEventProps;
  input: {
    data: string;
    inputType: TextInputType;
  };
  wheel: { deltaX: number; deltaY: number };
  fullscreen: { enabled: boolean };
  move: { left: number; top: number };
  resize: { height: number; width: number };
  frame: { frame: number };
  draw: { frame: number };
  blur: {};
  focus: {};
  setup: {};
  close: {};
};

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

  readonly ctx: CanvasRenderingContext2D;
  canvas: Canvas;
  visible: boolean;
  fullscreen: boolean;
  borderless: boolean;
  resizable: boolean;
  title: string;
  cursor: CursorStyle;
  fit: FitStyle;
  left: number;
  top: number;
  width: number;
  height: number;
  page: number;
  background: string;
  readonly closed: boolean;

  open(): void;
  close(): void;
}

export interface App extends EventEmitter<{
  idle: [{ type: "idle"; target: App }];
}> {
  readonly windows: Window[];
  readonly running: boolean;
  eventLoop: EventLoopMode;
  fps: number;

  launch(): Promise<undefined>;
  quit(): void;
}

export const App: App;
