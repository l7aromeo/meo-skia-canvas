<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/brand/hero-dark%402x.png">
  <img alt="meo-skia-canvas" src="https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/brand/hero%402x.png">
</picture>

[![npm](https://img.shields.io/npm/v/meo-skia-canvas.svg)](https://www.npmjs.com/package/meo-skia-canvas)
[![crates.io](https://img.shields.io/crates/v/meo-skia-canvas.svg)](https://crates.io/crates/meo-skia-canvas)
[![docs.rs](https://img.shields.io/docsrs/meo-skia-canvas?label=docs.rs)](https://docs.rs/meo-skia-canvas)
[![jsdocs.io](https://img.shields.io/badge/jsdocs.io-reference-blue)](https://www.jsdocs.io/package/meo-skia-canvas)
[![CI](https://img.shields.io/github/actions/workflow/status/l7aromeo/meo-skia-canvas/ci.yml?branch=main&label=ci)](https://github.com/l7aromeo/meo-skia-canvas/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

The HTML Canvas 2D [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API), off-screen and
on-screen, on Google's [Skia] engine — so output matches Chrome's
[`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) closely, while doing a number of
things the browser's canvas cannot.

**One library, two surfaces.** The same source tree ships a Rust crate and a Node addon, and they are
the same API seen twice: same method names, same argument order, same state model, one implementation
of the colour parser and the font stack underneath. One thing remains JavaScript-only — writing a
gradient stop as a CSS string.

> A fork of [samizdatco/skia-canvas], by way of [phyrondev/phyron-skia-canvas], and substantially
> diverged from both. The design is theirs; see [Acknowledgements](#acknowledgements).

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

Requires Rust 1.90 or newer.

```toml
[dependencies]
meo-skia-canvas = { version = "0.6", default-features = false, features = ["vulkan", "freetype"] }
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
Node binding stays behind an internal module, and no signature anywhere in the crate hands you a
`skia_safe` or `neon` type — windowing included. `scripts/check-public-api.mjs` reads rustdoc's JSON
in CI and fails on a leak, with no module exempted, so the promise is checked rather than kept by
hand.

Reference: [`docs/rust.md`](docs/rust.md). Runnable code:
[`examples/rust`](examples/rust) — six programs, four of them line-for-line translations of their
[`examples/node`](examples/node) counterparts, so anything one surface can draw the other can too.

#### Cargo features

| Feature      | Notes                                                                                    |
| ------------ | ---------------------------------------------------------------------------------------- |
| `vulkan`     | Vulkan backend (Linux / Windows).                                                        |
| `metal`      | Metal backend (macOS).                                                                   |
| `window`     | `winit`-backed event loop.                                                               |
| `freetype`   | Bundle FreeType + WOFF2 (recommended on minimal containers).                             |
| `node-addon` | Register the `#[neon::main]` Node addon entry point. Pure-Rust consumers leave this off. |

The default feature set is empty; opt in to the backend you need.

#### Skia version

Built on `skia-safe` 0.99, which pins Skia
[M150](https://skia.googlesource.com/skia/+/refs/heads/chrome/m150/RELEASE_NOTES.md) — the branch
Chrome 150 builds from, which is what "output matches Chrome's canvas" is measured against.

The Skia revision comes from `skia-safe`; bumping it is a minor-version event for this crate, and
the [changelog](CHANGELOG.md) records which pairing each release shipped.

## What it does

Everything a browser canvas does, and then:

- **Twelve export formats** — PNG, JPEG, WebP, GIF, APNG, TIFF, ICO, BMP, AVIF, PDF, SVG and raw
  pixel buffers. Skia encodes three of them; the rest are written here.
- **The depth the drawing has** — a float canvas is written at sixteen bits a channel as a PNG,
  APNG or TIFF rather than rounded to eight on the way out, and AVIF codes 8, 10 or 12 through
  `bitDepth`. JPEG, WebP, GIF, ICO and BMP are eight-bit by definition and narrow what they are
  handed.
- **Animation** — pages are frames. WebP, GIF, APNG and AVIF take `fps` or a per-frame
  `frameDelays` array. All four send only the rectangle each frame changed; AVIF also codes frames
  _against each other_, which is why eight frames of a moving square come to 1146 bytes where a
  single still is 285.
- **An animation read back reports its own `frames` and `delays`**, so re-encoding one is a round
  trip. Skia decodes neither APNG nor AVIF — it opens an APNG as the still inside it — so this
  library demuxes both itself, `fcTL` rectangles, disposal and blending included, and reads AVIF
  end to end: tile grids, `irot`/`imir` orientation, ICC profiles, narrow-range levels, 4:2:0
  chroma.
- **AVIF has dials the others do not** — `chromaSampling` (`"4:4:4"`, `"4:2:2"`, `"4:2:0"`) and
  `lossless`. Both default to the conservative answer; both are measured in
  [Performance](#performance-and-memory).
- **An SVG says what the canvas drew** — a conic gradient, shadow, blend mode or filter is embedded
  as pixels where SVG cannot describe it rather than silently dropped; everything else stays vector.
- **Multi-page documents** — [`newPage()`](https://www.jsdocs.io/package/meo-skia-canvas#Canvas.newPage)
  builds a canvas up as pages, written as one PDF, TIFF or ICO, or as an image sequence.
  `pageRange` takes a span rather than one page or all of them — which is how an animation that
  plays an introduction once then cycles forever is written from one canvas, since a file carries
  one loop count.
- **A canvas drawn onto a canvas is replayed, not resampled.** `drawCanvas` re-rasterizes the source
  recording at the destination scale, so scaling up has no resampling artifacts — where a browser
  would rasterize first and filter after. Its compositing stays its own.
- **GUI windows** with a browser-like event framework
  ([`Window`](https://www.jsdocs.io/package/meo-skia-canvas#Window),
  [`App`](https://www.jsdocs.io/package/meo-skia-canvas#App)), from Rust as well as Node, behind the
  `window` feature.
- **Path geometry** — boolean operations plus
  [`simplify`, `round`, `trim`, `jitter`, `points`, `interpolate`](docs/api/path2d.md); 3D
  perspective via `createProjection()`; vector textures (`createTexture()`) as a fill style; custom
  line-dash markers.
- **The full CSS filter set** — blur, drop-shadow, hue-rotate and the rest — plus CanvasKit's
  `ColorFilter`, `ImageFilter`, `MaskFilter`, `Shader` and `ColorMatrix`.
- **Typography** — word-wrapped multi-line text, per-line metrics, variable-font axes, OpenType
  features through `font-variant`, letter/word spacing, fonts from disk or memory, and
  `ParagraphBuilder`/`Paragraph` for rich text with mixed styles, per-run shadows and hit-testing.
- **Threaded rendering and I/O** — a worker pool handles asynchronous export off the main thread.

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
with it, which is what a Rec. 2020 pipeline wants. They do not carry HDR _values_: a colour still
clamps at 1.0 on the way in, and none of the formats Skia encodes here — PNG, JPEG, WebP — is an HDR
container.

## Performance and memory

`just bench` runs [`examples/node/benchmark.js`](examples/node/benchmark.js) and prints all of
this. Figures are one machine — an Apple M4 Pro on Metal, 1200×900. **Treat the ratios as the
transferable part and the milliseconds as local colour.**

**Drawing.** A mixed vector scene — 300 bezier strokes, 60 shadowed rounded panels, 40 lines of
text — takes 7.1 ms on the GPU against 28.0 on the CPU. What a float canvas costs runs in both
directions, which is why there is no single multiplier:

| workload (CPU)         | `RGBA8888` | `RGBAF16` | `RGBAF32` |
| ---------------------- | ---------- | --------- | --------- |
| mixed vector scene     | 27.7 ms    | 1.32×     | 1.49×     |
| 120 translucent layers | 99.3 ms    | **0.77×** | **0.78×** |
| 120 opaque fills       | 6.8 ms     | 1.33×     | **7.43×** |

Blending translucent layers is _faster_ in float: an eight-bit surface converts through its
transfer function on every layer and a float one does not. Opaque fills go the other way, and
`RGBAF32` falls off a cliff rather than scaling with its byte count. `RGBAF16` stays close to its
memory cost throughout, which makes it the one to reach for unless you need 32-bit precision.

**Encoding one page.** JPEG 13.6 ms · BMP 26.1 · PDF 28.3 · APNG 29.1 · SVG 47.0 · PNG 55.9 ·
GIF 63.7 · WebP 71.4 · TIFF 83.1 · AVIF 235.8. Decoding: PNG 9.1 ms, AVIF 69.2 — AVIF both ways
is this library's own code, since Skia reads none of it.

AVIF is the slow one, 17× JPEG, and it buys something: 561 KB at 41.7 dB PSNR where JPEG is
802 KB at 34.9 — smaller _and_ closer to the original. WebP lands at 411 KB and 25.6 dB, which is
libwebp targeting a perceptual metric rather than PSNR on the hardest case for it, antialiased
diagonals and small type. Its own dials move both axes at once:

| AVIF option    | time     | size    |     | AVIF option               | time     | size    |
| -------------- | -------- | ------- | --- | ------------------------- | -------- | ------- |
| `quality` 0.5  | 220.2 ms | 215 KB  |     | `chromaSampling: "4:2:2"` | 205.0 ms | 443 KB  |
| `quality` 0.92 | 235.6 ms | 561 KB  |     | `chromaSampling: "4:2:0"` | 185.0 ms | 368 KB  |
| `quality` 1.0  | 269.6 ms | 2010 KB |     | `lossless: true`          | 287.5 ms | 2351 KB |

Subsampling is cheaper _and_ smaller, but on text and flat panels it costs far more quality than
it saves bytes — right for a photograph, wrong for a chart, hence the `"4:4:4"` default.

**Encoding an animation** is a different question, because the work is between frames: each
format sends only the rectangle a frame differs from its predecessor in, and compresses frames on
whatever cores are free. Thirty frames of the same page with one moving element:

| 30 frames | time     | size    |
| --------- | -------- | ------- |
| APNG      | 128.7 ms | 1960 KB |
| WebP      | 202.6 ms | 570 KB  |
| GIF       | 287.6 ms | 724 KB  |
| AVIF      | 1146 ms  | 1686 KB |

The per-frame cost is far below the single-page figures above — a still background is compressed
once, not thirty times. AVIF is the exception and stays the slowest by a distance: AV1 predicts
each frame from the one before it, so its frames genuinely cannot be coded in parallel.

**Memory** is the one figure that is simply arithmetic — 4, 8 and 16 bytes a pixel, landing within
2% of it: 4.22 MB, 8.35 MB and 16.58 MB a canvas for `RGBA8888`, `RGBAF16` and `RGBAF32`. It needs
repeating before it is believed, though; a single pass over twenty canvases reads whatever the
allocator happened to do and has come back at 2.91 MB for the eight-bit case and at a negative
number for `RGBAF32`.

**Antialiasing coverage is where the GPU and the CPU disagree**, and neither GPU path matches the
raster one. Sweeping a rectangle's width from 0.05 to 1 pixel: the CPU renderer is exact to within
a level; 4𝗑 MSAA quantizes to quarters — 0, 64, 127, 191, 255 — so a shape thinner than about an
eighth of a pixel drops out entirely; shader-based AA is smooth but reads systematically low,
putting 159 where a half-covered black edge should read 127. Total error runs 10, 307 and 427
respectively, identical on Metal and Vulkan. The default is the closer of the two GPU options; if
coverage has to match the CPU exactly, render on the CPU.

Two caveats. **Benchmark on a release build or not at all.** Most rows barely move — that work is
inside Skia and is optimized either way — but AVIF is **2810 ms on a dev build against 236 on
release**, because its pixels reach libaom through this crate's own per-pixel colour conversion,
and that is Rust. And **the GPU row is the least reproducible**, moving between 10.3 and 12.1 ms
across runs where the CPU rows held to a few tenths.

## Examples

Three runnable scripts in [`examples/node`](examples/node). The images below are their actual output
and `just examples` redraws them, so they cannot drift from what the library does. The two still
sheets pin `{gpu: false}` so their files are byte-identical between machines: the renderers
antialias differently enough that 19% of bytes differ on the same drawing, and a committed image
that changes on every regeneration is noise in every diff. The animation draws on the GPU, which is
what you would actually use, and so is not byte-reproducible across machines; `MEO_EYE_CPU=1` pins
it to the CPU, which is. Build the release binary before regenerating: a hundred and fifty frames of
APNG and a k-means palette per GIF frame take 13 seconds through `just build-release` and six
minutes through the debug build, which is the same encoders with the optimizer switched off.

Each has a Rust twin in [`examples/rust`](examples/rust) that draws the same picture —
`cargo run --example report_card`, `feature_sheet`, `animated_eye`, `benchmark`. They are the
parity test that matters: an operation the crate cannot express is one the port cannot compile, and
writing them turned up five real bugs, including gradient stops rendering far too dark and a
`rects_for_placeholders` that could only ever return empty.

### [`report-card.js`](examples/node/report-card.js)

The sort of composition a report generator produces: gradients, a conic-gradient logo drawn on its
own canvas, rounded panels with shadows, a `MaskFilter` glow on the tallest bar, a noise `Shader`
background, a `Path2D.round()` trend line, and a wrapping `Paragraph` with a styled run. It exports
the same drawing to PNG, JPEG, WebP, PDF and SVG, and writes a three-page PDF through `newPage()`.

![report card](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/report%402x.png)

### [`feature-sheet.js`](examples/node/feature-sheet.js)

Test cards, one labelled panel per feature area — the shape of thing worth checking by eye after a
change that could move pixels, since a diff against a previous build only proves nothing _changed_.

![typography](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/typography%402x.png)

![images and pixels](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/images%402x.png)

![effects and paths](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/effects%402x.png)

### [`animated-eye.js`](examples/node/animated-eye.js)

An eye that winks, written without a single keyframe. The lid, pupil, gaze, brow and all 200 lashes
are spring-dampers integrated at a fixed 240 Hz, so the motion is what the forces produce rather
than a curve someone drew. The lid spring is deliberately asymmetric — stiff closing, soft opening —
so the wink snaps shut and drifts back open past its resting point, and lid velocity draws a second
ghost copy of every lash, which is motion blur that costs nothing and shows only on the snap. Two
details are there because eyes have them and drawings usually do not: the ball rolls up as the lid
falls, and the catchlights sit on the cornea rather than the iris plane, tracking the gaze at half
speed. That parallax is most of what makes it read as a dome rather than a disc.

It leans on four things a browser canvas has no answer for: `Path2D.jitter()` for a hand-drawn edge
on every hair, `MaskFilter` for the occlusion in the socket, a Display P3 canvas for iris blues
outside sRGB, and writing the animation straight out of the canvas's own pages.

The same 150 frames are **2.7 MB as an AVIF, 4.7 MB as a WebP and 12.2 MB as a GIF**. AVIF wins
because it codes each frame against the ones before it. GIF loses on colour, not on structure: it
quantises each frame to 256 entries and this drawing is mostly smooth gradient, which is what
banding shows up in worst. It is also the drawing where dirty rectangles buy nothing — 260
film-grain specks are reseeded every frame, so nearly the whole page changes and every format's
rectangle is nearly the whole page. Both AVIF and WebP carry the Display P3 profile, which GIF has
nowhere to put.

Timing separates them the other way, and AVIF does not win it. GIF stores a delay in hundredths of a
second, so a 60fps frame is not a whole number of them; the delays are spread so the average rate is
right, but a browser will not play it — Firefox and Chrome both render any frame of 10ms or less at
100ms, so above 50fps the short frames stretch and the animation limps. AVIF and WebP store whole
milliseconds, alternating 16 and 17. The one format exact at 60fps is APNG, which stores the delay
as a fraction, and it cost 34 MB to be right about a third of a millisecond a frame; this example
stopped writing one.

The showcase below is the WebP, for a reason unrelated to encoding: browsers do not loop an animated
AVIF. They play it through once, so the smallest of the three is the one that stops after a single
wink.

![animated eye](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/animated-eye.webp)

## Platform support

Prebuilt binaries are published for Linux (x64/arm64, glibc and musl), macOS (arm64) and Windows
(x64/arm64). The Linux floors are measured on the released artifacts rather than assumed:

| distribution                   | glibc | support window    |
| ------------------------------ | ----- | ----------------- |
| RHEL / Rocky / Alma 8          | 2.28  | supported to 2029 |
| Ubuntu 20.04, Debian 11        | 2.31  |                   |
| AWS Lambda / Amazon Linux 2023 | 2.34  | supported to 2028 |
| RHEL / Rocky / Alma 9          | 2.34  | supported to 2032 |

There are two floors, not one: the module links `libstdc++` as well, and a symbol newer than the
target's fails to load exactly like a glibc one. The build asserts both ceilings on every Linux
artifact — glibc `2.34`, `GLIBCXX` `3.4.25` — which is what makes the table above a commitment
rather than a description.

## Documentation

Both surfaces have a generated reference, built from the source they ship rather than written
alongside it:

| Reference                                                      | Built from                                                     |
| -------------------------------------------------------------- | -------------------------------------------------------------- |
| [**docs.rs**](https://docs.rs/meo-skia-canvas)                 | The Rust crate, from its own doc comments.                     |
| [**jsdocs.io**](https://www.jsdocs.io/package/meo-skia-canvas) | The JavaScript API, from the type declarations in the package. |

Both track the published release rather than `main`. To build either locally against the working
tree, `just docs` does both — and fails on a broken link or a type that reaches a signature
without being exported, which a rendered page would show you no sign of.

The pages below are written by hand, and are the half a generator has nothing to say about:

| Guide                                      | Covers                                                                   |
| ------------------------------------------ | ------------------------------------------------------------------------ |
| [Getting started](docs/getting-started.md) | Install and first render.                                                |
| [Node API](docs/node.md)                   | Platform notes, JavaScript API, benchmarks.                              |
| [Rust crate](docs/rust.md)                 | The crate surface, and how it differs from the JavaScript one.           |
| [Drawing context](docs/api/context.md)     | The illustrated tour — conic curves, textures, dash markers, projection. |
| [Path geometry](docs/api/path2d.md)        | Boolean operations, trim, jitter, interpolate, with pictures.            |
| [Changelog](CHANGELOG.md)                  | Both release channels.                                                   |

## What this fork changes

**How the native binary reaches you.** One npm package per target, selected by `os`/`cpu`/`libc`,
rather than fetched by an install script — bun blocks those unless the package appears in the
consuming project's `trustedDependencies`, a list not inherited from dependencies, and
`--ignore-scripts` blocks them everywhere else. The download remains as a fallback.

**The Rust crate is a first-class surface.** `Canvas` and `Context2D` mirror the JavaScript API
rather than exposing whatever the binding happened to need, and colour strings and font queries go
through one implementation on both sides.

**Two GPU faults that predate this fork.** Every thread dlopened the Vulkan loader and the last
`Arc` to drop closed it, so the idle watcher could unload it under a thread still opening it — a
segfault in about half of thirteen runs. Every thread also built its own `VkInstance` and
`VkDevice`, so a `vkDestroyDevice` at thread exit could deadlock against another thread mid-submit
inside NVIDIA's process-global locks. One loader and one device are now shared for the life of the
process, with a queue per thread. Separately, **Metal exports now drain an autorelease pool**:
`toBuffer`/`toFile` hand work to a `rayon` pool whose workers have none, so Objective-C
allocations accumulated for the life of the process.

**Memory that a long-running process holds.** These shaped the fork most, and they only appear once
something renders for hours rather than once. The page cache memoizes a rasterized page so a later
export can composite it instead of replaying every layer — a good trade, except an entry left only
when V8 finalized the `JsBox` holding the context, and V8 sizes that box at a few machine words and
cannot see the half-megabyte image behind it. A thousand fresh 400×300 canvases, each drawn once and
exported, settled at 235 MB before this was bounded and 141 MB after. The bound is by bytes rather
than count, because pages are not one size. The font and variant parse caches had the same shape.

**And the pages go back when rendering stops.** glibc keeps freed memory in its own arenas, so
resident memory only ever climbed: 200 card exports peaked at 165 MB and stayed there. A watcher now
returns them a few seconds after the last render — 88 MB against 72 at startup — without
interrupting work in flight.

None of this was visible in the design. The cache, the finalizer and the allocator each behave
exactly as documented; it is the three together, under sustained load, that hold the memory.

Beyond that: correctness fixes to inherited code — the Linux ABI floors above, rendering
regressions introduced during phyron's `skia-safe` migration, and a long list of calls that
typechecked and then did nothing. The [changelog](CHANGELOG.md) records each with the measurement
that identified it.

[Skia]: https://skia.org
[samizdatco/skia-canvas]: https://github.com/samizdatco/skia-canvas
[phyrondev/phyron-skia-canvas]: https://github.com/phyrondev/phyron-skia-canvas

## Acknowledgements

Built on [`rust-skia`](https://github.com/rust-skia/rust-skia) (`skia-safe` + `skia-bindings`).

Forked from [samizdatco/skia-canvas], by way of [phyrondev/phyron-skia-canvas]. The architecture
here is theirs — the Skia binding, the canvas state model, the font stack — and so is the
groundwork everything since has been built on; thanks to the contributors of
[both](https://github.com/samizdatco/skia-canvas/graphs/contributors)
[projects](https://github.com/phyrondev/phyron-skia-canvas/graphs/contributors).

## License

MIT. See [`LICENSE`](LICENSE).

© 2020–2026 Samizdat Drafting Co., Phyron AB and contributors.
© 2026 L A Romeo, for changes made in this fork.
