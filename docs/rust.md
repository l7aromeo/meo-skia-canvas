---
description: Using the Rust crate -- how the surface is shaped and where it differs from the JavaScript one
---

# `meo_skia_canvas` -- the Rust crate

The counterpart of [`node.md`](node.md), for the other front door. It covers what a reader cannot
learn one item at a time: how the two surfaces relate, what the crate promises not to expose, how
colour and pages behave across every call, and what fails.

**The per-item reference is [docs.rs][docs-rs]**, generated from the source and versioned with each
release, so it cannot drift from the code the way a hand-written list does. Where this page names a
type, follow it there for the signatures.

[docs-rs]: https://docs.rs/meo-skia-canvas

Every public type is reachable straight off the crate root:

```rust
use meo_skia_canvas::{Canvas, CanvasOptions, EncodeOptions, FillRule, PathBuilder};
```

The modules (`canvas`, `context2d`, `color`, `export`, `filter`, `font`, `geometry`, `image`, `paint`, `path`, `pattern`, `pixels`, `shader`, `text`, `texture`, `error`) group the same types by subject, which is how they are documented, and `meo_skia_canvas::prelude::*` globs the lot for anyone who prefers a prelude. One draw usually reaches across several modules -- `Canvas::to_buffer` alone speaks `ImageFormat`, `EncodeOptions`, the pixel types and `Error` -- so nothing requires knowing which module a type lives in.

The Node/Neon binding lives under the internal `node` module; it exists for Node compatibility, intentionally leaks `skia_safe` and Neon types, and is `pub(crate)` -- not a surface for Rust consumers.

## One API, two front doors

`Canvas` + `Context2D` mirror the HTML Canvas API: method names and argument order match `CanvasRenderingContext2D`, a mutable graphics state carries fill style, transform and clip, and `to_buffer` / `to_file` encode to PNG, JPEG, WebP, GIF, APNG, TIFF, ICO, BMP, AVIF, PDF or SVG.

There is no second, lower layer. An earlier fork carried a parallel `Surface` / `Recorder` / `DrawTarget` API for an external consumer that never materialised; nothing in this crate used it, and it was removed. What it could do, `Canvas` and `Context2D` do -- including mask filters, the shader factories, image sampling modes, bounded layers and variable-font axes.

For text laid out ahead of a draw, `TextEngine` and `FontLibrary` are the paragraph-level entry points; `Context2D::draw_paragraph` puts the result on a canvas.

## Stability commitment

- Public types do **not** expose `skia_safe`, `neon`, `RefCell`, `FunctionContext`, `JsBox`, or `Handle<...>`. This covers the whole crate, `gui` included, not only the crate-root modules.
- `skia_safe` remains a private implementation detail. Wrapping or aliasing Skia types in `pub` signatures is treated as an API regression.
- Checked by `just check-api`, which walks rustdoc's JSON for every item reachable from the crate root — including methods, enum variant payloads and tuple-struct fields — and fails on a leak, with no module exempted. A grep cannot answer this: it reads the source rather than the resolved API, so it misses re-exports and type aliases, and rustdoc's HTML renders `skia_safe::Color` as a bare `Color`.

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
- The compositing format follows the _canvas_, never the call: `PixelExportOptions { depth: F32 }` on an eight-bit canvas reads back float pixels holding eight-bit values, rather than quietly recompositing the page.
- A float canvas renders on the raster backend whatever `CanvasOptions::gpu` says, and `Canvas::engine_kind` reports which one took it. No GPU can currently give that precision: Skia's Metal and Vulkan backends implement no 32-bit float surface, and while both provide `F16`, a GPU quantises the paint colour to eight bits before compositing -- the same sixty layers land on 0.235, further from right than the eight-bit 0.361. Metal and Vulkan return that figure to the digit, which is what makes it Skia's limit rather than a driver's.
- The capability is probed once at runtime rather than assumed, so a Skia that grows the support keeps these canvases on the GPU with no change here.
- A readback with no layout of its own -- `get_image_data` -- takes both the canvas's space and its format, and reports them on the `ImageData` it returns. `get_image_data_as` overrides either. This is what a browser does: `getImageData()` on a Display P3 canvas hands back P3 components.
- `EncodeOptions::color_space` is an `Option<PixelColorSpace>`: the space an export converts _into_, where `None` means the canvas's own. Requesting a wider one re-expresses what the surface holds; it cannot widen it.
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

For building an `RgbaLinear` directly rather than through a paint style:

- `RgbaLinear::opaque(r, g, b)` and `new_premultiplied(r, g, b, a)` take linear-light components -- the second one already multiplied through by alpha, as the name says.
- `from_srgb(r, g, b, alpha)` and `from_srgb8(r, g, b, alpha)` take **sRGB** components, as floats and as bytes, and convert. This is the pair to reach for when porting JavaScript: `fillStyle = "#808080"` is `RgbaLinear::from_srgb8(0x80, 0x80, 0x80, 1.0)`, not the same three numbers handed to `opaque`, which would be a different, lighter grey.
- `from_hex(hex)` parses the CSS notations -- `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, the leading `#` optional -- and returns `Error::InvalidColor` for anything else.
- `with_opacity(opacity)` scales every component, alpha included, so the result stays premultiplied.
- `fading_out()` returns the colour at zero alpha with its hue intact. `with_opacity(0.0)` multiplies the channels away, which is what premultiplication means and is right wherever a colour is painted -- at zero alpha nothing is drawn, so the hue cannot matter. It matters in one place, a gradient stop, because there the colour is interpolated _toward_ rather than painted: multiplied away, a transparent cream is the same four zeros as CSS's `transparent`, which is a transparent _black_, and the gradient fades toward black instead of toward cream.

## Colour spaces

`PixelColorSpace` is the one vocabulary, used for the canvas, for readbacks and for exports:

| Variant           | Primaries  | Transfer function | JavaScript name                   |
| ----------------- | ---------- | ----------------- | --------------------------------- |
| `Srgb`            | sRGB       | sRGB              | `srgb`                            |
| `SrgbLinear`      | sRGB       | linear            | `srgb-linear`, `linear`           |
| `DisplayP3`       | Display P3 | sRGB              | `display-p3`, `p3`                |
| `DisplayP3Linear` | Display P3 | linear            | `display-p3-linear`, `p3-linear`  |
| `Rec2020`         | Rec. 2020  | Rec. 709          | `rec2020`, `bt2020`               |
| `Rec2020Linear`   | Rec. 2020  | linear            | `rec2020-linear`, `bt2020-linear` |
| `Rec2020Pq`       | Rec. 2020  | PQ                | `rec2020-pq`, `hdr10`             |
| `Rec2020Hlg`      | Rec. 2020  | HLG               | `rec2020-hlg`, `hlg`              |

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

None of them is `#[non_exhaustive]`, deliberately. That attribute forbids the struct expression _including_ the `..Default::default()` form, so every construction would become a `let mut` followed by a field assignment per override -- measured at 82 sites in this repository alone. It buys protection the rest pattern already provides.

## Premultiplied alpha

- `RgbaLinear` channel values are **premultiplied** linear-light RGBA. `RgbaLinear::opaque(1.0, 0.5, 0.5)` is opaque; `RgbaLinear::new_premultiplied(0.5, 0.0, 0.0, 0.5)` is half-alpha red.
- Canvases composite in premultiplied alpha space.
- `Context2D::get_image_data` returns **unpremultiplied** components -- the wire format `putImageData` expects, and what a browser hands back.
- `get_image_data_as(PixelExportOptions { premultiplied: true, .. })` keeps the premultiplied values, and takes the depth and colour space alongside.

## Pixel formats and depths

`PixelFormat` names the layout a raw image is created from, and `PixelDepth` the bit depth a readback comes back at and a canvas composites in -- the variants and what each one costs are on [docs.rs][docs-rs], which is where they stay current. This page listed three of `PixelDepth`'s variants and was still listing three after it grew to twenty-four.

`PixelExportOptions { color_space, depth, premultiplied }` is the explicit handshake; the three combine orthogonally, and an unsupported combination returns a typed `Error`.

## Pages

A canvas holds one or more pages, and each page is a recording materialised at export time.

- `Canvas::context()` borrows the current page's `Context2D`.
- `new_page` / `new_page_with` start another, and `page_count` / `page` select among them.
- `EncodeOptions::page` picks which one an export encodes; PDF encodes all of them.
- `EncodeOptions::page_range` picks a span of them, as `Option<Range<usize>>` -- zero-based and end-excluded, as a Rust range is, where `page` is zero-based too and the JavaScript `pageRange` counts from one and includes both ends. Each side counts the way its own language does. Naming it alongside `page` is an `Error::InvalidExportOption`, as is an empty range, a range past the last page, and a range on a format that encodes a single page.
- `Canvas::set_size(width, height)` resizes and **clears** the current page, which is what assigning `canvas.width` does in a browser -- the drawing is discarded rather than rescaled or cropped. Pages added earlier keep the size they had.

## Exports

`to_buffer(format, &options)` returns the encoded bytes, `to_file(path, &options)` writes them with the format taken from the path's extension -- an unrecognized or absent one is an error rather than a silent PNG -- and `to_data_url(format, &options)` returns the same bytes base64-encoded behind their media type, ready for an `<img src>` or a CSS `url()`. Base64 costs a third more bytes than the buffer it wraps.

A format that spans pages emits all of them as one file: PDF, TIFF, ICO and the three animated formats. The rest encode the page `Canvas::context` currently hands back, unless `EncodeOptions::page` names another.

`EncodeOptions::page_range` narrows that to a span. It is what lets one canvas produce an introduction that plays once and a cycle that repeats forever -- a file carries a single loop count, so the two halves cannot be one file -- and it serves the paged documents as much as the animations, pulling one chapter out of a long PDF. The pages are sliced before the encoder is built rather than skipped as it runs: WebP codes each frame as the rectangle it differs from its predecessor in, so a range whose first page still had a predecessor would open on a rectangle diffed against a page the file does not carry.

`ImageFormat` answers what it is without a match of your own: `mime_type()`, `extension()`, `is_vector()`, and `ImageFormat::from_extension(ext)` which returns `Option<Self>` for a name or extension a caller supplied.

### Animation

WebP, GIF and APNG take the canvas's pages as their frames, timed by three `EncodeOptions` fields:

- `fps: Option<f32>` is the rate the pages play at, defaulting to 30. GIF stores hundredths of a second, so its frame times round to the nearest 10ms, with the rounding spread so the average rate stays right.
- `frame_delays: Vec<u32>` overrides `fps` with a duration in milliseconds per page. It must be empty or hold exactly one entry per page; any other length is `Error::InvalidExportOption` rather than a silent retiming. This is the field `Image::frame_delays()` feeds directly, which is what makes re-encoding an animation possible.
- `loops: Option<u32>` is how many times it plays, `0` meaning forever, as both GIF and APNG spell it. `1` is the count GIF cannot state outright -- its loop block's zero already means forever, so a single play is spelled by omitting the block, and decoders may report either answer.

The three are read only by the formats that animate. Setting them on a PNG or a PDF does nothing here, where the JavaScript binding refuses the call: a Rust caller building `EncodeOptions` from a default and reusing it across formats would otherwise have to strip fields per format. `fps` is still validated as a positive number, and `frame_delays` as one entry per page, whatever the format.

## Windows

Behind the `window` feature, plus a GPU backend -- `gui` needs a renderer, so `window` alone does not build.

```rust
use meo_skia_canvas::prelude::*;

let mut win = Window::new(480.0, 320.0);
win.set_title("hello");

win.on_event(|event| {
    if let UiEvent::Keyboard { code: Key::Escape, .. } = event {
        App::quit();
    }
});

win.on_draw(|ctx, frame| {
    ctx.set_fill_style_css("skyblue").ok();
    ctx.fill_rect(0.0, 0.0, 100.0 + frame as f32, 100.0);
});

win.open();
App::run();
```

- `Window::new(width, height)` makes its own `Canvas`; `Window::with_canvas` takes one you already have. Reach it again through `canvas()` / `canvas_mut()`.
- `on_draw` is called with that canvas's context and the frame number. Whatever it leaves on the canvas is what the window shows.
- `on_event` is called once per `UiEvent`, in arrival order, before the frame it preceded is drawn.
- `open()` queues the window; nothing appears until `App::run()`. Windows cannot be created before the event loop exists, which is why the two are separate steps.
- `App::run()` blocks and takes over the calling thread. On macOS that thread must be the main one. It returns when the last window closes or `App::quit()` is called.
- `App::set_fps` sets the target frame rate; `App::close_window` takes the id from `Window::id()`.

The handlers are independent closures, so state shared between them has to be shared explicitly -- an `Rc<Cell<_>>` or `Rc<RefCell<_>>`. A plain local captured by `move` in both gives each its own copy. `examples/window.rs` is a runnable version of the above.

Run it with:

```bash
cargo run --example window --features "window,metal"
```

Swap `metal` for `vulkan` on Linux and Windows.

## Render engine selection

- `CanvasOptions::gpu` asks for the GPU: `true` (the default) uses it when a backend is compiled in _and_ runtime-reachable, and falls back to the raster backend otherwise. `Canvas::set_gpu` changes it after construction.
- `Canvas::engine_kind()` reports what asking actually got -- `EngineKind::Cpu` or `EngineKind::Gpu`. `Canvas::gpu()` reports what was asked for.
- The GPU path requires the `vulkan` (Linux / Windows) or `metal` (macOS) feature; without either, everything renders on the raster backend.
- `BackendInfo::query()` reports what this build on this machine offers before a canvas exists: `renderer`, `api`, `device`, `driver`, `threads`, `gpu_available`, and an `error` naming why the GPU declined, which is what tells a build with no GPU support apart from a driver that refused. The device and driver strings come from the driver and are for logs, not for matching on.
- The two backends are not bit-identical. The GPU composites through 4x MSAA by default, so coverage lands in quarter steps where the raster backend computes it exactly; sub-pixel geometry is where the two differ most. `EncodeOptions::msaa` changes the sample count, and `0` or `1` mean none.

## Paint

- `Paint` carries the full Canvas paint accumulator: `color`, `style` (`Fill` / `Stroke`), `stroke_width`, `stroke_cap`, `dash`, `anti_alias`, `alpha` modulator, `blend_mode`, optional `shader`, optional `image_filter`, optional `color_filter`.
- `Paint::fill(color)` and `Paint::stroke(color, width)` are convenience constructors.
- Each of those is set through a method of its own: `set_color`, `set_alpha`, `set_style`, `set_stroke_width`, `set_stroke_cap`, `set_blend_mode`, `set_anti_alias`, `set_dither`, `set_dash(intervals, phase)` with `clear_dash()` to go back to a solid line, and the four attachment points `set_shader` / `set_image_filter` / `set_color_filter` / `set_mask_filter`, each taking an `Option`.
- `set_dither(true)` breaks up the banding an eight-bit surface shows across a shallow gradient. A float canvas has the precision it compensates for and gains nothing from it.
- `BlendMode` covers Canvas `globalCompositeOperation`, including `Lighter` (additive, Canvas `lighter` / CSS `plus-lighter`, mapped to Skia's `Plus`) alongside the separable `Lighten`, plus the CanvasKit-only `Clear`, `Modulate` and `Destination`.

## Paths

- `Path2D::from_svg(svg_data, FillRule::{NonZero, EvenOdd})` parses SVG path data (the `d=""` form). Invalid input returns `Error::InvalidSvgPath`.
- `PathBuilder` builds one segment by segment: `move_to`, `line_to`, `bezier_curve_to`, `quadratic_curve_to`, `conic_curve_to`, `arc`, `ellipse`, `arc_to`, `rect`, `round_rect`, `round_rect_elliptical`, `add_path`, `close_path`. Same names, arguments and semantics as the `Context2D` methods, minus the current transform, which belongs to a context. `build(fill_rule)` snapshots without ending the build; `PathBuilder::from_path` starts one from an existing `Path2D`.
- A negative width or height reverses the winding of `rect` and `round_rect`, as it does in a browser, so a reversed rectangle inside another punches a hole under `NonZero`. Two negatives cancel.
- `arc_to` and the `round_rect` pair return `Error::InvalidRect` for a negative or non-finite radius.
- `Context2D::clip_path` / `fill_path` / `stroke_path` consume `Path2D`.

A built `Path2D` also answers questions about itself and derives new paths from itself, which is the same set of effects the JavaScript `Path2D` carries:

- **Measure and test.** `bounds()` returns the `Rect` enclosing it, `contains(x, y)` hit-tests under the current fill rule, `is_empty()` says whether anything was added, and `to_svg()` writes the `d=""` string back out. `fill_rule()` / `set_fill_rule(rule)` read and change the rule after the fact.
- **Derive.** `transform(affine)`, `offset(dx, dy)`, `round(radius)` which blunts every corner, `simplify(fill_rule)` which resolves self-intersections into a path with none, and `unwind()` which rewrites it so that a `NonZero` fill covers what an `EvenOdd` fill of the original would.
- **Combine.** `combine(other, op)` with `PathOp::{Difference, Intersect, Union, Xor, ReverseDifference}`. It returns `Option`, since Skia declines pairs it cannot resolve.
- **Excerpt and perturb.** `trim(start, end, invert)` keeps the fraction of the path's length between the two positions (`invert` keeps the complement instead), and `jitter(segment_length, variance, seed)` chops it into segments of that length and displaces each end by up to `variance` -- the hand-drawn look, and reproducible from the seed.
- **Atomize.** `points(step)` walks the path and returns `(x, y)` samples every `step` units along it; `edges()` returns the underlying `Vec<PathSegment>` -- the verbs and their control points -- and `interpolate(other, weight)` blends two paths that share an edge list, returning `None` when they do not.

## Shaders

- `Shader::linear_gradient(start, end, stops, interpolation)` builds a linear gradient. The interpolation argument takes a `GradientColorSpace` -- the eight CSS Color 4 names, `Srgb` (the default, gamma-encoded, what a browser draws) through `Oklch` -- or the pair a `GradientColorSpace::hue(HueMethod::{Shorter, Longer, Increasing, Decreasing})` builds, which selects the direction hue travels in the four cylindrical spaces. `GradientStop { position, color }` carries `RgbaLinear` colours in the canvas's own colour space. Stops must be sorted with positions in `0.0..=1.0`; violations return `Error::InvalidGradient`. A stop that is fully transparent wants `RgbaLinear::fading_out()` rather than `with_opacity(0.0)`, so that it says _which_ colour is disappearing: the second multiplies the hue away and leaves CSS's `transparent`, a transparent black, which the gradient then fades toward. OKLCH interpolation flows through Skia's `OKLCH` color space directly -- no silent fallback to sRGB.
- `Shader::radial_gradient(center, radius, stops, interpolation)` is the concentric case, and `two_point_conical_gradient(start, start_radius, end, end_radius, stops, interpolation)` the general one the Canvas API spells `createRadialGradient` -- two circles that need share neither centre nor radius.
- `Shader::sweep_gradient(center, start_angle, end_angle, stops, interpolation)` is the conic gradient, with both angles in degrees. Naming the end angle is what the JavaScript side reaches through the optional fourth argument to `createConicGradient`: a sweep narrower than a full turn, with the end stops clamped across the rest of the circle.
- `Shader::fractal_noise(base_frequency_x, base_frequency_y, octaves, seed)` and `turbulence(...)` are Perlin noise generators taking the same arguments, matching the SVG `feTurbulence` primitive's two `type` values. Fractal noise is the smoother of the two; turbulence takes the absolute value at each octave, which is what gives it its creased look. A non-finite or negative frequency returns `Error::FilterCreate`.
- All six take the same interpolation argument and attach the same way: `Paint::set_shader(Some(shader))`.

## Filters

`ImageFilter` runs over the pixels a draw produces; `ColorFilter` maps each colour in isolation. Both are attached through `Paint::set_image_filter` / `set_color_filter`, and both return `Result`, since Skia declines some otherwise well-formed argument combinations.

Most `ImageFilter` constructors end with an optional `input` -- the filter whose output this one reads, or `None` for the draw itself -- and an optional `crop: Option<Rect>` that clips the result.

- **Blur and shadow.** `blur(sigma_x, sigma_y, tile_mode, input, crop)`, `drop_shadow(dx, dy, sigma_x, sigma_y, color, input, crop)`, `drop_shadow_only(..)` for the shadow without the source.
- **Geometry.** `offset(dx, dy, input, crop)`, `matrix_transform(transform, sampling, input)`, `crop(rect, tile_mode, input)`, `tile(src, dst, input)`, `displacement_map(x_channel, y_channel, scale, displacement, color, crop)`, `magnifier(lens_bounds, zoom, inset, sampling, input, crop)`.
- **Morphology and convolution.** `dilate(radius_x, radius_y, input, crop)`, `erode(..)`, and `matrix_convolution(kernel_width, kernel_height, kernel, gain, bias, kernel_offset_x, kernel_offset_y, tile_mode, convolve_alpha, input, crop)`. For these three the crop is not the same as composing a separate `crop` filter afterwards: it bounds the domain the kernel reads from as well as clipping the output, so a dilation stops spreading at the edge rather than spreading and then being cut.
- **Compositing.** `blend(mode, background, foreground, crop)`, `arithmetic(k1, k2, k3, k4, enforce_premultiplied, background, foreground, crop)`, `merge(filters, crop)`, `compose(outer, inner)` which chains as `outer(inner(source))`, `from_color_filter(color_filter, input, crop)`, `color_matrix(matrix, input, crop)`, and `empty()`.
- **Lighting.** Six filters matching the SVG lighting primitives, each reading its input's alpha as a height map: `distant_lit_diffuse(direction, light_color, surface_scale, kd, input, crop)`, `point_lit_diffuse(location, ..)`, `spot_lit_diffuse(location, target, falloff_exponent, cutoff_angle, ..)`, and the three `*_lit_specular` counterparts, which take `ks` and a `shininess` exponent in place of `kd`. Positions are `Point3`.

`ColorFilter` covers the colour-domain half:

- `matrix(matrix)` and `hsla_matrix(matrix)` apply a 5x4 matrix in RGBA and in HSLA respectively; `ColorMatrix::{identity, from_rows, scaled, rotated, concat, post_translate, into_rows}` builds one without writing twenty floats by hand.
- `table(table)` maps every channel through one 256-entry lookup, and `table_argb(alpha, red, green, blue)` takes a separate table per channel, `None` leaving that channel alone.
- `blend(color, mode)`, `lighting(multiply, add)`, `lerp(weight, from, to)` which crossfades two filters, `luma()` -- the building block for `destination-in` mask paths -- plus `srgb_to_linear_gamma()`, `linear_to_srgb_gamma()` and `compose(outer, inner)`.

`MaskFilter::blur(style, sigma, respect_ctm)` is the third kind, blurring coverage rather than colour: `BlurStyle::{Normal, Solid, Outer, Inner}` gives a glow, a glow keeping the shape, a halo only, or an inner shadow. `respect_ctm` false keeps the blur screen-fixed under a scaled transform. Attach with `Paint::set_mask_filter`.

## Images

- `Image::from_encoded(bytes)` decodes PNG / JPEG / WebP raster bytes via Skia's image codec.
- `Image::from_pixels(bytes, width, height, stride, pixel_format, color_space)` builds an image directly from a raw pixel buffer -- the way to hand over a decoded video frame or a buffer you generated yourself. **No PNG / JPEG / WebP round trip on the hot path.**
- `Image::from_svg_xml(svg, width, height)` rasterizes an SVG document. `from_encoded` does **not** decode SVG XML.
- `Context2D::draw_image` / `draw_image_rect` / `draw_image_src` paint images.
- `Context2D::set_image_smoothing_enabled(false)` gives nearest-neighbour. With smoothing on, `set_image_smoothing_quality` picks how: `Low` is bilinear, `Medium` adds mipmaps, and `High` is cubic -- Mitchell when the draw enlarges the source, Catmull-Rom otherwise, and bilinear where the scale is not known. A browser makes the same distinction, which is why `High` is only visibly different from `Medium` on an upscale.
- `Image::is_premultiplied()` reports which alpha convention the pixels are in, which is what a buffer handed back out has to be read under.

An animated GIF, WebP or APNG decodes to a still first frame plus the rest on request:

- `frame_count()` is how many frames the encoded data holds -- `1` for a still image -- and `frame_delays()` is one duration in milliseconds per frame, so the slice is always `frame_count()` long. A still image reports a single `0`, which is not a duration: it is shown until something else is drawn.
- `frame(index)` decodes one frame as an `Image` of its own, compositing frames that cover only part of the canvas against the ones before them, so each comes back whole and they may be asked for in any order. An index past the last frame returns `Error::FrameOutOfRange`, and a frame that is present but will not decode returns `Error::DecodeImage`.
- Nothing advances a frame on its own. An animation plays because the caller picks the frame each output frame shows, and `EncodeOptions::frame_delays` takes the same milliseconds back, so an animation can be read in, redrawn and written out with its timing intact.
- APNG is demuxed by this crate rather than by Skia, which opens one as the still image its `IDAT` holds and reports a single frame. GIF and WebP go through `SkCodec`.

## Text

- `FontLibrary::{register_font_from_data, register_font_from_path, has_font, families}` registers TTF / OTF / WOFF / WOFF2 typefaces under family aliases. Internal state is a `parking_lot::Mutex` -- no `RefCell` exposure.
- `TextEngine::new(&font_manager)` wires the registry into a paragraph `FontCollection` (with system-font fallback). `with_system_fonts()` is the no-registry convenience.
- `TextStyle` carries font selection, size, weight, slant, color, alignment, line height, letter / word spacing, decoration (`underline` / `overline` / `line_through` plus style, color, thickness), shadows, and baseline shift. `font_weight: i32` drives `SkFontStyle` weight-bucket matching and (when a `wght` axis is not pinned via `font_variations`) auto-synthesizes a design-space weight on variable typefaces. Construct with `..TextStyle::default()`: the struct is not `#[non_exhaustive]` (no crate-root type is), so listing every field compiles today and breaks the next time one is added.
- **`TextStyle::font_variations: Vec<FontVariation>`** pins variable-font axis positions before layout (CanvasKit's `fontVariations` shape). When non-empty, the engine finds typefaces matching the requested families + style, clones each variable typeface at the requested axes (clamped to the typeface's declared `[min, max]`), and seeds them on a per-call `FontCollection`. Use `FontAxisTag::WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL` for the common axes, or `FontAxisTag::from_str("xxxx")` / `FontAxisTag::new(b"xxxx")` for arbitrary tags. Rich-text variations come from the _base_ style: `SkParagraphBuilder` reads its collection once at construction, so per-span axis changes are not supported.
- `FontLibrary::installed_families()` lists every family a draw can match -- the platform's own plus anything registered here -- and `family_details(name)` reports the weights, widths and styles one offers, or `None` when nothing resolves under that name. The counterparts of the JavaScript `FontLibrary.families` and `FontLibrary.family()`. `families()` stays the narrower question: what this registry was given.
- `Context2D::set_font_stretch` selects a narrower face where the family ships one, and pins the `wdth` axis where it is a variable font -- which is how most variable fonts carry their widths.
- `TextEngine::layout_text(text, style, max_width)` lays out plain text. `layout_rich_text(spans, base_style, max_width)` lays out a sequence of `RichTextSpan` overrides on top of a base style.
- `Paragraph::{width, max_width, height, line_count, first_line_ascent, line_metrics, rects_for_range}` exposes laid-out paragraph metrics. `width()` returns the **measured** longest-line width, not the wrapping budget -- `max_width()` gives back the budget the layout was asked for.
- The rest of the metric surface: `alphabetic_baseline()` and `ideographic_baseline()` are the two first-line baselines measured from the top of the block; `min_intrinsic_width()` / `max_intrinsic_width()` are the widths at which the line breaks stop changing -- the widest unbreakable word, and the width that fits every line without wrapping; `did_exceed_max_lines()` says whether the paragraph style's line cap dropped content, which is how truncated text is told from text that fit; and `unresolved_codepoints()` lists the characters no font in the collection could draw.
- Hit testing: `glyph_position_at_coordinate(x, y)` maps a point to a `TextPosition` in the source text, `rects_for_range(start, end, ..)` returns the boxes a selection covers, and `rects_for_placeholders()` returns one box per inline placeholder, in the order they were added.
- `Context2D::draw_paragraph(layout, x, y)` paints the laid-out paragraph. `(x, y)` is the top-left corner of the block, not a baseline; add `first_line_ascent()` to place it the way `fill_text` places a string.

### Building a paragraph run by run

`layout_text` and `layout_rich_text` cover the common cases in one call. Where the runs are assembled rather than known up front -- and where inline placeholders are needed -- `TextEngine::paragraph_builder(&base_style)` returns a `ParagraphBuilder` that takes them one at a time:

```rust
let mut builder = engine.paragraph_builder(&base_style);
builder.add_text("Reticulating ");
builder.push_style(&emphasis);
builder.add_text("splines");
builder.pop();
builder.add_placeholder(Placeholder::new(24.0, 24.0).on_baseline(PlaceholderBaseline::Alphabetic));
let paragraph = builder.build(320.0);
```

- `push_style(&style)` starts a run in a new style and `pop()` returns to the one beneath it, so styles nest on a stack. Text added with nothing pushed uses the base style.
- `add_placeholder(placeholder)` reserves an inline box -- for an image, an icon, anything laid out beside the text rather than by it. `Placeholder::new(width, height)` defaults to resting on the alphabetic baseline; `aligned(PlaceholderAlignment)`, `on_baseline(PlaceholderBaseline)` and `baseline_offset(offset)` chain onto it. After layout, `rects_for_placeholders()` says where each one landed.
- `build(max_width)` consumes the builder and lays out at that width, returning a `Paragraph` ready to measure and draw.
- Font variations come from the base style. `SkParagraphBuilder` reads its font collection once at construction, so `push_style` cannot change an axis position mid-paragraph.

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
