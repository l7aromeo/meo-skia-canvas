---
description: Styled coverage blurs for glows, halos, and inner shadows
---

# MaskFilter

> A `MaskFilter` blurs the coverage mask a shape produces rather than the pixels it paints. Because the blur happens before the paint is applied, the `style` argument can keep, discard or invert the original shape — which is what turns one blur into glows, halos, inner shadows and feathered fills. Assign one to a [context][context]'s `.maskFilter` property and it applies to subsequent fills, strokes, text and [`drawImage()`][drawImage] calls until you set the property back to `null`. [`drawCanvas()`][drawcanvas] and [`putImageData()`][putImageData] bypass it. The value is part of the saved state, so [`save()`][save] and [`restore()`][restore] bracket it like any other context setting. 🧪

| Styles                  | Construction           | Methods                |
| ----------------------- | ---------------------- | ---------------------- |
| [`"normal"`][mf_normal] | [new MaskFilter()][mf_new] | [delete()][mf_delete] |
| [`"solid"`][mf_solid]   | [MakeBlur()][mf_new]   |                        |
| [`"outer"`][mf_outer]   |                        |                        |
| [`"inner"`][mf_inner]   |                        |                        |

## Creating mask filters

Blur is the only mask filter Skia offers, so unlike [`ColorFilter`][colorfilter] and [`ImageFilter`][imagefilter] there is no kind argument — the first argument is the blur style:

```js returns="MaskFilter"
new MaskFilter(style, sigma, (respectCTM = true));
```

```js returns="MaskFilter | null"
MaskFilter.MakeBlur(style, sigma, (respectCTM = true));
```

The two forms differ only in how they report failure. `MakeBlur` returns `null` for a `sigma` Skia will not accept — anything at or below `0` — while the constructor throws a `TypeError` naming the value it was given. A `style` outside the four names, or a `sigma` that is not a number, throws from both.

```js
MaskFilter.MakeBlur("normal", 0); // → null
new MaskFilter("normal", 0); // → TypeError
new MaskFilter("bogus", 4); // → TypeError from both
```

### style

The four styles all use the same Gaussian blur and differ in what they keep:

| Style      | Result                                                                       |
| ---------- | ---------------------------------------------------------------------------- |
| `"normal"` | Blurs inward and outward, softening the edge in both directions               |
| `"solid"`  | Keeps the shape fully opaque and adds the blur outside it — a glow            |
| `"outer"`  | Keeps only the blur outside the shape; everything inside it goes to zero alpha |
| `"inner"`  | Keeps only the blur inside the shape, fading in from the edges                |

`"solid"` gives a glow in one pass, but the halo has to be the same color as the shape. Drawing `"outer"` under an unfiltered copy costs a second pass and lets the two differ:

```js
ctx.maskFilter = new MaskFilter("outer", 12);
ctx.fillStyle = "#39f";
ctx.font = "bold 48px Helvetica";
ctx.fillText("glow", 20, 110);

ctx.maskFilter = null;
ctx.fillStyle = "white";
ctx.fillText("glow", 20, 110);
```

### sigma

The blur's standard deviation in pixels. It must be greater than `0`.

### respectCTM

By default the blur scales with the current transform, so a shape drawn under `ctx.scale(4, 4)` gets a blur four times as wide. Pass `false` to keep the blur fixed in device pixels regardless of the transform — useful when the blur is a UI effect rather than part of the artwork.

---

## Methods

### `delete()`

```js returns="void"
delete();
```

Marks the filter as unusable. Drawing through a deleted filter throws an `Error`. Calling it is optional — filters are garbage-collected like any other object.

<!-- references_begin -->

[mf_normal]: #style
[mf_solid]: #style
[mf_outer]: #style
[mf_inner]: #style
[mf_new]: #creating-mask-filters
[mf_delete]: #delete
[colorfilter]: color-filter.md
[imagefilter]: image-filter.md
[context]: context.md
[drawcanvas]: context.md#drawcanvas
[drawImage]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/drawImage
[putImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/putImageData
[save]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/save
[restore]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/restore

<!-- references_end -->
