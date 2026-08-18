//
// ColorFilter & ImageFilter - CanvasKit-compatible color matrix API
//

"use strict";

const { neon, REPR, ø, HANDLE, handled } = require("./neon");

const DELETED = Symbol("deleted");

// Bind a boxed struct to an instance and mark it live.
//
// Shared by the wrap helpers, which allocate onto a bare prototype, and by the
// constructors, which allocate onto `this`. Both have to arrive at the same
// shape: an instance carrying no boxed struct still satisfies `instanceof`, and
// every consumer in context.js gates on exactly that, so a half-built one would
// pass validation and then draw nothing.
// The handle goes in the same private slot as every other wrapped object's,
// read back through the `ø` accessor the four classes below install on
// themselves. This used to define `ø` on each instance with a descriptor of
// its own -- a third spelling of the same idea, and the slowest of the three.
function attach(instance, result) {
  instance[HANDLE] = result;
  instance[DELETED] = false;
  return instance;
}

// Pick a factory out of a class's kind table, or refuse naming the kinds that
// exist. Several of these classes build more than one kind of thing, so the
// constructor takes the kind as its first argument the way CanvasGradient
// already takes "linear" | "radial" | "conic".
function kindOf(table, kind, cls) {
  let make = table[String(kind).toLowerCase()];
  if (!make) {
    let known = Object.keys(table)
      .map((k) => `"${k}"`)
      .join(", ");
    throw new TypeError(
      `Unknown ${cls} kind "${kind}" (expected one of: ${known})`,
    );
  }
  return make;
}

// Finish construction, or refuse. The factories answer arguments Skia rejects
// with `null`; a constructor cannot, so it throws instead -- an instance with
// nothing behind it would still satisfy `instanceof` at every consumer.
function allocate(instance, result, cls, hint) {
  if (result === null) {
    throw new TypeError(`Could not create a ${cls} (${hint})`);
  }
  return attach(instance, result);
}

// Helper to create ColorFilter instance from boxed result
function wrapColorFilter(result) {
  if (result === null) return null;
  return attach(Object.create(ColorFilter.prototype), result);
}

class ColorFilter {
  /**
   * Colour transform, set as `ctx.colorFilter`.
   *
   * The kind names the transform to build and the remaining arguments are the
   * ones that kind takes, the way CanvasGradient reads its "linear" | "radial"
   * | "conic" first argument:
   *
   * ```js
   * new ColorFilter("matrix", twentyNumbers)
   * new ColorFilter("blend", "red", "multiply")
   * new ColorFilter("luma")
   * ```
   *
   * @param {ColorFilterKind} kind
   * @param {...any} args - whatever that kind takes; see the matching `Make` method
   */
  constructor(kind, ...args) {
    let made = kindOf(COLOR_FILTER_KINDS, kind, "ColorFilter")(...args);
    allocate(
      this,
      made && made[ø],
      "ColorFilter",
      `the arguments for kind "${kind}" did not describe a filter Skia could build`,
    );
  }

  /**
   * Create ColorFilter from 4x5 row-major matrix.
   * @param {Float32Array|ArrayLike<number>} matrix - 20 elements
   * @returns {ColorFilter}
   */
  static MakeMatrix(matrix) {
    if (matrix == null || typeof matrix.length !== "number") {
      throw new TypeError("Expected Float32Array or ArrayLike<number>");
    }
    if (matrix.length !== 20) {
      throw new RangeError(`Expected 20 matrix elements, got ${matrix.length}`);
    }
    const arr = Array.isArray(matrix) ? matrix : Array.from(matrix);
    return wrapColorFilter(neon.ColorFilter.makeMatrix(null, arr));
  }

  /**
   * Create ColorFilter that converts sRGB to linear gamma.
   * @returns {ColorFilter}
   */
  static MakeSRGBToLinearGamma() {
    return wrapColorFilter(neon.ColorFilter.makeSRGBToLinearGamma(null));
  }

  /**
   * Create ColorFilter that converts linear to sRGB gamma.
   * @returns {ColorFilter}
   */
  static MakeLinearToSRGBGamma() {
    return wrapColorFilter(neon.ColorFilter.makeLinearToSRGBGamma(null));
  }

  /**
   * Create ColorFilter that blends with a solid color.
   * @param {string} color - CSS color string
   * @param {string} mode - blend mode
   * @returns {ColorFilter|null}
   */
  static MakeBlend(color, mode) {
    return wrapColorFilter(neon.ColorFilter.makeBlend(null, color, mode));
  }

  /**
   * Compose two ColorFilters (outer applied after inner).
   * @param {ColorFilter} outer
   * @param {ColorFilter} inner
   * @returns {ColorFilter|null}
   */
  static MakeCompose(outer, inner) {
    if (!(outer instanceof ColorFilter) || !(inner instanceof ColorFilter)) {
      throw new TypeError("Expected ColorFilter arguments");
    }
    if (outer._deleted || inner._deleted) {
      throw new Error("ColorFilter has been deleted");
    }
    return wrapColorFilter(
      neon.ColorFilter.makeCompose(null, outer[ø], inner[ø]),
    );
  }

  /**
   * Interpolate between two ColorFilters.
   * @param {number} t - interpolation factor (0-1)
   * @param {ColorFilter} dst - destination filter (t=0)
   * @param {ColorFilter} src - source filter (t=1)
   * @returns {ColorFilter|null}
   */
  static MakeLerp(t, dst, src) {
    if (!(dst instanceof ColorFilter) || !(src instanceof ColorFilter)) {
      throw new TypeError("Expected ColorFilter arguments");
    }
    if (dst._deleted || src._deleted) {
      throw new Error("ColorFilter has been deleted");
    }
    return wrapColorFilter(neon.ColorFilter.makeLerp(null, t, dst[ø], src[ø]));
  }

  /**
   * Create HSLA color matrix filter.
   * @param {Float32Array|number[]} matrix - 20 elements
   * @returns {ColorFilter}
   */
  static MakeHSLAMatrix(matrix) {
    if (
      matrix == null ||
      typeof matrix.length !== "number" ||
      matrix.length !== 20
    ) {
      throw new RangeError("Expected 20 matrix elements");
    }
    const arr = Array.isArray(matrix) ? matrix : Array.from(matrix);
    return wrapColorFilter(neon.ColorFilter.makeHSLAMatrix(null, arr));
  }

  /**
   * Create lighting effect filter.
   * @param {string} multiply - multiply color (CSS)
   * @param {string} add - add color (CSS)
   * @returns {ColorFilter|null}
   */
  static MakeLighting(multiply, add) {
    return wrapColorFilter(neon.ColorFilter.makeLighting(null, multiply, add));
  }

  /**
   * Create luma (luminance) color filter.
   * @returns {ColorFilter}
   */
  static MakeLumaColorFilter() {
    return wrapColorFilter(neon.ColorFilter.makeLumaColorFilter(null));
  }

  /**
   * Create table-based color filter (same table for all channels).
   * @param {Uint8Array|number[]} table - 256 elements (0-255)
   * @returns {ColorFilter|null}
   */
  static MakeTable(table) {
    if (
      table == null ||
      typeof table.length !== "number" ||
      table.length !== 256
    ) {
      throw new RangeError("Expected 256 table elements");
    }
    const arr = Array.isArray(table) ? table : Array.from(table);
    return wrapColorFilter(neon.ColorFilter.makeTable(null, arr));
  }

  /**
   * Create table-based color filter with separate tables per channel.
   * @param {Uint8Array|number[]|null} tableA - alpha table (256 elements) or null
   * @param {Uint8Array|number[]|null} tableR - red table (256 elements) or null
   * @param {Uint8Array|number[]|null} tableG - green table (256 elements) or null
   * @param {Uint8Array|number[]|null} tableB - blue table (256 elements) or null
   * @returns {ColorFilter|null}
   */
  static MakeTableARGB(tableA, tableR, tableG, tableB) {
    const convert = (t) => {
      if (t == null) return null;
      if (t.length !== 256)
        throw new RangeError("Table must have 256 elements");
      return Array.isArray(t) ? t : Array.from(t);
    };
    return wrapColorFilter(
      neon.ColorFilter.makeTableARGB(
        null,
        convert(tableA),
        convert(tableR),
        convert(tableG),
        convert(tableB),
      ),
    );
  }

  delete() {
    if (!this[DELETED]) {
      neon.ColorFilter.delete(this[ø]);
      this[DELETED] = true;
    }
  }

  get _deleted() {
    return this[DELETED];
  }

  [REPR]() {
    if (this[DELETED]) {
      return "ColorFilter (deleted)";
    }
    return `ColorFilter (${neon.ColorFilter.repr(this[ø])})`;
  }
}

// Kind names derive from the static factories -- MakeHSLAMatrix -> "hsla-matrix"
// -- with one exception: MakeLumaColorFilter is "luma", since "luma-color-filter"
// would repeat the class name inside `new ColorFilter(...)`.
//
// Each entry goes through the static rather than calling neon directly, because
// the statics carry the argument checking: matrix length, deleted-filter guards,
// table sizes. Routing around them would give the constructor a weaker contract
// than the factory it mirrors.
const COLOR_FILTER_KINDS = {
  matrix: (...a) => ColorFilter.MakeMatrix(...a),
  "srgb-to-linear-gamma": (...a) => ColorFilter.MakeSRGBToLinearGamma(...a),
  "linear-to-srgb-gamma": (...a) => ColorFilter.MakeLinearToSRGBGamma(...a),
  blend: (...a) => ColorFilter.MakeBlend(...a),
  compose: (...a) => ColorFilter.MakeCompose(...a),
  lerp: (...a) => ColorFilter.MakeLerp(...a),
  "hsla-matrix": (...a) => ColorFilter.MakeHSLAMatrix(...a),
  lighting: (...a) => ColorFilter.MakeLighting(...a),
  luma: (...a) => ColorFilter.MakeLumaColorFilter(...a),
  table: (...a) => ColorFilter.MakeTable(...a),
  "table-argb": (...a) => ColorFilter.MakeTableARGB(...a),
};

// Helper to create ImageFilter instance from boxed result
function wrapImageFilter(result) {
  if (result === null) return null;
  return attach(Object.create(ImageFilter.prototype), result);
}

// Helper to validate and get boxed input filter
function getInputFilter(input) {
  if (input === null || input === undefined) return null;
  if (!(input instanceof ImageFilter)) {
    throw new TypeError("Expected ImageFilter or null");
  }
  if (input._deleted) {
    throw new Error("Input ImageFilter has been deleted");
  }
  return input[ø];
}

// Validate the optional crop rectangle the three morphology/convolution
// filters take. For those, the rectangle bounds the domain the kernel reads
// from as well as clipping the output, which is why it is an argument rather
// than a `"crop"` filter composed afterwards: a dilation given one stops
// spreading at the edge instead of spreading and then being cut.
function getCropRect(crop) {
  if (crop === null || crop === undefined) return null;
  if (
    !Array.isArray(crop) ||
    crop.length !== 4 ||
    crop.some((side) => typeof side != "number" || !isFinite(side))
  ) {
    throw new TypeError(
      "Expected an array of four numbers [x, y, width, height] for `crop`",
    );
  }
  return crop;
}

class ImageFilter {
  /**
   * Pixel-processing filter, set as `ctx.imageFilter` or passed as a
   * `saveLayer` backdrop.
   *
   * The kind names the filter to build and the remaining arguments are the
   * ones that kind takes:
   *
   * ```js
   * new ImageFilter("blur", 4, 4)
   * new ImageFilter("drop-shadow", 2, 2, 3, 3, "black")
   * new ImageFilter("empty")
   * ```
   *
   * Most kinds take an optional trailing `input` filter, which is what chains
   * them; passing `null` (the default) reads from the layer being drawn.
   *
   * @param {ImageFilterKind} kind
   * @param {...any} args - whatever that kind takes; see the matching `Make` method
   */
  constructor(kind, ...args) {
    let made = kindOf(IMAGE_FILTER_KINDS, kind, "ImageFilter")(...args);
    allocate(
      this,
      made && made[ø],
      "ImageFilter",
      `the arguments for kind "${kind}" did not describe a filter Skia could build`,
    );
  }

  /**
   * Create ImageFilter from ColorFilter.
   * @param {ColorFilter} colorFilter
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeColorFilter(colorFilter, input = null) {
    if (!(colorFilter instanceof ColorFilter)) {
      throw new TypeError("Expected ColorFilter as first argument");
    }
    if (colorFilter._deleted) {
      throw new Error("ColorFilter has been deleted");
    }
    return wrapImageFilter(
      neon.ImageFilter.makeColorFilter(
        null,
        colorFilter[ø],
        getInputFilter(input),
      ),
    );
  }

  /**
   * Compose two ImageFilters (outer applied after inner).
   * @param {ImageFilter} outer
   * @param {ImageFilter} inner
   * @returns {ImageFilter|null}
   */
  static MakeCompose(outer, inner) {
    if (!(outer instanceof ImageFilter) || !(inner instanceof ImageFilter)) {
      throw new TypeError("Expected ImageFilter arguments");
    }
    if (outer._deleted) {
      throw new Error("Outer ImageFilter has been deleted");
    }
    if (inner._deleted) {
      throw new Error("Inner ImageFilter has been deleted");
    }
    return wrapImageFilter(
      neon.ImageFilter.makeCompose(null, outer[ø], inner[ø]),
    );
  }

  /**
   * Create blur ImageFilter.
   * @param {number} sigmaX - horizontal blur radius
   * @param {number} sigmaY - vertical blur radius
   * @param {string} [tileMode="decal"] - "clamp" | "repeat" | "mirror" | "decal"
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeBlur(sigmaX, sigmaY, tileMode = "decal", input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeBlur(
        null,
        sigmaX,
        sigmaY,
        tileMode,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Create drop shadow ImageFilter.
   * @param {number} dx - shadow x offset
   * @param {number} dy - shadow y offset
   * @param {number} sigmaX - horizontal blur radius
   * @param {number} sigmaY - vertical blur radius
   * @param {string|number[]} color - CSS color string or [r,g,b,a] array (0-1)
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeDropShadow(dx, dy, sigmaX, sigmaY, color, input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeDropShadow(
        null,
        dx,
        dy,
        sigmaX,
        sigmaY,
        color,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Create drop shadow ImageFilter (shadow only, no source).
   * @param {number} dx - shadow x offset
   * @param {number} dy - shadow y offset
   * @param {number} sigmaX - horizontal blur radius
   * @param {number} sigmaY - vertical blur radius
   * @param {string|number[]} color - CSS color string or [r,g,b,a] array (0-1)
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeDropShadowOnly(dx, dy, sigmaX, sigmaY, color, input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeDropShadowOnly(
        null,
        dx,
        dy,
        sigmaX,
        sigmaY,
        color,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Create offset ImageFilter.
   * @param {number} dx - x offset
   * @param {number} dy - y offset
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeOffset(dx, dy, input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeOffset(null, dx, dy, getInputFilter(input)),
    );
  }

  /**
   * Create morphological dilation ImageFilter.
   * @param {number} radiusX - horizontal radius
   * @param {number} radiusY - vertical radius
   * @param {ImageFilter|null} [input]
   * @param {number[]|null} [crop] - [x, y, width, height] bounding the
   *   kernel's read domain as well as the output
   * @returns {ImageFilter|null}
   */
  static MakeDilate(radiusX, radiusY, input = null, crop = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeDilate(
        null,
        radiusX,
        radiusY,
        getInputFilter(input),
        getCropRect(crop),
      ),
    );
  }

  /**
   * Create morphological erosion ImageFilter.
   * @param {number} radiusX - horizontal radius
   * @param {number} radiusY - vertical radius
   * @param {ImageFilter|null} [input]
   * @param {number[]|null} [crop] - [x, y, width, height] bounding the
   *   kernel's read domain as well as the output
   * @returns {ImageFilter|null}
   */
  static MakeErode(radiusX, radiusY, input = null, crop = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeErode(
        null,
        radiusX,
        radiusY,
        getInputFilter(input),
        getCropRect(crop),
      ),
    );
  }

  /**
   * Merge multiple ImageFilters.
   * @param {(ImageFilter|null)[]} filters - array of filters to merge
   * @returns {ImageFilter|null}
   */
  static MakeMerge(filters) {
    if (!Array.isArray(filters)) {
      throw new TypeError("Expected array of ImageFilters");
    }
    const boxed = filters.map((f) => {
      if (f === null || f === undefined) return null;
      if (!(f instanceof ImageFilter)) {
        throw new TypeError("Expected ImageFilter or null");
      }
      if (f._deleted) {
        throw new Error("ImageFilter has been deleted");
      }
      return f[ø];
    });
    return wrapImageFilter(neon.ImageFilter.makeMerge(null, boxed));
  }

  /**
   * Create empty (no-op) ImageFilter.
   * @returns {ImageFilter}
   */
  static MakeEmpty() {
    return wrapImageFilter(neon.ImageFilter.makeEmpty(null));
  }

  /**
   * Create tile ImageFilter.
   * @param {number[]} src - source rect [x, y, width, height]
   * @param {number[]} dst - destination rect [x, y, width, height]
   * @param {ImageFilter|null} [input]
   * @returns {ImageFilter|null}
   */
  static MakeTile(src, dst, input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeTile(null, src, dst, getInputFilter(input)),
    );
  }

  // ==================== Advanced ImageFilter methods ====================

  /**
   * Blend two image filters using a blend mode.
   * @param {string} mode - blend mode ("srcOver", "multiply", "screen", etc.)
   * @param {ImageFilter|null} [background] - background filter
   * @param {ImageFilter|null} [foreground] - foreground filter
   * @returns {ImageFilter|null}
   */
  static MakeBlend(mode, background = null, foreground = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeBlend(
        null,
        mode,
        getInputFilter(background),
        getInputFilter(foreground),
      ),
    );
  }

  /**
   * Arithmetic blend: k1*fg*bg + k2*fg + k3*bg + k4.
   * @param {number} k1 - coefficient for fg*bg
   * @param {number} k2 - coefficient for fg
   * @param {number} k3 - coefficient for bg
   * @param {number} k4 - constant offset
   * @param {boolean} [enforcePMColor=true] - enforce premultiplied color
   * @param {ImageFilter|null} [background] - background filter
   * @param {ImageFilter|null} [foreground] - foreground filter
   * @returns {ImageFilter|null}
   */
  static MakeArithmetic(
    k1,
    k2,
    k3,
    k4,
    enforcePMColor = true,
    background = null,
    foreground = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeArithmetic(
        null,
        k1,
        k2,
        k3,
        k4,
        enforcePMColor,
        getInputFilter(background),
        getInputFilter(foreground),
      ),
    );
  }

  /**
   * Displacement map filter.
   * @param {string} xChannel - color channel for x displacement ("R", "G", "B", "A")
   * @param {string} yChannel - color channel for y displacement ("R", "G", "B", "A")
   * @param {number} scale - displacement scale
   * @param {ImageFilter|null} [displacement] - displacement map filter
   * @param {ImageFilter|null} [color] - color source filter
   * @returns {ImageFilter|null}
   */
  static MakeDisplacementMap(
    xChannel,
    yChannel,
    scale,
    displacement = null,
    color = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeDisplacementMap(
        null,
        xChannel,
        yChannel,
        scale,
        getInputFilter(displacement),
        getInputFilter(color),
      ),
    );
  }

  /**
   * Matrix convolution filter (e.g., sharpen, edge detect).
   * @param {number[]} kernelSize - [width, height] of kernel
   * @param {number[]} kernel - convolution kernel (width*height elements)
   * @param {number} gain - scale factor applied to result
   * @param {number} bias - bias added to result
   * @param {number[]} kernelOffset - [x, y] offset for kernel center
   * @param {string} [tileMode="decal"] - tile mode for edge handling
   * @param {boolean} [convolveAlpha=true] - whether to convolve alpha channel
   * @param {ImageFilter|null} [input] - input filter
   * @param {number[]|null} [crop] - [x, y, width, height] bounding the
   *   kernel's read domain as well as the output
   * @returns {ImageFilter|null}
   */
  static MakeMatrixConvolution(
    kernelSize,
    kernel,
    gain,
    bias,
    kernelOffset,
    tileMode = "decal",
    convolveAlpha = true,
    input = null,
    crop = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeMatrixConvolution(
        null,
        kernelSize,
        kernel,
        gain,
        bias,
        kernelOffset,
        tileMode,
        convolveAlpha,
        getInputFilter(input),
        getCropRect(crop),
      ),
    );
  }

  /**
   * Apply a matrix transformation to the image.
   * @param {number[]} matrix - 6 elements (2D affine) or 9 elements (3x3)
   * @param {string} [sampling="linear"] - sampling mode ("nearest" or "linear")
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeMatrixTransform(matrix, sampling = "linear", input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeMatrixTransform(
        null,
        matrix,
        sampling,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Magnifier (fisheye) effect.
   * @param {number[]} lensBounds - [x, y, width, height] of lens area
   * @param {number} zoomAmount - magnification factor
   * @param {number} inset - edge distortion width
   * @param {string} [sampling="linear"] - sampling mode
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeMagnifier(
    lensBounds,
    zoomAmount,
    inset,
    sampling = "linear",
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeMagnifier(
        null,
        lensBounds,
        zoomAmount,
        inset,
        sampling,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Crop filter with optional tile mode.
   * @param {number[]} rect - [x, y, width, height] crop rectangle
   * @param {string} [tileMode="decal"] - tile mode for pixels outside rect
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeCrop(rect, tileMode = "decal", input = null) {
    return wrapImageFilter(
      neon.ImageFilter.makeCrop(null, rect, tileMode, getInputFilter(input)),
    );
  }

  // ==================== Lighting ImageFilter methods ====================

  /**
   * Diffuse lighting from a distant light source.
   * @param {number[]} direction - [x, y, z] light direction
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} kd - diffuse reflectance coefficient
   * @param {ImageFilter|null} [input] - input filter (alpha as height map)
   * @returns {ImageFilter|null}
   */
  static MakeDistantLitDiffuse(
    direction,
    lightColor,
    surfaceScale,
    kd,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeDistantLitDiffuse(
        null,
        direction,
        lightColor,
        surfaceScale,
        kd,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Diffuse lighting from a point light source.
   * @param {number[]} location - [x, y, z] light position
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} kd - diffuse reflectance coefficient
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakePointLitDiffuse(
    location,
    lightColor,
    surfaceScale,
    kd,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makePointLitDiffuse(
        null,
        location,
        lightColor,
        surfaceScale,
        kd,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Diffuse lighting from a spot light source.
   * @param {number[]} location - [x, y, z] light position
   * @param {number[]} target - [x, y, z] spot target
   * @param {number} falloffExponent - falloff exponent
   * @param {number} cutoffAngle - cutoff angle in degrees
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} kd - diffuse reflectance coefficient
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeSpotLitDiffuse(
    location,
    target,
    falloffExponent,
    cutoffAngle,
    lightColor,
    surfaceScale,
    kd,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeSpotLitDiffuse(
        null,
        location,
        target,
        falloffExponent,
        cutoffAngle,
        lightColor,
        surfaceScale,
        kd,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Specular lighting from a distant light source.
   * @param {number[]} direction - [x, y, z] light direction
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} ks - specular reflectance coefficient
   * @param {number} shininess - specular exponent
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeDistantLitSpecular(
    direction,
    lightColor,
    surfaceScale,
    ks,
    shininess,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeDistantLitSpecular(
        null,
        direction,
        lightColor,
        surfaceScale,
        ks,
        shininess,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Specular lighting from a point light source.
   * @param {number[]} location - [x, y, z] light position
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} ks - specular reflectance coefficient
   * @param {number} shininess - specular exponent
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakePointLitSpecular(
    location,
    lightColor,
    surfaceScale,
    ks,
    shininess,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makePointLitSpecular(
        null,
        location,
        lightColor,
        surfaceScale,
        ks,
        shininess,
        getInputFilter(input),
      ),
    );
  }

  /**
   * Specular lighting from a spot light source.
   * @param {number[]} location - [x, y, z] light position
   * @param {number[]} target - [x, y, z] spot target
   * @param {number} falloffExponent - falloff exponent
   * @param {number} cutoffAngle - cutoff angle in degrees
   * @param {string} lightColor - CSS color of the light
   * @param {number} surfaceScale - height scale factor
   * @param {number} ks - specular reflectance coefficient
   * @param {number} shininess - specular exponent
   * @param {ImageFilter|null} [input] - input filter
   * @returns {ImageFilter|null}
   */
  static MakeSpotLitSpecular(
    location,
    target,
    falloffExponent,
    cutoffAngle,
    lightColor,
    surfaceScale,
    ks,
    shininess,
    input = null,
  ) {
    return wrapImageFilter(
      neon.ImageFilter.makeSpotLitSpecular(
        null,
        location,
        target,
        falloffExponent,
        cutoffAngle,
        lightColor,
        surfaceScale,
        ks,
        shininess,
        getInputFilter(input),
      ),
    );
  }

  delete() {
    if (!this[DELETED]) {
      neon.ImageFilter.delete(this[ø]);
      this[DELETED] = true;
    }
  }

  get _deleted() {
    return this[DELETED];
  }

  [REPR]() {
    if (this[DELETED]) {
      return "ImageFilter (deleted)";
    }
    return `ImageFilter (${neon.ImageFilter.repr(this[ø])})`;
  }
}

// Every kind name derives from its static factory with no exceptions here:
// MakeDropShadowOnly -> "drop-shadow-only". As with ColorFilter, each entry
// goes through the static so the constructor inherits its argument checking.
const IMAGE_FILTER_KINDS = {
  "color-filter": (...a) => ImageFilter.MakeColorFilter(...a),
  compose: (...a) => ImageFilter.MakeCompose(...a),
  blur: (...a) => ImageFilter.MakeBlur(...a),
  "drop-shadow": (...a) => ImageFilter.MakeDropShadow(...a),
  "drop-shadow-only": (...a) => ImageFilter.MakeDropShadowOnly(...a),
  offset: (...a) => ImageFilter.MakeOffset(...a),
  dilate: (...a) => ImageFilter.MakeDilate(...a),
  erode: (...a) => ImageFilter.MakeErode(...a),
  merge: (...a) => ImageFilter.MakeMerge(...a),
  empty: (...a) => ImageFilter.MakeEmpty(...a),
  tile: (...a) => ImageFilter.MakeTile(...a),
  blend: (...a) => ImageFilter.MakeBlend(...a),
  arithmetic: (...a) => ImageFilter.MakeArithmetic(...a),
  "displacement-map": (...a) => ImageFilter.MakeDisplacementMap(...a),
  "matrix-convolution": (...a) => ImageFilter.MakeMatrixConvolution(...a),
  "matrix-transform": (...a) => ImageFilter.MakeMatrixTransform(...a),
  magnifier: (...a) => ImageFilter.MakeMagnifier(...a),
  crop: (...a) => ImageFilter.MakeCrop(...a),
  "distant-lit-diffuse": (...a) => ImageFilter.MakeDistantLitDiffuse(...a),
  "point-lit-diffuse": (...a) => ImageFilter.MakePointLitDiffuse(...a),
  "spot-lit-diffuse": (...a) => ImageFilter.MakeSpotLitDiffuse(...a),
  "distant-lit-specular": (...a) => ImageFilter.MakeDistantLitSpecular(...a),
  "point-lit-specular": (...a) => ImageFilter.MakePointLitSpecular(...a),
  "spot-lit-specular": (...a) => ImageFilter.MakeSpotLitSpecular(...a),
};

// Helper to create MaskFilter instance from boxed result
function wrapMaskFilter(result) {
  if (result === null) return null;
  return attach(Object.create(MaskFilter.prototype), result);
}

class MaskFilter {
  /**
   * Gaussian coverage-mask blur. Unlike an ImageFilter blur, the style
   * controls how the blur relates to the geometry (glow, halo, inner
   * shadow).
   *
   * A blur is the only kind of mask filter there is: Skia's core header
   * declares `MakeBlur` and nothing else, and the three in `include/effects`
   * are all marked for removal. So the first argument is the blur style
   * rather than a kind, and no second kind is expected.
   *
   * Where {@link MaskFilter.MakeBlur} answers arguments Skia rejects with
   * `null`, this throws — a constructor has no way to report failure by
   * returning, and an instance with no filter behind it would still satisfy
   * `instanceof` at every consumer that checks it.
   *
   * @param {"normal"|"solid"|"outer"|"inner"} style
   * @param {number} sigma - blur standard deviation in pixels, greater than 0
   * @param {boolean} [respectCTM=true] - scale the blur with the transform
   */
  constructor(style, sigma, respectCTM = true) {
    allocate(
      this,
      neon.MaskFilter.makeBlur(null, style, sigma, respectCTM),
      "MaskFilter",
      `sigma must be a number greater than 0, got ${sigma}`,
    );
  }

  /**
   * Gaussian coverage-mask blur. Unlike an ImageFilter blur, the style
   * controls how the blur relates to the geometry (glow, halo, inner
   * shadow). Mirrors CanvasKit's `MaskFilter.MakeBlur`.
   * @param {"normal"|"solid"|"outer"|"inner"} style
   * @param {number} sigma - blur standard deviation in pixels
   * @param {boolean} [respectCTM=true] - scale the blur with the transform
   * @returns {MaskFilter|null}
   */
  static MakeBlur(style, sigma, respectCTM = true) {
    return wrapMaskFilter(
      neon.MaskFilter.makeBlur(null, style, sigma, respectCTM),
    );
  }

  delete() {
    if (!this[DELETED]) {
      neon.MaskFilter.delete(this[ø]);
      this[DELETED] = true;
    }
  }

  get _deleted() {
    return this[DELETED];
  }

  [REPR]() {
    return this[DELETED] ? "MaskFilter (deleted)" : "MaskFilter";
  }
}

// Helper to create Shader instance from boxed result
function wrapShader(result) {
  if (result === null) return null;
  return attach(Object.create(Shader.prototype), result);
}

// Kind names derive from the static factories: MakeFractalNoise -> "fractal-noise".
const SHADER_KINDS = {
  "fractal-noise": (...args) => neon.Shader.makeFractalNoise(null, ...args),
  turbulence: (...args) => neon.Shader.makeTurbulence(null, ...args),
};

class Shader {
  /**
   * Procedural noise shader, set as `ctx.fillStyle` or `strokeStyle`.
   *
   * Both kinds take the same four arguments; they differ in how the noise is
   * summed. `"fractal-noise"` is soft and cloud-like, `"turbulence"` sharper
   * and more chaotic.
   *
   * @param {"fractal-noise"|"turbulence"} kind
   * @param {number} baseFreqX - noise frequency along x (small = larger features)
   * @param {number} baseFreqY - noise frequency along y
   * @param {number} octaves - detail levels
   * @param {number} seed - pattern seed
   */
  constructor(kind, baseFreqX, baseFreqY, octaves, seed) {
    let make = kindOf(SHADER_KINDS, kind, "Shader");
    allocate(
      this,
      make(baseFreqX, baseFreqY, octaves, seed),
      "Shader",
      "check the frequency, octave and seed arguments",
    );
  }

  /**
   * Procedural fractal (Perlin) noise -- film grain, clouds, organic
   * texture. Set as `ctx.fillStyle`/`strokeStyle`. Mirrors CanvasKit's
   * `Shader.MakeFractalNoise`.
   * @param {number} baseFreqX - noise frequency along x (small = larger features)
   * @param {number} baseFreqY - noise frequency along y
   * @param {number} octaves - detail levels
   * @param {number} seed - pattern seed
   * @returns {Shader|null}
   */
  static MakeFractalNoise(baseFreqX, baseFreqY, octaves, seed) {
    return wrapShader(
      neon.Shader.makeFractalNoise(null, baseFreqX, baseFreqY, octaves, seed),
    );
  }

  /**
   * Turbulence (absolute-value Perlin noise) -- sharper, more chaotic
   * than fractal noise. Mirrors CanvasKit's `Shader.MakeTurbulence`.
   * @returns {Shader|null}
   */
  static MakeTurbulence(baseFreqX, baseFreqY, octaves, seed) {
    return wrapShader(
      neon.Shader.makeTurbulence(null, baseFreqX, baseFreqY, octaves, seed),
    );
  }

  delete() {
    if (!this[DELETED]) {
      neon.Shader.delete(this[ø]);
      this[DELETED] = true;
    }
  }

  get _deleted() {
    return this[DELETED];
  }

  [REPR]() {
    return this[DELETED] ? "Shader (deleted)" : "Shader";
  }
}

// These four do not extend `RustClass` -- each is built from a kind table
// rather than from one Rust constructor -- so they install the accessor that
// reads their handle for themselves, here, where all four are defined.
[ColorFilter, ImageFilter, MaskFilter, Shader].forEach(handled);

module.exports = { ColorFilter, ImageFilter, MaskFilter, Shader };
