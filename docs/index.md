---
title: ""
hide_title: true
sidebar_position: -1
sidebar_label: "About"
---

<div id="hero">

![meo-skia-canvas](./assets/brand/hero@2x.png)
![meo-skia-canvas](./assets/brand/hero-dark@2x.png)

</div>

`meo-skia-canvas` implements the HTML Canvas drawing [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API) for both on- and off-screen rendering, as a Rust crate and a Node addon built from one source tree. Since it uses Google’s [Skia](https://skia.org) graphics engine, its output is very similar to Chrome’s [`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) element — though it's also capable of things the browser’s Canvas still can't achieve.

In particular, it:

- generates images in vector (PDF & SVG) as well as bitmap (PNG, JPEG, WebP, GIF, APNG, TIFF, ICO, BMP & AVIF) formats
- animates: pages become frames in a WebP, GIF, APNG or AVIF, timed by `fps` or a per-frame `frameDelays` array. An animated image read back reports its own `frames` and `delays` — for the first three; Skia ships no AVIF decoder, so that one is write-only
- can draw to interactive GUI [windows][window] and provides a browser-like [event][win_bind] framework
- can save images to [files][toFile], encode to [dataURL][toURL] strings, and return [Buffers][toBuffer] or [Sharp][sharp] objects
- uses native threads in a [user-configurable][multithreading] worker pool for asynchronous rendering and file I/O
- can create [multiple ‘pages’][newPage] on a given canvas and then [output][toFile] them as a single, multi-page PDF or an image-sequence saved to multiple files
- can [simplify][p2d_simplify], [blunt][p2d_round], [combine][bool-ops], [excerpt][p2d_trim], and [atomize][p2d_points] Bézier paths using [efficient](https://www.youtube.com/watch?v=OmfliNQsk88) boolean operations or point-by-point [interpolation][p2d_interpolate]
- provides [3D perspective][createProjection()] transformations in addition to [scaling][scale()], [rotation][rotate()], and [translation][translate()]
- can fill shapes with vector-based [Textures][createTexture()] in addition to bitmap-based [Patterns][createPattern()] and supports line-drawing with custom [markers][lineDashMarker]
- supports the full set of [CSS filter][filter] image processing operators
- offers rich typographic control including:
  - multi-line, [word-wrapped][textwrap] text
  - line-by-line [text metrics][c2d_measuretext]
  - small-caps, ligatures, and other opentype features accessible using standard [font-variant][fontvariant] syntax
  - proportional [letter-spacing][letterSpacing], [word-spacing][wordSpacing], and [leading][c2d_font]
  - support for [variable fonts][VariableFonts] and transparent mapping of weight values
  - use of non-system fonts [loaded][fontlibrary-use] from local files
- can be used for server-side image rendering on standard Linux hosts and ‘serverless’ platforms like Vercel and AWS Lambda

## Example Usage

### Generating image files

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas;

let sweep = ctx.createConicGradient(Math.PI * 1.2, width / 2, height / 2);
sweep.addColorStop(0, "red");
sweep.addColorStop(0.25, "orange");
sweep.addColorStop(0.5, "yellow");
sweep.addColorStop(0.75, "green");
sweep.addColorStop(1, "red");
ctx.strokeStyle = sweep;
ctx.lineWidth = 100;
ctx.strokeRect(100, 100, 200, 200);

// render to multiple destinations using a background thread
async function render() {
  // save a ‘retina’ image...
  await canvas.toFile("rainbox.png", { density: 2 });
  // ...or use a shorthand for canvas.toBuffer("png")
  let pngData = await canvas.png;
  // ...or embed it in a string
  let pngEmbed = `<img src="${await canvas.toDataURL("png")}">`;
}
render();

// ...or write the file synchronously from the main thread
canvas.toFileSync("rainbox.pdf");
```

### Multi-page sequences

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas;

for (const color of ["orange", "yellow", "green", "skyblue", "purple"]) {
  ctx = canvas.newPage();
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, width, height);
  ctx.fillStyle = "white";
  ctx.arc(width / 2, height / 2, 40, 0, 2 * Math.PI);
  ctx.fill();
}

async function render() {
  // save to a multi-page PDF file
  await canvas.toFile("all-pages.pdf");

  // save to files named `page-01.png`, `page-02.png`, etc.
  await canvas.toFile("page-{2}.png");
}
render();
```

### Rendering to a window

```js
import { Window } from "meo-skia-canvas";

let win = new Window(300, 300);
win.title = "Canvas Window";
win.on("draw", (e) => {
  let ctx = e.target.canvas.getContext("2d");
  ctx.lineWidth = 25 + 25 * Math.cos(e.frame / 10);
  ctx.beginPath();
  ctx.arc(150, 150, 50, 0, 2 * Math.PI);
  ctx.stroke();

  ctx.beginPath();
  ctx.arc(150, 150, 10, 0, 2 * Math.PI);
  ctx.stroke();
  ctx.fill();
});
```

### Integrating with [Sharp.js][sharp]

```js
import sharp from "sharp";
import { Canvas, loadImage } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas,
  [x, y] = [width / 2, height / 2];

ctx.fillStyle = "red";
ctx.fillRect(0, 0, x, y);
ctx.fillStyle = "orange";
ctx.fillRect(x, y, x, y);

// Render the canvas to a Sharp object on a background thread then desaturate
await canvas
  .toSharp()
  .modulate({ saturation: 0.25 })
  .jpeg()
  .toFile("faded.jpg");

// Convert an ImageData to a Sharp object and save a grayscale version
let imgData = ctx.getImageData(0, 0, width, height, {
  matte: "white",
  density: 2,
});
await imgData.toSharp().grayscale().png().toFile("black-and-white.png");

// Create an image using Sharp then draw it to the canvas as an Image object
let sharpImage = sharp({
  create: { width: x, height: y, channels: 4, background: "skyblue" },
});
let canvasImage = await loadImage(sharpImage);
ctx.drawImage(canvasImage, x, 0);
await canvas.toFile("mosaic.png");
```

## Benchmarks

Against `canvas`, `@napi-rs/canvas`, `canvaskit-wasm` and upstream `skia-canvas`, measured on
2026-08-12 with samizdatco's [canvas-benchmarks] harness: ten drawing and export tests, each
library rendering the identical seeded scene in a fresh process.

This fork tracks upstream `skia-canvas` within measurement noise. Against the rest it is the fastest
of all five on bezier stroking, SVG export, PDF export and image scaling, and beats
`@napi-rs/canvas` on seven of the ten tests — `@napi-rs/canvas` takes the three lightest scenes, by
6–8%. Only this library and `canvas` export PDF at all.

Run asynchronously, where iterations resolve on the worker pool, it is several times faster than any
serial result here; no other library in the comparison offers that mode.

**[Full tables, method and caveats](node.md#benchmarks)** — including what the numbers do not say,
and why the startup figure upstream publishes cannot be measured by that harness.

For this library on its own — GPU against CPU, what a float `colorType` costs, encode times and
memory per canvas — run `just bench`.

[canvas-benchmarks]: https://github.com/samizdatco/canvas-benchmarks

<!-- references_begin -->

[bool-ops]: api/path2d.md#complement-difference-intersect-union-and-xor
[c2d_font]: api/context.md#font
[c2d_measuretext]: api/context.md#measuretext
[createProjection()]: api/context.md#createprojection
[createTexture()]: api/context.md#createtexture
[fontlibrary-use]: https://www.jsdocs.io/package/meo-skia-canvas
[fontvariant]: api/context.md#fontvariant
[lineDashMarker]: api/context.md#linedashmarker
[newPage]: https://www.jsdocs.io/package/meo-skia-canvas
[p2d_interpolate]: api/path2d.md#interpolate
[p2d_points]: api/path2d.md#points
[p2d_round]: api/path2d.md#round
[p2d_simplify]: api/path2d.md#simplify
[p2d_trim]: api/path2d.md#trim
[toFile]: https://www.jsdocs.io/package/meo-skia-canvas
[textwrap]: api/context.md#textwrap
[toBuffer]: https://www.jsdocs.io/package/meo-skia-canvas
[toURL]: https://www.jsdocs.io/package/meo-skia-canvas
[win_bind]: https://www.jsdocs.io/package/meo-skia-canvas
[window]: https://www.jsdocs.io/package/meo-skia-canvas
[multithreading]: getting-started.md#multithreading
[sharp]: https://sharp.pixelplumbing.com
[VariableFonts]: https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_Fonts/Variable_Fonts_Guide
[filter]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/filter
[letterSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/letterSpacing
[wordSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/wordSpacing
[createPattern()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createPattern
[rotate()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/rotate
[scale()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/scale
[translate()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/translate

<!-- references_end -->
