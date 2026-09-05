The HTML Canvas drawing API, implemented on Google's [Skia](https://skia.org)
and shipped as a Node addon. A `Canvas` here takes the same calls a
`<canvas>` element takes in a browser, and renders them with the engine
Chrome renders with — off-screen to a file or a buffer, or on-screen to a
GUI window.

This is the **reference**: every type the package ships, generated from the
declaration files it ships them in, so what an editor shows on hover and what
this page shows are the same text. The prose — installation, the guides, the
measured comparisons — is [next door](https://github.com/l7aromeo/meo-skia-canvas/tree/main/docs).

## Drawing something

```js
import { Canvas } from "meo-skia-canvas";

const canvas = new Canvas(400, 400);
const ctx = canvas.getContext("2d");

ctx.fillStyle = "#0b1220";
ctx.fillRect(0, 0, 400, 400);

ctx.strokeStyle = "#6366f1";
ctx.lineWidth = 24;
ctx.lineCap = "round";
ctx.beginPath();
ctx.moveTo(120, 280);
ctx.bezierCurveTo(184, 68, 280, 292, 344, 80);
ctx.stroke();

await canvas.toFile("curve.png");
```

Three types carry almost all of it. {@link index.Canvas | `Canvas`} owns the
dimensions, the pages and the exporters;
{@link index.CanvasRenderingContext2D | `CanvasRenderingContext2D`} is the
drawing surface and the largest type in the reference;
{@link index.Path2D | `Path2D`} is a shape you can build once and reuse, and
here it can also be measured, trimmed, simplified and combined with another.

Rendering is deferred until an export asks for it, and then runs on a
background thread — so `toFile`, `toBuffer` and `toDataURL` are the exporters
to reach for when more than one image is in flight. The `Sync` variants block
the main thread and exist for scripts that have nothing else to do.

## The two entry points

The package resolves to a different module in Node than in a bundle, and they
are not the same API.

|                         | `index`                                                                                                                      | `browser`                                       |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| resolved by             | Node, `require` or `import`                                                                                                  | a bundler, through the `"browser"` condition    |
| draws with              | Skia, in this addon                                                                                                          | the page's own `<canvas>`                       |
| encodes                 | 11 raster and vector formats                                                                                                 | {@link browser.ExportFormat PNG, JPEG and WebP} |
| async exports give      | a `Buffer`                                                                                                                   | an `ArrayBuffer`                                |
| synchronous exports     | yes                                                                                                                          | none                                            |
| windows, fonts, filters | {@link index.Window `Window`}, {@link index.FontLibrary `FontLibrary`}, {@link index.ImageFilter `ImageFilter`} and the rest | absent                                          |

`browser` is deliberately a short list rather than a second copy of the API.
Everything backed by Skia, the filesystem or an event loop has no counterpart
in a page, so it is left undeclared instead of declared and undefined — and
`Canvas` itself is narrowed, because a browser cannot encode the eleven
formats Node can or hand back a `Buffer`. Write against `index` unless you
are bundling for a page; the shared drawing calls are identical either way.

## Where to go next

- {@link index.Canvas | `Canvas`} and
  {@link index.CanvasRenderingContext2D | `CanvasRenderingContext2D`} — the
  two types most work starts from.
- {@link index.Window | `Window`} and {@link index.App | `App`} — interactive
  rendering, with a browser-shaped event framework.
- {@link index.FontLibrary | `FontLibrary`} — fonts loaded from files, and
  variable-font axes.
- {@link index.ParagraphBuilder | `ParagraphBuilder`} — multi-line text with
  per-line metrics, beyond what `fillText` reaches.
- [The guides](https://github.com/l7aromeo/meo-skia-canvas/tree/main/docs) —
  installation, GPU and threading, the extensions this fork adds.
- [The Rust reference](https://docs.rs/meo-skia-canvas) — the same engine as a
  crate; this addon is one of its two front ends.
