# Where output differs from a browser, or extends it

Everything here is intentional or inherited from Skia. If a differential run
against a browser flags one of these, it is not a regression -- read this first.

**Inherited from Skia, and not fixable here.** Glyph antialiasing differs by a
few percent of pixels, confined to stem edges; metrics are unchanged. A contour
of a single `moveTo` no longer survives, so `Path2D.d`, `.edges` and `.bounds`
lose the orphan point -- no pixel effect, since a move-only contour paints
nothing.

**Chrome's colours on a Display P3 canvas, a level off where its conversion
crosses a rounding boundary.** Chrome converts a colour into an eight-bit
Display P3 canvas through skcms's approximate transfer function, whose
results lie 64 ulps apart, so a channel that falls near a boundary between
two bytes can land on the wrong side of it. Here the colour is converted
exactly and stored as the exact product rounded, a tie rounded up -- see
`ExactColour::at`. Measured in Chrome 153 on a Display P3 canvas, read back
in P3:

| colour                                    | exact, x255              | here                           | Chrome                         |
| ----------------------------------------- | ------------------------ | ------------------------------ | ------------------------------ |
| `hsl(0 -50% 50%)`                         | 127.5 in each channel    | 128, 128, 128                  | 128, 127, 127                  |
| `hwb(from red h 60% 60%)`                 | 127.5 in each channel    | 128, 128, 128                  | 128, 127, 127                  |
| `color(from red xyz-d50 calc(x / 2) y z)` | red 148.498              | 148                            | 149                            |
| `hsl(from red 1e3 s l)`                   | green 30.505             | 31                             | 30                             |
| `color(display-p3 1 0 0 / 0.5)`           | red 127.5, premultiplied | stores 128, reads 255          | stores 127, reads 253          |
| `color(display-p3 1 0 0 / 0.1)`           | red 25.5, premultiplied  | stores 26, reads 255, 0, 0, 26 | stores 25, reads 245, 0, 0, 26 |
| `color(display-p3 0.5 0.2 0.1 / 0.2)`     | red 25.5, premultiplied  | stores 26, reads 130           | stores 25, reads 125           |
| `color(display-p3 0.5 0.2 0.1 / 0.6)`     | red 76.5, premultiplied  | stores 77, reads 128           | stores 76, reads 127           |

A grey with one channel apart from the others, and 148.498 stored as 149,
are wrong by arithmetic, which is why this differs. A tie Chrome rounds either
way -- the same canvas stores the green 25.5 of
`color(display-p3 0.5 0.2 0.1 / 0.5)` as 26 -- and here always up. Away from a boundary the
two agree, translucent colours included: Chrome's canvas premultiplies in
float and rounds once, as this one does, so
`color(a98-rgb 0.3 0.6 0.9 / 0.5)` reads 50, 151, 225 and
`rgb(from oklch(0.8 0.18 165) r g b / 0.5)` 92, 221, 163 in both. Chrome's
sRGB canvas rounds the colour to a byte before it premultiplies, which this
one does not -- see below.

**Translucent colours on an sRGB canvas, stored as the exact product where
Chrome's CPU canvas rounds first.** Chrome blits a flat translucent colour
onto an eight-bit sRGB canvas through Skia's legacy blitter
(`SkBlitter::UseLegacyBlitter`), which rounds the colour and its alpha to
bytes and multiplies those. This canvas stores the exact product rounded
once, a tie rounded up, on either engine, as it does on every other
eight-bit canvas -- see `ExactColour::at`. Colours and alphas on the byte
grid come out the same both ways; off it they part by a level. Over 65,025
greys and alphas, `(c + 0.37) / 255` at `(a + 0.61) / 255`, Chrome's rule
stores 8,260 a level off the product. `rgb(200 60 30 / 0.75)` over
`rgb(40 90 160)` reads blue 63 here, the product 22.5 rounded up over the
backdrop's 40, and 62 in Chrome 141, which stores 30 at 191 as 22.

**Over earlier content, the backdrop's share is taken at the alpha rounded to
a byte, as Chrome's is.** The exact composite is `c·a + d·(1 − a)`. A fill
rounds the product half of it once, and the backdrop's half comes from Skia's
eight-bit blend at `A`, the alpha rounded to a byte. On the CPU engine
a fill adds `(d · (256 − A)) >> 8`, the legacy blitter's term, to its product
rounded once. A drawn image or canvas and a `saveLayer` premultiply
their bytes by `A` and add `(d · (255 − A) + 127) / 255`, the raster
pipeline's divide on ARM (`div255` in `SkRasterPipeline_opts.h`; x86 divides
differently and is not measured here). On the GPU engine, Metal, the blend
runs in half precision: a fill gives it the product placed on its byte and
`A`, and an image, a canvas or a layer gives it the colour times the alpha
unrounded. `#dc2828` at `globalAlpha` 0.99 over white is 220.35, 42.15
exactly, and white's share at `A` = 252 is 3 where the exact share is 2.55:

|                           | CPU         | GPU         |
| ------------------------- | ----------- | ----------- |
| fill                      | 221, 43, 43 | 221, 43, 43 |
| drawn canvas, `saveLayer` | 220, 43, 43 | 220, 42, 42 |
| Chrome 158, CPU canvas    | 220, 43, 43 |             |

Each rule above is a byte model that reproduces every pixel of a sweep of
427,520 on darwin-arm64 -- greys 0 to 255, 334 alphas, backdrops 255, 200,
128, 37 and 0 -- and so does Chrome's legacy rule for its CPU canvas. Over
5,592,064 colour, alpha and backdrop triples with the alpha off the byte grid,
computed from those rules, a CPU fill stores 34.2% a level off the exact
composite, a CPU image or layer 27.1%, and Chrome's rule 40.0%. On the GPU
the misses of an image or layer are all within 0.18 of a level of a tie, from
the half-precision arithmetic: 3.3% of the sweep, against 21.4% for a fill.

Text sets its glyph masks' contrast from the colour it is drawn in, as Skia
does (`SkMaskGamma`), and on this canvas that colour is the one that stores
the exact product. Each channel is drawn in the colour nearest the one named
of those that store it on either engine -- see
`ExactColour::nearest_text_paint` -- which keeps the named colour's contrast
for all but 637 of the 65,025 pairs above, 456 of them at an alpha under 32
levels. Those store the exact product inside the glyph, and their edges take
the contrast of the bucket next to the named colour's.

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
in `skia_text_metrics.cc` reads them from the Skia strike -- but does not step
the same way: at a device scale of 1 it lays a web font out without subpixel
positioning, and that function then rounds each advance to a whole pixel. The
next entry has the measurements. The `UPSTREAM` note at the size in
`Context2D::set_font` carries the re-check.

On Linux an advance comes out a fraction off its exact size, in every face
whose units per em do not divide the size's 64ths into 16.16 fixed point
exactly. Skia's FreeType scaler takes an unhinted advance from FreeType's
`linearHoriAdvance` (`SkFontHost_FreeType.cpp`), which scales the font's units
by the 16.16 `x_scale` that `FT_Set_Char_Size` rounds: at 16px in a face of
1000 units, 1024 × 65536 / 1000 is 67,108.864 and is stored as 67,109. So a
1000-unit ideograph advances 16 + 2/65536 px, about 3 × 10⁻⁵, where CoreText
gives 16, as #695 found DirectWrite does, and the excess adds up along a line:
past 512 glyphs it passes Chrome's 1/64 px layout unit. Measured at 16px with
npm 7.1.0's linux-arm64-glibc binary, against this library on macOS:

| face, text                                 | Linux      | macOS      |
| ------------------------------------------ | ---------- | ---------- |
| Noto Sans SC (1000 units), `漢`            | 16.0000305 | 16         |
| Noto Sans SC, six `漢`                     | 96.0001831 | 96         |
| Oswald (1000 units), `H`                   | 8.9760132  | 8.9759979  |
| Amstelvar (2000 units), `H` at `"opsz" 14` | 13.9198151 | 13.9199982 |
| Arial (2048 units), `Hamburgefonstiv`      | 120.945313 | 120.945313 |

A face of 2000 units comes out under, its scale of 33,554.432 stored as
33,554, and one of 2048 units is exact, its scale a whole number. Amstelvar at
its default instance stands a further 0.0016 apart, the `HVAR` rounding
recorded above. Skia hands HarfBuzz these same advances through its own font
functions (`SkShaper_harfbuzz.cpp`), so no route here takes an exact one.

Chrome 158.0.8127.18 on Linux (Chrome for Testing's headless shell, in
Playwright's `v1.65.0-noble` image on arm64) lays #695's ideograph out
at exactly 16 because it rounds every advance of a web font to a whole pixel
at a device scale of 1, as `SkFontGetGlyphWidthForHarfBuzz` does where the
font is not subpixel-positioned, which `gfx::GetFontRenderParams` turns on only
above a scale of 1 (`font_render_params_linux.cc`). Measured as `measureText`
and as a `<span>`'s width, which agree at a scale of 1, against the exact
advances CoreText gives on macOS:

| face, size, text                  | Chrome, scale 1 | exact   |
| --------------------------------- | --------------- | ------- |
| Noto Sans SC 16px, `漢`           | 16              | 16      |
| Noto Sans SC 13.5px, `漢`         | 13              | 13.5    |
| Noto Sans SC 13.5px, six `漢`     | 78              | 81      |
| Oswald 16px, `Hamburgefonstiv`    | 100             | 98.784  |
| Amstelvar 16px, `Hamburgefonstiv` | 134             | 129.161 |
| Oswald 17.3px, `Hamburgefonstiv`  | 104             | 106.810 |

At a scale of 2 a `<span>` lays them out at half pixels -- 98.5 for Oswald at
16px, 81 for the six ideographs at 13.5px -- while `measureText` stays at whole
pixels. Liberation Sans, a system face reached through fontconfig, measures
fractional advances at both scales: `measureText("a")` at 16px is 8.8984375,
its 1139 units of 2048 exactly. This library keeps every advance fractional on
every platform, as Chrome does on macOS, so on Linux it stands apart from
Chrome's whole pixels by more than the 16.16 excess does. The `UPSTREAM` note
at the width in `Typesetter::measure` carries the re-check.

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

On Linux a glyph sheared past about 80 degrees is drawn sheared, where Chrome
on Linux is expected to draw it upright. FreeType 2.14.3, which skia-bindings
0.156.0 embeds, has `FT_Set_Transform` replace with the identity any matrix
`FT_Matrix_Check` refuses (`src/base/ftobjs.c`, FreeType's f4e40ad03e), and
that refuses one whose `32 |det|` is no more than the sum of its squares. Skia
hands FreeType a run's transform with its scale divided out, so a shear `k`
between the run's axes is refused once `k^2 + 2 >= 32`, about 79.65 degrees:
`oblique 85deg`, or upright text under `setTransform(1, 0, -5.6, 1, 0, 0)`,
came out upright, through glyph masks and through the paths Skia draws large
and stroked text from alike. Here a run sheared past `k^2 = 28`, a little
inside that, is drawn from its outlines, sheared as `outlineText` shears them.
Its shape is the sheared glyph's; its edges are a filled path's rather than a
glyph mask's. Against the masks FreeType 2.14.2 drew -- measured on Linux
arm64, an `I` and an `H` in Oswald -- that moves edge pixels by up to 18
levels at 200px and up to about 100 at 12 to 40px, and adds up to 2% to the
ink, 13% at 12px and 89 degrees. Chrome on Linux draws text through the same
Skia and FreeType, so from FreeType 2.14.3 it is expected to draw these runs
upright; that is inferred, and Chrome on Linux was not measured. macOS and
Windows draw text with CoreText and DirectWrite rather than FreeType, and are
unchanged; macOS draws 85 degrees sheared through its glyph masks. The
`UPSTREAM` note at `past_freetype_shear` carries the re-check.

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
- _`fontBoundingBoxAscent` and `fontBoundingBoxDescent` are not rounded to
  whole pixels._ Chrome 156 reports the face's ascent and descent rounded to
  the pixel, and its `hangingBaseline`, four fifths of that ascent, follows
  the rounding; this library reports them as the face gives them. At 100px
  Oswald the ascent is 119.30 here and 119 in Chrome, the descent 28.90
  against 29, the hanging baseline 95.44 against 95.20; Georgia's ascent is
  91.70 against 92.
- _Helvetica, Times and Courier report the ascent their tables give._
  Chrome 156 on macOS adds about 15% of the font size to the
  `fontBoundingBoxAscent` of a face whose own family is Helvetica, Times or
  Courier -- matched by the font's family name, so the same Helvetica bytes
  loaded as a web font under another name are adjusted too -- and its
  `hangingBaseline`, four fifths of that ascent, follows. Nothing in the
  files says so, and CoreText itself reports the tables' figure. This
  library reports each face's own ascent. At 100px: Helvetica 77 here and 92
  in Chrome, Times 75 against 90, Courier 75.39 against 90; Helvetica's
  hanging baseline 61.60 against 73.60. The descents agree. Arial, Georgia,
  Menlo, Helvetica Neue and Times New Roman are not adjusted. Chrome's
  `line-height: normal` for these faces rises by the same amount, so a line
  height built from `fontMetrics()` -- `Context2D::font_metrics` in Rust --
  as Chrome builds its own comes out short of Chrome's: 16 against 18 at
  16px, 100 against 115 at 100px, for all three.
- _The em square that places `top`, `middle` and `bottom` is not snapped to
  a 64th of a pixel._ Both divide the font size at the alphabetic baseline
  as the face's typographic ascender and descender divide it, whether or not
  the face sets `USE_TYPO_METRICS`; Chrome 156 then rounds the top to its
  1/64 px layout unit, which this library does not, so the two part by at
  most 1/128 px. At 100px Georgia the top is 77.722 here and 77.71875 in
  Chrome, Arial 77.575 against 77.578125, Times New Roman 76.262 against
  76.265625. A face whose typographic ascender and descender are zero is
  divided as its ascent and descent divide it, in both.
- _An instance's em square follows its `MVAR`._ A variable face's `MVAR`
  table can vary the typographic ascender and descender from one instance to
  the next (`hasc`, `hdsc`). Chrome 156 keeps the stored values at every
  instance; this library divides the em as the instance's own do, which is
  what the face says its em square is there. Amstelvar, its `MVAR` turned so
  that it moves them along `wght`, puts `top` 75 above the alphabetic
  baseline of 100px at weights 100, 400 and 900 in Chrome, while its text
  widens with the weight; here it is 70.16, 75 and 84.375, the stored 1500
  and -500 moved to 1434 and -610 at 100 and to 1620 and -300 at 900.

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
- _`saveLayer` and a fill at the same alpha part by up to a level_ over
  earlier content, in either direction: a layer composites as a drawn image
  does, not as a fill -- see "Over earlier content" above. Over the sweep
  there, a layer stores darker than the fill for 15.1% of the CPU's triples
  and lighter for 21.9%, and on the GPU darker for 13.4% of pixels over a
  backdrop and lighter for 13.6%. Alike at alphas 0 and 1.
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
  `"serif"` with no family of that name is such a list. A character the named
  face lacks takes the same default: the COLR test font has no space, so a
  space in `32px colr` is `sans-serif`'s here, 8.891 px, and Times' in Chrome,
  8.000, as it is here under `32px colr, serif`. `new_font_collection`
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
  The same oblique face with its `italicAngle` set to 0, flagged only by
  `OS/2` `fsSelection` bit 9, is drawn as the last row's is for `oblique`
  and `italic`, in either load order, though CoreText and an unpatched
  DirectWrite report it upright -- `FontLibrary::slanted` in
  `src/node/font_library.rs`. Chrome was not measured on that face.
  An SVG document's text follows the same rule, `oblique <angle>` included:
  Chrome 153 draws an inline SVG's `italic` and an oblique of 14deg or more
  in a family with no slanted face with a quarter's skew, and a smaller or
  negative angle upright, as its canvas text does.

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

- _A Paragraph API background ends where its text does, where Chrome snaps
  its ends to the pixel._ A span's background in CSS covers its text,
  trailing spaces included, and Chrome paints it from and to whole pixels:
  behind `"ab  "` in 16px Oswald, 21.30px wide, it covers columns 0 to 21 in
  Chrome 158, and here 0 to 22, the last column covered for the 0.30 of it
  the text reaches. Text has no such rounding, and a background that stopped
  short of its last glyph's advance would cut the glyph's box.

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

- _A `Paragraph` style's shadows paint the first listed on top, where
  CanvasKit paints them in the order given, the first lowest._ CSS Text
  Decoration 3 section 4 paints a list of text shadows as CSS Backgrounds 3
  paints `box-shadow`, front to back. SkParagraph, which CanvasKit's
  paragraph is, draws a style's shadows in list order. The paragraph is this
  library's extension and follows CSS here. Pinned in
  tests/paragraph_shadow.rs, `a_styles_first_listed_shadow_lies_on_top`.

- _A style's run of text on a line casts its shadow as one, the exact
  Gaussian blur of the whole run, where SkParagraph blurs each glyph with a
  mask filter._ A mask filter draws a glyph heavier than the glyph itself on
  macOS -- a 16px word drawn through one at a sigma of 0.001 held 1.23 times
  the ink of the word -- and blurs drawn a glyph at a time lose ink where
  they overlap: the same word blurred by 20px kept 0.54 of its sharp
  shadow's ink on the CPU engine. Each run is one element, as CSS Text
  Decoration 3 casts `text-shadow` for each fragment of an element; Chrome
  156 draws an inline span's shadow wrapped onto two lines exactly as it
  draws two blocks of one line each. A style set in two faces, its own and a
  fallback's, is one run. The shadows composite source-over in document
  order, beneath all the text. On pixels the blur is
  `context::exact_blur`'s; a PDF or SVG keeps Skia's mask filter, which its
  backends hold as vectors. Pinned in tests/paragraph_shadow.rs.

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

**`color(display-p3-linear ...)` converts by CSS Color 4's matrices, and
Chrome 158's figures stray from them from the fourth.** Read through
`color-mix(in srgb, ...)`, Chrome takes `color(display-p3-linear 0.2 0.5
0.3)` for `color(srgb 0.399341 0.74364 0.571495)`; the sample code's
`lin_P3_to_XYZ` and `XYZ_to_lin_sRGB` in `f64` give 0.39949968, 0.7436066
and 0.57145216, which is what this library paints. Away from a byte's edge
the two paint alike, and at one they part. P3 and sRGB share D65's white, so
a grey in linear light is a grey in sRGB: `color(display-p3-linear 0.5 0.5
0.5)` is 0.73535698 in each channel, 187.52 as a byte, and paints 188, 188,
188 here, where Chrome Headless Shell 158.0.8127.18 paints 187, 188, 188 on
an sRGB canvas, by `fillStyle`, `strokeStyle`, `shadowColor` and `filter`
alike.

**A `drop-shadow()` filter keeps a wide colour on a wide canvas, as a fill
does; Chrome 158 clips it to sRGB.** Filter Effects Module Level 1 gives
`drop-shadow()` a `<color>` and says nothing of its gamut, and Chrome's own
`fillStyle` and `shadowColor` keep the same colour wide on the same canvas.
Measured in Chrome Headless Shell 158.0.8127.18 on a `display-p3` canvas,
read back with `getImageData` in `display-p3`, each filter a `drop-shadow(30px
0 0 ...)` of a black square:

        colour                            fill and shadowColor   drop-shadow
        color(display-p3 1 0 0)           255, 0, 0              234, 51, 35
        color(display-p3-linear 1 0 0)    255, 0, 0              234, 51, 35
        color(rec2020 0 1 0)              0, 255, 0              117, 251, 76

234, 51, 35 is sRGB red as Display P3 holds it, and 117, 251, 76 sRGB green.
Here the drop-shadow paints each as its fill does. A colour inside sRGB
stays within a level of its fill on the same canvas:
`color(display-p3-linear 0.3 0.15 0.6)` fills 149, 108, 203 in both and
filters 149, 108, 204 in Chrome. On an sRGB canvas Chrome's filter and fill
agree.

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
the next entry blurs in closed form, is drawn here as exactly that, rounded once:
within half a level on 8-bit pixels on the CPU engine, and of an opaque
colour on the GPU engine, and on F16 and F32 to the surface's precision, as
measured on the CPU engine, where a float canvas made for the GPU on macOS is
drawn. On an 8-bit canvas a translucent colour scales the blur by its alpha
on the byte a fill stores it on -- 128 of 0.5, 77 of 0.3 -- so that the
interior stores what a fill of the colour does, as Chrome's CPU canvas does
too. On the CPU engine an 8-bit page blurs such a path's coverage and colours
it after, so that each channel is rounded once, from the exact product of the
blurred coverage, the colour and its alpha, where a channel blurred from pixels
already rounded to bytes could land a level off it. On the GPU engine such a colour is blurred through a layer of half
floats, which keeps that interior, and its edges are rounded twice, to a half
float and then to a byte: up to a level off the exact Gaussian, 65 where it
is 65.52. Chrome 156.0.8078.4 headless approximates the blur, on both of
its canvases alike. A black triangle on a 160 by 100 canvas, worst levels from
the exact blur of Chrome's own unblurred triangle:

        sigma        2      8     30
        here      0.50   0.50   0.50
        Chrome   11.01   6.24   5.56

Chrome's figures are the ones Skia's image blur gave this library before, to
the hundredth. An image, or a canvas drawn into another, through a `blur()`
or a chain of blurs and colour functions is blurred the same way, and
composited at its `globalAlpha` rounded once: a 40px red square blurred by
8px reads alpha 72 and 30 five and ten pixels outside it and 228 ten pixels
inside it here, where Chrome's `drawImage` of it reads 72, 28 and 233. So is
a fill or stroke in a pattern or a gradient, whose blur is of its pixels:
through Skia's image blur, a triangle in a canvas's pattern of translucent
stripes blurred by 4px was 5.78 levels off the exact blur on the CPU engine
and 1.19 on the GPU. `tests/blur_result.rs`
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

**Text under `blur()` is the exact Gaussian blur of the drawn text, and
Chrome's is not.** The rule is a blurred path's: the text's own glyphs, as they
rasterise without the filter, convolved with the Gaussian sampled at whole-pixel
offsets and rounded once, on both engines -- `tests/blur_text.rs` holds 16px
blurred by 2, 40px by 3 and 48px by 10. Chrome 156.0.8078.4 headless draws text
under `blur()` through Skia's image filter, which approximates the Gaussian with
three box blurs. One 40px "Ag" in Oswald, `rgb(200 120 40)`, blurred by 3px on a
transparent 120 by 80 canvas, came up to 12.7 levels of alpha from the exact
blur of Chrome's own unblurred text, and 10.0, 5.7 and 2.2 in premultiplied
red, green and blue, over 1,672 pixels more than a level off. Drawn through that
filter this library matched it byte for byte. Text under a `drop-shadow()` is
the same rule's, as any draw's shadow is -- see the entry on a `drop-shadow()`
-- where Chrome's shadow of that text is 12.7 levels off. On the CPU engine an
8-bit page blurs a run in one flat colour from its coverage and colours it
after, so that each premultiplied channel, and a translucent colour's alpha,
is rounded once from the exact product of the blurred coverage, the colour and
its alpha. Blurred from pixels already rounded to bytes, a channel could land
a level off it: 16px blurred by 2 in `rgb(200, 80, 30)` moved by a level on
491 of 2,322 inked pixels, and in `rgba(200, 80, 30, 0.6)` on 646 of 2,259,
alpha among them; an opaque run's alpha is the same either way. That holds at
every blur, whether summed tap by tap or through the FFT. A run on a page
drawn onto one in another space is blurred in the chain's layer drawn opaque,
its alpha rounded once as the route's is and its colour from the layer's
bytes. A run in a gradient or a pattern, one whose decoration names a colour
of its own, and every run on the GPU engine or a float page are blurred from
their pixels in the chain's layer. A translucent run's shadow under
`drop-shadow()` on an 8-bit page on the CPU engine is cast from the run
drawn opaque, its alpha taken as a factor in floats, so that the shadow's
alpha too is rounded once; the run is drawn over it as Skia draws it
translucent, since it is composited over its shadow rather than scaled with
it. Cast from the translucent run's own bytes, rounded before the blur and
again after, runs of 13 to 60px moved by a level on 82 to 1,706 pixels. On
the GPU engine a translucent run's shadow is still cast from its pixels in
the chain's layer: the same runs come a level off on 7 to 265 pixels, where
an opaque run's shadow is exact.

**A blurred rectangle is its exact Gaussian blur, and Chrome's is not.**
Filter Effects Module Level 1 defines `blur()` as a Gaussian blur whose
standard deviation is the length given. Blurring a rectangle by it gives the
product of the blurred interval across and the blurred interval down, which
has a closed form, and a flat-colour rectangle that stays one on the device is
drawn here as exactly that, averaged over each pixel: within half a level on
8-bit pixels on either engine, the GPU landing the CPU's bytes, and to the
surface's precision on F16 and F32. Both engines draw the same profiles,
worked on the CPU: the GPU's own `exp` and division, working them per
fragment, put a rectangle up to 0.86 of a level off at a sigma of 0.25 and
over half a level at every sigma. On an 8-bit canvas the blur scales the bytes a fill of the colour
stores, as a blurred path's does, so that its interior stores what the fill
does: 128 of `rgba(0, 0, 0, 0.5)`, where the alpha as given stored 127. Chrome 152 approximates it, and its two canvases approximate it
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

**A fill or stroke a clip cuts is drawn whole and multiplied by the clip on
the CPU engine, and Chrome's CPU canvas is not.** Where a clip covers part of
a path, Skia's analytic anti-aliasing builds the path's edges against the
clip (`containedInClip` in `SkScan_AAAPath.cpp`), and the edges it adds at
the cut split the strips every other edge in those rows is stepped in, so
pixels far from the cut change. A triangle from (20, 80) through
(80, 15.25) to (140.5, 85), clipped at x 82, drew 60 pixels more than two
pixels inside the clip differently from the same triangle unclipped, by up to
17 levels, and Chrome 156's headless CPU canvas measures as Skia does. The
mean error over the pixels left partly covered, in levels from the exact area
of the triangle within the clip, at densities 1, 2 and 3:

        clip                  x1      x2      x3
        none                1.57    1.58    1.59
        cut at x 82
          Skia, Chrome      4.60    5.90    2.96
          here              1.55    1.55    1.56
        cut at x 82.5
          Skia              4.85    4.15    2.88
          here              1.42    1.56    1.35
        rectangle, edges between pixels
          Skia              5.79    6.39    3.79
          here              2.02    2.30    2.13
        circle of radius 45.3
          Skia              6.70    8.85   13.65
          here              3.38    3.65    5.23

Chrome was measured at density 1 under the cut at x 82 only. Here the draw is
recorded as a `CutOp` and played back on a CPU raster under the clip as a clip
shader -- a picture of the clip filled, rasterized at the density it is
played at -- so the path is drawn unclipped and the clip multiplies its
coverage. More than two pixels inside the clip the coverage is the unclipped
draw's, byte for byte, at every density, for fills, strokes, dashed strokes
and marker strokes alike, in a layer or not, where Skia moved up to 273 of
them by up to 92 levels -- for a draw the page's edge does not also cut, as
below; at the clip's edge the error left is the clip's own rasterization and
the product of the two coverages where both edges cross a pixel.
`tests/clip_cut.rs` holds both. The GPU engine is unchanged: its multisampled
coverage is not moved by the clip, and the clip shader would move 50 bytes of
a circle-clipped draw there. A PDF or SVG export is unchanged too, clipped by
the clip's path as before.

The coverage is the unclipped draw's; the blend is not always its bytes. A
clip shader takes the draw onto Skia's raster pipeline (`SkBlitter::Choose`
takes it whenever there is a clip shader), and on an eight-bit canvas in sRGB
an unclipped fill in a flat colour is blended by Skia's legacy blitter
instead, which rounds differently. So there a coloured or translucent fill,
or one over earlier content, lands within a level of a premultiplied channel
of the unclipped fill, and of Chrome's, whose CPU canvas blends either the
legacy way: along the edges, and at full coverage too for a translucent fill
over translucent content. 0.33 black over `rgba(200 40 90 / 0.6)` lays down
alpha 187 inside the triangle where the unclipped fill lays down 186. Read
back unpremultiplied, which divides by the alpha, a level grows where the
alpha is low. On a canvas in another space or at a float
depth the unclipped fill takes the pipeline already, and the two blend alike.

The pipeline runs at low precision, holding the paint's colour at eight bits
as the legacy blitter does, and on ARM its divide by 255 rounds exactly, so
there the cut fill is the nearer of the two to exact source-over: 187 against
an exact 186.6, a mean error of 0.294 levels a premultiplied channel over the
triangle's interior against 0.344. Elsewhere that divide is `(v + 255) / 256`,
as much as a level high (`div255` in `SkRasterPipeline_opts.h`), and which is
nearer is not measured here. `tests/clip_cut.rs` holds the bound of a level,
the eight-bit colour, the pages that blend alike, and on ARM the figures.

A draw goes this way only when the clip cuts it: one wholly inside or outside
the clip, an axis-aligned rectangle filled, text, an image, a texture fill,
and a draw with a shadow, a filter or a blend that reaches past it are clipped
as Skia clips them. One inside a `saveLayer` goes this way among the layer's
entries. The layer is made to hold what the draw paints before the clip, and on
a CPU raster is opened under its own clip as a clip shader too, since a layer
opened under a clip path is no larger than the clip's bounds: Skia's
anti-aliasing builds edges against the layer's bounds as against a clip, and
either way a triangle cut at x 82 moved 57 pixels inside the clip.

A canvas drawn into another, used as a pattern or drawn as an image, draws its
cut draws on the CPU as it draws them itself, at densities 1 to 3 -- an image
at density 1, where its pixels are the canvas's own. The recording Skia
rasterizes for an image applies the clip as a clip shader, a pattern
rasterizes the canvas as the canvas composites itself, and `drawCanvas` keeps
a canvas holding a cut draw as its entries, which each target clips its own
way. A PDF or SVG export keeps such a canvas drawn in with `drawCanvas` as
vectors clipped by the path, as before: Skia's SVG device cannot write a clip
shader and writes an empty clip in its place, which hides the draw. The raster
a PDF embeds for an image or a pattern of the canvas, and an SVG for an image,
is exact at the cut. On the GPU engine an image of such a canvas is rasterized
with the clip shader too: under the circle above, with a stroke beside the
triangle, 50 pixels change, 288 differ from the GPU's own draw of the page
where 244 did, and their mean distance from the CPU's exact draw falls from
22.96 levels to 21.09. One drawn in with `drawCanvas`, or used as a pattern,
draws there as the GPU draws the page.
`a_page_drawn_into_another_draws_as_it_does_itself` and its neighbours in
`src/context/mod.rs` hold it.

A canvas drawn in through a source rectangle that cuts what it draws --
`drawCanvas` with a crop -- is drawn whole and cropped by the rectangle's
coverage on pixels, on both engines, rather than clipped by it, which on
the CPU moved its paths' edges as any clip does: a triangle by up to 28
levels at 156 pixels more than two inside the crop. On the GPU the clip moved
no edge, but bounded what a blur in the canvas read: a pixel four inside the
crop parted by 63 at density 3. On the CPU a canvas composited plainly is
drawn into a layer, which the crop multiplies before it closes. That keeps
every draw on Skia's legacy blitter -- a clip shader would put each through
its raster pipeline, a level off for each draw a pixel lies under -- and
composites the canvas as one image, as `drawImage` does. Over what the page already holds it lands
within two levels of the same canvas drawn in uncropped, which blends each
of its draws onto the page in turn: 72 pixels at density 1 over a band of
colour. On an empty page, through a layer -- at an alpha, under a blend --
and on the GPU it is that canvas's draw to the byte.
`a_page_drawn_in_cropped_draws_its_own_pixels_inside_the_crop` holds it. A
PDF or SVG writes the crop as a clip, as before.

A draw the page's edge also cuts is not exact here either. The route plays the
draw back on a raster of the page, whose edge cuts it as the next entry
describes: a triangle from (-30, 80) through (60, 15.25) to (140.5, 95) under a
clip at x 82 moved 158, 315 and 266 pixels inside the clip at densities 1, 2
and 3, as far as 79 pixels from the edge.
`the_page_edge_moves_pixels_far_inside_the_page` in `tests/clip_cut.rs` holds
it with the unclipped draw beside it, and goes red when Skia draws a path the
page cuts as it draws one the page contains.

**A `drop-shadow()` is the exact Gaussian blur of what casts it, and
Chrome's is not.** Filter Effects Module Level 1 defines the shadow as the
draw's alpha in the shadow's colour, blurred by a Gaussian whose standard
deviation is the length given and moved by the offset, with the draw over
it. Here it is exactly that, made from the draw's own pixels and rounded
once: within half a level on 8-bit pixels on either engine, and on F16 and
F32 to the surface's precision. Chrome 153.0.8010.12 headless approximates the
blur, on both its canvases alike. A black triangle on a 160 by 100 canvas,
worst levels from the exact shadow of Chrome's own unshadowed triangle:

        drop-shadow     0 0 2px   3px 2px 2px   3px 2px 8px
        here              0.50        0.50          0.50
        Chrome            6.85       10.75          3.77

Its lengths land where Chrome's do: `drop-shadow(12px 4px 0 red)` lies 12
pixels right of what casts it and 4 down under a scale, a rotation or a
skew, as `shadowOffsetX` and `shadowOffsetY` do, and a canvas drawn into
another at twice its size casts it twice as far, 24 and 8. The draw is made
on the page's own pixels whatever the transform, as Chrome makes it: a
30-pixel square drawn through `drop-shadow(80px 0 0 red)` under
`rotate(30deg)` or a skew is the square drawn unfiltered, to the level. An
offset that is not whole pixels moves the shadow bilinearly, which keeps
where it lies: `drop-shadow(12.5px 4.25px 0 red)` lies 12.5 and 4.25 from
what casts it here, and 12.5 and 4.5 in Chrome, which puts it at 4.32 when
it is blurred by 2px. `tests/blur_result.rs` holds the shadow to the exact
one and `tests/filter_transform.rs` the offsets.

A draw under a `globalCompositeOperation` that composites the whole canvas
is drawn through Skia's filter layer instead. There the offset and the blur's length
are Chrome's, but the blur is Skia's approximation, and under a turn or a
skew Skia draws the filtered draw at the transform's scale and turns it onto
the page, which resamples the draw itself: a 30-pixel square read up to 16
levels from the square drawn unfiltered under `rotate(30deg)` on the CPU
engine and 40 on the GPU, and 65 and 80 under a `0.5` skew. A Skia
`ImageFilter` is drawn the same way: its lengths are in local units, and
turn and stretch with the transform as CanvasKit's do, so it is made at the
transform's scale and the filtered draw is turned onto the page. On an F16
or F32 page that is a layer of the page's own floats, where Skia's would be
eight bits a channel; beside a CSS filter it is Skia's layer on any page.

**A `shadowColor` shadow is the exact Gaussian blur of what casts it, and
Chrome's is not.** The HTML Standard casts the shadow from the draw's alpha
in `shadowColor`, blurred by a Gaussian whose standard deviation is half the
`shadowBlur`, in canvas pixels whatever the transform. Here it is that, made
from the draw's own pixels and rounded once, as a `drop-shadow()` is: within
half a level on 8-bit pixels on either engine. Chrome for Testing
158.0.8127.18 headless approximates the blur, on its raster canvas by up to
12.5 levels. A black triangle on a 560 by 300 canvas, worst levels from the
exact shadow of Chrome's own unshadowed triangle:

        shadowBlur          2       4       8      12      24
        here             0.50    0.50    0.50    0.50    0.50
        Chrome raster   12.49   11.33    8.38    8.97    3.97
        Chrome GPU       0.84    0.86    0.99    1.87    2.17

A filled rectangle in a flat colour, untransformed and unfiltered, casts the
closed form of its blurred rectangle, as `blur()` draws one -- see the entry
on a blurred rectangle -- and not the Gaussian of the rectangle as Skia
rasterises it, whose edges step in quarters of a pixel.

The shadow of a filtered draw is cast from what the filter makes, and the
shadow is not filtered, as the HTML Standard orders the two and as Chrome
does on both its canvases: a black draw through `invert(1)` casts its red
`shadowColor` red, not cyan, and one through `drop-shadow(30px 0 0 blue)`
casts a black shadow of the draw and of its copy, with no blue. Through `blur(2px)`
the shadow here is within 0.6 of a level of the exact shadow of the exact
filtered draw at a `shadowBlur` of 4, 8 and 16, the filtered draw being
held as pixels between the two; Chrome's raster canvas is 9.33, 7.84 and
5.47 off, its GPU canvas 1.05, 1.12 and 1.24.

Casting a shadow leaves the draw's own pixels as they are casting none: the
shadow is a group of its own, cast before the draw, and the draw takes the
route it takes casting none. `tests/shadow_result.rs` holds the shadow to
the exact one -- translucent, clipped and filtered -- and the draw to its
bytes casting none, under a filter and under a clip.

Four shadows are still Skia's: one under a `globalCompositeOperation` that
composites the whole canvas, one under a CanvasKit filter, one cast by a run
holding colour glyphs, and every shadow in a PDF or SVG document. Each is
cast from what the draw's filter makes, and each blur is Skia's.

**On an eight-bit sRGB canvas a shadow stores the exact product of its colour
and its alpha, on either engine, and Chrome's do not all.** `shadowColor`, a
CSS `drop-shadow()` and a CanvasKit drop shadow store the product rounded
once, as a translucent fill does, and so does a CanvasKit blend filter over a
fill. Chrome for Testing 156.0.8078.4 headless, measured on 205 colours,
rounds the colour and its alpha to bytes and premultiplies those on its
raster canvas, for fills and both shadows alike, so that 35 of the 205 store
a level off the product; on its GPU canvas a fill and a `shadowColor` shadow
store the product, and a `drop-shadow()` its raster canvas's bytes, a level
off its own fill on 34 of them: `color(srgb-linear 0.25 0.25 0.25 / 0.5)`
fills 68 there and casts 69, where both are 68 here. On the GPU, Skia's own
shadow filter moved a product lying within a few thousandths of a level of
a half across it, on 654 of the 65,536 greys and alphas:
`rgb(173 173 173 / 14/255)` cast 10 where the product is 9.498 and the fill
stores 9. So here the colour is placed on the bytes its product rounds to
first -- see `ExactColour::at` -- which leaves nothing for the filter to
round. `tests/eight_bit_shadow_colours.rs` holds every grey at
every alpha, and under a `globalAlpha`, to the product, on both engines and
across a change of `canvas.gpu` between drawing and reading. One shadow is
still a level off it: that of a run filled in a translucent colour and
naming a line colour, whose glyphs are rounded at the fill's alpha before
the shadow multiplies them by the `globalAlpha`, as
`tests/named_colour_precision.rs` holds.

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

**Text under a filter is drawn with some edge pixels other than before, and
neither placement is the edge's coverage.** A run of text under `blur()` or
`drop-shadow()` is filtered in a layer the size of what it inks rather than the
page's (`layer::hinted_reach`), and Skia's analytic anti-aliasing covers a
curved edge by where on the device it lies: the same geometry, moved by a whole
number of pixels, is covered differently. Times New Roman's 96px "Ag" stroked
40 wide with round joins, under `drop-shadow(0 0 3px black)` at density 3, is
drawn in 16 pixels up to 25 levels from the page-sized layer's drawing, and
without the filter, moved 15 device pixels, in the same 16 by up to 32. At the
worst of them the two give 114 and 146 where 8 by 8 samples of the edge give
133.8. No origin keeps it: moved 3 or 6 device pixels the stroke is covered
alike, moved 15 to 300 it is not, and a plain oval on the 1/64 grid flips at a
shift of 2 (`skia_covers_a_curve_by_where_it_lies`). The same two glyphs filled
rather than stroked, and a star of straight edges, did not move at any shift
tried. Chrome's CPU canvas rasterizes through the same Skia converter, so it is
inferred to place the same error by its own layers; that was not measured.

**Text under 254 device pixels on a GPU canvas is drawn as the CPU canvas
draws it, and Chrome's GPU canvas draws it otherwise.** Ganesh draws a glyph run from 162 to 324 device
pixels as distance fields, whose coverage moves with where in the atlas a
glyph lands, and so with what the context drew before.
`containers/skia-text-distance-fields-off.patch` draws a run under 254
device pixels as a direct mask from the scaler the CPU raster uses instead,
so such text on the GPU page is the CPU page's byte for byte
(`text_on_the_gpu_draws_as_the_cpu_draws_it_whatever_came_before`). A run of
254 device pixels and up is a path, as one above 324 already was: from 254
to 324 it was a distance field before and is drawn from its outline now,
which is not the CPU page's either. On issue
#622's page -- `italic 900 96px` Oswald under `scale(2.4)`, 230 device pixels
-- Chrome 158's CPU canvas draws our CPU page exactly, one row higher (#142).
Its GPU canvas draws the same page after every history tried, and that page
differs from its CPU one in 3,624 pixels' alpha, by up to 168 levels. At
column 375, rows 251, 253, 254 and 255 of ours and a row higher in Chrome's,
our GPU and CPU pages and Chrome's CPU page read 255, 189, 122 and 60, Chrome's
GPU page 248, 209, 152 and 62, and the distance fields this library drew
before 228, 177, 118 and 38. Which route Chrome's GPU canvas took is not
established: in that run a fill of `rgb(127.5 127.5 127.5)`, which Chrome's
GPU raster reads a byte low as 127, read 128, so the run may not have
rasterized on the GPU at all. A direct mask is the glyph's coverage at the
device transform, which no sample interpolated from a distance field
reproduces, so this library keeps it whatever Chrome's GPU canvas does.

**A COLR colour glyph is drawn here from its paint graph on every platform,
as Chrome draws it -- on the CPU, to within three levels.** CoreText
draws a COLRv1 glyph as nothing on macOS 26 and 15, and a COLRv0 glyph as
nothing on 27.2. Chrome draws every COLR web font on Apple platforms through
Fontations, so this library reads the `COLR` table with `skrifa`, Fontations'
reader, and draws the paint graph in its own code (`font::colr`), each glyph
composited through a layer of its own as Skia composites a drawable glyph. A
glyph's origin is placed as Chrome places it: floored to the device pixel
after adding 1/8 across and 1/2 down. On a CPU canvas against Chrome 158's on
macOS 27.2, every pixel of every glyph with colour paint among the 221 of
`test_glyphs-glyf_colr_1_variable.ttf` at 48px is within three levels, edges
included, one pixel at three and ten past one; with ten of its axes varied,
one at three and eight past one. The COLRv0 test font is Chrome's byte for
byte at 16, 30, 48 and 100px (`colour_fonts` in the crate's tests,
`colour-fonts.test.js` in the JavaScript suite). On Windows, against the same
references, DirectWrite drew the COLRv1 glyphs up to 215 levels inside them
and 6,034 pixels past 81, and this library's painter within one inside them
and 8 past 81, so Windows takes the same route; the COLRv0 font was the same
either way. On Linux FreeType drew the COLRv1 glyphs with ten axes varied up
to 179 levels inside them and 768 pixels past 81, and the COLRv0 font with
412 pixels past 81; this library's painter drew both sheets of COLRv1 glyphs
within one inside them and 58 past 81, and the COLRv0 font with none past 8,
so Linux takes it too. The font's 20 code points with no colour paint are
drawn by the platform here and by Fontations' outlines in Chrome, and stand
up to 215 apart on macOS.

**On a GPU canvas a COLR colour glyph is an image made on the CPU, as Skia
draws a colour glyph from the image its scaler context makes, so that the GPU
canvas composites the bytes the CPU canvas does.** Composited through a layer
of its own on the GPU, a glyph's gradients were drawn by Ganesh, which places
a conic gradient's hard stops apart from the CPU's (#667), and the test glyphs
stood up to 255 levels from the CPU canvas. The image is made where the page
is played back, at the scale and in the pixel format it is played onto, so
that an export at a density, a canvas drawn into another and a canvas pattern
draw the glyph as drawing it there would. A shadow's filter is applied to
the glyph image on the CPU, as the CPU canvas applied it to the glyph's layer
(`layer::ColourText`). On the GPU canvas the four test glyphs, standing
apart, are the CPU canvas's bytes under a shadow, a blur filter,
`source-in` and a rotation, and through a `Paragraph`.

Where the GPU blends a glyph image with what lies beneath it at an alpha --
a translucent glyph over another, over its own shadow, or under a
`globalAlpha` -- Metal blends at half precision, as it blends any image, and
parts from the exact composite only within 0.18 of a tie (see the eight-bit
composite entry above): 11 of 320 overlapping bytes on the test sheet, and
674 of 5,697 channels of the four test glyphs at a `globalAlpha` of 0.5, a
level from the CPU at most. The CPU canvas composites a glyph image through
the `globalAlpha` as Chrome's CPU canvas does, byte for byte over
`rgb(200 120 60)` at 0.5 and at 0.3 under `multiply`. Applying the alpha to
the image on the CPU first would have made the GPU canvas the CPU's there,
but stood a level from Chrome in 2,326 of 118,800 channels at 0.5 and was
not closer to the exact composite in every case: it missed it in 600 of
5,697 channels at 0.5 against Chrome's 2,436, and in 1,130 under
`multiply` against 1,117. A
`Paragraph`'s colour glyphs are rasterized on the CPU from its recording, so
they keep the device pixel they were snapped to where the paragraph was
drawn, where a canvas's own text is snapped again wherever it is played.

**A gradient on a GPU canvas is drawn in the bytes the CPU canvas draws it
in.** Ganesh's own gradient parts from the CPU's in three places: its sweep
computes the angle and `t` in half precision (`SweepLayout`), a gradient of
more than 128 colours keeps its stop offsets in half floats
(`make_buffered_colorizer`), and a radial or a translucent linear ramp lands
up to four levels away for a cause not isolated. A hard stop then moved by up
to a tenth of a degree or half a pixel, and the conic of #667 put 8 pixels on
the far side of a stop, 203 levels off. Here a draw dyed by a gradient is kept
as a `GradientDraw` and chosen where the page is played: a CPU canvas and a
document play Skia's own gradient, as before; a GPU canvas fills the draw with
an effect doing the CPU's eight-bit arithmetic in floats -- the sweep's angle
is Skia's own polynomial, which is itself up to 0.0095 degrees from the true
angle -- itself for a rectangle on whole pixels, and otherwise through an
image of it, one for the draws that follow one another, sampled nearest at
each pixel's centre; or, for a draw the effect does not reproduce, has the CPU
draw it over the GPU's pixels read back (`exact_gradient`,
`layer::GradientDraw`). The effect fills the image as an aliased rectangle on
whole pixels, the one geometry for which Ganesh hands a shader each pixel
centre's coordinates exactly: drawn directly through a path, or as a rectangle
whose edges fall inside pixels, it put 0.5% and 16% of its pixels a level off.
Over 600 generated gradients drawn as a rectangle, a rectangle off whole
pixels, a triangle and a polygon, the GPU canvas parted from the CPU on 582
pixels, every one within float noise of a half level or a stop. What stays
apart: those ties; a shape's anti-aliased edge, which is Ganesh's coverage as
for a flat colour; a gradient the CPU draws in floats -- a translucent
two-point conical one, one interpolated in a space other than the page's, one
under a filter, a shadow or a blend the CPU's eight-bit pipeline has no stage
for -- inside a layer a group opens, where the pixels cannot be read back and
the CPU's raster is composited instead, a level apart wherever its two
roundings differ from the CPU's one (9,792 of 16,384 pixels on a translucent
radial over a fill, measured by compositing at the page); a canvas drawn with
`drawImage` into a GPU canvas, whose picture Ganesh rasterizes itself (2,064
pixels, up to 203 levels, on the conic above), where `drawCanvas` keeps the
choice (#703); a GPU canvas drawn with `drawCanvas` into another under a
transform other than a whole pixel's translation, where the image would be
resampled and the draw is made on the CPU at the destination's pixels instead,
inside the layer the source is drawn through and composited through the draw's
blend -- exact inside an opaque `source-over` sweep scaled by 2 or 0.5, moved
part of a pixel or rotated; within a level through a blend the CPU makes in
eight bits (`multiply` on 365 of 16,384 pixels at a scale of 2, where Ganesh's
own gradient parted 5,301 by up to 35); and through `color-dodge`, which the
CPU makes in floats from a gradient it never rounds, the composite's rounding
of it multiplied by the dodge's division (7,091 pixels up to 29 levels at a
scale of 2, where Ganesh's own gradient parted 5,766 by up to 66); and what a
GPU canvas does with any colour -- `multiply` a level apart on 97 of 4,096
pixels of the sweep, as for an image of the same pixels. Chrome's GPU canvas
was not measured: headless Chrome drew it on SwiftShader.

**On a CPU canvas a COLR colour glyph's image composites at full alpha as
the exact source-over rounded to nearest, on every architecture; Chrome's
CPU canvas may not on x86-64.** Skia's eight-bit source-over rounds to
nearest on aarch64 and truncates on x86-64. Over the 183 colour glyph images
of `test_glyphs-glyf_colr_1_variable.ttf` at 32px, an image composited onto
an N32 page lands a level under the exact composite in 14,573 of 282,272
channels on x86-64, storing 109 for 109.706, and in none on an Apple M4 Pro.
So off aarch64 this library composites a glyph image at full alpha through
an identity colour matrix, which moves Skia onto its float pipeline, and no
channel misses (`composite_onto` in `font::colr`). Under an alpha the
composite is Skia's eight-bit one on every architecture, which is Chrome's
CPU canvas's on macOS. What Chrome's CPU canvas stores on x86-64 was not
measured; it draws through the same Skia code, so it may store the truncated
bytes. Any other N32 image, and a canvas drawn with `drawCanvas`, still
composite through the eight-bit path and truncate on x86-64: #676.

**On Mesa's llvmpipe a GPU canvas blends at eight bits, so it stands further
from the CPU canvas than Metal's does.** llvmpipe, the software Vulkan the
Linux CI binding job draws on, blends a blend Skia does by coefficients in
its fixed-function stage at eight bits, as the CPU raster does. Two plain
images composited under `source-over` at a `globalAlpha` of 0.5 land on the
CPU canvas's bytes. Of 2,392 channels, 117 lie more than 0.18 from a tie,
and none lies a level from the exact composite. Blends the shader does --
`multiply`, `darken`, `difference` -- are exact there. So on llvmpipe a
channel lies within a level of the exact composite at any distance from a
tie. Under a `globalAlpha` of 0.5 the GPU canvas stands up to two levels from
the CPU canvas, where Metal stands one. At a clip edge, whose coverage enters
the blend as an alpha does, a colour glyph stands a level from the CPU canvas
at full alpha. Overlapping images under `exclusion`
stand up to three levels from the CPU canvas there, where Metal stands two,
for COLR glyphs and plain images alike. llvmpipe is a software rasterizer
used for testing, and Chrome was not compared on it.

**`color-dodge`, `color-burn`, `hue`, `saturation`, `color` and
`luminosity` divide exactly on the CPU, the same bytes on every machine, and
Chrome's CPU canvas divides through an estimate.** Skia's raster pipeline
divides in these stages through `rcp_fast`, the instruction's reciprocal
estimate: `_mm256_rcp_ps` at the AVX2 level, `_mm512_rcp14_ps` at AVX-512,
NEON's estimate and one refinement step on arm64. An x86 machine runs the
widest level it has, so the same page drew a level apart in places on an AMD
EPYC with AVX-512 and one without (#642).
`containers/skia-exact-blend-division.patch` divides with `/`, which every
level rounds correctly, and every other operation in these stages is already
the same at each level, so a page draws the same bytes on all of them
(`the_dividing_blend_modes_draw_their_formulas_with_exact_division`). A byte
the patch moves moves by one level, to the formula's value in single
precision with exact division. On an opaque 256 by 256 ramp of every source
value over every backdrop value on an Apple M4 Pro, Chrome 158's CPU canvas
is off that value on the same 304 pixels under `color-dodge` and 4 under
`color-burn` as this library was before the patch, each by one level; this
library is off it on none. What Chrome draws on an x86 machine was not
measured: it runs the same stages, so it is expected to split by SIMD level
as this library did.

**A PDF's blend modes render as the canvas's in poppler, and macOS's
CoreGraphics renders a few of them otherwise.** A composite operation on an
sRGB canvas is written into the PDF as the blend mode it is, on a page whose
transparency group blends in DeviceRGB, the space the page's colours are
written in (`export::pdf`). `lighter`, `xor`, and `destination-atop` under a
source that may be translucent are not -- PDF has no mode for the first two,
and Skia's PDF backend writes the third 35 levels off -- so they are embedded
as pixels with what lies beneath them, as every blend is on a canvas in any
other colour space. Measured with a rectangle and a circle in
`rgb(200 40 120)` over two opaque colours, opaque and at an alpha of 0.8,
against the canvas drawn at four times the size: poppler 26.10 (`pdftoppm`)
comes within 2 levels of the canvas for every mode the PDF writes, wherever
the source covers a pixel whole. CoreGraphics, rendering through `sips` on
macOS 27.2, does too, except:

- `soft-light`, 16 levels off with an opaque source and 14 at 0.8, on every
  pixel the source covers.
- At 0.8, `color-burn` 35, `color-dodge` 11 and `source-atop` 5, over the 6
  levels CoreGraphics puts a translucent `source-over` fill off by with no
  blend at all.
- Along an antialiased edge, `color-dodge` and `color-burn` up to 215 and 200
  levels, on the edge's partly covered pixels.

**A blend Skia writes through a soft mask goes wrong in CoreGraphics inside a
group on a scaled page, so there it is embedded.** Skia's PDF backend writes
`source-in`, `source-out`, `source-atop`, `destination-in`, `destination-out`,
`destination-atop`, `copy` and `clearRect` through a soft mask made from the
draw's shape (`needShape` in `SkPDFDevice.cpp`). Inside a group -- a canvas
drawn into another, which is drawn through a layer once it holds a blend, or a
`saveLayer` -- on a page exported at a density other than 1, which scales the
page's content, CoreGraphics renders it otherwise: a canvas holding a square
cut by `destination-out`, drawn into a page over two opaque colours and
exported at density 2.5, came out with 20,000 of its 150,000 pixels more than
8 levels off the canvas, up to 160, in `sips` on macOS 27.2, where poppler
renders it right. So at a density other than 1 such a group is embedded as
pixels with what lies beneath it (`context::page::pdf_cannot`); that page then
comes out with 2,985 pixels over 8 levels, up to 89 -- the embedded image as
CoreGraphics draws it, which poppler draws 3,833 pixels apart from on the same
file. A masked blend on the page itself, and one in a group at a density of 1,
stay vector, since CoreGraphics renders both as the canvas does: the
reproduction at density 1 comes out within a level.

**A gradient drawn inside a layer, a COLR colour glyph's among them, renders
mirrored or cropped in CoreGraphics, and the PDF is the one the specification
describes.** A layer is written as a transparency-group form drawn with `Do`
under the page's y flip, and a gradient inside it as a pattern whose matrix
maps to the space in force at that `Do`, flip included. ISO 32000-1 section
8.7.2 asks for exactly that: a pattern used in a form XObject maps to "the
form coordinate space at the time the form is painted with the Do operator".
Skia's PDF backend writes it so because a layer's device carries no initial
transform of its own, where the page's carries the flip
(`SkPDFDevice::createDevice` in `SkPDFDevice.cpp`). poppler and pdf.js read it
as the specification does. CoreGraphics anchors the pattern to the page's
default space instead, so in Preview, Quick Look, Safari and `sips` a vertical
gradient in `saveLayer()` comes out upside down. A red-to-blue gradient down a
200-pixel layer, sampled 60 pixels above and below its centre:

| renderer                          | above      | below      |
| --------------------------------- | ---------- | ---------- |
| the canvas                        | 203, 0, 52 | 50, 0, 205 |
| poppler 26.11 (`pdftoppm`)        | 204, 0, 51 | 51, 0, 204 |
| pdf.js 5.4.530                    | 203, 0, 52 | 50, 0, 205 |
| CoreGraphics, macOS 27.2 (`sips`) | 51, 0, 204 | 205, 0, 50 |

A COLR glyph, each composited through a form of its own, paints its gradients
off the glyph in CoreGraphics: cropped, or missing outside the shading's
domain. On a PDF of the COLR test glyphs on macOS 27.2, 4,445 of 40,000 pixels
stand more than 32 levels off the canvas in CoreGraphics and 409 in poppler.
The glyphs stay vector. Writing the form so that every reader agrees -- the
layer's device given the page's transform and its form a matching matrix --
is a change to Skia's PDF backend, tracked as #664. PDFium is not measured.

**A canvas's pattern is written as PDF tiling where its tiles land on whole
device pixels, and poppler draws seams between the tiles.** A draw dyed by a
pattern of a canvas -- repeated both ways, the canvas whole pixels across,
composited only where it lands with no filter, colour filter or shadow, and
played in the document under a transform that scales both axes by the same
whole number and moves by whole pixels, the export's density and any canvas
it is drawn into included -- is painted from the canvas rasterized at that
scale, which Skia writes as a PDF tiling pattern
(`context::layer::tiles_exactly`). Any other is embedded as pixels as the page
composites it, and so is one whose canvas is too large to rasterize whole at
that scale, which a renderer would resample with a filter of its own. Measured over flat and busy tiles 7, 10 and
40 pixels a side, plain, scaled by 2, moved, at an alpha and under `multiply`,
`screen` and `source-atop`, at densities 1, 2 and 3, against the canvas and
against the same draws embedded: PDFium 153 within 2 levels, as it draws them
embedded; CoreGraphics no further off than it draws them embedded, up to 19
levels; poppler 26.10 a seam a pixel wide along tile edges, up to 210 levels,
over fewer pixels than it misdraws the embedded image -- 3,420 against 6,264
for a 10-pixel tile at density 1. The draw's alpha is put into the tile and
the pattern painted at full alpha, since PDFium paints a pattern fill at the
alpha the pattern cell's own graphics state sets, which Skia writes as 1, and
drew a `globalAlpha` of 0.6 up to 84 levels off. pdf.js and Acrobat are not
measured.

poppler's figures are held by `tests/pdf_blend.rs`, which renders every
blend a PDF writes with `pdftoppm` and runs on CI. CoreGraphics' are a
manual check on macOS: `sips -s format png -Z 800 page.pdf --out page.png`,
then `sips -m "/System/Library/ColorSync/Profiles/sRGB Profile.icc"
page.png`, since `sips` renders into Display P3, compared with the canvas
exported at density 4.

Without a page group CoreGraphics chose a blending space of its own and put
every separable mode 10 to 51 levels off. Before blends were written natively
the whole backdrop was embedded as one JPEG at the export's density, and the
blended source came out 8 to 38 levels off in poppler, on 7,900 to 46,000 of
its pixels at four times the size.
