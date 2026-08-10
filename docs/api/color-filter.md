---
description: Per-pixel color transforms applied while drawing
---

# ColorFilter

> A `ColorFilter` remaps every pixel's color as it is drawn. Assign one to a [context][context]'s `.colorFilter` property and it applies to subsequent fills, strokes, text and [`drawImage()`][drawImage] calls until you set the property back to `null`. [`drawCanvas()`][drawcanvas] and [`putImageData()`][putImageData] bypass it. The value is part of the saved state, so [`save()`][save] and [`restore()`][restore] bracket it like any other context setting. Filters are immutable and can be reused across contexts and canvases. 🧪

| Color mapping                    | Blending                   | Gamma                                       | Lookup tables                  |
| -------------------------------- | -------------------------- | ------------------------------------------- | ------------------------------ |
| [`"matrix"`][cf_matrix]          | [`"blend"`][cf_blend]      | [`"srgb-to-linear-gamma"`][cf_srgb_linear]  | [`"table"`][cf_table]          |
| [`"hsla-matrix"`][cf_hsla]       | [`"compose"`][cf_compose]  | [`"linear-to-srgb-gamma"`][cf_linear_srgb]  | [`"table-argb"`][cf_table_argb] |
| [`"lighting"`][cf_lighting]      | [`"lerp"`][cf_lerp]        |                                             | [delete()][cf_delete]          |
| [`"luma"`][cf_luma]              |                            |                                             |                                |

## Creating color filters

`ColorFilter` is not part of the HTML Canvas standard. It mirrors CanvasKit's class of the same name, so each kind of filter has both a constructor form and a `Make…` static factory:

```js
ctx.colorFilter = new ColorFilter("blend", "red", "multiply");
ctx.colorFilter = ColorFilter.MakeBlend("red", "multiply");
```

The first argument to the constructor names the kind; the rest are the arguments that kind takes, in the same order as the matching static. The two forms differ only in how they report failure:

- The **statics return `null`** when Skia declines to build a filter from otherwise well-formed arguments.
- The **constructor throws a `TypeError`** instead, since there is no useful object it could return.

Arguments of the wrong shape — an unknown kind, a mis-sized matrix, an unparseable color, a blend mode that does not exist — throw from both forms.

```js
ColorFilter.MakeTableARGB(null, null, null, null); // → null
new ColorFilter("table-argb", null, null, null, null); // → TypeError
new ColorFilter("blend", "not-a-color", "multiply"); // → TypeError from both
```

Matrices and tables are copied when the filter is built, so the array you passed in is safe to mutate afterwards.

---

## Kinds

### `"matrix"`

```js returns="ColorFilter"
new ColorFilter("matrix", matrix);
```

```js returns="ColorFilter"
ColorFilter.MakeMatrix(matrix);
```

Applies a 4×5 row-major color matrix. `matrix` is a 20-element array or `Float32Array` laid out as four rows of `[…scales, offset]`, one per output channel:

```
R' = m0·R + m1·G + m2·B  + m3·A  + m4
G' = m5·R + m6·G + m7·B  + m8·A  + m9
B' = m10·R + m11·G + m12·B + m13·A + m14
A' = m15·R + m16·G + m17·B + m18·A + m19
```

Any other length throws a `RangeError`. The matrix operates in the canvas's working color space, so the result depends on whether that space is sRGB, P3, or linear.

The exported [`ColorMatrix`][colormatrix] helper builds these:

```js
ctx.colorFilter = new ColorFilter("matrix", ColorMatrix.scaled(1, 0.6, 0.6, 1));
ctx.fillStyle = "rgb(200, 100, 50)";
ctx.fillRect(0, 0, 64, 64); // → rgb(200, 60, 30)
```

### `"hsla-matrix"`

```js returns="ColorFilter"
new ColorFilter("hsla-matrix", matrix);
```

```js returns="ColorFilter"
ColorFilter.MakeHSLAMatrix(matrix);
```

The same 20-element matrix, but applied to the pixel's hue, saturation, lightness and alpha instead of its red, green, blue and alpha. Useful for saturation and lightness grades that would need an awkward RGB matrix.

### `"lighting"`

```js returns="ColorFilter"
new ColorFilter("lighting", multiply, add);
```

```js returns="ColorFilter | null"
ColorFilter.MakeLighting(multiply, add);
```

Multiplies each channel by the matching channel of the `multiply` color, then adds the matching channel of `add`. Both are CSS color strings.

```js
ctx.colorFilter = new ColorFilter("lighting", "#808080", "#202020");
ctx.fillStyle = "rgb(200, 100, 50)";
ctx.fillRect(0, 0, 64, 64); // → rgb(132, 82, 57)
```

### `"luma"`

```js returns="ColorFilter"
new ColorFilter("luma");
```

```js returns="ColorFilter"
ColorFilter.MakeLumaColorFilter();
```

Replaces the pixel's alpha with its luminance and its color with black. Drawing opaque `rgb(200, 100, 50)` through it produces `rgba(0, 0, 0, 118/255)`. This is the building block for luminance masks: combine it with a [`"destination-in"`][gco] composite operation to knock a bright-to-transparent gradient out of what is already on the canvas.

### `"blend"`

```js returns="ColorFilter"
new ColorFilter("blend", color, mode);
```

```js returns="ColorFilter | null"
ColorFilter.MakeBlend(color, mode);
```

Blends a solid `color` into every pixel using `mode`, which accepts the same names as [`globalCompositeOperation`][gco].

```js
ctx.colorFilter = new ColorFilter("blend", "#ff0000", "multiply");
ctx.fillStyle = "rgb(200, 100, 50)";
ctx.fillRect(0, 0, 64, 64); // → rgb(200, 0, 0)
```

### `"compose"`

```js returns="ColorFilter"
new ColorFilter("compose", outer, inner);
```

```js returns="ColorFilter | null"
ColorFilter.MakeCompose(outer, inner);
```

Runs `inner` first and feeds its result to `outer`.

### `"lerp"`

```js returns="ColorFilter"
new ColorFilter("lerp", t, dst, src);
```

```js returns="ColorFilter | null"
ColorFilter.MakeLerp(t, dst, src);
```

Interpolates between the outputs of two filters: `t` of `0` gives `dst`, `1` gives `src`. Passing anything that is not a `ColorFilter` for `dst` or `src` throws.

### `"srgb-to-linear-gamma"`

```js returns="ColorFilter"
new ColorFilter("srgb-to-linear-gamma");
```

```js returns="ColorFilter"
ColorFilter.MakeSRGBToLinearGamma();
```

Removes the sRGB transfer curve, leaving linear-light values. Opaque `rgb(200, 100, 50)` becomes `rgb(147, 32, 8)`.

### `"linear-to-srgb-gamma"`

```js returns="ColorFilter"
new ColorFilter("linear-to-srgb-gamma");
```

```js returns="ColorFilter"
ColorFilter.MakeLinearToSRGBGamma();
```

The inverse: re-applies the sRGB transfer curve. Opaque `rgb(200, 100, 50)` becomes `rgb(229, 168, 122)`.

### `"table"`

```js returns="ColorFilter"
new ColorFilter("table", table);
```

```js returns="ColorFilter | null"
ColorFilter.MakeTable(table);
```

Applies one 256-entry lookup table to **all four** channels, alpha included. `table` is a `Uint8Array` or array of 256 numbers; any other length throws a `RangeError`. Because alpha is remapped too, an inverting table turns an alpha byte of 64 into 191 — use [`"table-argb"`][cf_table_argb] if you only want the color channels.

### `"table-argb"`

```js returns="ColorFilter"
new ColorFilter("table-argb", tableA, tableR, tableG, tableB);
```

```js returns="ColorFilter | null"
ColorFilter.MakeTableARGB(tableA, tableR, tableG, tableB);
```

One 256-entry table per channel. Pass `null` for a channel to leave it untouched; passing `null` for all four builds nothing (`null` from the static, a `TypeError` from the constructor).

```js
const invert = Array.from({ length: 256 }, (_, i) => 255 - i);

ctx.colorFilter = new ColorFilter("table-argb", null, invert, invert, invert);
ctx.fillStyle = "rgba(200, 100, 50, 0.25)";
ctx.fillRect(0, 0, 64, 64); // → r 56, g 155, b 203, alpha byte still 64
```

---

## Methods

### `delete()`

```js returns="void"
delete();
```

Marks the filter as unusable. Drawing through a deleted filter, or passing one to `"compose"` or `"lerp"`, throws an `Error`. Calling it is optional — filters are garbage-collected like any other object.

---

## `ColorMatrix`

The module also exports a `ColorMatrix` object of helpers that build the 20-element arrays the [`"matrix"`][cf_matrix] and [`"hsla-matrix"`][cf_hsla] kinds take.

| Method                                                   | Result                                                                     |
| -------------------------------------------------------- | -------------------------------------------------------------------------- |
| `identity()`                                              | The no-op matrix                                                           |
| `scaled(redScale, greenScale, blueScale, alphaScale)`     | Per-channel scale, `1` leaving a channel unchanged                         |
| `rotated(axis, sine, cosine)`                             | Hue rotation around an axis — `0` red, `1` green, `2` blue                  |
| `concat(outer, inner)`                                    | A matrix applying `inner` then `outer`                                     |
| `postTranslate(m, dr, dg, db, da)`                        | Adds a per-channel offset **in place** and returns the same array           |

```js
const grade = ColorMatrix.concat(
  ColorMatrix.scaled(1.1, 1, 0.9, 1),
  ColorMatrix.rotated(0, Math.sin(0.3), Math.cos(0.3)),
);

ctx.colorFilter = new ColorFilter("matrix", grade);
```

<!-- references_begin -->

[cf_matrix]: #matrix
[cf_hsla]: #hsla-matrix
[cf_lighting]: #lighting
[cf_luma]: #luma
[cf_blend]: #blend
[cf_compose]: #compose
[cf_lerp]: #lerp
[cf_srgb_linear]: #srgb-to-linear-gamma
[cf_linear_srgb]: #linear-to-srgb-gamma
[cf_table]: #table
[cf_table_argb]: #table-argb
[cf_delete]: #delete
[colormatrix]: #colormatrix
[context]: context.md
[gco]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/globalCompositeOperation
[save]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/save
[restore]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/restore
[drawImage]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/drawImage
[putImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/putImageData
[drawcanvas]: context.md#drawcanvas

<!-- references_end -->
