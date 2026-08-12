# `meo-skia-canvas`

[![npm](https://img.shields.io/npm/v/meo-skia-canvas.svg)](https://www.npmjs.com/package/meo-skia-canvas)
[![crates.io](https://img.shields.io/crates/v/meo-skia-canvas.svg)](https://crates.io/crates/meo-skia-canvas)
[![CI](https://img.shields.io/github/actions/workflow/status/l7aromeo/meo-skia-canvas/ci.yml?branch=main&label=ci)](https://github.com/l7aromeo/meo-skia-canvas/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

The HTML Canvas 2D [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API), off-screen and
on-screen, on Google's [Skia] engine — so output matches Chrome's
[`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) closely, while doing a number of
things the browser's canvas cannot.

**One library, two surfaces.** The same source tree ships a Rust crate and a Node addon, and they are
the same API seen twice: same method names, same argument order, same state model, one implementation
of the colour parser and the font stack underneath. Two things remain JavaScript-only — opening a
window, and writing a gradient stop as a CSS string.

> A fork of [phyrondev/phyron-skia-canvas], itself a fork of [samizdatco/skia-canvas].
> Nearly all of the code is theirs. See [Acknowledgements](#acknowledgements).

## Contents

[Quick start](#quick-start) · [What it does](#what-it-does) · [Colour and precision](#colour-and-precision) · [Performance and memory](#performance-and-memory) · [Examples](#examples) · [Platform support](#platform-support) · [Documentation](#documentation) · [What this fork changes](#what-this-fork-changes)

## Quick start

### Node.js

Requires Node 22 or newer.

```bash
npm install meo-skia-canvas
```

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(800, 600),
  ctx = canvas.getContext("2d");

ctx.fillStyle = "#1e293b";
ctx.fillRect(0, 0, 800, 600);

ctx.font = "600 72px Helvetica";
ctx.fillStyle = "#f8fafc";
ctx.textWrap = true;
ctx.fillText("Hello", 60, 140, 680);

await canvas.toFile("out.png"); // or .pdf, .svg, .jpg, .webp
```

No `trustedDependencies` entry and no `--ignore-scripts` exception is needed — see
[What this fork changes](#what-this-fork-changes).

### Rust

Requires Rust 1.85 or newer.

```toml
[dependencies]
meo-skia-canvas = { version = "0.4", default-features = false, features = ["vulkan", "freetype"] }
```

```rust
use meo_skia_canvas::prelude::*;

let mut canvas = Canvas::new(800.0, 600.0);

{
    let ctx = canvas.context();
    ctx.set_fill_style_css("#1e293b")?;
    ctx.fill_rect(0.0, 0.0, 800.0, 600.0);

    let mut font = Font::new("Helvetica", 72.0);
    font.weight = 600;
    ctx.set_font(&font);
    ctx.set_fill_style_css("#f8fafc")?;
    ctx.fill_text("Hello", 60.0, 140.0, Some(680.0));
}

canvas.to_file("out.png", &EncodeOptions::default())?;
```

The crate is a consumer API, not a byproduct of building the addon. Every public type is reachable
as `meo_skia_canvas::Thing` — the modules group them by subject for reading, but one drawing reaches
across several and nothing should require knowing which — and the `prelude` globs the same set. The
Node binding stays behind an internal module, and no signature on the drawing surface hands you a
`skia_safe` or `neon` type.

Reference: [`docs/api/native-rust.md`](docs/api/native-rust.md). Runnable code: [`examples/`](examples).

#### Cargo features

| Feature | Notes |
|---|---|
| `vulkan` | Vulkan backend (Linux / Windows). |
| `metal` | Metal backend (macOS). |
| `window` | `winit`-backed event loop. |
| `freetype` | Bundle FreeType + WOFF2 (recommended on minimal containers). |
| `node-addon` | Register the `#[neon::main]` Node addon entry point. Pure-Rust consumers leave this off. |

The default feature set is empty; opt in to the backend you need.

#### Skia version

| `meo-skia-canvas` | `skia-safe` | Skia milestone |
|---|---|---|
| `0.4.x` | `0.99.x` | [M150](https://skia.googlesource.com/skia/+/refs/heads/chrome/m150/RELEASE_NOTES.md) |
| `0.3.x` | `0.99.x` | [M150](https://skia.googlesource.com/skia/+/refs/heads/chrome/m150/RELEASE_NOTES.md) |
| `0.2.x` | `0.97.x` | [M148](https://skia.googlesource.com/skia/+/refs/heads/chrome/m148/RELEASE_NOTES.md) |

The Skia revision is pinned by `skia-safe`; bumping it is a minor-version event for this crate.

## What it does

Everything a browser canvas does, and then:

- **Vector and bitmap output** — PDF and SVG alongside PNG, JPEG, WebP and raw pixel buffers.
- **Multi-page documents** — [`newPage()`](docs/api/canvas.md) builds a canvas up as pages, written
  out as one multi-page PDF or an image sequence.
- **GUI windows** with a browser-like event framework ([`Window`](docs/api/window.md),
  [`App`](docs/api/app.md)), not just headless rendering.
- **Threaded rendering and I/O** — a worker pool handles asynchronous export off the main thread.
- **Path geometry** — boolean operations, plus
  [`simplify`, `round`, `trim`, `jitter`, `points`, `interpolate`](docs/api/path2d.md) on any
  `Path2D`.
- **3D perspective** via `createProjection()`, on top of the usual affine transforms.
- **Vector textures** (`createTexture()`) as a fill style, and custom line-dash markers.
- **A canvas drawn onto a canvas is replayed, not resampled.** `drawCanvas` re-rasterizes the source
  recording at the destination scale, so scaling one up has no resampling artifacts to speak of —
  where a browser would rasterize the source first and then filter the pixels.
- **The full CSS filter set** — blur, drop-shadow, hue-rotate, and the rest — plus CanvasKit's
  `ColorFilter`, `ImageFilter`, `MaskFilter`, `Shader` and `ColorMatrix`.
- **Typography** — word-wrapped multi-line text, per-line metrics, variable-font axes, OpenType
  features through `font-variant`, letter/word spacing, and fonts loaded from disk or memory.
- **`ParagraphBuilder`/`Paragraph`** — rich text with mixed styles, per-run shadows, hit-testing and
  line metrics.

## Colour and precision

A canvas composites in the space you name and exports in it, rather than compositing in sRGB and
converting at the end:

```js
let canvas = new Canvas(1920, 1080, {
  colorType: "RGBAF16", // case-sensitive; an unrecognized name silently means RGBA8888
  colorSpace: "display-p3", // or rec2020, srgb-linear, rec2020-pq, ...
});
```

Fifteen names across eight spaces — sRGB, Display P3, Rec. 2020, HDR10 (PQ), HLG and linear variants
— on both surfaces. `getImageData` reads back in any of them, and the CSS Color 4 functions parse:
`lab()`, `lch()`, `oklab()`, `oklch()`, `hwb()` and `color(<space> r g b / a)`. The same sRGB red,
read four ways: `srgb` 255,0,0 · `display-p3` 234,51,35 · `rec2020` 210,84,46 · `rec2020-pq`
136,83,56.

**A float `colorType` composites in float**, not merely reads back in it. Sixty fills at 0.6% alpha
land on `0.30308` (`RGBAF32`) and `0.30298` (`RGBAF16`) against an arithmetic answer of `0.30308`.
At eight bits every layer rounds to a whole level and the error compounds: `0.23922` on the CPU, and
`0.36078` on the GPU, which misses in the other direction. It costs twice the memory for `RGBAF16`
and four times for `RGBAF32`; the time cost depends entirely on what you draw, and is measured under
[Performance and memory](#performance-and-memory). Such a canvas renders on the raster backend
whatever `gpu` says, because no GPU backend Skia ships today composites in float accurately, and
`canvas.engine` reports which engine took it.

The `rec2020-pq` and `rec2020-hlg` spaces build a canvas with that transfer function and tag exports
with it, which is what a Rec. 2020 pipeline wants. They do not carry HDR *values*: a colour still
clamps at 1.0 on the way in, and none of the formats Skia encodes here — PNG, JPEG, WebP — is an HDR
container.

## Performance and memory

`just bench` runs [`examples/node/benchmark.js`](examples/node/benchmark.js) and prints these. It
builds the release binary first on purpose — a dev build leaves the Rust glue unoptimized, which
moves per-call overhead without touching Skia. Figures below are one machine, an Apple M4 Pro on
Metal, at 1200×900. **Treat the ratios as the transferable part and the milliseconds as local
colour.**

| mixed vector scene | |
|---|---|
| `RGBA8888` GPU | 9.8 ms |
| `RGBA8888` CPU | 24.8 ms — 2.5× the GPU |

300 bezier strokes, 60 shadowed rounded panels, 40 lines of text.

**What a float canvas costs in time depends entirely on what you draw**, and it runs in both
directions — which is why there is no single multiplier here:

| workload | `RGBA8888` | `RGBAF16` | `RGBAF32` |
|---|---|---|---|
| mixed vector scene | 24.7 ms | 1.29× | 1.46× |
| 120 translucent layers | 99.6 ms | **0.74×** | **0.77×** |
| 120 opaque fills | 6.6 ms | 1.29× | **7.58×** |

Blending translucent layers is *faster* in float: an eight-bit surface converts through its transfer
function on every layer and a float one does not, which more than pays for the wider pixel. Opaque
fills go the other way, and `RGBAF32` in particular falls off a cliff rather than scaling with its
byte count — 7.6× for 4× the bytes. `RGBAF16` stays close to its memory cost throughout, which makes
it the one to reach for unless you specifically need 32-bit precision.

| encode a drawn page | |
|---|---|
| SVG | 8.6 ms |
| JPEG (q 0.92) | 13.2 ms |
| PDF | 27.8 ms |
| PNG | 54.7 ms |
| WebP (q 0.9) | 70.6 ms |

| resident memory per canvas | measured | surface alone |
|---|---|---|
| `RGBA8888` | 3.74 MB | 4.12 MB |
| `RGBAF16` | 8.28 MB | 8.24 MB |
| `RGBAF32` | 16.51 MB | 16.48 MB |

Memory is the one figure that is simply arithmetic — 4, 8 and 16 bytes a pixel, and the measurement
lands within about 1% of it. RSS undercounts the eight-bit case because not every page is resident
when it is read.

Two caveats worth stating plainly. **The release build changes less than you would expect** — against
a dev binary the GPU scene went 12.3 ms → 9.8 ms, while translucent blending (99.0 → 99.6) and every
encode were unmoved. The work is inside Skia, compiled optimized either way; the profile only affects
the Rust at the boundary. And **the GPU row is the least reproducible**: it moved between 7.9 and
9.8 ms across runs where the CPU rows held to a tenth of a millisecond.

## Examples

Two runnable scripts in [`examples/node`](examples/node). The images below are their actual output
and `just examples` redraws them, so they cannot drift from what the library does. Both pin
`{gpu: false}`, so the files are reproducible on any machine rather than reflecting whichever
renderer the last person to regenerate them happened to have.

### [`report-card.js`](examples/node/report-card.js)

The sort of composition a report generator produces: gradients, a conic-gradient logo drawn on its
own canvas, rounded panels with shadows, a `MaskFilter` glow on the tallest bar, a noise `Shader`
background, a `Path2D.round()` trend line, and a wrapping `Paragraph` with a styled run. It exports
the same drawing to PNG, JPEG, WebP, PDF and SVG, and writes a three-page PDF through `newPage()`.

![report card](docs/assets/examples/report@2x.png)

### [`feature-sheet.js`](examples/node/feature-sheet.js)

Test cards, one labelled panel per feature area — the shape of thing worth checking by eye after a
change that could move pixels, since a diff against a previous build only proves nothing *changed*.

![typography](docs/assets/examples/typography@2x.png)

![images and pixels](docs/assets/examples/images@2x.png)

![effects and paths](docs/assets/examples/effects@2x.png)

## Platform support

Prebuilt binaries are published for Linux (x64/arm64, glibc and musl), macOS (arm64) and Windows
(x64/arm64). The Linux floors are measured on the released artifacts rather than assumed:

| | glibc | |
|---|---|---|
| RHEL / Rocky / Alma 8 | 2.28 | supported to 2029 |
| Ubuntu 20.04, Debian 11 | 2.31 | |
| AWS Lambda / Amazon Linux 2023 | 2.34 | supported to 2028 |
| RHEL / Rocky / Alma 9 | 2.34 | supported to 2032 |

There are two floors, not one: the module links `libstdc++` as well, and a symbol newer than the
target's fails to load exactly like a glibc one. The build asserts both ceilings on every Linux
artifact — glibc `2.34`, `GLIBCXX` `3.4.25` — which is what makes the table above a commitment
rather than a description.

## Documentation

| | |
|---|---|
| [Getting started](docs/getting-started.md) | Install and first render. |
| [Node API](docs/node.md) | Platform notes, JavaScript API, benchmarks. |
| [API reference](docs/api/index.md) | [Canvas](docs/api/canvas.md) · [Context](docs/api/context.md) · [Path2D](docs/api/path2d.md) · [Image](docs/api/image.md) · [ImageData](docs/api/imagedata.md) · [FontLibrary](docs/api/font-library.md) · [Window](docs/api/window.md) · [App](docs/api/app.md) |
| [Native Rust API](docs/api/native-rust.md) | The crate surface. |
| [Changelog](CHANGELOG.md) | Both release channels. |

## What this fork changes

**How the native binary reaches you.** It is published as one npm package per target, selected by
`os`/`cpu`/`libc`, rather than fetched by an install script. Install scripts are blocked by bun
unless the package appears in the consuming project's `trustedDependencies` — a list that is not
inherited from dependencies — and by `--ignore-scripts` everywhere else. The download remains as a
fallback.

**The Rust crate is a first-class surface.** `Canvas` and `Context2D` mirror the JavaScript API
rather than exposing whatever the binding happened to need, colour strings and font queries go
through the same implementation on both sides, and the parallel render-target layer that nothing
reached is gone.

**Two GPU faults that predate this fork.** Every thread dlopened the Vulkan loader and the last
`Arc` to drop closed it, so the idle watcher could unload it under a thread still opening it —
a segfault in about half of thirteen runs. Every thread also built its own `VkInstance` and
`VkDevice`, so a `vkDestroyDevice` at thread exit could deadlock against another thread mid-submit
inside NVIDIA's process-global locks. One loader and one device are now shared for the life of the
process, with a queue per thread.

**Metal exports drain an autorelease pool**, which they previously did not — `toBuffer`/`toFile` hand
work to a `rayon` pool whose workers have none, so Objective-C allocations accumulated for the life
of the process. Open upstream as phyrondev#30, with the packaging change as phyrondev#29.

Beyond that, this fork carries correctness fixes to inherited code — the Linux ABI floors above, a
set of rendering regressions introduced during phyron's `skia-safe` migration, and a long list of
calls that typechecked and then did nothing. The [changelog](CHANGELOG.md) records each with the
measurement that identified it.

[Skia]: https://skia.org
[samizdatco/skia-canvas]: https://github.com/samizdatco/skia-canvas
[phyrondev/phyron-skia-canvas]: https://github.com/phyrondev/phyron-skia-canvas

## Acknowledgements

Built on [`rust-skia`](https://github.com/rust-skia/rust-skia) (`skia-safe` + `skia-bindings`).

Forked from [phyrondev/phyron-skia-canvas], itself a fork of [samizdatco/skia-canvas]. Nearly all of
the code here is theirs; thanks to the contributors of
[both](https://github.com/samizdatco/skia-canvas/graphs/contributors)
[projects](https://github.com/phyrondev/phyron-skia-canvas/graphs/contributors).

## License

MIT. See [`LICENSE`](LICENSE).

© 2020–2026 Samizdat Drafting Co., Phyron AB and contributors.
© 2026 L A Romeo, for changes made in this fork.
