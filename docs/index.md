---
title: ""
hide_title: true
sidebar_position: -1
sidebar_label: "About"
---

<div id="hero">

![Skia Canvas](./assets/hero@2x.png)
![Skia Canvas](./assets/hero-dark@2x.png)

</div>

Skia Canvas is a Node.js implementation of the HTML Canvas drawing [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API) for both on- and off-screen rendering. Since it uses Google’s [Skia](https://skia.org) graphics engine, its output is very similar to Chrome’s [`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) element — though it's also capable of things the browser’s Canvas still can't achieve.

In particular, Skia Canvas:

- generates images in vector (PDF & SVG) as well as bitmap (JPEG, PNG, & WEBP) formats
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
import { Canvas } from "skia-canvas";

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
import { Canvas } from "skia-canvas";

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
import { Window } from "skia-canvas";

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
import { Canvas, loadImage } from "skia-canvas";

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

In these benchmarks, Skia Canvas is tested running in two modes: serial and async. When running serially, each rendering operation is awaited before continuing to the next test iteration. When running asynchronously, all the test iterations are begun at once and are executed in parallel using the library’s multi-threading support.

[See full results here…](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/index.md)

### [Startup latency](https://github.com/samizdatco/canvas-benchmarks/tree/main/tests/cold-start.js)

| Library              | Per Run   | Total Time (100 iterations)                                  |
| -------------------- | --------- | ------------------------------------------------------------ |
| _canvaskit-wasm_     | `  25 ms` | ` 2.47 s` ![ ](./assets/benchmarks.svg#cold-start_wasm)      |
| _canvas_             | `  88 ms` | ` 8.77 s` ![ ](./assets/benchmarks.svg#cold-start_canvas)    |
| _@napi-rs/canvas_    | `  69 ms` | ` 6.87 s` ![ ](./assets/benchmarks.svg#cold-start_napi)      |
| _skia-canvas_        | `  <1 ms` | `  33 ms` ![ ](./assets/benchmarks.svg#cold-start_skia-sync) |

### [Bezier curves](https://github.com/samizdatco/canvas-benchmarks/tree/main/tests/beziers.js)

| Library                                                                                                                                                | Per Run   | Total Time (20 iterations)                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------------ | --------- | ---------------------------------------------------------- |
| _canvaskit-wasm_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/beziers_wasm.png)            | ` 790 ms` | `15.81 s` ![ ](./assets/benchmarks.svg#beziers_wasm)       |
| _canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/beziers_canvas.png)                  | ` 486 ms` | ` 9.72 s` ![ ](./assets/benchmarks.svg#beziers_canvas)     |
| _@napi-rs/canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/beziers_napi.png)           | ` 230 ms` | ` 4.60 s` ![ ](./assets/benchmarks.svg#beziers_napi)       |
| _skia-canvas (serial)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/beziers_skia-sync.png) | ` 137 ms` | ` 2.74 s` ![ ](./assets/benchmarks.svg#beziers_skia-sync)  |
| _skia-canvas (async)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/beziers_skia-async.png) | `  28 ms` | ` 558 ms` ![ ](./assets/benchmarks.svg#beziers_skia-async) |

### [SVG to PNG](https://github.com/samizdatco/canvas-benchmarks/tree/main/tests/from-svg.js)

| Library                                                                                                                                                 | Per Run   | Total Time (100 iterations)                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | ----------------------------------------------------------- |
| canvaskit-wasm                                                                                                                                          | ` ————— ` | ` ————— `   *not supported*                                 |
| _canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/from-svg_canvas.png)                  | ` 122 ms` | `12.16 s` ![ ](./assets/benchmarks.svg#from-svg_canvas)     |
| _@napi-rs/canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/from-svg_napi.png)           | `  84 ms` | ` 8.42 s` ![ ](./assets/benchmarks.svg#from-svg_napi)       |
| _skia-canvas (serial)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/from-svg_skia-sync.png) | `  58 ms` | ` 5.83 s` ![ ](./assets/benchmarks.svg#from-svg_skia-sync)  |
| _skia-canvas (async)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/from-svg_skia-async.png) | `  11 ms` | ` 1.08 s` ![ ](./assets/benchmarks.svg#from-svg_skia-async) |

### [Scale/rotate images](https://github.com/samizdatco/canvas-benchmarks/tree/main/tests/image-blit.js)

| Library                                                                                                                                                   | Per Run   | Total Time (50 iterations)                                    |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | ------------------------------------------------------------- |
| _canvaskit-wasm_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/image-blit_wasm.png)            | ` 274 ms` | `13.72 s` ![ ](./assets/benchmarks.svg#image-blit_wasm)       |
| _canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/image-blit_canvas.png)                  | ` 283 ms` | `14.13 s` ![ ](./assets/benchmarks.svg#image-blit_canvas)     |
| _@napi-rs/canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/image-blit_napi.png)           | ` 112 ms` | ` 5.60 s` ![ ](./assets/benchmarks.svg#image-blit_napi)       |
| _skia-canvas (serial)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/image-blit_skia-sync.png) | ` 100 ms` | ` 5.00 s` ![ ](./assets/benchmarks.svg#image-blit_skia-sync)  |
| _skia-canvas (async)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/image-blit_skia-async.png) | `  19 ms` | ` 935 ms` ![ ](./assets/benchmarks.svg#image-blit_skia-async) |

### [Basic text](https://github.com/samizdatco/canvas-benchmarks/tree/main/tests/text.js)

| Library                                                                                                                                             | Per Run   | Total Time (200 iterations)                             |
| --------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | ------------------------------------------------------- |
| _canvaskit-wasm_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/text_wasm.png)            | `  24 ms` | ` 4.75 s` ![ ](./assets/benchmarks.svg#text_wasm)       |
| _canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/text_canvas.png)                  | `  24 ms` | ` 4.88 s` ![ ](./assets/benchmarks.svg#text_canvas)     |
| _@napi-rs/canvas_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/text_napi.png)           | `  19 ms` | ` 3.83 s` ![ ](./assets/benchmarks.svg#text_napi)       |
| _skia-canvas (serial)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/text_skia-sync.png) | `  21 ms` | ` 4.26 s` ![ ](./assets/benchmarks.svg#text_skia-sync)  |
| _skia-canvas (async)_ [👁️](https://github.com/samizdatco/canvas-benchmarks/blob/main/results/darwin-arm64/2025-09-26/snapshots/text_skia-async.png) | `   4 ms` | ` 819 ms` ![ ](./assets/benchmarks.svg#text_skia-async) |

<!-- references_begin -->

[bool-ops]: api/path2d.md#complement-difference-intersect-union-and-xor
[c2d_font]: api/context.md#font
[c2d_measuretext]: api/context.md#measuretext
[createProjection()]: api/context.md#createprojection
[createTexture()]: api/context.md#createtexture
[fontlibrary-use]: api/font-library.md#use
[fontvariant]: api/context.md#fontvariant
[lineDashMarker]: api/context.md#linedashmarker
[newPage]: api/canvas.md#newpage
[p2d_interpolate]: api/path2d.md#interpolate
[p2d_points]: api/path2d.md#points
[p2d_round]: api/path2d.md#round
[p2d_simplify]: api/path2d.md#simplify
[p2d_trim]: api/path2d.md#trim
[toFile]: api/canvas.md#tofile
[textwrap]: api/context.md#textwrap
[toBuffer]: api/canvas.md#tobuffer
[toURL]: api/canvas.md#tourl
[win_bind]: api/window.md#on--off--once
[window]: api/window.md
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
