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

On Linux a variable face's advances move in whole font units between its
masters, where on macOS they follow the interpolation as Chrome on macOS
does. The FreeType that skia-bindings 0.156.0 embeds, 2.14.3, rounds each
`HVAR` advance delta to a whole unit: `tt_var_get_item_delta` in
`src/truetype/ttgxvar.c` returns `ft_round_and_shift16`, which is
`(value + 0x8000) >> 16`, and `tt_hvadvance_adjust` adds it to an integer
advance before any scaling. No load flag or linear advance avoids it, and
neither Skia's scaler nor its HarfBuzz font functions take an advance from
anywhere else. OpenType 1.9.1 rounds the normalised coordinate to 2.14 and
nothing after: "It is recommended that at least 16 fractional bits of
precision be maintained, and that any rounding be done at the last point
before a value is used." So Oswald's "0" at weight 450 and 1000px measures
526 on Linux and 525.52 on macOS, where Chrome 156 on macOS measures 525.516;
Chrome on Linux was not measured. `ch` and `ic` take the advance from the
face's tables and are the interpolation's on every platform. The `UPSTREAM`
note at the width in `Typesetter::measure` carries the re-check.

On Linux a font size is laid out at the 64th of a pixel at or below it. Skia's
FreeType scaler hands the size to `FT_Set_Char_Size` in 26.6 fixed point
through `SkScalarToFDot6`, which truncates (`SkFontHost_FreeType.cpp`), and
corrects its matrix for the size FreeType chose only below 1px, so advances and
outlines come from the truncated size while the font's metrics take the one
asked for. In Oswald, `measureText("Hamburgefonstiv")` at each hundredth from
17.00px to 18.00px steps by 0 at 36 of the 100 and by 0.0956 to 0.0973 at the
rest, where on macOS every step is 0.0617 or 0.0618, and each width is within
0.0005 of macOS's at `floor(size × 64) / 64`, the rest being FreeType's
advances against CoreText's: 17.29px measures 106.69469 on Linux and 17.28125px
106.69444 on macOS; 17.30px measures 106.79041 on Linux and 17.296875px
106.79089 on macOS. Hinting on and off measure alike. Chrome on Linux takes its
glyph advances from the same scaler -- Blink's `SkFontGetGlyphWidthForHarfBuzz`
in `skia_text_metrics.cc` reads them from the Skia strike -- so it is expected
to step the same way; Chrome on Linux was not measured. The `UPSTREAM` note at
the size in `Context2D::set_font` carries the re-check.

A line fits a paragraph up to a hundredth narrower than it. With
SkParagraph's rounding hack off, as every paragraph here is,
`LineBreakerWithLittleRounding` in `TextWrapper.cpp` still floors a line's
width to hundredths before comparing it with the paragraph's width, within a
quarter pixel of it. So "Hamburgefonstiv quick" in 24px Oswald, 200.51942
wide on Linux, fits on one line down to a width of 200.51 rather than
breaking below its own width; on macOS its width is 200.52002, just above the
hundredth, and the two edges coincide. No width given to `layout` undoes a
floor taken on the line's side. The `UPSTREAM` note at `paragraph_style`
carries the re-check.

**Text set at 17.3px is laid out at 17.3px; Chrome 156 lays it out at
17.29px.** Measured on macOS in Oswald, `measureText("Hamburgefonstiv")` at
17.3px is 0.062 narrower in Chrome than here, and narrower by the same at
16.3, 16.8, 17.8, 18.3, 18.8, 19.3, 19.8 and 20.3px. Of the 201 sizes from
12px to 32px in steps of a tenth, those nine are the only ones that differ,
and they are the nine at which the size times 100, multiplied in `f32`, falls
just short of a whole number -- 17.3 × 100 is 1729.9999 -- so that truncating
it gives a hundredth less. The rule `int(f32(size) × 100) / 100` predicts all
nine, and Chrome's widths at the sizes it predicts are this library's widths
scaled to them within 0.0002. Blink's `FontCacheKey`
(`third_party/blink/renderer/platform/fonts/font_cache_key.h`) truncates the
size this way, through an unsigned integer of hundredths, but
`FontPlatformDataCache` creates the font from the untruncated size, so where
Blink applies the truncated size to layout is not identified. It is not
the order in which sizes are asked for: 17.3px measured first in a fresh
profile is already 17.29px's width, and DOM text at 17.3px is too, so it is
not canvas alone. A size names a length; laying text out a hundredth smaller
than asked for is a defect in Chrome, not a rule, and the test "17.3px is a
hundredth wider than 17.29px" holds this library to the size it is given.
On Linux that size is itself laid out at the 64th of a pixel below it,
17.296875px for 17.3px, as the FreeType entry above records, where Chrome
truncates to 17.29px first.

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
0.156.0 both `Paragraph::alphabetic_baseline` and `Paragraph::origin` return a
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

- _Word spacing is added after every word separator, a leading space
  included._ CSS Text 3 section 8.2 applies `word-spacing` to each
  word-separator character, and the canvas keeps every space, so `" a"` at
  -100px word spacing in 40px Oswald measures -73.80. Chrome 156 measures
  26.20 there: it does not space a leading U+0020, though it spaces a leading
  no-break space (-73.80), a trailing space (`"a "` -73.80) and each of a
  doubled one (`"a  b"` -146.76). What it does with a space also depends on
  what it measured before, in one profile: once a text that does not space a
  space has been measured, every later U+0020, in that text and the next,
  goes unspaced -- `" a b "` measures 62.40, unspaced throughout, and
  `"a b"` measured after `" a"` is 44.08 where alone it is -55.92 -- while
  `" a"` measured after `"a b"` is -73.80. Its widths in the tests here are
  each measured alone in a fresh profile, and the leading-space rows carry
  none.
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
  alike; here it measures 120.95, as in `16px sans-serif`. A quoted
  `"serif"` with no family of that name is such a list. `new_font_collection`
  in `src/node/font_library.rs` sets the default. On Linux a family the
  machine lacks but fontconfig binds strongly or as "same" -- Times New Roman
  to Liberation Serif or Tinos, by its metric aliases -- resolves to that
  substitute, and one it binds only weakly, through the generic defaults,
  does not: Skia's fontconfig `matchFamilyStyle` drops weak bindings before
  matching, so in a container holding only DejaVu, `16px Times New Roman`
  draws in this library's `sans-serif` rather than the DejaVu Serif `fc-match`
  names. Chrome on Linux resolves families through the same call and, as
  `FontCache::GetLastResortFallbackFont` in `font_cache_skia.cc` walks its
  list, is expected to land on a sans face there too; that is inferred from
  the code, and Chrome on Linux was not run.
- _A family name matches by Unicode's full case folding, and Chrome folds one
  character to one._ CSS Fonts 4 section 5.1 matches family names by the
  Default Caseless Matching of The Unicode Standard section 3.13, "using the
  case mappings with status field C or F" of `CaseFolding.txt`, without
  normalising and without language tailoring. Chrome 156 matches an
  `@font-face` family `"Straße Σίσυφος Iris Café"` by
  `"STRAßE ΣΊΣΥΦΟΣ IRIS CAFÉ"` and by `"sTRAßE σΊσυφος iRIS cafÉ"`, so it
  folds `Σ`, `ς` and `É`, and a family `"ſerif"` by `"SERIF"`, so it folds
  the long s; but not by `"STRASSE ΣΊΣΥΦΟΣ IRIS CAFÉ"`, which needs the F
  mapping of `ß` to `ss`. Here all of these match. Neither matches a Turkic
  `ı` for `I` or a decomposed `e` and acute for `é`, as the standard has it.
  The table is `src/font/caseless/folding.rs`, generated from Unicode 18.0's
  file.
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
- _A fractional `font-weight` is drawn and read back as written, and Chrome
  holds it to quarters for drawing and to a whole number for reading back._
  CSS Fonts 4 makes the weight a `<number>` from 1 to 1000, and OpenType
  defines `wght` on the same scale, so `512.5 1000px Oswald` instances the
  face at 512.5 and `ctx.font` reads back `512.5 1000px "Oswald"`, a number
  in its shortest form as CSSOM writes one. Chrome 156 draws a weight at the
  quarter below it -- `550.7` draws, and `getComputedStyle` reads, as
  `550.5`, and `999.9` as `999.75` -- and its canvas getter truncates:
  `512.5` reads back `512`, `550.7` `550`. At a quarter the two draw alike:
  Oswald's "0" at 1000px is 535.127 at 512.25 and 535.150 at 512.5 in both,
  against 535.105 at 512. A `Font` holds a whole weight, so `Font::parse` and
  `Font::from_css` keep the whole part and `set_font_css` the fraction too.
  `tests/assets/cases/css-fonts.json` has Chrome's read-back beside ours.
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

A text decoration's own colour, and a `drop-shadow()` filter's, are held
the same way, at the precision they name. A decoration draws the bytes a
fill of its colour draws, which Chrome's canvas cannot be compared on: it
has no `textDecoration`. `tests/named_colour_precision.rs` holds the line to
the fill's bytes on an eight-bit canvas and to the alpha it names on
`RGBAF16` and `RGBAF32`, and the drop-shadow to the fill's bytes on an
eight-bit canvas.

**A draw too faint for eight bits draws on a float canvas, and Chrome drops
it. Ours is the arithmetic's answer.** Skia skips a source-over draw whose
paint alpha rounds to zero as a byte -- `SkPaint::nothingToDraw` tests
`getAlpha()` -- whatever the pixels it would land in, and Chrome draws
through it. On a `float16` canvas, which holds 0.001 to within a
two-thousandth, Chrome 152 (152.0.7977.167) reads back:

        draw                                      Chrome        here
        fillRect in rgba(0, 0, 0, 0.001)          0             0.001
        fillRect under globalAlpha 0.001          0             0.001
        a shadow under globalAlpha 0.001          0.000999      0.001
        a shadow under globalAlpha 0.5            0.5           0.5

So Chrome keeps a faint shadow and drops the faint fill casting it. Nothing
in the Canvas standard lets an alpha below a byte's resolution be treated
as zero on a canvas that stores more than a byte, and every draw here --
fills, strokes, text, images, patterns, gradients, textures, layers and
shadows -- keeps it: `Faint::drawable` hands Skia such a paint opaque with
its alpha in a colour filter, and a shadow takes the paint's alpha in its
colour. An eight-bit canvas keeps Skia's skip and draws what Chrome does.
`tests/faint_alpha.rs` pins each draw on `RGBAF16` and `RGBAF32`, and the
eight-bit canvas to the bytes it drew before.

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

**`color-mix()` takes one colour or more, as CSS Color 5's grammar does, and
mixes by CSS Color 4's sample code; Chrome takes exactly two and strays from
the fifth figure.** The draft writes the function as `color-mix(
<color-interpolation-method>? , [ <color> && <percentage [0,100]>? ]#)`, a
list of one or more mix items. Chrome 156 refuses `color-mix(in srgb, red)`
and `color-mix(in srgb, red, blue, lime)`; here the first is red and the
second the colours mixed in order, a third of each. Two colours read and
paint as they do in Chrome wherever the arithmetic agrees, measured on 92
rows in `tests/assets/cases/css-color-mix.json`.

Where it does not, the figures here are CSS Color 4's sample code worked in
`f64`, held at `f32`, and Chrome's are not:

        color-mix(in oklab, red, blue)
            Chrome  oklab(0.539974 0.0962086 -0.0928316)
            here    oklab(0.5399845 0.09620305 -0.09284095)
            f64     0.53998454 0.09620305 -0.09284094
        color-mix(in display-p3, red, blue)
            Chrome  color(display-p3 0.458734 0.100055 0.549017)
            here    color(display-p3 0.45874378 0.1001434 0.5490743)
            f64     0.45874378 0.10014340 0.54907431
        color-mix(in a98-rgb, red, blue)
            Chrome  color(a98-rgb 0.426364 -0.00755272 0.487894)   paints 128,0,128
            here    color(a98-rgb 0.4292958 0 0.49053438)           paints 129,0,129
            f64     0.42929582 -3.05e-8 0.49053437, 128.52 as sRGB

A98 RGB shares sRGB's red and blue primaries, so the mix has no green; the
sample code's own arithmetic leaves 1e-17 there before A98's pure-power
encode makes it -3e-8, and a matrix product within its own rounding of zero
is zero here. A mix in `rec2020` differs from Chrome by the 2.4 gamma above.
Chrome's `addColorStop` also takes `color-mix(in srgb, red, blue)foo`, which
its `fillStyle` refuses; both refuse it here.

**`AccentColor` and `AccentColorText` are read; Chrome 156 refuses both.**
CSS Color 4 section 6.2 lists them among the system colours, valued by the
user agent like the rest, so a string naming one is a valid colour. Here they
are `#0075ff` and `#ffffff`: Chromium's light-scheme control accent, measured
in a screenshot from Chrome 156.0.8078.49 headless with
`--force-color-profile=srgb`: a checked checkbox, a checked radio button and a
full progress bar fill with `#0075ff`, and the checkbox's mark is white. The
other system colours read as Chrome reads them on a canvas, in its light scheme
on macOS; `tests/assets/cases/css-system-colors.json` has every row.

**A blurred path is the exact Gaussian blur of what it draws, and Chrome's is
not.** Filter Effects Module Level 1 defines `blur()` as a Gaussian blur whose
standard deviation is the length given. A rasterised pixel is already its
shape averaged over the pixel's box, so the blur of what a draw paints is
those pixels convolved with the Gaussian sampled at whole-pixel offsets, and
a flat-colour path that is not a rectangle the transform keeps one, which
the next entry blurs in closed form, is drawn here as exactly that, rounded once at
whatever alpha it is drawn: within half
a level on 8-bit pixels on either engine, and on F16 and F32 to the surface's
precision, as measured on the CPU engine, where a float canvas made for the
GPU on macOS is drawn. Chrome 156.0.8078.4 headless approximates it, on both of its
canvases alike. A black triangle on a 160 by 100 canvas, worst levels from
the exact blur of Chrome's own unblurred triangle:

        sigma        2      8     30
        here      0.50   0.50   0.50
        Chrome   11.01   6.24   5.56

Chrome's figures are the ones Skia's image blur gave this library before, to
the hundredth. An image, or a canvas drawn into another, through a `blur()`
or a chain of blurs and colour functions is blurred the same way, and
composited at its `globalAlpha` rounded once: a 40px red square blurred by
8px reads alpha 72 and 30 five and ten pixels outside it and 228 ten pixels
inside it here, where Chrome's `drawImage` of it reads 72, 28 and 233. `tests/blur_result.rs`
holds the pixels, at a density and inside a `saveLayer` too. A draw that its clip cuts is the exception, and
the same one any clipped draw is: Skia anti-aliases a path its clip does not
contain by another route, whose edges move by up to 15 levels, and the blur
is exact on what that route draws. On the CPU engine the page's edge is such
a clip for a draw past it by more than the blur's reach: an 80 by 40 rounded
rectangle 40 pixels past the right edge of a 300 by 200 page reads 15 pixels
off the exact blur of the whole draw blurred by 2px, by at most 0.85 of a
level, and none off blurred by 8px. The cause is the one in the entry on a
fill or stroke the page's edge cuts, and it is not worked around for the
same reason. Within the reach, and on the GPU engine, a blur of a draw past
the page is the exact blur of all of it.

**A blurred rectangle is its exact Gaussian blur, and Chrome's is not.**
Filter Effects Module Level 1 defines `blur()` as a Gaussian blur whose
standard deviation is the length given. Blurring a rectangle by it gives the
product of the blurred interval across and the blurred interval down, which
has a closed form, and a flat-colour rectangle that stays one on the device is
drawn here as exactly that, averaged over each pixel: within half a level on
8-bit pixels on the CPU engine, within 0.56 of one on the GPU engine, whose
fragments hand the value on as a half float, and to the surface's precision on
F16 and F32. Chrome 152 approximates it, and its two canvases approximate it
differently. A black 60 by 40 rectangle at a fractional position, worst levels
from the exact blur:

        sigma                    2      12     40     41    100
        Chrome GPU canvas     1.70    3.25   1.67   1.69   1.11
        Chrome CPU canvas    10.57    6.10   2.72   2.51   0.97

The CPU canvas is the one `willReadFrequently` selects. At a sigma of 100
Chrome keeps 97.3% of the rectangle's ink on its GPU canvas and 97.9% on its
CPU one, where a Gaussian keeps all of it. A red square over the middle 60% of
a 100 pixel canvas sums, across its alpha:

        blur        1000     500     200      100       50
        exact          0   10000   38600   131204   393512
        Chrome GPU     0   10000   49252   109412   403200
        Chrome CPU 10000   10000   37296   133468   407096

and on a 400 pixel canvas blurred by 1000px its centre is 2 here and 8 and 9
on Chrome's CPU and GPU canvases. `tests/suite/context2d.test.js` holds the
sums to the exact values, and `tests/blur_rect.rs` the pixels. Copying Chrome
would mean choosing between two approximations that differ from each other by
more than either differs from the definition.

A rectangle in a canvas drawn into another is played onto the page with the
rest of that canvas, and is drawn in closed form there too: within half a
level at densities 1 and 2. One played into a picture -- a canvas used as a
pattern -- has no pixels for the closed form, and is blurred on its result,
by the exact Gaussian of the entry above.

**A fill or stroke the page's edge cuts is drawn differently far inside the
page on the CPU engine.** The page's edge cuts a path as a clip does: Skia's
analytic anti-aliasing builds a path's edges against the raster's bounds where
they do not contain it (`SkScan_AAAPath.cpp` lines 1615 and 1729, given the
bounds at `SkScan_AntiPath.cpp` line 130), and the edges it adds at the page's
edge re-slice the rows they span. A triangle from (-30, 80) through
(60, 15.25) to (140.5, 95) on a 200 by 120 page differs from the same triangle
drawn on a larger canvas, over the page, in 223 pixels at density 1, as far as
138 pixels from the edge -- the whole width the triangle spans -- and in 411
and 439 at densities 2 and 3. Over twelve shapes -- a triangle, a curved fill
and a stroke crossing each of the four edges -- up to 354 pixels moved by up
to 52 levels at density 1, and up to 720 by up to 65 at density 2. Chrome's
CPU canvas rasterizes through the same Skia code; it was not measured at the
page's edge.

The draw on a raster that holds the whole shape is the only construction found
exact. Three cheaper ones were measured against it, over the twelve shapes,
and none is:

- Cutting the path to the page before filling it -- a stroke by its outline
  -- gives Skia's pixels for 5 of the 12 at density 1, fewer moved for 3 and
  more for 4: a triangle crossing the left edge moved 241 pixels cut first
  and 142 drawn as it is. The new edge lies on the page's pixel boundary, but
  it is still an edge, and it changes how the rows it spans are split.
- A raster wider than the page by 16, 64 or 256 device pixels, with the shapes
  stretched to reach 600 units past the edge, lowers the largest error but not
  how far in it reaches: the left triangle's worst pixel fell from 23 levels
  at 16 to 5 at 256, while the pixels moved fell only from 149 to 132, as deep
  in the page. A 256-pixel band left one shape of the twelve unmoved, at both
  densities. The raster is 1.5, 3.4 and 18.7 times the page's at density 1.
- No other converter is reachable. `SkDraw.cpp` line 932 sends every
  anti-aliased fill to `SkScan::AntiFillPath`, whose one anti-aliased route is
  `SkScan::AAAFillPath` (`SkScan_AntiPath.cpp` line 130); its other branch, at
  lines 87 and 88, fills a path too large for anti-aliasing without it. This
  Skia has no supersampling converter and no setting that selects one.
