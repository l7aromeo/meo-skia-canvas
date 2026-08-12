---
description: The Rust crate surface -- Canvas, Context2D, colour management and the typed error set
---

# `meo_skia_canvas` -- Rust Consumer API

Every public type is reachable straight off the crate root:

```rust
use meo_skia_canvas::{Canvas, CanvasOptions, EncodeOptions, FillRule, PathBuilder};
```

The modules (`canvas`, `context2d`, `color`, `export`, `filter`, `font`, `geometry`, `image`, `paint`, `path`, `pattern`, `pixels`, `shader`, `text`, `texture`, `error`) group the same types by subject, which is how they are documented, and `meo_skia_canvas::prelude::*` globs the lot for anyone who prefers a prelude. One draw usually reaches across several modules -- `Canvas::to_buffer` alone speaks `ImageFormat`, `EncodeOptions`, the pixel types and `Error` -- so nothing requires knowing which module a type lives in.

The Node/Neon binding lives under the internal `node` module; it exists for Node compatibility, intentionally leaks `skia_safe` and Neon types, and is `pub(crate)` -- not a surface for Rust consumers.

## One API, two front doors

`Canvas` + `Context2D` mirror the HTML Canvas API: method names and argument order match `CanvasRenderingContext2D`, a mutable graphics state carries fill style, transform and clip, and `to_buffer` / `to_file` encode to PNG, JPEG, WebP, PDF or SVG.

There is no second, lower layer. An earlier fork carried a parallel `Surface` / `Recorder` / `DrawTarget` API for an external consumer that never materialised; nothing in this crate used it, and it was removed. What it could do, `Canvas` and `Context2D` do -- including mask filters, the shader factories, image sampling modes, bounded layers and variable-font axes.

For text laid out ahead of a draw, `TextEngine` and `FontManager` are the paragraph-level entry points; `Context2D::draw_paragraph` puts the result on a canvas.

## Stability commitment

- Public types in the crate-root API do **not** expose `skia_safe`, `neon`, `RefCell`, `FunctionContext`, `JsBox`, or `Handle<...>`.
- `skia_safe` remains a private implementation detail. Wrapping or aliasing Skia types in `pub` signatures is treated as an API regression.
- The audit `rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" $(ls src/*.rs)` (the crate-root modules, excluding `src/node/`) returns no matches; CI guards this.

## Colour management

A canvas composites in the space it was built with, and an export converts out of it -- the same rule a browser's canvas follows.

```rust
let mut canvas = Canvas::with_options(1920.0, 1080.0, CanvasOptions {
    color_space: PixelColorSpace::DisplayP3,
    color_type: PixelDepth::Uint8,
    gpu: true,
})?;
```

- `color_space` fixes the compositing space. `RgbaLinear` is interpreted in it, so `RgbaLinear::opaque(1.0, 0.0, 0.0)` is Display P3 red on a P3 canvas and sRGB red on an sRGB one. A colour outside the canvas's gamut is clipped as it is drawn, not at the export.
- `color_type` selects the format exports and readbacks default to, and -- when it is `F16` or `F32` -- the format the page composites in. Blending then keeps what eight bits would round away: sixty fills at 0.6% alpha land on 0.303 in float against 0.239 at eight bits, where 0.303 is right. Reckon on 1.4x the time and twice the memory for `F16`, 1.5x and four times for `F32`. Every other format composites at N32, since an opaque or narrower one loses more inside the page than it saves.
- The compositing format follows the *canvas*, never the call: `PixelExportOptions { depth: F32 }` on an eight-bit canvas reads back float pixels holding eight-bit values, rather than quietly recompositing the page.
- A float canvas renders on the raster backend whatever `CanvasOptions::gpu` says, and `Canvas::engine_kind` reports which one took it. No GPU can currently give that precision: Skia's Metal and Vulkan backends implement no 32-bit float surface, and while both provide `F16`, a GPU quantises the paint colour to eight bits before compositing -- the same sixty layers land on 0.235, further from right than the eight-bit 0.361. Metal and Vulkan return that figure to the digit, which is what makes it Skia's limit rather than a driver's.
- The capability is probed once at runtime rather than assumed, so a Skia that grows the support keeps these canvases on the GPU with no change here.
- A readback with no layout of its own -- `get_image_data` -- takes both the canvas's space and its format, and reports them on the `ExportedPixels` it returns. `get_image_data_as` overrides either. This is what a browser does: `getImageData()` on a Display P3 canvas hands back P3 components.
- `EncodeOptions::color_space` is an `Option<PixelColorSpace>`: the space an export converts *into*, where `None` means the canvas's own. Requesting a wider one re-expresses what the surface holds; it cannot widen it.
- `Canvas::new` is `with_options` with the defaults -- sRGB, 8-bit, GPU allowed.

The JavaScript side takes the same two settings as `new Canvas(w, h, { colorSpace, colorType })`, and both surfaces name the same spaces.

## Colours

`RgbaLinear` is premultiplied linear light, interpreted in the canvas's own
space: `RgbaLinear::opaque(1.0, 0.0, 0.0)` is Display P3 red on a P3 canvas and
sRGB red on an sRGB one.

For a colour written the way CSS writes it, `Context2D::set_fill_style_css`,
`set_stroke_style_css` and `set_shadow_color_css` take the notations the
JavaScript side takes -- named colours, `#rgb`, `rgb()`, `hsl()`, `hwb()`,
`lab()`, `lch()`, `oklab()`, `oklch()` and `color(<space> r g b / a)` -- through
the same parser, so both surfaces land on the same pixel.

```rust
ctx.set_fill_style_css("oklch(70% 0.2 140)")?;
ctx.set_stroke_style_css("color(display-p3 1 0 0)")?;
```

The space a string names is kept rather than routed through sRGB, so
`color(display-p3 1 0 0)` on a P3 canvas is that canvas's own red while `"red"`
is sRGB red converted into it. A browser keeps the previous fill when a string
will not parse; these return [`Error::InvalidColor`], since a Rust caller has
somewhere to put the answer.

## Colour spaces

`PixelColorSpace` is the one vocabulary, used for the canvas, for readbacks and for exports:

| Variant | Primaries | Transfer function | JavaScript name |
| --- | --- | --- | --- |
| `Srgb` | sRGB | sRGB | `srgb` |
| `SrgbLinear` | sRGB | linear | `srgb-linear`, `linear` |
| `DisplayP3` | Display P3 | sRGB | `display-p3`, `p3` |
| `DisplayP3Linear` | Display P3 | linear | `display-p3-linear`, `p3-linear` |
| `Rec2020` | Rec. 2020 | Rec. 709 | `rec2020`, `bt2020` |
| `Rec2020Linear` | Rec. 2020 | linear | `rec2020-linear`, `bt2020-linear` |
| `Rec2020Pq` | Rec. 2020 | PQ | `rec2020-pq`, `hdr10` |
| `Rec2020Hlg` | Rec. 2020 | HLG | `rec2020-hlg`, `hlg` |

Both surfaces build these from the same CICP pair, so a canvas made from Rust and one made from JavaScript are the same canvas, ICC profile included.

The two HDR rows build a canvas with that transfer function and tag exports with it. They do not make the pixels carry HDR: a colour still clamps at 1.0 on the way in, and none of the formats this crate encodes -- PNG, JPEG, WebP -- is an HDR container. They are useful for producing correctly tagged Rec. 2020 output for a pipeline that takes the buffer elsewhere.

## Options structs

`CanvasOptions`, `EncodeOptions`, `PixelExportOptions`, `TextureOptions`, `TextStyle`, `TextBoxOptions` and `StrutStyle` are plain structs with public fields and a `Default`. **Build them from the default and override what you need:**

```rust
let options = EncodeOptions {
    color_space: Some(PixelColorSpace::DisplayP3),
    ..EncodeOptions::default()
};
```

The trailing `..` is not a style preference -- it is the compatibility contract. A field added to one of these structs is source-compatible with every caller that writes it, and breaks exactly the callers that list every field instead.

None of them is `#[non_exhaustive]`, deliberately. That attribute forbids the struct expression *including* the `..Default::default()` form, so every construction would become a `let mut` followed by a field assignment per override -- measured at 82 sites in this repository alone. It buys protection the rest pattern already provides.

## Premultiplied alpha

- `RgbaLinear` channel values are **premultiplied** linear-light RGBA. `RgbaLinear::opaque(1.0, 0.5, 0.5)` is opaque; `RgbaLinear::new_premultiplied(0.5, 0.0, 0.0, 0.5)` is half-alpha red.
- Canvases composite in premultiplied alpha space.
- `Context2D::get_image_data` returns **unpremultiplied** components -- the wire format `putImageData` expects, and what a browser hands back.
- `get_image_data_as(PixelExportOptions { premultiplied: true, .. })` keeps the premultiplied values, and takes the depth and colour space alongside.

## Pixel formats and depths

- `PixelFormat::{Rgba8UnormPremul, Rgba8UnormUnpremul, Rgba16fPremul, Rgba32fPremul}` covers raw image creation.
- `PixelDepth::{Uint8, F16, F32}` selects bit depth for readbacks and for `CanvasOptions::color_type`.
- `PixelExportOptions { color_space, depth, premultiplied }` is the explicit handshake; combine the three orthogonally. Unsupported combinations return a typed `Error`.

## Pages

A canvas holds one or more pages, and each page is a recording materialised at export time.

- `Canvas::context()` borrows the current page's `Context2D`.
- `new_page` / `new_page_with` start another, and `page_count` / `page` select among them.
- `EncodeOptions::page` picks which one an export encodes; PDF encodes all of them.

## Render engine selection

- `CanvasOptions::gpu` asks for the GPU: `true` (the default) uses it when a backend is compiled in *and* runtime-reachable, and falls back to the raster backend otherwise. `Canvas::set_gpu` changes it after construction.
- `Canvas::engine_kind()` reports what asking actually got -- `EngineKind::Cpu` or `EngineKind::Gpu`. `Canvas::gpu()` reports what was asked for.
- The GPU path requires the `vulkan` (Linux / Windows) or `metal` (macOS) feature; without either, everything renders on the raster backend.
- The two backends are not bit-identical. The GPU composites through 4x MSAA by default, so coverage lands in quarter steps where the raster backend computes it exactly; sub-pixel geometry is where the two differ most. `EncodeOptions::msaa` changes the sample count, and `0` or `1` mean none.

## Paint

- `Paint` carries the full Canvas paint accumulator: `color`, `style` (`Fill` / `Stroke`), `stroke_width`, `stroke_cap`, `dash`, `anti_alias`, `alpha` modulator, `blend_mode`, optional `shader`, optional `image_filter`, optional `color_filter`.
- `Paint::fill(color)` and `Paint::stroke(color, width)` are convenience constructors.
- `BlendMode` covers Canvas `globalCompositeOperation`, including `Lighter` (additive, Canvas `lighter` / CSS `plus-lighter`, mapped to Skia's `Plus`) alongside the separable `Lighten`, plus the CanvasKit-only `Clear`, `Modulate` and `Destination`.

## Paths

- `Path::from_svg(svg_data, FillRule::{NonZero, EvenOdd})` parses SVG path data (the `d=""` form). Invalid input returns `Error::InvalidSvgPath`.
- `PathBuilder` builds one segment by segment: `move_to`, `line_to`, `bezier_curve_to`, `quadratic_curve_to`, `conic_curve_to`, `arc`, `ellipse`, `arc_to`, `rect`, `round_rect`, `round_rect_elliptical`, `add_path`, `close_path`. Same names, arguments and semantics as the `Context2D` methods, minus the current transform, which belongs to a context. `build(fill_rule)` snapshots without ending the build; `PathBuilder::from_path` starts one from an existing `Path`.
- A negative width or height reverses the winding of `rect` and `round_rect`, as it does in a browser, so a reversed rectangle inside another punches a hole under `NonZero`. Two negatives cancel.
- `arc_to` and the `round_rect` pair return `Error::InvalidRect` for a negative or non-finite radius.
- `Context2D::clip_path` / `fill_path` / `stroke_path` consume `Path`.

## Shaders

- `Shader::linear_gradient(start, end, stops, interpolation)` builds a linear gradient. The interpolation argument takes a `GradientColorSpace` -- the eight CSS Color 4 names, `Srgb` (the default, gamma-encoded, what a browser draws) through `Oklch` -- or the pair a `GradientColorSpace::hue(HueMethod::{Shorter, Longer, Increasing, Decreasing})` builds, which selects the direction hue travels in the four cylindrical spaces. `GradientStop { position, color }` carries `RgbaLinear` colours in the canvas's own colour space. Stops must be sorted with positions in `0.0..=1.0`; violations return `Error::InvalidGradient`. OKLCH interpolation flows through Skia's `OKLCH` color space directly -- no silent fallback to sRGB.
- Attach via `Paint::set_shader(Some(shader))`.

## Filters

- `ImageFilter::{blur, drop_shadow, color_matrix, from_color_filter, compose}` builds image-domain filters. Compose chains them as `outer(inner(source))`.
- `ColorFilter::{luma, srgb_to_linear_gamma, linear_to_srgb_gamma, compose}` builds color-domain filters; luma is the building block for `destination-in` mask paths.
- Attach via `Paint::set_image_filter` / `set_color_filter`.

## Images

- `Image::from_encoded(bytes)` decodes PNG / JPEG / WebP raster bytes via Skia's image codec.
- `Image::from_pixels(bytes, width, height, stride, pixel_format, color_space)` builds an image directly from a raw pixel buffer -- the way to hand over a decoded video frame or a buffer you generated yourself. **No PNG / JPEG / WebP round trip on the hot path.**
- `Image::from_svg_xml(svg, width, height)` rasterizes an SVG document. `from_encoded` does **not** decode SVG XML.
- `Context2D::draw_image` / `draw_image_rect` / `draw_image_src` paint images.
- `Context2D::set_image_smoothing_enabled(false)` gives nearest-neighbour. With smoothing on, `set_image_smoothing_quality` picks how: `Low` is bilinear, `Medium` adds mipmaps, and `High` is cubic -- Mitchell when the draw enlarges the source, Catmull-Rom otherwise, and bilinear where the scale is not known. A browser makes the same distinction, which is why `High` is only visibly different from `Medium` on an upscale.

## Text

- `FontManager::{register_font_from_data, register_font_from_path, has_font, families}` registers TTF / OTF / WOFF / WOFF2 typefaces under family aliases. Internal state is a `parking_lot::Mutex` -- no `RefCell` exposure.
- `TextEngine::new(&font_manager)` wires the registry into a paragraph `FontCollection` (with system-font fallback). `with_system_fonts()` is the no-registry convenience.
- `TextStyle` carries font selection, size, weight, slant, color, alignment, line height, letter / word spacing, decoration (`underline` / `overline` / `line_through` plus style, color, thickness), shadows, and baseline shift. `font_weight: i32` drives `SkFontStyle` weight-bucket matching and (when a `wght` axis is not pinned via `font_variations`) auto-synthesizes a design-space weight on variable typefaces. Construct with `..TextStyle::default()`: the struct is not `#[non_exhaustive]` (no crate-root type is), so listing every field compiles today and breaks the next time one is added.
- **`TextStyle::font_variations: Vec<FontVariation>`** pins variable-font axis positions before layout (CanvasKit's `fontVariations` shape). When non-empty, the engine finds typefaces matching the requested families + style, clones each variable typeface at the requested axes (clamped to the typeface's declared `[min, max]`), and seeds them on a per-call `FontCollection`. Use `FontAxisTag::WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL` for the common axes, or `FontAxisTag::from_str("xxxx")` / `FontAxisTag::new(b"xxxx")` for arbitrary tags. Rich-text variations come from the *base* style: `SkParagraphBuilder` reads its collection once at construction, so per-span axis changes are not supported.
- `FontManager::installed_families()` lists every family a draw can match -- the platform's own plus anything registered here -- and `family_details(name)` reports the weights, widths and styles one offers, or `None` when nothing resolves under that name. The counterparts of the JavaScript `FontLibrary.families` and `FontLibrary.family()`. `families()` stays the narrower question: what this registry was given.
- `Context2D::set_font_stretch` selects a narrower face where the family ships one, and pins the `wdth` axis where it is a variable font -- which is how most variable fonts carry their widths.
- `TextEngine::layout_text(text, style, max_width)` lays out plain text. `layout_rich_text(spans, base_style, max_width)` lays out a sequence of `RichTextSpan` overrides on top of a base style.
- `TextLayout::{width, max_width, height, line_count, first_line_ascent, line_metrics, rects_for_range}` exposes laid-out paragraph metrics. `width()` returns the **measured** longest-line width, not the wrapping budget -- `max_width()` gives back the budget the layout was asked for.
- `Context2D::draw_paragraph(layout, x, y)` paints the laid-out paragraph.

## What fails, and how

Audited before the stable release, since a method that grows a `Result`
afterwards breaks every caller:

- **An operation that can fail returns `Result`.** Canvas construction, every
  export and readback, image decoding, SVG path parsing, gradient building and
  font registration all do.
- **Three panics exist in the consumer API**, each on an invariant rather than
  on input: `Canvas::new` assumes sRGB is constructible (the one space every
  Skia build has), `Canvas::context` assumes a canvas has a page (it is seeded
  with one and nothing removes it), and one font-stretch conversion runs behind
  a match guard that already proved the value. Nothing a caller passes can
  reach them.
- **Degenerate input is ignored, not rejected**, exactly as a browser's canvas
  ignores it: `NaN` or infinite coordinates, a negative line width, a zero-sized
  canvas, an empty font family. These draw nothing and carry on rather than
  returning an error, because that is what the API being mirrored does.
- **Queries answer rather than fail.** `measure_text` on a missing font
  measures the fallback, `has_font` returns `false`, `first_line_ascent` of an
  empty paragraph is `0.0`.

## Errors

`Error` is the unified error type. Variants are exhaustive and carry typed reasons:

- Dimension / rect / stride / byte-length errors for canvas, image and readback construction (`InvalidDimensions`, `InvalidRect`, `InvalidStride`, `InvalidByteLength`).
- Unsupported colour-space / pixel-format / pixel-depth combinations (`UnsupportedPixelColorSpace`, `UnsupportedPixelFormat`, `UnsupportedPixelDepth`).
- Filter / gradient / SVG-path / colour-string / image-decode failures (`FilterCreate`, `InvalidGradient`, `InvalidSvgPath`, `InvalidColor`, `DecodeImage`).
- Canvas creation, rendering and encoding failures (`SurfaceCreate`, `Render`, `Encode`).
- Pixel readback / write failures (`PixelReadback`, `PixelWrite`).
- Font register failures, invalid data or IO error (`FontRegister`).
- A GPU engine that was asked for and is not reachable (`EngineUnavailable`).

`Error` implements `std::error::Error` and `Display`, and works directly with `anyhow` / `thiserror` callers.

## Verification commands

Run on Linux with the project's feature subset (the `metal` feature is macOS-only):

```bash
just fmt-check
just typecheck
just lint-check
cargo test --features "vulkan,window,freetype"
```

The test run covers all five binaries: `native_context2d` (the Canvas facade,
the largest of them), `native_api_contract`, `native_studio_renderer_contract`,
`native_studio_renderer_adapter` and `native_text_perf_smoke`, plus the
doctests.

Audits:

```bash
rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" src/*.rs
rg -n "\.unwrap\(|\.expect\(|panic!|todo!|unimplemented!" src/*.rs
rg -n "use skia_safe" tests/native_studio_renderer_adapter.rs
```

The first should be empty. The second is **not** expected to be empty: `AGENTS.md` permits `.unwrap()`/`.expect()` where a `// SAFETY:` comment justifies it, so read the hits rather than counting them -- an uncommented one is the defect. It covers library code only; the tests are deliberately full of `.expect("...")`, which is how a test reports a failure. The third returns only doc-comment hits referring to the audit itself.
