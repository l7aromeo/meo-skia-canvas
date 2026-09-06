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

**The version is not yet decided.** Three entries below break the public API,
so this is not a patch.

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
  anything `width` did not: measuring `" H"` gave `0.000 / 24.000` -- left at
  the origin, right at the advance. They now report the outline, `-8.555 /
22.219`. Breaking and **silent**: the old values were finite numbers of a
  plausible size, so code reading them got an answer and no reason to doubt
  it.

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

### Added

- **`Affine::inverse` and `Affine::multiply`.** A Rust caller could not invert
  or compose a transform without reaching for `skia_safe`, which the crate's
  own API guarantee forbids surfacing.

- **`PixelColorSpace::as_str`**, absent at `rust-v0.15.0`.

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

  Three measured through `measure_text`, which is where a caller meets them:

        a control character truncated the run
          "A\u{b}B C D"      16.010  ->  86.680     16.010 is "A" alone
        kerning applied across a space
          "A V"               37.370  ->  38.680     "AV" 30.250 unchanged
        letter_spacing added n-1 units
          four glyphs at 10   30.000  ->  40.000

  The first is the worst of them: VT, FF, U+2028 and U+2029 each discarded
  everything after the control character, so the crate returned a measurement
  for a string the caller never passed.

        max_width ink   1048  ->  2197        unconstrained 4134

  The released crate wrapped at the width and discarded everything past the
  first line, so most of the run was not drawn.

  The ink box is taken from the outline rather than from the rasterised box,
  which was outward-rounded and padded a pixel a side. `both_surfaces_measure_
the_same_lines` pins crate line widths against the JavaScript surface and
  this release moved those numbers, which is a before-and-after of
  crate-rendered output sitting in the repository:

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

### Fixed

- **A cropped readback at a density other than 1 could report no
  intersection** with a region it covered. The page bounds are scaled into
  device space before the test.

- **`get_image_data` spans the last column of a maximum-width canvas.** Six
  pixels requested at x=16777213 on a canvas 16777220 wide came back 7x1 and
  now come back 6x1.

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

### Coverage this release did not add

Both colour changes above -- the D50 adaptation and the Rec. 2020 conversion
-- are silent pixel changes reachable from `set_fill_style_css`, and
`tests/native_context2d.rs` gained no test for either. It gained eight tests
in this release and not one is about colour; no line using
`set_fill_style_css` was added at all. A crate consumer's colour regression
would have to be caught by the JavaScript suite.

Two more sit on the crate's own side. `is_point_in_path` under a transform
has nothing pinning it -- both existing hit-test cases run at identity and
pass identically on either version -- and it is the change here most likely to
break a caller. `Context2D::round_rect`'s start corner is untested at the
entry point that changed; the test that exists pins `PathBuilder::round_rect`,
which already started at 0.

Three of the text corrections are in the same position: `set_letter_spacing`
counting `n` units rather than `n - 1`, `text_align` counting the trailing
letter-space, and kerning suppressed across a word boundary. The crate has
`letter_spacing` tests, but they pin the accessor and the `em` parsing rather
than the count -- a refactor that moved it would pass every Rust test in the
tree.

### Not a crate change

Recorded because each was checked and the answer was no, and because the npm
changelog carries an entry that a reader might expect to find here.

- **`measureText` reporting `height`** is npm-only. The crate's `TextMetrics`
  carried the field at `rust-v0.15.0` and reports the same value on both.
- **The gradient that paints nothing, the numeric style-code refusals, and the
  declaration corrections** are all binding-side. A crate gradient is a
  `Shader`, which refuses fewer than two stops and never reports itself
  opaque.
