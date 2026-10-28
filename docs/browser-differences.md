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
- _`TextMetrics` has a `toJSON`._ HTML's interface defines none, and Chrome
  152 puts its values on `TextMetrics.prototype` as getters with nothing of
  the object's own, so there `JSON.stringify` of a measurement gives `"{}"`.
  The values are on the prototype here too, and `Object.keys` is empty and a
  spread copies nothing, as in Chrome; but `toJSON` returns every value,
  this library's `emHeightAscent`, `emHeightDescent`, `height` and `lines`
  included, in the order 6.x serialised them as own properties, so a
  measurement serialises as it always did. It is WebIDL's default `toJSON`,
  the one `DOMRect` has, and works the ink box out when it is called.
  `structuredClone` of a measurement gives `{}`, a plain object with none of
  the values, where Chrome refuses it with a `DataCloneError`: a class
  instance in Node is cloned as an ordinary object, and nothing here marks
  it as uncloneable.

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
- _A family list nothing answers draws in this library's `sans-serif`,
  where Chrome uses its standard font._ What a list naming no available
  family falls back to is the user agent's choice, and the canvas's own
  default font, `10px sans-serif`, names this one. Chrome 152 on macOS
  measures `Hamburgefonstiv` in `16px NoSuchFamilyAnywhere` at 111.70, as in
  `16px Times` and `16px serif`, in a canvas and in an `OffscreenCanvas`
  alike; here it measures 126.16, as in `16px sans-serif`. A quoted
  `"serif"` with no family of that name is such a list. `new_font_collection`
  in `src/node/font_library.rs` sets the default.
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
  the angle grows; Chrome draws 90deg as it draws 14deg and -90deg as any
  other negative angle, by the rules below. `resolve_for_oblique` and `run_shear` in
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

- _`ctx.direction` reads `"inherit"` on a fresh context._ The Canvas
  standard makes `inherit` the attribute's initial value, and a getter that
  answered `ltr` could not report the state a fresh context is in. Chrome
  152 reads `"ltr"` from a detached canvas's fresh context. Text is laid out
  left to right either way: with no element to inherit a direction from,
  `inherit` resolves to `ltr`.
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
- _A family name, and every other CSS identifier, holds only the code points
  CSS Syntax 3 lists._ Section 4.2 makes a name of ASCII letters, digits,
  `_`, `-` and the "non-ASCII ident code points", a list that leaves out
  U+00A0, U+00AA, U+00B5, U+00D7, U+2028 and U+3000 among others. Chrome 152
  still reads by the rule the list replaced, that every code point at or
  above U+0080 is a name's, so it takes `12px aµb`, `12px a×b`, `12px aªb`,
  `12px serif` followed by U+00A0, read as the family `serif` with that
  space in it, which matches no face and falls back without a word, and
  `12px Times New Roman` with U+00A0 between the words. Here each is refused,
  as the current definition makes it no part of a name; a name of code points
  on the list, `12px a·b` or `12px Café`, is read as Chrome reads it.
  `is_ident_code_point` in `src/css.rs` is the one definition, which the font
  family and math function readers both use.
- _A comment beside a colour keyword is dropped._ CSS Syntax 3 drops a
  comment when it tokenizes, so `red/**/` and `/**/red` are the keyword
  `red`, and here each paints red. Chrome 152 ignores a comment beside a
  keyword -- `red/**/`, `/**/red`, `red /**/`, `transparent/**/`,
  `currentcolor/**/` and `red/*` all leave the fill as it was -- while it
  takes one beside a hex colour or a function, `#f00/**/` and
  `/**/ rgb(255 0 0)`, and anywhere inside a function.
- _`letterSpacing` and `wordSpacing` take whitespace and comments around the
  length._ HTML's setters parse the value "as a CSS <length>", and CSS
  Syntax 3's "parse a component value" says "Discard whitespace from input"
  before the value and after it, its tokenizer having dropped the comments,
  so `" 5px "`, a carriage return or form feed before or after the length,
  `/**/5px` and `5px/**/` are each 5px here. Chrome 152 ignores each of
  them, while it takes `/**/-3px`. Inside the length nothing may come
  between the number and its unit, here as in Chrome: `5 px`, `5/**/px` and
  `-/**/3px` are refused.
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

**`color(rec2020 ...)` decodes by a 2.4 gamma, and Chrome does not. Ours is
the specification's answer.** CSS Color 4 section 10.8 gives `rec2020`'s
transfer function as "gamma 2.40, from [REC_BT.1886]", and its sample code's
`lin_2020` raises each component's magnitude to 2.4 and keeps its sign. The
changes section dates it: since the Candidate Recommendation Draft of 24
April 2025, "Defined rec2020 color space to use display-referred, 2.4
gamma." Drafts before that used BT.2020's piecewise inverse OETF, and Chrome
152 (152.0.7977.167) still does. Measured on an sRGB canvas, fill and
`shadowColor` alike:

        color(rec2020 0.2 0.2 0.2)   Chrome 67,67,67      here 40,40,40
        color(rec2020 0.5 0.5 0.5)   Chrome 139,139,139   here 120,120,120
        color(rec2020 0.2 0.4 0.6)   Chrome 0,120,168     here 0,97,153

Every figure in the right-hand column is the standard's arithmetic, worked
in `f64` from its sample code. A Rec. 2020 canvas stores by the same curve,
so a colour named in `rec2020` reads back from one as the components it was
named with. The colour space this library builds for `rec2020` is a CICP
one, BT.2020 primaries with transfer characteristic 1, which Skia's
`named_transfer_fn::REC709` makes a pure 2.4 gamma, so the colour is handed
to Skia with that tag and nothing converts it on the way. Skia's own
`SkNamedTransferFn::kRec2020` is BT.2020's piecewise curve, and is not
used for it. Pinned in
`a_rec2020_colour_takes_css_color_4s_curve_on_every_canvas` on sRGB,
Display P3 and Rec. 2020 canvases.

A gradient interpolated in `rec2020` mixes by the same 2.4 gamma. Skia's
gradient interpolation space of that name is built from
`SkNamedTransferFn::kRec2020` too, so the gradient is computed in the
CICP space instead. On a `rgb(250 2 0)` to `rgb(0 0 254)` ramp the midpoint
reads 157,18,143 here; Chrome 156 (156.0.8078.48) paints
`color-mix(in rec2020, ...)` of the same pair as 159,19,147, which is what
Skia's own space gives. Pinned in
`a_rec2020_gradient_mixes_by_the_css_gamma` in four geometries, on both
engines, with translucent and out-of-gamut stops.

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

**A colour reads back in the function it was declared in, with the figures
the colour holds. Chrome writes eight bits and six figures.** CSS Color 4
section 16 serialises a colour from its value: hex only for a legacy colour
whose components are whole bytes and whose alpha is 1 (16.2.1), `rgb()`
numbers for any other legacy colour (16.2.2), and `lab()`, `oklch()` or
`color()` in its own function, `none` kept (16.3 to 16.5). Chrome holds a
legacy colour in bytes, so its getter only has bytes to write; this library
holds floats and paints `hwb(90 10% 20%)` with a red of 114.75, so that is
what it reports. Measured against Chrome 156 (156.0.8078.48):

        set                          Chrome                    here
        hwb(90 10% 20%)              #73cc1a                   rgb(114.75, 204, 25.5)
        rgb(127.5 0 0)               #800000                   rgb(127.5, 0, 0)
        #ff000080                    rgba(255, 0, 0, 0.5)      rgba(255, 0, 0, 0.5019608)
        rgb(255 0 0 / 0.123456789)   rgba(255, 0, 0, 0.12)     rgba(255, 0, 0, 0.12345679)
        lab(29.123456789 39 20)      lab(29.1235 39 20)        lab(29.123457 39 20)
        color(display-p3 0.123456789 0.5 0.25)
                                     color(display-p3 0.123457 0.5 0.25)
                                                               color(display-p3 0.12345679 0.5 0.25)

Alpha follows section 16.1.2, the rule for an alpha held as a number, and
not 16.1.1, which writes a byte as the shortest two- or three-place
fraction that maps back to it. `#ff000080` holds 128/255; Chrome's `0.5`
names a different alpha, which an eight-bit canvas cannot tell apart and a
float canvas can.

Six figures cannot restate a colour held in `f32`. 128/255 written to six
places is 0.501961, 1.8e-7 from the `f32` an `RGBAF32` canvas stores, so
setting the read-back again paints a different pixel there. Each number
takes the fewest significant figures that repaint the colour exactly in the
space it was declared in: a short value stays short -- 0.5, 114.75,
`lab(29 39 20)` -- and one the colour holds more of carries more.
`tests/suite/color-serialisation.test.js` pins every row of
`tests/assets/cases/css-color-serialisation.json` by the pixel the
read-back paints on `rgba`, `RGBAF16` and `RGBAF32` canvases. Written to
six figures, nine of those rows repaint differently on `RGBAF32`, two on
`RGBAF16` and none on `rgba`.

A relative colour converted to Lab or OkLCh reads back with Chrome's
channels some way from CSS Color 4's sample code: `lab(from red l a b)` is
`lab(54.29 80.8198 69.8997)` there against 54.290541 80.804928 69.890965
worked in `f64`, and set again it paints red with a green of -6.7e-4 on a
float canvas. Here it reads `lab(54.290541 80.80493 69.89097)`: the sample
code's channels, converted in `f64` and held at `f32`, which repaints the
relative colour exactly.

`Image.currentColor` reads back through the same code, so it reports the
string `fillStyle` would for the same colour, although the SVG is recorded
with its eight-bit equivalent.
