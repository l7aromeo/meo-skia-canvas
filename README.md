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

[Capabilities](#capabilities) · [Rust](#rust) · [Node.js](#nodejs) · [Platform support](#platform-support) · [Documentation](#documentation) · [What this fork changes](#what-this-fork-changes)

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

- **`F16`/`F32` pixel formats** for HDR compositing.
- **Extended color spaces** — Display P3, Rec.2020, HDR10 (PQ), HLG, and linear variants.
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

The stable API is the crate root, re-exported through `meo_skia_canvas::prelude`. Public signatures
never expose `skia_safe` or `neon` types — a compile-time pin in
`tests/native_studio_renderer_adapter.rs` enforces that, and the Node binding stays behind an
internal module.

```rust
use meo_skia_canvas::prelude::*;

let backend = Backend::new();
let mut surface = backend.create_surface(
    1920,
    1080,
    SurfaceOptions {
        color_space: LinearColorSpace::DisplayP3,
        ..SurfaceOptions::default()
    },
)?;

surface.with_canvas(|canvas| {
    canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
    canvas.draw_rect(
        Rect::from_xywh(100.0, 100.0, 200.0, 100.0),
        &Paint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
    );
});

let frame = surface.read_pixels()?; // tight RGBA8, sRGB-gamma, unpremultiplied
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

HDR and wide-gamut output, using the formats phyron added:

```js
let canvas = new Canvas(1920, 1080, {
  colorType: "RGBAF16", // case-sensitive; an unrecognized name silently means RGBA8888
  colorSpace: "rec2020-pq", // HDR10
});
```

[`docs/node.md`](docs/node.md) covers installation, Docker, AWS Lambda, Next.js, the JavaScript API
and benchmarks.

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

**Metal exports drain an autorelease pool**, which they previously did not — `toBuffer`/`saveAs` hand
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
