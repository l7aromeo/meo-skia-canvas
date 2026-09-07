# Changelog — the Rust crate

Changes to the crate `meo-skia-canvas`, published on crates.io and versioned
independently of the npm package.

> Sibling files: the npm addon's is [CHANGELOG-npm.md](CHANGELOG-npm.md), and
> [CHANGELOG.md](CHANGELOG.md) is the index. The channels number separately and are not
> comparable version for version — the crate starts at `0.2.0`, npm continues
> `phyron-skia-canvas`'s from `3.6.0`.
>
> **A change that affects both surfaces appears in both files**, written for
> each audience rather than copied. `TextDirection` gaining `Inherit` is the
> same commit as `ctx.direction` reporting `"inherit"`, and a Rust reader
> should not have to work that out from a JavaScript property name.
>
> Releases from before the split are at the foot of this file, so the crate
> tarball carries them: `CHANGELOG-npm.md` is not in `Cargo.toml`'s
> `include` list and is not there to be pointed at.

## 📦 ⟩ [UNRELEASED] ⟩ September 7, 2026

**The version is not yet decided.** Ten entries below break, so this is not a
patch. Five of them stop a caller compiling -- `Font::slant`,
`TextDirection` and `Error::InvalidRadius` all sit on types that are not
`#[non_exhaustive]`; marking the three gradient types `#[non_exhaustive]`
breaks an exhaustive match once so that the next interpolation space does not;
and `Image::from_pixels` changes signature while the type it used to take is
removed. The other five change a value or a rendering without any diagnostic
at all, and are marked where they appear.

### Breaking

- **`Image::from_pixels` takes a `PixelExportOptions` where it took a
  `PixelFormat` and a `PixelColorSpace`, and `PixelFormat` is gone with it.**
  The crate had two vocabularies for one choice. `PixelExportOptions` carries
  the depth, the colour space and a `premultiplied` flag as independent parts,
  and is what a readback returns; `PixelFormat` baked depth and alpha mode into
  four variant names, and was reachable from this one method.

        // 0.15.0
        Image::from_pixels(&px, w, h, stride,
            PixelFormat::Rgba8UnormUnpremul, PixelColorSpace::Srgb)?;
        // now -- the same layout, and what a readback hands back
        Image::from_pixels(&px, w, h, stride, PixelExportOptions::default())?;

  `ImageData::from_pixels` already took `PixelExportOptions`, so the two
  siblings now agree, and a buffer read out of one canvas can be handed to
  another without restating what it is.

  The four-variant enum was under-expressive rather than protective, which is
  the part worth knowing before assuming the wider type is riskier: it could
  spell three depths and, above eight bits, only the premultiplied half --
  there was no `Rgba16fUnpremul`. All thirty crossings of the fifteen depths
  with either alpha mode build, and
  `every_depth_crosses_with_either_alpha_mode` pins that, with a control that
  must be refused so a green run means something.

  `Error::UnsupportedPixelFormat` goes too. Nothing produced it -- every
  `PixelFormat` mapped to a Skia colour type -- and the type it carried no
  longer exists. `Error` is not `#[non_exhaustive]`, so an exhaustive match
  over it needs the arm removed.

- **`Context2D::is_point_in_path` and `is_point_in_stroke` no longer map the
  point through the current transform when testing the context's own path.**
  That path is accumulated in device space, so mapping the query point put the
  two in different spaces and a hit was reported at the wrong place -- or not
  at all where it had just been drawn.

        translate(100); rect(0, 0, 10, 10); is_point_in_path(105, 5)
        0.15.0   false        now   true

  Against a `Path2D` the mapping is what puts the point and the path in the
  same space, so it stays; the three `Path2D` cases are identical on both
  versions, which is the control proving the mapping moved from callee to
  caller rather than disappearing.

- **`Font::italic: bool` becomes `Font::slant: FontSlant`.** `Normal`,
  `Italic` and `Oblique` are three values where a boolean carried two. A
  caller setting `font.italic = true` writes `font.slant = FontSlant::Italic`,
  and `Font::parse("oblique 16px X")` yields `Oblique` where it yielded
  `italic: true`. **Rendering does not change**: `FontSlant::Oblique` resolves
  to the family's italic face where it has no oblique one, which is what the
  collapsed boolean already selected.

- **`TextDirection` gains `Inherit`, and it is the new default.** The Canvas
  standard makes `inherit` the initial value of the direction attribute and a
  state it holds, naming the surrounding document's direction -- which a
  canvas does not have. `Context2D::direction()` on a fresh context returns
  `Inherit` where it returned `LeftToRight`. **Nothing about layout moves**:
  with no document to inherit from, text still lays out left to right. The
  enum is not `#[non_exhaustive]`, so both the new variant and the moved
  `Default` break an exhaustive match.

  _The same change reaches JavaScript as `ctx.direction` reporting
  `"inherit"`, where it is the release's one silent break._

- **`Error::InvalidRadius` is split out of `Error::InvalidRect`.** `Error` is
  not `#[non_exhaustive]`, so **the break is the exhaustive match**: any
  `match` over `Error` without a wildcard stops compiling.

  The variant is worth more than its name. A bad radius reported a rectangle,
  and in two of three cases that rectangle was perfectly valid -- the error
  was true about a shape that was not the problem:

        arc(10, 10, -5, ..)             InvalidRect { 15, 15, 5, 5 }   edges crossed
        arc_to(0, 0, 10, 10, -5)        InvalidRect { 0, 0, 10, 10 }   a valid rect
        round_rect(5, 5, 30, 30, NaN)   InvalidRect { 5, 5, 35, 35 }   a valid rect

  Now `InvalidRadius { radius: -5.0 }` and `{ radius: NaN }`.

- **`TextMetrics::actual_bounding_box_left` and `_right` describe the ink, not
  the advance.** Both were pinned to the advance box, so neither carried
  anything `width` did not: measuring `" H"` in Helvetica at 24px gave
  `0.000 / 24.000` -- left at the origin, right at the advance, which is that
  face's advance for the string. They now report the outline,
  `-8.555 / 22.219`. Breaking and **silent**: the old values were finite
  numbers of a plausible size, so code reading them got an answer and no
  reason to doubt it.

  The face and size are named because the figures do not survive without
  them: Arial at the same size gives `-8.590 / 22.066`. The test covering
  this asserts the relations rather than these numbers, deliberately -- an
  advance comes from the font's metrics and a bound from the rasteriser, and
  the bound is not stable across CI's legs.

- **An SVG with no stated size is measured differently, and silently.** A size
  is still returned; it is a different one.

        document                                 0.15.0            now
        viewBox 0 0 400 100, no width/height     600 x 150         300 x 75
        no viewBox, no width/height              150 x 150         300 x 150
        width="100" only, 4:1 viewBox            100 x 100         100 x 25
        viewBox 0 0 40 0                         Infinity x 150    300 x 150
        width="80" height="40"  (control)         80 x 40           80 x 40

  Three faults under one change: an unbounded width, a
  stated-dimension-squared rule no clause names and no browser follows, and a
  degenerate `viewBox` yielding a non-finite size.

- **`GradientColorSpace::Srgb` means literal sRGB, and the surface-tracking
  behaviour is now `Destination`.** One variant was doing two jobs. As the
  `#[default]` it mapped to Skia's `Destination`, which follows the canvas --
  correct, and what the HTML Standard requires, since interpolation happens in
  "the context's color space". As an explicit choice it was wrong: a caller
  writing `Srgb` on a wide-gamut canvas got that canvas's space, not sRGB.

  `#[default]` moves to `Destination`, so nothing rendered by a caller who
  never chose a space moves. Measured on a `display-p3` canvas, red to blue,
  midpoint of a 64-pixel ramp:

        following the surface   [115, 25, 142]
        literal sRGB            [115, 20, 125]
        on an sRGB canvas both  [126,  0, 129]

  Breaking and **silent**: `GradientColorSpace::Srgb` compiles exactly as
  before and renders differently on a non-sRGB canvas. Nothing in the type
  system catches it, and `#[non_exhaustive]` does not help -- it makes an
  _added_ variant safe, where this is a name that kept compiling and changed
  meaning.

- **`GradientColorSpace`, `HueMethod` and `GradientInterpolation` are
  `#[non_exhaustive]`.** Breaking for an exhaustive `match` or a struct
  literal, once. CSS keeps adding interpolation spaces; without this every one
  of them is its own breaking release, and this release already carries three
  breaks of that shape. `GradientInterpolation` is the one that bites hardest:
  two public fields and no marker means adding `alpha` breaks every literal
  construction downstream.

- **`TextStyle::slant = TextSlant::Oblique` now renders the italic face.**
  Skia's font matcher does not fall back from oblique to italic, so a family
  carrying an italic face and no oblique one rendered upright. The matcher now
  maps `Oblique` to `Italic` before matching, at both text-layout sites.
  Breaking and **silent**: an oblique run was byte-identical to an upright one
  and is now byte-identical to an italic one. At 64px Times it moves from 1687
  inked pixels at centroid 55.08 to 1656 at 55.44, with the upright and italic
  rows unchanged across both versions -- which is what makes those numbers a
  measurement of the slant rather than of anything else that moved.

  The `Font` route is not affected and did not need to be. `Font` carried
  `italic: bool` at `rust-v0.15.0` and `"oblique"` set it true, so
  `Context2D::font()` already selected the italic face; see `Font::slant`
  under Breaking, which changes the type without changing what it renders.

  The npm changelog says both of _its_ routes were affected, and both files
  are right: the two surfaces parsed the keyword separately at
  `rust-v0.15.0`. `src/context2d.rs` folded `"oblique"` into `italic: bool`
  and so reached the italic face; `src/node/typography.rs` mapped it to
  Skia's `Oblique` and so reached the upright one. One rule serves both now.

### Added

- **`Affine::from_css`**, which builds a transform from a CSS `transform`
  list -- `"translate(10px, 20px) rotate(45deg)"` and the ten other functions
  the property defines, with the units CSS gives each. `"none"`, the empty
  string and whitespace are the identity.

  The last capability the JavaScript surface had and the crate did not. A
  caller porting a transform from CSS or from the binding had no route to it:
  `Affine` is assembled from its constructors, and nothing in the crate read
  the transform grammar, though `src/css.rs` already parsed every piece of it
  for `filter`.

  Returns `Option`, like the rest of that module. "That is not a transform" is
  the whole of what there is to say, and inventing an error type here would be
  a second way to spell what `parse_filter` and its neighbours already spell
  one way.

  **The whole list is refused if any function in it is**, rather than the
  readable parts kept. A transform list missing one step puts the drawing
  somewhere else, and dropping the step nobody could parse is how a typo
  becomes a rendering bug -- the same reasoning `parse_filter` carries. An
  angle without a unit is one of those: CSS requires it and browsers reject
  `rotate(45)`.

- **`Affine::inverse` and `Affine::multiply`.** A Rust caller could not invert
  or compose a transform without reaching for `skia_safe`, which the crate's
  own API guarantee forbids surfacing.

- **Eight more `Affine` operations**, every one of which `DOMMatrix` has and
  the crate did not: a skew in each axis and in both at once, a mirror in each
  axis, a rotation named by a vector, applying a transform to a point, and
  asking whether it is the identity. Composing a skew previously meant writing
  the shear component by hand and knowing which of `b` and `c` it lands in.

  Eleven methods rather than eight, because each of the three skews comes in
  radians and degrees, matching `rotation_radians` and `rotation_degrees`.

  **The ones that build a matrix are constructors**, like `translation` and
  `rotation_degrees` beside them, rather than methods that compose onto the
  receiver. `m.multiply(&Affine::skew_x_degrees(30.0))` is exactly what
  `DOMMatrix.skewX(30)` returns; giving them a second, implicit spelling of
  composition would leave the crate with two, and `multiply` already documents
  which operand a point meets first. `transform_point` and `is_identity` take
  `&self` because they consume a transform rather than produce one.

  `rotation_from_vector` uses the direction of `(x, y)` and discards its
  length, so `(3, 4)` and `(6, 8)` agree. A zero vector gives `IDENTITY`:
  without that case `atan2(-0.0, -0.0)` would make a negative zero a half
  turn.

  `is_identity` compares components exactly, as `DOMMatrix.isIdentity` does. A
  transform that maps every point to within a rounding error of itself can
  still answer `false` -- a four-degree rotation composed with its own inverse
  leaves `a` at 0.99999994, though 336 of the 360 whole-degree rotations do
  round-trip exactly.

- **Five more interpolation spaces, and three synonyms.** `DisplayP3`,
  `Rec2020`, `ProphotoRgb` and `A98Rgb` join the eight that shipped, alongside
  the literal `Srgb` described above. `Xyz`, `XyzD50` and `XyzD65` are exact
  synonyms for `SrgbLinear` rather than new behaviour: interpolating in a space
  is convert, lerp, convert back, and every step between linear sRGB and either
  XYZ white point is an invertible linear map, which commutes with a
  componentwise lerp. Verified numerically at a worst deviation of 4.4e-16 and
  through the renderer as byte-identical buffers.

  Skia's two gamut-mapped variants are deliberately not exposed. They strip a
  gradient of all chroma -- green to yellow renders `[197, 197, 197]` against
  Oklab's `[158, 221, 0]`, with a grey-to-grey control showing it is the colour
  being destroyed rather than the pipeline failing -- because they need a
  destination gamut the gradient does not carry.

- **`GradientInterpolation` carries `alpha`**, defaulting to unpremultiplied.
  Canvas interpolates unpremultiplied and CSS premultiplied; the default is
  unchanged and the option is additive.

- **`PixelColorSpace::as_str`**, absent at `rust-v0.15.0`.

- **`Font::slant`, the builder method**, beside the field of the same name.
  `Font::italic()` already existed and remains the common case; this is the
  route to `Oblique`, which a family shipping both faces resolves differently.
  Listed separately from the field because a caller who never touches the
  struct literal still meets it.

### Changed

- **`Context2D::font()` returns the CSS serialization rather than the
  canonical form.** Components at their initial value are dropped, weight 700
  is spelled `bold`, and the line height is gone -- "the serialized form of
  the current font of the context (with no 'line-height' component)", as HTML
  puts it.

        line_height 24   ->  "16px Times"
        weight 700       ->  "bold 16px Times"
        weight 800       ->  "800 16px Times"

  The practical consequence for a Rust caller: **`Font::parse(ctx.font())` no
  longer round-trips**, because the line height it set is not in what it reads
  back.

- **Text is measured and drawn the way the Canvas standard describes it**,
  across eight corrections -- `max_width` condensing the run instead of
  wrapping and clipping it, kerning stopping at a word boundary,
  `set_letter_spacing` adding one unit per character rather than `n - 1`, and
  `text_align` counting the trailing letter-space among them. Reached through
  `fill_text`, `measure_text`, `outline_text` and `set_letter_spacing`.

  Three measured through `measure_text`, which is where a caller meets them.
  Helvetica at 24px for the first two, since a width is a property of the
  face and these figures are not reproducible without it; the third is
  arithmetic and holds for any face:

        a control character truncated the run
          "A\u{b}B C D"      16.010  ->  86.680     16.010 is "A" alone
        kerning applied across a space
          "A V"               37.370  ->  38.680     "AV" 30.250 unchanged
        letter_spacing added n-1 units
          four glyphs at 10   30.000  ->  40.000     three units against four

  The first is the worst of them: VT, FF, U+2028 and U+2029 each discarded
  everything after the control character, so the crate returned a measurement
  for a string the caller never passed.

        max_width ink   1048  ->  2197        unconstrained 4134

  The released crate wrapped at the width and discarded everything past the
  first line, so most of the run was not drawn.

  The ink box is taken from the outline rather than from the rasterised box,
  which was outward-rounded and padded a pixel a side. `both_surfaces_measure_the_same_lines`
  pins crate line widths against the JavaScript surface and this release moved
  those numbers, which is a before-and-after of crate-rendered output sitting
  in the repository:

        line 0   91.59   ->   87.99851
        line 1  111.41   ->  107.834755

  about 1.8 narrower at each end.

- **`outline_text` fills where `fill_text` draws.** A face kerned through the
  legacy `kern` table reported glyph positions half a kern to the right of
  where it painted them, so a path taken from the outline did not fill as the
  same string draws.

- **`lab()` and `lch()` apply the D50-to-D65 chromatic adaptation CSS Color 4
  requires.** `csscolorparser` omits it. The error is zero on the neutral axis
  and grows along `b`, so it read as slightly wrong blues and yellows rather
  than an obviously wrong colour. Through `set_fill_style_css` on a default
  build:

        lab(30% 30 -60)   ->  63, 56, 167        red was 31 unadapted

- **A `rec2020` colour is converted to sRGB before the paint is set**, rather
  than being handed to Skia with its space attached. Silent: a colour is still
  painted, a different one.

        fillStyle                             0.15.0        now
        color(rec2020 0.2 0.2 0.2)            40,40,40      67,67,67
        color(rec2020 0.8 0.3 0.1)            248,0,0       255,56,10
        color(display-p3 0.2 0.2 0.2)  ctrl    51,51,51      51,51,51
        rgb(51 51 51)                  ctrl    51,51,51      51,51,51

  The second row is the one that matters: the old path was not merely dark, it
  was **clipping**, and a channel reading 0 where 56 is right carries no
  information a later correction could recover. Both controls are unchanged,
  so the change is confined to Rec. 2020.

- **`Context2D::round_rect` starts its contour where the standard does.** The
  shape is unchanged and the dash phase moves -- dashed ink 844 against 824,
  with solid ink 1451 on both as the control -- and so does where the pen is
  left. A `line_to` after a `round_rect` now runs from the top-left corner:
  sampling the stroke at (11, 11) gives alpha 0 on `rust-v0.15.0` and 255 now,
  while (11, 24) and (24, 11) are unlit on both. It previously started at none
  of the three sampled corners, consistent with Skia's own start index landing
  mid-edge.

- **Two `Error` variants print differently.** `InvalidRect` was
  `invalid rect: {rect:?}` and now names the rectangle it was given as
  `{w}x{h} at {x},{y}`; `UnsupportedPixelColorSpace` prints the colour space's
  CSS name rather than its `Debug` spelling. Both are `Display`, so a caller
  logging the error or matching on its text sees different output. Which
  variant is returned is unchanged -- that is `Error::InvalidRadius` under
  Breaking, and this is separate from it.

### Fixed

- **A cropped readback at a density other than 1 could report no
  intersection** with a region it covered. The page bounds are scaled into
  device space before the test.

- **`get_image_data` spans the last column of a maximum-width canvas.** Six
  pixels requested at x=16777213 on a canvas 16777220 wide came back 7x1 and
  now come back 6x1.

- **`draw_image_sized` and `draw_image_region` draw a negative extent rather
  than nothing.** A negative width or height still names a well-formed
  rectangle -- the standard defines the destination by its corners, not by a
  direction -- but `SkRect::from_xywh` gives that one `left > right`, and Skia
  declines to draw an unsorted rectangle, so the call was a silent no-op.
  Measured against Chrome 148, which draws the sorted rectangle for a negative
  `dw`, `dh`, `sw` or `sh`, with a `scale(-1, 1)` control in the same run to
  show the probe reports a flip where there is one. Both rectangles are sorted
  now, which is what the binding had been doing on its own side.

- **`Window { visible: false }` opens a hidden window** -- feature `window`.
  It opened a visible one that took focus: the option was honoured at
  construction and discarded a step later.

### Internal

- **Nine enum parsers produce this crate's types rather than Skia's**, across
  ten parsers. Nothing observable changed on either surface: no string
  vocabulary moved, every refusal message is byte-identical, and the public
  enums carry the same variants they did at `rust-v0.15.0`.

  The parsers are unreachable from Rust for a plainer reason than the module
  being private: **this crate's API takes the enum, never the string.**
  `set_line_cap` takes a `StrokeCap`, and none of the nine is called from any
  crate-public file. `node-addon` gates nothing -- it is an empty feature that
  registers `#[neon::main]`, and `src/node` compiles for every consumer -- so
  a reader checking whether the module is built would find that it is and
  conclude the opposite.

### Not a crate change

Recorded because each was checked and the answer was no, and because the npm
changelog carries an entry that a reader might expect to find here.

**What this section covers, so omission can be told from oversight.** It names
the npm entries a Rust reader has reason to look for: those describing
behaviour that sounds like the engine's. It deliberately does not enumerate
the npm-only changes to `lib/classes/*.js`, `lib/index.d.ts` and the wrapper's
own internals -- roughly nine of them -- because nothing about a JavaScript
declaration or a Neon wrapper suggests a crate counterpart to go looking for.
If a reader does want that list it is `CHANGELOG-npm.md` in full; this file
does not mirror it.

- **`measureText` reporting `height`** is npm-only. The crate's `TextMetrics`
  carried the field at `rust-v0.15.0` and reports the same value on both.
- **The gradient that paints nothing, the numeric style-code refusals, and the
  declaration corrections** are all binding-side. A crate gradient is a
  `Shader`, which refuses fewer than two stops and never reports itself
  opaque.

---

## Releases before the channels were separated

Everything below shipped before the changelogs were split, under the heading
it was cut with. Each of these releases carried the crate; most carried the
npm addon in the same cut, and the heading says which.

**Their entries are mostly not separated by channel.** Where one is, it says
so on its own -- a `Crate 0.8.0 -- breaking` section, a `**Crate only**`
line, an italic `_(Rust only)_` after the bold lead. Nine places do that and
every one of them marks the crate, so **a marked entry below is the crate's
alone**; nothing in this history marks an entry as npm's. An unmarked entry
either affected both surfaces or affected npm alone, and the file does not
record which. They are reproduced
whole rather than filtered, because deciding which to drop would mean
deciding from memory rather than from the code.

## 📦 ⟩ [v5.9.0] (npm) / [v0.15.0] (crate) ⟩ September 6, 2026

An SVG release. The crate could rasterize one but could not answer anything about
it first, and the npm package could size one but not recolour it -- so a document
whose size you did not already know, or that you wanted in two colours, meant
either guessing or shipping a second file.

Fixing the first of those turned up the second thing in this release: the sizes
the library was already producing for a document declaring physical units were
6.25% small -- 6.6% for `pt` and `pc`, which carry a second error -- and had been
since it learned to size SVG documents at all, in `349a0e6d` last May. Nothing reported it, because a wrong size is still a size.

### Added

- **`Svg` in the crate: parse a document without rasterizing it.** `Svg::parse(xml)`
  holds the parsed DOM, and the three questions that have to be answered before a
  size is chosen are now askable:

  ```rust
  let mut svg = Svg::parse(xml)?;
  let size = svg.intrinsic_size();     // what the document asks to be drawn at
  let derived = svg.is_autosized();    // whether that was read or derived
  svg.set_current_color(color);
  let image = svg.rasterize(w, h)?;
  ```

  Sizing an SVG runs the opposite way round from a bitmap: a bitmap reports its
  extent as soon as it decodes, while an SVG's extent is what decides the box to
  rasterize into. `Image::from_svg_xml(svg, w, h)` is unchanged and is now exactly
  `Svg::parse(svg)?.rasterize(w, h)`, so the two paths cannot answer differently.

  The derivation itself is not new -- the Neon binding has always had it, and the
  npm package has always sized SVG sources with it. What is new is that it lives
  at the crate root, where AGENTS.md says the public API lives, rather than only
  behind `pub(crate)`.

- **`Image.currentColor` on npm, and a `currentColor` option on `loadImage`.**

  ```js
  const icon = new Image();
  icon.currentColor = "red";
  icon.src = svgBuffer; // recorded once, colour already applied

  // or, the same thing at load:
  const icon = await loadImage("logo.svg", { currentColor: "red" });
  ```

  A browser cannot do this. An SVG referenced by `<img>` is an isolated document:
  the page's CSS, including its `color`, does not reach inside it, so recolouring
  one in a browser means inlining the markup, which is what SVGR and its
  neighbours exist for. There is no inlining step here, so this sets the SVG
  root's own `color` -- the mechanism `currentColor` was defined for, reached by a
  route a browser does not have.

  Ordering is ordinary JavaScript ordering rather than an API distinction: set
  before the source and it costs nothing, set after and the document is parsed and
  recorded again, because a `Picture` has its paint resolved into a display list
  and cannot be recoloured in place.

  The getter reports what was set, serialised through the same function
  `fillStyle` uses, and `null` before anything is set. It answers `null` on a
  raster image whatever was assigned, because there is nothing there for
  `currentColor` to reach and a getter answering with a colour would say
  otherwise. A subtree declaring its own `color` keeps it -- a `currentColor`
  fill inside `<g color="#0000FF">` still paints blue under a red override --
  while a `color` on the root itself is what the override replaces. So the getter
  reports the override, not its effect.

- **`Svg::set_current_color` on the crate**, which is what the above goes through.
  Set on the root and inherited from there, so it applies at any depth and to
  strokes as readily as fills, and it carries alpha: a colour at half alpha paints
  `currentColor` at half alpha rather than being flattened to opaque.

### Changed

- **An SVG sized in physical units now rasterizes at CSS's 96 dpi, where it used
  to come out 6.25% small (6.6% for `pt` and `pc`).** Measured against the previous behaviour, for a
  document declaring `10<unit>`:

  ```
  unit  before     now       ratio
  px     10.0000    10.0000  1.00000
  in    900.0000   960.0000  0.93750
  cm    354.3307   377.9528  0.93750
  mm     35.4331    37.7953  0.93750
  pt     12.4530    13.3333  0.93397
  pc    149.4355   160.0000  0.93397
  ```

  0.9375 is 90/96. skia-safe's shim builds the length context itself and takes
  Skia's default of 90 dpi, where CSS Values and Units 3 pins `1in` to 96 px and
  SVG 2 defers to CSS. `pt` and `pc` carried a second error on top, because
  Skia's `kPTMultiplier` divides by 72.272 rather than CSS's 72. Every unit is now
  converted here rather than read from Skia, each factor a named constant citing
  the section that fixes it.

  **This changes output.** A document declaring `width="10cm"` rasterizes 6.7%
  larger than it did in 5.8.0, and one in `pt` or `pc` 7.1% larger -- the inverse
  of the ratios above, which are the old size over the new. That is the browser's
  answer and the old one was not, but it is a change to pixels a caller may have
  been depending on.

- **`em` and `ex` are read instead of falling through to the 150px default**, and
  they resolve against the root's own `font-size` rather than a flat 16px.
  `<svg width="10em" font-size="20"/>` is 200x200, where 5.8.0 answered 150x150 --
  the fallback, not a conversion. A parentless `font-size="2em"` resolves against the initial 16 and
  is therefore 32 -- the reference is what the initial value supplies, not the
  result, so `<svg width="10em" font-size="2em"/>` is 320.

  CSS requires `1ex` to be assumed at half the `em` where the font's own metric
  cannot be read, which is the case here.

- **`skia-safe` 0.153.2 to 0.153.3.** `Cargo.toml` declared `"0.153.2"`, which
  Cargo reads as `^0.153.2` rather than a pin, so our builds were fixed at 0.153.2
  by `Cargo.lock` while anyone depending on this crate without that lockfile
  resolved 0.153.3. The versions we tested and our consumers compiled had drifted
  apart with nothing reporting it. 0.153.3 itself carries little for us -- Skia
  m153-0.101.2 and a Yocto cross-build fix for a platform this project does not
  build -- so taking it is about closing that gap rather than about its contents.

### Fixed

- **`Image::from_svg_xml` documented `Error::SurfaceCreate` for a failed surface
  allocation while returning `Error::DecodeImage`**, and `DecodeImage`'s own
  documentation claims that case. The comment was wrong, not the code.

### Documented

- **What an SVG `<style>` element costs, on `Svg`.** Skia's SVG module does not
  apply stylesheets, so anything declared only in a `<style>` block is lost and the
  affected elements paint as black. The documented workaround is svgo's
  `inlineStyles`, with the two defaults that decide whether it helps: it skips any
  selector matching more than one element (`onlyMatchedOnce`, which is right for a
  minifier and wrong here -- a class shared by three rects is exactly the shape
  that comes out black), and its `useMqs` default of `['', 'screen']` has a
  `'screen'` entry that matches nothing, because the string compared is the
  at-rule's name followed by its prelude, so a rule inside `@media screen`
  presents as `"media screen"`.

- **The root's size follows CSS and the document's contents do not.** Only the
  root's own `width`/`height` are converted at 96 dpi. Every length _inside_ the
  document is resolved by Skia at 90, through an `SkSVGLengthContext` built with
  no dpi argument, and skia-safe exposes no way to change it. So
  `<svg width="1in"><rect width="1in"/></svg>` is a 96-pixel box holding a
  90-pixel rect. A `viewBox` hides it, and so does content in user units, which is
  the common case.

### Internal

- **Four wall-clock assertions in the JS suite are no longer noise-dominated.**
  They compared a single timing reading against another, while `node --test` runs
  sixteen files concurrently and CI runners are shared -- so a reading was partly a
  record of what else held a core. Each trial now times both sides and the best
  whole trial is kept, which cancels a burst landing in one side and not the
  other; a ratio of separately-taken minima does not, because a short timed region
  deflates more than a long one. One of the two defects was also a 20000-op source
  build inside the timed call, landing a quarter of the heavy side's cost on the
  numerator alone. No bound moved.

- **The build fails when the rust-skia static-fontconfig patch stops applying**,
  rather than continuing against an unpatched tree.

## 📦 ⟩ [v5.8.0] (npm) / [v0.14.0] (crate) ⟩ September 5, 2026

Skia moves three milestones, and the export handle 0.13.0 introduced gains the two
things it turned out to be missing. Both channels move because a milestone bump
rebuilds the binary.

The rest is a read of the whole tree and the fixes for what it found. The JavaScript
had never been seen by a linter or a type checker -- `tsconfig` covers the two
declaration files and stops -- and most of what turned up there shares one shape: an
option accepted and then quietly not honoured. Three of those fixes change what
working code produces, which is unusual for a release of bug fixes, so they come
first.

### Changed

- **Uppercase and mixed-case CSS is now read, so values that were silently dropped
  take effect.** Keywords and unit identifiers are ASCII case-insensitive in CSS and
  none of the parsers knew it, so an assignment in the wrong case failed to parse and
  the property kept its previous value -- no error, no warning, nothing in a log.
  Every one of these is a valid declaration that did nothing before and does
  something now:

  ```
  ctx.font = "5PX serif"            ctx.font = "1EM serif"
  ctx.font = "MEDIUM serif"         ctx.font = "ITALIC BOLD 20px serif"
  ctx.letterSpacing = "2PX"         ctx.wordSpacing = "3PT"
  ctx.filter = "BLUR(3px)"          ctx.filter = "Drop-Shadow(2px 2px 2px red)"
  ctx.textDecoration = "UNDERLINE"  ctx.fontStretch = "CONDENSED"
  ```

  **A reader skimming a heading about case-insensitivity cannot tell whether this
  affects them, so it is worth checking directly.** Anyone whose codebase contains
  one of these has been getting the property's default and has laid out around it.
  Their text moves. It is the only change here that alters output for code running
  clean today.

  - The flag on the regular expression is not the fix and would have made it worse.
    `5PX` matched once `numSizeRE` gained `/i`, then missed every unit arm downstream
    and produced `NaN`, which the addon refused out loud -- turning a silent drop into
    a thrown error in a property nobody had edited. Each parser normalises at the
    point of use instead: the unit, the keyword, the filter function name and its
    argument. A colour inside a `text-decoration` is left exactly as written, because
    it goes on to a colour parser with its own names.
  - Values read back in their canonical lowercase form whatever case they were
    written in, which is what a browser reports.

- **`canvas.toSharp()` and `toSharpSync()` hand sharp the pixels the canvas actually
  holds, so thirteen colour types produce different bytes.** sharp was told four
  8-bit channels whatever the canvas was made of. **A canvas built without a
  `colorType` is byte-identical before and after**, as are `rgb`, `RGB888x`,
  `RGBA8888` and `N32` -- measured across all twenty-six published types, not
  reasoned about.

  Of the rest, eight raised an error before and can have no working code depending on
  them: `Alpha8`, `Gray8`, `R8UNorm`, `A16Float`, `A16UNorm`, `ARGB4444`,
  `R8G8UNorm` and `RGB565`, each too narrow for the buffer sharp was told to read --
  a `Gray8` canvas produced `VipsImage: memory area too small -- should be 64 bytes,
you passed 16`, naming a byte count and no option the caller had passed.

  The thirteen that changed silently are the blue-first, packed and float layouts:
  `bgra`, `BGRA8888`, `BGR101010x`, `BGRA1010102`, `RGB101010x`, `RGBA1010102`,
  `R16G16Float`, `R16G16UNorm`, `SRGBA8888`, `R16G16B16A16UNorm`, `RGBAF16`,
  `RGBAF16Norm` and `RGBAF32`. A 4x4 `RGBAF16` canvas filled solid red came back as
  `0,60,0,0` -- half-float bytes read as `u8` -- against `255,0,0,255` through PNG,
  encoding the first quarter of its buffer as a plausible image with no error
  anywhere. **If a pipeline consumed that output and compensated downstream, it now
  needs the compensation removed.**

  - The two exporters are fixed differently because only one of them can convert.
    `toSharp` owns the export and now asks the raw exporter for `rgba`, which
    converts on the way out for every published type; that makes the channel count a
    fact rather than an assumption. `ImageData.toSharp()` holds bytes that already
    exist, so it refuses by name the layouts sharp's raw reader cannot express -- its
    input takes a channel count and nothing else, so a packed, wider-than-a-byte or
    blue-first buffer has no honest description. `RGB565` turned a red fill into
    `0,0,0,248` before, and is refused now.

- **In the browser build, `toFile()` with a `{}` filename template can reject.** The
  `catch` around the zip download covered the whole chain, so a failure inside
  `generateAsync`, `payload.file` or any `toBlob` was reported to the console as a
  missing JSZip and the promise resolved anyway -- telling a caller who had bundled
  JSZip to bundle it, and reporting success for a download that never happened. Only
  the dynamic import is caught now. Code with no `.catch` on `toFile` gets an
  unhandled rejection where it previously got a silent no-op.

- **A consumer who also depends on `skia-safe` directly must move to 0.153.** This
  crate exposes no `skia_safe` type in any public signature, so there is no type
  conflict to resolve -- but Cargo will happily resolve two major versions of the
  crate and link two copies of the Skia native library. Invisible until it is a build
  error.

- **`Cargo.lock` is tracked.** The dependency counts in `THIRD-PARTY-NOTICES.md` were
  a property of whenever the graph last resolved rather than of the repository -- two
  checkouts of the same commit gave 167 and 168 linking crates. `just licenses` now
  reads both counts back out of the prose and fails when what it counted disagrees.

- **Every workflow has a timeout and a concurrency group.** All eleven. The three
  publish workflows take distinct group names and `cancel-in-progress: false`, so a
  publish is never cancelled mid-flight by a second run -- which is the partial state
  the publish recipe's own guard then refuses to resume from.

- `0.14.0` rather than `0.13.1`: under Cargo's 0.x rules minor is the breaking lever,
  and the dependency jump is breaking in practice for anyone sharing that dependency.

- **Skia M150 to M153**, by way of `skia-safe` 0.153.2. rust-skia realigned its
  version to the milestone it binds, so 0.99.0 is followed directly by 0.153.2 with
  nothing in between -- the gap reads as fifty-four releases and is three milestones,
  eleven weeks.

  - M151 through M153 document no rasterisation or antialiasing change, which is what
    separates this from the M130 to M150 bump that produced the differences listed
    under "Where output differs from upstream" in AGENTS.md. Measured rather than
    trusted: six scenes hashed on both sides -- text at three sizes, a stroked
    variant, antialiased shapes, a gradient, a blur -- byte-identical on macOS and
    Metal. Text first, because glyph antialiasing is what moved last time.
  - Performance is flat. Thirty-five timed measurements, median move -0.5%, and the
    three that moved five percent or more were single runs on sub-millisecond or
    single-encode figures.
  - `metal` and `vulkan` gained `skia-safe/ganesh` explicitly. In 0.99 those features
    expanded to `["gpu", ...]`; in 0.153 they do not, and `gpu` is an empty
    compatibility shim. Without the addition the bindings build with no backend under
    them and the feature that used to carry one silently does nothing.
  - Two entries reach this crate: `SkPath::updateBoundsCache` was removed and was
    never called here, and ICO decoding moved to a Rust decoder, which
    `tests/assets/images/format.ico` exercises through three suites.

### Added

- **`Canvas::compositing_color_type` answers what a canvas is made of, which is not
  what `colorType` says.** A canvas built with `Gray8` reports `Gray8`, holds four
  bytes a pixel and hands back one. `colorType` is an _output_ format: it decides
  what a readback or an export converts into, and the surface underneath is N32
  unless a float format was asked for. Nothing said so, and the obvious reading of a
  pixel format on a canvas is that the canvas is made of it -- so choosing a narrow
  one to quarter the memory gets the pixels and not the memory, silently.

  - Skia refuses none of these formats. A raster surface builds in every published
    one, at every alpha type, with or without a colour space -- `Gray8` at 4 MB and
    `RGB565` at 8 MB on a 2048x2048 canvas, against 16 for N32. The substitution is
    this crate's own choice, so the two reasons its comment gives are the whole of
    the case for it, and both were checkable and had never been checked. They are
    now recorded there rather than asserted: a surface cleared to transparent and
    read back reads `[0,0,0,255]` in `Gray8`, `RGB565`, `R8UNorm` and `R8G8UNorm` --
    opaque black, transparency gone -- while `Alpha8`, `A16Float` and `A16UNorm`
    keep the clear and read every colour back as `[0,0,0,255]`.
  - So the group that keeps transparency discards colour and the group that keeps
    colour discards transparency. `ARGB4444` is the only format below N32 that keeps
    both, and it is excluded for reasons that are not about correctness: four bits a
    channel quantises every intermediate blend rather than only the output -- a 50%
    alpha fill reads back at 136, since `0.5 * 15` rounds to 8 and `8/15` is `0.533`
    -- and the Metal backend refuses an `ARGB4444` surface, so the 8 MB it would save
    on a raster canvas is zero on the default backend, and taking it would mean
    routing that canvas off the GPU. Memory bought with rendering speed is the wrong
    trade.

- **`Pages::write_each` writes each page to its own numbered file.** The third gap of
  the same shape as `encode` and the spanning write: `canvas.saveAs("frame-{}.png")`
  had no Rust equivalent, and a caller working around it called `to_file` once per
  page with `EncodeOptions::page`, which re-resolves the sequence every time and
  fills the 64 MB page cache at a hit rate of zero. This is also the only route to
  `single_use`, and that flag is not a detail -- on a 150-frame export it is 594 MB
  of resident memory against 681, same milliseconds and same bytes out, because a
  page written once is never asked for again.

  - `digits` is an `Option<usize>`: `None` uses as many as the page count needs, and
    a width past `MAX_FOLIO_WIDTH` (255, `NAME_MAX` on Linux and macOS and the same
    for an NTFS path segment) is refused rather than built. The bound is on the
    allocation and not on the result -- the folio string is built before anything
    tries to open a file with it. The sentinel the binding sends is decoded at the
    boundary now, so no negative can be expressed in the type at all.

- **Two checks now run on the pull request that can break what they guard**, rather
  than only after a merge.

  - _The ABI floors, asserted inside the image that produces the binaries._
    `containers.yml` builds the container on a pull request touching `containers/`,
    loads it into the local daemon and runs `containers/abi-probe.sh` inside it. The
    floors were otherwise asserted only in `build.yml`, which is dispatch-only, so a
    base-image or toolchain change could merge and be discovered at release. This
    covers the container and not a Rust change: verified against the regression it
    exists for, dropping `-C linker=clang++` from `RUSTFLAGS` fails the probe with an
    undefined `_M_replace_cold` -- the symbol no version ceiling can see, because it
    carries no `GLIBCXX_` tag.
  - _Doc comments anchoring to the wrong item._ A block written above an existing
    block rather than above its own item is concatenated by rustdoc and lands on the
    following item, leaving the item it described undocumented. `missing_docs` is
    satisfied, because a comment exists. `scripts/check-stacked-docs.mjs` runs over a
    pull request's diff in `rust-ci.yml`, and in front of every commit through `just
precommit`. Scoped to the diff because it cannot be made exact: a stacked summary
    and a paragraph's closing sentence are textually identical, and what separates
    them is whether the sentence describes the item below.

- **`Pages::write` finishes an export to a file rather than a buffer.** `to_file`
  streams a page-spanning format straight to disk so a long animation is bounded by
  disk rather than by memory, and a handle had only `encode`, which returns a
  `Vec<u8>`. An asynchronous `toFile` built on the seam therefore buffered the whole
  file -- exactly the difference the comment on `to_file` exists to draw.

  - `to_file` is now built on it, so the page-versus-spanning question is asked once.
    Asked any other way, an `EncodeOptions::page` naming one frame of a GIF is a
    silent no-op on the writing path while the encoding path honours it.

### Fixed

- **Six native exports panicked when JavaScript called them with no arguments at
  all.** The argument decoder opens by computing how many arguments it was given as
  `cx.len() - start`, before anything has checked that there are that many, so a call
  with none evaluates `0usize - 1`. The six are the three gradient constructors,
  `MaskFilter_makeBlur`, `App_closeWindow` and `App_setRate`; the other 291 survive
  only because they take their boxed receiver first and fail on that instead.

  - The two build profiles failed differently, which is why this was worth chasing
    rather than patching. A debug build has overflow checks and panicked across the
    Neon boundary as `internal error in Neon module: attempt to subtract with
overflow` -- opaque, naming no cause, nothing a caller can handle. Release ships
    without overflow checks, so the subtraction wrapped to `usize::MAX`, the count
    check was skipped, and the export reported the wrong thing entirely --
    ``Expected a number for `x1` as 1st arg``, for a call that passed no arguments to
    be wrong about. All six now answer `not enough arguments (missing: ...)` in both
    profiles, which is what the same call already got through the JavaScript
    wrappers.

- **`getImageData` on a `BGR101010x` canvas failed the first time it was asked and
  succeeded the second.** With the GPU enabled the first read at a given state is
  served from the surface, and a Metal-backed surface refuses that destination
  format; only a second read is worth a copy of the whole page, and a raster copy of
  the same pixels accepts it. So the branch that failed was the cheap one and the
  branch that worked was the one only a repeat reached. A failed surface read now
  falls through to that copy. Swept every published colour type on both backends
  before and after: one row wrong before, none after.

- **Two threads could evict twice as much of the page cache as either intended.** The
  eviction pass reads every entry, decides what to drop from that snapshot, and only
  then removes, so two threads holding the same snapshot each took the map down to
  the low-water mark. Nothing was wrong afterwards -- it is a memo -- but live pages
  re-rasterised to refill it, and concurrent entry is now the ordinary case rather
  than a corner. One pass runs at a time; a thread that finds one running skips
  rather than queueing, because a second pass would decide from a snapshot the first
  has already invalidated.

- **Nine doc comments described the wrong item.** A doc block written directly above
  an existing one, with no item between them, is concatenated by rustdoc into a
  single comment on the item below -- so the first block's summary is read as the
  second item's, and the item it was written for is left with none. `missing_docs` is
  satisfied by a comment existing above an item and rustdoc has no opinion about
  which item a comment describes, so the whole gate is green on it.

  - One was rendering wrong on docs.rs: `Pages::write` opened with
    `spans_every_page`'s summary, and `spans_every_page` had none. The rest were in
    `pub(crate)` modules, where the only reader is someone working on the code.
    Paired mechanically rather than by taste -- in every case the orphaned summary
    named an item with no documentation at all, and the item it had drifted onto
    already had its own. One block was a link to a type that has never existed in
    this tree; it now points at the constant that names what it was describing.

- **`ctx.fontVariant` raised at the caller where every sibling property ignores.**
  Three ways in. An unknown keyword threw `Invalid font variant`; a _valid_ keyword
  in the wrong case threw the same, because the match was case-sensitive; and a
  parameterized form with an unknown name -- `"bogus(1)"` -- threw `TypeError: Cannot
read properties of undefined (reading 'replace')` from inside the parser, an
  internal error escaping rather than a refusal, because the alternates table was
  indexed without checking the key was in it. All three are ignored now, which is what
  the Canvas standard asks of an attribute setter and what `filter`, `letterSpacing`,
  `textDecoration`, `fontStretch` and `font` itself already did.

- **A negative font size was accepted.** `ctx.font = "-5px serif"` read back as
  `normal 400 -5px serif`, and `"12px/-1.2 serif"` gave a line height of `-14.4px`.
  CSS defines both over a non-negative length, so the shorthand is invalid and the
  assignment is ignored. Zero is not negative and still parses.

  - Checked in the shorthand rather than in the shared length parser, which looks
    like the obvious place and is the wrong one: a `drop-shadow` offset reaches that
    parser by the same route and is legitimately negative, so refusing there would
    have taken `drop-shadow(-20px 0 0 red)` with it. There is a test for that, beside
    the ones for the sizes.

- **Data URLs that browsers accept were reported as invalid.** `decodeDataURL`
  required a `;` between the media type and the encoding and searched only the first
  forty characters, so `data:image/svg+xml,<svg ...>` -- the form CSS and every
  icon-as-a-string helper emits -- failed through `loadImage` while the identical URL
  with `;charset=utf-8` loaded. RFC 2397 makes both the media type and `;base64`
  optional; the comma is the only required part, and a media type can be longer than
  any window worth choosing. Reading `charset` as the transfer encoding was the root
  of it: it names the text's encoding, not the URL's.

- **`ctx.createImageData(imageData)` dropped the colour space.** The one-argument
  clone passed the format and not the space, so a Display P3 buffer came back sRGB
  while `new ImageData(source)` kept it -- two documented ways to copy one object,
  disagreeing about it. Invisible for sRGB, which is why it survived.

- **`ctx.createImageData(100000, 100000)` killed the process.** V8 does not raise for
  an oversized typed array; it aborts with `Check failed: change_in_bytes <
kMaxReasonableBytes`, which no `catch` can reach. The addon already refused the same
  product on its side of the boundary. The allocation now applies the same `i32::MAX`
  bound -- what Skia can address -- and throws.

- **`loadImageData` discarded `colorType` and `colorSpace` on its raw branch.** The
  decoded branch forwarded the caller's options to the `ImageData` constructor and the
  raw branch passed width and height alone, so which one ran -- and therefore whether
  the settings survived -- depended on what came back from the fetch rather than on
  how the call was written. Only a sharp source reaches the raw branch, which is why
  it went unnoticed.

  - The two are not the same case. `colorSpace` is a tag rather than a conversion, the
    same bytes either way, so carrying it through takes the caller's assertion on the
    terms the other branch already does. `colorType` cannot be honoured there at all:
    the source arrives through `.ensureAlpha().raw()`, so the bytes are eight-bit RGBA
    by construction, and naming anything else describes them wrongly and fails a
    length check that blames a buffer the caller never supplied. It is refused by name
    -- which is how the export options already treat `bitDepth`, `fps`, `frameDelays`
    and `loop`.

- **`lib/browser.js` had never been executed by a test.** It destructures `window` at
  module scope, so requiring it under Node threw before any of its logic ran, and
  nothing under `tests/` loaded it -- 377 shipped lines reachable through the
  package's `browser` export condition, with only its declarations checked, as text.
  That is how two fixes to the Node exporter never reached their twin here. With a
  stub for the handful of DOM calls it actually makes, three of them surfaced at once:
  an out-of-bounds page error naming the resolved index instead of the page asked for,
  so page 9 of a two-page canvas was told `8 is out of bounds`; a `density` check
  demanding a whole number under a message naming a range it refused; and an
  unreachable branch implying the browser build gathers pages into a PDF, which it
  cannot.

- **One option, three validators.** `density` was any positive number for an export, a
  whole number of 1 or more for `getImageData` under a message reading "non-negative
  integer", and a third rule again in the browser build. The wording is corrected
  everywhere and the browser build now matches the Node one.

  - `getImageData` still takes only whole numbers, permanently and by decision. The
    addon rounds the scaled rectangle edge by edge, which is what makes abutting reads
    tile exactly -- the halves of an eight-wide canvas at density 1.5 sum to the whole,
    where flooring each region's size independently gives five and six against twelve.
    A page has no origin, so an export scales a size and cannot do otherwise. The two
    agree whenever the product is whole, which is why this was invisible.

- **The download integrity check failed open on a triplet the manifest did not
  list.** A missing entry read as "nothing to verify" and installed the binary -- on
  precisely the platform the release had forgotten. It is now an error, and the
  partial download is removed.

- **`snapshot` could write a short manifest silently.** GitHub returns no digest for
  an asset it is still processing, so those entries vanished without a word and the
  published package pinned fewer hashes than it had assets. `snapshot` now counts what
  it is about to write against `lib/targets.json` plus the two Lambda archives and
  refuses a short one, naming the wait as the fix. The count derives from the target
  list rather than a literal, so it cannot drift from the targets themselves.

- **Uploads refused a published release, on both sites that clobber.** Every upload
  passes `--clobber`, and a published release's assets are the ones the published
  package pins by sha256, so replacing them breaks the integrity check for everyone
  installing through the download fallback. Guarded now in `lib/prebuild.mjs` and in
  `build.yml`.

- **Six error messages said `Exptected`.** All six are `throw_type_error` text
  from the argument decoder -- the string a caller sees when they pass the wrong
  type, not an internal one.

- **`Pages::encode` never drained an autorelease pool.** The note above
  `gpu::autorelease` states the rule -- every entry point that touches Metal wraps its
  work in it -- and the Node binding obeys it. The public entry point added in 0.13.0
  did not, and it is the one a consumer calls on their own worker with no binding in
  between. A rayon worker has no pool, so the Objective-C allocations a rasterisation
  makes accumulate for the life of the process: memory growth on a Metal build under
  sustained load, no crash, and nothing a test would notice.

- **`node lib/prebuild.mjs` exited 1 after five of its eight usage lines.** `usage()`
  destructured `{ triplet }` and then referenced `version`, a `ReferenceError` that
  `main()`'s catch swallowed, so it failed silently.

- **The `build-custom` feature sanitiser admitted a bracket.**
  `/[^[a-z0-9\_\-\,]/g` has its opening bracket inside the negated class rather
  than opening it, so `metal[]` sanitised to `metal[`, which cargo then refuses. Not
  an injection -- every character that class admits is inert inside the double quotes
  it lands in -- but wrong. Checked exhaustively over every code point: the corrected
  pattern differs on exactly one character.

### Notes

- The documentation site said AVIF could not be decoded. It can, and always could
  here -- Skia ships no AVIF decoder, so this library carries its own, and it was
  already in `v5.7.0`. A reader taking the landing page at its word would have
  reached for a second dependency to read a format this one already reads.

- **None of the tooling below reaches the published package**, which ships `lib/` and
  nothing that builds it. A consumer cannot observe any of it.

  - Bun is the package manager; Node remains the runtime. Two lockfiles is a drift
    trap -- Dependabot updates one, the other goes stale, and a frozen install then
    fails on every pull request -- so `bun.lock` is the only one. Package scripts and
    the recipes that call them run through bun as well, where it was already a hard
    prerequisite, which is also the faster of the two by roughly 200ms to 30ms an
    invocation. The guard against a `Bun.*` API reaching shipped code is not a lint:
    `node --test` runs `lib/` under the runtime users actually have. Two
    consequences of the switch are fixed with it: `just docs-js` failed on any
    checkout that had never installed `scripts/typedoc`, because the recipe ran
    `npm ci` against a directory carrying only `bun.lock`, and Dependabot's two
    bun entries failed every week -- its updater image bakes in a Bun that
    cannot read the lockfile version this one writes, so it is asked nothing
    until that image catches up.
  - Prettier now formats everything it understands. `.github/` had been excluded so
    its YAML would keep matching GitHub's own documentation; five files under it were
    unformatted and the check reported success, because zero files matched once the
    ignore was applied. A gate that skips a directory says nothing about it.
  - ESLint reads the JavaScript and TypeScript for the first time. Most of what it
    found was noise, and the rest is in the fixes above -- an assertion that could not
    fail, dead branches, and a shared `var` that made two parsers unable to run
    interleaved.
  - An opt-in pre-commit hook is available through `just install-hooks`. It writes the
    one file rather than redirecting `core.hooksPath`, which would silently disable
    the four git-lfs hooks this repository already has, and it is opt-in rather than a
    `prepare` script because this project routes around install lifecycle scripts
    deliberately.
  - The test fixtures are ordinary git rather than LFS. Eleven checkouts across five
    workflows each pulled the whole 23.8 MB payload, of which 22.4 MB is
    `docs/assets`, which no workflow reads. The published crate was never affected:
    `crates-io-publish.yml` resolves LFS before packaging, and the 0.13.0 tarball on
    crates.io carries the real 8841-byte PNG rather than its 129-byte pointer.
  - A crate user can now file a bug truthfully. Blank issues are disabled and the only
    form required `npm ls`, `node -v` and a JavaScript reproduction; #36 ends with a
    hand-written "Not applicable from the template" section. There is a second form
    asking for the crate version, the resolved feature set and `rustc -vV`.
  - Dependency refresh, and a grouped Dependabot bump of three dev dependencies.

## 📦 ⟩ [v5.7.0] (npm) / [v0.12.0] (crate) ⟩ August 23, 2026

Paragraph text. Two defects, one of them silent for as long as the API has existed, and one
capability that was missing rather than broken.

### Fixed

- **A run can say which language it is written in.** Han characters are unified in Unicode: 直 is
  one codepoint drawn one way in Japanese and another in Simplified Chinese, and nothing in the
  text says which a reader should see. Skia takes a locale for exactly this; neither door passed
  one, so the platform's fallback decided. On this machine it decides Chinese -- 直骨今 laid out at
  73.51, matching PingFang SC, where Hiragino Sans gives 72.00. A Japanese document got Chinese
  letterforms and nothing said so. With `locale` plumbed, four tags give four answers where there
  was one: none 73.51, `ja` 72.00, `zh-Hans` 73.51, `ko` 62.28.

  - Naming a font family answers the same question and is not a substitute: it gives up fallback
    for every codepoint that family lacks, which is most of them for a CJK face used incidentally.

- **A style key the parser does not read is named, in strict mode.** A style object is read key by
  key, so an unknown key is not an error -- it is an absence. `{ locale: "ja" }` built a paragraph,
  laid it out and changed nothing for as long as `locale` went unread, and a misspelled `fontsize`
  leaves the size at its default with nothing to see. That is how the locale gap stayed invisible.
  Tolerant by default, as the Canvas API is about values it does not recognise; loud under
  `SKIA_CANVAS_STRICT`.

  - The `⚠️` strict-only marker was the obvious way to express that and is the wrong one here. It
    lets the Rust side abort and the JavaScript side swallow the message, which suits a setter --
    nothing was set and the call owed no result. A constructor with a swallowed error returns
    nothing, so `ParagraphBuilder.Make` handed back `undefined` and the failure surfaced two calls
    later as a failed downcast in `addText`. The flag is read on the Rust side instead, so the
    tolerant path never throws at all rather than throwing and being silenced.

### Added

- **`strokeWidth` outlines the glyphs instead of filling them** -- what CSS spells
  `-webkit-text-stroke`. Skia's text style carries a paint and both doors carried only a colour, so
  a run could be filled and nothing else.

  - Not positive is ignored rather than refused, matching `lineWidth`, which this tree already
    ignores when it is not positive because a browser does. Skia reads a zero width as a hairline --
    one pixel however the run is transformed -- which is a real thing to want and not what zero
    means from a caller computing a width from a scale.
  - One paint draws one way, so a run is filled or stroked and never both. Both is two paragraphs
    drawn in the order the caller chooses, which is what makes `paint-order` expressible.

### Notes

- **Hit-testing, bidi and grapheme clusters were already right, and nothing said so.** All three
  lay out and paint correctly, so a wrong answer would have been invisible to every rendering
  assertion in the suite -- a caret inside a joined emoji looks exactly like a correct one until
  someone clicks. Checked before writing anything: a Hebrew run reports right-to-left and
  hit-tests from the right; a selection crossing from Latin into Hebrew returns two rectangles
  carrying different directions, which is the case a naive implementation collapses into one; a
  family emoji built from three code points and two joiners selects as one rectangle and
  hit-tests to its boundaries. Now guarded, on both doors.

  - The assertions are on structure -- direction, run count, cluster boundaries, monotonicity --
    and never on coordinates. Bidi and cluster segmentation come from ICU and hold wherever this
    runs; glyph widths belong to whichever font the host resolves, and asserting those would fail
    on a machine with different fonts rather than on a regression. Every assertion was inverted
    and confirmed to fail before being left in.
  - The crate test pins a difference between the doors that is easy to trip over: its
    `TextPosition::index` counts UTF-8 bytes where the binding reports UTF-16 code units.

- **The probe that refuses the GPU for float canvases named the wrong mechanism.** It blamed the
  paint colour slot and predicted that sixty faint layers land on 60/255. Both are checkable and
  both are false -- 60/255 is 0.23529 and nothing measures it, while a GPU F16 surface reads
  0.37695 where the arithmetic gives 0.30308. Each draw's source colour is rounded to eight bits
  before blending, whatever the surface is made of; feeding an alpha eight bits can hold exactly
  makes the GPU and the CPU agree again. A colour arriving through a shader rounds identically, so
  no runtime effect escapes it. The refusal is right by a wider margin than the old comment
  claimed: float on the GPU is further from the truth than eight-bit storage on either backend, at
  twice the memory.

- **A timing test of ours had no business passing reliably.** The clipped-nested-draw ratio timed
  one pass, and `cargo test` runs in parallel, so a loaded machine stretched the cheap leg four
  times further than the expensive one and broke the very ratio being asserted -- 5.9ms against
  34.6 in a full run, passing alone three times out of three. It takes the fastest of five passes
  now, since load only ever adds time.

## 📦 ⟩ [v5.6.6] (npm) / [v0.10.6] (crate) ⟩ August 21, 2026

One correctness fix. A canvas handed to another canvas as a source went through an eight-bit sRGB
image whatever the canvas was made with, so a wide or float source lost what made it wide or float
before the destination ever saw it.

### Fixed

- **A canvas source keeps its own gamut and depth now.** The picture behind a source canvas was
  given to Skia as a deferred image fixed at eight bits and sRGB, and that image is what the
  picture is replayed into when it is finally drawn. So a `display-p3` canvas drawn into a
  `display-p3` canvas went out through sRGB and came back: P3 red read `[234, 51, 35]` where the
  source held `[255, 0, 0]` -- sRGB red converted up, with every colour the smaller gamut cannot
  name already gone. An `RGBAF32` canvas came back on the 1/255 grid, an alpha of 0.002 reading
  0.003922 and 0.5 reading 0.501961. The image now carries the canvas's own space and the deepest
  format the deferred-image API offers.

  - The nested path narrowed in two places, and fixing the first left the second. A draw that can
    show most of its source flattens the whole page; one behind a small clip rasterizes only the
    visible region, through a surface fixed at N32. That second surface was invisible while every
    source was eight-bit sRGB and became a narrower copy of the same defect the moment sources
    carried their canvas's format -- a clipped P3 draw still read `[234, 51, 35]` after the first
    fix. It takes the source's own format now.
  - `drawCanvas` never had the problem, because it replays the picture onto the destination
    rather than going through an image. That is what makes this visible from JavaScript without
    reading pixels twice: the two entry points disagreed about the same drawing, and the tests
    assert against each other.
  - `BitDepth` names only U8 and F16, so an `RGBAF32` canvas is carried at F16 rather than F32.
    Its eleven-bit significand and its exponent put eight levels between two eight-bit steps near
    1.0 and about two thousand near 0.002, which is where a canvas that accumulates low alpha
    needs them, and unlike U8 it holds values outside `[0, 1]`. F32 end to end would need a
    different image API than the one a deferred picture has.
  - Both surfaces, `drawImage` and the crate's `draw_canvas`. Each is tested, and each test was
    mutated back to the old hardcode to prove it notices -- both then read `[234, 51, 35, 255]`,
    which is the signature of the round trip.
  - Unaffected: an ordinary sRGB eight-bit canvas, which asks for exactly what it asked for
    before, and every source that is not a canvas.

### Faster

- **A clipped draw of a nested source costs the region now, not the whole page.** Rasterizing the
  visible region drew the source's deferred image into a region-sized surface, and Skia answers
  that by materializing the whole page and copying the sliver out -- `SkBitmapDevice::drawImageRect`
  calls `getROPixels`, which has no notion of a source rectangle. So every op in the source ran
  however little of it showed. Replaying the picture into that surface instead lets Skia cull
  against its bounds. On a 1400-square source behind a 180x24 clip, per draw:

  | ops in the source | cpu v5.6.5 | cpu v5.6.6 | gpu v5.6.5 | gpu v5.6.6 |
  | ----------------- | ---------: | ---------: | ---------: | ---------: |
  | 40                |     0.36ms |     0.07ms |     1.14ms |     0.48ms |
  | 2,000             |     4.84ms |     0.05ms |     5.43ms |     0.39ms |
  | 20,000            |    46.25ms |     0.07ms |    46.99ms |     0.53ms |

  Release binary against release binary rather than two paths behind a switch, so the left column
  of each pair is what a caller on v5.6.5 gets. Sub-linear where it had been linear, on both
  engines: a hundredfold heavier source costs about three times more rather than a hundred and
  thirty times. The binding's own per-call cost hides the rest of that -- through the crate, where
  nothing sits in the way, the same two sources measure 14.7 and 43.8 microseconds, and in
  JavaScript both round into the same tenth of a millisecond. Replaying still walks the picture
  and culls it, and that walk is what remains proportional to the source.

  The region is rasterized on a raster surface whichever engine the canvas uses, so the defect and
  the fix are the same on both. The gpu columns sit higher because each round ends in a read, and
  reading a gpu surface flushes and waits for the device -- about 146 microseconds of sync that
  has nothing to do with the draw.

  Both doors again, and both tested by ratio rather than duration so the machine cancels out --
  each mutated back to prove it notices, which took the crate's from 1.5ms to 963.7ms. Both
  benchmarks carry the row as well, so a return of this is visible without reading a test.

  - Byte-identical output, not merely similar: sha256 over the whole page matches on a plain
    draw, a rotation, a scale and a blur. The blur is the one that matters, because it reads
    outside the region it writes.
  - No memory change. Sixty draws grew the process by the same 84 MB either way -- this buys
    time and not footprint, which is the opposite of what the region work in v5.6.5 bought.
  - `Context2D::get_picture` takes `&self` now. A source is resolved while its destination is
    already borrowed, and the two are the same object when a canvas is drawn into itself, so
    asking for `&mut` there would have turned a working self-draw into a panic. The mutation is
    the recorder's own `RefCell`, as it already was for `get_source_image`.

### Notes

- **v5.6.5 said only the visible part of a nested source is rasterized, and that was half true.**
  Only the visible part is _kept_ -- which is what the 492 MB to 43 MB reading measured, and that
  reading stands. Every op in the source still ran on every clipped draw, because drawing a
  deferred image asks Skia for the whole page and takes the sliver from the result. The entry
  measured memory, memory was the half that was fixed, and it says in as many words that the
  clock cannot see this. Time against source complexity is the axis the remaining cost lived on
  and it was never run. An instrument that cannot see a defect is not evidence there is none.

- **One claim from v5.6.5 is still unexplained rather than wrong.** It records Skia serving a
  repeated rasterization from its own cache -- sixty whole-page flattens in 0.26 seconds against
  0.24. The region path does not behave that way: sixty draws of one source cost 0.68ms each at
  forty ops and 45.26 at twenty thousand, which is no cache at all. Both may hold, since a
  whole-page flatten and a region blit reach Skia differently, but the reason the second is not
  amortized is not established here.

- **The same defect was found and fixed on the compositing surface, and not looked for here.**
  `ExportOptions::compositing_color_type` carries the note that an `F32` canvas composited at
  eight bits read an alpha of 0.002 back as 1/255 -- the same sentence this entry needed, written
  for the surface a page draws into rather than for the image a page becomes. One layer was
  checked and the other was not.

- **`CanvasOptions::color_type` documented the opposite of what it does.** It said compositing is
  eight bits per channel whatever the field says, which is what the compositing fix above had
  already made untrue. Corrected, and it now also records that the field fixes the depth a canvas
  carries as a source.

## 📦 ⟩ [v5.6.5] (npm) / [v0.10.5] (crate) ⟩ August 20, 2026

One performance fix, following v5.6.4's. A canvas carrying a nested picture is rasterized before
it is drawn, and was rasterized whole however little of it the draw could show.

### Lighter

- **Only the visible part of a nested source is rasterized now.** Sixty draws of a 1400-square
  source through a 180×24 clip grew resident memory by 492 MB and took 185 milliseconds. They now
  grow it by 43 and take 137 -- eleven times less memory and a quarter off the time.

  - The clip alone is not the region, which is what makes this more than an intersection. A
    filter reads outside the pixels it writes, so cropping the source to the visible rectangle
    starves a blur at its edges and leaves a seam. Skia's `filterBounds`, run backwards, asks the
    filter how far it reaches -- the only source of that number that stays right when the filter
    changes. Both rectangles move with the subset, so the pixels land where they would have
    landed.
  - Measured against ten drawings rendered both ways: nine identical to the byte, and a rotated
    clip differing at two pixels of 160,000 by one level in one channel, which is what resampling
    a subset through a rotation costs.
  - Unaffected: a source with nothing nested in it, which is still handed over as a picture and
    never rasterized, and the copy-and-draw-back loop v5.6.4 fixed, which stays flat.
  - Both surfaces, `drawImage` and the crate's `draw_canvas`. They take a canvas by separate
    doors and the narrowing reached one of them first, which is the second release running that
    the crate has had to be caught up separately. The machinery is shared; the decision to use it
    is made where a source is resolved, and there are two of those.

  Both doors are tested, each on the axis that moves. The clock cannot see this -- Skia serves a
  repeated rasterization from its own cache, so sixty whole-page flattens run in 0.26 seconds
  against 0.24 while holding an order of magnitude more -- so both tests assert what the process
  holds, and both were mutated back to the old behaviour to prove they notice: 492 MB from
  JavaScript, 474 MB from the crate.

### Notes

- **This was filed as not worth doing, and that was wrong.** The first measurement used v5.6.4's
  copy-and-draw-back loop, where the rasterization that dominates is a page copied whole into a
  fresh canvas -- no clip, and so nothing to narrow. The saving read as zero and the work was
  shelved. That workload cannot show this effect; the one above can, and does, by a factor of
  eleven. A measurement only answers for the workload it runs.

## 📦 ⟩ [v5.6.4] (npm) / [v0.10.4] (crate) ⟩ August 20, 2026

One performance fix, in four places. Drawing a canvas into another canvas cost about twice as
much each time it was repeated, on the raster backend, through every door that takes a canvas.
Output is unchanged -- the same pixels, including a magnified source, which was measured rather
than assumed -- and neither API gains or loses anything.

### Fixed

- **A canvas drawn into a canvas compounded.** Copying a page into a fresh canvas and drawing
  that copy back, round after round, doubled the work of the eventual rasterization each round:
  ten rounds took 0.94 seconds, eleven 1.78 and twelve 3.54, where the default surface stayed at
  0.06 throughout. Twelve rounds now take 0.09 and sixteen 0.08.

  - The copies themselves were free and the whole cost landed at rasterization, which is the
    shape of the cause. A canvas handed to `drawImage` answers with an image backed by its
    picture rather than with its pixels, so the source's whole recording travels with it. A
    picture reached by two paths is replayed twice while being recorded once -- the recording
    grows by a constant while the replay doubles.
  - The size of the recording is therefore no use as a signal. Skia's own nested operation count
    reads 4, 13, 22, 31 and 40 across the rounds whose time is doubling.
  - So a page now counts what replaying it would cost someone else, and a canvas that already
    carries a nested picture is rasterized when it is drawn rather than nested again. One level
    of nesting, and no more.
  - A page that has only been drawn on is still handed over as a picture, so an ordinary source
    neither rasterizes nor loses the vector form a backend can see through: two thousand draws of
    a small canvas measure 0.029 seconds.

- **The same through `createPattern`, and through both of the Rust API's own doors.** A canvas
  reaches a drawing by four separate routes, and the first fix covered one. A page painted
  through a pattern of itself doubled the same way -- 47, 122, 378 and 1409 milliseconds at
  eight, twelve, fourteen and sixteen rounds, now 41, 49, 59 and 70. `draw_canvas` and
  `create_pattern` on the Rust API took a canvas without passing through the resolver the first
  fix went into, so the crate kept the whole defect while the npm package no longer had it: a
  test of the same shape took 63 seconds and now takes 0.21.

### Lighter

- **Peak memory halves on the same drawing.** The nested pictures the old path kept were tiny
  while they were being recorded -- 0.05 MB a round against a 5.49 MB surface -- and the whole
  cost arrived when the export replayed them: twenty rounds settled at 450 MB. Flattening pays
  about 7 MB a round as it goes and settles at 228.

  What is left is the page's own content rather than anything cached, which is why neither a
  collection nor the idle watcher returns it: forty rounds put 252 MB into a page whose surface
  is 5.49 MB, and that is forty flattened snapshots recorded into it. There is a further saving
  available and not taken here -- the whole source page is rasterized even where the destination
  clips it to a two-hundred-pixel sliver -- but the clip belongs to the destination and is not
  visible where the source is resolved, so it wants a change of shape rather than a smaller
  number.

### Internal

- **The rule does not ask which backend will draw, and an earlier version of it did.** Flattening
  costs a GPU about 3 milliseconds a nested draw for nothing, since the nested replay is cheap
  there, and skipping it measured 0.06 seconds against 0.10. That gate could not be made to hold.
  The decision happens where the source is resolved, so it read a flag on the context -- and a
  context is built by `getContext` before `canvas.gpu = false` is usually reached, which brought
  the doubling back for anyone who wrote those two lines in that order. Measured at a ratio of
  2.49 per round with the flag set late against 1.01 with it set at construction, and the Rust
  API has the same trap because `set_gpu` after `Canvas::new` is the only way to ask for the CPU
  at all. A blowup that depends on the order two properties are set is worse than the draw it
  avoids.

- **`chunks_exact` with a constant size is now `as_chunks`, at twenty-nine call sites.** Rust
  1.98 added `clippy::chunks_exact_to_as_chunks`, and the clippy jobs track stable, so the lint
  arrived on its own and failed a build that had nothing to do with it. Fourteen of the sites are
  in the AVIF, BMP and GIF encoders and the AVIF decoder, five elsewhere under `src`, and the
  rest in a test or an example -- clippy reads `--all-targets`. None were touched by the fixes
  above.

  The two `par_chunks_exact` calls in the AVIF encoder stay as they are. They are `rayon`'s, not
  the standard library's, the lint does not reach them, and there is no parallel `as_chunks` to
  move them to.

  The change is not only a lint's preference: `as_chunks::<N>()` hands back `&[T; N]` where
  `chunks_exact(N)` hands back a slice that happens to be N long, so the length is in the type
  and an index into it is checked at compile time. Two sites were doing work the array form makes
  unnecessary -- a `try_into` and a `first_chunk` that each recovered an array the iterator can
  now yield directly.

  It costs no support: `as_chunks` stabilized in 1.88, checked against the toolchain rather than
  assumed, and this crate's floor is 1.90.

- **What else was checked, and found clean.** The defect is a recording that composes
  multiplicatively, and `get_picture` has exactly five callers -- the four above and the one that
  makes the deferred image -- so that surface is covered rather than sampled. An SVG export of a
  nested canvas is flat at 31 milliseconds and six kilobytes however deep the nesting goes.
  Ten accumulation patterns were timed per draw as the count rose -- plain fills, alternating
  blend modes, a `clearRect` every draw, save/clip/restore, a distinct filter each draw, a
  distinct transform, a rebuilt gradient, many pages, text runs, interleaved `getImageData` --
  and every one falls rather than rises. So do the heavy effects: a blur from 4 to 128 pixels,
  `saveLayer` nested 32 deep with a blur at each level, filter chains up to 16 links, `shadowBlur`
  across 800 draws, and `drop-shadow` to a 64-pixel radius.

## 📦 ⟩ [v5.6.3] (npm) / [v0.10.3] (crate) ⟩ August 20, 2026

Three fixes to `ctx.filter`. A blur was half as wide on an image; on a shape it blurred the
outline and left the paint inside untouched; and a filter carrying a zero written without its
unit was rejected outright rather than applied. Nothing else changes, and neither API gains or
loses anything.

Figures are device pixels, measured as the width of a blurred edge, and checked against a
browser rather than against the specification alone.

### Fixed

- **A CSS blur was half as wide on anything drawn through an image.** The same filter on the same
  edge gave a blurred band 46 pixels across from a `fillRect` and 43 from a `drawImage` at 3px,
  52 against 46 at 6px, 63 against 52 at 12px. Each image reading equalled the shape reading for
  half the radius, which is what named the cause: the image path used the length as a diameter
  where the shape path used it as a standard deviation.

  - Filter Effects defines `blur(<length>)` as the standard deviation itself, so the shape path
    was right. Halving it is the `box-shadow` convention, where the radius is twice sigma. That
    convention is real and still applies to `shadowBlur`, which is a different property; it had
    no business on the filter path.
  - Scaling by the canvas transform is unchanged. Both paths mean to end with a blur measured in
    device pixels, so a fix that removed the transform along with the halving would have swapped
    one wrong answer for another under `scale()`.

  Affected: `drawImage` in both its short and nine-argument forms. Not affected, checked rather
  than assumed: `ImageFilter.MakeBlur`, which takes a standard deviation from its caller and
  passes it through.

- **A blur softened a shape's outline and left the paint inside it alone.** A pattern of hard
  stripes under `blur(6px)` came back byte-identical to no blur, every stripe edge razor sharp,
  and a gradient's hard stop stayed a step at any radius. A fill whose shader painted nothing
  outside its own rectangle had nothing to spread into and kept a hard edge: 40 pixels across at
  `blur(12px)` and still 40 at `blur(30px)`, against 62 for the same shape filled with a colour.

  - What named the cause is that `repeat-x` was right and `repeat-y` wrong, on the same source
    and the same fill. The blur reached exactly as far as the shader painted, and no further.
  - A blur on a shape was built as a coverage blur, which softens the outline rather than what is
    drawn. That is the same picture only while the paint is one flat colour -- blurred coverage
    times a constant is that constant times blurred coverage, so the colour factors out and the
    two agree exactly. It stops being true the moment the paint varies with position, which a
    pattern, a gradient, a texture and a noise shader all do.
  - So the choice is now made from the paint rather than from the kind of draw. A flat colour
    keeps the coverage blur; anything carrying a shader takes the same path an image already
    took. The boundary is where the equivalence actually holds.

  Sending every draw down the image path is also correct and was measured and rejected: a
  blurred rectangle went 93.6 microseconds to 167.9, a blurred arc 90.9 to 199.5 and blurred text
  67.7 to 284.2. Keeping the flat-colour path leaves those at 81.1, 83.5 and 62.6 -- at or inside
  the noise of where they were -- and charges only the draws that were wrong. Those do pay: a
  blurred pattern fill is 290.8 against a blurred solid's 102.6 in the same run, and there is no
  cheaper correct construction, because blurring painted content requires the content.

  Export fidelity is unaffected either way. The SVG backend already rasterized a draw carrying
  either kind of filter, and the PDF backend objects to neither.

- **A zero written without its unit threw the whole declaration away.** Setting
  `ctx.filter = "drop-shadow(20px 0 0 #f00)"` left the property reading back `"none"`, so nothing
  was drawn and nothing said why. In a chain the affected function vanished while its neighbour
  stood, and `blur(3px) drop-shadow(20px 0 0 #f00)` became `blur(3px)`.

  - A zero length may be written without a unit, and only a zero may. The grammar here required
    one, so each bare `0` failed to parse; a drop shadow needs two lengths or three, got one,
    was retried with the arguments reversed, failed again, and was discarded.
  - Not specific to shadows, though that is where it shows: the same grammar governs `blur()`,
    which took `blur(0px)` and refused `blur(0)`, and the angle beside it refused
    `hue-rotate(0)`. An offset shadow with no blur is normally written `20px 0 0`, which is what
    made this look like `drop-shadow` being unimplemented rather than one token being refused.
  - Checked against a browser in both directions, because the fix is only right if it stops where
    the browser stops. Now taken, as there: `blur(0)`, `hue-rotate(0)`, `drop-shadow(0 0 6px red)`,
    and zero however it is spelled -- `+0`, `-0`, `0.0`. Still refused, as there: `blur(5)`,
    `hue-rotate(45)`, `drop-shadow(20 0 red)`.

## 📦 ⟩ [v5.6.2] (npm) / [v0.10.2] (crate) ⟩ August 20, 2026

One rendering fix. A clip set inside `saveLayer()` was applied under the current transform a
second time, so it landed at its user coordinates times the scale _squared_.

### Fixed

- **A clip inside a layer landed at the wrong size under any transform but the identity.** Under
  `scale(2)` a clip meant to end at device pixel 100 ended at 200; under `scale(3)`, 450; under
  `scale(4)`, 800. Drawing coordinates inside the layer were unaffected — only the clip — and at
  `scale(1)` the wrong answer and the right one are the same number, which is why it survived:
  every existing test of layers and of clipping was written at the default transform.

  - It presents as images disappearing rather than as a clip being the wrong size. A doubled clip
    anchored at the origin only grows outward, so it keeps whatever sits at 0,0 and drops
    whatever was laid out further along. A caller drawing each image inside its own
    `save`/`clip`/`restore` sees one image in a group render at `scale: 2` and the rest not.
  - A clip is held in device space — it is transformed by the CTM when it is set — so whatever
    applies it later has to do so under an identity matrix. Opening a layer recorded its floor
    while the layer's own matrix was still in effect, and every rebuild inside the layer then
    transformed that device-space clip again.
  - Fixed at the point the floor is recorded rather than at the point it is read, because it has
    two readers: an inherited clip is applied at the same depth, so a layer nested inside another
    layer was wrong for a second reason and a fix at the reading end would have left that one
    standing.

### Tests

- **Nine pixel-asserted cases for clipping inside a layer**, six of which fail against v5.6.1: a
  clip in a layer at 1x, 2x and 3x; the same clip outside a layer at each, which is the path an
  ordinary `clip()` takes and guards against a fix that merely moves where the transform is
  applied; nested layers with a clip apiece; a clip inherited from before the layer;
  `saveLayer()` with an explicit bounds rect; `scale(2, 3)`, which separates the fix from one
  that only ever saw square matrices; and a quarter turn, compared by inked area because a
  rotated clip has no axis-aligned edge to measure.

  The 1x cases and the inherited-clip case pass before the fix and are kept anyway. 1x is the
  transform that hid this for the life of the code, and the inherited clip is applied at the
  outer floor, which was already identity.

## 📦 ⟩ [v5.6.1] (npm) / [v0.10.1] (crate) ⟩ August 20, 2026

A GPU export holds fewer contexts, and gives back the ones it does hold. Nothing here changes a
byte of output or a line of either API — the files a canvas writes are identical to v5.6.0's —
which is what makes this a patch rather than a minor.

Figures are release builds exporting twenty-four 800×600 pages as PNG, measured on both
backends: an M-series Mac on Metal, reporting physical footprint, and a twelve-core Linux box
with a GTX 1050 Ti on Vulkan, reporting resident memory. Idle figures are sampled nine seconds
after the last export. Each baseline is a build of the commit before the change it is compared
against.

### Lighter

- **A sequential export no longer wakes every GPU owner.** Exports were dealt to the four owner
  threads in turn, so four calls awaited one after another — no two of them ever overlapping —
  still reached all four, and each built its own Skia `DirectContext` and resource cache on the
  way. A job now goes to the owner with least in flight, ties to the lowest index, so a caller
  whose exports never overlap stays on one.

  - Metal: four contexts and 159.5, 159.8, 159.8 MB before; one context and 102.1, 89.4,
    102.4 MB after. About 19 MB a context, the same order as the 22 to 25 MB the per-worker
    arrangement in v5.6.0 cost.
  - Vulkan: 363.2, 363.1, 363.0 MB before against 332.4, 332.7, 332.5 after, so about 10 MB a
    context on that device. Same direction, smaller contexts.
  - Concurrent exports are untouched, by construction: under real overlap every queue is busy
    and the choice is the one dealing in turn would have made. On Metal, 117, 84, 89 ms before
    against 102, 84, 83 after; on Vulkan 441, 438, 438 against 439, 438, 438. Still four owners
    on both.

- **An idle owner now gives its context back.** Both backends reap an idle context by
  `rayon::spawn_broadcast`, which reaches every worker in the pool and no owner — an owner is a
  plain spawned thread, and a broadcast cannot touch a thread-local it does not run on. So the
  five-second lifespan both backends document never applied to the threads that actually hold a
  context during an export, and instrumenting the Metal constructor showed four contexts built
  and none released, eight seconds past that lifespan. Held until the process ended.

  Each backend keeps the answer it already had, because they do not agree and the disagreement
  is deliberate: Metal drops the context, Vulkan frees its resources and keeps it, since
  dropping would release its queue while texture-backed images it has handed out can still
  reach it. That difference is most of the difference in what the two get back.

  - Metal, sequential: 102.2, 104.5, 102.4 MB before against 90.7, 90.4, 90.5 after.
  - Metal, concurrent — the case with four contexts to drop: 182.8, 178.5, 182.8 MB before
    against 133.9, 139.1, 136.3 after. Roughly 45 MB.
  - Vulkan, sequential: 304.5, 306.3, 306.2 MB before against 284.6, 284.5, 284.6 after.
  - Vulkan, concurrent: 320.4, 322.7, 323.0 MB before against 315.8, 318.9, 322.1 after, which
    is inside the run-to-run spread. Keeping the context is the point rather than a shortfall,
    so the concurrent 45 MB above is a Metal figure and not a claim about both.
  - Nothing moves while work is arriving — the check runs only after a second with no job — so
    busy memory is unchanged on both and so is throughput.

### Internal

- **The PNG row-filter probe is now scored against real drawings.** Every number it is tuned by
  — two bands of 48 rows, a threshold of one, deflate level six — was measured once and written
  down in prose, and nothing checked any of them; the one test there was asserted the direction
  of the answer on two generated images, which a probe with its band length cut to a twelfth
  still passes. That gap is not theoretical: the threshold was already "corrected" from one to
  0.8 once and had to be put back, and no test failed either time.

  - Ten drawings — a photograph, panels, a striped table, a bar chart, body text in a real face,
    a report page, gradients along two axes, a flat fill — are encoded both ways, the smaller
    taken as truth, and the probe scored on how far over it lands. It is exact on nine and 5.9%
    over on the tenth, against a 10% ceiling.
  - Scored on cost rather than on which setting it picked, because those are different questions
    and only one matters: the flat fill encodes to 6416 bytes filtered and 6417 unfiltered, a
    disagreement worth nothing.
  - Each of the four constants was then broken in turn and the test watched to fail — always
    filtering costs 41.2% on text, never filtering up to 13106% across five drawings, the old
    0.8 threshold 18.6% on the photograph, and a four-row band 198.4% on the diagonal gradient.

- **Three cheaper-looking replacements for that probe are measured and rejected**, in a comment
  beside it, because each looks obviously right until it is timed. Encoding both ways and
  keeping the smaller is exact and 4 to 9 times more expensive. Probing with Skia's encoder
  rather than `flate2` reads like free speed — Skia links a SIMD zlib about 4 times faster per
  byte — and comes to 7.9 ms against the 8.25 the probe already costs, because the probe takes
  one cheap row difference where the encoder tries five filters a row. Sampling fewer rows is
  the only one that saves anything, and it saves under 1% of a 150-page export. What the probe
  costs is also recorded for the first time: 11 to 48% of the single encode that follows it.

- **Two page-cache tests no longer clear each other's entries.** One failed about once in
  fifteen runs of the Rust suite on an assertion that an ordinary export leaves its bitmap
  cached. It does; a sibling test calling `release_cached_pages` — which empties every entry,
  as the idle watcher wants — was taking it out from another thread. Two failures in thirty runs
  racing, none in thirty taking turns.

### Known, and not fixed here

- **The idle watcher still cannot see an owner.** Owners now release their own contexts, which
  covers the case that matters, but the watcher's broadcast reaches only `rayon` workers. Any
  future context held by a non-`rayon` thread has the same blind spot.

## 📦 ⟩ [v5.6.0] (npm) / [v0.10.0] (crate) ⟩ August 19, 2026

Drawing calls no longer cross into Rust one at a time, a GPU export no longer allocates a Skia
context per `rayon` worker, and a page is no longer rebuilt to erase what is on it. Those three
are most of what is below, and the rest is what looking closely at each of them turned up.
Nothing was added to either API and nothing was removed; PNG files change size, which is what
makes this a minor.

One correctness fix is worth reading before the speed: a blend mode — including a partial
`clearRect` — did not survive an SVG export. See Fixed.

Figures are release builds on an M-series Mac, measured by recording and exporting 150 frames of
`examples/node/animated-eye.js` at 640×500 as a PNG sequence, and separately by exporting
canvases that are redrawn between exports — the shape a server has. Each baseline is a build of
the commit before the change it is compared against, so the two differ only in that.

### Faster

- **Drawing calls are recorded and handed to Rust in batches, rather than crossing one at a
  time.** A `lineTo` inside a path that then gets stroked cost 97 nanoseconds, of which the
  drawing — appending a line segment — was a few. Decomposed against an isolated call, which put
  it at 82: 17 on the crossing itself, 39 on reading two numbers out of the arguments, 20 on
  unboxing the receiver, 6 on the JavaScript wrapper. A frame of `examples/node/animated-eye.js`
  makes 12,549 operations — about 6500 drawing calls, 4800 property writes, and 1159 path effects
  that answer with a path and so cannot be batched at all.

  Verbs are now written into a buffer and handed over in one crossing when something needs an
  answer. Both trees built for release, one harness, same machine, median of seven, every figure
  counting the flush the batch ends with:

      lineTo                              97 ns -> 31
      the 48-segment path it belongs to  4713    -> 1481
      stroke(path)                        306    -> 121
      fill(path)                          463    -> 197
      an arc built and filled            1025    -> 556
      an ellipse                          989    -> 558
      fillRect in a colour-setting loop   350    -> 213
      a 100,000-point polyline           9.32 ms -> 2.79

  Recording 150 frames of the animated eye is 656 milliseconds against 817 — that one is
  everything in this section together rather than this entry alone, and it is the only figure
  here that is.

  What carries the design:

  - **One declaration per verb.** Its name, its arguments and their rules, and the code that
    applies it are written once in Rust, and that generates the entry point a direct call
    reaches, the arm that applies a decoded record, the table JavaScript builds its writers
    from, and the row in the test that makes both paths prove they draw the same thing.
    Seventy-six verbs and property writes are declared; nothing lists them twice.
  - **The handle is the flush.** The buffer goes over when JavaScript asks for something only
    Rust can answer, and the boxed handle every path into Rust goes through is an accessor that
    drains first — so a call that cannot be recorded still lands in order, and no future getter
    can forget to flush.
  - **A lane beside the buffer** carries what is not a number, so a colour, a `Path2D`, a dash
    pattern or an image can be recorded rather than forcing a crossing.
  - **Writers are generated when a verb is installed**, rather than interpreting the schema per
    call, which is worth about a third of what recording one costs.

  Drawing an image and drawing text are recorded too, and neither for the reason the numbers
  suggest. Laying out a text run is 2130 nanoseconds against the 82 an isolated crossing costs,
  so batching a `fillText` saves 3% of it — but a call that crosses hands over everything queued
  behind it, and a drawing that labels what it draws was ending a batch on every label:

      a bar and its label                2846 ns -> 2515
      drawImage with a source rect        325    -> 225
      a frame-shaped loop of five verbs   850    -> 544

  The same effect is what makes carrying a string worth it at all: recorded on its own, a
  `lineCap` write is 109 nanoseconds against 95 crossing, and it is the four numeric verbs either
  side of it — 406 against 624 — that pay for the lane.

  **Nothing about the API moved.** Bad arguments are answered as they were in every case but the
  two under Changed below, and the two under Fixed that this work broke and this release repairs
  — measured, not asserted: 3700 ways of calling the API wrongly, against a build of the commit
  before any of it. `tests/suite/arguments.test.js` was written before this started to pin those
  answers, and `tests/suite/boundary.test.js` generates itself from the published table, so a
  verb declared without a sample value to test it with fails rather than goes uncovered.

  **Still crossing one call at a time**, each for a reason:

  - `drawCanvas` wants its source as a picture where `drawImage` wants pixels, and a slot
    resolves what it was handed without being told which; it composites a page where the others
    place a sprite.
  - `putImageData`, and `drawImage` of an `ImageData`, carry pixels that are a JavaScript array —
    a caller can change them without crossing anything that would hand a pending batch over
    first.
  - `font`, `filter`, `letterSpacing`, `wordSpacing`, `textDecoration`, `fontVariant`,
    `fontVariationSettings` and `currentTransform` cross a parsed object rather than a string.
  - `colorFilter` and the two Skia filters cross a boxed handle of a type no slot resolves, and
    `lineDashMarker` takes a `Path2D` or `null`, which a slot has no way to be.

  `font` is the one worth naming: it costs 1503 nanoseconds a write, of which the JavaScript
  parse is 5 — so the boundary is not what is wrong with it.

- **Using a path no longer costs a copy of it.** `Path2D` holds a `PathBuilder`, and the path it
  has built was taken from it afresh every time one was asked for — by a read of `d`, `bounds`
  or `edges`, and by every `fill`, `stroke` or `clip` that names the path. Taking it walks the
  whole builder, so the cost of _using_ a path grew with the path, and a drawing that fills the
  same complex shape every frame paid it every frame.

  It is taken once and kept until the next append now, which the builder being private is what
  makes safe: reaching it goes through `builder_mut`, and that is where the kept copy is
  dropped. Filling a 2000-segment path goes from 4.10 microseconds to 0.20, a 200-segment one
  from 0.58 to 0.19, and reading one from 2.65 to 0.65 — flat against the length of the path
  where all three used to climb with it.

  And the builder itself is now made only when something appends. A path that arrives whole —
  from a path effect, from an SVG string, from a copy of another path — is held as it is, where
  before it was walked to build a builder that most such paths never use: an effect's result
  usually goes straight into a fill. `jitter` 1019 nanoseconds to 862 on a short path and 34.0
  microseconds to 28.0 on a long one, `simplify` 598 to 520, `offset` 606 to 557. Building an
  empty path costs about 45 nanoseconds more for the emptier representation, which is the trade
  and much the smaller half of it.

- **A class's table of Rust functions lives on its prototype, not on every instance.** Every
  object wrapping a Rust handle carried its own `native` — the table of exported functions for its class — defined on
  the instance. It is the same table for every instance of a class, so it belongs on the
  prototype, where it is defined once and found through the chain.

  `new Path2D()` 480 ns to 375, `new Path2D(other)` 670 to 546, `jitter()` 1125 to 1075. It
  shows up wherever a drawing makes paths rather than reusing them: a frame of the animated eye
  builds 1428 of them and is handed 1159 more back from `jitter`.

- **Laying out text no longer searches for the font it was already given.** Every `fillText`,
  `strokeText`, `measureText` and `outlineText` matched the family against the font collection a
  second time, inside the layout, to find the style the matched face reports — which is what
  stops Skia synthesising a bold or an oblique for a family that has neither. The collection had
  just been chosen by the same search, so for any family without a variable font in it the
  answer was already in hand. It comes back with the collection now.

  `fillText` 2.12 microseconds to 1.95, `strokeText` 2.13 to 1.93, `outlineText` 4.67 to 4.42,
  `measureText` 4.29 to 4.17. Nothing about the rendering moves: 320 combinations of family,
  weight, slant, stretch and variation axis — including instanced variable fonts, where the
  match legitimately differs from the one made against the library — render and measure to the
  same bytes.

- **`measureText` builds its answer in JavaScript, from one buffer.** The measurements crossed
  as an object built property by property in Rust — twelve for the metrics, twelve more for each
  line and eleven for each run inside it, about forty in all, and every one of them a call across
  the binding. That was 4.6 microseconds of a 9.4-microsecond `measureText`, against 3.5 for the
  typesetting it reports. A wrapper on the JavaScript side then copied the whole object across
  again to make its properties read-only, for another microsecond.

  The numbers now travel in one `Float64Array` with the family names in an array beside it, and
  the object is assembled in JavaScript, where a property write is a few nanoseconds rather than
  a crossing. The part that was 4.6 microseconds is now 0.52. Collecting the measurement was
  tidied in the same pass — a `Vec` of run indices per line, a collection of line rectangles
  built only to be reduced, a second copy of every run's family name, and a UTF-16 index over
  text that is usually ASCII, where a byte offset already is the index — for a further 4 to 6%.

  Short text goes from 9.37 microseconds to 4.37, a sentence from 10.54 to 5.37. What is left is
  almost all typesetting: the same layout `fillText` does, plus the walk over the glyph runs that
  `lines` and `runs` are made of.

  Nothing about the shape moved. The fields are declared once in Rust, and both the buffer and
  the list the JavaScript reader is built from are generated from that one declaration, so a
  field cannot land in the wrong slot and be reported under another field's name.

- **Setting the font is answered from what the last one resolved to.** `ctx.font = "16px
Helvetica"` cost 1440 nanoseconds, of which parsing the CSS was five — that parse is memoized
  on the JavaScript side and had been for some time. The rest was the boundary: the parsed
  specification crossed as an object and Rust read nine keys off it one at a time, about a
  hundred nanoseconds each, and then asked the font library which typeface the family named.

  The canonical string that the CSS parser already produces names the specification uniquely, so
  it now crosses on its own ahead of the object, and the object is only read the first time a
  name is seen. Measured release-to-release on the same machine: one repeated font 1440
  nanoseconds to 268, alternating between two 1460 to 316, and a label with its font set first —
  the shape a chart's inner loop has — 3971 to 2838.

  A font string never seen before pays 211 nanoseconds more than it did, for the lookup that
  missed and the entry it leaves behind. That is 2% of what naming a new font already costs,
  because a CSS parse that misses its own memo is about eight microseconds; the cache holds 1024
  fonts, matching the memo in front of it, and drops the older half when it fills rather than
  scanning for one victim per insert.

- **A PNG's rows are filtered when filtering makes the file smaller, and not otherwise.** The
  encoder used to filter every page, which is right for a photograph and wrong for a gradient, so
  a few bands of rows are now deflated as they are and again after the Up filter, and filtering
  is asked for only where it wins.

  **The sample took two goes**, and the first one is why this reads the way it does.

  - Sampling pairs of rows spread down the page flatters filtering: two adjacent rows of anything
    smooth differ by almost nothing, so the filtered sample looks tiny.
  - It also hides the opposite case. Deflate finds matches across a whole image, so a page whose
    rows repeat — an interface, a chart — compresses better _unfiltered_ than any two-row sample
    can show. A page of flat blocks probed at 0.24, meaning filtering should shrink it to a
    quarter, and filtering took it from 45 KB to 67.
  - Two bands of forty-eight rows read both cases correctly, picked by measuring rather than
    reasoning: ten 1200×900 pages were encoded both ways to find which answer was actually
    smaller, and every combination of one, two, four and eight bands against sixteen to
    ninety-six rows was scored against that. Several reach the right answer on all ten; this one
    does it across the widest span of thresholds.
  - The threshold is one — filter when filtering is smaller. It used to be 0.8, which was
    compensation for the short sample, and a sample that holds what deflate exploits does not
    need a handicap.

  **The deflate level is pinned at 6 rather than probed for.** It was probed for, by compressing
  the winning sample again at level 4 and taking the cheaper one where the deeper earned little.
  That cannot work from a sample: deflate's deeper search pays off over a whole image, and a few
  bands of rows are far too small to show it. On a diagonal gradient the sample put level 4 at
  5.3% more bytes and the page came out at 128% more — 91 KB where the same pixels fit in 40, to
  save 0.9 ms.

  - What pinning costs depends on how much there is to compress. On a page that writes a
    megabyte — the mixed scene `just bench` draws — level 6 is 47.3 ms and 1071 KB against level
    4's 37.2 and 1090: 27% more time for 1.7% fewer bytes. On the ten pages it ranges from
    nothing to about 6%.
  - It is still the answer, and not as a trade: level 4 is not uniformly the faster one either.
    On a diagonal gradient it is 105% slower _and_ 4.2× larger — 178.6 KB against 42.9 — so there
    is no page for which it is the answer, and the alternative to pinning was never level 4
    everywhere but a sample that cannot tell the two apart.

  **What the two together are worth**, on those ten pages: 40.2, 6.8, 6.5, 157.1, 45.1, 233,
  761.1, 6.4, 6.3 and 9 KB, and every one is the smaller of the two answers available. Before,
  two of them were not — the gradient by 2.3×.

  **What the probe costs**: about two milliseconds, and its answer is shared by the pages of one
  export rather than found again for each, with a fresh look every sixteenth page so a sequence
  whose pages are not all the same kind of drawing is never far behind its own content. A cheaper
  one was looked for and does not exist:

  - At deflate levels 1, 2 and 4 the sample misreads the flat blocks, because the long-range
    matching that makes unfiltered win only appears at the level the encoder will use.
  - Sampling a narrower window halves the cost and keeps all ten answers, but a centred window
    can miss the drawing — on the text page it lands in the margin and probes exactly 1.000 — so
    the full row width stays.

  **No pixels change.** PNG is lossless and both row filtering and deflate are reversible, which
  is verified rather than assumed: five drawings exported and decoded back to pixels identical to
  what was drawn.

- **Concurrent exports of a canvas that is still being drawn.** A 1200×900 canvas repainted
  between exports and written as PNG, every export in flight at once, median of five passes:

  | in flight |  before |   peak |   after |   peak |
  | --------: | ------: | -----: | ------: | -----: |
  |         1 | 24.8 ms | 115 MB | 21.9 ms | 122 MB |
  |         8 |    3.99 |    307 |    2.91 |    214 |
  |        32 |    2.83 |    417 |    1.92 |    229 |

  A single export in flight is the one case that costs memory rather than saving it: four
  contexts against the one worker that would have built one.

  That is the opposite of what serialising the GPU was expected to cost, and it is the point
  worth keeping: the contexts were never free. Each worker built its own, cold, with its own
  resource cache, and a texture-backed image cannot be handed to a thread whose context did not
  make it — so every cache update downloaded the page first, under a
  `rayon::current_thread_index()` test standing in for "may I share this". A few warm contexts
  and no per-worker allocation are worth more than the parallelism they replace.

  Encoding stays on every core. Only rasterization moved.

  **A few owners, not one.** Bounding the number of contexts is the point; making it one is not.
  Rasterizing the 150 pages is about 1090 ms of work, and a single owner does all of it in
  series, so no amount of encoding behind it finishes the export sooner — the same sequence went
  from 890 ms to 1091 that way, buying its memory with time. Four owners, then, or fewer on a
  machine with fewer cores:

  | owners | 150-frame GPU export |    peak |
  | -----: | -------------------: | ------: |
  |      1 |              1091 ms |  669 MB |
  |      2 |                  543 |     694 |
  |  **4** |              **431** | **744** |
  |      8 |                  536 |     811 |

  Against 890 ms and 909 MB before any of this — faster and lighter, rather than one traded for
  the other. Eight is what says four is the number: past it the contexts contend for one device
  and pay for their own resource caches to do it.

- **A PNG sequence probes its rows once rather than once a frame.** `newPage()` builds a fresh
  recorder with a fresh id, so there is no page identity to cache the answer against; what the
  frames of one export share is the options they were called with, and the answer lives there
  now. Probing every page instead would cost 32 ms across a 150-frame export.

- **Erasing a page no longer rebuilds it.** Clearing a canvas cost 1476 nanoseconds, and the
  same defect sat under `newPage` and `getContext` at 8.8 and 8.7 microseconds. Both halves were
  in what it took to get an empty page.

  A `clearRect` that covers the canvas erased by replacing the whole `PageRecorder`, which
  allocated a `PictureRecorder` and claimed a new cache identity every time. Nothing asks it to
  change size — a resize goes elsewhere — so the recording is now finished and begun again on
  the recorder already in hand, which is how a flush has always reused one. The page identity
  still changes, and reusing it was a bug worth naming: exports run concurrently and each holds
  the layers it was handed, so with one key between two generations an export in flight was
  served the bitmap cached for the frame that replaced it. A generation that recorded something
  takes a new id; one that did not keeps its own, which is the case `getContext` and `newPage`
  hit when they clear a recorder built moments earlier.

  The other half was the page cache. Registering a page walked every entry to decide whether to
  evict, and a fresh entry holds no bitmap, so it cannot put the byte budget over — only the
  count, and only by one. Eviction itself re-read the map after each removal, so being one entry
  over cost two full passes; it now takes one pass, sorts what it found, and carries the totals
  down the removals.

      clearRect, whole canvas   1476.0 ns -> 330.6
      newPage                      8781  -> 6842
      getContext                   8731  -> 6718

- **A run of erasing draws shares one layer.** A `clearRect` that does _not_ cover the canvas is
  kept in a layer of its own, because `Clear` is a blend mode and the SVG backend writes none of
  them, so the draw has to be available to rasterize rather than emitted as a vector that would
  come out wrong. What was not intended is that a run of them became a layer each, with nothing
  coalescing them afterwards: a drawing that clears a region every frame and never starts a new
  page grew without bound.

  Consecutive draws asking for the same features now share one layer, which closes on whatever
  would make sharing wrong — an ordinary draw, a clip or matrix change, a different set of
  features, or the flush every export goes through.

      partial clearRect           560.6 ns -> 27.1
      a hundred thousand of them   114.7 MB -> 18.0

  The memory figure is a resident-set delta rather than a count of anything, so read it as the
  order it is: what grew with every call now grows with the page.

- **A transform no longer rebuilds the recording canvas.** `translate`, `rotate` and `scale`
  each tore the canvas down and built it again — restoring to the base depth, saving, re-applying
  the clip path, setting the matrix — once per call. None of those intermediate frames can be
  observed: a transform means something to a draw made under it and to nothing else. The canvas
  is now brought up to date before a draw instead, so a run of twenty transforms with no draw
  between them rebuilds once.

      translate                68.4 ns -> 7.9
      scale                    74.4    -> 7.8
      rotate                   72.1    -> 14.8
      save/restore            344.5    -> 282.2
      clearRect, whole canvas 330.6    -> 96.4

  That last one is the entry above carried the rest of the way: erasing a page in place took it
  from 1476 nanoseconds to 331, and not rebuilding the canvas afterwards takes it to 96.

- **A filter string is parsed once.** Setting `ctx.filter` cost 4175 nanoseconds, and the
  suspicion was the boundary, because it is one of the properties that still crosses a parsed
  object one call at a time. It was not: the crossing is 573 ns and reading the font the `em`
  units resolve against is 80. `css.filter` itself was 3226 of the 4175, re-parsing the same
  string on every write. It is memoized now, on the same terms the font parse already was, with
  the `em` size in the key because it is part of the answer.

      css.filter("blur(2px)", 16)   3226.4 ns -> 2.6
      ctx.filter = "blur(2px)"      4175.5    -> 747.6

- **One read is one readback.** `getImageData` over a whole 400×300 page cost 601 microseconds
  where a 32×32 patch of it cost 139, and the gap grew with the area asked for. It is not area:
  `Surface::read_pixels` costs about 430 microseconds per call and almost nothing per pixel. A
  read spanning a 2×2 patch of the tile grid made four of those calls where the page surface
  makes one, and a page of exactly four tiles — anything up to 512×512 — served every read from
  the grid, including a read of the whole page.

  A read may now touch one tile before the page serves it instead. The grid still keeps a hit
  test or a sampled pixel from compositing the whole page, which is what it was built for; it
  may no longer split one read into several. How many tiles it _keeps_ is a separate number, and
  conflating the two briefly left it holding one, so a hit test moving between quadrants
  re-composited on every call.

      getImageData, 400×300 full   601.4 us -> 92.3
      512×512, read 300×300         573.8   -> 69.0
      512×512, 32×32 patch          139.3   -> 139.9

  Those compare each change against the commit before it, as everything here does. The net
  against 5.5.1 is a different shape and worth stating on its own, because a reader upgrading
  gets this rather than the rows above: **a read no longer composites the whole page, so what it
  costs stops growing with the canvas.** The first read after drawing, 64×64:

      1200×900     923.8 us -> 315.8
      2400×1800   2869.8    -> 314.6

  Flat, where it used to be proportional to the area. Reading the same unchanged pixels again is
  3.4 microseconds either way — each tile keeps the CPU copy the page surface always kept, so
  nothing was traded for that. What also falls is memory: two hundred 1200×900 canvases, each
  drawn and read twice, held 6274 KB apiece and now hold 996, because only the tiles a read
  touches are ever composited.

- **Raw pixels are read off the surface they were drawn on.** A raw export asked the rasterizer
  for an image and then copied the pixels out of it, which on the GPU means the whole page comes
  back from the device before anything reads it — paying for it twice when the caller only ever
  wanted bytes. Compositing and taking an image away afterwards are now separate, so a raw
  export composites and reads, while an encoder still gets the image it needs.

  The shortcut does not always apply. Converting into a space the page was not drawn in is a
  redraw into a surface of that space rather than a readback, and `read_pixels` converts to
  different bytes, so that case takes the image path — as does a page with something new to
  cache, which has to come back from the device anyway and whose pixels are then read from that
  copy rather than from the surface as well.

      toBufferSync("raw"), 400×300   398.3 us -> 264.8

- **An unrotated arc goes straight into the path.** Building an arc used to snapshot the whole
  path, rotate it into the arc's frame and rotate it back, so adding one grew with the path it
  was added to; that was replaced this release by building the arc in a builder of its own and
  adding it under the rotation, which is flat. Most calls have no rotation to apply — `arc()`
  has no parameter for one — and there the arc can go straight into the path, since `arc_to`
  already continues the current contour with a connecting line.

      Path2D.arc       911.6 ns -> 738.7
      Path2D.ellipse   930.1    -> 770.3

  The quadratic stays fixed, which is the thing this must not undo: one arc appended to a path
  already holding N segments is 175 ns at 0 segments, 127 at 250, 109 at 2000 and 102 at 8000.

- **A transform written as a matrix is recorded rather than crossed.** `ctx.currentTransform =
matrix` cost 719 nanoseconds and `setTransform(matrix)` 740, where the same call written as six
  numbers cost 387 — a matrix argument went over the boundary as an object on every write. Almost
  every matrix a canvas is given is a plain 2D transform, and the recorded verbs already assume
  exactly that, so a `DOMMatrix` whose projective row says nothing is read as six numbers and
  recorded like anything else. A projective or perspective matrix, a matrix-like object, a CSS
  string, an array or a non-finite field still takes the crossing it always took.

  The numeric path was the slower of the two once the object path was fixed, which is backwards.
  It read its arguments with `Array.prototype.every` over `arguments` and then spread them into
  the call — both walk the iterator, and together they cost more than the recorded write they
  were guarding. Read by index instead.

      currentTransform = matrix   719.3 ns -> 35.9
      setTransform(matrix)        740.3    -> 38.2
      setTransform(6 numbers)     387.0    -> 28.6
      transform(6 numbers)        385.4    -> 28.3

  A fast path that accepted a projective matrix would silently drop the projection, so this was
  checked rather than argued: thirty-nine forms — six numbers with a NaN, an infinity and a
  string among them; identity, translated, rotated and compound matrices; one with `m14` set, one
  with `m24`, one with `m44`; a matrix-like object; a CSS string; an array; no arguments; three
  arguments — each through `setTransform`, `transform` and `currentTransform`, compared on the
  error raised, the matrix read back and the pixels drawn. All identical, and a projective matrix
  still renders differently from an identity one.

- **The current transform comes back packed.** Reading `ctx.currentTransform` cost 621
  nanoseconds against 36 to set one, and `getTransform` is the same call. The binding built an
  array of nine and filled it with nine separate property sets, which was 444 of the 621; it is
  one `Float64Array` now, the shape `measureText` already answers in.

      currentTransform get   621.4 ns -> 414.7
      its crossing           444.1    -> 223.4

  What is left is the `DOMMatrix` the getter has to return, and that is a floor rather than an
  oversight — constructing one costs about 160 nanoseconds with no arguments at all.

- **A context builds its page recorder once, at the size it will have.** Making a context built a
  recorder at the default 300×150 and threw it away, because the canvas's real dimensions were
  applied immediately afterwards: two recorder allocations, two recordings begun and two saves,
  for a page nothing had been drawn on. The size is a constructor argument now.

      new Canvas + getContext   7.01 us -> 6.53
      fresh canvas + newPage    7.20    -> 6.68

  A second change to the same path landed later in the release and is much the larger of the two
  — see the page cache's eviction below. Against 5.5.1 the pair come to 11.36 microseconds → 3.55
  and 11.16 → 3.78.

- **A still image is not asked how many frames it holds.** Decoding cost about 0.8 microseconds
  more than it needed to, and the same 0.8 for a 64×64 PNG, a 512×512 one and a JPEG — a
  constant, so not the decode. Constructing an `Image` opened a second Skia codec over bytes this
  crate had just parsed, to be told there was one frame. Only GIF and WebP can answer with more
  than one, because only those two reach that codec; APNG and AVIF animate too but are read by
  this crate's own scanners above it. Everything else is still by construction and now says so
  without opening anything.

      new Image(png 64x64)     2.79 us -> 1.88
      new Image(png 512x512)   3.05    -> 2.14
      new Image(jpeg 64x64)    5.36    -> 4.24

  Frame reporting is unchanged, which is the whole risk: 47 images compared — eight formats as
  stills, four as six-frame animations, three as one-page animations in an animating container,
  an SVG, a RIFF file that is not a WebP, an empty buffer, a garbage buffer, and every
  checked-in fixture — on frame count, delays, dimensions and completeness.

- **Every boxed handle lives in one slot, read through one accessor.** Making a `Path2D` cost
  169 nanoseconds where the Rust call it wraps cost 86, so the JavaScript half roughly doubled
  it — and 55 of those 83 were one line, attaching the handle with `Object.defineProperty` on
  every instance. That is the second of the two such definitions this release removes, and the
  hot one; the first was the class's function table, above. The rest of the chain together came
  to about 28 and was left alone.

  Three arrangements existed for that one job: a per-instance definition for most classes, a
  second with a different descriptor for the filter classes, and a private slot behind a
  prototype accessor for the two classes that record their drawing. The last is the cheap one,
  and it is now what all of them do.

      new Path2D()               356 ns -> 334
      createLinearGradient      1244    -> 1178
      new ColorFilter("luma")    446    -> 258
      new ImageFilter("blur")   1091    -> 924

  Nothing the old descriptor guaranteed is given up, checked across six classes: the handle is
  still not an own property, so a spread cannot carry one out of an object; it is still hidden
  from enumeration; and assigning to it still throws in strict mode. Reading one back costs 3.61
  nanoseconds through the accessor against 3.63 as an own property, so no cost moved from
  construction onto the calls.

- **A batch is handed over without the slot list it does not have.** A flush crossed with four
  arguments, the last being the things a buffer of numbers cannot hold — a string, a path, an
  image. Most batches name none: a run of line segments, a page of property writes, a rectangle.
  That empty array still had to be recognised and walked on arrival, and is now passed only when
  it holds something.

      fresh path, 1 verb     341 ns -> 317
      fresh path, 4 verbs    358    -> 322
      fresh path, 16 verbs   459    -> 438
      fresh path, 64 verbs  1056    -> 1017

  Measured by alternating the two builds three times over rather than measuring one and then the
  other: this machine's absolute figures drift badly under load, and the three-argument build was
  ahead in eleven of the twelve pairs.

- **AVIF divides a page into tiles the encoder can code at once.** It always coded one. The
  code computed how many tiles a page was worth and handed that answer to libaom as a thread
  count alone — `AV1E_SET_TILE_COLUMNS` and `AV1E_SET_TILE_ROWS` were never called — so the
  threads had a single tile between them and nothing to divide. The comment above it credited
  tiles with taking a 1200×900 page from 5.6 seconds to 1.1; the threading was real but that
  was row-level threading, which libaom turns on for itself.

  On that page, at 41.76 dB either way: 240.7 ms untiled, 142.2 at eight tiles, 77.6 at
  thirty-two, for 580.8 KB, 582.0 and 585.6. Through the benchmark it is 237 ms to 90 for a
  page and 1132 to 729 for thirty frames, and `lossless: true` — which was the slowest option
  — goes 286 ms to 92.

  What it costs is 0.8% of the file: tiles are coded independently, so the entropy coder
  restarts at each boundary and prediction cannot cross it. A page is divided along whichever
  side of the _tile_ is longer, so the pieces stay square rather than becoming strips, and
  halving stops before a tile would fall under a thirty-second of a megapixel — which leaves a
  small image whole without needing a special case. A 320×120 strip comes out byte for byte
  what it was.

- **The page cache evicts below its bound rather than exactly to it.** A pass stopped the moment
  the map was inside its bound again, so it removed one entry — and to choose that one it
  collected every entry into a vector, summed the bytes and sorted. Once the map sits at its
  bound, which is where any workload making more than sixty-four pages leaves it, that walk was
  paid on every `new Canvas`, every `getContext`, every `newPage` and every full-canvas clear, to
  retire a single page.

  It was the largest cost left in building a context, and not where it looked. Measured inside
  the binding: reading the argument 34 nanoseconds, boxing the result 280, and `Context2D::new`
  3400 — of which `PageRecorder::new` was 3200 and the paint state, with its paragraph and text
  styles, was 207.

      new Canvas + getContext   6.15 us -> 3.19
      newPage                   6.03    -> 3.54

  A pass now takes the count to three quarters of the bound, so one walk serves sixteen
  insertions. Only the count: a pass the byte budget asked for still stops at the bound, because
  an entry carrying no bitmap frees nothing. The cache holds forty-eight to sixty-four pages
  where it held sixty-four to sixty-five, so it is marginally smaller, and what it memoizes is
  unchanged — a repeat raw export of a 400×300 page is 1.90 ms and then 0.38.

### Changed

- **`lineWidth` and `miterLimit` refuse a value they cannot use, in strict mode.** They read
  their argument with the reader that ignores an unusable one, where `shadowBlur`,
  `lineDashOffset`, `globalAlpha`, `shadowOffsetX` and `shadowOffsetY` all read theirs with the
  one that objects — so two of seven numeric properties stayed silent while the other five
  spoke. Declaring them alongside the rest settled it the way the majority already behaved.

  Nothing changes with `SKIA_CANVAS_STRICT` unset, which is the default: the property is left
  alone either way. Measured across 3700 ways of calling the API wrongly — every method and
  every property, each argument in turn replaced by a NaN, an infinity, a string, an object, an
  array, `null`, `undefined`, a boolean, a symbol, a BigInt, a function and a number too large
  to be one — the answer is identical to the release before this one in default mode, and
  differs in strict mode only in these 26 cases and in one stray character removed from the
  strict-only messages.

- **A one-page APNG is now the size of the PNG it is.** A canvas with one page has no
  animation chunks, so `toBuffer("apng")` writes a plain PNG — and it wrote a much worse one.
  The APNG path pinned the `png` crate's fast compressor with adaptive row filtering, where
  the `png` path probes whether filtering pays for the drawing and deflates at level six, so
  the same pixels came out at 1212.4 KB against 700.7 on a mixed scene, 101.0 against 57.9 on
  a flat interface, and 601.6 against 42.9 on a diagonal gradient. Fourteen times the file,
  for a format a caller has every reason to expect to match.

  The comment defending the fast setting put its cost at "16% to 42% larger". That is what it
  costs on a drawing with detail in it, and not what it costs on a smooth one: the fast path
  is `fdeflate`, which does not do the long-range matching that a gradient's near-identical
  rows compress under, and no filter setting rescues it — the three tried came to 601.6, 605.8
  and 4220.0 KB.

  Animations were the reason given for pinning it and the measurement does not support that
  either: thirty frames at 1200×900 came to 3575.5 KB as it was and 1641.9 KB at level six
  with filtering off, for 28 milliseconds against 64. So the probe the PNG writer uses now
  answers for both, and both knobs are asked rather than assumed — the two are not separable,
  and `fdeflate` with filtering off wrote 126 MB for that same animation.

  What it costs is time: 14.2 ms to 56.9 on the mixed scene, 2.6 to 11.9 on the flat one and
  4.2 to 12.4 on the gradient, which is within a millisecond of what `toBuffer("png")` takes
  for the same page. No pixels change — four drawings exported both ways and decoded back, one
  hash per pair.

- **TIFF's horizontal predictor follows the drawing rather than being always on.** The
  predictor stores each channel as a difference from its left neighbour, which is the same
  idea as PNG's row filter and has the same answer: it depends entirely on what was drawn.
  Measured on five 1200×900 pages, with it on and off — 883.0 KB against 703.4 on a mixed
  scene, 76.3 against 56.1 on a flat one, 99.9 against 52.5 on a gradient, 358.5 against
  1708.4 on a photographic page and 2739.1 against 2957.0 on noise.

  So it was costing a fifth to a half of the file on three of those five, and up to 45% of the
  encode time with it, under a comment saying it was "what makes a gradient compress at all" —
  the case it gets most wrong. It is now probed once per export, and all five come out at the
  smaller of the two.

  Along the row rather than down the page: PNG's own probe answers a neighbouring question and
  was tried first, and it reads the noise page the other way, where differencing to the left
  is 7% smaller. No pixels change; a TIFF cannot be checked through a canvas because Skia has
  no decoder for one, so this is verified against the `tiff` crate's decoder in the Rust suite.

- **Cached page bitmaps are dropped once rendering stops.** A rasterized page is kept so that
  exporting an unchanged canvas again does not composite it again. An entry leaves when its
  page's generation is retired, and a canvas JavaScript has dropped retires nothing until V8
  finalizes the box holding it — which it is slow to do, because the box it can see is a few
  machine words and the bitmap behind it is megabytes. Thirty 1200×900 canvases drawn once each
  and dropped left fifteen entries holding 61.8 MB, and ten seconds of idle recovered none of it.

  The idle watcher that already trims the allocator's arenas now empties the bitmaps first, on
  the same tick and on every platform rather than only glibc, once rendering has held still for
  three seconds. The order is why the two belong together: a trim can only hand back pages that
  are already free.

  What it costs is one composite — 1.9 milliseconds on a 400×300 raw export, 1.2 on a 1200×900
  PNG — on the first export of a canvas that has been quiet for longer than that window, and only
  the first: the identity stays, so the page caches again rather than replaying in full for the
  rest of its life.

  It is worth the whole tick on both platforms, which an earlier draft of this entry got wrong.
  The bitmaps are megabyte-scale and mapped on their own, so freeing them returns the pages
  whatever the allocator: two hundred 1200×900 card exports settle at a 206.9 MB physical
  footprint against 469.4 without this. The arena trim that follows is glibc-only —
  `malloc_zone_pressure_relief` returns nothing on macOS, re-checked against physical footprint
  rather than resident size, because that allocator gives a block back when it is freed instead
  of holding it. Resident size is the wrong meter on macOS in any case: `MADV_FREE_REUSABLE`
  pages leave the resident set while staying mapped.

### Fixed

- **An argument a recorded verb refuses is blamed on the caller.** A drawing call checks its
  arguments where the recording happens, so the first line of the stack named this library and
  the caller had to read past it to find their own call — `at checkArity (drawlist.js:181)` for a
  short call, an anonymous frame in the same file for a string that was not one. The half of the
  binding that does not record has always trimmed itself out of the trace; the half that does was
  doing the opposite of its neighbour.

  Each refusal now trims back to the verb's own writer, so the caller's line is on top. A
  property write goes through a different path and trims to it instead, which puts the accessor
  on top: `ctx.lineCap = 5` reads `at set lineCap`, which is where the unrecorded half points
  too. The messages were already the better of the two and are unchanged — a recorded
  `fillRect(1, 2)` says "missing: width, height" where the unrecorded path says only "not enough
  arguments".

- **A page's bitmap is filed only while that page still exists.** An export runs on a worker and
  finishes whenever it finishes, so the generation it was handed can be retired while it is still
  going — by a full-canvas clear, by an opaque fill covering the canvas, or by the canvas being
  collected. Either way the cache entry has already been removed, and the finishing export put it
  straight back. That line creates the entry it cannot find on purpose, so that a page evicted
  while it is still being drawn can cache again, but it could not tell the two cases apart.
  Nothing can look the resurrected entry up: a lookup needs the identity, and the only holder of
  it is the recorder that just replaced it.

  Thirty-two concurrent exports of one 1200×900 canvas, five times over: 155 of the 160 stores
  landed on a generation that no longer existed, and what they left held 57.7 MB. The identity is
  now held weakly by each page handed to an export, so a store finds nothing to file under when
  the generation is gone.

      peak, 2 exports in flight    149.0 MB -> 132.6
      peak, 4 in flight            204.5    -> 157.9
      peak, 8 in flight            228.0    -> 183.6
      peak, 32 in flight           249.8    -> 229.1

  A different harness from the concurrent-export table further down, which reports a median of
  five passes rather than the process's high-water mark, so these figures are comparable within
  this entry and not across the two. Same 7787430 bytes of PNG out and the same milliseconds. What remains is about 5 MB per export
  in flight against a 4.32 MB page, which is one compositing surface each and cannot be shared.

- **A blend mode survives an SVG export.** A canvas filled red and then partly cleared exported
  as a solid red square — not a mangled clear but an absent one: the file was well-formed,
  contained a single path, and simply did not say that anything had been erased.
  `destination-out` did the same, and `multiply` and `screen` came out wrong in a different way,
  blended against nothing.

  Rasterizing each exported SVG and comparing it against the PNG of the same canvas, before the
  fix: a partial `clearRect` and `destination-out` differed on 17.4% of pixels, `multiply` and
  `screen` on 25%, while a shadow, a `blur()` and a conic gradient were already exact.

  Those three say what the mechanism is for and that it works. A layer is marked with what it
  uses that the backend refuses, rendered on its own and embedded as an image — exactly right
  for a shader, an image filter or a mask filter, because each describes how a draw paints
  _itself_. A blend mode past source-over describes how a draw combines with what is beneath it,
  and a layer rendered by itself has nothing beneath it. The erasing pair failed harder: they lay
  down no ink of their own, and a run is cropped to the ink it finds, so nothing was embedded at
  all.

  Everything from the bottom of the page through the last blend-refused layer now goes into the
  same image, which is what gives it a backdrop, and whatever is drawn afterwards stays vector.
  The last rather than the first, so every blend on the page gets a complete backdrop. What that
  costs is bounded by where the blend happens — on a page of twenty fills, a blend first leaves
  20 paths and one image, a blend last leaves one image. Only blend modes take this path.

  All eight scenes now match the raster export. The tests rasterize the SVG and compare rather
  than asserting on the markup, because the markup was never malformed — which is why this went
  unnoticed.

- **A window on a machine with no display panicked instead of saying so.** Building the event
  loop was an `expect` under a comment claiming it "only fails on unsupported platforms". It
  fails on Linux with neither `WAYLAND_DISPLAY` nor `DISPLAY` too — a container, a CI runner, an
  `ssh` session — and the panic crossed the binding as `internal error in Neon module`, naming
  neither the display nor the window that wanted one. `App.launch()` now says what is missing
  and where to look for it.

  A window opened and closed before the launch it schedules has run also cancels that launch,
  which it did not before: on a display-less machine it went on to fail for a launch nobody had
  asked for, and on any machine it started an event loop with no windows in it.

  A window that never opened leaves nothing behind either, which is the case that cancelling on
  close could not reach. Opening one validates that there is a GPU to draw with, and that refusal
  comes back out through the `Window` constructor — so the caller never receives the handle it
  would have closed, and the launch scheduled a line earlier had nothing left to cancel it. It
  then failed the same way with nobody to report to, which ends the process rather than the call.
  The native call now goes first and the bookkeeping after it, so a window that does not open is
  not tracked and schedules nothing.

- **The verbs a wrapper picks between were reachable as methods of their own.** Declaring a verb
  installed it on the prototype, so `fillPath2D`, `drawImageAt`, `appendPath`, `saveLayerAlpha`
  and sixteen others became public methods that had never existed. They took anything and drew
  nothing when given the wrong thing, where the call they stand for says what was wrong — the
  checking lives in the wrapper, and reaching past it skipped the check. A generated writer now
  replaces a method the class already declares and never introduces one.

- **A radius of `-Infinity` was refused for the wrong reason.** The recorded path checked the
  rule before checking that the value was a number at all, so `arc(x, y, -Infinity, …)` raised
  "Radius value must be positive" where the call it stands for ignores it, or in strict mode
  says it is not a number. Not a number first now, then the rule, which is the order the call
  reads them in.

- **An SVG that paints outside its own viewport leaked past a crop.** `drawImage` with a source
  rectangle reaching beyond the image is specified to clip that rectangle to the image and clip
  the destination in the same proportion. The clipped pair was computed and thrown away, which
  cost nothing for a bitmap — Skia hands the source rect to `drawImageRect` under a `Strict`
  constraint and clips it the same way itself — but a picture has no such constraint. Nothing
  bounded an SVG's overflow but the unclipped destination rect, so it drew into the part of the
  destination the crop had excluded.

  A 20×20 SVG with a shape at x = 20…40, drawn with
  `drawImage(img, -5, -5, 30, 30, 0, 0, 40, 40)`, along the row at y = 10:

  ```
  x =              0      12     24     32     34     36     38
  before        ......  ff0000 0000ff 0000ff 00ff00 00ff00 00ff00
  after         ......  ff0000 0000ff 0000ff ...... ...... ......
  Chrome 148    ......  ff0000 0000ff 0000ff ...... ...... ......
  ```

  Measured across seven crop shapes and five kinds of source — bitmap, SVG, overflowing SVG,
  intrinsically-sized SVG, canvas — thirty-four of thirty-five are byte-identical before and
  after. That one is the whole of the change, and it now matches both the specification and the
  browser.

- **Building a path out of arcs was quadratic.** Every `arc()`, `ellipse()` and `roundRect()` on a
  `Path2D` snapshotted the whole path built so far, transformed it, and rebuilt the builder from
  it — twice, once to rotate the path into the arc's frame and once to rotate it back. So the cost
  of adding an arc grew with the path it was added to: 12 µs on a 250-segment path, 76 µs on a
  2000-segment one, where a path of straight lines stays flat at about 0.25 µs. One path of 2000
  ellipses took 152 ms to build; it now takes 2.

  The rotation is why it was written that way, and it was paid whether or not there was one —
  `arc()` always rotates by zero, and so does most use of `ellipse()`. The arc is built on its own
  now and added with the rotation applied to it, which is the same drawing for a fraction of the
  work.

  Found while looking for something else, which is worth saying: it is not what makes recording a
  frame of `examples/node/animated-eye.js` cost what it does. That drawing's paths are a few
  segments each, so nothing there ever grew enough to notice.

- **A page written once was cached as though it would be read again.** Writing one file per page
  — `saveAs("frame-{}.png")` and the Rust `write_sequence` behind it — exported each page once
  and never asked for it again, while filling the 64 MB page cache with bitmaps at a hit rate of
  zero. Peak memory over the 150-frame sequence: 681 MB with them kept, 590 without, at the same
  speed and the same bytes.

  Only the store is skipped. A page that already has an entry still replays only its new layers,
  which is what the cache is for.

- **Peak memory no longer grows with the size of the thread pool.** Measured on the same
  sequence with `RAYON_NUM_THREADS` pinned, it was 648 MB at one worker, 728 at four and 800 at
  eight — about 22 MB a worker, each context carrying its own Skia resource cache, and on Apple
  Silicon the device side of that is the same resident memory. It is 680, 714 and 731 across the
  same range now: a slope of about 4 MB a worker, with no context in it.

  A GPU export of the same sequence peaked at 909 MB before this release and peaks at 744 after,
  while its time went from 890 ms to 431. Sweeping the pool with the owners in place: 680 MB at
  one worker, 714 at four, 731 at eight, 743 at sixteen — about 4 MB a worker rather than 22,
  and what is left is the encoders' own buffers rather than a context.

## 📦 ⟩ [v5.5.1] (npm) / [v0.9.1] (crate) ⟩ August 17, 2026

Documentation only, on both channels. `README.md` ships inside the npm package and is what
docs.rs renders for the crate, so a claim that is wrong there is wrong everywhere someone
looks before installing.

### Fixed

- **The Rust quick start pinned a version from three minors ago.** The first snippet a Rust
  reader copies asked for `meo-skia-canvas = "0.6"`, against a crate at `0.9`. Nothing in the
  example needed the older version; it had simply not been touched since.

- **Two claims about the animated example had gone from true to false.** Regenerating it was
  described as costing 13 seconds and attributed to "a k-means palette per GIF frame". It
  measures 27 seconds, and since v5.5.0 a GIF quantizes the rectangle a frame changed rather
  than the whole frame. The paragraph read as correct while its reasoning had stopped being
  so, which is the worse of the two ways to be wrong: the file size it explains is still
  12.2 MB, so nothing about the output invited a second look. That size is the 256-entry
  palette against a drawing that is mostly smooth gradient, and the same example is the one
  where dirty rectangles buy nothing at all -- it reseeds 260 film-grain specks a frame, so
  nearly the whole page changes.

- **The benchmark tables were stale and reported time without size.** They now come from one
  run of `just bench`, and each format carries the bytes it produced beside the milliseconds
  it took, because neither figure means much alone -- the fastest encoder here writes the
  largest file and among the slowest writes the smallest.

### Internal

- **An erasing GIF animation is now checked by its picture rather than by its disposal byte.**
  The existing test asserted the encoder asked for the canvas to be cleared between frames,
  which is weaker than it looks: it says nothing about whether clearing it produces the frames
  that were drawn. Erasing is the part of the per-frame rectangle work most likely to be
  silently wrong, because it is the one thing a GIF frame cannot do for itself -- a
  transparent index means "leave what is underneath" -- and when it is wrong the animation
  does not fail, it accumulates. Forcing the disposal back to `Keep` fails the new test, which
  is the regression it exists to catch.

## 📦 ⟩ [v5.5.0] (npm) / [v0.9.0] (crate) ⟩ August 17, 2026

Every format that carries a clock exports faster, for three separate reasons: one asks the
compressor to do less, one uses more than one core, and one stops sending pixels that did not
change. Nothing was added to either API and nothing was removed — `FrameSink`, where most of
this landed, is `pub(crate)` — but two of the four formats now produce different files, which
is what makes this a minor rather than a patch.

Figures throughout are release builds of a 1200×900 page, a still background with a moving
foreground, with the baseline taken either side of each change to catch machine drift. That
scene matters: it is what a dirty-rectangle encoder is actually asked to compress, and a page
where everything moves would show none of this.

### Faster

- **A GIF frame carries only the rectangle that changed.** The large one: 120 frames went from
  2905 ms to 384 and from 16,275 KB to 361 — 7.6 times faster and 45 times smaller — and 30
  frames from 724.6 ms to 125.1, 4070 KB to 207. A GIF re-encoded the whole page for every
  frame while the two encoders beside it sent only what moved, though the image descriptor has
  carried a per-frame offset and size since 1987. The time follows the bytes: quantizing and
  LZW both run over the rectangle now instead of the page, so a still background costs nothing
  twice.

  What stopped this being done was real rather than an oversight, and it is worth writing down
  because it is a fact about the format rather than about this encoder. **A GIF frame cannot
  erase.** Its transparent index means "leave what is underneath", so a rectangle laid on the
  canvas can add pixels and change them and can never take one away, and the only eraser the
  format has is disposing a frame to the background — which happens _after_ that frame is
  shown, and clears the rectangle that frame covered. So a frame needing a pixel erased depends
  on the frame _before_ it having arranged the clearing, and the encoder used to be handed one
  frame at a time and could not look ahead. It is handed a batch now, so it holds one frame
  back and lets the next settle two things: whether that frame disposes to the background or
  keeps, and whether its rectangle has to widen to the whole canvas so that disposing it clears
  the whole canvas.

  The files are different, not merely smaller — rectangles at offsets, `Keep` disposal — so
  anything comparing a GIF export against a stored fixture will see it move once.

- **APNG, WebP and AVIF compress their frames on every core.** APNG went from 49.9 ms to 14.0
  at 30 frames and 151.8 to 46.6 at 120; WebP from 81.4 to 41.5 and 223.9 to 84.8; AVIF from
  814.7 to 743.3 at 30. Rasterizing the pages was already spread across the pool, and then the
  batch was handed to the encoder a frame at a time, so every compression in the animation ran
  one after another on the thread doing the writing.

  Nothing about any of these formats required that order. The dirty rectangle a frame carries
  is found by comparing it with the _pixels_ of the frame before it, and those were rasterized
  long before — it never waited on a byte being compressed. What genuinely has to stay in order
  is the container, which is a few hundred bytes a frame.

  The cost is peak memory during an animated export, because a batch of frames is now narrowed and
  held at once rather than one at a time. Measured over 120 frames at 1200×900: WebP peaked at
  227 MB against 200, APNG at 218 against 199, AVIF at 482 against 478. It scales with the batch —
  one frame per core — rather than with the length of the animation. GIF went the other way, 237 MB
  against 256, because it now quantizes a rectangle instead of building a full-canvas index buffer
  for every frame.

  **WebP and AVIF are byte-identical**, verified by checksum at several frame counts rather
  than assumed, and so is the parallel half of the APNG change. This is worth stating plainly
  because "faster" usually is not.

  AVIF is the partial one, and the exception that proves the rule: AV1 predicts each frame from
  the one before it, so its frames really cannot be coded in parallel and are not. Only the
  colour conversion feeding the coder was moved — widening an eight-bit page to the sixteen-bit
  buffer the coder reads, which allocated 8.6 MB per frame on one thread, plus the transparency
  scan and the YCbCr conversion. That is 8.8%, and it is all that was there to take: skipping
  the encode call outright takes the same export from 838 ms to 135, so the codec is 84% of it
  and already uses every core.

- **APNG asks the compressor to do less.** The `png` crate has two compressor paths, and they
  differ by strategy rather than by implementation: `Balanced` — the default nobody had chosen
  — goes through flate2, while `Fast` uses `fdeflate`, a DEFLATE written for PNG's data. A
  still 1200×900 page encoded in 13.9 ms against 89.4.

  No pixels change. PNG is lossless and both the compression and the row filtering are
  reversible, so the two settings decode identically — verified rather than assumed, with a
  twelve-frame animation written both ways decoding to byte-identical RGBA, 15,360,000 bytes at
  the same checksum. There is no quality dial here, unlike the `quality` on JPEG, WebP or AVIF.

  The cost is size: **APNG files are 16% to 42% larger**, the spread depending on how much
  redundancy the drawing holds for the slower search to find. Flat panels and hard edges lose
  most, a noisy scene least. That is the whole trade — an order of magnitude of time against a
  fraction of the bytes — and it is why this is a default rather than an option: a caller who
  wanted the smaller file would otherwise wait ten times as long for pixels they already had.

  Swapping flate2's own backend instead, via the `zlib-rs` feature, was measured on the same
  benchmark and changed nothing at all — 93.5 ms against 93.3, inside the drift between two
  runs of the unchanged build. That result is what identifies the strategy rather than the
  implementation as the thing that mattered.

### Fixed

- **The page cache evicted the entries that were working and kept the ones that were not.** Its
  clock was marked by every lookup rather than by every lookup that could be served, so an entry
  that no longer matches — a different density, matte or sample count — was marked fresh by each of
  the misses it caused. A page being replayed from scratch on every export therefore outranked a
  page actually being served from the cache, and outlived it under eviction. The bound exists to
  keep the entries that save a replay, and a miss saves nothing.

  Eviction also walks the map once now rather than twice: it summed the bytes in one pass and
  searched for the oldest entry in another, on every export of every page.

### Known, and not fixed here

- **A GIF export is not reproducible across machines with different core counts.** The palette
  comes from `quantette`, whose parallel path reduces in whatever order its threads finish, so
  the same drawing quantizes differently on an eight-core machine and a sixteen-core one —
  three thread counts, three different files. This predates the change above and none of it is
  touched by the rectangles, which is why it is recorded rather than claimed as fixed.

## 📦 ⟩ [v5.3.0] (npm) / [v0.8.0] (crate) ⟩ August 16, 2026

One new export option, one saving that needs no option at all, and five correctness fixes that
were none of them new. Two came from upstream in July 2025 and have been in every release since,
one has been there as long as the page cache has, and one — a gradient fading a colour out fading
it toward black — has been in the Rust surface since gradients were. Most surfaced in an audit of
the released tree rather than from a report, which is worth saying: a process that renders a few
hundred canvases and exits was never going to notice the memory ones, and the gradient only shows
where a stop is fully transparent.

### New

- **`pageRange` gathers a span of pages rather than all of them or one.** `page` names a single
  page and its absence names every page, and there was nothing in between — so an intro that plays
  once followed by a cycle that repeats forever could not be written from one canvas, because a
  file carries one loop count and the two halves want different ones. Two calls do it now:

  ```js
  const intro = await canvas.toBuffer("webp", {
    fps: 30,
    pageRange: [1, 20],
    loop: 1,
  });
  const cycle = await canvas.toBuffer("webp", {
    fps: 30,
    pageRange: [21, 60],
    loop: 0,
  });
  ```

  It counts the way `page` counts: from one, inclusive at both ends, negatives from the end, so
  `[2, -1]` is everything after the first page. It serves the paged documents as much as the
  animations — `{ pageRange: [12, 18] }` pulls a chapter out of a long PDF, and a filename
  template such as `frame-{}.png` writes only the frames named. Naming it alongside `page` is a
  `TypeError`, a bound past either end is a `RangeError` naming the page asked for, and a range
  given to a format that encodes one page is refused rather than ignored. `frameDelays` is counted
  against the frames the call will write, as it already was for `page`.

  The pages are sliced before the encoder is built rather than skipped as it runs, which is what
  the animations need: WebP codes each frame as the rectangle it differs from its predecessor in,
  so a range whose first page still had a predecessor would open on a rectangle diffed against a
  page the file does not carry.

- **`RgbaLinear::fading_out`** gives a colour at zero alpha with its hue intact.
  `with_opacity(0.0)` multiplies the channels away, which is what premultiplication means and is
  right everywhere a colour is painted — at zero alpha nothing is drawn, so the hue cannot matter.
  It matters in one place, a gradient stop, because there the colour is not painted but
  interpolated toward. This is how a stop says which colour is disappearing.

### Fixed

- **Every exported canvas kept its rasterized page for the life of the process.** `PageCache`
  memoizes the last bitmap of a page so a later export can composite it rather than replaying every
  layer. An entry went in for each canvas and came out in exactly one place: a `Drop` that runs when
  V8 finalizes the `JsBox` holding the context. V8 sizes that box at a few machine words and cannot
  see the half-megabyte image behind it, so it feels little pressure to collect and is slow to
  schedule the finalizer. Measured over a thousand fresh 400×300 canvases, each drawn once and
  exported: 235 MB held, against 141 with the cache bounded, and the published upstream this is
  forked from settles at 233 on the same test. It does level off — V8 gets to the boxes eventually,
  under pressure from its own heap — so this is a plateau far above what the work needs rather than
  a climb without end. The same canvases exported to SVG retained 2 KB apiece, which is what
  identified the cache rather than the machinery around it: a vector export never reaches the store.

  The bound is a byte budget rather than an entry count, because an entry is a whole page and pages
  are not one size. Sixty-four entries is a different quantity of memory at every canvas size:
  exporting one card at a time, a thousand times, 0.76 MB pages settled at 184 MB, 3.0 MB pages at
  290, and 12 MB pages at 820. Against a budget the same workloads settle at 161, 173 and 165 —
  flat in page size instead of sixty-four times it. Evicting changes no pixel: a miss replays the
  layers the recorder always keeps, which is what the first export does anyway.

- **The font parse caches grew for the life of the process too.** `parseFont` and `parseVariant`
  memoized into plain objects that nothing evicted, so every distinct string passed to `ctx.font` or
  `ctx.fontVariant` stayed until exit — 435 bytes each, 83 MB across two hundred thousand. A font
  string carries a number whenever text is sized to fit, so an animation that scales a label adds an
  entry a frame and never asks for it again. Bounded at a thousand, the cache holds flat at 0.65 MB
  however many strings go through it. The parse is pure, so an evicted entry reparses to the same
  value.

- **Resident memory falls again once rendering stops, on glibc.** A C allocator keeps freed pages in
  its own arenas rather than handing them back, so resident memory only ever climbed: two hundred
  card exports peaked at 165 MB and stayed there, holding pages for a render that might never come.
  Nothing was lost — the next export is served out of those pages, which is why repeating a workload
  costs nothing after the first pass — but a process that has finished a job should not read like
  one still doing it. A watcher started on the first rasterization now returns them once rendering
  has stopped for three seconds: 165 MB during the batch, 88 four seconds after the last card,
  against 72 at startup. Rendering continuously for eight seconds holds at 150 to 153 and is never
  interrupted, and the trim fires once rather than walking the arenas every second forever. Nothing
  to call and nothing to configure. glibc only — macOS returned nothing to
  `malloc_zone_pressure_relief` when measured, and musl's allocator is a different design that wants
  its own measurement first; elsewhere no thread is started at all, and none is started on glibc
  either until something renders.

- **An export changed what the next one drew, on the GPU.** Drawing, exporting, drawing again and
  exporting produced different pixels from drawing the same thing straight through — an arc came out
  192 bytes and up to 64 levels away from its twin. Multisampled coverage is per-sample and binary,
  so drawing an edge twice resolves to the same value, while compositing an already-resolved bitmap
  and then drawing over it mixes a partial-alpha texel with fresh sample coverage. Identical at
  `msaa: 0` and `msaa: 1`, and identical on the CPU. The cached bitmap is now used only where
  compositing it is the same operation as drawing the layers it stands for: when nothing remains to
  draw on top. Re-exporting an unchanged canvas still takes the cache; an export that follows further
  drawing replays, which costs 0.65 ms against 0.77 on a two-hundred-shape scene and is work it was
  going to do for those layers anyway.

- **A gradient fading a colour out faded it toward black.** Only from Rust, and only where a stop
  was fully transparent — which is every soft vignette, every glow, every edge that dissolves into
  its background. `RgbaLinear` is premultiplied, so `from_srgb8(246, 242, 238, 0.0)` is four zeros
  and cannot be told apart from CSS's `transparent`, which is a transparent _black_. Skia
  interpolates unpremultiplied, so the conversion had to undo the premultiplication and could not
  at zero alpha; it substituted black, and every such gradient ran toward black. Halfway along a
  cream vignette over a blue ground it read `[67, 88, 142]` where the JavaScript binding reads
  `[123, 143, 195]` — 56 levels of red apart, and visible as a grey ring around the animated-eye
  example.

  At zero alpha premultiplication has multiplied nothing away, so whatever channels are stored are
  already the straight hue, and the conversion now reads them instead of discarding them. CSS's
  `transparent` still fades to black, because a transparent black stores black; both cases are now
  expressible and both are pinned by tests. The two surfaces share Skia and had always differed
  only in how colour reached it — the binding never premultiplies on the way in, so it never had
  the problem.

### ⚠️ Five font strings that used to throw are now ignored

`ctx.font = "constructor"` threw `failed to downcast any to object`, and so did `toString`,
`__proto__`, `valueOf` and `hasOwnProperty`. The cache was a plain object, so those names found
`Object.prototype` members sitting in the slot and handed back a function where a parsed font
belonged. With a `Map` they fail to parse and are ignored, which is what the Canvas standard asks of
an unparseable font string and what a browser does. `ctx.fontVariant = "constructor"` now throws
`Invalid font variant "constructor"` rather than the downcast error. Every one of these was broken
before; none of them was doing anything useful.

### Faster

- **An APNG frame carries only the rectangle that changed.** WebP has done this since animations
  arrived; APNG wrote every frame at full canvas size, though `fcTL` has carried an offset and a
  size since 2008. On a page that is mostly still — a ground, a heading, a list, and one badge
  sliding across the bottom, 150 frames at 640×500 — the file went from 396,556 bytes to 32,122, a
  little over twelve times smaller. Nothing about the picture changes: each frame states the
  rectangle it covers, disposes nothing and replaces rather than blends, so everything outside it
  is what the frame before left there.

  How much it saves is entirely the drawing's business, and the honest range is wide. Where the
  whole page moves the rectangle is the whole page and the file comes out byte-identical, so the
  pass costs nothing where it can win nothing. Anything scattering marks over the page lands near
  that end: this repository's animated-eye example reseeds 260 film-grain specks every frame, and
  its rectangles do shrink — 63 distinct sizes across 150 frames, only 11 of them the full canvas
  — but most still cover about 97% of the page, and the file falls 0.6%, from 48,897,087 bytes to
  48,618,039.

  Two frames that are identical still have to carry a pixel, since a zero-sized `fcTL` is a format
  error, so a still passage costs one pixel a frame. Comparing against the last frame means holding
  it: an animated APNG export now carries one extra canvas for its duration — 1.2 MB at 640×500 and
  eight bits, twice that at sixteen — which is what the WebP encoder has always done.

### ⚠️ Crate `0.8.0` — breaking

- `EncodeOptions` gained `page_range`, so a caller who builds one by naming every field has to add
  it or switch to `..EncodeOptions::default()`. It is `Option<Range<usize>>` — zero-based and with
  the end excluded, as a Rust range is, matching `page` on this side being zero-based while the
  binding's is not. Each surface counts the way its own language does.

### Internal

- **The Rust animated-eye example drew a shut eye.** No sclera and no iris in any of its 150
  frames, against a JavaScript twin that has always been right — which made it the one example
  whose output contradicted the drawing it claims to demonstrate. `f32::consts::PI` rounds _up_
  past pi, so `(1.0 * PI).sin()` is `-8.74e-8` rather than zero, and a negative base under a
  fractional exponent is NaN. Every lid profile samples `u` at exactly 1.0, so the last vertex of
  each curve was NaN, and Skia turns a path holding one into an empty path rather than into an
  error: `opening_path` returned bounds of (0, 0)-(0, 0), the clip built from it was empty, and the
  eyeball drawn inside that clip went nowhere. The lid, lashes and brow were unaffected, so the
  result looked like a closed eye rather than like a fault. The JavaScript computes the same
  expression in double precision, where `Math.sin(Math.PI)` is a small _positive_ number, and never
  had it to solve. Present since the example was written and in every release since. `clip_path`,
  `restore` popping a clip and a mask filter, and opaque gradients on both an sRGB and a Display P3
  canvas were each checked along the way and each behaved; the grey ring around the same drawing
  was a second fault, and that one was ours — see the gradient entry under Fixed.

- **The Rust guide moved to `docs/rust.md`.** It sat in `docs/api/` beside the two JavaScript
  references, which made it look like a third reference; it is the counterpart of `docs/node.md`, a
  platform guide, and it now sits beside it. The per-item reference for the crate is docs.rs, and
  the page says so at the top: generated from the source and versioned per release, where a
  hand-written list drifts. It had been listing three variants of `PixelDepth` since that enum grew
  to twenty-four. Every link to the old path moved with it, including the copy packaged in the
  crate and the CI path filter. The note about what `set_dither` is for went into the rustdoc it
  belongs in on the way past.

- **The benchmark's memory table measured the allocator rather than the canvases.** It read an RSS
  delta inline, after every other section had run, by which time the process holds a pool of freed
  pages the new allocations come out of. It reported `RGBAF32` at 0.31 MB against a surface of
  16.48 — impossible, since a held canvas cannot cost less than its own pixels — and 6.89 before the
  page cache was bounded. Each depth is measured in its own process now, three times, median taken,
  and the table reads 4.22, 8.35 and 16.58 MB against surfaces of 4.12, 8.24 and 16.48. No library
  behaviour changed; the same binary measured in a fresh process always landed on the arithmetic.

- **The crate docs mention the CSS colour setters.** The colour section explained why `RgbaLinear`
  is not the triple a CSS colour parses to and sent a reader to `from_srgb8` to convert one by hand,
  without mentioning that the fill and stroke styles take the string directly.

## 📦 ⟩ [v5.2.0] (npm) / [v0.7.0] (crate) ⟩ August 16, 2026

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
  trade at all — 86% smaller _and_ three and a half decibels better — because libaom has
  screen-content tools, palette mode and intra block copy, that rav1e does not. That is the content
  a canvas library actually produces. `quality` keeps its meaning: the curve produces a fraction of
  the encoder's range rather than a step count, so moving from rav1e's 255 steps to libaom's 63
  moved the scale and left the dial alone.

- **AVIF codes losslessly, through `lossless`.** Off by default, and deliberately: AVIF is reached
  for because it is small, a lossless one is several times the size of a lossy one and often larger
  than the PNG it would replace. The flag alone would not have been honest — quantizing at zero
  preserves what the encoder was given, and converting red, green and blue into a luma and two
  colour differences has already lost before quantisation runs. So this codes the identity matrix,
  ITU-T H.273 matrix 0, where the three planes _are_ green, blue and red, and states it in `colr` so
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

- **A Rust `TextStyle` can ask for a condensed or expanded face.** It carried weight and slant and
  hardcoded the third `SkFontStyle` axis to normal at both places it built one, so a family's
  condensed faces were unreachable from Rust while `fontStyle.width` reached them from JavaScript.
  The field is `stretch` and takes the same `FontStretch` the canvas `fontStretch` property does.
  It selects among the widths a family ships rather than transforming glyphs; a variable font's
  `wdth` axis is still `font_variations`.

- **A Rust paragraph can be laid out right to left.** `TextStyle` gained `direction`, which the
  JavaScript surface has had as `textDirection` all along. Without it every paragraph built from
  Rust was left-to-right whatever it held — not a styling difference but a layout one, since the
  base direction is what decides which edge a line starts from and where `Start` and `End` point.
  Runs still take their own direction from the characters, as the bidi algorithm requires.

- **The two rectangle styles are named constants in JavaScript.** `getRectsForRange` took bare
  integers for its height and width style while `TextDecoration`, `TextDecorationStyle`,
  `PlaceholderAlignment` and `TextBaseline` were all exported by name, so the two enums that decide
  whether a selection highlight meets its neighbours were the ones a caller had to spell as numbers.
  `RectHeightStyle` and `RectWidthStyle` are exported now, frozen like the four beside them, and the
  declaration types the two parameters rather than leaving them `number`.

- **`ImageFilter` reaches two more samplers and three crop rects.** `"mipmap"` and `"cubic"` were
  reachable from Rust and not from JavaScript on the same two filters; dilate, erode and matrix
  convolution take a crop that bounds the kernel's read domain as well as clipping the output.
  `createConicGradient` takes an optional fourth argument for the end angle — the Canvas API always
  sweeps a full turn, and Skia can sweep any arc.

### Fixed

- **Rendering on Vulkan from many threads no longer crashes the process.** A `VkQueue` has to be
  externally synchronised, and queues were handed out by a counter modulo the queue count — a
  counter of threads ever created rather than threads alive — so a thread that outlived the next
  sixteen shared its queue with a thread that had no idea. Nothing serialised the two: Skia submits
  on that queue from behind a surface, long after the call that created it returned and any lock it
  held was gone, and the NVIDIA driver answered a concurrent submit by faulting rather than
  returning an error. Skia asks the client for every Vulkan function it uses, so it is now handed a
  `vkQueueSubmit` that takes the lock for that queue and forwards to the driver's — which serialises
  every submit Skia makes, wherever it makes it from. Every thread still renders on the GPU,
  including on the integrated devices that offer a single queue and where all of them share it.
  Reproduced on a GTX 1050 Ti: three of three runs crashed before, none of eight after, and the
  Vulkan validation layer went from sixty-four threading violations in a run to none — on that card
  and on an Intel UHD 630 driving twelve threads through one queue.

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

- **An APNG whose two frame counts disagree is read at the shorter one.** The specification makes
  `acTL`'s `num_frames` equal to the number of `fcTL` chunks, and `png` 0.18 enforces neither — it
  checks subframe rectangles and `fdAT` sequence numbers and never compares these two. This library
  read one count in each half: `frames` counted the `fcTL` chunks, the reader stopped at
  `num_frames`. On a file declaring two frames and carrying four, index 3 therefore cleared the
  range check and then answered differently depending on whether the decoder had been used — a
  fresh one silently returned frame 1's pixels under index 3, one already part-way through the
  animation returned `The APNG has no frames`. Same file, same index, two answers. Nothing could
  see it while walking an animation forward, which is why every test here passed: it needs a jump
  to a late frame or a step backwards. **This shortens such a file** — it reports two frames now
  and refuses indices 2 and 3, rather than offering four that cannot be decoded.

- **An APNG with a separate default image plays from its first frame.** Where no `fcTL` precedes
  `IDAT`, that image is a still poster rather than part of the animation, and `acTL` does not count
  it — but `png` hands it back as an extra subframe ahead of the ones that are counted. The reader
  here numbered subframes as it received them, so every index named the frame before the one it
  should have: index 0 drew the poster, and the animation's last frame sat one past the count and
  could not be asked for at all. The poster is now read past when the file is opened, so an index is
  the frame `frames` timed. Found while fixing the frame-count disagreement above; this library's
  own encoder never writes the shape, which is why nothing caught it.

- **A synchronous export on the GPU no longer leaks the surface it drew.** `toBuffer` and `toFile`
  wrap their work in an autorelease pool because they run on a `rayon` worker; `toBufferSync` and
  `toFileSync` did not, on the belief that node's event loop drains one on the main thread. It does
  not — node runs no `NSRunLoop`, so Metal's `objc` allocations had nowhere to go. A hundred GPU
  canvases exported per pass grew RSS 512, 886, 1257, 1633, 2004, 2376 MB across six passes that
  each awaited and forced two collections: about 3.9 MB a canvas, near enough the whole surface,
  never returned. The same run now peaks at 152 MB.

- **A variable font registered under an alias keeps its axis.** An instanced typeface has to be
  filed under the name the lookup will search by — the family the caller asked for — and it was
  filed under the name inside the font file. Where the two differed the match found nothing and the
  request fell through to the uninstanced face, silently. Oswald registered as `"OswaldAlias"`
  measured the same width at `wght` 200 and 700 — the family's default, twice — where the same font
  under its own name gives 359.04 and 446.46. No error, no warning, just a weight axis that did
  nothing.

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

- **An export that panics rejects instead of killing the process.** `toBuffer` and `toFile` encode on
  a `rayon` worker, and `rayon` aborts the process when a panic escapes one — a `SIGABRT` that
  neither `try` nor `.catch()` can reach. The same panic through `toBufferSync` was an ordinary
  catchable `Error`, so one input was a rejected promise one way and a dead process the other, and
  the asynchronous form is the one the documentation shows. Both now carry a barrier that turns a
  panic into an error. This does not make panicking acceptable — every one is still a defect and the
  message is still opaque — but a server survives one.

- **A fractional `page` is refused rather than indexed.** `{page: 1.5}` cleared every guard: greater
  than zero, so it became index `0.5`; neither negative nor past the end, so the range check passed;
  and `pages[0.5]` is `undefined`, which left the native side indexing an empty list. Combined with
  the above it aborted the process. `page` must be an integer now, as `loop` already had to be.
  **This can throw where something worked**: a non-number that used to coerce — `{page: "2"}` — is a
  `TypeError`, and `{page: NaN}` no longer silently exports every page. `density` still takes a
  fraction, deliberately.

- **A paragraph shadow's `blurRadius` is a radius, not a sigma.** It was handed to Skia unscaled,
  and Skia's parameter is the sigma — which is half the radius, by the same CSS sentence
  `shadowBlur` has always been halved against. So one library answered a single number two ways:
  `shadowBlur = 8` on a context blurred half as far as `blurRadius: 8` on a paragraph. Measured on
  a 64px glyph, the shadow spread 90px where the context's spread 67px. **This changes existing
  output** — a paragraph shadow now renders at half its previous blur, which is what the option
  always claimed. Double the value to keep what you had. Neither side had ever been measured
  against the other, which is why it survived: either alone looks like a shadow.

### Faster

Every figure here is measured on one machine, so read the ratios rather than the milliseconds.

- **`getImageData` on the GPU cost a device sync per call.** `Surface::read_pixels` flushes and
  blocks until the device is done, and that wait was the entire cost: an 8×8 read measured 154
  microseconds against 7 on the CPU, and it was flat against both the rectangle and the canvas —
  the same eight-by-eight read took 149 to 220 microseconds on canvases from 64 to 2048 square, and
  reading the same unchanged canvas again paid it again. The surface is copied to the CPU once per
  state and read from there, so that read is now 7 microseconds and a full-canvas read at 256
  square went from 224 to 61. Per-pixel work — hit testing, image diffing, a visual-regression
  suite — was an order of magnitude faster with `gpu: false`, which is the opposite of what the
  default implies.

- **A variable-font layout stood up a whole font manager each time.** `collection_for` builds a
  fresh `FontCollection` whenever a style carries `font_variations`, and it called `FontMgr::new()`
  for every one — 9.0 milliseconds on its own against 9.6 for the entire layout. Held once and
  cloned instead: 9637 microseconds to 98.

- **Measuring a wrapped paragraph was quadratic in its lines.** 480 lines took 211 milliseconds and
  doubling the count multiplied the time by about 3.9 each step. The conversion from Skia's byte
  offsets to the UTF-16 positions JavaScript counts in rebuilt a table of the whole text on every
  line, then summed from the beginning of it to reach that line. Built once, with the lookups a
  binary search and a subtraction: 19 milliseconds.

- **`measureText` stopped serialising, and then stopped building a tree nobody read.** The metrics
  crossed the binding as a JSON string the wrapper parsed back. That went first, replaced by an
  object built from a `serde_json::Value`; then the `Value` went too, and the object is now
  assembled straight from the measurement.

  Only the pair is a win, and the first half on its own was not. Measured again after the fact,
  release builds of each commit run alternately in one session so a drifting machine lands on all
  of them equally, ten-character measurement, median of five:

  | binding returns                            | µs      |
  | ------------------------------------------ | ------- |
  | a JSON string the wrapper parses           | 12.1    |
  | an object built from a `serde_json::Value` | 13.8    |
  | an object built directly                   | **9.8** |

  Dropping the round trip cost 1.7 microseconds rather than saving any: V8 parses JSON very
  quickly, and building the same object through N-API a field at a time does not beat it. What
  paid was removing the `Value` — the metrics had been assembled into a tree, copied out of it
  field by field, and dropped, with nothing reading the tree in between. Net across both, 12.1 to
  9.8.

  An earlier draft of this entry claimed the first change alone took a measurement from 80
  microseconds to 59. Those figures came from a debug build, where `serde_json` is disproportionately
  slow and N-API calls are not, and they are wrong about the direction as well as the scale.

  Output is unchanged — 84 cases spanning wrapping, condensing, letter and word spacing, non-BMP
  characters and multi-font fallback runs compare byte for byte against the previous build. The
  Rust API never went through the tree, so it is untouched.

- **A GIF frame was narrowed four times to be written once.** Twice inside `quantize`, once for the
  transparent index and once in the rewrite loop. On a float canvas each is a whole-page conversion,
  about 8 MB at 1080p, so a six-frame export paid for eighteen it did not need; on an eight-bit
  canvas the doubled alpha scan cost a second full pass over every pixel. Byte-identical output.

- **An animated AVIF kept its first frame twice.** The `meta` box points at a still, so the sink
  holds frame zero while the sequence goes past — widened to sixteen bits, then cloned from a buffer
  the widening had already made owned. Measured with a counting allocator around the first
  `write_frame` of a 1920×1080 sequence, live bytes after frame zero went 16,601,393 to 8,306,993:
  exactly one 1920×1080×4 buffer, gone.

- **The variable-font collection cache had no bound.** Its key carries the axis values quantized at
  a thousandth of a unit, so a page tweening `wght` added an entry per frame and kept it for the
  life of the process — the library is a `thread_local` `OnceLock`, so it does not go when a canvas
  does. Three thousand distinct values held 27 MB of map. Bounded at 128 entries, least recently
  used. Worth being exact about what that does not fix: those three thousand instances grow RSS by
  about 130 MB and this map is 27 of it. The rest is retained inside Skia per instanced typeface,
  where nothing here can reach it.

- **A window redraw cloned the page it only read.** `Page` holds a `Vec<Picture>` and a
  `Vec<VectorFeatures>`, so every frame of a live window paid two vector allocations and a refcount
  bump per picture for a value the renderer never mutated. Small — a hundredth of a percent of a
  frame at sixty a second — and free to stop doing.

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
  quadratic _parse_ behind the quadratic decode it removed.
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
- A multi-chunk AVIF sequence is read rather than misread. Samples sit end to end _within_ a chunk
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
  const spinner = await loadImage("spinner.gif");
  for (let i = 0; i < 24; i++) {
    ctx.drawImage(spinner.frame(i % spinner.frames), 0, 0);
    canvas.newPage();
  }
  ```

- **GIF and APNG, with the pages as frames.** One page is one frame. `fps` defaults to 30;
  `frameDelays` overrides it per frame and takes exactly the array `Image.delays` reports, so
  re-encoding an animation is a round trip. `loop` is `0` for forever, which is how both formats
  spell it.

  ```js
  await canvas.saveAs("out.gif", { fps: 12, loop: 3 });
  await canvas.saveAs("out.apng", { frameDelays: source.delays });
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

- **Gradient stops were coming out dark** _(Rust only)_. They were handed to Skia untagged, which
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
- **A text decoration with no colour of its own takes the text colour** _(Rust only)_, as the web
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
- **`page` is honoured by every format that spans pages** _(Rust only — the binding always sliced to
  the page first)_. The spanning branch was taken before `page` was read, so
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

  | call                                         | before                      | now         |
  | -------------------------------------------- | --------------------------- | ----------- |
  | `MaskFilter.MakeBlur("bogus", 4)`            | a normal blur               | `TypeError` |
  | `ImageFilter.MakeBlur(4, 4, "bogus")`        | tile mode `decal`           | `TypeError` |
  | `ColorFilter.MakeBlend("red", "bogus")`      | source-over                 | `TypeError` |
  | `ColorFilter.MakeBlend("notacolour", …)`     | blended with black          | `TypeError` |
  | `ColorFilter.MakeLighting("white", "bogus")` | fell back to white          | `TypeError` |
  | `ImageFilter.MakeDropShadow(…, "bogus")`     | a black shadow              | `TypeError` |
  | `canvas.newPage(500)`                        | a page at the old size      | `TypeError` |
  | `new Paragraph()` / `new TextMetrics()`      | an object that failed later | `TypeError` |
  | an unrecognised `colorSpace`                 | silently sRGB               | `TypeError` |

  Omitted arguments still take their defaults, and `null`/`undefined` still mean "use the default".
  `globalCompositeOperation` is deliberately unchanged: the Canvas standard requires it to _ignore_ an
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
  ctx.maskFilter = new MaskFilter("outer", 6);
  ctx.fillStyle = new Shader("turbulence", 0.08, 0.08, 4, 0);
  ctx.colorFilter = new ColorFilter("blend", "red", "multiply");
  ctx.imageFilter = new ImageFilter("drop-shadow", 2, 2, 3, 3, "black");
  const para = new ParagraphBuilder({ textStyle: { fontSize: 16 } })
    .addText("hi")
    .build();
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
  translucent layers is _faster_ in float, because an eight-bit surface converts through its
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
  - With 4× unavailable, the sample-count fallback took the _largest_ the device offered — up to 32×
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
  not survive checking: public signatures were said to expose no `skia_safe` or `neon` type _with CI
  verifying it_, when no such check existed and four `gui` methods do; the eight-bit compositing
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

| case                                    | differing pixels, before |
| --------------------------------------- | ------------------------ |
| rounded rect built from `arc()`, filled | 44.35%                   |
| clip through an arc                     | 27.00%                   |
| fill after `lineTo` + `arc`             | 16.95%                   |
| `ellipse()` filled                      | 13.14%                   |

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

Separately, `colorType` was being used to allocate the _compositing_ surface rather than the readback
format. Rasterizing into an opaque type turned the transparent clear black and resolved every blend
against it — `rgba(255,0,0,0.5)` read back as `[128,0,0,255]` instead of `[255,0,0,255]` — and the
degraded surface was cached and reused for later exports.

**`ctx.saveLayer()` was discarded by any transform or clip inside it.** The recorder rebuilds the
recording canvas's save stack from a fixed depth whenever the matrix or clip changes, and knew
nothing about layer frames, so the layer was composited while still empty and everything after it
landed at full alpha. The stack floor now moves with open layers.

**Paragraph decorations were drawn in transparent ink.** An underline or line-through set through
`ParagraphBuilder` rendered nothing unless `decorationColor` was also passed: the text color goes in
as a foreground _paint_, leaving `TextStyle::color` at its default, and Skia defaults the decoration
color to transparent. It now falls back to the text color, as CSS does.

**A registered font answered every lookup** _(this is the crate fix — see below)_.

### `imageSmoothingQuality = "high"`

Was Mitchell bicubic for every draw, which matches no engine. A cubic resampler makes Skia ignore the
mipmap chain, so heavy minification aliased where upstream's trilinear `high` did not.

There is no specification to appeal to — the HTML spec declines to mandate an algorithm, and Firefox
does not implement the property at all. Chrome's mapping is scale-aware: Mitchell only for a strict
upscale, trilinear otherwise, decided from the full local-to-device matrix so the canvas transform
counts. Ported directly.

| zone plate, 512 → 64        | roughness |
| --------------------------- | --------- |
| upstream (trilinear)        | 65.46     |
| Mitchell everywhere (4.1.0) | 76.22     |
| this release                | 65.44     |

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

One fix reaches the Rust API. `TextEngine::new` passed no default _family_ to the font collection,
and Skia's `defaultFallback()` needs a name to resolve — without one, an unmatched lookup falls
through to the asset provider. So once a `FontManager` had any typeface registered, that typeface
answered every query, including one naming an unknown family and one naming no family at all:

| `layout_text("Studio", 24px)`    | before | after |
| -------------------------------- | ------ | ----- |
| system fonts, no family          | 68.05  | 68.05 |
| `FontManager`, registered family | 55.61  | 55.61 |
| `FontManager`, unknown family    | 55.61  | 68.05 |
| `FontManager`, no family         | 55.61  | 68.05 |

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

[v5.7.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.6...v5.7.0
[v5.6.6]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.5...v5.6.6
[v5.6.5]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.4...v5.6.5
[v5.6.4]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.3...v5.6.4
[v5.6.3]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.2...v5.6.3
[v5.6.2]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.1...v5.6.2
[v5.6.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.6.0...v5.6.1
[v5.6.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.5.1...v5.6.0
[v5.5.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.5.0...v5.5.1
[v5.5.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.4.0...v5.5.0
[v5.3.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.2.0...v5.3.0
[v5.2.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.1.0...v5.2.0
[v5.1.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.0.0...v5.1.0
[v5.0.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v4.1.1...v5.0.0
[v4.1.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/v4.1.0...v4.1.1
[v4.0.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.7.0...v4.0.0
[v3.6.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v3.5.2...v3.6.0
[v5.9.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.8.0...v5.9.0
[v0.15.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.14.0...rust-v0.15.0
[v5.8.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/v5.7.0...v5.8.0
[v0.14.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.13.0...rust-v0.14.0
[v0.12.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.11.0...rust-v0.12.0
[v0.10.6]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.5...rust-v0.10.6
[v0.10.5]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.4...rust-v0.10.5
[v0.10.4]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.3...rust-v0.10.4
[v0.10.3]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.2...rust-v0.10.3
[v0.10.2]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.1...rust-v0.10.2
[v0.10.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.10.0...rust-v0.10.1
[v0.10.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.9.1...rust-v0.10.0
[v0.9.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.9.0...rust-v0.9.1
[v0.9.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.8.0...rust-v0.9.0
[v0.8.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.7.0...rust-v0.8.0
[v0.7.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.6.0...rust-v0.7.0
[v0.6.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.5.0...rust-v0.6.0
[v0.4.0]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.3.1...rust-v0.4.0
[v0.3.1]: https://github.com/l7aromeo/meo-skia-canvas/compare/rust-v0.3.0...rust-v0.3.1
[v0.3.0]: https://github.com/l7aromeo/meo-skia-canvas/releases/tag/rust-v0.3.0
[v0.2.0]: https://docs.rs/meo-skia-canvas/0.2.0
[crates.io 0.1.0]: https://docs.rs/skia-canvas/0.1.0
