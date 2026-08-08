# `meo-skia-canvas`

[![npm](https://img.shields.io/npm/v/meo-skia-canvas.svg)](https://www.npmjs.com/package/meo-skia-canvas)
[![CI](https://img.shields.io/github/actions/workflow/status/l7aromeo/meo-skia-canvas/ci.yml?branch=main&label=ci)](https://github.com/l7aromeo/meo-skia-canvas/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

GPU-accelerated, multi-threaded HTML Canvas-compatible 2D rendering for **Rust** and **Node.js**, powered by [Skia].

A fork of [phyrondev/phyron-skia-canvas], itself a fork of [samizdatco/skia-canvas].

This fork changes how the native binary reaches you: it is published as one npm package per target,
selected by `os`/`cpu`/`libc`, instead of downloaded by an install script. Install scripts are
blocked by bun unless the package is listed in the consuming project's `trustedDependencies` — a
list that is not inherited from dependencies — and by `--ignore-scripts` everywhere else. The
download remains as a fallback.

Inherited from phyron:

- **`F16`/`F32` pixel formats** for HDR compositing.
- **Extended color spaces**: Display P3, Rec.2020, HDR10 (PQ), HLG, plus *linear* variants.
- **OkLab gradient interpolation** in OkLab, OkLCH, Lab, LCH, HSL, HWB.
- **CanvasKit filter parity** (`ColorFilter`, `ImageFilter`).
- **Font registration from buffers** without writing to disk.
- **Variable font axis control** (`wght`, `wdth`, `opsz`, `slnt`, custom axes).
- **Linear-light premultiplied colors** plumbed through paint, gradient, filter, text.
- **`ParagraphBuilder`/`Paragraph`** rich text with mixed styles, per-run shadows, hit-testing, line metrics.

[Skia]: https://skia.org
[samizdatco/skia-canvas]: https://github.com/samizdatco/skia-canvas
[phyrondev/phyron-skia-canvas]: https://github.com/phyrondev/phyron-skia-canvas

## Rust

This fork is not published to crates.io, so depend on it by git:

```toml
[dependencies]
skia-canvas = { git = "https://github.com/l7aromeo/meo-skia-canvas", default-features = false, features = ["vulkan", "freetype"] }
```

The stable Rust API is the crate root, re-exported through `skia_canvas::prelude`. Public signatures never expose `skia_safe` or `neon` types -- a compile-time pin in `tests/native_studio_renderer_adapter.rs` enforces this; the Node/Neon binding lives under the internal `node` module.

```rust
use skia_canvas::prelude::*;

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

See [`docs/api/native-rust.md`](docs/api/native-rust.md) for the reference and [`examples/`](examples) for runnable code.

### Cargo features

| Feature | Notes |
|---|---|
| `vulkan` | Vulkan backend (Linux / Windows). |
| `metal` | Metal backend (macOS). |
| `window` | `winit`-backed event loop. |
| `freetype` | Bundle FreeType + WOFF2 (recommended on minimal containers). |
| `node-addon` | Register the `#[neon::main]` Node addon entry point. Pure-Rust consumers leave this off. |

Default feature set is empty; opt in to the backend you need.

### Skia version

| `skia-canvas` | `skia-safe` | Skia milestone |
|---|---|---|
| `0.2.x` | `0.97.x` | [M148](https://skia.googlesource.com/skia/+/refs/heads/chrome/m148/RELEASE_NOTES.md) |

The Skia revision is pinned by `skia-safe`; bumping `skia-safe` is a `skia-canvas` minor-version event.

## Node.js

The same source tree also produces the [`meo-skia-canvas`](https://www.npmjs.com/package/meo-skia-canvas) npm package.

```bash
npm install meo-skia-canvas
```

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(1920, 1080, {
  colorType: "rgbaf16",
  colorSpace: "rec2020-pq", // HDR10
});
```

See [`docs/node.md`](docs/node.md) for installation, platform support (Linux / Docker / AWS Lambda / Next.js), the JavaScript API, and benchmarks.

## Acknowledgements

Built on top of the [`rust-skia`](https://github.com/rust-skia/rust-skia) project (`skia-safe` + `skia-bindings`).

Forked from [phyrondev/phyron-skia-canvas], which is itself a fork of [samizdatco/skia-canvas].
Nearly all of the code here is theirs; thanks to the contributors of
[both](https://github.com/samizdatco/skia-canvas/graphs/contributors)
[projects](https://github.com/phyrondev/phyron-skia-canvas/graphs/contributors).

## License

MIT. See [`LICENSE`](LICENSE).

© 2020–2026 Samizdat Drafting Co., Phyron AB and contributors.
© 2026 L A Romeo, for changes made in this fork.
