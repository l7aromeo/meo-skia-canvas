# `meo-skia-canvas`

[![npm](https://img.shields.io/npm/v/meo-skia-canvas.svg)](https://www.npmjs.com/package/meo-skia-canvas)
[![crates.io](https://img.shields.io/crates/v/meo-skia-canvas.svg)](https://crates.io/crates/meo-skia-canvas)
[![CI](https://img.shields.io/github/actions/workflow/status/l7aromeo/meo-skia-canvas/ci.yml?branch=main&label=ci)](https://github.com/l7aromeo/meo-skia-canvas/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

An implementation of the HTML Canvas 2D [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API)
for off-screen and on-screen rendering, built on Google's [Skia] engine — so output matches Chrome's
[`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) closely, while doing several things
the browser's canvas cannot.

The same source tree ships to **two registries**: a Rust crate and a Node addon.

> A fork of [phyrondev/phyron-skia-canvas], itself a fork of [samizdatco/skia-canvas].
> Nearly all of the code is theirs. See [Acknowledgements](#acknowledgements).

## Contents

[Capabilities](#capabilities) · [Rust](#rust) · [Node.js](#nodejs) · [Examples](#examples) · [Platform support](#platform-support) · [Documentation](#documentation) · [What this fork changes](#what-this-fork-changes)

## Capabilities

Inherited from `skia-canvas`, and all present here:

- **Vector and bitmap output** — PDF and SVG alongside PNG, JPEG, WEBP and raw pixel buffers.
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
- **The full CSS filter set** — blur, drop-shadow, hue-rotate, and the rest.
- **Typography** — word-wrapped multi-line text, per-line metrics, variable-font axes, OpenType
  features through `font-variant`, letter/word spacing, and fonts loaded from disk or memory.

Added by `phyron-skia-canvas`:

- **`F16`/`F32` pixel formats** for readbacks, exports, and compositing: a float canvas blends
  without rounding to whole eight-bit levels at every layer. Sixty fills at 0.6% alpha land on 0.303
  against 0.239 at eight bits, where 0.303 is right — at about 1.4x the time and twice the memory
  (`F16`), or 1.5x and four times (`F32`). A float canvas renders on the raster backend whatever
  `gpu` says, because no GPU backend Skia has today composites in float accurately; `canvas.engine`
  reports which engine took it.
- **Extended color spaces** — Display P3, Rec.2020, HDR10 (PQ), HLG, and linear variants, on
  both the JavaScript and the Rust surface.
- **OkLab gradient interpolation**, plus OkLCH, Lab, LCH, HSL and HWB.
- **CanvasKit filter parity** — `ColorFilter`, `ImageFilter`, `MaskFilter`, `Shader`, `ColorMatrix`.
- **`ParagraphBuilder`/`Paragraph`** — rich text with mixed styles, per-run shadows, hit-testing and
  line metrics.
- **A native Rust API**, so the crate is usable without going through the Node binding.

## Rust

```toml
[dependencies]
meo-skia-canvas = { version = "0.3", default-features = false, features = ["vulkan", "freetype"] }
```

Requires Rust 1.85 or newer.

The stable API is the crate root: every public type is reachable as `meo_skia_canvas::Thing`, with
the modules grouping them by subject and `prelude` globbing the lot. Public signatures never expose
`skia_safe` or `neon` types — CI greps for it, and the Node binding stays behind an internal module.

```rust
use meo_skia_canvas::prelude::*;

let mut canvas = Canvas::with_options(
    1920.0,
    1080.0,
    CanvasOptions {
        color_space: PixelColorSpace::DisplayP3,
        ..CanvasOptions::default()
    },
)?;

{
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    ctx.fill_rect(100.0, 100.0, 200.0, 100.0);
}

canvas.to_file("out.png", &EncodeOptions::default())?;

// Or the raw pixels: Display P3 here, because that is what this canvas
// composites in and an export keeps unless asked otherwise.
let frame = canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
```

Reference: [`docs/api/native-rust.md`](docs/api/native-rust.md). Runnable code: [`examples/`](examples).

### Cargo features

| Feature | Notes |
|---|---|
| `vulkan` | Vulkan backend (Linux / Windows). |
| `metal` | Metal backend (macOS). |
| `window` | `winit`-backed event loop. |
| `freetype` | Bundle FreeType + WOFF2 (recommended on minimal containers). |
| `node-addon` | Register the `#[neon::main]` Node addon entry point. Pure-Rust consumers leave this off. |

The default feature set is empty; opt in to the backend you need.

### Skia version

| `meo-skia-canvas` | `skia-safe` | Skia milestone |
|---|---|---|
| `0.3.x` | `0.99.x` | [M150](https://skia.googlesource.com/skia/+/refs/heads/chrome/m150/RELEASE_NOTES.md) |
| `0.2.x` | `0.97.x` | [M148](https://skia.googlesource.com/skia/+/refs/heads/chrome/m148/RELEASE_NOTES.md) |

The Skia revision is pinned by `skia-safe`; bumping it is a minor-version event for this crate.

## Node.js

Requires Node 22 or newer.

```bash
npm install meo-skia-canvas
```

No `trustedDependencies` entry and no `--ignore-scripts` exception is needed — see
[What this fork changes](#what-this-fork-changes).

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

Wide-gamut output — a canvas composites in the space you name and exports in it:

```js
let canvas = new Canvas(1920, 1080, {
  colorType: "RGBAF16", // case-sensitive; an unrecognized name silently means RGBA8888
  colorSpace: "display-p3", // or rec2020, srgb-linear, rec2020-pq, ...
});
```

The `rec2020-pq` and `rec2020-hlg` spaces build a canvas with that transfer function and tag exports
with it, which is what a Rec. 2020 pipeline wants. They do not carry HDR *values*: a colour still
clamps at 1.0 on the way in, and none of the formats Skia encodes here — PNG, JPEG, WebP — is an HDR
container.

[`docs/node.md`](docs/node.md) covers installation, Docker, AWS Lambda, Next.js, the JavaScript API
and benchmarks.

## Examples

Two runnable scripts in [`examples/node`](examples/node). The images below are their actual output,
and `just examples` redraws them, so they cannot drift from what the library does.

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

**Published to both registries** from one source tree, versioned independently.
[npm](https://www.npmjs.com/package/meo-skia-canvas) continues `phyron-skia-canvas`'s numbering from
`3.6.0`, so it does not line up with `skia-canvas`'s own 3.0.x releases.
[The crate](https://crates.io/crates/meo-skia-canvas) is numbered separately from `0.2.0`.

**Metal exports drain an autorelease pool**, which they previously did not — `toBuffer`/`toFile` hand
work to a `rayon` pool whose workers have none, so Objective-C allocations accumulated for the life
of the process.

Both of the above are open upstream as phyrondev#30 and phyrondev#29.

Beyond that, this fork carries correctness fixes to inherited code — the Linux ABI floors above, and
a set of rendering regressions introduced during phyron's `skia-safe` migration. The
[changelog](CHANGELOG.md) records each with the measurement that identified it.

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
