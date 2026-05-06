# `skia_canvas::native` -- Rust Consumer API

`skia_canvas::native` is the only supported Rust consumer API for this crate. The older modules under the crate root (`canvas`, `context`, `paragraph`, ...) exist for Node / Neon compatibility and intentionally leak `skia_safe` and Neon types in their public signatures; they are not the supported surface for new Rust consumers.

## Stability commitment

- Public types in `skia_canvas::native` do **not** expose `skia_safe`, `neon`, `RefCell`, `FunctionContext`, `JsBox`, or `Handle<...>`.
- `skia_safe` remains a private implementation detail. Wrapping or aliasing Skia types in `pub` signatures is treated as an API regression.
- The audit `rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" src/native` returns no matches; CI guards this.
- A compile-time pin in `tests/native_studio_renderer_adapter.rs` references the full Studio-shaped adapter surface, so any future patch that smuggles a Skia type into a public method breaks the test.

## Color spaces

The facade distinguishes **working** and **export** color spaces:

- **Working space** -- `LinearColorSpace::{Srgb, DisplayP3, Rec2020}`. Surfaces composite at linear-light precision. Each variant is a real linear-light space with its own primaries; `LinearColorSpace::DisplayP3` is **not** an alias for linear sRGB. Studio rendering, blending, gradients, and filters operate in this space.
- **Export space** -- `PixelColorSpace::{Srgb, SrgbLinear, DisplayP3, DisplayP3Linear, Rec2020, Rec2020Linear}`. Used for `read_pixels_as`, `write_pixels`, and `NativeImage::from_pixels`. Linear and gamma-coded variants are explicit; there is no implicit fallback to sRGB.

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
- `PixelExportOptions { color_space, depth, premultiplied }` is the explicit handshake; combine the three orthogonally. Unsupported combinations return typed `NativeError::Unsupported{PixelColorSpace, PixelFormat, PixelDepth}`.

## Surfaces, recorder, and canvas

- `NativeBackend::new()` is the entry point; cheap, no GPU context.
- `backend.create_surface(width, height, options)` builds a `NativeSurface`. Surfaces own their pixel storage and render at RGBAF16 precision.
- `surface.with_canvas(|canvas| ...)` borrows a `NativeCanvas` for the closure. Canvas methods cover save / restore, transforms, clipping, draws, layers, and filters.
- `surface.snapshot()` -> `NativeImage` for compositing snapshots.
- `surface.create_offscreen(width, height)` builds an offscreen surface with the same working color space.
- `NativeRecorder` is the original picture-recording API kept for completeness; new consumers should prefer `NativeSurface` (it owns real pixel storage and supports read / write / snapshot).

## Paint

- `NativePaint` carries the full Canvas paint accumulator: `color`, `style` (`Fill` / `Stroke`), `stroke_width`, `stroke_cap`, `dash`, `anti_alias`, `alpha` modulator, `blend_mode`, optional `shader`, optional `image_filter`, optional `color_filter`.
- `NativePaint::fill(color)` and `NativePaint::stroke(color, width)` are convenience constructors.
- `BlendMode` covers Canvas `globalCompositeOperation` plus `PlusLighter` (additive). Mapped to Skia's `Plus`.

## Paths

- `NativePath::from_svg(svg_data, FillRule::{NonZero, EvenOdd})` parses SVG path data (the `d=""` form). Invalid input returns `NativeError::InvalidSvgPath`.
- `NativeCanvas::clip_path` / `draw_path` consume `NativePath`.
- `draw_line(p1, p2, &NativePaint)` uses the paint's stroke width / cap / dash.

## Shaders

- `NativeShader::linear_gradient(start, end, stops, GradientInterpolation::{Srgb, Oklch})` builds a linear gradient. `GradientStop { position, color }` carries `RgbaLinear` colors in the destination working color space. Stops must be sorted with positions in `0.0..=1.0`; violations return `NativeError::InvalidGradient`. OKLCH interpolation flows through Skia's `OKLCH` color space directly -- no silent fallback to sRGB.
- Attach via `NativePaint::set_shader(Some(shader))`.

## Filters

- `NativeImageFilter::{blur, drop_shadow, color_matrix, from_color_filter, compose}` builds image-domain filters. Compose chains them as `outer(inner(source))`.
- `NativeColorFilter::{luma, srgb_to_linear_gamma, linear_to_srgb_gamma, compose}` builds color-domain filters; luma is the building block for `destination-in` mask paths.
- Attach via `NativePaint::set_image_filter` / `set_color_filter`.

## Images

- `NativeImage::from_encoded(bytes)` decodes PNG / JPEG / WebP raster bytes via Skia's image codec.
- `NativeImage::from_pixels(bytes, width, height, stride, pixel_format, color_space)` builds an image directly from a raw pixel buffer -- the bridge for rsmpeg-decoded video frames and Citra-generated images. **No PNG / JPEG / WebP round trip on the hot path.**
- `NativeImage::from_svg_xml(svg, width, height)` rasterizes an SVG document. `from_encoded` does **not** decode SVG XML.
- `NativeCanvas::draw_image_rect` / `draw_image_src` paint images; `SamplingMode::{Nearest, Linear, Mipmapped}` controls resampling.

## Text

- `NativeFontManager::{register_font_from_data, register_font_from_path, has_font, families}` registers TTF / OTF / WOFF / WOFF2 typefaces under family aliases. Internal state is a `parking_lot::Mutex` -- no `RefCell` exposure.
- `NativeTextEngine::new(&font_manager)` wires the registry into a paragraph `FontCollection` (with system-font fallback). `with_system_fonts()` is the no-registry convenience.
- `TextStyle` carries font selection, size, weight, slant, color, alignment, line height, letter / word spacing, decoration (`underline` / `overline` / `line_through` plus style, color, thickness), shadows, and baseline shift. `font_weight: i32` allows variable-font passthrough (e.g. `350`).
- `NativeTextEngine::layout_text(text, style, max_width)` lays out plain text. `layout_rich_text(spans, base_style, max_width)` lays out a sequence of `RichTextSpan` overrides on top of a base style.
- `NativeTextLayout::{width, max_width, height, line_count, first_line_ascent, line_metrics, get_rects_for_range}` exposes laid-out paragraph metrics. `width()` returns the **measured** longest-line width (matches the TS renderer's `TextLayout.width`), not the wrapping budget.
- `NativeCanvas::draw_text_layout(layout, x, y)` paints the laid-out paragraph.

## Errors

`NativeError` is the unified error type. Variants are exhaustive and carry typed reasons:

- Dimension / stride / byte-length errors for surface and image construction.
- Unsupported color-space / pixel-format / pixel-depth combinations.
- Filter / gradient / SVG-path / image-decode failures.
- Pixel readback / write failures.
- Font register failures (invalid data or IO error).

`NativeError` implements `std::error::Error` and `Display`, and works directly with `anyhow` / `thiserror` callers.

## Verification commands

Run on Linux with the project's feature subset (the `metal` feature is macOS-only):

```bash
just fmt-check
just check
just lint-check
cargo test --features "vulkan,window,freetype" --test native_api_contract
cargo test --features "vulkan,window,freetype" --test native_studio_renderer_contract
cargo test --features "vulkan,window,freetype" --test native_studio_renderer_adapter
```

Audits:

```bash
rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" src/native
rg -n "\.unwrap\(|\.expect\(|panic!|todo!|unimplemented!" src/native tests/native_*.rs
rg -n "use skia_safe" tests/native_studio_renderer_adapter.rs
```

The first two should be empty. The third returns only doc-comment hits referring to the audit itself.
