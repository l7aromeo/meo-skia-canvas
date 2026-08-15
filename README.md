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

> A fork of [samizdatco/skia-canvas], by way of [phyrondev/phyron-skia-canvas].
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

Reference: [`docs/api/native-rust.md`](docs/api/native-rust.md). Runnable code:
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
  pixel buffers. Skia encodes three of them; the rest are written here, from the pixels it hands
  back.
- **The depth the drawing has** — a canvas composited in float is written at sixteen bits a channel
  as a PNG, APNG or TIFF instead of being rounded to eight on the way out, and AVIF codes 8, 10 or
  12 through `bitDepth`. JPEG, WebP, GIF, ICO and BMP are eight-bit formats by definition and
  narrow what they are handed; nothing here pretends otherwise.
- **Animation** — pages are frames. WebP, GIF, APNG and AVIF take `fps` or a per-frame
  `frameDelays` array. AVIF codes the frames _against each other_ rather than storing stills in a
  container, which is the whole reason its animated form exists: eight frames of a moving square
  come to 1146 bytes where a single still of one frame is 285 -- four times the file for eight times
  the frames. A WebP sends only the rectangle each frame changed, as the format intends.
- **AVIF has dials the other formats do not** — `chromaSampling` picks `"4:4:4"`, `"4:2:2"` or
  `"4:2:0"`, and `lossless` codes with no loss at all. Both default to the conservative answer, and
  both are measured rather than assumed: see [Performance](#performance-and-memory).
- **An animation read back reports its own `frames` and `delays`**, so re-encoding one is a round
  trip — for WebP, GIF, APNG and AVIF. Skia decodes neither of the last two. It opens an APNG as
  the still image inside it, so this library demuxes and composites APNG itself, `fcTL` rectangles,
  disposal and blending included; and it ships no AVIF decoder at all, so this library reads that
  format end to end — the ISOBMFF container parsed here, the frames handed to libaom. That covers
  what other encoders write as well as what this one does: grids of tiles, `irot` and `imir`
  orientation, ICC profiles, narrow-range levels and 4:2:0 chroma.
- **An SVG says what the canvas drew** — a conic gradient, a shadow, a blend mode or a filter is
  embedded as pixels where SVG cannot describe it, rather than silently dropped, and everything
  else stays vector.
- **Multi-page documents** — [`newPage()`](https://www.jsdocs.io/package/meo-skia-canvas) builds a canvas up as pages, written
  out as one multi-page PDF, TIFF or ICO, or as an image sequence.
- **GUI windows** with a browser-like event framework ([`Window`](https://www.jsdocs.io/package/meo-skia-canvas), [`App`](https://www.jsdocs.io/package/meo-skia-canvas)), not just headless rendering — from Rust as well as from Node, behind
  the `window` feature.
- **Threaded rendering and I/O** — a worker pool handles asynchronous export off the main thread.
- **Path geometry** — boolean operations, plus
  [`simplify`, `round`, `trim`, `jitter`, `points`, `interpolate`](docs/api/path2d.md) on any
  `Path2D`.
- **3D perspective** via `createProjection()`, on top of the usual affine transforms.
- **Vector textures** (`createTexture()`) as a fill style, and custom line-dash markers.
- **A canvas drawn onto a canvas is replayed, not resampled.** `drawCanvas` re-rasterizes the source
  recording at the destination scale, so scaling one up has no resampling artifacts to speak of —
  where a browser would rasterize the source first and then filter the pixels. Its compositing
  stays its own: the source's `destination-out` shapes the source, not what it is drawn onto.
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
with it, which is what a Rec. 2020 pipeline wants. They do not carry HDR _values_: a colour still
clamps at 1.0 on the way in, and none of the formats Skia encodes here — PNG, JPEG, WebP — is an HDR
container.

## Performance and memory

`just bench` runs [`examples/node/benchmark.js`](examples/node/benchmark.js) and prints these. It
builds the release binary first on purpose — a dev build leaves the Rust glue unoptimized, which
moves per-call overhead without touching Skia. Figures below are one machine, an Apple M4 Pro on
Metal, at 1200×900. **Treat the ratios as the transferable part and the milliseconds as local
colour.**

| mixed vector scene | time                   |
| ------------------ | ---------------------- |
| `RGBA8888` GPU     | 10.7 ms                |
| `RGBA8888` CPU     | 28.7 ms — 2.7× the GPU |

300 bezier strokes, 60 shadowed rounded panels, 40 lines of text.

**What a float canvas costs in time depends entirely on what you draw**, and it runs in both
directions — which is why there is no single multiplier here:

| workload               | `RGBA8888` | `RGBAF16` | `RGBAF32` |
| ---------------------- | ---------- | --------- | --------- |
| mixed vector scene     | 28.5 ms    | 1.25×     | 1.47×     |
| 120 translucent layers | 110.4 ms   | **0.70×** | **0.71×** |
| 120 opaque fills       | 7.4 ms     | 1.30×     | **7.24×** |

Blending translucent layers is _faster_ in float: an eight-bit surface converts through its transfer
function on every layer and a float one does not, which more than pays for the wider pixel. Opaque
fills go the other way, and `RGBAF32` in particular falls off a cliff rather than scaling with its
byte count — 7.2× for 4× the bytes. `RGBAF16` stays close to its memory cost throughout, which makes
it the one to reach for unless you specifically need 32-bit precision.

| encode a drawn page | time    | notes                                                         |
| ------------------- | ------- | ------------------------------------------------------------- |
| JPEG (q 0.92)       | 14.8 ms |                                                               |
| BMP                 | 27.8 ms | uncompressed, so the size of the raw buffer                   |
| PDF                 | 29.9 ms |                                                               |
| SVG                 | 49.7 ms | this scene is shadowed; a page SVG can describe whole is 8 ms |
| PNG                 | 59.6 ms |                                                               |
| GIF                 | 67.2 ms | k-means palette, one frame                                    |
| WebP (q 0.9)        | 76.5 ms |                                                               |
| TIFF                | 92.2 ms | deflate with a horizontal predictor                           |
| APNG                | 96.2 ms | one frame                                                     |
| AVIF (q 0.92)       | 250 ms  | eight tiles across eight threads                              |

AVIF is the slow one — 17× JPEG — and it buys something. On this page at the same `quality` it is
561 KB at 41.7 dB PSNR where JPEG is 802 KB at 34.9 dB: smaller _and_ closer to the original. WebP
lands at 411 KB and 25.6 dB, which is the trade libwebp makes at that dial rather than a fault —
it targets a perceptual metric, not PSNR, and this scene is antialiased diagonal lines and small
type, the hardest thing to keep. Reach for AVIF when the file matters more than the quarter-second
it costs, JPEG when neither does.

AVIF's own dials move both axes, so they are worth seeing apart from the format comparison:

| AVIF option               | time   | size    |
| ------------------------- | ------ | ------- |
| `quality` 0.5             | 231 ms | 215 KB  |
| `quality` 0.92            | 249 ms | 561 KB  |
| `quality` 1.0             | 281 ms | 2010 KB |
| `chromaSampling: "4:2:2"` | 216 ms | 443 KB  |
| `chromaSampling: "4:2:0"` | 194 ms | 368 KB  |
| `lossless: true`          | 305 ms | 2351 KB |

Subsampling is cheaper _and_ smaller — there is a quarter of the chroma to code at 4:2:0 — but on a
page like this one, which is text and flat panels rather than photography, it costs far more quality
than it saves bytes. It is the right choice for a photograph and the wrong one for a chart, which is
why the default is `"4:4:4"`. `lossless` costs 22% more time than `quality` 0.92 and four times the
size; the expense is bytes, not seconds.

Reading is this library's own code end to end, since Skia decodes no AVIF:

| decode a drawn page | time    |
| ------------------- | ------- |
| PNG                 | 9.7 ms  |
| AVIF                | 71.8 ms |

| resident memory per canvas | measured | surface alone |
| -------------------------- | -------- | ------------- |
| `RGBA8888`                 | 4.16 MB  | 4.12 MB       |
| `RGBAF16`                  | 8.28 MB  | 8.24 MB       |
| `RGBAF32`                  | 16.52 MB | 16.48 MB      |

Memory is the one figure that is simply arithmetic — 4, 8 and 16 bytes a pixel, and the measurement
lands within about 1% of it. It is also the one that needs repeating before it is believed: a single
pass over twenty canvases reads whatever the allocator happened to do, and has come back at 2.91 MB
for the eight-bit case and at a negative number for `RGBAF32`. The figures above are the settled
value across three passes, which is what the arithmetic predicts.

**Antialiasing coverage is where the GPU and the CPU disagree**, and neither GPU path matches the
raster one. Sweeping a rectangle's width from 0.05 to 1 pixel and reading the alpha back: the CPU
renderer is exact to within a level; 4𝗑 MSAA quantizes to quarters — 0, 64, 127, 191, 255 — so a
shape thinner than about an eighth of a pixel drops out entirely; and shader-based AA is smooth but
reads systematically low, putting 159 where a half-covered black edge over white should read 127.
Total error across that sweep runs 10 for the CPU, 307 at 4𝗑, and 427 with MSAA off, and the figures
come out the same on Metal and on Vulkan. The default is the closer of the two GPU options; if
coverage has to match the CPU renderer exactly, render on the CPU.

Two caveats worth stating plainly. **The release build changes little for most of this and a great
deal for one row.** Against a dev binary the GPU scene went 12.1 ms → 10.7 ms, PNG 56.9 → 58.5 and
JPEG 14.1 → 14.4 — unmoved, because that work is inside Skia and is compiled optimized either way.
AVIF is the exception, at **2810 ms on a dev build against 248 ms on release**, because the pixels
reach libaom through this crate's own per-pixel colour conversion and that is Rust: unoptimized, it
costs more than the codec does. Benchmark AVIF on a release build or not at all. And **the GPU row
is the least reproducible**: it moved between 10.3 and 12.1 ms across runs where the CPU rows held
to a few tenths of a millisecond.

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
are spring-dampers integrated at a fixed 240 Hz; the motion is what the forces produce rather than a
curve someone drew. The lid spring is deliberately asymmetric -- stiff closing, soft opening -- so
the wink snaps shut and drifts back open past its resting point, and each lash lags it through a
spring of its own while its root angle blends from the open fan to a swept-down rest pose, so the
fan rotates outward instead of sweeping through the eye. Lid velocity draws a second ghost copy of
every lash, which is motion blur that costs nothing and shows up only on the snap.

Two details are there because eyes have them and drawings usually do not: the ball rolls up as the
lid falls, so the iris is seen climbing out of view mid-wink, and the catchlights sit on the cornea
rather than in the iris plane, tracking the gaze at about half speed. That parallax is most of what
makes it read as a dome rather than a disc.

It leans on four things a browser canvas has no answer for: `Path2D.jitter()` for a hand-drawn edge
on every hair and fibre, `MaskFilter` for the occlusion in the socket and under the lashes, a
Display P3 canvas for iris blues outside sRGB, and writing the animation straight out of the
canvas's own pages -- one page per frame, no encoder to wire up.

It writes AVIF, WebP and GIF, and the differences are arithmetic rather than taste. The same 150
frames are **2.7 MB as an AVIF, 4.7 MB as a WebP and 12.2 MB as a GIF**. AVIF wins because it codes
each frame against the ones before it, and this drawing moves very little between frames; WebP sends
only the rectangle that changed, which is the same idea more cheaply. GIF stores whole frames and
quantises each to a 256-entry palette, and this drawing is mostly smooth gradient -- skin, sclera,
iris -- which is exactly what banding shows up in worst. Both AVIF and WebP carry the canvas's
Display P3 profile, which GIF has nowhere to put.

Timing separates them the other way, and AVIF does not win it. GIF stores a frame delay in
hundredths of a second, so a 60fps frame -- 16.67ms -- is not a whole number of them; the delays are
spread so the average rate is right, but individual frames alternate between 10 and 20ms and the
format cannot do better. The file still declares the rate it was asked for and nothing here caps it
-- but a browser will not play it: Firefox renders any GIF frame of 10ms or less at 100ms and Chrome
does the same, so above 50fps the short frames stretch and the animation limps. AVIF and WebP both
store whole milliseconds, alternating 16 and 17. AVIF's container counts in ticks of a 90 kHz clock
and _could_ be exact, but this library's frame delays are whole milliseconds all the way through, so
it is not. The one format that is exact at 60fps is APNG, which stores the delay as a fraction, and
it cost 34 MB to be right about a third of a millisecond a frame. This example stopped writing one.

The showcase below is the AVIF; the WebP and GIF are written beside it for anywhere that will not
take one.

![animated eye](https://media.githubusercontent.com/media/l7aromeo/meo-skia-canvas/main/docs/assets/gallery/animated-eye.avif)

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
| [Native Rust API](docs/api/native-rust.md) | The crate surface, and how it differs from the JavaScript one.           |
| [Drawing context](docs/api/context.md)     | The illustrated tour — conic curves, textures, dash markers, projection. |
| [Path geometry](docs/api/path2d.md)        | Boolean operations, trim, jitter, interpolate, with pictures.            |
| [Changelog](CHANGELOG.md)                  | Both release channels.                                                   |

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
of the process.

Beyond that, this fork carries correctness fixes to inherited code — the Linux ABI floors above, a
set of rendering regressions introduced during phyron's `skia-safe` migration, and a long list of
calls that typechecked and then did nothing. The [changelog](CHANGELOG.md) records each with the
measurement that identified it.

[Skia]: https://skia.org
[samizdatco/skia-canvas]: https://github.com/samizdatco/skia-canvas
[phyrondev/phyron-skia-canvas]: https://github.com/phyrondev/phyron-skia-canvas

## Acknowledgements

Built on [`rust-skia`](https://github.com/rust-skia/rust-skia) (`skia-safe` + `skia-bindings`).

Forked from [samizdatco/skia-canvas], by way of [phyrondev/phyron-skia-canvas]. Nearly all of
the code here is theirs; thanks to the contributors of
[both](https://github.com/samizdatco/skia-canvas/graphs/contributors)
[projects](https://github.com/phyrondev/phyron-skia-canvas/graphs/contributors).

## License

MIT. See [`LICENSE`](LICENSE).

© 2020–2026 Samizdat Drafting Co., Phyron AB and contributors.
© 2026 L A Romeo, for changes made in this fork.
