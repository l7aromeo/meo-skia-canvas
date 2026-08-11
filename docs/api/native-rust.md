# `meo_skia_canvas` -- Rust Consumer API

The crate-root modules (`canvas`, `context2d`, `paint`, `path`, `text`, `surface`, `image`, ...) are the supported Rust consumer API, re-exported in full through `meo_skia_canvas::prelude` -- `use meo_skia_canvas::prelude::*;`. The Node/Neon binding lives under the internal `node` module (`node::canvas`, `node::paragraph`, ...); it exists for Node compatibility, intentionally leaks `skia_safe` and Neon types, and is `pub(crate)` -- not a surface for Rust consumers.

## Two APIs, and which to use

- **`Canvas` + `Context2D`** mirror the HTML Canvas API: method names and argument order match `CanvasRenderingContext2D`, a mutable graphics state carries fill style, transform and clip, and `to_buffer`/`to_file` encode to PNG, JPEG, WebP, PDF or SVG. This is the entry point for most consumers, and the one to reach for when porting JavaScript.
- **`Surface` + `Recorder` + `DrawTarget`** are the layer beneath. Each draw takes an explicit `Paint` rather than reading state, and you get the pixel buffer rather than an encoded file. Use them for a render target you own, or when drawing into someone else's frame loop.

Note that `Context2D::save_layer_with` and `set_filter` are facade-level conveniences; the equivalent lower-layer entry points are `DrawTarget::save_layer_with` and `Paint::set_image_filter`. `create_pattern` has no lower-layer equivalent -- `Shader`'s six constructors are the four gradients plus fractal noise and turbulence, none of which tiles an image, so tiling a bitmap means going through `Context2D`.

## Stability commitment

- Public types in the crate-root API do **not** expose `skia_safe`, `neon`, `RefCell`, `FunctionContext`, `JsBox`, or `Handle<...>`.
- `skia_safe` remains a private implementation detail. Wrapping or aliasing Skia types in `pub` signatures is treated as an API regression.
- The audit `rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" $(ls src/*.rs)` (the crate-root modules, excluding `src/node/`) returns no matches; CI guards this.
- A compile-time pin in `tests/native_studio_renderer_adapter.rs` references the full Studio-shaped adapter surface, so any future patch that smuggles a Skia type into a public method breaks the test.

## Color spaces

The facade distinguishes **working** and **export** color spaces:

- **Working space** -- `LinearColorSpace::{Srgb, DisplayP3, Rec2020}`. Surfaces composite at linear-light precision. Each variant is a real linear-light space with its own primaries; `LinearColorSpace::DisplayP3` is **not** an alias for linear sRGB. Studio rendering, blending, gradients, and filters operate in this space.
- **Export space** -- `PixelColorSpace::{Srgb, SrgbLinear, DisplayP3, DisplayP3Linear, Rec2020, Rec2020Linear}`. Used for `read_pixels_as`, `write_pixels`, and `Image::from_pixels`. Linear and gamma-coded variants are explicit; there is no implicit fallback to sRGB.

`RgbaLinear` values are interpreted in **the destination surface's working color space**. Drawing `RgbaLinear::opaque(1.0, 0.0, 0.0)` onto a `LinearColorSpace::Rec2020` surface stores red in linear Rec.2020 primaries; the same value on a `LinearColorSpace::Srgb` surface stores red in linear sRGB primaries. The wrapper plumbs the surface's working color space through to every `Color4f` handoff (paint, clear, save_layer, draw_surface, draw_text_box) so Skia does not silently re-decode linear values as if they were sRGB-encoded.

HDR values above `1.0` are valid internally. Surfaces use RGBAF16 storage so out-of-gamut and out-of-display values survive compositing. Clamping happens only at export to a fixed-range format (e.g. `PixelDepth::Uint8`).

```rust
let mut surface = backend.create_surface(
    1920,
    1080,
    SurfaceOptions { color_space: LinearColorSpace::DisplayP3, ..SurfaceOptions::default() },
)?;
```

## Premultiplied alpha

- `RgbaLinear` channel values are **premultiplied** linear-light RGBA. `RgbaLinear::opaque(1.0, 0.5, 0.5)` is opaque; `RgbaLinear::new_premultiplied(0.5, 0.0, 0.0, 0.5)` is half-alpha red.
- Surfaces composite in premultiplied alpha space.
- `read_pixels()` (no args) returns **unpremultiplied** RGBA8 in sRGB gamma -- the wire format expected by `HTMLCanvasElement.putImageData`. Use `read_pixels_as(PixelExportOptions { premultiplied: true, ... })` to keep the premul values.
- `read_pixels_raw()` returns the surface in its native format (RGBAF16, premultiplied, working color space) for callers that want exact internal values.
- `read_pixels_linear()` returns RGBAF32 premultiplied in the surface's working color space for HDR round-trips (Citra postprocessing, ID buffers).

## Pixel formats and depths

- `PixelFormat::{Rgba8UnormPremul, Rgba8UnormUnpremul, Rgba16fPremul, Rgba32fPremul}` covers raw image creation and frame readback.
- `PixelDepth::{Uint8, F16, F32}` selects bit depth for `read_pixels_as` / `write_pixels`.
- `PixelExportOptions { color_space, depth, premultiplied }` is the explicit handshake; combine the three orthogonally. Unsupported combinations return typed `Error::Unsupported{PixelColorSpace, PixelFormat, PixelDepth}`.

## Surfaces, recorder, and canvas

- `Backend::new()` is the entry point; cheap, no GPU context.
- `backend.create_surface(width, height, options)` builds a `Surface`. Surfaces own their pixel storage and render at RGBAF16 precision.
- `surface.with_canvas(|canvas| ...)` borrows a `DrawTarget` for the closure. Its methods cover save / restore, transforms, clipping, draws, layers, and filters.
- `surface.snapshot()` -> `Image` for compositing snapshots.
- `surface.create_offscreen(width, height)` builds an offscreen surface inheriting the parent's working color space and engine.
- `surface.flush()` submits any queued GPU work; no-op for CPU surfaces.
- `surface.engine()` reports the rasterizer the surface ended up using (`EngineKind::Cpu` or `Gpu`) -- useful when `RenderEngine::Auto` was requested.
- `Recorder` is the original picture-recording API kept for completeness; new consumers should prefer `Surface` (it owns real pixel storage and supports read / write / snapshot).

## Render engine selection

- `SurfaceOptions::engine` selects the rasterizer:
  - `RenderEngine::Auto` (default) -- GPU when a backend is compiled in *and* runtime-reachable, CPU otherwise.
  - `RenderEngine::Cpu` -- forces the raster path. Use for deterministic snapshots / tests.
  - `RenderEngine::Gpu` -- requires GPU. Surface construction returns `Error::EngineUnavailable { engine: Gpu, reason }` when no GPU backend is compiled in or the runtime cannot reach a device.
- `backend.engine_status(engine)` returns a typed `EngineStatus { renderer, api, device, driver, threads, is_gpu_available, error }` for diagnostics; cheap and side-effect free, so it's safe to call before `create_surface`.
- `RenderEngine::Gpu` requires the `vulkan` (Linux / Windows) or `metal` (macOS) feature; `Auto` and `Cpu` work without either.
- HDR values above `1.0` are preserved by CPU surfaces. GPU drivers may clamp to the `[0, 1]` range during compositing depending on the backend's intermediate format. Pin `RenderEngine::Cpu` if you need bit-exact HDR round-trips, or accept that `Auto` will use whatever the platform offers.

## Paint

- `Paint` carries the full Canvas paint accumulator: `color`, `style` (`Fill` / `Stroke`), `stroke_width`, `stroke_cap`, `dash`, `anti_alias`, `alpha` modulator, `blend_mode`, optional `shader`, optional `image_filter`, optional `color_filter`.
- `Paint::fill(color)` and `Paint::stroke(color, width)` are convenience constructors.
- `BlendMode` covers Canvas `globalCompositeOperation`, including `Lighter` (additive, Canvas `lighter` / CSS `plus-lighter`, mapped to Skia's `Plus`) alongside the separable `Lighten`, plus the CanvasKit-only `Clear`, `Modulate` and `Destination`.

## Paths

- `Path::from_svg(svg_data, FillRule::{NonZero, EvenOdd})` parses SVG path data (the `d=""` form). Invalid input returns `Error::InvalidSvgPath`.
- `PathBuilder` builds one segment by segment: `move_to`, `line_to`, `bezier_curve_to`, `quadratic_curve_to`, `conic_curve_to`, `arc`, `ellipse`, `arc_to`, `rect`, `round_rect`, `round_rect_elliptical`, `add_path`, `close_path`. Same names, arguments and semantics as the `Context2D` methods, minus the current transform, which belongs to a context. `build(fill_rule)` snapshots without ending the build; `PathBuilder::from_path` starts one from an existing `Path`.
- A negative width or height reverses the winding of `rect` and `round_rect`, as it does in a browser, so a reversed rectangle inside another punches a hole under `NonZero`. Two negatives cancel.
- `arc_to` and the `round_rect` pair return `Error::InvalidRect` for a negative or non-finite radius.
- `DrawTarget::clip_path` / `draw_path` consume `Path`.
- `draw_line(p1, p2, &Paint)` uses the paint's stroke width / cap / dash.

## Shaders

- `Shader::linear_gradient(start, end, stops, interpolation)` builds a linear gradient. The interpolation argument takes a `GradientColorSpace` -- the eight CSS Color 4 names, `Srgb` (the default, gamma-encoded, what a browser draws) through `Oklch` -- or the pair a `GradientColorSpace::hue(HueMethod::{Shorter, Longer, Increasing, Decreasing})` builds, which selects the direction hue travels in the four cylindrical spaces. `GradientStop { position, color }` carries `RgbaLinear` colors in the destination working color space. Stops must be sorted with positions in `0.0..=1.0`; violations return `Error::InvalidGradient`. OKLCH interpolation flows through Skia's `OKLCH` color space directly -- no silent fallback to sRGB.
- Attach via `Paint::set_shader(Some(shader))`.

## Filters

- `ImageFilter::{blur, drop_shadow, color_matrix, from_color_filter, compose}` builds image-domain filters. Compose chains them as `outer(inner(source))`.
- `ColorFilter::{luma, srgb_to_linear_gamma, linear_to_srgb_gamma, compose}` builds color-domain filters; luma is the building block for `destination-in` mask paths.
- Attach via `Paint::set_image_filter` / `set_color_filter`.

## Images

- `Image::from_encoded(bytes)` decodes PNG / JPEG / WebP raster bytes via Skia's image codec.
- `Image::from_pixels(bytes, width, height, stride, pixel_format, color_space)` builds an image directly from a raw pixel buffer -- the bridge for rsmpeg-decoded video frames and Citra-generated images. **No PNG / JPEG / WebP round trip on the hot path.**
- `Image::from_svg_xml(svg, width, height)` rasterizes an SVG document. `from_encoded` does **not** decode SVG XML.
- `DrawTarget::draw_image_rect` / `draw_image_src` paint images; `SamplingMode::{Nearest, Linear, Mipmapped, Cubic}` controls resampling.

## Text

- `FontManager::{register_font_from_data, register_font_from_path, has_font, families}` registers TTF / OTF / WOFF / WOFF2 typefaces under family aliases. Internal state is a `parking_lot::Mutex` -- no `RefCell` exposure.
- `TextEngine::new(&font_manager)` wires the registry into a paragraph `FontCollection` (with system-font fallback). `with_system_fonts()` is the no-registry convenience.
- `TextStyle` carries font selection, size, weight, slant, color, alignment, line height, letter / word spacing, decoration (`underline` / `overline` / `line_through` plus style, color, thickness), shadows, and baseline shift. `font_weight: i32` drives `SkFontStyle` weight-bucket matching and (when a `wght` axis is not pinned via `font_variations`) auto-synthesizes a design-space weight on variable typefaces. `TextStyle` is `#[non_exhaustive]` -- construct with `..TextStyle::default()`.
- **`TextStyle::font_variations: Vec<FontVariation>`** pins variable-font axis positions before layout (CanvasKit's `fontVariations` shape). When non-empty, the engine finds typefaces matching the requested families + style, clones each variable typeface at the requested axes (clamped to the typeface's declared `[min, max]`), and seeds them on a per-call `FontCollection`. Use `FontAxisTag::WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL` for the common axes, or `FontAxisTag::from_str("xxxx")` / `FontAxisTag::new(b"xxxx")` for arbitrary tags. Rich-text variations come from the *base* style: `SkParagraphBuilder` reads its collection once at construction, so per-span axis changes are not supported.
- `TextEngine::layout_text(text, style, max_width)` lays out plain text. `layout_rich_text(spans, base_style, max_width)` lays out a sequence of `RichTextSpan` overrides on top of a base style.
- `TextLayout::{width, max_width, height, line_count, first_line_ascent, line_metrics, rects_for_range}` exposes laid-out paragraph metrics. `width()` returns the **measured** longest-line width (matches the TS renderer's `TextLayout.width`), not the wrapping budget.
- `DrawTarget::draw_text_layout(layout, x, y)` paints the laid-out paragraph.

## Errors

`Error` is the unified error type. Variants are exhaustive and carry typed reasons:

- Dimension / stride / byte-length errors for surface and image construction.
- Unsupported color-space / pixel-format / pixel-depth combinations.
- Filter / gradient / SVG-path / image-decode failures.
- Pixel readback / write failures.
- Font register failures (invalid data or IO error).

`Error` implements `std::error::Error` and `Display`, and works directly with `anyhow` / `thiserror` callers.

## Verification commands

Run on Linux with the project's feature subset (the `metal` feature is macOS-only):

```bash
just fmt-check
just typecheck
just lint-check
cargo test --features "vulkan,window,freetype" --test native_api_contract
cargo test --features "vulkan,window,freetype" --test native_studio_renderer_contract
cargo test --features "vulkan,window,freetype" --test native_studio_renderer_adapter
```

Audits:

```bash
rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" src/*.rs
rg -n "\.unwrap\(|\.expect\(|panic!|todo!|unimplemented!" src/*.rs tests/native_*.rs
rg -n "use skia_safe" tests/native_studio_renderer_adapter.rs
```

The first should be empty. The second is **not** expected to be empty: `AGENTS.md` permits `.unwrap()`/`.expect()` where a `// SAFETY:` comment justifies it, so read the hits rather than counting them -- an uncommented one is the defect. The third returns only doc-comment hits referring to the audit itself.

## CanvasKit parity additions (0.2.0)

The P0+P1 CanvasKit-parity sweep added the following to the `native`
facade. See the rustdoc on each item for full per-argument detail.

- **Text**: `TextStyle.font_features: Vec<FontFeature>` (OpenType
  features), `TextStyle.{half_leading, strut, text_height_behavior,
  max_lines}` with the `StrutStyle` and `TextHeightBehavior` types;
  `TextLayout::{did_exceed_max_lines, number_of_lines (line_count),
  rects_for_placeholders, unresolved_codepoints}`; font fallback is
  enabled on every `TextEngine` collection.
- **Paint**: `Paint.{dither, mask_filter}` with `set_dither` /
  `set_mask_filter`; `MaskFilter::blur(BlurStyle, sigma,
  respect_ctm)`; `BlendMode::{Clear, Modulate, Destination}`.
- **DrawTarget**: `DrawTarget::save_layer_with(SaveLayerOptions { paint,
  bounds, backdrop })`.
- **Shaders**: `Shader::{radial_gradient, sweep_gradient,
  two_point_conical_gradient, fractal_noise, turbulence}` alongside the
  existing `linear_gradient`.
- **Images**: `SamplingMode::Cubic` (Mitchell-Netravali bicubic).

`ColorFilter`-side color-matrix helpers ship on the Node surface as the
`ColorMatrix` object; on the Rust side, build the 4x5 matrix directly and
pass it to `ImageFilter::color_matrix`.
