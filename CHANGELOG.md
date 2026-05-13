# Changelog

> Two release channels live in this file:
>
> - **crates.io** (Rust crate `skia-canvas`): semver-tracked, starts at `0.1.0`.
> - **npm** (Node addon `phyron-skia-canvas`): tracks the upstream `skia-canvas` lineage; current `v3.5.x`.

<!--## 🥚 ⟩ [Unreleased]-->

## 📦 ⟩ [v3.5.2] ⟩ May 13, 2026

### New Features

- **`Color4fInput` on `fillStyle`, `strokeStyle`, and `addColorStop`**:
  paint and gradient APIs now accept the same `string | [r, g, b, a]`
  union that text already does. Pass a `[r, g, b, a]` array of
  premultiplied linear-light sRGB-primaries floats to skip the lossy
  CSS-encoding round-trip; CanvasKit's `Paint.setColor4f` shape. New
  exported `Color4fInput` type alias; `TextColorInput` becomes a
  `@deprecated` alias of `Color4fInput`. (#21)

### Internals

- `paragraph.rs`: dropped the dead `set_typeface(face)` branch in
  `parse_text_style`. `SkParagraphBuilder` reads its font collection at
  construction time, so variable-typeface clones must be seeded on the
  collection (which `paragraph::new` already does via
  `fonts_for_style`); a per-`pushStyle` `set_typeface` call was
  silently ignored on every glyph run.

## 📦 ⟩ [v3.5.1] ⟩ May 13, 2026

### New Features

- **`TextStyleInput.fontVariations`**: paragraph text now honors variable-font
  axes. Pass `fontVariations: [{ axis: "wght", value: 350 }, ...]` on a text
  style; `ParagraphBuilder.Make` instantiates a typeface clone at the
  requested axis positions before layout. Previously `fontStyle.weight` only
  drove `SkFontStyle`-based font matching and the matched typeface was used
  at its default master instance, so a `wght`-axis font (e.g. Dosis) rendered
  at its base weight regardless of the requested value -- producing different
  glyph densities than CanvasKit-WASM. New `FontVariationInput` type alias
  exported from `lib/index.d.ts`. (#19)

### Internals

- Release tooling: `publish.yml` now sets `registry-url` on `setup-node` and
  passes `NODE_AUTH_TOKEN`; `lib/prebuild.mjs snapshot` reads asset digests
  from the REST `/releases/{id}/assets` endpoint (works on every `gh`
  version); `just publish` swaps `gh release edit` for
  `gh api -X PATCH .../releases/{id} -F draft=false` and passes `-R` on
  every `gh` call. (#18)
- `containers/Dockerfile.{glibc,musl}` now install `git-lfs`; linux build job
  runs `git lfs pull` after checkout and the test gate is strict again. (#18)

## 📦 ⟩ [v3.5.0] ⟩ May 12, 2026

First npm release since the native Rust API split. All changes are additive
to the JavaScript surface; existing CSS-string color callers are unaffected.

### New Features

- **Linear-light `F32` color channel for paragraph text**. `TextStyle.color`,
  `foregroundColor`, `backgroundColor`, `decorationColor`, and
  `TextShadow.color` now accept either a CSS string or a
  `[r, g, b, a]` array of premultiplied linear-light sRGB-primaries floats
  (the CanvasKit `Paint.setColor` shape). The linear path tags the Skia
  paint with `srgb_linear`, so glyphs blend in linear light on F16 / F32
  surfaces -- replacing the lossy `oklchToSrgbHex` shortcut that dropped
  alpha and assumed sRGB regardless of the working color space. New
  `TextColorInput` type alias is exported from `lib/index.d.ts`. (#12)
- **Render engine selection on the native Rust API**. `RenderEngine::{Auto,
  Cpu, Gpu}` on `SurfaceOptions`, plus `NativeBackend::engine_status` for a
  typed snapshot of the renderer. The JavaScript-side `Canvas.engine` /
  `Canvas.toBuffer` paths are unchanged. (#10)

### Bugfixes

- **Color-space double-decode fixed across the wrapper**. `Paint::set_color4f`,
  `Canvas::clear`, `image_filters::drop_shadow`,
  `TextStyle::set_decoration_color`, and `TextShadow::new` were
  interpreting linear-light `Color4f` values as sRGB-encoded and
  gamma-decoding them a second time, darkening every linear color (an
  input of 0.198 was reading back as byte ~52 instead of ~124). The
  wrapper now plumbs the destination surface's working color space
  through every Skia handoff. (#9)

### Internals / Rust crate

- **Rust crate published to crates.io as `skia-canvas` 0.1.0**. The Rust
  consumer API lives under `skia_canvas::native`; public signatures never
  expose `skia_safe` or `neon` types, enforced by a compile-time pin. The
  npm package keeps its `phyron-skia-canvas` name; only the cargo channel
  renames. The cargo and npm version channels are independent from this
  release on. (#9, #11)
- **Native Rust API surface complete** -- `NativeSurface`, `NativePaint`,
  `NativePath`, `NativeShader`, `NativeImageFilter`, `NativeColorFilter`,
  `NativeImage`, `NativeFontManager`, `NativeTextEngine`,
  `NativeTextLayout`, full color pipeline (`LinearColorSpace`,
  `PixelColorSpace`, `RgbaLinear`). (#5, #8)

## 📦 ⟩ [crates.io 0.1.0] ⟩ May 6, 2026

First publish to crates.io as `skia-canvas`. The Rust API surface lives under
`skia_canvas::native` and is held to a stable Rust contract: no `skia_safe` or
`neon` types appear in public signatures, enforced by a compile-time pin in
`tests/native_studio_renderer_adapter.rs`.

### What lands in 0.1.0

- **HTML Canvas-shaped Rust API**: `NativeBackend`, `NativeSurface`,
  `NativeCanvas`, `NativePaint`, `NativePath`, `NativeShader`,
  `NativeColorFilter`, `NativeImageFilter`, `NativeImage`,
  `NativeFontManager`, `NativeTextEngine`, `NativeTextLayout`. Save / restore,
  path ops, gradient + pattern shaders, filter chains, raw-pixel image
  creation, premultiplied linear-light colors.
- **Color pipeline**: `LinearColorSpace::{Srgb, DisplayP3, Rec2020}` for the
  working space; `PixelColorSpace` with linear / gamma variants for export.
  Surfaces composite at RGBAF16 precision; `RgbaLinear` is the typed
  premultiplied linear-light color primitive. Color-space tagging is plumbed
  through every Skia handoff so `RgbaLinear` values are never silently
  double-decoded.
- **Render engine selection**: `RenderEngine::{Auto, Cpu, Gpu}` on
  `SurfaceOptions`. `Auto` picks GPU (Vulkan / Metal) when compiled in and
  runtime-reachable; `Cpu` forces the raster path; `Gpu` returns
  `NativeError::EngineUnavailable` if no backend is selectable.
  `NativeBackend::engine_status` returns a typed snapshot.
- **Cargo features**: `vulkan` (Linux / Windows GPU), `metal` (macOS GPU),
  `window` (`winit` event loop), `freetype` (FreeType + WOFF2 bundled),
  `node-addon` (registers the Neon entry point so the cdylib loads as a
  Node.js addon). The default feature set is empty -- pure-Rust consumers
  pick the backend they need.
- **Examples**: `cargo run --example basic_render --no-default-features --features "vulkan,freetype" --release`.
- **Docs**: `docs/api/native-rust.md` and crate-level rustdoc cover color
  spaces, surfaces, paint, paths, shaders, filters, images, text, fonts.

### Notes

- The npm package `phyron-skia-canvas` and the cargo crate `skia-canvas` ship
  from the same source tree but version independently.
- HDR (>1.0) values are preserved on CPU surfaces. GPU drivers may clamp
  during compositing; pin `RenderEngine::Cpu` for bit-exact HDR round-trips.

## 📦 ⟩ [v3.4.5] ⟩ Apr 8, 2026

### New Features

- **Color-space-aware solid colors**: `fillStyle`, `strokeStyle`, and texture colors now use
  Skia's `setColor4f` with explicit color space tagging. CSS colors (hex, `rgb()`, named) are
  tagged as sRGB; float array colors (`[r, g, b, a]`) are tagged with the canvas's working
  color space. Skia automatically converts colors during picture replay when the export surface
  has a different color space (e.g. linear → sRGB gamma correction).

- **`colorSpace` export option**: `toBuffer()` and `toFile()` now accept a `colorSpace` option
  (e.g. `"srgb"`, `"srgb-linear"`, `"display-p3"`) that sets the output surface's color space.
  Combined with color-space-aware colors, this enables correct gamma-corrected output from
  linear working spaces without manual pixel conversion.

### Bugfixes

- Fixed `colorSpace` option not being forwarded from JS `exportOptions()` to the native side.

## 📦 ⟩ [v3.3.0] ⟩ Jan 29, 2026

### New Features

- **CanvasKit Filter Parity**: Added `ColorFilter` and `ImageFilter` classes with CanvasKit-compatible API
  - `ColorFilter.MakeMatrix(matrix)` - 4x5 color transformation matrix
  - `ColorFilter.MakeSRGBToLinearGamma()` - sRGB to linear gamma conversion
  - `ColorFilter.MakeLinearToSRGBGamma()` - linear to sRGB gamma conversion
  - `ImageFilter.MakeColorFilter(colorFilter, input?)` - wrap ColorFilter as ImageFilter
  - `ImageFilter.MakeCompose(outer, inner)` - compose two ImageFilters
  - `ImageFilter.MakeBlur(sigmaX, sigmaY, tileMode?, input?)` - gaussian blur
  - `ImageFilter.MakeDropShadow(dx, dy, sigmaX, sigmaY, color, input?)` - drop shadow with source
  - `ImageFilter.MakeDropShadowOnly(dx, dy, sigmaX, sigmaY, color, input?)` - drop shadow only

- **Context Filter Properties**: Added `ctx.colorFilter` and `ctx.imageFilter` properties
  - Filters apply during drawing operations (fillRect, stroke, drawImage, etc.)
  - Filters compose with existing CSS `ctx.filter` property
  - Filters work correctly with `save()`/`restore()`/`reset()`

### Internal Changes

- Renamed internal `ImageFilter` → `SamplingFilter` to avoid naming collision with new Skia ImageFilter wrapper

## 📦 ⟩ [v3.0.8] ⟩ Sep 25, 2025

### Bugfix

- Fix rendering to windows with semi-transparent backgrounds

## 📦 ⟩ [v3.0.7] ⟩ Sep 19, 2025

### Bugfix

- Added missing TypeScript definitions for `resizable` property (thanks to @goldenratio #265)

### Misc. Improvements

- Upgraded Skia to [milestone 140](https://github.com/rust-skia/rust-skia/releases/tag/0.88.0)
- Added a bounding box hierarchy cache to further speed up canvas-to-canvas drawing via [drawImage][mdn_drawImage] or [drawCanvas][drawCanvas] (thanks to @Shiranuit #261)

## 📦 ⟩ [v3.0.6] ⟩ Aug 28, 2025

### Bugfix

- Fixed Windows CI build

## 📦 ⟩ [v3.0.5] ⟩ Aug 28, 2025

### Misc. Improvements

- Decreased memory usage when drawing one canvas's contents onto another (via [drawImage][mdn_drawImage] or [drawCanvas][drawCanvas]).
- Reduced dependency footprint (from 294 to 22 modules when installed with `devDependencies` included):
  - replaced `nodemon` with `node --watch`
  - replaced `jest` with `node --test` for unit tests
  - replaced `express` with `hono` for visual tests
  - dropped `lodash` and `fast-glob` usage in test suite

## 📦 ⟩ [v3.0.4] ⟩ Aug 22, 2025

### Bugfixes

- Variable fonts can now correctly function as fallbacks (previously only the first-matched font in a stack would be converted to a usable instance)

### Misc. Improvements

- When installing the module, any proxy server defined via `npm config set proxy` or an `HTTPS_PROXY` environment variable will be used to fetch the prebuilt binary
- Replaced `fetch` altogether, now using Node's built-in `http` and `https` modules for better backward compatibility, support for additional [request parameters][request_opts] for [loadImage()][loadImage()], and a further reduction in the number of npm dependencies (now down to 8)

[request_opts]: https://nodejs.org/api/http.html#httprequestoptions-callback

## 📦 ⟩ [v3.0.3] ⟩ Aug 20, 2025

### Bugfix

- Fixed a segfault where windows on Vulkan platforms were being deallocated incorrectly upon close.

## 📦 ⟩ [v3.0.2] ⟩ Aug 17, 2025

### Misc. Improvements

- Only use `node-fetch` on systems lacking a built-in `fetch`
- Dropped `fast-glob` (reducing external dependency count to 11)

### Breaking Changes

- Glob-handling has been removed from [FontLibrary.use()][FontLibrary.use]. If you want the old behavior, try using the [`fast-glob`](https://www.npmjs.com/package/fast-glob) or [`glob`](https://www.npmjs.com/package/glob) modules to [prepare the file-list][font_globbing] you pass to the method.

[font_globbing]: /docs/api/font-library.md#with-a-list-of-glob-patterns

## 📦 ⟩ [v3.0.1] ⟩ Aug 16, 2025

### Misc. Improvements

- Updated `node-fetch` to v3 to fix deprecation warnings on recent node versions
- Updated `winit` and other rust dependencies

## 📦 ⟩ [v3.0.0] ⟩ Aug 15, 2025

### New Features

#### GUI

- The `App` global now has an [`eventLoop`][app_eventLoop] property which can be set to:
  - `"native"` (the default) in which case the Node event loop is suspended while the OS handles displaying GUI windows
  - `"node"` where the Node event loop maintains control (allowing `setInterval` and `setTimeout` to run) and handles GUI events manually every few milliseconds (though note some of the [caveats][winit_caveats] associated with the Winit feature this uses).
- [**Window**][window] objects now have a read-only [`closed`][win_closed] property and emit a [`close`][win_close] event when they are closed. Closed windows can later be re-opened by calling the new [`open()`][win_open()] method.
- The new [`borderless`][win_borderless] attribute allows **Window** titlebars and borders to be hidden (thanks to @hydroperx #230)

#### Imagery

- The [`loadImage()`][loadImage()] and [`loadImageData()`][loadImageData()] helpers now use `node-fetch` to handle web requests and can accept a [fetch options][fetch_opts] object as the final argument.
- `Image` objects can now be created by passing a Buffer or dataURL-containing string as a [constructor argument][image_constructor] and will be immeditately drawable (no asynchronous loading required).
- Added support for integrating the [Sharp][sharp] image processor into canvas workflows (if the `sharp` npm module has been installed):
  - The new Canvas[.toSharp()][canvas_toSharp] & ImageData[.toSharp()][id_toSharp] convenience methods convert their contents to a Sharp bitmap object
  - `loadImage()` & `loadImageData()` can now be called with a Sharp object as their sole argument
  - The `src` property on a new Image object can be set to a Sharp object and it will begin asynchronously loading
- Added new options to [`createTexture()`][createTexture()] for setting the [line cap][createTexture_cap] style and selecting whether vector patterns should be clipped or [outlined][createTexture_outline]

#### Rendering

- Significant speed-ups for deeply layered drawing in which the canvas isn't cleared or reset (potentially resulting in numerous vector objects being re-drawn despite being hidden by shapes drawn on top):
  - The bitmap generated by [getImageData()][mdn_getImageData]/[toBuffer()][Canvas.toBuffer]/[toFile()][Canvas.toFile] is now cached. When called repeatedly, only newly added drawing commands will need to be rasterized (and will be layered atop the bitmap saved in the prior call).
  - Window contents are now cached between screen refreshes, improving performance during resizing and in cases where the canvas is drawn to in multiple passes and not cleared with every frame
  - Calling clearRect() or fillRect() with an area that covers the canvas now erases all the vector shapes below
- The toFile(), toBuffer(), and toDataURL() methods now accept an optional [`downsample`][downsample] flag (for jpegs only), which enables 4:2:0 chroma-subsampling. By default, no subsampling (a.k.a. 4:4:4) will be performed
- The getImageData() method now accepts additional rendering arguments ([`density`][density], [`matte`][matte], and [`msaa`][msaa]) which behave the same as their equivalents in the [toFile()][Canvas.toFile] method.

#### Typography

- Text lightness can now be fine-tuned through a pair of optional arguments that can be passed to the [Canvas][canvas_text_rendering] or [Window][window_text_rendering] constructors:
  - `textContrast` — a number in the range 0.0–1.0 controlling the amount of additional weight to add (defaults to `0.0`)
  - `textGamma` — a number in the range 0.0–4.0 controlling how glyph edges are blended with the background (defaults to `1.4`)
- The [`textAlign`][textAlign] attribute can now be set to `"justify"`
- [`measureText()`][measureText()] has been rewritten to calculate metrics based not just on the font specified in [`font`][ctx_font] but also any fallback fonts that were used for character glyphs not present in the ‘main’ font. The line-by-line measurements now include a [`runs`][measureText.runs] array with bounds and metrics for each single-font range of characters on the line.

#### Supported Platforms

- Added precompiled binaries for Arm-based Windows systems
- Now providing pre-built ‘layer’ archives for use with [AWS Lambda][running_lambda] (for Node v20 and above)
- Linux builds now include a statically linked version of fontconfig, as a result:
  - `libfontconfig` packages no longer need to be installed on the host system using `apt`, `apk`, `yum`, `dnf`, etc.
  - it now runs on ‘serverless’ platforms like Vercel without modification (sadly Cloudflare [doesn't support](https://github.com/cloudflare/workers-sdk/issues/4913) native modules at all though)

### Breaking Changes

- Renamed export functions and options to be more consistent with similar browser APIs and other Node modules:
  - `saveAs()` and `saveAsSync()` are now called [`toFile()`][Canvas.toFile] and [`toFileSync()`][Canvas.toFile]
  - [`toDataURL()`][toDataURL] now behaves the same as its browser equivalent: it is synchronous and its only configuration option is a numerical `quality` setting
  - `toDataURLSync()` has been removed
  - [`toURL()`][toURL] and [`toURLSync()`][toURL] produce data URLs and support the same enhanced export options as [`toBuffer`][Canvas.toBuffer]
- When exporting to an SVG, text is now converted to paths only if the [`outline`][export_outline] option is set to `true`

### Misc. Improvements

- [`App.launch()`][App.launch()] now returns a Promise that resolves when the final window is closed, allowing you to schedule code to run before the process would otherwise exit (see also the new [`idle`][app_idle] event which fires under the same circumstances).
- `input` event objects now contain an `inputType` property to distinguish between insertion, deletion, and IME composition
- Mouse events are no longer coalesced down to a single instance per frame (most relevant for `mousemove` events)
- Mouse events now include a standard [`buttons`][mdn_buttons] attribute
- DPI metadata is now included in webp files (reflecting the [`density`][density] option passed to [toFile()][Canvas.toFile] or [toBuffer()][Canvas.toBuffer])
- Argument validation now emulates browser behavior much more closely—including converting what were previously TypeErrors in certain cases into silent failures. To reënable these errors, set the `SKIA_CANVAS_STRICT` environment variable to `1` or `true`.
- Replaced `node-pre-gyp` with a custom installation script and `glob` with `fast-glob`, cutting the number of `node_modules` directories installed from 83 to 29.
- [loadImage()][loadImage()], [loadImageData()][loadImageData()], and [Image.src][Image.src] can now accept [URL][node_url] objects (using http(s), file, or data protocols). Likewise, [toFile()][Canvas.toFile] now accepts `file:` URLs (allowing relative paths to be constructed with [`import.meta.url`][meta_url])
- The Canvas constructor's options argument can now contain a [`gpu` property][gpu_opt] which can be set to `false` in order to use CPU-based rendering

### Bugfixes

- Setting a window's `cursor` property to "none" now hides the cursor
- Spurious `moved` window events are no longer emitted during resizes
- `resize` events now update the window object’s width & height properties in addition to providing the new size in the event object
- [`roundRect()`][roundRect] now reflects context's current transform state and accepts plain `{x, y}` objects for corner-radii in addition to Numbers and DOMPoints (thanks to @mpaperno #223)
- Angles passed to [`createConicGradient()`][createConicGradient()] are no longer incorrectly offset by 90°
- Calling `lineTo` on an empty Path2D no longer adds a line from the origin to the specified coordinates: it now acts as if it were a `moveTo`
- [`measureText()`][measureText()] now correctly calculates widths when letterSpacing has been set
- `startRange` and `endRange` in TextMetrics.lines[] now correspond to character indices in the string passed to measureText(), not byte indices into the UTF-8 buffer backing it

[App.launch()]: /docs/api/app.md#launch
[app_eventLoop]: /docs/api/app.md#eventLoop
[app_idle]: /docs/api/app.md#idle
[win_close]: /docs/api/window.md#close
[win_closed]: /docs/api/window.md#closed
[win_open()]: /docs/api/window.md#open
[win_borderless]: /docs/api/window.md#borderless
[winit_caveats]: https://docs.rs/winit/latest/winit/platform/pump_events/trait.EventLoopExtPumpEvents.html#platform-specific
[mdn_buttons]: https://developer.mozilla.org/en-US/docs/Web/API/MouseEvent/buttons
[textAlign]: /docs/api/context.md#textalign
[roundRect]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/roundRect
[loadImageData()]: /docs/api/imagedata.md#loadimagedata
[fetch_opts]: https://developer.mozilla.org/en-US/docs/Web/API/RequestInit
[export_outline]: /docs/api/canvas.md#outline
[sharp]: https://sharp.pixelplumbing.com
[canvas_toSharp]: /docs/api/canvas.md#tosharp
[id_toSharp]: /docs/api/imagedata.md#tosharp
[downsample]: /docs/api/canvas.md#downsample
[canvas_text_rendering]: /docs/api/canvas.md#controlling-font-rendering
[window_text_rendering]: /docs/api/window.md#controlling-font-rendering
[running_lambda]: /docs/getting-started.md#running-on-aws-lambda
[node_url]: https://nodejs.org/api/url.html#class-url
[meta_url]: https://nodejs.org/api/esm.html#importmetaurl
[Image.src]: /docs/api/image.md#src
[createTexture_cap]: /docs/api/context.md#cap
[createTexture_outline]: /docs/api/context.md#outline
[ctx_font]: /docs/api/context.md#font
[measureText.runs]: /docs/api/context.md#per-font-metrics
[Canvas.toFile]: /docs/api/canvas.md#tofile
[toDataURL]: https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/toDataURL
[toURL]: /docs/api/canvas.md#tourl
[gpu_opt]: /docs/api/canvas.md#choosing-a-rendering-engine
[image_constructor]: /docs/api/image.md#constructor

## 📦 ⟩ [v2.0.2] ⟩ Jan 27, 2025

### New Features

- Added `fontHinting` attribute (off by default to better match font weights in browser rendering). Setting it to `true` may result in crisper edges but adds some weight to the font.

### Bugfixes

- Text spacing
  - Setting `letterSpacing` no longer indents text at beginning of line
  - `letterSpacing` now properly handles negative values
- Improved accuracy of [`measureText()`][measureText()]
  - Now uses font metrics' default leading when the line-height is left unspecified in the `ctx.font` string (NB: this is likely to cause vertical shifts for non-`alphabetic` baselines)
  - Updated baseline offset calculations for `middle` & `hanging` to better match browsers
  - The `actualBoundingBox*` & `lines[].x/y/width/height` rectangles returned by measureText() are now just the glyph-occupied area, not the whole line-height of the textblock
  - Fixed the sign on `actualBoundingBoxLeft` (positive values now mean _left_ of the origin)
  - `lines[].baseline` now corresponds to the selected `ctx.textBaseline`, previously it was always the alphabetic baseline
- TypeScript definitions no longer include the entire DOM library (which had been pulling in tons of non-Canvas-related object types that this library doesn't emulate)

## 📦 ⟩ [v2.0.1] ⟩ Dec 8, 2024

### Misc. Improvements

- Added support for Intel integrated GPUs that would previously throw an "instantiated but unable to render" error
  - Note: you may need to upgrade to the latest Mesa drivers ([24.3.1 or later][mesa_ppa]), especially for in-window rendering to work correctly on Linux
- Fixed window initialization for Vulkan GPUs that default to a framebuffer color-format Skia doesn't support
- Vulkan drivers that fall back to the [Mesa LLVMpipe][mesa_llvmpipe] software renderer now work correctly
- Optimized font library initialization to improve SVG parsing speed

[mesa_ppa]: https://launchpad.net/~kisak/+archive/ubuntu/kisak-mesa
[mesa_llvmpipe]: https://docs.mesa3d.org/drivers/llvmpipe.html

## 📦 ⟩ [v2.0.0] ⟩ Dec 2, 2024

### New Features

#### Website

- Documentation is now hosted at [skia-canvas.org](https://skia-canvas.org). Go there for a more readable version of all the details that used to be wedged into the README file.

#### Imagery

- Added initial SVG rendering support. **Image**s can now load SVG files and can be drawn in a resolution-independent manner via [`drawImage()`][mdn_drawImage] (thanks to @mpaperno #180). Note that **Image**s loaded from SVG files that don't have a `width` and `height` set on their root `<svg>` element have some quirks as of this release:
  - The **Image** object's `height` will report being `150` and the `width` will be set to accurately capture the image's aspect ratio
  - When passed to `drawImage()` without size arguments, the SVG will be scaled to a size that fits within the **Canvas**'s current bounds (using an approach akin to CSS's `object-fit: contain`).
  - When using the 9-argument version of `drawImage()`, the ‘crop’ arguments (`sx`, `sy`, `sWidth`, & `sHeight`) will correspond to this scaled-to-fit size, _not_ the **Image**'s reported `width` & `height`.
- WEBP support
  - **Canvas**.[saveAs()][Canvas.toFile] & [toBuffer()][Canvas.toBuffer] can now generate WEBP images and **Image**s can load WEBP files as well (contributed by @mpaperno #177, h/t @revam for the initial work on this)
- Raw pixel data support
  - The `toBuffer()` and `saveAs()` methods now support `"raw"` as a format name and/or file extension, causing them to return non-encoded pixel data (by default in an `"rgba"` layout like a standard [ImageData][ImageData] buffer)
  - Both functions now take an optional [`colorType`][colorType] argument to specify alternative pixel data layouts (e.g., `"rgb"` or `"bgra"`)
- [**ImageData**][ImageData] enhancements
  - The [drawImage()][mdn_drawImage] and [createPattern()][mdn_createPattern] methods have been extended to accept **ImageData** objects as arguments. Previously only [putImageData()][mdn_putImageData] could be used for rendering, but this method ignores the context's current transform, filters, opacity, etc.
  - When creating an **ImageData** via the [getImageData()][mdn_getImageData] & [createImageData()][mdn_createImageData] methods or `new ImageData()` constructor, the optional settings arg now allows you to select the `colorType` for the buffer's pixels.

#### Typography

- **FontLibrary.**[use()][FontLibrary.use] now supports dynamically loaded [WOFF & WOFF2][woff_wiki] fonts
- The [`outlineText()`][outline_text] method now takes an optional `width` argument and supports all the context's typographic settings (e.g., `.font`, `.fontVariant`, `.textWrap`, `.textTracking`, etc.)
- Fonts with condensed/expanded widths can now be selected with the [`.fontStretch`][fontStretch] property. Note that stretch values included in the `.font` string will overwrite the current `.fontStretch` setting (or will reset it to `normal` if omitted).
- Generic font family names are now mapped to fonts installed on the system. The `serif`, `sans-serif`, `monospace`, and `system-ui` families are currently supported.
- Underlines, overlines, and strike-throughs can now be set via the **Context**'s `.textDecoration` property.
- Text spacing can now be fine-tuned using the [`.letterSpacing`][letterSpacing] and [`.wordSpacing`][wordSpacing] properties.

#### GUI

- The [**Window**][window] class now has a [`resizable`][resizable] property which can be set to `false` to prevent the window from being manually resized or maximized (contributed by @nornagon #124).
- **Window** [event handlers][win_bind] now support Input Method Editor events for entering composed characters via the [compositionstart][compositionstart], [compositionupdate][compositionupdate], & [compositionend][compositionend] events. The [`input`][input] event now reports the composed character, not the individual keystrokes.

#### Rendering

- The **Canvas** object has a new `engine` property which describes whether the CPU or GPU is being used, which graphics device was selected, and what (if any) error prevented it from being initialized.
- The `.transform` and `.setTransform` methods on **Context**, **Path2D**, and **CanvasPattern** objects can now take their arguments in additional formats. They can now be passed a [**DOMMatrix**][DOMMatrix] object or a string with a list of transformation operations compatible with the [CSS `transform`][css_transform] property. The **DOMMatrix** constructor also supports these strings as well as plain, matrix-like objects with numeric attributes named `a`, `b`, `c`, `d`, `e`, & `f` (contributed by @mpaperno #178).
- The number of background threads used for asynchronous exports can now be controlled with the [`SKIA_CANVAS_THREADS`][multithreading] environment variable

### Breaking Changes

- An upgrade to [Neon][neon_rs] with [N-API v8][node_napi] raised the minimum required Node version to 12.22+, 14.17+, or 16+.
- Images now load asynchronously in cases where the `src` property has been set to a local path. As a result, it's now necessary to `await img.decode()` or set up an `.on("load", …)` handler before drawing it—even when the `src` is non-remote.
- The **KeyboardEvent** object returned by the `keyup`/`keydown` and `input` event listeners now has fields and values consistent with browser behavior. In particular, `code` is now a name (e.g., `ShiftLeft` or `KeyS`) rather than a numeric scancode, `key` is a straightforward label for the key (e.g., `Shift` or `s`) and the new [`location`][key_location] field provides a numeric description of which variant of a key was pressed.
- The deprecated `.async` property has been removed. See the [v0.9.28](#--v0928--jan-12-2022) release notes for details.
- The non-standard `.textTracking` property has been removed in favor of the new [`.letterSpacing`][letterSpacing] property

### Bugfixes

- Initializing a GPU-renderer using Vulkan now uses the [`vulkano`](https://crates.io/crates/vulkano) crate and makes better selections among devices present (previously it was just using the first result, which is not always optimal).
- The **Image**.onload callback now properly sets `this` to point to the new image (contributed by @mpaperno & @ForkKILLET).
- Creating a **Window** with `fullscreen` set to `true` now takes effect immediately (previously it was failing silently)
- Drawing paths after setting an invalid transform no longer crashes (contributed by @mpaperno #175)
- Windows with `.on("draw")` handlers no longer [become unresponsive](https://github.com/gfx-rs/gfx/issues/2460) on macOS 14+ after being fully occluded by other windows
- Ellipses with certain combinations of positive and negative start- and stop-angles now render correctly—previously they would not appear at all if the total sweep exceeded 360° (contributed by @mpaperno #176)
- The `drawCanvas()` method now clips to the specified crop size (contributed by @mpaperno #179)
- Hit-testing with [`isPointInPath`][isPointInPath()] and [`isPointInStroke`][isPointInStroke()] now works correctly when called with a **Path2D** object as the first argument

### Misc. Improvements

- Upgraded Skia to [milestone 131](https://github.com/rust-skia/rust-skia/releases/tag/0.80.0)
- Added TypeScript definitions for the **Window** object’s event types (contributed by @saantonandre #163) and the `roundRect` method (contributed by @sandy85625 & @santilema)
- Performance improvements to **FontLibrary**, speeding up operations like listing families and adding new typefaces.
- Updated `winit` and replaced the end-of-life’d [skulpin](https://github.com/aclysma/skulpin)-based Vulkan renderer with a new implementation using Vulkano for window-drawing on Windows and Linux.
  > It’s a fairly direct adaptation of Vulkano [sample code][vulkano_demo] for device setup with skia-specific rendering routines inspired by [@pragmatrix](https://github.com/pragmatrix)’s renderer for [emergent][pragmatrix_emergent]. All of which is to say, if you understand this better than I do I'd love some suggestions for improving the rendering setup.
- The GPU is now initialized only when it is needed, not at startup. As a result, setting that **Canvas**'s [`.gpu`][canvas_gpu] property to `false` immediately after creation will prevent any GPU-related resource acquisition from occurring (though rendering speed will be predictably slower).
- The sample-count used by the GPU for multiscale antialiasing can now be configured through the optional [`msaa`][msaa] export argument. If omitted, defaults to 4x MSAA.
- Added support for non-default imports (e.g., `import {Image} from "skia-canvas"`) when used as an ES Module.
- The [getImageData()][mdn_getImageData] method now makes use of the GPU (if enabled) and caches data between calls, greatly improving performance for sequential queries

[resizable]: /docs/api/window.md#resizable
[key_location]: https://developer.mozilla.org/en-US/docs/Web/API/KeyboardEvent/location
[vulkano_demo]: https://github.com/vulkano-rs/vulkano/blob/master/examples/triangle/main.rs
[pragmatrix_emergent]: https://github.com/pragmatrix/emergent/blob/master/src/skia_renderer.rs
[woff_wiki]: https://en.wikipedia.org/wiki/Web_Open_Font_Format
[css_transform]: https://developer.mozilla.org/en-US/docs/Web/CSS/transform
[DOMMatrix]: https://developer.mozilla.org/en-US/docs/Web/API/DOMMatrix
[FontLibrary.use]: /docs/api/font-library.md#use
[Canvas.toFile]: /docs/api/canvas.md#tofile
[Canvas.toBuffer]: /docs/api/canvas.md#tobuffer
[letterSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/letterSpacing
[wordSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/wordSpacing
[fontStretch]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/fontStretch
[isPointInPath()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/isPointInPath
[isPointInStroke()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/isPointInStroke
[node_napi]: https://nodejs.org/api/n-api.html#node-api-version-matrix
[neon_rs]: https://neon-rs.dev
[msaa]: /docs/api/canvas.md#msaa
[multithreading]: /docs/getting-started.md#multithreading
[compositionstart]: https://developer.mozilla.org/en-US/docs/Web/API/Element/compositionstart_event
[compositionupdate]: https://developer.mozilla.org/en-US/docs/Web/API/Element/compositionupdate_event
[compositionend]: https://developer.mozilla.org/en-US/docs/Web/API/Element/compositionend_event
[input]: https://developer.mozilla.org/en-US/docs/Web/API/HTMLElement/input_event
[win_bind]: /docs/api/window.md#on--off--once
[ImageData]: /docs/api/imagedata.md
[colorType]: /docs/api/imagedata.md#colortype
[mdn_createPattern]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createPattern
[mdn_getImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/getImageData
[mdn_createImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createImageData
[mdn_putImageData]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/putImageData

## 📦 ⟩ [v1.0.2] ⟩ Aug 21, 2024

### Maintenance

- After getting a surprise bill from Amazon for the S3 bucket hosting the pre-compiled binaries, I've moved them to GitHub Releases instead. Aside from resolving some security warnings by upgrading dependencies, this version _should_ be functionally identical to 1.0.1…

### Breaking Changes

- The 32-bit ARM-based linux builds are no longer provided pre-compiled; you'll now need to build from source.

## 📦 ⟩ [v1.0.1] ⟩ Oct 15, 2022

### Bugfixes

- If an offscreen buffer can't be allocated using the Vulkan renderer, CPU rendering is used as a fallback
- The `drawCanvas()` routine now works even when the destination canvas is later saved as an SVG (previously, the source canvas would be missing from the output). Caveat: this only works if the destination canvas is using the default `source-over` blend mode, has its `globalAlpha` set to 1, and is not using shadows or the `effect` property. If any of those defaults have been changed, the drawn canvas will not appear in the saved SVG. Bitmap and PDF exports do not have this restriction.

### Misc. Improvements

- Added a `fullscreen` event to the `Window` class to flag changes into and out of full-screen mode.

## 📦 ⟩ [v1.0.0] ⟩ Aug 5, 2022

### New Features

- The new [Window][window] class can display a **Canvas** on screen, respond to mouse and keyboard input, and fluidly [animate][window_anim] by calling user-defined [event handlers][window_events].
- Bitmap rendering now occurs on the GPU by default and can be configured using the **Canvas**'s [`.gpu`][canvas_gpu] property. If the platform supports hardware-accelerated rendering (using Metal on macOS and Vulkan on Linux & Windows), the property will be `true` by default and can be set to `false` to use the software renderer.
- Added support for recent Chrome features:
  - the [`reset()`][chrome_reset] context method which erases the canvas, resets the transformation state, and clears the current path
  - the [`roundRect()`][chrome_rrect] method on contexts and **Path2D** objects which adds a rounded rectangle using 1–4 corner radii (provided as a single value or an array of numbers and/or **DOMPoint** objects)

### Bugfixes

- The `FontLibrary.reset()` method didn't actually remove previously installed fonts that had already been drawn with (and thus cached). It now clears those caches, which also means previously used fonts can now be replaced by calling `.use()` again with the same family name.
- The [`.drawCanvas()`][drawCanvas] routine now applies filter effects and shadows consistent with the current resolution and transformation state.

### Misc. Improvements

- The [`.filter`][filter] property's `"blur(…)"` and `"drop-shadow(…)"` effects now match browser behavior much more closely and scale appropriately with the `density` export option.
- Antialiasing is smoother, particularly when down-scaling images, thanks to the use of mipmaps rather than Skia's (apparently buggy?) implementation of bicubic interpolation.
- Calling `clearRect()` with dimensions that fully enclose the canvas will now discard all the vector objects that have been drawn so far (rather than simply covering them up).
- Upgraded Skia to milestone 103

[window]: /docs/api/window.md
[window_anim]: /docs/api/window.md#events-for-animation
[window_events]: /docs/api/window.md#on--off--once
[canvas_gpu]: /docs/api/canvas.md#gpu
[filter]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/filter
[chrome_reset]: https://developer.chrome.com/blog/canvas2d/#context-reset
[chrome_rrect]: https://developer.chrome.com/blog/canvas2d/#round-rect

## 📦 ⟩ [v0.9.30] ⟩ Jun 7, 2022

### New Features

- Enhacements to the shared **FontLibrary** object:
  - Added a [`reset()`][FontLibrary.reset] method to FontLibrary which uninstalls any fonts that had been dynamically installed via `FontLibrary.use()`
  - The [`use()`][FontLibrary.use] method now checks for previously installed fonts with the same family name (or alias) and will replace them with the newly added font
- Added pre-compiled binaries for Alpine Linux on arm64

### Bugfixes

- Calling `clip` with an empty path (or one that does not intersect the current clipping mask) will now prevent drawing altogether
- Transformation (`translate`, `rotate`, etc.) and line-drawing methods (`moveTo`, `lineTo`, `ellipse`, etc.) are now silently ignored if called with `NaN`, `Infinity`, or non-**Number** values in the arguments rather than throwing an error
  - applies to both the Context and Path2D versions of the drawing methods
  - a **TypeError** is thrown only if the number of arguments is too low (mirroring browser behavior)
- [`conicCurveTo()`][conicCurveTo] now correctly reflects the canvas's transform state
- The browser-based version of [`loadImage()`][loadImage()] now returns a **Promise** that correctly resolves to an **Image** object
- SVG exports no longer have an invisible, canvas-sized `<rect/>` as their first element
- Fixed an incompatibility on Alpine between the version of libstdc++ present on the `node:alpine` docker images and the version used when building the precompiled binaries

### Misc. Improvements

- Upgraded Skia to milestone 101

[conicCurveTo]: /docs/api/context.md#coniccurveto
[FontLibrary.reset]: /docs/api/font-library.md#reset

## 📦 ⟩ [v0.9.29] ⟩ Feb 7, 2022

### New Features

- PDF exports now support the optional [`matte`][matte] argument.

### Breaking Changes

- When the [`drawImage()`][mdn_drawImage] function is passed a **Canvas** object as its image source it will now rasterize the canvas before drawing. The prior behavior (in which it is drawn as a vector graphic) can now be accessed through the new [`drawCanvas()`][drawCanvas] method which supports the same numerical arguments as `drawImage` but requires that its first argument be a **Canvas**.

### Bugfixes

- Regions erased using [`clearRect()`][mdn_clearRect] are now properly antialiased
- The [`clip()`][mdn_clip] method now interprets the current translate/scale/rotate state correctly when combining clipping masks

### Misc. Improvements

- Upgraded Skia to milestone 97

[drawCanvas]: /docs/api/context.md#drawcanvas
[mdn_clip]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/clip
[mdn_clearRect]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/clearRect

## 📦 ⟩ [v0.9.28] ⟩ Jan 12, 2022

### New Features

- Added TypeScript definitions for extensions to the DOM spec (contributed by [@cprecioso](https://github.com/cprecioso))
- Added 3D-perspective transformations via the new [createProjection()][createProjection()] context method
- Colors can now use the [hwb()](<https://developer.mozilla.org/en-US/docs/Web/CSS/color_value/hwb()>) model

### Breaking Changes

- The **Canvas** [`.async`][async_depr] property has been **deprecated** and will be removed in a future release.
  - The `saveAs`, `toBuffer`, and `toDataURL` methods will now be async-only (likewise the [shorthand properties][shorthands]).
  - Use their synchronous counterparts (`saveAsSync`, `toBufferSync`, and `toDataURLSync`) if you want to block execution while exporting images.
- The [ImageData](https://developer.mozilla.org/en-US/docs/Web/API/ImageData/ImageData) constructor now orders its arguments properly: the optional buffer/array argument now comes first

### Bugfixes

- Fixed a stack overflow that was occurring when images became too deeply nested for the default deallocator to handle (primarily due to many thousands of image exports from the same canvas)
- The `source-in`, `source-out`, `destination-atop`, and `copy` composite operations now work correctly for paths rather than rendering shapes without color (contributed by [@meihuanyu](https://github.com/meihuanyu))
- Shape primitives now behave consistently with browsers when being added to a non-empty path:
  - `rect()` now issues an initial `moveTo` rather than extending the path, then leaves the ‘current’ point in its upper left corner
  - `ellipse()` extends the current path rather than implicitly closing it (contributed by [@meihuanyu](https://github.com/meihuanyu))
  - `arc()` also extends the current path rather than closing it

### Misc. Improvements

- Upgraded Skia to milestone 96
- Added workflow for creating docker build environments

[createProjection()]: /docs/api/context.md#createprojection
[shorthands]: /docs/api/canvas.md#pdf-svg-png-jpg-webp--raw
[async_depr]: https://github.com/samizdatco/skia-canvas/tree/v0.9.28#async

## 📦 ⟩ [v0.9.27] ⟩ Oct 23, 2021

### New Features

- Added pre-compiled binaries for Alpine Linux using the [musl](https://musl.libc.org) C library

## 📦 ⟩ [v0.9.26] ⟩ Oct 18, 2021

### New Features

- Added pre-compiled binaries for 32-bit and 64-bit ARM on Linux (a.k.a. Raspberry Pi)

### Bugfixes

- Windows text rendering has been restored after failing due to changes involving the `icudtl.dat` file
- `FontLibrary.use` now reports an error if the specified font file doesn't exist
- Fixed a crash that could result from calling `measureText` with various unicode escapes

### Misc. Improvements

- Upgraded Skia to milestone 94
- Now embedding a more recent version of the FreeType library on Linux with support for more font formats

## 📦 ⟩ [v0.9.25] ⟩ Aug 22, 2021

### Bugfixes

- Improved image scaling when a larger image is being shrunk down to a smaller size via [`drawImage()`][mdn_drawImage]
- modified [`imageSmoothingQuality`](https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/imageSmoothingQuality) settings to provide a more meaningful range across `low`, `medium`, and `high`
- [`measureText()`][measureText()] now returns correct metrics regardless of current `textAlign` setting
- Rolled back `icudtl.dat` changes on Windows (which suppressed the misleading warning message but required running as Administrator)

### Misc. Improvements

- Now using [Neon](https://github.com/neon-bindings/neon) v0.9 (with enhanced async event scheduling)

[mdn_drawImage]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/drawImage
[measureText()]: /docs/api/context.md#measuretext

## 📦 ⟩ [v0.9.24] ⟩ Aug 18, 2021

### New Features

- **Path2D** objects now have a read/write [`d`][p2d_d] property with an [SVG representation](https://developer.mozilla.org/en-US/docs/Web/SVG/Attribute/d#path_commands) of the path’s contours and an [`unwind()`][p2d_undwind] method for converting from even-odd to non-zero winding rules
- The [`createTexture()`][createTexture()] context method returns **CanvasTexture** objects which can be assigned to `fillStyle` or `strokeStyle`
- Textures draw either a parallel-lines pattern or one derived from the provided **Path2D** object and positioning parameters
- The marker used when `setLineDash` is active can now be customized by assigning a **Path2D** to the context’s [`lineDashMarker`][lineDashMarker] property (default dashing can be restored by assigning `null`)
- The marker’s orientation & shape relative to the path being stroked can be controlled by the [`lineDashFit`][lineDashFit] property which defaults to `"turn"` but can be set to `"move"` (which preserves orientation) or `"follow"` (which distorts the marker’s shape to match the contour)

[p2d_d]: /docs/api/path2d.md#d
[p2d_unwind]: /docs/api/path2d.md#unwind
[createTexture()]: /docs/api/context.md#createtexture
[lineDashMarker]: /docs/api/context.md#linedashmarker
[lineDashFit]: /docs/api/context.md#linedashfit

### Bugfixes

- Removed use of the `??` operator which is unavailable prior to Node 14
- Prevented a spurious warning on windows incorrectly claiming that the `icudtl.dat` file could not be found

### Misc. Improvements

- The **Path2D** [`simplify()`][simplify] method now takes an optional fill-rule argument
- Added support for versions of macOS starting with 10.13 (High Sierra)

## 📦 ⟩ [v0.9.23] ⟩ Jul 12, 2021

### New Features

- [Conic béziers][conic_bezier] can now be drawn to the context or a Path2D with the [`conicCurveTo()`][conicCurveTo] method
- Text can be converted to a Path2D using the context’s new [`outlineText()`][outline_text] method
- Path2D objects can now report back on their internal geometry with:
  - the [`edges`][edges] property which contains an array of line-drawing commands describing the path’s individual contours
  - the [`contains()`][contains] method which tests whether a given point is on/within the path
  - the [`points()`][points] method which returns an array of `[x, y]` pairs at the requested spacing along the curve’s periphery
- A modified copy of a source Path2D can now be created using:
  - [`offset()`][offset] or [`transform()`][transform] to shift position or apply a DOMMatrix respectively
  - [`jitter()`][jitter] to break the path into smaller sections and apply random noise to the segments’ positions
  - [`round()`][round] to round off every sharp corner in a path to a particular radius
  - [`trim()`][trim] to select a percentage-based subsection of the path
- Two similar paths can be ‘tweened’ into a proportional combination of their coordinates using the [`interpolate()`][interpolate] method

### Bugfixes

- Passing a Path2D argument to the `fill()` or `stroke()` method no longer disturbs the context’s ‘current’ path (if one has been created using `beginPath()`)
- The `filter` property will now accept percentage values greater than 999%

### Misc. Improvements

- The `newPage()` and `saveAs()` methods now work in the browser, including the ability to save image sequences to a zip archive. The browser’s canvas is still doing all the drawing however, so file export formats will be limited to PNG and JPEG and none of the other Skia-specific extensions will be available.
- The file-export methods now accept a [`matte`][matte] value in their options object which can be used to set the background color for any portions of the canvas that were left semi-transparent
- Canvas dimensions are no longer rounded-off to integer values (at least until a bitmap needs to be generated for export)
- Linux builds will now run on some older systems going back to glibc 2.24

[conic_bezier]: https://docs.microsoft.com/en-us/xamarin/xamarin-forms/user-interface/graphics/skiasharp/curves/beziers#the-conic-bézier-curve
[conic_curveto]: https://github.com/samizdatco/skia-canvas#coniccurvetocpx-cpy-x-y-weight
[outline_text]: /docs/api/context.md#outlinetext
[matte]: /docs/api/canvas.md#matte
[edges]: /docs/api/path2d.md#edges
[contains]: /docs/api/path2d.md#contains
[points]: /docs/api/path2d.md#points
[offset]: /docs/api/path2d.md#offset
[transform]: /docs/api/context.md#transform--settransform
[interpolate]: /docs/api/path2d.md#interpolate
[jitter]: /docs/api/path2d.md#jitter
[round]: /docs/api/path2d.md#round
[simplify]: /docs/api/path2d.md#simplify
[trim]: /docs/api/path2d.md#trim

## 📦 ⟩ [v0.9.22] ⟩ Jun 09, 2021

### New Features

- Rasterization and file i/o are now handled asynchronously in a background thread. See the discussion of Canvas’s new [`async`][async_orig] property for details.
- Output files can now be generated at pixel-ratios > 1 for High-DPI screens. `SaveAs` and the other canvas output functions all accept an optional [`density`][density] argument which is an integer ≥1 and will upscale the image accordingly. The density can also be passed using the `filename` argument by ending the name with an ‘@’ suffix like `some-image@2x.png`.
- SVG exports can optionally convert text to paths by setting the [`outline`][outline] argument to `true`.

### Breaking Changes

- The canvas functions dealing with rasterization (`toBuffer`, `toDataURL`, `png`, `jpg`, `pdf`, and `svg`) and file i/o (`saveAs`) are now asynchronous and return `Promise` objects. The old, synchronous behavior is still available on a canvas-by-canvas basis by setting its `async` property to `false`.
- The optional `quality` argument accepted by the output methods is now a float in the range 0–1 rather than an integer from 0–100. This is consistent with the [encoderOptions](https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/toDataURL) arg in the spec. Quality now defaults to 0.92 (again, as per the spec) rather than lossless.

### Bugfixes

- `measureText` was reporting zero when asked to measure a string that was entirely made of whitespace. This is still the case for ‘blank‘ lines when `textWrap` is set to `true` but in the default, single-line mode the metrics will now report the width of the whitespace.
- Changed the way text rendering was staged so that SVG exports didn’t _entirely omit(!)_ text from their output. As a result, `Context2D`s now use an external `Typesetter` struct to manage layout and rendering.

[density]: /docs/api/canvas.md#density
[outline]: /docs/api/canvas.md#outline
[async_orig]: https://github.com/samizdatco/skia-canvas/tree/v0.9.22#async

## 📦 ⟩ [v0.9.21] ⟩ May 22, 2021

### New Features

- Now runs on Windows and Apple Silicon Macs.
- Precompiled binaries support Node 10, 12, 14+.
- Image objects can be initialized from PNG, JPEG, GIF, BMP, or ICO data.
- Path2D objects can now be combined using [boolean operators][boolean-ops] and can measure their own [bounding boxes][p2d_bounds].
- Context objects now support [`createConicGradient()`][createConicGradient()].
- Image objects now return a promise from their [`decode()`](https://developer.mozilla.org/en-US/docs/Web/API/HTMLImageElement/decode) method allowing for async loading without the [`loadImage`][loadImage()] helper.

### Bugfixes

- Calling `drawImage` with a `Canvas` object as the argument now uses a Skia `Pict` rather than a `Drawable` as the interchange format, meaning it can actually respect the canvas's current `globalAlpha` and `globalCompositeOperation` state (fixed #6).
- Improved some spurious error messages when trying to generate a graphics file from a canvas whose width and/or height was set to zero (fixed #5).
- `CanvasPattern`s now respect the `imageSmoothingEnabled` setting
- The `counterclockwise` arg to `ellipse` and `arc` is now correctly treated as optional.

### Misc. Improvements

- Made the `console.log` representations of the canvas-related objects friendlier.
- Added new test suites for `Path2D`, `Image`, and `Canvas`’s format support.
- Created [workflows](https://github.com/samizdatco/skia-canvas/tree/master/.github/workflows) to automate precompiled binary builds, testing, and npm package updating.

[boolean-ops]: /docs/api/path2d.md#complement-difference-intersect-union-and-xor
[p2d_bounds]: /docs/api/path2d.md#bounds
[createConicGradient()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createConicGradient
[loadImage()]: /docs/api/image.md#loadimage

## 📦 ⟩ [v0.9.20] ⟩ Mar 27, 2021

### Bugfixes

- The `loadImage` helper can now handle `Buffer` arguments

### Misc. Improvements

- Improved documentation of compilation steps and use of line height with `ctx.font`

## 📦 ⟩ [v0.9.19] ⟩ Aug 30, 2020

**Initial public release** 🎉

[unreleased]: https://github.com/samizdatco/skia-canvas/compare/v3.0.8...HEAD
[v3.0.8]: https://github.com/samizdatco/skia-canvas/compare/v3.0.7...v3.0.8
[v3.0.7]: https://github.com/samizdatco/skia-canvas/compare/v3.0.6...v3.0.7
[v3.0.6]: https://github.com/samizdatco/skia-canvas/compare/v3.0.5...v3.0.6
[v3.0.5]: https://github.com/samizdatco/skia-canvas/compare/v3.0.4...v3.0.5
[v3.0.4]: https://github.com/samizdatco/skia-canvas/compare/v3.0.3...v3.0.4
[v3.0.3]: https://github.com/samizdatco/skia-canvas/compare/v3.0.2...v3.0.3
[v3.0.2]: https://github.com/samizdatco/skia-canvas/compare/v3.0.1...v3.0.2
[v3.0.1]: https://github.com/samizdatco/skia-canvas/compare/v3.0.0...v3.0.1
[v3.0.0]: https://github.com/samizdatco/skia-canvas/compare/v2.0.2...v3.0.0
[v2.0.2]: https://github.com/samizdatco/skia-canvas/compare/v2.0.1...v2.0.2
[v2.0.1]: https://github.com/samizdatco/skia-canvas/compare/v2.0.0...v2.0.1
[v2.0.0]: https://github.com/samizdatco/skia-canvas/compare/v1.0.2...v2.0.0
[v1.0.2]: https://github.com/samizdatco/skia-canvas/compare/v1.0.1...v1.0.2
[v1.0.1]: https://github.com/samizdatco/skia-canvas/compare/v1.0.0...v1.0.1
[v1.0.0]: https://github.com/samizdatco/skia-canvas/compare/v0.9.30...v1.0.0
[v0.9.30]: https://github.com/samizdatco/skia-canvas/compare/v0.9.29...v0.9.30
[v0.9.29]: https://github.com/samizdatco/skia-canvas/compare/v0.9.28...v0.9.29
[v0.9.28]: https://github.com/samizdatco/skia-canvas/compare/v0.9.27...v0.9.28
[v0.9.27]: https://github.com/samizdatco/skia-canvas/compare/v0.9.26...v0.9.27
[v0.9.26]: https://github.com/samizdatco/skia-canvas/compare/v0.9.25...v0.9.26
[v0.9.25]: https://github.com/samizdatco/skia-canvas/compare/v0.9.24...v0.9.25
[v0.9.24]: https://github.com/samizdatco/skia-canvas/compare/v0.9.23...v0.9.24
[v0.9.23]: https://github.com/samizdatco/skia-canvas/compare/v0.9.22...v0.9.23
[v0.9.22]: https://github.com/samizdatco/skia-canvas/compare/v0.9.21...v0.9.22
[v0.9.21]: https://github.com/samizdatco/skia-canvas/compare/v0.9.20...v0.9.21
[v0.9.20]: https://github.com/samizdatco/skia-canvas/compare/v0.9.19...v0.9.20
[v0.9.19]: https://github.com/samizdatco/skia-canvas/compare/v0.9.15...v0.9.19
