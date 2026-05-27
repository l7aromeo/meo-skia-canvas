# CanvasKit → phyron-skia-canvas parity: P0 + P1 implementation design

**Date:** 2026-05-27
**Source gap list:** `studio:docs/phyron-skia-canvas-parity.md` (branch `docs/phyron-skia-canvas-parity`)
**Goal:** implement all P0 + P1 parity gaps across both surfaces, then ready the crate (`skia-canvas`) and the node package (`phyron-skia-canvas`) for publishing.

## Surfaces (every feature touches up to all three)

1. **Rust crate public API** — `src/native/*` (the stable `native` facade; no `skia_safe`/`neon` types leak).
2. **Neon binding** — root `src/*.rs` modules (`typography.rs`, `paragraph.rs`, `context/*`, `image.rs`, `image_filter.rs`, `color_filter.rs`, `gradient.rs`, ...). These are `#[doc(hidden)]` and expose the JS surface.
3. **TS types** — `lib/index.d.ts` (hand-maintained, ~2244 lines). Browser shim `lib/browser.js` only if an API needs a browser-native polyfill.

No wasm target exists; `phyron-skia-canvas` is a Neon native addon (`main: lib/index.js`) plus a `browser` shim. Studio-side wiring (`packages/renderer/src/backend/skia-canvas/`) is an explicit **follow-up**, out of scope here.

## Execution order (subsystem-coherent batches, text first — unblocks PP-780)

### Batch 1 — Text / Paragraph
- **fontFeatures** (P0 #1, the trigger). `TextStyle.font_features: Vec<FontFeature{ name, value }>`. Thread into `build_text_style` via `SkTextStyle::add_font_feature`. Neon: parse `fontFeatures: [{name,value}]` in `TextStyleInput`. `.d.ts`: add `fontFeatures?: TextFontFeatures[]`. Mirrors existing `fontVariations` wiring.
- **strutStyle + halfLeading + textHeightBehavior** (P1). `ParagraphStyle.strut_style` (`StrutStyle{ font_families, font_size, height, leading, force_strut_height, strut_enabled, half_leading }`), `TextStyle.half_leading: bool`, `ParagraphStyle.text_height_behavior`. skia: `SkParagraphStyle::set_strut_style`, `SkTextStyle::set_half_leading`, `set_text_height_behavior`.
- **Paragraph overflow queries** (P1). On `NativeTextLayout`: `did_exceed_max_lines()`, `number_of_lines()`, `rects_for_placeholders()`. skia: `SkParagraph::did_exceed_max_lines/line_number/get_rects_for_placeholders`.
- **Font fallback + missing-glyph** (P1). `FontCollection::enable_font_fallback` in the engine; expose `unresolved_codepoints()` on the layout (verify skia-safe 0.97.2 exposes it; if not, document the gap).

### Batch 2 — Paint / compositing
- **setDither** (P0 #3). `NativePaint.dither: bool` → `SkPaint::set_dither`. Neon: `ctx`-level or paint-level toggle.
- **per-draw blend modes Clear/Modulate/Dst** (P1). Extend the `globalCompositeOperation` ↔ `BlendMode` mapping (currently omits these three).
- **MaskFilter blur + BlurStyle + respectCTM** (P1). New `NativeMaskFilter` (Normal/Solid/Outer/Inner, sigma, respectCTM) → `skia_safe::MaskFilter::blur`. Wire onto paint.
- **saveLayer(paint?, bounds?, backdrop?, flags?)** (P0 #2, heaviest). `NativeCanvas::save_layer(SaveLayerRec)` → `Canvas::save_layer`. Backdrop image-filter for blur-behind. Neon: `ctx.saveLayer(...)`.

### Batch 3 — Effects / Shader
- **Shader subsystem** (P1). First-class `NativeShader` factories: linear/radial/sweep/two-point-conical (already have `linear_gradient`; add the rest), `TileMode`, `local_matrix`, premul flags, color-space/hue; plus `fractal_noise`/`turbulence`, `color`/`blend`. Neon: `Shader` class.
- **ColorMatrixHelpers** (P1). `concat/identity/post_translate/rotated/scaled` 4x5 matrices feeding `ColorFilter::matrix`/`hsla_matrix`. Pure math.

### Batch 4 — Image sampling
- **HQ sampling** (P1). Cubic resampling (`CubicResampler`, draw-image-cubic/rect-cubic), mipmaps (`MipmapMode`, default-mipmaps copy), `drawImageOptions` filter+mipmap. Reduces alias/shimmer on downscaled/moving video.

## Verification per batch
- `cargo fmt` clean; `cargo clippy --no-default-features --features "freetype,window,node-addon" --all-targets -- -D warnings` clean.
- A focused test per capability under `tests/` (extend `native_api_contract.rs`; new tests as needed). Reuse the warm Skia binary — none of these add skia-safe **build** features, only API usage.
- `DOCS_RS=1 cargo doc --no-deps --no-default-features` green (docs.rs equivalence; `native` facade only).

## Publish-readiness (stop condition; do NOT run publish without final go)
- Crate: bump `0.1.1 → 0.2.0` (new public API = minor pre-1.0). `skia-canvas` README/`docs/api/native-rust.md` updated for the new `native` surface.
- Node: bump `phyron-skia-canvas` `3.5.2 → 3.6.0`. `lib/index.d.ts` matches the implemented JS surface. Node addon builds (`cargo-cp-artifact` / `npm run build`).
- Both green on fmt/clippy/test/docs. Then hand off for `cargo publish` + `npm publish`.

## Notes / risks
- Some "absent" rows are reachable via Canvas2D detours (pixels via `getImageData`/`putImageData`, image shaders via `createPattern`, perspective via `createProjection`). Where the detour preserves fidelity, document it rather than add a redundant API (per the gap doc).
- `unresolvedCodepoints`, `getRectsForPlaceholders`, `StrutStyle`, `MaskFilter`, `CubicResampler`, `MipmapMode` availability in skia-safe 0.97.2 must be confirmed at implementation time; fall back to documenting any genuinely-unexposed primitive.
- `node-addon` + `--all-targets` is the clippy config that compiles the Neon glue; keep it green.
