---
description: Procedural noise usable as a fill or stroke style
---

# Shader

> A `Shader` computes a color per pixel instead of holding a single one, so it can be assigned to a context's `fillStyle` or `strokeStyle` in place of a color string. This class currently covers the two procedural-noise functions; the gradient shaders are reached through [`createLinearGradient()`][createLinearGradient], [`createRadialGradient()`][createRadialGradient] and [`createConicGradient()`][createConicGradient] instead. 🧪

| Kinds                              | Construction                | Methods              |
| ---------------------------------- | --------------------------- | -------------------- |
| [`"fractal-noise"`][sh_fractal]    | [new Shader()][sh_new]      | [delete()][sh_delete] |
| [`"turbulence"`][sh_turbulence]    | [MakeFractalNoise()][sh_fractal] |                 |
|                                    | [MakeTurbulence()][sh_turbulence] |                |

## Creating shaders

```js returns="Shader"
new Shader(kind, baseFreqX, baseFreqY, octaves, seed);
```

```js returns="Shader | null"
Shader.MakeFractalNoise(baseFreqX, baseFreqY, octaves, seed);
Shader.MakeTurbulence(baseFreqX, baseFreqY, octaves, seed);
```

`kind` is `"fractal-noise"` or `"turbulence"`. Both take the same four arguments and differ only in how the octaves are summed. The constructor throws a `TypeError` for an unknown kind or for arguments Skia will not accept; the statics return `null` in the second case.

```js
Shader.MakeFractalNoise(-1, -1, 4, 0); // → null
new Shader("fractal-noise", -1, -1, 4, 0); // → TypeError
new Shader("perlin", 0.1, 0.1, 4, 0); // → TypeError from both
```

A shader is assigned like a color, and stays in effect until `fillStyle` is set to something else:

```js
ctx.fillStyle = new Shader("fractal-noise", 0.02, 0.02, 4, 0);
ctx.fillRect(0, 0, 200, 200);
```

Reading the property back returns the same `Shader` object you assigned.

#### baseFreqX & baseFreqY

The noise frequency along each axis, in cycles per pixel. Small values produce large, smooth features; values approaching `1` produce per-pixel static. Setting the two axes differently stretches the noise.

#### octaves

How many successively finer layers of noise to sum. Each additional octave doubles the frequency and adds detail, at a proportional cost.

#### seed

Chooses which pattern is generated. The same seed gives the same output every run, so noise is reproducible across processes.

---

## Kinds

### `"fractal-noise"`

Signed Perlin noise summed across the octaves. The result is soft and cloud-like, with values distributed around a mid-point — the usual choice for film grain, paper texture and organic gradients.

Because the noise covers the full RGBA range, alpha varies too. To use it as a texture over existing artwork, composite it at a low `globalAlpha`:

```js
ctx.fillStyle = "steelblue";
ctx.fillRect(0, 0, 200, 200);

ctx.globalAlpha = 0.15;
ctx.globalCompositeOperation = "overlay";
ctx.fillStyle = new Shader("fractal-noise", 0.9, 0.9, 1, 0);
ctx.fillRect(0, 0, 200, 200);
```

### `"turbulence"`

The same Perlin noise with the absolute value taken before summing. Folding the negative lobe back over zero creates creases where the signal crosses it, giving a sharper, more chaotic result — smoke, flame and marble rather than cloud.

```js
ctx.fillStyle = new Shader("turbulence", 0.08, 0.08, 4, 0);
ctx.fillRect(0, 0, 200, 200);
```

---

## Methods

### `delete()`

```js returns="void"
delete();
```

Marks the shader as unusable. Drawing with a deleted shader throws an `Error`. Calling it is optional — shaders are garbage-collected like any other object.

<!-- references_begin -->

[sh_fractal]: #fractal-noise
[sh_turbulence]: #turbulence
[sh_new]: #creating-shaders
[sh_delete]: #delete
[createLinearGradient]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createLinearGradient
[createRadialGradient]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createRadialGradient
[createConicGradient]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createConicGradient

<!-- references_end -->
