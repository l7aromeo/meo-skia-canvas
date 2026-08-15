# Changelog

> Two release channels live in this file:
>
> - **crates.io** (Rust crate `meo-skia-canvas`): semver-tracked, versioned independently of npm.
>   Published from `0.2.0` — the `0.1.0` entry below went out under the earlier crate name
>   `skia-canvas`, before this fork existed.
> - **npm** (Node addon `meo-skia-canvas`): continues `phyron-skia-canvas`'s numbering, picking up
>   at `3.6.0`. That in turn forked from `skia-canvas`, which numbers separately and is currently
>   on 3.0.x — so these are not comparable version for version.

## 📦 ⟩ [v5.2.0] (npm) / [v0.7.0] (crate) ⟩ August 15, 2026

Two formats learned to animate, two learned to be read back, and the pixels stopped being flattened
to eight bits on the way out. Underneath all of it is the same correction: this library was writing
files that said less than the canvas held, and reading files as though they said what it would have
said itself — and saying nothing about either.

The crate release is breaking — the text API changed shape and `PixelDepth` grew twenty variants.
The npm one is not, with one deliberate exception noted under Fixed: an unrecognised `colorType`
used to become `RGBA8888` and now throws.

### New

- **A multi-page canvas exports an animated WebP.** Skia can encode one — `SkWebpEncoder::EncodeAnimated`
  is in the header shipped with skia-bindings — and nothing binds it, so the container is written
  here around the frames Skia encodes one at a time. Each frame sends only the rectangle that
  changed, as the format intends: on the animated eye in the gallery that is a fifth of the pixels
  after the first frame.

- **AVIF animates, and codes its frames against each other.** Not stills in a container: eight
  frames of a moving square come to 333 bytes where a single still is 95 — three and a half times
  the size for eight times the content. `avif-serialize` writes the still shape and has no `moov` or
  `trak`, so the ISOBMFF track is written here. Alpha travels as a second monochrome track tied to
  the colour one by a `tref` of type `auxl`, and the loop count is spent on the movie duration
  because ISOBMFF has no field for it — both following libavif, since a file plays the way players
  expect only when it is built the way theirs are.

- **A float canvas is written at the depth it holds.** Every export read the surface back as
  `RGBA8888` before any encoder could choose, so a canvas composited in float was rounded to eight
  bits before anything saw it. APNG and TIFF now carry sixteen bits a channel from one. A ramp from
  `#101010` to `#141414` decodes with five distinct levels from an eight-bit canvas and 258 from a
  float one.

- **AVIF codes at 8, 10 or 12 bits, through `bitDepth`.** AV1 carries all three and this wrote ten,
  always. Unasked, an eight-bit canvas is still written at ten — AV1's transforms work above the
  input depth and the headroom keeps a gradient from banding — and a float canvas at twelve. Naming
  one matters for reach: eight and ten at 4:4:4 are High profile, twelve is Professional. Decoded
  back, the worst channel error on a ramp runs 2.29/255 at eight bits, 0.87 at ten, 0.46 at twelve.

- **An APNG this library writes is one it can read.** Skia decodes no APNG at all — `SkCodec` opens
  one as the still image its `IDAT` holds and reports a single frame — so animations went out and
  came back as their first frame with no timing, while the GIF and WebP beside them round-tripped.
  APNG is demuxed here now, `fcTL` rectangles, disposal and blending included, using the same `png`
  crate the encoder writes it with.

- **An AVIF anybody wrote is one this library can read.** Skia decodes none at all, so this crate
  decoded its own output and nothing else — every AVIF test encoded a canvas and read it back, which
  proved nothing about a file this code did not write. The container is parsed here now: `meta`,
  `iloc`, `iinf`, `iref` and `idat`, covering `iloc` versions 0/1/2, variable field widths, extents
  split across ranges, and offsets into `idat` rather than `mdat`. Grid-tiled images compose, which
  is what a phone produces past a few hundred pixels — Apple's 6016×6016 wallpaper is 38 tiles and
  would not open before. `irot` and `imir` are applied in the order MIAF fixes rather than parsed
  past, so a photograph is no longer decoded on its side in silence; `colr` is honoured for its
  matrix coefficients, its ICC profile, and its full-range flag; `clap` crops before the rotation.
  Every one of those failed the same quiet way — the picture decodes, the size is right, the result
  is wrong, and nothing says so.

- **AVIF is encoded by libaom, not rav1e.** rav1e cannot code losslessly — its source says the
  lossless block is `not yet supported`, and that is a coding tool rather than a dial — and libaom
  was already linked in to decode, so both halves are now one library's reading of the specification.
  The size and quality change on every file, measured on the same bench at 4:4:4 throughout:

      photo   q92   49448 B / 45.49 dB  ->  39271 B / 44.99 dB
      ui+text q92   53393 B / 50.07 dB  ->   7358 B / 53.59 dB
      ui+text q50   24076 B / 35.69 dB  ->   5300 B / 41.93 dB

  A fifth off a photograph for half a decibel is the ordinary trade. Text and flat UI are not a
  trade at all — 86% smaller *and* three and a half decibels better — because libaom has
  screen-content tools, palette mode and intra block copy, that rav1e does not. That is the content
  a canvas library actually produces. `quality` keeps its meaning: the curve produces a fraction of
  the encoder's range rather than a step count, so moving from rav1e's 255 steps to libaom's 63
  moved the scale and left the dial alone.

- **AVIF codes losslessly, through `lossless`.** Off by default, and deliberately: AVIF is reached
  for because it is small, a lossless one is several times the size of a lossy one and often larger
  than the PNG it would replace. The flag alone would not have been honest — quantizing at zero
  preserves what the encoder was given, and converting red, green and blue into a luma and two
  colour differences has already lost before quantisation runs. So this codes the identity matrix,
  ITU-T H.273 matrix 0, where the three planes *are* green, blue and red, and states it in `colr` so
  a reader knows nothing was mixed. Proved by equality rather than tolerance — `assert_eq!(worst,
  0)` on both surfaces, across saturated primaries and a gradient. `quality` is ignored when this is
  set and is deliberately not promoted at `1.0`, which means the finest quantizer rather than no
  quantizer, and changing what it meant would change every file already written.

- **Chroma subsampling is a choice, through `chromaSampling`.** 4:4:4 stays the default, and the
  default is now measured rather than assumed. On flat UI with text at quality 92, 4:2:0 came out
  27.96 dB against 4:4:4's 50.07 — twenty-two decibels — for no size benefit whatever: 53828 bytes
  against 53393, and across three qualities larger twice and smaller once, every one within a
  percent. On that content it is not a trade, it is strictly worse, because the artefacts cost bits
  of their own. On a photograph the usual trade holds and is worth taking: 30% smaller for seven
  decibels. 4:2:2 is dominated on both and is offered because the format offers it.

- **A canvas can be built in every layout the binding names.** `PixelDepth` had three variants
  against the twenty-six `colorType` accepts, so a Rust caller wanting a single-channel readback
  took four bytes a pixel and discarded three. Twenty more, one per Skia colour type.

- **Text gained what the JavaScript surface already had**: `foreground_color` and `background_color`
  on `TextStyle`, `ellipsis`, `em_height_ascent` and `em_height_descent` on `TextMetrics`, the two
  line-end offsets a selection needs at a wrap point, `Paragraph::layout(width)` for re-wrapping
  without rebuilding, and per-line metrics with the single-font runs inside them.

- **`ImageFilter` reaches two more samplers and three crop rects.** `"mipmap"` and `"cubic"` were
  reachable from Rust and not from JavaScript on the same two filters; dilate, erode and matrix
  convolution take a crop that bounds the kernel's read domain as well as clipping the output.
  `createConicGradient` takes an optional fourth argument for the end angle — the Canvas API always
  sweeps a full turn, and Skia can sweep any arc.

### Fixed

- **A canvas of eight bits is no longer written as sixteen.** The depth check named the two 8888
  types and sent everything else to sixteen, which is backwards: seven of the types a canvas can be
  built with hold eight bits a channel or fewer, and all seven wrote sixteen-bit APNGs and TIFFs
  carrying eight bits of information at twice the pixel data. The still PNG of the same canvas wrote
  eight, so one drawing had two answers.

- **An unrecognised `colorType` is refused rather than replaced.** Every unknown name became
  `RGBA8888`, so `new Canvas(w, h, {colorType: "rgba8888"})` — right type, wrong case — built the
  default and reported it back as `"rgba"`. The export path already threw; the constructor shrugged.
  This is the one behaviour change that could affect working JavaScript.

- **A crop rectangle reaches the filter it was given to.** Declared on three `ImageFilter`
  constructors and forwarded by none of them, so the argument type-checked, ran, and was ignored.

- **A PDF keeps the blend modes it was mishandling.** Measured per backend rather than assumed: the
  PDF backend renders conic gradients, shadows and filters pixel-identically to the raster export
  and gets only blend modes wrong, where a `multiply` moved a fifth of the page. Those layers now
  rasterize into the document; everything else stays vector.

- **An animated WebP declares the colour space it was drawn in.** The ICC profile Skia writes for
  the first frame is lifted to the file, so a Display P3 animation is not read as sRGB.

- **A fully saturated colour is no longer coded one level past the depth.** `rgb_to_ycbcr` rounded
  and never clamped, so a primary that puts a chroma difference exactly on the top of the range —
  pure red at ten bits computes 1023.5 for Cr — rounded to 1024, one past what ten bits hold. The
  arithmetic had been wrong since it was written and nothing could see it: rav1e absorbed the value
  silently. libaom aborts inside `av1_count_colors_highbd`, which is how it surfaced.

- **`direction = "inherit"` is honoured.** The third value the Canvas API defines was dropped on the
  floor, so setting `"rtl"` and then `"inherit"` stayed right-to-left. A canvas has no element to
  inherit from, which Chrome resolves to `ltr`.

- **An SVG root carries a `viewBox`**, so the drawing scales with the element rather than being
  pinned to its pixel size.

- **A paragraph shadow's `blurRadius` is a radius, not a sigma.** It was handed to Skia unscaled,
  and Skia's parameter is the sigma — which is half the radius, by the same CSS sentence
  `shadowBlur` has always been halved against. So one library answered a single number two ways:
  `shadowBlur = 8` on a context blurred half as far as `blurRadius: 8` on a paragraph. Measured on
  a 64px glyph, the shadow spread 90px where the context's spread 67px. **This changes existing
  output** — a paragraph shadow now renders at half its previous blur, which is what the option
  always claimed. Double the value to keep what you had. Neither side had ever been measured
  against the other, which is why it survived: either alone looks like a shadow.

### ⚠️ Crate `0.7.0` — breaking

- `Paragraph::rects_for_range` and `rects_for_placeholders` return `Vec<TextBox>` rather than
  `Vec<Rect>`, and the first takes the height and width styles the binding could already choose.
  Skia supplies a direction per box and this dropped it, so a Rust caller could draw a selection
  over bidirectional text and not tell which runs were right-to-left.
- `TextBoxOptions` and `VerticalAlign` are gone. Nothing referenced either outside their own
  definitions, and the doc comment described a `TextEngine` method that was never written.
- `TextMetrics` no longer derives `Copy` — it carries per-line detail now — and gained the em-box
  fields. `LineMetrics` and `TextStyle` gained fields; construct them from `..Default::default()`.
- `PixelDepth` has twenty more variants, which breaks an exhaustive `match`.
- `EncodeOptions` gained `bit_depth`, `chroma` and `lossless`, and `ChromaSampling` is a new public
  enum. Naming `lossless` alongside a `chroma` other than `Full` is refused rather than quietly
  resolved: identity planes at 4:2:0 would be discarding literal red and blue samples, so a caller
  who asked for both wants something the format cannot give.
- AVIF spans pages: a multi-page canvas exported as AVIF is now one animation rather than the
  current page. There is no minimum size — an earlier draft of this release refused an animation
  under 16 pixels a side and reported it as though AV1 required it, which was rav1e's floor rather
  than the format's. libaom codes 2×2.
- rav1e, `v_frame` and `av1-grain` are gone from the dependency tree; libaom arrives through
  `libaom-sys`. Both are BSD-2 with the AOM Patent License 1.0. This also removes `avif-parse`
  (MPL-2.0), which reached the tree through `aom-decode`'s `avif` feature — `just licenses` reports
  `copyleft or unlicensed: none`. The build now needs a C toolchain for libaom on every target.

### Internal

- The JavaScript API reference is generated from `lib/index.d.ts` by TypeDoc, and `just docs` builds
  it beside `cargo doc`. A broken link or a type that reaches a signature unexported fails the
  build; undocumented members are counted against a baseline that may not rise. The thirteen
  hand-written reference pages under `docs/api` are retired in favour of docs.rs and jsdocs.io, both
  of which have been building this project all along. The guides stay.
- Reaching frame `n` of an animation decoded every frame before it, because each is coded against
  the ones before it. Correct, and quadratic for the loop the documentation tells people to write —
  one frame per output frame. Both decoders now keep their state between calls: 150 AVIF frames cost
  1.15 seconds against roughly 87 before, and 60 APNG frames of 320×240 walk in 11 milliseconds
  against near 125. An index that is not the expected one rebuilds, so random access is unchanged.
  The AVIF sample tables travel with the decoder as well, since holding the decoder alone left a
  quadratic *parse* behind the quadratic decode it removed.
- Opening an APNG no longer decodes it. `frame_delays` runs on every `Image` this crate constructs
  and reached the timings by inflating every frame to keep one integer from each — 60 frames of
  960×540 is about 248 MB alive at once to answer with 60 numbers, paid on open rather than on play.
  The timings are in the `fcTL` chunks and a walk reaches them without decoding anything; opening
  that file now measures zero milliseconds and no growth.
- The AVIF sink codes frames as they arrive rather than holding every page's pixels until `finish`,
  which is the invariant `encode/mod.rs` states in its own words. For 120 frames of 960×540 the
  buffer it no longer holds was 475 MB. The alpha track starts at the first frame that is not fully
  opaque and is fed synthesized opaque frames for the ones it missed — they were opaque by
  definition, which is why it had not started — so fully opaque animations pay nothing and
  transparency appearing late is still correct.
- Five ways a malformed file could bring the process down, all reachable from `loadImage` of
  anything, since `Image::from_encoded` asks every image for its delays: `be32` sliced before
  bounds-checking, where 48 bytes of `ftyp` plus an empty `hdlr` panicked; sample-table counts sized
  allocations before comparing them to the file, where `stsz` at `0xFFFFFFFF` asked for 34 GB from a
  hundred-byte input; a track declaring zero samples; an alpha track shorter than its picture track;
  and an APNG chunk claiming more length than the file holds. The last was found by a mutation sweep
  that put 8,335 mutations through APNG, WebP, AVIF, GIF and both foreign AVIF fixtures and produced
  that one message and nothing else — every earlier fix in this list held under it.
- A multi-chunk AVIF sequence is read rather than misread. Samples sit end to end *within* a chunk
  and a track may hold many; this read `stco`'s first offset and laid every sample out from there,
  which is right for what this crate writes and silently wrong for a file chunked otherwise. `stsc`
  is walked now, and `co64` came along for the same price.
- Documented and not fixed: `clli` content light level is parsed past rather than applied, and boxes
  using 64-bit `largesize` are refused, which only matters past 4 GB. `clap` is covered by unit
  tests against the specification's own arithmetic rather than end to end — inserting the box into a
  fixture would shift `mdat` and invalidate every `iloc` offset, and nothing available here writes
  one.

## 📦 ⟩ [v5.1.0] (npm) / [v0.6.0] (crate) ⟩ August 14, 2026

Six image formats Skia has no encoder for, the colour management to make them honest, every frame of
an animated source reachable — and, on the Rust side, the surface JavaScript already had. Skia's
encoder list is three modules — JPEG, PNG and WebP — so GIF, APNG, TIFF, ICO, BMP and AVIF are
written here, from pixels Skia hands back.

The crate release is breaking; the npm one is not. Eight fixes change what already-working code
draws — six on both surfaces, two on the Rust one, each marked below. All were checked against
Chrome or against the specification they implement rather than reasoned about.

### New

- **Animated sources are reachable frame by frame.** `Image` reports `frames` and `delays`, and
  `frame(i)` returns one frame as an `Image` of its own, composited against the frames before it so
  partial frames come back whole. Nothing advances on its own — there is no clock here; an animation
  plays because the caller picks the frame each output frame shows. A negative index counts from the
  end, the rule `page` and `Array.prototype.at` follow.

  ```js
  const spinner = await loadImage("spinner.gif")
  for (let i = 0; i < 24; i++) {
    ctx.drawImage(spinner.frame(i % spinner.frames), 0, 0)
    canvas.newPage()
  }
  ```

- **GIF and APNG, with the pages as frames.** One page is one frame. `fps` defaults to 30;
  `frameDelays` overrides it per frame and takes exactly the array `Image.delays` reports, so
  re-encoding an animation is a round trip. `loop` is `0` for forever, which is how both formats
  spell it.

  ```js
  await canvas.saveAs("out.gif", {fps: 12, loop: 3})
  await canvas.saveAs("out.apng", {frameDelays: source.delays})
  ```

- **TIFF, ICO, BMP and AVIF.** TIFF and ICO gather every page — untimed, which is a different
  question from animating. `avif` takes `quality`. Each is reachable by name, by filename extension,
  and by media type.

- **A file says which colour space it holds.** PNG and APNG write cICP, plus `cHRM`/`gAMA` for
  readers older than it; TIFF, BMP, ICO and AVIF each carry the space in whatever field they have
  for it. A P3 canvas exported to any of them is read back as P3 rather than as sRGB with the wrong
  primaries.

- **An SVG says what the canvas drew.** Skia's SVG backend serialises four paint servers — a solid
  colour, a linear, radial or two-point conical gradient, and an image shader — and one filter, and
  drops everything else without a word. A conic gradient left the element with no `fill` attribute
  at all, which SVG reads as black; shadows, `ctx.filter`, mask filters and every blend mode past
  source-over simply went missing. Those draws are now rendered at the export's `density` and
  embedded as images, cropped to the ink they laid down, while the rest of the document — text
  included — stays vector.

- **`ctx.filter` is available from Rust.** `Context2D::set_filter_css` takes the same CSS string the
  binding takes, through the same grammar.

- `Paragraph.getFirstLineAscent()` — the number to add to a `y` coordinate to place text by its
  baseline rather than its top. The same number as `getLineMetrics()[0].ascent`, and `0` for an
  empty paragraph.

### Rendering

Eight fixes change what already-working code draws.

- **Gradient stops were coming out dark** *(Rust only)*. They were handed to Skia untagged, which
  means "already
  in the destination's working colour space" — gamma-encoded sRGB — while the values passed were
  linear light. Every gradient was affected. None of the six tests covering gradients could see it:
  all six ramp black to white, and those are the two fixed points of the transfer function.
- **`contrast()` pivots at 127.5, not 127.** Filter Effects Level 1 defines it as a linear transfer
  with intercept `0.5 - 0.5 * amount` on channels normalised to 0..1 — a byte pivot of 127.5,
  because 0..255 has 256 levels and its midpoint falls between two. Truncation is kept rather than
  paired with the .5, because that is what browsers do: Chrome returns 127 for `contrast(0)` where a
  rounded 127.5 is 128. `contrast(2)` moved 128 of the 256 ramp entries. Five sampled amounts now
  match Chrome exactly, where four of five did not.
- **`drop-shadow` takes all five spellings its grammar allows.** Filter Effects 1 gives `<color>? &&
  <length>{2,3}`: the colour may sit at either end and the blur radius is optional. The parser read
  three lengths from the front and demanded a colour after them, and a refused step rejects the
  whole chain — so `drop-shadow(red 2px 2px 4px)` was an error here and a picture in Chrome. The
  JavaScript side had the same two gaps and a third: it required a colour at all.
- **A `drop-shadow` colour that will not parse invalidates the whole declaration.** It used to be
  dropped on its own, so the shadow vanished from the render while the getter still named it and
  `ctx.filter` reported a filter nothing was drawing. An invalid declaration leaves the previous one
  standing, which is what `blur(NaN)` already did and what a browser does.
- **`em` in `ctx.filter` resolves against the context font.** It fell back to a hardcoded 16
  regardless, so every `em` in a filter meant the same thing whatever the font said. At `40px`,
  `blur(0.5em)` was 8px and is 20px.
- **`blur(50%)` is refused.** `blur()` is defined over `<length>`, and Chrome refuses a percentage;
  this accepted one for as long as it shared the font-size parser, which takes percentages because a
  font size may be one. An unparseable filter string is ignored whole, as the standard requires.
- **A text decoration with no colour of its own takes the text colour** *(Rust only)*, as the web
  does. It was painting with whatever the last decoration had set.
- **A canvas drawn into another keeps its compositing to itself.** The source arrives as a recorded
  picture rather than a bitmap, so its vectors survive the trip — and so did its erasures: a
  `destination-out` inside it punched a hole through what was already on the destination, where the
  Canvas API says this draws the source's pixels.

### Fixed

- **Timing given to a format with no clock is a `TypeError`.** `fps`, `frameDelays` or `loop` on
  `"png"`, `"tiff"`, `"ico"`, `"pdf"` or the rest used to be dropped, so a caller who asked for
  twelve frames a second and got a single still image was owed the reason.
- **A `frameDelays` of the wrong length is a `RangeError`**, and a non-number entry a `TypeError`,
  at both layers. A sparse array's holes reached the addon as `undefined` and were read as
  zero-length frames, so the animation was retimed to nothing and nothing said so. The length is
  counted against the frames the call will actually write, which is one when `page` names a page.
- **`page` is honoured by every format that spans pages** *(Rust only — the binding always sliced to
  the page first)*. The spanning branch was taken before `page` was read, so
  `to_buffer(Gif, page: Some(0))` on a three-page canvas returned the whole animation and an index
  past the end was ignored rather than refused. `to_file` and `to_buffer` answer the question in one
  place now, having disagreed about it for a release.
- **`density` accepts any positive number.** The whole-number rule came from the `@2x` filename
  convention, which is a way of naming files rather than a constraint on a scale factor — 1.5 is an
  ordinary device pixel ratio, and the Rust API has always taken it. The old message said
  "non-negative" while the check demanded 1 or more, so it named a range containing two values it
  refused.
- **A page out of range names the number the caller typed.** The message speaks in 1-based pages and
  ended with the 0-based index, so asking for page 9 of a two-page canvas was told that "8 is out of
  bounds", a number nobody had entered.
- **A GIF frame no longer leaves the previous one under it**, and an APNG frame delay longer than
  the format's field coarsens the time base instead of wrapping to a short one.
- **BMP records its density**, which it had a field for and ignored, and the resolution every format
  writes now rounds by one rule rather than four.
- `avif` at a quality the format accepts no longer panics.
- **A `resize` event reports fractional pixels**, carried over from the crate-only `0.5.0` release
  and reaching npm here for the first time. `e.width` was rounded to a whole number by winit's
  integer conversion, so at a 1.5 device pixel ratio a 1000px window reported `667` where it now
  reports `666.667` — and agrees with `WindowSpec.width`, which was already unrounded. Only
  observable on a fractional ratio, which means Windows and Linux display scaling.

### ⚠️ Crate `0.6.0` — breaking

Measured against the published `0.5.0`.

- **Two types are renamed to what the web calls them.** `Path` is `Path2D` and `FontManager` is
  `FontLibrary`. `Path` stutters against `std::path::Path` and reads as redundant on its own, but
  `2D` is information rather than repetition the moment a `Path3D` is imaginable, and a rendering
  library is where one is. The seven names that stay different carry a prefix JavaScript needs only
  because it has a single global namespace; `js_names` re-exports them under the web spelling, and
  `doc(alias)` puts the web name in the search index everywhere else.
- `ImageFormat` gains `Gif`, `Apng`, `Tiff`, `Ico`, `Bmp` and `Avif`, so exhaustive matches need six
  more arms.
- `Error` gains `FrameOutOfRange`, `InvalidFilter` and `InvalidExportOption`. The last is distinct
  from `Error::Encode`: nothing was drawn or encoded, the call was refused on the way in. These were
  once quietly substituted — a negative `fps` became 30, a `quality` outside `0..=1` was clamped, a
  mismatched `frame_delays` was ignored — all of which the JavaScript binding had always refused, so
  the same call behaved differently depending on which surface made it.
- `EncodeOptions` gains `color_type`, `fps`, `frame_delays` and `loops`. A struct literal that does
  not end in `..EncodeOptions::default()` will not compile.
- **`EncodeOptions::outline` defaults to `false`.** It defaulted to `true` here while the binding
  defaulted it to `false`, so the same `to_file("card.svg")` produced live `<text>` from Node and a
  wall of `<path>` from Rust — 73 KB against 205 KB on the example the two surfaces share.
- **Four filter constructors take the arguments they were discarding.** `ImageFilter::blur` takes a
  tile mode and a crop rect; `drop_shadow`, `color_matrix` and `from_color_filter` take a crop rect.
  Skia has accepted all of them since before this crate existed, and a blurred layer always sampled
  transparent black past its edge for want of the first.
- **`ColorFilter::matrix` and `hsla_matrix` return a `Result`.** Both handed their twenty numbers
  straight to Skia, which reads a non-finite entry as a NaN matrix and paints nothing for the life
  of the filter — reachable from safe code, from a value a caller could compute without noticing.
- **The declared minimum Rust version is `1.90`**, up from `1.88`. The graph asks for it, not the
  source: `quantette` declares it, and it is the palette quantizer the GIF encoder needs. The
  alternative — `color_quant`, which builds on 1.56 — is NeuQuant, a 1994 neural quantizer whose
  palettes are visibly worse on gradients and flat colour alike. The format would have shipped
  either way; the pictures would not have.

### Crate `0.6.0` — new

The Rust surface picks up what the JavaScript one had and it did not. Four of the Node examples were
ported to Rust to find out what was missing, which is how five of the fixes above were found: an
operation the crate cannot express is one the port cannot compile.

- `Path2D` gains the thirteen operations only the JavaScript `Path2D` had: `to_svg`, `contains`,
  `combine`, `interpolate`, `simplify`, `unwind`, `offset`, `transform`, `round`, `trim`, `jitter`,
  `edges` and `points`. The binding calls them rather than reimplementing them alongside.
- The twenty `ImageFilter` constructors and seven `ColorFilter` constructors that existed only as
  JavaScript factory names, plus `ColorMatrix`, which builds a colour matrix by axis so a hue
  rotation says which axis it turns.
- `ParagraphBuilder` — the style stack, and `Placeholder` with the alignment and baseline modes the
  layout engine understands. Text layout was reachable only as a whole paragraph in one call.
- `TextMetrics` gains `alphabetic_baseline`, `ideographic_baseline`, `min_intrinsic_width`,
  `max_intrinsic_width` and `glyph_position_at_coordinate`.
- The CSS strings the binding parses: `set_filter_css`, `set_text_decoration_css`, and
  `set_letter_spacing_css` / `set_word_spacing_css`, which take `em` as well as pixels. Every CSS
  getter now echoes the string the caller passed rather than a normalised rewrite of it, which is
  what the web does.
- `Context2D::transform_projection`, the multiplying form beside the replacing `create_projection`.
- `Canvas::to_data_url`, `BackendInfo::query`, `FontLibrary::reset`, `CanvasOptions::text_contrast`
  and `text_gamma`, and `DEFAULT_WIDTH` / `DEFAULT_HEIGHT` as named constants rather than two
  literals.
- `Image::frame_count`, `frame_delays` and `frame`, behind the same rules the JavaScript members
  follow.
- A `Window` can be asked what it was told: title, background, fit, resizable, fullscreen, size,
  position, cursor, borderless, visible, page, text contrast and gamma all read back, and `App`
  reports its windows, whether one is open, and whether the loop is running or idle. `Cursor` is a
  type rather than a string, carrying the CSS name the window system wants. `Window::close` was
  added last release and could not be called.

### Internal

- **One table describes every format, and the binding asks it.** `lib/classes/canvas.js` kept its
  own copy of the extension and media-type maps, the list of names its error message offered, and —
  the one that would have gone wrong silently — a bare `format == "pdf"` deciding which exports
  gather every page. Adding a multi-page raster format with that line in place would have quietly
  encoded the last page alone and reported nothing wrong. The compiler cannot reach across the
  boundary to catch that, so the boundary asks instead of remembering.
- **Frames are pushed into a sink** rather than every byte returned, so a long animation is written
  as it is encoded instead of held whole.
- **TIFF is deflated, with a horizontal predictor.** The crate's encoder defaults to writing the
  pixels out whole and nothing had overridden it, so a TIFF was the size of the raw buffer: 4.2 MB
  for a 1200x900 page, tying the format with BMP. Deflate is in TIFF 6.0's own tag list and is
  lossless, so the picture is unchanged; the page is 1.6 MB now.
- **AVIF encodes on eight tiles rather than one.** A tile is what rav1e parallelises over, and a
  still picture is one tile by default, so the encode ran on a single core whatever the machine had
  — 5.6 seconds for a page here, and 1.1 now. Tiles cost a little compression, because the entropy
  coder restarts at each boundary: the file grew by 0.8%.
- **An SVG embeds a run of unsupported draws as one image.** Each embedded image costs a page-sized
  surface, a playback and a scan for its bounds, and they arrive in runs — sixty shadowed panels in
  a row are sixty of them, which took 1.1 seconds where the same page without shadows took 8
  milliseconds. As runs it is 56 ms, and the file drops from 535 KB to 141 KB.
- **One sRGB transfer function for the whole crate.** It was written out three times — in the pixel
  readback, in the CSS colour formatter, and again in the encoder — each with its own literals for
  the five constants IEC 61966-2-1 names. Consolidating it is what exposed the gradient bug above.
- **Forty-nine unnamed Rec. 709 luma coefficients**, in two spellings, are now constants — which is
  how the contrast pivot above was found. `grayscale()`, `saturate()` and `sepia()` are the same
  shape and were written out three times as sixty literals, forty-two of which were `1 - x` for an
  `x` on the same line; one `faded_toward_identity` builds all three. The graphics-state defaults
  got the same treatment: nine literals, two of them the number ten, meaning a miter ratio in one
  place and a font size in pixels in the other.
- **Two dependencies nothing imported are gone** — `crossbeam` and `once_cell` — along with a `just`
  recipe that finds the next ones. New: `gif`, `png`, `quantette`, `tiff`, `rav1e` and
  `avif-serialize`. One focused crate per format rather than the `image` umbrella, which is a facade
  over these same crates and would pull in decoders for a dozen formats this crate already decodes
  through Skia. BMP and ICO are headers wrapped around pixels this crate already has and are written
  by hand — `ico` would otherwise pull a second copy of `png` into the tree beside the one APNG
  uses.
- **The licence audit runs in `ci`.** It had been a recipe nobody ran, and the count in
  `THIRD-PARTY-NOTICES.md` claimed 135 packages long after the graph had moved. It asks `cargo tree`
  which crates actually link into a release binary and exits non-zero on a copyleft or unlicensed
  one. Two libraries Skia bundles were unnamed and now are: Expat, which parses the XML behind SVG,
  and Wuffs, which decodes GIF and PNG.
- **The examples are `examples/rust` beside `examples/node`**, four of them line-for-line
  translations of each other, so neither surface reads as the afterthought.
- **Every illustration in the documentation is generated.** The seventeen on the API pages were
  inherited with no way to reproduce them, so nothing could tell whether they still matched the
  library; two scripts under `docs/generate` draw them from the current code and two consecutive
  runs produce identical bytes. Redrawing them found six that were wrong — among them a
  `measureTextBaselines` plate giving 34px type 22px rows, and a `createProjection` plate with four
  panels and three pictures.
- **The hero banner is ours.** It was Skia Canvas's own wordmark and mark, inherited with the fork
  and describing a library for Node.js alone. The replacement is drawn by the library, from
  `docs/generate/brand.js`.
- **The test fixtures are one directory of fonts and one of images**, 3.4 MB lighter: twenty-eight
  static font weights nothing read, superseded by the variable fonts beside them, and a decoded copy
  of a PNG no test opened.

## 📦 ⟩ [v0.5.0] (crate) ⟩ August 13, 2026

Windowing from Rust, and the type surgery that made it possible. A crate-only release: the Node
addon's API is unchanged, and the one behavioural difference on that side is noted at the end.

### A window can be opened from Rust

`gui` was public and unreachable. `Window::new` took an `ActiveEventLoop` and a `Page` — one that
only exists inside a running event loop, the other behind a crate-private module — so the module
documented something no Rust caller could construct. Opening a window was one of the two things the
README listed as JavaScript-only. It is now one thing.

```rust
let mut win = Window::new(480.0, 320.0);
win.set_title("hello");
win.on_event(|event| { /* … */ });
win.on_draw(|ctx, frame| { /* … */ });
win.open();
App::run();
```

`Window` is what a window is made from — a spec, a canvas, and the handlers — because winit creates
windows inside its loop and not before. `open` queues it, `App::run` starts the loop and blocks,
and the window appears on the first pass through the same path the Node binding has always used.
Each frame drains the events, hands them to `on_event`, adopts the spec the window system reports
back, then calls `on_draw` and shows what it left. `examples/window.rs` is a runnable version.

### No `skia_safe`, `neon` or `winit` type in a public signature

The crate has promised the first two since `v0.2.0`, and `gui` was exempt from the check that
enforced it. Closing the exemption meant replacing what leaked:

- `Window::fitting_matrix` returns `Affine` rather than Skia's `Matrix`.
- `Window::set_background` takes a CSS string, the form `WindowSpec.background` already held, and
  returns whether it parsed.
- `UiEvent` carries `Point`, `Size` and a new `Key` enum in place of winit's `LogicalPosition`,
  `LogicalSize` and `KeyCode`. `Key` mirrors all 194 of winit's variants and serializes identically,
  plus `Unidentified` for a key a future winit adds — the DOM's answer for a key it cannot name,
  and preferable to panicking in a keystroke handler.
- `ModifierKeys` gained `shift`/`ctrl`/`alt`/`meta`; its fields were private with only a
  `Serialize` derive, so a Rust caller handed one could not read it.
- `Sieve`, `WindowManager`, `Window::surface_props` and the winit-backed window — now `OpenWindow`
  — became crate-private. They are plumbing addressed by winit's own types, and none was reachable
  in a useful way regardless.

`EXEMPT_MODULES` in `scripts/check-public-api.mjs` is empty, so the claim is checked rather than
maintained by hand.

### The check that enforces it had a blind spot

It never descended into enum variant payloads or tuple-struct bodies, leaving 62 public items
unwalked — including every field of `UiEvent`'s variants and the public `FontAxisTag`. A
`skia_safe::Rect` planted on `UiEvent::Mouse.point` reported the tree clean and exited 0. It is
caught now. No real leak was hiding there; the enforcement was weaker than the claim it backed.

### Behaviour

- **`resize` events are no longer rounded to whole pixels.** `UiEvent::Resize` carried
  `LogicalSize<u32>`, and winit rounds when converting to an integer type; `Size` is `f32` and does
  not. At a 1.5 device pixel ratio a 1000px window reported `667` and now reports `666.667`. This
  reaches the Node addon too, as `e.width` on a `resize` event, and is the one JavaScript-visible
  change in this release. Kept because `WindowSpec.width` was already unrounded through the same
  conversion — the event now agrees with the spec instead of disagreeing with it by up to half a
  pixel. Only observable on a fractional ratio, which is Windows and Linux display scaling.
- `Window::set_background` records the string it was given even when the colour it parses to is
  unchanged, so `"red"` and `"#ff0000"` no longer disagree with the spec.

## 📦 ⟩ [v5.0.0] (npm) / [v0.4.0] (crate) ⟩ August 12, 2026

The release this fork exists for. `v4.1.1` audited the rendering against upstream; this one audits
everything else — the declarations, the documentation, the Rust crate, the GPU backends, and the
calls that typechecked and then did nothing.

The recurring shape is a promise the package made and did not keep: a `colorSpace` you could pass but
not read back, a `new` that returned an object with nothing behind it, a `colorType` that named a
precision it never composited at, an argument accepted and discarded. Most were inherited rather than
introduced here, and each was checked against samizdatco/skia-canvas at `12e1c6e` — or against a
browser, or against Skia's own source — before being called a bug.

Both surfaces are much closer to being the same library seen twice: the drawing API, the colour
vocabulary and the font queries are shared, through one implementation rather than two. Two things
are still JavaScript-only — opening a window, and writing a gradient stop as a CSS string.

### ⚠️ Breaking

- **Invalid arguments throw instead of substituting a value.** Each is typed as a string union in
  `lib/index.d.ts`, so the substitution made the declaration a lie, and WebIDL throws for an
  unrecognised enum in a method argument.

  | call | before | now |
  |---|---|---|
  | `MaskFilter.MakeBlur("bogus", 4)` | a normal blur | `TypeError` |
  | `ImageFilter.MakeBlur(4, 4, "bogus")` | tile mode `decal` | `TypeError` |
  | `ColorFilter.MakeBlend("red", "bogus")` | source-over | `TypeError` |
  | `ColorFilter.MakeBlend("notacolour", …)` | blended with black | `TypeError` |
  | `ColorFilter.MakeLighting("white", "bogus")` | fell back to white | `TypeError` |
  | `ImageFilter.MakeDropShadow(…, "bogus")` | a black shadow | `TypeError` |
  | `canvas.newPage(500)` | a page at the old size | `TypeError` |
  | `new Paragraph()` / `new TextMetrics()` | an object that failed later | `TypeError` |
  | an unrecognised `colorSpace` | silently sRGB | `TypeError` |

  Omitted arguments still take their defaults, and `null`/`undefined` still mean "use the default".
  `globalCompositeOperation` is deliberately unchanged: the Canvas standard requires it to *ignore* an
  unrecognised name, so that parser stays separate from the one the filter factories use.

- **A float `colorType` now composites in float**, where it used to select only the readback format.
  Sixty fills at 0.6% alpha land on `0.30308`, which is the arithmetic answer, where eight bits
  round every layer to a whole level and compound the error — `0.23922` on the CPU, `0.36078` on the
  GPU, which misses the other way.
  Costs about 1.4× the time and twice the memory for `RGBAF16`, 1.5× and four times for `RGBAF32`.
  Such a canvas renders on the raster backend — no GPU Skia ships composites in float accurately —
  and `canvas.engine` reports which engine took it.

- **Some fixes change what already-working code draws.** A gradient interpolates in sRGB, so the
  default black-to-white ramp reads `128` at its midpoint where it read `188`; a wide-gamut canvas
  composites in its own space instead of sRGB; and a GPU canvas that cannot offer 4× samples takes
  the count nearest four rather than the largest the device has. Each of these matches a browser, and
  each will move a golden-image test.

- **Crate only**, measured against the published `0.3.1`.
  - The parallel `Surface` / `Recorder` / `DrawTarget` / `Backend` layer is gone, with everything
    reachable only through it: `SurfaceOptions`, `RawFrame`, `RawFrameOptions`, `LinearColorSpace`,
    `OutputColorSpace`, `AlphaMode`, `Paint`, `PaintStyle`, `DashPattern`, `SamplingMode`,
    `RenderEngine`, `EngineStatus` and two `Error` variants. It was built for an external consumer
    that never materialised; nothing in the crate reached it, and `Canvas` + `Context2D` cover what
    it did. `EngineKind` survives as `canvas::EngineKind`. 5,061 lines out.
  - `PixelColorSpace` gains `Rec2020Pq` and `Rec2020Hlg`, so exhaustive matches need two more arms.
  - The additive blend mode is `BlendMode::Lighter`, as Canvas spells it, rather than `PlusLighter`.
  - `Shader::linear_gradient` and friends take a `GradientInterpolation`, which carries the hue
    direction alongside the space.
  - Nine `gui` items are `pub(crate)`: they take `FunctionContext` and `Deferred`, so they were the
    Node binding rather than public surface. `Window::suface_props` was also missing an `r`.
  - The declared minimum Rust version is `1.88`. `0.3.1` said `1.85` and could not build on it —
    the crate uses let-chains in 33 places and those stabilized in 1.88, while edition 2024 makes
    `1.85` look right and no dependency contradicts it. Nothing changed about what compiles; the
    claim is now true, and CI checks it against whatever `Cargo.toml` declares.
  - Nothing else here is a break. `Canvas`, `Context2D`, `PathBuilder`, `EncodeOptions` and
    `TextMetrics` are all new below, so the renames and error changes they went through on the way
    to this release change no code that could have been written against `0.3.1`.

### New

- **The Rust crate is a consumer API, not a byproduct.** `Canvas` and `Context2D` mirror the
  JavaScript surface — same method names, same argument order, same state model — and the Node
  binding stays behind an internal module.
  - Every public type is reachable straight off the crate root, so `use meo_skia_canvas::{Canvas,
    PathBuilder, FillRule}` works: the sixteen modules group them by subject for reading, but one
    draw reaches across several and nothing should require knowing which.
  - `PathBuilder` builds a `Path` segment by segment, with the `Context2D` names and semantics minus
    the current transform.
  - The six graphics-state readers and the three filter-slot readers that had setters and no getters.
  - `Canvas::with_options` takes the colour space, pixel format and GPU flag the JavaScript
    constructor takes.
  - `set_fill_style_css`, `set_stroke_style_css` and `set_shadow_color_css` accept CSS colour strings
    through the same parser the binding uses, so both surfaces land on the same pixel — nine
    notations checked across two colour spaces, byte for byte.
  - `FontManager::installed_families` and `family_details` mirror `FontLibrary.families` and
    `FontLibrary.family()`.

- **Colour management, end to end.**
  - The CSS Color 4 functions parse: `lab()`, `lch()`, `oklab()`, `oklch()`, `hwb()` and
    `color(<space> r g b / a)`.
  - A colour keeps the space it was named in, so `color(display-p3 1 0 0)` on a P3 canvas is that
    canvas's own red rather than a value that went through sRGB and lost a level.
  - `getImageData` reads back in any supported space, where it used to reject everything but sRGB
    above a stale TODO. The same sRGB red, read four ways: `srgb` 255,0,0 · `display-p3` 234,51,35 ·
    `rec2020` 210,84,46 · `rec2020-pq` 136,83,56.
  - A readback with no space or format of its own inherits the canvas's, as a browser does, and
    reports what it used.
  - `canvas.colorSpace` is readable and normalises aliases — `p3` reads back as `display-p3`,
    `hdr10` as `rec2020-pq`. The name is stored rather than derived, because Skia keeps no record of
    which CICP pair built a space.
  - Fifteen names across eight spaces, including HDR10 (PQ) and HLG, on both surfaces.

- **`new` works on the classes that only had factories.** It used to produce an instance with no
  native state, and because every consumer gates on `instanceof`, the forgery passed validation:
  `ctx.fillStyle = new Shader()` was accepted, read back as `"#000000"`, and filled black without
  raising.

  ```js
  ctx.maskFilter  = new MaskFilter("outer", 6)
  ctx.fillStyle   = new Shader("turbulence", 0.08, 0.08, 4, 0)
  ctx.colorFilter = new ColorFilter("blend", "red", "multiply")
  ctx.imageFilter = new ImageFilter("drop-shadow", 2, 2, 3, 3, "black")
  const para = new ParagraphBuilder({ textStyle: { fontSize: 16 } }).addText("hi").build()
  ```

  Where a class builds more than one kind of thing the first argument names the kind, following
  `CanvasGradient`'s `"linear" | "radial" | "conic"`. The 37 names derive from the `Make` methods they
  mirror, and a test derives them mechanically so the two cannot drift. `Paragraph` and `TextMetrics`
  stay non-constructible: they are outputs of an operation, and the browser has no `TextMetrics`
  constructor either.

- **Standard members that were never implemented**: `isContextLost()`, `fontVariantCaps`,
  `naturalWidth`/`naturalHeight`, `toBlob`. Also newly reachable: `Canvas.contexts`,
  `Canvas.toSharpSync()`, and `PlaceholderAlignment`/`TextBaseline` as exported constants.

- **A gradient chooses which way its hue travels** — `shorter`, `longer`, `increasing`, `decreasing`
  — for the four cylindrical interpolation spaces.

- **Timing and memory are measured rather than asserted.** `just bench` builds the release binary
  first and reports GPU against CPU, what each pixel format costs, encode times per format, and
  memory per canvas. It replaced a claim that did not survive being run: the cost of a float canvas
  is not one multiplier but a range from **0.74× to 7.58×** depending on the workload — blending
  translucent layers is *faster* in float, because an eight-bit surface converts through its
  transfer function on every layer, while `RGBAF32` opaque fills cost 7.6× for 4× the bytes.

### Fixed

- **Colour and compositing**
  - A canvas composited in sRGB whatever its `colorSpace` said, converting only at the end, so a
    wide-gamut canvas clipped colours it should have held. It composites in its own space now and
    converts on the way out, as a browser does.
  - A raw export ignored the space it was given: `toBuffer("raw")` always handed back sRGB.
  - Gradients interpolated in linear light, so the default black-to-white ramp read `188` at its
    midpoint where CSS, Canvas and every browser give `128`.
  - Gradient alpha interpolated premultiplied, holding the hue as it faded instead of travelling
    toward the next stop.
  - `from_hex` accepted any number of leading hashes.
  - A readback error reported a value other than the one that was wrong.

- **GPU**
  - **A crash.** Every thread dlopened the Vulkan loader and the last `Arc` to drop closed it, so the
    idle watcher could unload it under a thread still opening it — a null function pointer inside
    `vkEnumerateInstanceExtensionProperties`. Segfaulted in about half of thirteen runs before, none
    of nineteen after.
  - **A hang.** Every thread also built its own `VkInstance` and `VkDevice`, so a `vkDestroyDevice`
    at thread exit could land while other threads were mid-submit; NVIDIA's driver takes
    process-global locks, and the two deadlocked. Under `gdb`: one thread in a thread-local
    destructor, four in `GrVkGpu::onReadPixels`, all parked on `libnvidia-glsi` mutexes. One device
    is now shared for the life of the process, with a queue per thread. Wedged on the first and third
    attempt before; ten clean runs after, twice over.
  - `msaa: 1` rendered on Linux and threw on macOS, because the Metal backend's candidate list
    omitted the count that means "no multisampling".
  - With 4× unavailable, the sample-count fallback took the *largest* the device offered — up to 32×
    — rather than the nearest to what was asked for.
  - `new Canvas(w, h, {gpu: false})` reported `.gpu === true` while rasterizing on the CPU, and
    `canvas.engine` reported the machine rather than the canvas: every canvas on a machine with a GPU
    claimed GPU, including ones the CPU was drawing.

- **Text and fonts**
  - A font registered through `FontManager` was invisible to `ctx.set_font`: registration reached
    paragraph layout only, so `Font::new("MyFamily", …)` measured a fallback face.
  - `font-stretch` selected among faces and ignored a variable font's `wdth` axis — which is how most
    variable fonts carry their widths, and how Ubuntu ships. Amstelvar now measures 262.8 / 204.0 /
    145.1 at normal / condensed / ultra-condensed, where all three were 262.8.
  - A `TextEngine` built with `with_system_fonts` had no default family, so anything asking the
    collection for a face outright got nothing: a strut laid out at negative infinity.
  - Every `textDecoration` without an explicit colour was discarded.
  - `ctx.fontVariant` could not be set back to `"normal"`.
  - `ctx.drawParagraph` ignored `globalAlpha` and `globalCompositeOperation`; alpha `0.5` changed
    nothing and every blend behaved as source-over.
  - `ParagraphBuilder.addPlaceholder` read `align` and `baseline` and dropped them.
  - The font getter returned a string missing part of what was set; a size split from its unit was
    accepted; `TextAlign` defaulted to `Left` where a context defaults to `Start`; a zero text bound
    reported negative zero.

- **Textures**
  - A texture grid was stamped over an unbounded lattice: `spacing: 0.001` took 29 GB and called
    `SK_ABORT`, which no `catch_unwind` could see.
  - A sub-pixel line mark lost its coverage rather than keeping it, and a tile mark narrower than
    half a device pixel vanished on the GPU while the raster backend antialiased it.
  - A line grid reported and laid out at a period other than the one it drew.

- **Paths, geometry and state**
  - `new Path2D().roundRect(x, y, w, h)` — the four-argument form the standard defines — produced an
    empty path, where `ctx.roundRect` with the same arguments drew. `Path2D` carried no default for
    the radius, and an absent one is falsy.
  - A `Path2D` rect with one negative dimension kept its winding, so a reversed rectangle inside
    another failed to punch a hole under `nonzero`.
  - A negative arc radius drew instead of throwing, alone among its siblings.
  - A transform that cannot be solved poisoned the CTM for every later draw.
  - Setters accepted values the standard says to ignore, and an odd-length dash pattern was not
    repeated.
  - The composite operation was computed and never written onto the paint.
  - A layer was dropped when an opaque fill covered the page, losing what it composited onto.
  - `globalAlpha` was stored as a float where its IDL declares a double.
  - `DOMRect(10, 10, -6, -4)` reported `left=10 right=4`, inside-out.

- **Readbacks and limits**
  - Every readback allocation is guarded, not one of three: `toBufferSync("raw", {colorType:
    "RGBAF32"})` on a 12,000-square page aborted at 2.3 GB.
  - A readback rect spanning the coordinate range panicked inside Skia's rounding.
  - A pixel buffer larger than Skia can address is refused rather than aborting.

- **Filters**
  - **A filtered fill could erase the page.** `ctx.imageFilter = new ImageFilter("empty")` followed
    by a `fillRect` over the whole canvas left transparent black, while the same fill one pixel
    smaller left the page untouched. The fast path that discards the recording for an opaque covering
    fill never asked whether a filter would deliver that fill; all four filter slots now bar it.
  - CanvasKit's camelCase blend names had never resolved — `colorDodge` composited source-over and
    reported nothing — because three blend-mode parsers existed and none agreed. They share one now.
  - A drop shadow reported its colour differently from the binding.
  - A filter angle lost its sign, its unit was read case-sensitively, and the pattern was not
    anchored to the whole value.

- **The JavaScript surface did what the types promised.** `setTransform()` with no arguments threw
  instead of resetting; `createImageData(imagedata)` threw; `DOMMatrix.invertSelf()` left the
  receiver unmodified; `inverse()` on a singular matrix returned `undefined` rather than an all-NaN
  matrix; `transformPoint({x, y})` returned all-NaN; and `saveAs`, `saveAsSync` and `toDataURLSync`
  dropped the value they forward, so `await canvas.saveAs(…)` resolved before the write finished.
  `PlaceholderAlignment` and `TextBaseline` reached CommonJS but not ESM.

- **A window's `setup` event could never fire.** The frame counter was seeded at zero and
  pre-incremented, so the first frame was numbered 1 and the `frame == 0` branch beside it was
  unreachable — an event the declarations export and the Window page documents, emitted nowhere in
  the package. Frames now start at 0, which is both what the docs say and the frame `setup`
  precedes.

- **Every exported PDF carried `Producer: Skia Canvas`**, hardcoded in two places, misattributing
  output to a different project in a header every reader displays. It derives from `CARGO_PKG_NAME`
  now; `Creator` stays unset, since only the caller knows the application.

### Types and documentation

- `lib/index.d.ts` is what the package advertises as its `types`, and nothing had ever checked it —
  `just typecheck` was `cargo check`, Rust only, despite the name. It typechecks under `strict` now,
  in CI too. That found its own blocker first: the file hard-imported `sharp`, an optional peer in no
  dependency list a developer installs, so the declarations could not resolve in an editor.
- **Declared against nothing, now removed:** `DOMPointReadOnly`, `DOMRectReadOnly`, `DOMRectList` and
  `FontOptions` had constructors at the type level and no runtime counterpart.
- **Described wrongly, now corrected:** `CanvasTexture` was an empty class despite being produced by
  `createTexture()`; `Image.complete` was declared settable; `TextStyleInput.decoration` was a bare
  number where the values are flags; `MakeEmpty` was called a no-op; `MakeMatrixTransform`'s six- and
  nine-element forms are read in different orders; `textAlign` and `textDirection` were bare strings.
- **Four tests enforce what was previously found by hand**: that the declarations and the runtime
  export the same names, that both entry points do, that every non-standard member carries a 🧪
  marker, and that no marker sits on real Canvas API. The marking test had a blind spot of its own,
  hiding 25 extensions, 19 of them on `Path2D`.
- **Six classes had no documentation page at all** — `ColorFilter`, `ImageFilter`, `MaskFilter`,
  `Shader`, `Paragraph`, `ParagraphBuilder` — which mattered more once `new` began working on five of
  them. Every runnable example was executed against the binary rather than written from memory.
- **323 public Rust items rendered blank on docs.rs**, and `gui` was absent entirely, feature-gated
  behind `window` while docs.rs metadata set `no-default-features`. Both fixed, and a CI job renders
  what docs.rs renders.
- **Five doc comments described something other than what they sat on.** A doc comment belongs to the
  item after it, so deleting an item hands its summary to whatever follows: `shader` was published as
  "Deferred recording of drawing commands", `text` as "Drawable raster targets", and
  `with_composited_canvas` carried `save_layer`'s entire summary — returned to `save_layer`, which
  had none. Found by reading the rendered page rather than the source, where they look like ordinary
  prose.
- **The Rust page describes the crate that exists**: the deleted layer is gone from it, and it now
  records what fails and how — which operations return `Result`, why the three remaining panics sit
  on invariants no caller can reach, and that degenerate input is ignored the way a browser ignores
  it.
- **`examples/` had a Rust example and nothing for Node.** Two runnable scripts now live in
  `examples/node`, and the README embeds their real output; `just examples` redraws it.
- **The docs told you to install the wrong package** — every `npm install`, every `import`, and the
  bundler config for Next.js and Webpack named `skia-canvas`, in 29 places.
- **The README leads with the library rather than its provenance**, and three of its own claims did
  not survive checking: public signatures were said to expose no `skia_safe` or `neon` type *with CI
  verifying it*, when no such check existed and four `gui` methods do; the eight-bit compositing
  figure was quoted as `0.239` without saying that is the CPU, where the GPU misses the other way at
  `0.361`; and the Rust example did not compile. It is now built from outside the repo as a consumer
  would.
- **The benchmarks are this library's own.** Both `docs/node.md` and `docs/index.md` carried
  upstream's 2025 tables with no sign they were someone else's measurements, of a different library,
  on a machine nobody here has seen — and one row was worse than stale: the harness imports
  `skia-canvas` at module scope, so its startup test times a module-cache hit, 0.35 ms against
  15.35 ms for a real first import. Re-measured here against current versions of `canvas`,
  `@napi-rs/canvas`, `canvaskit-wasm` and upstream `skia-canvas`, with this fork added as its own
  entry so upstream stays in the comparison.
- **The example images are reproducible.** Neither script pinned an engine, so the committed images
  were whatever renderer the last machine to regenerate them offered; both draw on the CPU now.
  Three panels were also misrepresenting what they showed — `imageSmoothingQuality` demonstrated
  itself with `drawCanvas`, which replays a recording rather than resampling pixels, so its `low`
  and `high` cells were byte-identical; a crop panel stretched a square source into 244×60; and a
  `repeat-x` fill drew nothing at all, because a pattern is anchored to the coordinate origin and
  the rect never reached its one tile-high band.
- **Nineteen documented claims did not survive being run.** Four audits went through the pages no
  test covers, and each finding was reproduced before it was corrected: `getImageData`'s `colorSpace`
  was described as unused when it changes the pixels returned, the `"rgba"` alias was annotated with
  its channels reversed, five `setTransform` forms were presented as equivalent while three of them
  specified a different `f`, SVG scaling was called `object-fit: contain` where it behaves like
  `cover`, `new Image(ArrayBuffer)` was shown for a constructor that takes a `Buffer`, macOS was
  promised "arm64 or x64" where only arm64 is published, Node was said to reach back to v12.22 where
  `engines` requires 22, and a Lambda deployment script expanded its architecture from a variable it
  never set. Three further findings turned out to be code rather than prose, and are under Fixed.

### Internal

None of this changes the published package.

- **The gates now run what they claim.** `just ci` never ran `cargo test` at all — the Rust suite,
  the larger of the two, was checked only when someone invoked cargo by hand. `just test` also reused
  a stale `lib/skia.node` rather than building, which kept the JavaScript suite green for a day after
  the `node-addon` feature stopped compiling.
- **The suite runs off macOS and off one GPU vendor.** Nine tests named fonts only macOS ships and
  measured whatever fontconfig substituted; three sampled a gradient at exactly the midpoint of a
  101-pixel ramp, where the value is 127.5 and the backend decides; two compared against constants
  measured on Metal. Verified on Linux/Vulkan, and in a container with eight fonts.
- Test coverage grew where it had been shallow: clipping, transforms, degenerate rects, pixel
  layouts, page sizes, translucent compositing, and the parameters and errors nothing ever passed.
- **The Metal backend moved to objc2**, dropping `objc` 0.2.7 and `block` 0.1.6 — last released in
  2019 and 2016 — along with `cocoa`, `dispatch` and `foreign-types`. `metal` on its own also
  compiles for the first time.
- `release-npm` restores `package.json` on any abort, dispatches the binary build, sets the release
  notes from this file, and resolves the platform lockfile without fetching tarballs; `release-crate`
  could not complete in either direction, and can now.
- **Two claims this release makes are now enforced rather than stated.** One job checks the crate
  against the Rust version `Cargo.toml` declares, so bumping `rust-version` moves the check with it
  and reaching for a newer feature without bumping fails. Another fails when a public signature
  exposes a `skia_safe` or `neon` type — which took three attempts, each caught by injecting a
  deliberate leak instead of trusting the output: grepping rustdoc's HTML reports zero on a tree
  that leaks, because a foreign type renders as a bare name; filtering on rustdoc's `paths` map
  flags seven items under the `pub(crate)` `context` module; and walking the JSON without following
  `impl` blocks reaches no method at all, which took coverage from 412 items to 3,090.
- The pinned rustfmt nightly moved to `2026-08-10`, checked to format this tree byte-identically
  first, so the bump carries no reformat.
- `Cargo.toml` names this fork's author beside the upstream two, and `LICENSE` carries its copyright
  beside the original — the file had gone untouched since 2020.
- A CI job fails when `build.yml`'s container digest pins go stale; line endings are normalized so
  the format gate passes on Windows; the declaration-diff test locates `lib.dom.d.ts` on every
  platform rather than a hardcoded five.

## 📦 ⟩ [v4.1.1] (npm) / [v0.3.1] (crate) ⟩ August 10, 2026

Correctness. No new API.

This release is the result of auditing the whole fork against samizdatco `v3.0.8` — the commit this
history diverges from, so `git diff` answers the question directly. Method was differential
rendering: 44 cases drawn through both builds and compared pixel by pixel, on the CPU rasterizer and
on both GPU backends. Everything below was measured rather than reasoned about, and every number
quoted is reproducible.

### Rendering regressions

Three, all introduced by phyron's migration from `skia-safe` 0.88 to 0.99 — where the mutable `Path`
API was replaced by `PathBuilder` — and none present in samizdatco. They share a shape: the new call
compiles, reads as equivalent, and draws differently, because its default differs from the old one.

**Arcs started a new contour instead of continuing the current one.** `add_path` took an explicit
`Extend` upstream; the migration passed `None`, which means `Append`. Stroking looked identical, so
the bug was invisible until something was filled — at which point the arc became a separate region.
`ctx.arc`, `ctx.ellipse` and `ctx.roundRect` were all affected:

| case | differing pixels, before |
|---|---|
| rounded rect built from `arc()`, filled | 44.35% |
| clip through an arc | 27.00% |
| fill after `lineTo` + `arc` | 16.95% |
| `ellipse()` filled | 13.14% |

All are byte-identical to upstream now.

**`Path2D.roundRect` began at the wrong corner.** Skia m86 changed `addRRect`'s default start index
from 0 to 6 or 7 by winding direction; upstream pins 0. A closed rounded rectangle fills and strokes
the same either way, so this surfaced only through `Path2D.d`, through dash phase along the outline,
and through where `Extend` attaches. `ctx.roundRect` deliberately keeps 6/7 — the two entry points
are not the same upstream, and making them agree is itself a regression.

**A conic with a non-positive weight drew a curve.** `SkPath::conicTo` opened with
`if (!(w > 0)) lineTo(x2, y2)`; `SkPathBuilder::conicTo` has no such branch and stores any finite
weight as a conic. A zero weight rendered through the control point instead of straight, and a
negative one produced a rational curve whose denominator crosses zero.

That last one had been failing in CI since January and was skipped rather than diagnosed — as a
platform difference, because it correlated with one. It was not: the GPU rasterizer drew the
degenerate conic as a line while the CPU rasterizer drew the curve, so it failed on exactly the
runners without a GPU. Both backends agree now and the test runs everywhere.

### Features that were declared but did nothing

**`colorType` and `colorSpace` on `new Canvas()`** were stored and never read. Two layers each
substituted their own default before the canvas's value could apply, so the struct-update fallback
meant to carry it could never fire. A canvas built with `{colorType: 'Gray8'}` exported 8-bit RGBA;
`display-p3`, `rec2020` and `hdr10` all produced sRGB. Both are honoured now, and an explicit option
on the call still wins. `Canvas.colorType` is readable, which is what makes it observable.

Separately, `colorType` was being used to allocate the *compositing* surface rather than the readback
format. Rasterizing into an opaque type turned the transparent clear black and resolved every blend
against it — `rgba(255,0,0,0.5)` read back as `[128,0,0,255]` instead of `[255,0,0,255]` — and the
degraded surface was cached and reused for later exports.

**`ctx.saveLayer()` was discarded by any transform or clip inside it.** The recorder rebuilds the
recording canvas's save stack from a fixed depth whenever the matrix or clip changes, and knew
nothing about layer frames, so the layer was composited while still empty and everything after it
landed at full alpha. The stack floor now moves with open layers.

**Paragraph decorations were drawn in transparent ink.** An underline or line-through set through
`ParagraphBuilder` rendered nothing unless `decorationColor` was also passed: the text color goes in
as a foreground *paint*, leaving `TextStyle::color` at its default, and Skia defaults the decoration
color to transparent. It now falls back to the text color, as CSS does.

**A registered font answered every lookup** *(this is the crate fix — see below)*.

### `imageSmoothingQuality = "high"`

Was Mitchell bicubic for every draw, which matches no engine. A cubic resampler makes Skia ignore the
mipmap chain, so heavy minification aliased where upstream's trilinear `high` did not.

There is no specification to appeal to — the HTML spec declines to mandate an algorithm, and Firefox
does not implement the property at all. Chrome's mapping is scale-aware: Mitchell only for a strict
upscale, trilinear otherwise, decided from the full local-to-device matrix so the canvas transform
counts. Ported directly.

| zone plate, 512 → 64 | roughness |
|---|---|
| upstream (trilinear) | 65.46 |
| Mitchell everywhere (4.1.0) | 76.22 |
| this release | 65.44 |

So `high` now costs nothing against upstream when minifying, and still means something when
magnifying.

### Performance

**Path construction was quadratic.** The implicit-`moveTo` check ran before every segment append and
answered "is this path empty?" by copying the entire path. 16,000 `lineTo` calls took 134 ms against
upstream's 3.4 ms; at 64,000 the two are within 10% of each other.

### Types and packaging

- `MaskFilter`, `Shader` and `ColorMatrix` are exported from the ESM entry point, which had 24 of the
  27 names the CommonJS one provides.
- The browser build no longer claims exports it cannot have. It gains `ColorMatrix`,
  `TextDecoration` and `TextDecorationStyle` — the three that are plain data — and a
  `browser.d.ts` so the rest are absent from its types rather than declared and undefined.
- `sharp` is declared as an optional peer dependency. Nothing needs it at runtime, but the types
  hard-import it, and it was declared nowhere at all.
- `ImageDataSettings.colorSpace` is narrowed to `"srgb"`, which is all the constructor accepts;
  `fillStyle`/`strokeStyle` accept `Shader`, which the runtime always did; and
  `ParagraphBuilder.Make`'s unread `fontLibrary` parameter is gone.
- A short array assigned to `fillStyle` is parsed as CSS again. `['red']` set red upstream, via
  `toString()`; the float-color branch added for `[r,g,b,a]` was rejecting it.

### Tests

156 → 187 JavaScript tests, and none skipped. 87 of the 107 Rust tests had never run: both workflows
passed `--test native_api_contract`, which restricts the run to a single target. Unblocking them
surfaced the font-manager bug below on the first try.

New coverage for `saveLayer`, `dither`, the `colorFilter`/`maskFilter`/`imageFilter`/`Shader` context
properties, `TextDecoration`, `colorType` inheritance, and the sampler selection above. The
`ParagraphBuilder` tests now read back what they configure — they previously asserted only that
height was greater than zero, so `maxLines` and `ellipsis` could have been ignored entirely.

`npm test` no longer silently tests the published binary instead of the one just built: an installed
platform package outranks `lib/skia.node`, and `MEO_SKIA_CANVAS_BINARY` now overrides both. On the
same tree, `node --test` reports 114 pass / 73 fail against the published binary where `just test`
reports 187 / 0.

### Crate `0.3.1`

One fix reaches the Rust API. `TextEngine::new` passed no default *family* to the font collection,
and Skia's `defaultFallback()` needs a name to resolve — without one, an unmatched lookup falls
through to the asset provider. So once a `FontManager` had any typeface registered, that typeface
answered every query, including one naming an unknown family and one naming no family at all:

| `layout_text("Studio", 24px)` | before | after |
|---|---|---|
| system fonts, no family | 68.05 | 68.05 |
| `FontManager`, registered family | 55.61 | 55.61 |
| `FontManager`, unknown family | 55.61 | 68.05 |
| `FontManager`, no family | 55.61 | 68.05 |

## 📦 ⟩ [v4.1.0] (npm) ⟩ August 9, 2026

Linux compatibility. No API changes, and the crate is unaffected.

### The Linux binaries now load where they always claimed to

The published binaries required **glibc 2.35** while the documentation promised
2.28. Anyone below that installed successfully and then hit a loader error,
having read that it would work.

The build container's final stage had moved to Debian 12 (glibc 2.36) while the
comment above it still described Debian 10 (2.28), and nothing checked. The
build now runs on AlmaLinux 8, so the floor is **2.28** — the lowest of any
release in this lineage.

Newly supported, none of which worked in 4.0.0:

| | glibc | |
|---|---|---|
| RHEL / Rocky / Alma 8 | 2.28 | supported to 2029 |
| Ubuntu 20.04 | 2.31 | |
| AWS Lambda / Amazon Linux 2023 | 2.34 | supported to 2028 |
| RHEL / Rocky / Alma 9 | 2.34 | supported to 2032 |

`libstdc++` mattered as much as glibc here, and was easier to miss. The module links against
it too, and a symbol newer than the target's fails at load identically. 4.0.0 required
`GLIBCXX_3.4.30`, while RHEL 8 ships 3.4.25 and RHEL 9 ships 3.4.29 — so RHEL 9 could not have
loaded it even with the glibc floor fixed.

The new build toolchain links its own newer `libstdc++` statically and leaves only the old
baseline symbols dynamic, so 4.1.0 needs just `GLIBCXX_3.4.21` — below every supported
platform.

Both are now asserted after each Linux build, glibc against a 2.34 ceiling and `GLIBCXX`
against 3.4.25, so neither can drift back unnoticed.

### The AWS Lambda layer works for the first time

`aws-lambda-x64.zip` and `aws-lambda-arm64.zip` have been published on every
release since 3.6.0 and **have never been loadable**. Lambda's Node runtimes run
on Amazon Linux 2023, which is glibc 2.34 — one minor version below what the
binaries required. It failed with `ERR_DLOPEN_FAILED`, not a graceful error.

Fixed by the floor above, and now verified on every build: CI loads the
published layer on `public.ecr.aws/lambda/nodejs:22` and renders through it. A
separate check asserts the glibc floor after each Linux build, so neither can
drift again unnoticed.

## 📦 ⟩ [v4.0.0] (npm) / [v0.3.0] (crate) ⟩ August 9, 2026

### Breaking

- **Node 22 is now the minimum.** `engines` moves from `>=20.11` to `>=22`.
  Node 20 reached end-of-life on 2026-04-30, so it no longer receives security
  fixes; the CI matrix drops it and now covers 22 and 24. Nothing in the addon
  requires a 22-only API today — this is a support-window decision, and 3.x
  remains installable for anyone still pinned to 20.

### Rendering

- **Skia M150**, by way of `skia-safe` 0.99 (was 0.97.2 / M148).
- **Vulkan setup moved to the `BackendContext` builder.** `BackendContext::new`
  is deprecated as of `skia-safe` 0.98; the Vulkan engine and renderer now use
  `BackendContext::new_builder(...).build()`. No behavior change — the
  deprecated constructor would have become a hard break on the next bump.

### Dependencies

- `detect-libc` 2.1.1 → 2.1.2, `follow-redirects` 1.15.11 → 1.16.0,
  `https-proxy-agent` 7.0.6 → 9.1.0, plus five dev-dependencies.
- Rust dependencies advanced across their semver-incompatible boundaries.

## 📦 ⟩ [v3.6.0] (npm) / [v0.2.0] (crate) ⟩ May 27, 2026

CanvasKit → phyron-skia-canvas API parity, P0 + P1.
Both the Node addon and the Rust crate gain:

### Text / Paragraph

- **OpenType `fontFeatures`** on the paragraph path (`TextStyleInput`
  `fontFeatures: [{name, value}]`; native `TextStyle.font_features`) --
  small caps, ligatures, oldstyle/tabular figures, stylistic sets, etc.,
  on multi-style server text. Closes the PP-780 small-caps trigger.
- **Strut style, half-leading, text-height-behavior, max-lines** for
  deterministic line boxes and first/last-line leading trim
  (`ParagraphStyleInput.strutStyle` / `textHeightBehavior`,
  `TextStyleInput.halfLeading`).
- **Paragraph overflow / glyph queries**: `didExceedMaxLines`,
  `getNumberOfLines`, `getRectsForPlaceholders`, `getUnresolvedCodepoints`.
- **Font fallback** enabled on every text engine collection (missing
  glyphs resolve against system fonts instead of tofu).

### Paint / compositing

- **`setDither`** (`ctx.dither`) -- anti-banding for gradients and dark
  frames.
- **`MaskFilter.MakeBlur`** with `BlurStyle` {normal, solid, outer,
  inner} + `respectCTM` (`ctx.maskFilter`) -- glows, feathered edges,
  outline blur.
- **`Canvas.saveLayer(alpha?, bounds?, backdrop?)`** -- grouped
  opacity/blend/filter compositing plus a backdrop filter for
  blur-behind.
- Per-draw blend modes **`clear`, `modulate`, `destination`** wired into
  `globalCompositeOperation`.

### Effects / shaders

- First-class **`Shader`** with `MakeFractalNoise` / `MakeTurbulence`
  (settable as `fillStyle`/`strokeStyle`); native `NativeShader` also
  gains radial / sweep / two-point-conical gradient factories.
- **`ColorMatrix`** helpers (`identity`, `concat`, `postTranslate`,
  `rotated`, `scaled`) for hue / saturation / brightness grades.

### Images

- **Cubic (Mitchell) sampling**: `ctx.imageSmoothingQuality = "high"`
  now resamples bicubically; native `SamplingMode::Cubic`.

### Rust API shape (breaking, crate only)

The Rust consumer API was made idiomatic. This does not affect the Node
package, whose JS surface is unchanged.

- The former `skia_canvas::native::*` facade is gone. Its modules now sit
  at the crate root, re-exported through a new `skia_canvas::prelude`:
  `use skia_canvas::prelude::*;`.
- Types dropped their `Native` prefix (there was exactly one of each):
  `NativePaint` -> `Paint`, `NativeError` -> `Error`,
  `NativeCanvas` -> `Canvas`, and so on.
- The Node/Neon binding moved under an internal `pub(crate) mod node`;
  it is no longer part of the crate's public surface.

  Migration: replace `use skia_canvas::native::{...};` with
  `use skia_canvas::prelude::*;` and drop the `Native` prefixes.

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

## 📦 ⟩ [crates.io 0.1.0] ⟩ May 14, 2026

First publish to crates.io as `skia-canvas`. The Rust API surface lives under
`skia_canvas::native` and is held to a stable Rust contract: no `skia_safe` or
`neon` types appear in public signatures, enforced by a compile-time pin in
`tests/native_studio_renderer_adapter.rs`.

### What lands in 0.1.0

- **HTML Canvas-shaped Rust API**: `NativeBackend`, `NativeSurface`,
  `NativeCanvas`, `NativePaint`, `NativePath`, `NativeShader`,
  `NativeColorFilter`, `NativeImageFilter`, `NativeImage`,
  `NativeFontManager`, `NativeTextEngine`, `NativeTextLayout`. Save /
  restore, path ops, gradient + pattern shaders, filter chains,
  raw-pixel image creation, premultiplied linear-light colors.
- **Color pipeline**: `LinearColorSpace::{Srgb, DisplayP3, Rec2020}`
  for the working space; `PixelColorSpace` with linear / gamma
  variants for export. Surfaces composite at RGBAF16 precision;
  `RgbaLinear` is the typed premultiplied linear-light color
  primitive. Color-space tagging is plumbed through every Skia
  handoff so `RgbaLinear` values are never silently double-decoded.
- **Render engine selection**: `RenderEngine::{Auto, Cpu, Gpu}` on
  `SurfaceOptions`. `Auto` picks GPU (Vulkan / Metal) when compiled
  in and runtime-reachable; `Cpu` forces the raster path; `Gpu`
  returns `NativeError::EngineUnavailable` if no backend is
  selectable. `NativeBackend::engine_status` returns a typed
  snapshot.
- **Variable-font axis instantiation**:
  `TextStyle::font_variations: Vec<FontVariation>` pins variable
  axis positions before paragraph layout (mirrors CanvasKit's
  `fontVariations`). `NativeTextEngine` builds a per-call
  `FontCollection` whose dynamic `TypefaceFontProvider` carries
  variable-typeface clones instantiated at the requested axes
  (clamped to each typeface's declared `[min, max]`). Without a
  pinned `wght`, one is synthesized from `font_weight` so existing
  weight-only `TextStyle`s still respond on variable typefaces.
  New `FontAxisTag` (`WGHT` / `WDTH` / `OPSZ` / `SLNT` / `ITAL`
  associated constants; `FontAxisTag::new(b"xxxx")` for compile-time
  tags; `FromStr` impl for runtime input) and `FontVariation` types.
- **Skia engine**: ships against
  [`skia-safe` 0.97](https://crates.io/crates/skia-safe/0.97.0) which
  vendors [Skia M148](https://skia.googlesource.com/skia/+/refs/heads/chrome/m148/RELEASE_NOTES.md).
  `allsorts` (used for font subsetting on the Neon side) is on 0.17.
- **Cargo features**: `vulkan` (Linux / Windows GPU), `metal` (macOS
  GPU), `window` (`winit` event loop), `freetype` (FreeType + WOFF2
  bundled), `node-addon` (registers the Neon entry point so the
  cdylib loads as a Node.js addon). The default feature set is
  empty -- pure-Rust consumers pick the backend they need.
- **Examples**: `cargo run --example basic_render --no-default-features --features "vulkan,freetype" --release`.
- **Docs**: `docs/api/native-rust.md` and crate-level rustdoc cover
  color spaces, surfaces, paint, paths, shaders, filters, images,
  text, fonts.

### Notes

- The npm package `phyron-skia-canvas` and the cargo crate
  `skia-canvas` ship from the same source tree but version
  independently.
- HDR (>1.0) values are preserved on CPU surfaces. GPU drivers may
  clamp during compositing; pin `RenderEngine::Cpu` for bit-exact HDR
  round-trips.

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

[unreleased]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.1.0...HEAD
[v5.2.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.1.0...v5.2.0
[v5.1.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.0.0...v5.1.0
[v5.0.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v4.1.1...v5.0.0
[v4.1.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/v4.1.0...v4.1.1
[v4.1.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v4.0.0...v4.1.0
[v4.0.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.7.0...v4.0.0
[v3.6.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.5.2...v3.6.0
[v3.5.2]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.5.1...v3.5.2
[v3.5.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.5.0...v3.5.1
[v3.5.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.4.5...v3.5.0
[v3.4.5]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.4.4...v3.4.5
[v3.3.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.2.2...v3.3.0

<!-- The crate has tags only from 0.3.0; earlier versions link to their docs. -->

[v0.7.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.6.0...rust-v0.7.0
[v0.6.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.5.0...rust-v0.6.0
[v0.5.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.4.0...rust-v0.5.0
[v0.4.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.3.1...rust-v0.4.0
[v0.3.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.3.0...rust-v0.3.1
[v0.3.0]: https://github.com/l7aromeo/meo-skia-canvas/releases/tag/rust-v0.3.0
[v0.2.0]: https://docs.rs/meo-skia-canvas/0.2.0
[crates.io 0.1.0]: https://docs.rs/skia-canvas/0.1.0

<!-- Entries below v3.3.0 are upstream's releases and link to upstream. -->

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
