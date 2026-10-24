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
- _`oblique <angle>` draws as Chrome draws a `FontFace`, in every family._
  An angle of 14deg or more slants -- the family's italic or oblique face,
  or where it has neither a slant synthesised at one skew whatever the
  angle, the quarter that slopes an `I` by -0.2504 -- and an angle below
  14deg, or a negative one, draws upright. That is Chrome's rule for a font
  loaded as a `FontFace`, in a canvas and in CSS text alike, on Oswald,
  Raleway with its italic face, and Arial, Georgia, Impact, Liberation,
  FreeSans and DejaVu loaded from their files: upright at 0.5, 10 and
  13.9deg and at every negative angle measured, slanted at 14 and 30deg, and
  on Oswald and Raleway also upright at 10.9, 11, 12, 13 and 13.75deg and
  slanted at 14.1, 20 and 90deg, `0.25turn` and `calc(10deg + 4deg)`.
  Measured by the slope of an `I` stem at 200px and by text widths, in
  Chrome 152 on macOS and Chrome 156 on Linux. `OBLIQUE_SLANTS_FROM_DEGREES` in
  `src/context2d.rs` is the threshold.

  Chrome draws a system family by other rules, and by different ones on each
  platform -- they follow the platform's font matcher rather than the
  `FontFace` matching. This library does not reproduce them: a system
  family follows the same rule as a registered one, which was the
  maintainer's choice. Slope of an `I` stem at 200px, 0 upright:

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
  upright at 13.99, 15, 20, 25, 45, 60 and 90deg.

  CSS Fonts 4 is different again. Its section 5.2, "Matching font styles",
  changes at 11deg rather than 14, and where a family has no oblique face it
  shears before it looks for an italic: "a fallback match is produced by
  geometric shearing to the specified oblique value". `oblique 10deg` would
  lean ten degrees and `oblique 20deg` twenty, in Raleway as in Oswald;
  Chrome follows that on no origin.

  An earlier entry here said Chrome selects a family's italic for any angle
  but zero and synthesises a slant only from 14deg up. That was two origins
  read as one rule: the italic half was measured on Arial, a macOS system
  font, and the synthesis half on Oswald, a `FontFace`. No family in Chrome
  behaves that way.

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
