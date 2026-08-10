---
description: Composable pixel effects applied while drawing
---

# ImageFilter

> An `ImageFilter` runs over the pixels a draw produces rather than over each color in isolation, so it can blur, offset, warp and light them. Assign one to a [context][context]'s `.imageFilter` property and it applies to subsequent fills, strokes, text and [`drawImage()`][drawImage] calls until you set the property back to `null`. [`drawCanvas()`][drawcanvas] and [`putImageData()`][putImageData] bypass it. The value is part of the saved state, so [`save()`][save] and [`restore()`][restore] bracket it like any other context setting. 🧪

| Blur & shadow                      | Geometry                              | Morphology            | Compositing                             | Lighting                                            |
| ---------------------------------- | ------------------------------------- | --------------------- | --------------------------------------- | --------------------------------------------------- |
| [`"blur"`][if_blur]                | [`"offset"`][if_offset]               | [`"dilate"`][if_dilate] | [`"blend"`][if_blend]                   | [`"distant-lit-diffuse"`][if_distant_diffuse]       |
| [`"drop-shadow"`][if_shadow]       | [`"matrix-transform"`][if_matrix]     | [`"erode"`][if_erode]   | [`"arithmetic"`][if_arithmetic]         | [`"point-lit-diffuse"`][if_point_diffuse]           |
| [`"drop-shadow-only"`][if_shadow_only] | [`"crop"`][if_crop]               |                       | [`"merge"`][if_merge]                   | [`"spot-lit-diffuse"`][if_spot_diffuse]             |
| [`"matrix-convolution"`][if_convolution] | [`"tile"`][if_tile]             |                       | [`"compose"`][if_compose]               | [`"distant-lit-specular"`][if_distant_specular]     |
| [`"magnifier"`][if_magnifier]      | [`"displacement-map"`][if_displacement] |                     | [`"color-filter"`][if_colorfilter]      | [`"point-lit-specular"`][if_point_specular]         |
| [`"empty"`][if_empty]              |                                       |                       | [delete()][if_delete]                   | [`"spot-lit-specular"`][if_spot_specular]           |

## Creating image filters

`ImageFilter` is not part of the HTML Canvas standard. It mirrors CanvasKit's class of the same name, so each kind of filter has both a constructor form and a `Make…` static factory:

```js
ctx.imageFilter = new ImageFilter("blur", 4, 4);
ctx.imageFilter = ImageFilter.MakeBlur(4, 4);
```

The first argument to the constructor names the kind; the rest are the arguments that kind takes, in the same order as the matching static. The two forms differ only in how they report failure:

- The **statics return `null`** when Skia declines to build a filter from otherwise well-formed arguments.
- The **constructor throws a `TypeError`** instead, since there is no useful object it could return.

Arguments of the wrong shape — an unknown kind, a matrix that is not 6 or 9 elements long, an unparseable color, a filter that has already been deleted — throw from both forms.

```js
ImageFilter.MakeBlur(-1, -1); // → null
new ImageFilter("blur", -1, -1); // → TypeError
```

### Chaining

Most kinds end with an optional `input` filter. When it is omitted or `null` the filter reads the pixels the draw itself produced; when it is another `ImageFilter` it reads that filter's output instead. So a chain is built inside-out:

```js
// blur the shape first, then cast a shadow from the blurred result
ctx.imageFilter = new ImageFilter(
  "drop-shadow",
  6,
  6,
  4,
  4,
  "rgba(0, 0, 0, 0.4)",
  new ImageFilter("blur", 2, 2),
);
```

[`"compose"`][if_compose] expresses the same thing when you already hold both filters, and the kinds that take two inputs ([`"blend"`][if_blend], [`"arithmetic"`][if_arithmetic], [`"displacement-map"`][if_displacement]) use `null` in either slot to mean "the draw itself".

### Rects and matrices

Every rectangle argument on this page is `[x, y, width, height]`, not `[left, top, right, bottom]`. `Point3` arguments are `[x, y, z]`.

---

## Kinds

### `"blur"`

```js returns="ImageFilter"
new ImageFilter("blur", sigmaX, sigmaY, tileMode, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeBlur(sigmaX, sigmaY, (tileMode = "decal"), (input = null));
```

Gaussian blur with independent horizontal and vertical standard deviations, in pixels. `tileMode` decides what the blur reads past the edge of its input: `"clamp"`, `"repeat"`, `"mirror"` or `"decal"`. Negative sigmas build nothing.

### `"drop-shadow"`

```js returns="ImageFilter"
new ImageFilter("drop-shadow", dx, dy, sigmaX, sigmaY, color, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeDropShadow(dx, dy, sigmaX, sigmaY, color, (input = null));
```

Draws the source, then a blurred copy of its alpha offset by `(dx, dy)` and tinted with `color` behind it. `color` is either a CSS string or a `[r, g, b, a]` array of floats in the 0–1 range.

Unlike [`shadowBlur`][shadowBlur], the sigmas are independent per axis and the shadow travels through the rest of the filter chain.

### `"drop-shadow-only"`

```js returns="ImageFilter"
new ImageFilter("drop-shadow-only", dx, dy, sigmaX, sigmaY, color, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeDropShadowOnly(dx, dy, sigmaX, sigmaY, color, (input = null));
```

The same shadow with the source omitted. Useful as one arm of a [`"merge"`][if_merge] when you want to interleave other drawing between the shadow and the shape.

### `"offset"`

```js returns="ImageFilter"
new ImageFilter("offset", dx, dy, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeOffset(dx, dy, (input = null));
```

Translates the result by `(dx, dy)` pixels without touching the transform.

### `"matrix-transform"`

```js returns="ImageFilter"
new ImageFilter("matrix-transform", matrix, sampling, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeMatrixTransform(matrix, (sampling = "linear"), (input = null));
```

Applies an arbitrary transform to the result. `matrix` is either **6 elements in `[a, b, c, d, e, f]` order — the same order [`transform()`][transform] takes** — or 9 elements as a row-major 3×3 matrix, which additionally allows perspective. Any other length throws. `sampling` is `"nearest"` or `"linear"`.

The two element counts do **not** use the same ordering, so a 6-element affine is not the first six entries of the 9-element form:

```js
new ImageFilter("matrix-transform", [2, 0, 0, 2, 0, 0]); // 2× scale
new ImageFilter("matrix-transform", [2, 0, 0, 0, 2, 0, 0, 0, 1]); // the same 2× scale
```

A matrix Skia cannot invert — `[2, 0, 0, 0, 2, 0]`, which reads as a zero vertical scale in the 6-element form — builds nothing.

### `"crop"`

```js returns="ImageFilter"
new ImageFilter("crop", rect, tileMode, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeCrop(rect, (tileMode = "decal"), (input = null));
```

Restricts the result to `rect`, given as `[x, y, width, height]`. `tileMode` decides what fills the area outside it: `"decal"` leaves it empty, while `"clamp"`, `"repeat"` and `"mirror"` extend the cropped content across the rest of the layer.

### `"tile"`

```js returns="ImageFilter"
new ImageFilter("tile", src, dst, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeTile(src, dst, (input = null));
```

Takes the `src` rectangle of the result and repeats it across `dst`. Both are `[x, y, width, height]`. If nothing was drawn inside `src` the output is empty.

```js
ctx.imageFilter = new ImageFilter("tile", [0, 0, 50, 50], [0, 0, 200, 200]);
ctx.fillStyle = "orange";
ctx.fillRect(0, 0, 25, 25);
ctx.fillStyle = "teal";
ctx.fillRect(25, 25, 25, 25); // → a checkerboard across the whole 200 × 200
```

### `"displacement-map"`

```js returns="ImageFilter"
new ImageFilter("displacement-map", xChannel, yChannel, scale, displacement, color);
```

```js returns="ImageFilter | null"
ImageFilter.MakeDisplacementMap(
  xChannel,
  yChannel,
  scale,
  (displacement = null),
  (color = null),
);
```

Moves each pixel of `color` by an amount read out of `displacement`. `xChannel` and `yChannel` name which channel drives each axis — `"R"`, `"G"`, `"B"` or `"A"` — and `scale` is the maximum displacement in pixels. Either input may be `null` to use the draw itself.

### `"dilate"` & `"erode"`

```js returns="ImageFilter"
new ImageFilter("dilate", radiusX, radiusY, input);
new ImageFilter("erode", radiusX, radiusY, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeDilate(radiusX, radiusY, (input = null));
ImageFilter.MakeErode(radiusX, radiusY, (input = null));
```

Morphological grow and shrink, with the radii in pixels. `"dilate"` expands bright areas by the radius, thickening shapes and closing gaps; `"erode"` contracts them by the same amount. A 20 px square under `dilate(5, 5)` covers 30 px; under `erode(5, 5)` it covers 10.

### `"matrix-convolution"`

```js returns="ImageFilter"
new ImageFilter(
  "matrix-convolution",
  kernelSize,
  kernel,
  gain,
  bias,
  kernelOffset,
  tileMode,
  convolveAlpha,
  input,
);
```

```js returns="ImageFilter | null"
ImageFilter.MakeMatrixConvolution(
  kernelSize,
  kernel,
  gain,
  bias,
  kernelOffset,
  (tileMode = "decal"),
  (convolveAlpha = true),
  (input = null),
);
```

Runs a convolution kernel over the result — sharpen, emboss, edge detect. `kernelSize` is `[width, height]` and `kernel` must hold exactly `width × height` numbers. Each output is `gain × Σ(kernel · neighbourhood) + bias`. `kernelOffset` is `[x, y]` and says which kernel cell lands on the pixel being computed, so a 3×3 kernel is normally centred with `[1, 1]`.

```js
// a standard sharpen
ctx.imageFilter = new ImageFilter(
  "matrix-convolution",
  [3, 3],
  [0, -1, 0, -1, 5, -1, 0, -1, 0],
  1,
  0,
  [1, 1],
  "clamp",
  true,
);
```

### `"magnifier"`

```js returns="ImageFilter"
new ImageFilter("magnifier", lensBounds, zoomAmount, inset, sampling, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeMagnifier(
  lensBounds,
  zoomAmount,
  inset,
  (sampling = "linear"),
  (input = null),
);
```

A fisheye lens over `lensBounds` (`[x, y, width, height]`). `zoomAmount` is the magnification at the centre and `inset` is how wide a band at the edge of the lens the distortion is spread across.

### `"blend"`

```js returns="ImageFilter"
new ImageFilter("blend", mode, background, foreground);
```

```js returns="ImageFilter | null"
ImageFilter.MakeBlend(mode, (background = null), (foreground = null));
```

Combines two filters with a blend mode, using the same names as [`globalCompositeOperation`][gco]. Either input may be `null` to use the draw itself.

### `"arithmetic"`

```js returns="ImageFilter"
new ImageFilter("arithmetic", k1, k2, k3, k4, enforcePMColor, background, foreground);
```

```js returns="ImageFilter | null"
ImageFilter.MakeArithmetic(
  k1,
  k2,
  k3,
  k4,
  (enforcePMColor = true),
  (background = null),
  (foreground = null),
);
```

Combines two filters channel by channel as `k1·fg·bg + k2·fg + k3·bg + k4`. `enforcePMColor` clamps the result back to valid premultiplied color.

### `"merge"`

```js returns="ImageFilter"
new ImageFilter("merge", filters);
```

```js returns="ImageFilter | null"
ImageFilter.MakeMerge(filters);
```

Draws an array of filters one over the next, first entry underneath. A `null` entry means the draw itself, which is how you put a shape on top of its own shadow:

```js
ctx.imageFilter = new ImageFilter("merge", [
  new ImageFilter("drop-shadow-only", 8, 8, 4, 4, "black"),
  null,
]);
```

An empty array builds a filter that draws nothing.

### `"compose"`

```js returns="ImageFilter"
new ImageFilter("compose", outer, inner);
```

```js returns="ImageFilter | null"
ImageFilter.MakeCompose(outer, inner);
```

Runs `inner` first and feeds its result to `outer`, the same thing passing `inner` as the trailing `input` argument would do. Both arguments are required and neither may have been deleted.

### `"color-filter"`

```js returns="ImageFilter"
new ImageFilter("color-filter", colorFilter, input);
```

```js returns="ImageFilter | null"
ImageFilter.MakeColorFilter(colorFilter, (input = null));
```

Lifts a [`ColorFilter`][colorfilter] into the image-filter chain, so a color transform can sit between two pixel effects rather than being applied to the whole draw.

### `"empty"`

```js returns="ImageFilter"
new ImageFilter("empty");
```

```js returns="ImageFilter"
ImageFilter.MakeEmpty();
```

Produces nothing. Drawing through it leaves the canvas untouched — it is a stand-in for "no output", not for "no filter". To turn filtering off, set the context's `.imageFilter` to `null`.

### Lighting

```js returns="ImageFilter"
new ImageFilter("distant-lit-diffuse", direction, lightColor, surfaceScale, kd, input);
new ImageFilter("point-lit-diffuse", location, lightColor, surfaceScale, kd, input);
new ImageFilter(
  "spot-lit-diffuse",
  location,
  target,
  falloffExponent,
  cutoffAngle,
  lightColor,
  surfaceScale,
  kd,
  input,
);
```

```js returns="ImageFilter"
new ImageFilter(
  "distant-lit-specular",
  direction,
  lightColor,
  surfaceScale,
  ks,
  shininess,
  input,
);
new ImageFilter(
  "point-lit-specular",
  location,
  lightColor,
  surfaceScale,
  ks,
  shininess,
  input,
);
new ImageFilter(
  "spot-lit-specular",
  location,
  target,
  falloffExponent,
  cutoffAngle,
  lightColor,
  surfaceScale,
  ks,
  shininess,
  input,
);
```

Six lighting filters, matching the SVG lighting primitives. Each reads its input's **alpha channel as a height map** and shades it, so the output covers the whole layer rather than just the drawn shape.

- The light source is a `direction` (`distant`), a `location` (`point`), or a `location` aimed at a `target` with `falloffExponent` and `cutoffAngle` in degrees (`spot`). All three are `[x, y, z]`.
- `lightColor` is a CSS color string and `surfaceScale` scales the height map.
- The `diffuse` variants take a diffuse reflectance `kd`; the `specular` variants take `ks` plus a `shininess` exponent.

Every one has a matching static — `MakeDistantLitDiffuse`, `MakePointLitSpecular`, and so on — with the same arguments minus the kind.

---

## Methods

### `delete()`

```js returns="void"
delete();
```

Marks the filter as unusable. Drawing through a deleted filter, or passing one as an `input`, throws an `Error`. Calling it is optional — filters are garbage-collected like any other object.

<!-- references_begin -->

[if_blur]: #blur
[if_shadow]: #drop-shadow
[if_shadow_only]: #drop-shadow-only
[if_offset]: #offset
[if_matrix]: #matrix-transform
[if_crop]: #crop
[if_tile]: #tile
[if_displacement]: #displacement-map
[if_dilate]: #dilate--erode
[if_erode]: #dilate--erode
[if_convolution]: #matrix-convolution
[if_magnifier]: #magnifier
[if_blend]: #blend
[if_arithmetic]: #arithmetic
[if_merge]: #merge
[if_compose]: #compose
[if_colorfilter]: #color-filter
[if_empty]: #empty
[if_distant_diffuse]: #lighting
[if_point_diffuse]: #lighting
[if_spot_diffuse]: #lighting
[if_distant_specular]: #lighting
[if_point_specular]: #lighting
[if_spot_specular]: #lighting
[if_delete]: #delete
[colorfilter]: color-filter.md
[context]: context.md
[drawcanvas]: context.md#drawcanvas
[gco]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/globalCompositeOperation
[shadowBlur]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/shadowBlur
[transform]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/transform
[drawImage]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/drawImage
[putImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/putImageData
[save]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/save
[restore]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/restore

<!-- references_end -->
