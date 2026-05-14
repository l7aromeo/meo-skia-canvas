# `skia-canvas`

[![crates.io](https://img.shields.io/crates/v/skia-canvas.svg)](https://crates.io/crates/skia-canvas)
[![docs.rs](https://img.shields.io/docsrs/skia-canvas)](https://docs.rs/skia-canvas)
[![CI](https://img.shields.io/github/actions/workflow/status/phyrondev/phyron-skia-canvas/bp-rust-ci.yml?branch=main&label=ci)](https://github.com/phyrondev/phyron-skia-canvas/actions/workflows/bp-rust-ci.yml)
[![License: MIT](https://img.shields.io/crates/l/skia-canvas.svg)](#license)

GPU-accelerated, multi-threaded HTML Canvas-compatible 2D rendering for **Rust** and **Node.js**, powered by [Skia].

A fork of [samizdatco/skia-canvas] that adds:

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

## Rust

```toml
[dependencies]
skia-canvas = { version = "0.1", default-features = false, features = ["vulkan", "freetype"] }
```

The stable Rust API lives under `skia_canvas::native`. Public signatures never expose `skia_safe` or `neon` types -- a compile-time pin in `tests/native_studio_renderer_adapter.rs` enforces this.

```rust
use skia_canvas::native::{
    LinearColorSpace, NativeBackend, NativePaint, Rect, RgbaLinear, SurfaceOptions,
};

let backend = NativeBackend::new();
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
        &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
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
| `0.1.x` | `0.97.x` | [M148](https://skia.googlesource.com/skia/+/refs/heads/chrome/m148/RELEASE_NOTES.md) |

The Skia revision is pinned by `skia-safe`; bumping `skia-safe` is a `skia-canvas` minor-version event.

## Node.js

The same source tree also produces the [`phyron-skia-canvas`](https://www.npmjs.com/package/phyron-skia-canvas) npm package.

```bash
npm install phyron-skia-canvas
```

```js
import { Canvas } from "phyron-skia-canvas";

let canvas = new Canvas(1920, 1080, {
  colorType: "rgbaf16",
  colorSpace: "rec2020-pq", // HDR10
});
```

See [`docs/node.md`](docs/node.md) for installation, platform support (Linux / Docker / AWS Lambda / Next.js), the JavaScript API, and benchmarks.

## Acknowledgements

Built on top of the [`rust-skia`](https://github.com/rust-skia/rust-skia) project (`skia-safe` + `skia-bindings`). Forked from [samizdatco/skia-canvas]; many thanks to its [contributors](https://github.com/samizdatco/skia-canvas/graphs/contributors).

## License

MIT. See [`LICENSE`](LICENSE).

© 2020–2026 Samizdat Drafting Co., Phyron AB and contributors.
