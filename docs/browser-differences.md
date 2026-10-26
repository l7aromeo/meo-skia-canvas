# Where output differs from a browser, or extends it

Everything here is intentional or inherited from Skia. If a differential run
against a browser flags one of these, it is not a regression -- read this first.

**Inherited from Skia, and not fixable here.** Glyph antialiasing differs by a
few percent of pixels, confined to stem edges; metrics are unchanged. A contour
of a single `moveTo` no longer survives, so `Path2D.d`, `.edges` and `.bounds`
lose the orphan point -- no pixel effect, since a move-only contour paints
nothing. On the GPU engine, a translucent colour can be stored one
premultiplied step away from where the raster engine and Chrome's software
canvas store it, because Skia's GPU rasteriser rounds the premultiply
differently: `color(display-p3 1 0 0 / 0.5)` stores red 128 and reads back as
255, where the raster engine and Chrome store 127 and read 253, and
`color(display-p3 1 0 0 / 0.1)` stores alpha 25 and reads 255, 0, 0, 25,
against 26 and 245, 0, 0, 26. The read itself converts exactly on both
engines; the step is in what was drawn.

`actualBoundingBoxAscent` and `actualBoundingBoxDescent` differ from Chrome
because `SkParagraph` snaps a run's baseline to a whole pixel and Chrome does
not. **The reported box is the box that is painted**, which is why it is not
corrected here: `Paragraph::paint` draws from the same snapped value. Measured
by rendering one glyph three ways -- through `Paragraph::paint`, through
`draw_str` at the exact baseline, and through `draw_str` at the snapped one --
and counting differing pixels:

        size   |para - exact|   |para - snapped|
         331            15678                 0
         480            69693                 0

At 200px and below both baselines rasterise identically, so a test at those
sizes cannot tell the two apart and is not evidence either way.

The rounding is `SkParagraph`'s and happens before any binding code runs: at
0.153.3 both `Paragraph::alphabetic_baseline` and `Paragraph::origin` return a
C++ struct field and do no arithmetic. **The property to hold on to is that
`alphabetic_baseline` is linear in the font size, at 0.770020 per pixel, while
the origin it is compared against is not.** No single residue describes it --
the gap is `-0.240` at 12px and `+0.039` at 48px, inverting below about 24px --
so a figure quoted for one size reads as a constant and is wrong at every
other.

Reporting the exact baseline instead would agree with Chrome and describe ink
that is not drawn: only `measureText` would move, `outlineText` would not, and
`both routes to the ink agree` in `tests/suite/text.test.js` would fail.
Fixing it here means not painting through `Paragraph::paint`. Tracked as #142.

**Deliberate, and worth keeping.**

- _A negative advance's ink box is the ink._ With a letter or word spacing
  that makes a run's advance negative, Chrome's `actualBoundingBoxLeft`
  reaches back to where the advance ends: Blink's `plain_text_node.cc` moves
  the left edge of the run's box out to the smaller of the advance and the
  ink's width, to cover glyphs a negative advance carries left without
  measuring each. Chrome 152 at 40px Oswald reports `a` at -50px with a left
  of 32.96, and a lone space, which has no ink, with 0.84. That box is wider
  than the text, so it is not the ink box the property names; this library
  reports the glyphs' own ink, -1.68 for that `a`, its ink beginning 1.68
  right of the origin, and 0 for the space. The right edge and the width
  agree with Chrome's.
- _`imageSmoothingQuality = "high"` picks its sampler from the device-space
  scale._ The Chrome mapping this ports, and why one unconditional cubic is
  wrong twice over, are documented on `ScalingOperation` and
  `SamplingFilter::sampling_for` in `src/node/filter.rs`.
- _Solid colours keep float alpha_ rather than being truncated to `u8` before
  painting, so `globalAlpha = 0.5` yields an alpha byte of 128 where truncation
  gives 127.
- _`simplify()` and `unwind()` do not mutate the receiver's fill type_, which
  would change later `contains()` answers.
- _`globalCompositeOperation` takes three operators the standard does not
  list_ -- `"clear"`, `"destination"` and `"modulate"`. All three are real and
  distinct, the standard's own rule is that an unlisted value is ignored, and
  the declared type separates them so a caller knows which half they are using.
- _`saveLayer` composites one 8-bit step darker_ than the equivalent
  `globalAlpha` fill, because Skia rasterises the layer to 8 bits before
  blending. Exact at 0 and 1.
- _`lh`, `rlh`, the viewport units and the container units read as zero_ in
  `ctx.filter`, `letterSpacing`, `wordSpacing` and a `font` size, so `blur(1vw)`
  is taken and draws no blur at all, and `2vw Oswald` is a 0px font. There is no
  line box, window or container here to measure. Chrome 152 gives zero too in
  `letterSpacing` everywhere and in a worker's `OffscreenCanvas`, the context
  closest to this library, but on a canvas in a document it resolves `blur(1vw)`
  against the window -- 8px at 800 wide -- `blur(1lh)` to 12px, and `1lh Oswald`
  to 13px. The list is `ZERO_LENGTH_UNITS` in `src/css.rs`.
- _A filter's root units resolve against a 16px root set in `serif`._ Chrome
  resolves `rem`, `rex`, `rch`, `rcap` and `ric` in `ctx.filter` against the
  document root -- 16px Times unless the page says otherwise -- and in
  `letterSpacing` against the context font, and so does this library, with
  no document: `blur(1rem)` and `blur(1ric)` are 16px on both. `rex`, `rch`
  and `rcap` follow the face the generic `serif` family resolves to here,
  which is Times where the system's `serif` is Times and something else where
  it is not. `State::filter_units` in `src/context/mod.rs` says so at the
  site.
- _A font size's relative units resolve against 10px `serif`._ Chrome reads
  `1ex Oswald` as 4.4873px and `2ch Oswald` as 10px: the x-height and zero
  of its standard font, 10px Times, whatever family is named, with `rem`
  and the other root forms of 16px Times. This library uses the face its
  `serif` resolves to, which is Times where the system's `serif` is Times.
- _`oblique <angle>` draws as CSS Fonts 4 matches it, and Chrome does not._
  Section 5.2, "Matching font styles", takes an oblique face leaning the
  angle's way first, sets a `slnt` axis to the angle where the family has
  one -- "For variable fonts with a slnt axis, a match is created by setting
  the slnt value with the specified oblique value" -- and otherwise puts "a
  fallback match ... produced by geometric shearing to the specified oblique
  value" ahead of any italic. So here `oblique 10deg` leans an upright face
  by `tan(10deg)` and `oblique -14deg` leans it back by `tan(14deg)`, a
  `slnt` axis is set as far as it goes, and an italic face is drawn only for
  a family with no upright face to shear. At 90deg either way a sheared
  glyph lies along its baseline and draws nothing, the limit it nears as
  the angle grows. `resolve_for_oblique` and `run_shear` in
  `src/node/typography.rs` are the match, and quote the section. A range of
  a `Paragraph` asking for oblique is matched and sheared the same way, at
  14deg, and draws what canvas text in `oblique` draws -- `src/node/oblique.rs`.
  Slope of an `I` stem at 200px, measured here on macOS:

  | This library               | `italic` | 0.5deg  | 10deg   | 13.9deg | 14deg   | 30deg   | -14deg | -30deg |
  | -------------------------- | -------- | ------- | ------- | ------- | ------- | ------- | ------ | ------ |
  | `-tan(angle)`              |          | -0.0087 | -0.1763 | -0.2475 | -0.2493 | -0.5774 | 0.2493 | 0.5774 |
  | Oswald (no italic)         | -0.2498  | -0.0083 | -0.1766 | -0.2467 | -0.2501 | -0.5775 | 0.2501 | 0.5777 |
  | Raleway with its italic    | -0.2137  | -0.0049 | -0.1767 | -0.2469 | -0.2502 | -0.5776 | 0.2502 | 0.5772 |
  | Recursive, `slnt` -15 to 0 | -0.2501  | -0.0144 | -0.1796 | -0.2477 | -0.2499 | -0.2499 | 0.2466 | 0.5776 |
  | Recursive, a 12deg oblique | -0.2141  | -0.2141 | -0.2141 | -0.2141 | -0.2141 | -0.2141 | 0.2466 | 0.5776 |

  The last two rows are fixtures made from Recursive, in
  tests/assets/fonts/Recursive: its `slnt` axis, and a static upright with
  a static `slnt` -12 instance flagged oblique. A family with both an italic
  and an oblique face would take the oblique; none was available to pin.

  Chrome draws neither the shear nor the angle. For a font loaded as a
  `FontFace` -- Oswald, Raleway with its italic face, and Arial, Georgia,
  Impact, Liberation, FreeSans and DejaVu loaded from their files -- an angle
  of 14deg or more takes the family's italic or oblique face, or where it has
  neither a synthesised skew of a quarter whatever the angle, sloping an `I`
  by -0.2504; an angle below 14deg, or a negative one, draws upright. On
  Oswald and Raleway that held at 0.5, 10, 10.9, 11, 12, 13, 13.75 and 13.9deg
  upright, at 14, 14.1, 20, 30 and 90deg, `0.25turn` and
  `calc(10deg + 4deg)` slanted, and at every negative angle measured
  upright, in a canvas and in CSS text alike. A system family follows the
  platform's font matcher instead, by a different rule on each platform:

  | Chrome 152, macOS (CoreText)    | `italic` | 0.5deg  | 10deg   | 13.9deg | 14deg   | 30deg   | -14deg  | -30deg  |
  | ------------------------------- | -------- | ------- | ------- | ------- | ------- | ------- | ------- | ------- |
  | Arial, system                   | -0.2096  | -0.2096 | -0.2096 | -0.2096 | -0.2096 | -0.2096 | -0.2096 | -0.2096 |
  | Arial, `FontFace`               | -0.2096  | 0       | 0       | 0       | -0.2096 | -0.2096 | 0       | 0       |
  | Georgia, system                 | -0.2301  | -0.2301 | -0.2301 | -0.2301 | -0.2301 | -0.2301 | -0.2301 | -0.2301 |
  | Impact, system (no italic face) | -0.2501  | -0.2501 | -0.2501 | -0.2501 | -0.2501 | -0.2501 | -0.2501 | -0.2501 |
  | Impact, `FontFace`              | -0.2501  | 0       | 0       | 0       | -0.2501 | -0.2501 | 0       | 0       |

  | Chrome 156, Linux (fontconfig)     | `italic` | 0.5deg  | 10deg   | 13.9deg | 14deg   | 30deg   | -14deg | -30deg |
  | ---------------------------------- | -------- | ------- | ------- | ------- | ------- | ------- | ------ | ------ |
  | Liberation Sans, system            | -0.1947  | -0.1947 | -0.1947 | -0.1947 | -0.1947 | -0.1947 | 0      | 0      |
  | Liberation Sans, `FontFace`        | -0.1947  | 0       | 0       | 0       | -0.1947 | -0.1947 | 0      | 0      |
  | FreeSans, system (an oblique face) | -0.2124  | -0.2124 | -0.2124 | -0.2124 | -0.2124 | -0.2124 | 0      | 0      |
  | DejaVu Sans, system (no italic)    | -0.2501  | 0       | 0       | 0       | -0.2501 | 0       | 0      | 0      |
  | DejaVu Sans, `FontFace`            | -0.2501  | 0       | 0       | 0       | -0.2501 | -0.2501 | 0      | 0      |

  So on macOS a system family slants at every angle but zero, negative ones
  included, with its italic face or a synthesised slant. On Linux one with
  an italic or oblique face takes it at every positive angle and draws
  upright at a negative one, and one with neither synthesises at 14deg
  alone: DejaVu Sans and Serif slant at 14, 14.01 and 14.1deg and draw
  upright at 13.99, 15, 20, 25, 45, 60 and 90deg. All of Chrome's figures are
  from Chrome 152 on macOS and Chrome 156 on Linux, by the slope of an `I`
  stem at 200px and by text widths.

  None of these is the standard's answer, and each substitutes a face or a
  fixed skew for the angle asked for. Chrome's fixed skew is also not 14deg:
  a quarter is `atan(1/4)`, 14.036deg.

- _Text decorations sit where the font's tables put them, and a line-through
  follows the `OS/2` stroke where Chrome does not._ HTML gives a canvas no
  decorations, so the comparison is with CSS text in Chrome 153, `from-font`
  and skip-ink `none`. An underline's top is `post.underlinePosition`, "the
  top of the underline", growing down, which is CSS Text Decoration 4
  section 2.8.1's "extending its thickness in the positive direction only";
  an overline stands on the ascent and grows up. The ascent is the one Skia
  reports for the face: on FreeType the `OS/2` typographic ascender where
  the face sets `USE_TYPO_METRICS` and `hhea`'s otherwise, on macOS
  `CTFontGetAscent`, and on Windows DirectWrite's `DWRITE_FONT_METRICS`.
  Oswald, measured here, sets the flag and gives both tables the same
  ascender, so every platform agrees on it. Chrome agrees on
  both, to its whole-pixel snapping: 96px Oswald underlines rows 10 to 14
  below the baseline against a table top of 9.6 and a thickness of 4.8. A
  line-through is the `OS/2` strikeout stroke, `yStrikeoutPosition` being
  "the top of the strikeout stroke". Chrome draws it about a third of the
  ascent up whatever the table says: 96px Oswald, whose stroke is 33.3 to
  28.5px above the baseline, crosses at 41 to 37, and 96px DejaVu Sans,
  24.8 to 20.1, at 32 to 28. A thickness other than the font's grows the
  stroke about its centre, which Chrome does about its own: Oswald at 1px
  and 8px centres on 38.5 and 38. `node::decorate` places the lines.

  One position and thickness for a box, from its first available font, as
  section 2.9 asks; the first available font is CSS Fonts 4's, "the first
  font for which the character U+0020 (space) is not excluded by a
  unicode-range", and nothing here has a `unicode-range`, so it is the
  first family there is, whether or not it has a space. Chrome agrees: with
  a face holding only `H`, whose underline is 18.5 to 25.4px down at 96px,
  first and Oswald second, Chrome underlines Oswald's spaces on rows 19 to
  24, the first face's, and Oswald alone on rows 10 to 13. Skia's own
  decorations took each run's font, so a glyph from a fallback face moved
  the line.

  Skip-ink is `none`: a line runs through descenders, where Chrome's CSS
  default, `auto`, breaks it around them.

- _A double line's gap is its thickness, where Chrome's is one pixel._ The
  lines are `[top, top + t]` and `[top + 2t, top + 3t]`, the second further
  out the way the line grows, and for a line-through below the first. Chrome
  153 leaves a fixed 1px: a 4px double underline on 96px Oswald covers 10 to
  14 and 15 to 19 below the baseline, an 8px one 10 to 18 and 19 to 27. A
  fixed pixel does not scale with the text or a transform, and at 8px two
  lines one pixel apart read as one. Dotted and dashed match Chrome's
  pattern: a dot `t` long and a gap of `t`, a dash `2t` and a gap of `t`,
  from the box's left edge.

- _A text shadow stays beneath every glyph it reaches -- its own span's,
  the spans before it on its line, and the lines above -- where Chrome paints
  a later span's or line's shadow over them._ CSS Text Decoration 3 section 4
  puts an element's shadow "at a stack level between the element's border
  and/or background (if present) and the elements text and text
  decoration", says shadows "may thus overlay each other, but they never
  overlay the text itself", and adds "UAs should avoid painting text shadows
  over text in adjacent elements belonging to the same stack level and
  stacking context." It calls the exact stacking UA-defined, but its
  preference is plain, so a `Paragraph` paints every line's shadows before
  any line's glyphs. Chrome does not, on a line or across lines:
  - Chrome 152, `H<span style="text-shadow:-200px 0 red">H</span>` at 400px
    Helvetica: the second H's shadow covers the first H's right stem, its
    crossbar red across the black.
  - Chrome 156, "HHH HHH" in 48px Oswald wrapped onto two lines with a red
    shadow cast up as far as the lines are apart: every black pixel of the
    first line is under the second line's shadow.

  Here the solid pixels of those glyphs are drawn as they are with no shadow
  at all. Within one span the order is section 5.1's, bottommost first:
  shadows, underlines and overlines, glyphs, line-through. The lines and
  glyphs are still painted a line box at a time, as CSS 2.1 Appendix E
  paints them, so a wrapped line's overline lies over the descenders of the
  line above, as in Chrome; only the shadows are taken out of that order.
  Pinned in tests/suite/paragraph.test.js, "a text shadow's stack level" and
  "a wrapped line's shadow".

- _A wavy line keeps Chrome's proportions without its pixel constants, and
  hangs from the font's top._ Chrome 153's wave on a 96px Oswald underline,
  read at four times the size in quarter pixels -- the period from the
  centreline's autocorrelation, the swing its range, the band the extreme
  inked rows below the baseline:

  | t   | period | swing  | band          | band height |
  | --- | ------ | ------ | ------------- | ----------- |
  | 2   | 8.747  | 3.585  | 8.50 to 14.25 | 5.75        |
  | 4   | 16.751 | 7.105  | 7.75 to 19.00 | 11.25       |
  | 8   | 32.747 | 14.079 | 6.25 to 28.50 | 22.25       |
  | 16  | 64.864 | 27.925 | 3.25 to 47.50 | 44.25       |

  The band height is `2.75t + 0.25` at all four, exactly; the period
  `4t + 0.75`, within 0.003 to t = 8 and 0.114 high at 16; the swing
  `1.75t`, within 0.11, with the band's `swing + t` saying `1.75t + 0.25`.
  The wave here is a stroke `t` wide, period `4t`, swing `1.75t`, in a band
  `2.75t` high, which differs from Chrome's in three ways. The band starts at
  the font's underline top, as a solid line does, where Chrome's starts
  `0.375t + 0.35` above it -- 1.10, 1.85, 3.35 and 6.35px over the measured
  four. And the period and the band drop Chrome's 0.75px and 0.25px, so that
  the figure is the same at every size and under every transform: the period
  is 0.75px shorter than Chrome's, 8.6 per cent at t = 2 and 1.2 at t = 16,
  and the band 0.25px lower.

- _A transform angle past the `float` range is not matched._ Chrome 152's
  `DOMMatrix` reads `rotate(1e38deg)`, `rotate(1e300deg)` and
  `rotate(calc(infinity * 1deg))` as no rotation, and `skewX()` of any of
  them as a shear of 3.964681007867002e-6. Reducing the largest `float` by
  whole turns gives exactly no rotation and `1e38deg` does not, so no
  reading of the angle explains both functions, and neither answer is a
  rotation anyone could have meant. Here the angle is held to the `float`
  range and used as it stands. `TransformArguments::angle` in `src/css.rs`
  says so at the site, and tests/assets/cases/css-transforms.json leaves
  those rows out.
- _A filter's `em` follows the context font in a worker too._ Chrome 152's
  worker `OffscreenCanvas` resolves `blur(1em)` to 16px whatever the font,
  while it resolves `blur(1ex)` against the font and a canvas in a document
  resolves both against it. The document behaviour is the consistent one and
  is what this library does everywhere.
- _`ex` reads the face's x-height metric first._ CSS Values 4 prefers the
  metric, and Chrome reads it on every platform but one: on macOS Blink
  measures the `x` glyph's ink instead, so a face whose `x` overshoots its
  declared x-height measures larger there -- `1ex` of 40px `sans-serif` is
  20.918px in Chrome on a Mac. Faces whose metric and glyph agree, as
  Oswald's do at 578/1000, read the same in both. The order is
  `x_height_ratio` in `src/image.rs`.
- _`ex` is half an em on a face with neither an x-height nor an `x`._ CSS
  Values 4 says to assume 0.5em "in the cases where it is impossible or
  impractical to determine the x-height", and a face that states no
  `sxHeight` and has no `x` glyph is that case. Skia reports a figure for it
  anyway and Chrome takes it: Chrome 152 on macOS sets `1ex` of Oswald with
  no `sxHeight` and its `x` unmapped at 0.698em, CoreText's own figure, and
  Skia's FreeType port reports that face's ascent, 1.193em, where Chrome on
  Linux was not measured. `declares_x_height` in `src/image.rs` is the check.

- _A system font keyword in `ctx.font` is ignored_ -- `caption`, `icon`,
  `menu`, `message-box`, `small-caption` and `status-bar`, which Chrome takes.
  What each names is the platform's interface font: Chrome 152 on macOS read
  every one as `16px Arial`, which no other platform has to agree with and no
  test here could pin. The crate's `Font::parse` refuses them for the same
  reason.
- _`ctx.font` and `Context2D::font()` read back in the specification's form,
  not Chrome's._ HTML says the getter returns "the serialized form of the
  current font of the context (with no 'line-height' component)", CSSOM 6.7.2
  puts a shorthand's components in "the canonical order of component values
  as given in the property definition table" -- for `font`, CSS Fonts 4
  section 2.7's grammar: style, `small-caps`, weight, width, size, family --
  a `<number>` "in the shortest form possible ... rounding the value if
  necessary to not produce more than 6 decimals", and a family name as "The
  string serialized as a string". Chrome 152 differs in each: it writes a
  weight of 700 as `bold`, leaves a width out (reporting it through
  `fontStretch` alone), reads `oblique` as `italic` and an `oblique` angle
  other than 14deg as no style at all, writes `small-caps` after the weight,
  keeps six significant digits (`2cm` is `75.5906px`, here `75.59055px`), and
  quotes a family only where it is not a single identifier. So
  `italic small-caps 300 condensed 16px Arial` reads
  `italic 300 small-caps 16px Arial` in Chrome and
  `italic small-caps 300 condensed 16px "Arial"` here.
  `tests/assets/cases/css-fonts.json` holds both, the specification's form as `font` and Chrome's as `chrome`.
- _`letterSpacing` and `wordSpacing` take `"0"`_, and any other zero
  written without a unit -- `0.0`, `-0`, `00`, `0e5` -- as no spacing,
  reading back as `0px`, where Chrome 152 ignored each of `0`, `-0`, `+0`
  and `0.0`. CSS Values 4 section 6: "For zero lengths the unit identifier
  is optional (i.e. can be syntactically represented as the <number> 0)."
  Whether a number is zero is read from its digits, so `1e-50`, which a
  32-bit float holds as zero, is still refused. The crate's
  `set_letter_spacing_css` and `set_word_spacing_css` take the same zeros.

**Gradient stops interpolate in sRGB, and Chrome does not. Ours is the
specification's answer.** The HTML Standard, section 4.12.5.1.10 "Fill and
stroke styles", requires that "the colors and the alpha component must be
linearly interpolated in the context's color space without premultiplying the
alpha value" -- which for a default canvas is sRGB. Chrome interpolates in
Oklab whenever a stop is written in `lab()`, `lch()`, `oklab()`, `oklch()` or
`color()`. Measured on Chrome 148, red to blue, midpoint of 101 pixels:

        rgb(255 0 0)      -> rgb(0 0 255)         127,0,127
        color(srgb 1 0 0) -> color(srgb 0 0 1)    140,83,162

The two rows are the same colour in two spellings, so the spelling is choosing
the interpolation space and not the value. `oklch()` and `lab()` spellings of
the same endpoints give 140,83,162 as well, and the Oklab midpoint of red and
blue computes to exactly 140,83,162 against 128,0,128 for sRGB -- so the space
is identified rather than guessed. This is the largest pixel
delta against Chrome anywhere in the library, the endpoints agree exactly, and
copying Chrome here would move away from the standard.

Chrome is ahead of the specification rather than wrong. Adding an interpolation
colour space to canvas gradients is an open proposal, not a requirement:
whatwg/html issues 7947, 8296 and 9911 all propose a
`colorInterpolationMethod` and none has landed. If one does, the default is
the only thing that would move: **this library already offers the choice.**
`GradientColorSpace` carries `Srgb`, `SrgbLinear`, `Lab`, `Oklab`, `Lch`,
`Oklch`, `Hsl` and `Hwb`, and `HueMethod` all four hue directions, on both
channels since `rust-v0.15.0` and `v5.9.0`. So the divergence is narrower than
it looks: we require the space to be asked for, and Chrome infers it from how
the stop was spelled. Inferring from spelling is the part the proposal is not
standardising.

All eight, and all four hue methods, are pinned in
`tests/gradient_interpolation.rs` against a table computed from the CSS Color 4
formulae and measured in Chrome through `color-mix()`, both derived before the
implementation was read. Every row matches exactly rather than within
tolerance.

**Every row agrees, and the one that used to disagree was an artefact of the
test's own endpoints.** Red to blue puts the sRGB midpoint on exactly 127.5,
which Chrome rounds down and Skia rounds up -- and which macOS and Linux also
round differently, so the same value failed CI while passing locally. The
endpoints now avoid a tie in every space and both sides read `125, 1, 127`.
A pinned byte whose unquantised value lands on `x.5` is a coin flip; the
property to hold is that no expected value sits on a boundary, not that all
sit far from one.

**The second half of that sentence is a requirement in its own right**, and the
one that separates a canvas gradient from a CSS gradient: CSS Images
interpolates in premultiplied space and canvas must not. `Shader` passes
`InPremul::No` with unpremultiplied `Color4f`, and
`a_gradient_fading_to_transparent_carries_its_colour_down` pins it -- a stop
fading out travels toward the next stop's colour rather than holding its hue,
which is what premultiplying would produce.

This is the clearest case of
[the rule AGENTS.md opens with](../AGENTS.md#the-rule-that-decides-everything)
choosing correctness over the baseline, and the reasoning is written out because that is
what makes it a decision rather than a bug.

**A relative `lch()` or `oklch()` over a neutral origin reads a hue of zero,
and Chrome reads one off its rounding. Ours is the specification's answer.**
CSS Color 4 makes the hue powerless near the neutral axis -- section 9.3:
"If the chroma of an LCH color is 0%, the hue component is powerless", with
the threshold its table gives as `C <= 0.0015`, and section 9.4 the same for
OkLCh at `C <= 0.000004` -- and a relative colour reads a powerless channel
of its origin as zero. White and grey have no chroma at all: the
specification's matrices take sRGB white to Lab and Oklab `a` and `b` within
1e-15 of zero. Chrome 153 reads a hue anyway. Measured on a Display P3 canvas
read back in P3:

        lch(from white 50 100 h)                     Chrome 231,0,131   here 233,0,123
        lch(from gray l 100 h)                       Chrome 242,0,141   here 244,0,132
        oklch(from white 0.6 0.3 h)                  Chrome 237,0,28    here 228,0,119
        oklch(from gray l 0.3 h)                     Chrome 237,0,29    here 227,0,119
        lch(from lab(50 0 0.001) l 50 h)             Chrome 133,118,44  here 179,85,121
        oklch(from oklab(0.5 0 0.000003) l 0.1 h)    Chrome 117,97,31   here 135,77,96

Fitting Chrome's own floats against the CSS Color 4 conversion puts its hue
for white and grey alike at about 356.6 degrees in LCH and 23.7 in OkLCh --
`lch(from white l 50 h)` is magenta there and `oklch(from white l 0.2 h)`
salmon. One angle for two different neutrals is the signature of its
arithmetic rather than of either colour: the direction of the rounding error
its matrices leave on the neutral axis. Nothing in the colour gives it, so
it cannot be reproduced without reproducing that arithmetic, and the last
two rows show the threshold itself is absent -- a chroma of 0.001 keeps its
quarter-turn hue there. Here the hue is zero at and below each threshold and
the origin's above it, which `a_neutral_origin_has_no_hue` in
`tests/native_api_contract.rs` pins against the absolute colours with that
hue, and `a_hue_is_powerless_at_the_chroma_css_color_4_names_and_no_further`
at the thresholds to the last `f32` step. The conversion is `to_polar` in
`src/node/utils.rs`.

**A colour is painted at the precision it names, whatever its spelling,
and Chrome rounds some spellings to 8 bits. Ours is the specification's
answer.** CSS Color 4 rounds `rgb()` channels to integers only when it
serialises them; parsing keeps the value, and nothing in CSS lets the
spelling of a colour change which colour it is. Chrome 153's canvas paints
some spellings of a legacy colour as 8-bit RGBA and others as floats, by
which of its parsers reads them, and the getter reports 8 bits for all of
them. Measured on a Display P3 canvas read back in P3:

        rgb(127.5 0 0)               Chrome 117,20,12      here 116,20,12
        RGB(127.5 0 0)               Chrome 116,20,12      here 116,20,12
        rgb(50%, 0%, 0%)             Chrome 117,20,12      here 116,20,12
        rgb(50% 0 0)                 Chrome 116,20,12      here 116,20,12
        rgb(127.5 calc(0) 0)         Chrome 116,20,12      here 116,20,12
        rgb(0 255 0 / 0.5)           Chrome 118,251,76,128 here 116,251,76,128
        rgb(0 255 0 / 50%)           Chrome 116,251,76,128 here 116,251,76,128
        rgb(0 255 0 / calc(0.5))     Chrome 116,251,76,128 here 116,251,76,128
        rgb(none 255 0 / 0.5)        Chrome 116,251,76,128 here 116,251,76,128
        hsl(0deg 150% 50%)           Chrome 234,51,35      here 255,18,0
        hsl(0 150 50)                Chrome 255,18,0       here 255,18,0
        hwb(0 0% 50%)                Chrome 116,20,12      here 116,20,12
        #00ff0080                    Chrome 118,251,76,128 here 118,251,76,128

Chrome rounds a lower-case `rgb()` with number channels to 128 and leaves
the same colour written `RGB(` or with percentages at 127.5; it rounds a
number alpha to 128/255 and leaves `50%` and `calc(0.5)` at 0.5; it clips
`hsl()` written with percentages to sRGB and not written with numbers; and a
math function or `none` anywhere, or `hwb()`, keeps the whole colour in
floats. Its getter reports `#800000` for `rgb(127.5 0 0)`, for
`RGB(127.5 0 0)` and for `rgb(127.5 calc(0) 0)` alike, so there a colour can
read back differently from how it paints. Hex is 8-bit by what it writes,
and paints the same here. Following Chrome would mean reproducing which
spellings take its 8-bit path, case included, and a colour whose bytes
depend on how its function name is cased is not one CSS describes. The
difference is a level at most where Chrome rounds, and more where its 8-bit
path clips an `hsl()` outside sRGB that this library keeps.

**Kept until 7.0, and deprecated.**

- _Colour forms no browser reads still paint._ The CSS parser underneath,
  `csscolorparser`, has always accepted them here, and Chrome 153 refuses
  each, keeping the previous fill:
  - `hwba()`, `hsv()` and `hsva()`, `"hwba(0 0% 0%)"` and
    `"hsv(0 100% 100%)"` among them, and their relative forms;
  - hex without its `#`, `"ff0000"`;
  - legacy comma syntax CSS Color 4 does not allow: numbers and percentages
    mixed, `"rgb(255, 0%, 0)"`; separators mixed, `"rgb(255,0 0)"`;
    `hsl()` with bare numbers, `"hsl(120, 100, 50)"`; `none` in a channel,
    `"rgb(none, 0, 0)"`; and commas in a function that has no legacy
    syntax, `"hwb(120, 0%, 0%)"`, `"lab(50, 40, 30)"`,
    `"oklch(0.6, 0.2, 30)"`, `"color(srgb 1, 0, 0)"`.

  npm 7.0 and crate 0.17 refuse them; until then
  `the_deprecated_colour_forms_still_paint` in
  `tests/native_api_contract.rs` holds what each paints. Write `#` before
  hex digits, spaces between channels, and `hwb()` for `hsv()`. A `none`
  alpha in legacy syntax, `rgb(255, 0, 0, none)`, painted transparent and
  is refused already.

  Hex and named colours are read by the library itself, so in 7.0
  `csscolorparser` goes with the forms above.

- _`TextMetrics` carries its values as its own properties._ Chrome 152 puts
  the ten it defines on `TextMetrics.prototype` as enumerable getters and
  gives the object none of its own, so there `Object.keys` is empty,
  `JSON.stringify` gives `"{}"` and a spread copies nothing. Here every
  value, this library's `emHeightAscent`, `emHeightDescent`, `height` and
  `lines` included, is an own property of a frozen object, in the order
  6.0.2 wrote them, so serialising or spreading a measurement still sees
  them all. The prototype, its `Symbol.toStringTag` and the constructor
  that refuses to be called are Chrome's already. In 7.0 the values move to
  the prototype; read them by name and nothing changes.
