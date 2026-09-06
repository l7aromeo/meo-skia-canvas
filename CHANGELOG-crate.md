# Changelog — the Rust crate

Changes to the crate `meo-skia-canvas`, published on crates.io and versioned
independently of the npm package.

> Sibling files: the npm addon's is [CHANGELOG-npm.md](CHANGELOG-npm.md), and
> [CHANGELOG.md](CHANGELOG.md) is the index and the combined history from
> before the two were separated. The channels number separately and are not
> comparable version for version — the crate starts at `0.2.0`, npm continues
> `phyron-skia-canvas`'s from `3.6.0`.
>
> **A change that affects both surfaces appears in both files**, written for
> each audience rather than copied. `TextDirection` gaining `Inherit` is the
> same commit as `ctx.direction` reporting `"inherit"`, and a Rust reader
> should not have to work that out from a JavaScript property name.
>
> Releases before the split are in [CHANGELOG.md](CHANGELOG.md).

## 📦 ⟩ [UNRELEASED] ⟩ September 7, 2026

**The version is not yet decided.** Seven entries below break, so this is not
a patch. Three of them stop a caller compiling -- `Font::slant`,
`TextDirection` and `Error::InvalidRadius` all sit on types that are not
`#[non_exhaustive]`. The other four change a value or a rendering without any
diagnostic at all, and are marked where they appear.

### Breaking

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

### Added

- **`Affine::inverse` and `Affine::multiply`.** A Rust caller could not invert
  or compose a transform without reaching for `skia_safe`, which the crate's
  own API guarantee forbids surfacing.

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
