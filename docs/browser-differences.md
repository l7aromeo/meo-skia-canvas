# Where output differs from a browser, or extends it

Everything here is intentional or inherited from Skia. If a differential run
against a browser flags one of these, it is not a regression -- read this first.

**Inherited from Skia, and not fixable here.** Glyph antialiasing differs by a
few percent of pixels, confined to stem edges; metrics are unchanged. A contour
of a single `moveTo` no longer survives, so `Path2D.d`, `.edges` and `.bounds`
lose the orphan point -- no pixel effect, since a move-only contour paints
nothing.

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
- _`oblique <angle>` draws as Chrome draws it, not as CSS Fonts 4 says._ The
  standard shears a family with no oblique face to the angle asked for,
  before it would fall back to the italic. Chrome selects the family's
  italic for any angle but zero, and in a family with no italic synthesises
  a slant only from the default 14deg up, at the same skew whatever the
  angle -- in a canvas and in ordinary CSS text alike, measured on both.
  The standard's wording on synthesis is permissive ("may be synthesized"),
  and Chrome's two paths agree, so this library follows Chrome: the choice
  was the maintainer's, and `parse_font_shorthand` in `src/context2d.rs`
  carries the reasoning.
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
