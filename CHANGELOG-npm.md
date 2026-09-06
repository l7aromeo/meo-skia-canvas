# Changelog — the npm addon

Changes to the Node addon `meo-skia-canvas`, published on npm.

> Sibling files: the Rust crate's is
> [CHANGELOG-crate.md](CHANGELOG-crate.md), and
> [CHANGELOG.md](CHANGELOG.md) is the index and the combined history from
> before the two were separated.
>
> **A change that affects both surfaces appears in both files**, written for
> each audience rather than copied.

## 📦 ⟩ [UNRELEASED] (npm) / [UNRELEASED] (crate) ⟩ September 7, 2026

**The version is not yet decided and the heading is deliberately unfilled.**
This began as a patch for one colour-conversion fix and has since taken
seventy merges. Three of the entries below break the crate's public API --
`Font::italic` becoming `Font::slant`, `TextDirection` gaining `Inherit` and
moving its default onto it, and `Error::InvalidRadius` -- so it is not a
patch, and the number is the maintainer's to choose.

Nearly every entry below moves pixels or changes a value a caller reads back.
The through-line is a differential against Chrome 148: each was measured
against a browser rather than inferred from a specification, and the handful
of places where this library deliberately does _not_ follow Chrome are marked
as such with the reason.

### Breaking

> **One of these breaks silently. Every other entry below raises.**
> `ctx.direction` now returns `"inherit"` where it returned `"ltr"`, so
> `if (ctx.direction === "ltr")` takes the other branch with no error at all.
> Nothing about rendering moves, so a visual check will not find it either.
> If you compare that property anywhere, read the first entry before upgrading.

- **`ctx.direction` now reports `"inherit"`.** The HTML Standard makes
  `"inherit"` the attribute's default and a value it holds -- it names the
  surrounding document's direction, which a canvas does not have. We resolved
  it to `"ltr"` on the way in, so a fresh context reported `"ltr"` and
  assigning `"inherit"` was indistinguishable from assigning `"ltr"`. The
  attribute now carries the keyword and reports it back; `"ltr"` and `"rtl"`
  are unchanged, an unrecognised value is still ignored, and **nothing about
  layout moves** -- a canvas has no document to inherit from, so text still
  lays out left to right, which the tests measure rather than assume. Code
  comparing `ctx.direction` against `"ltr"` on a context that was never
  assigned a direction will stop matching. `lib/index.d.ts` has declared
  `CanvasDirection` as `"inherit" | "ltr" | "rtl"` throughout, so this
  produces a value that was already promised. The Rust `TextDirection` gains
  an `Inherit` variant, which is its new default.

- **A refused `Window` cursor throws instead of being discarded.**
  `win.cursor = "hand"` type-checked, assigned nothing and reported nothing,
  because the setter had no `else`. It is a `TypeError` now -- WebIDL's rule
  for a value outside an enumeration -- and the constructor path throws with
  it, since `new Window(w, h, {cursor})` reaches the same setter through
  `Object.assign`. The declared `CursorStyle` union was wrong in both
  directions and now matches the validator: `"hand"` and `"arrow"` were
  declared and refused, `"pointer"` was accepted and undeclared -- and
  `"pointer"` is the CSS UI 4 name, what winit parses, and what the Rust
  enum's own `as_css` emits.

- **A numeric style code outside its set is refused rather than defaulted.**
  `decorationStyle`, `textHeightBehavior`, `fontStyle.slant`, and the rect
  height and width styles `getRectsForRange` takes are small integers, and each parser ended in
  a catch-all that turned anything it did not recognise into the default --
  `Solid`, `All`, `Tight`. A caller reading a constant off the wrong object
  got the default style, drawn without complaint, with nothing to say the
  value had been discarded. `{ decorationStyle: 9 }` now raises
  `RangeError: Unknown decorationStyle 9 (expected 0 to 4)`, and
  `textHeightBehavior`, `fontStyle.slant` and both rect styles behave the
  same way at their own entry points -- `slant: 9` used to paint upright,
  byte for byte identical to `slant: 0`. A `RangeError` because the argument is a number and its value
  is not one the set holds. Every valid code is unaffected, including the zero
  each catch-all used to stand in for -- the arm a refusal could most easily
  have swallowed. The parsers still match on the integer and still end in a
  catch-all -- it raises now instead of substituting a default. What the
  conversion to this crate's own enums buys is one step further in:
  `to_skia` matches the enum exhaustively, so a variant added there is a
  compile error rather than a value with no integer reaching it.

- **The browser build's declarations describe the browser's types.**
  `lib/browser.d.ts` re-exported nine names from the Node build --
  `CanvasRenderingContext2D`, `CanvasGradient`, `CanvasPattern`, `Image`,
  `ImageData`, `Path2D`, `DOMMatrix`, `DOMRect`, `DOMPoint` -- while
  `browser.js` takes them off `window`, unpatched. Roughly forty-eight
  members were promised that do not exist there: nineteen on `Path2D` alone,
  nineteen on the context. `loadImage` and `loadImageData` were wrong in both
  directions and are declared locally now -- the Node overloads take a
  `Buffer` or a Sharp image, neither of which exists in a page, and
  `loadImage` resolves to an `HTMLImageElement`. Four type re-exports go with
  them, all describing members of types this build does not have.

- **`Error::InvalidRadius` is split out of `Error::InvalidRect`.** One variant
  was answering two questions: its own documentation said it carried "the
  rectangle that was rejected, **or** the one the radius described", and a
  variant that needs an "or" is two variants. `round_rect(5, 5, 30, 30, [NaN,
0, 0, 0])` reported "invalid rect: 30x30 at 5,5" -- true about a rectangle
  that is perfectly valid and false about what went wrong -- while `arc` with
  a negative radius built one out of the centre and reported edges crossed,
  `left: 25, right: 15`, describing nothing a caller wrote. Five sites move;
  `InvalidRect` keeps the one case that is genuinely a rectangle. A caller
  matching `InvalidRect` for a radius will now miss it, which is the point.

- **`Font::italic: bool` becomes `Font::slant: FontSlant`** -- `Normal`,
  `Italic`, `Oblique`, shaped like the `FontStretch` beside it, with
  `Font::italic()` kept for the common case. The crate had no representation
  for `oblique`, so `Font::parse("oblique 16px Helvetica")` round-tripped as
  `italic` while the binding reported `oblique` for the same input: two halves
  of one project disagreeing about one string. The JavaScript path has been
  asking the matcher for a genuinely different face all along.

### Changed

- **`lab()` and `lch()` resolve against D50, adapted to D65, where they used to
  skip the adaptation.** CSS Color 4 puts CIE Lab's reference white at D50
  ([section 10.1]) and requires a Bradford adaptation before sRGB
  ([section 12.1]). `csscolorparser` converts as though the components were
  already D65-referred, so every non-neutral Lab colour came out wrong by the
  distance between the two whites.

  Computed from the specification's own conversion code, both paths. The `now`
  column is also what Chrome 148 paints for the same strings through its own
  canvas -- measured, byte for byte, rather than inferred from the spec:

  ```
  input                     now              before
  lab(60% 40 -30)           [193, 117, 199]  [189, 119, 198]
  lab(30% 30 -60)           [63, 56, 167]    [31, 60, 166]
  lab(70% -30 -30)          [26, 188, 225]   [0, 188, 224]
  lab(44.36% 36.05 -58.99)  [118, 84, 205]   [101, 88, 204]
  lab(50% 0 0)              [119, 119, 119]  [119, 119, 119]
  ```

  **This changes output.** A drawing that names a colour in `lab()` or `lch()`
  renders differently than it did in 5.9.0, and a stored image compared
  pixel-for-pixel will differ. Red moves by 32 on `lab(30% 30 -60)`.

  The error is near zero on the `a` axis and grows on `b`, so it read as a
  slight shift in blues and yellows rather than an obviously wrong colour --
  and on the neutral axis the two conversions coincide exactly, so
  `lab(50% 0 0)` was the same grey either way. That last row is why it survived
  a suite that tested it.

  `oklab()` and `oklch()` are unaffected and were correct throughout. Oklab is
  defined D65-referred and has no adaptation step to get wrong; its answers
  were checked against the specification's conversion code as a control.

  Fixed here rather than upstream because the remedy was ours to choose and
  [csscolorparser-rs#14] has been open since 2022. The crate is still a
  dependency and still converts Lab this way; nothing else in this tree parses
  colour strings, so there is no second path today.

  `color()` is unaffected -- it never went through the crate, which does not
  implement the function.

- **Colour is converted the way CSS Color 4 defines it.** A gradient stop given
  in a `color()` space no longer discards the space and paints raw components,
  and `color(rec2020 ...)` goes through BT.2020's transfer function rather than
  Rec. 709's. `createImageData(sw, sh)` inherits the context's colour space
  instead of labelling the result sRGB.

- **`oblique` renders as the italic face rather than the upright one.**
  Skia's matcher does not fall back from oblique to italic, so asking for an
  oblique slant on a family with no oblique face returned the upright one:
  `oblique 64px Times` painted exactly what `64px Times` paints -- 957 inked
  pixels at centroid 44.1, against italic's 922 at 41.0 -- and the same held
  for Helvetica and Arial. Chrome 148 renders that string as the italic face,
  which is what CSS Fonts 4 asks for: an oblique request prefers an oblique
  face and falls back to an italic one before an upright one. Both routes were
  affected, `ctx.font` and the paragraph API's `fontStyle.slant`, and both are
  fixed by one rule they now share.

  The substitution is for matching only: `ctx.font` still reports `oblique`,
  and `oblique` and `italic` remain two distinct values at the parse. The cost
  is a family shipping a true oblique face _and_ a separate italic, which
  would now get the italic -- against rendering upright for every family,
  which is what it did before.

- **A gradient the standard says paints nothing now paints nothing.** Five
  shapes are defined to paint nothing -- a linear, radial or conic gradient
  with no stops, a linear one whose endpoints coincide, and a radial one with
  one centre and one radius. On 5.9.0 all five painted, in two different ways
  and at every fill size: the three with no stops covered the area in opaque
  black, and the two degenerate-geometry ones painted a solid stop colour, the
  last stop for the linear and the first for the radial. All five now leave the
  destination untouched, which is what Chrome 148 does on the same scene,
  measured rather than inferred.

  Two independent faults, and the second only became visible once the first was
  gone. `shader()` returned `None` for these, and `Paint::set_shader(None)`
  clears the shader and leaves the paint's own opaque black -- so they painted
  black rather than nothing. Returning a transparent shader fixed that and
  exposed the other: `is_opaque()` for a gradient asked whether any stop was
  translucent, which an empty stop list satisfies vacuously, so a gradient with
  no colours at all reported itself opaque. `Context2D::draw_path` discards the
  recorded content instead of painting over it when a fill covers the page
  opaquely, so a **page-covering** fill with one of these threw the page away
  and then declined to paint anything back. A fill one column narrower was
  correct throughout, which is why this survived a test that covered all five
  shapes -- it fills a transparent page and expects transparent black, and
  "painted nothing" and "erased everything" are the same pixel there. The
  clauses now live in one predicate that both callers read.

  **npm only**, for two different reasons depending on the shape. The
  no-stop case cannot be built at all from Rust: `Shader::linear_gradient`
  refuses fewer than two stops outright. The degenerate-geometry cases can be
  built, with two valid stops -- but `paints_nothing` lives on the binding's
  gradient and `src/shader.rs` has no equivalent, so a crate caller building a
  zero-length gradient still gets Skia's own answer rather than this clause.
  Neither reaches the page-covering erase either way, because a crate gradient
  is a `Shader` and `Dye::is_opaque` answers `false` for that variant.

- **Text is measured and drawn the way the Canvas standard describes it, in
  eight places that each moved pixels.** `fillText`'s `maxWidth` condensed the
  run instead of being used as a line-wrap width -- it had never been
  implemented, only plumbed, and `max_lines(1)` discarded whatever wrapped, so
  a run given a `maxWidth` it exceeded came out wrapped and truncated rather
  than condensed to fit. Kerning is no longer applied across a space,
  which Chrome suppresses without exception across fourteen letter pairs.
  `textAlign` counts the trailing letter-space, so centred text sits half a
  space left of the anchor and right-aligned text a whole space. `letterSpacing`
  adds one unit per character rather than `n - 1`. A form feed, vertical tab,
  `U+2028` or `U+2029` no longer discards the rest of the string, and tab and
  carriage return are replaced with a space before measuring.
  `actualBoundingBoxLeft` and `Right` report the ink box rather than the
  advance, and take it from the glyph outline rather than the rasterisation
  box, so they match Chrome to three decimals. `ctx.font` serialises what the
  standard specifies rather than the parse. `bolder` and `lighter` resolve
  against the inherited weight -- 700 and 100 from a base of 400 -- rather than
  by a fixed table that gave 800 and 300.

- **`outlineText` fills where `fillText` draws.** For any run kerned through a
  font's legacy `kern` table -- which on macOS includes Helvetica and Times --
  Skia reports each glyph half a kern to the right of where its own painter
  puts it, through `extended_visit`, `get_path_at` and `get_rects_for_range`
  alike. The positions are reconstructed and the result checked against the
  run's own advance, so a script whose positioning the reconstruction cannot
  model -- Arabic's cursive and mark positioning -- keeps the reported ones
  untouched. Reproduced against bare Skia with none of this library in the
  path; a report is with the maintainer.

- **Geometry answers the question that was asked.** `isPointInPath` and
  `isPointInStroke` no longer map the query point through the current
  transform, which the standard forbids twice, once per method. `drawImage`
  draws the rectangle a negative destination or source extent describes,
  sorted rather than mirrored, as Chrome does. An undimensioned SVG is
  contained in the 300x150 default object size rather than hung from its
  height, including when only one dimension is stated.

- **Pixel reads are bounded in the space they are measured in.** A
  `density`-scaled `getImageData` whose crop landed exactly on the ink returned
  nothing, because a crop in device pixels was tested against bounds in canvas
  units. Region arithmetic near the right edge of a very wide canvas is
  computed in `f64` rather than `f32`, so a six-pixel request stops returning
  seven. A canvas dimension is clamped to the largest integer `f32` holds
  exactly, so `canvas.width` never reports a size the raster does not have --
  16777219 used to read back as 16777220, a canvas wider than the caller asked
  for whose extra column exists.

- **Values convert as the IDL says.** `canvas.width = "abc"` gives 0 rather
  than 300, with `25.7` truncating to 25 and `4294967296` wrapping to 0 --
  while the constructor and `newPage` keep the rule that an argument which
  cannot be used takes the default, because a `<canvas>` with an unparseable
  `width` attribute is 300 wide and not 0. A partial `DOMMatrixInit` keeps the
  3D cells it names instead of discarding all ten, so a perspective transform
  built from a dictionary is no longer silently flattened to 2D.
  `roundRect` no longer throws on a non-finite argument where its eight
  neighbours no-op, and a `Symbol` argument is refused rather than silently
  ignored.

- **Exceptions follow a rule that is now written down.** Where the Canvas
  standard names a `DOMException`, one is raised: `IndexSizeError` for a colour
  stop outside `[0, 1]`, for a zero or negative `ImageData` dimension and for a
  buffer whose length does not match the width; `SyntaxError` for a colour that
  will not parse and for an unknown `createPattern` repetition;
  `InvalidStateError` for a buffer length that is not a whole number of pixels.
  A value outside an enumeration is a `TypeError`, a sequence of the wrong
  length is a `TypeError`, and a number outside a permitted set stays a
  `RangeError`. Six sites moved to it and five more followed in the JavaScript
  layer; `roundRect`'s `RangeError` is kept deliberately, because its own
  clause in the standard names one where its three siblings name a
  DOMException, and Chrome agrees.

### Added

- **`Affine::inverse` and `Affine::multiply`.** A Rust caller could not invert
  a transform at all, and could compose two only by routing the composition
  through a `Context2D` -- which meant touching a context they might not want
  to disturb. `inverse` returns an `Option`, where `DOMMatrix.inverse()`
  answers with a matrix full of `NaN`, so a singular transform cannot be
  carried into a draw by accident. `multiply` follows `DOMMatrix.multiply`'s
  operand order.

- **`TextStyleInput.locale` and `TextStyleInput.strokeWidth` are declared.**
  Both were read and used -- `strokeWidth` reaching `paint.set_stroke_width`
  -- while TypeScript called them invalid.

- **`baselineShift` reaches the paragraph API**, which the crate applied when
  converting a text style and the paragraph path never assigned. Negative
  lifts the run and positive drops it -- measured against an isolated
  superscript rather than read off the declaration. The paragraph grows to
  contain the moved run, so the line box is not preserved in either
  direction. The shift is relative to the line, so it shows only against a
  run that did not move: shift every run on a line by the same amount and the
  glyphs and the paragraph's baseline move together and cancel, which is why
  the obvious single-run test of it proves nothing.

- **`measureText` reports `height`**, the laid-out height including line
  spacing. It is not derivable from `lines`, whose heights are the ink join.

- **`ColorChannel` declares the four long forms** -- `red`, `green`, `blue`,
  `alpha` -- which the runtime has always accepted alongside the single
  letters.

- **`clear`, `destination` and `modulate` are declared for
  `globalCompositeOperation`.** All three have always been accepted;
  `modulate` is the deliberate divergence from upstream recorded in the
  contributor guide, and until now its only record anywhere was that
  paragraph of prose -- absent from the declared union and from every test.

### Fixed

- **Unknown keys in export and window settings are refused under
  `SKIA_CANVAS_STRICT`**, as text-style keys already were. The binding took
  real trouble to reject `chromaSampling` on a PNG with a bespoke message,
  and a one-letter typo walked past it silently. The check sits where the
  caller's keys are still visible: `exportOptions` rebuilds its object from
  named locals, so an invented key never reached Rust at all.

- **The wrapper's own verbs are off the classes it backs.** `alloc`, `init`,
  `prop`, `ref` and a dispatcher were callable by name on eleven public
  classes and declared nowhere, and the accessor holding the Neon box was
  keyed by a registered symbol -- reachable from any module in the process.
  `ref` was storing every retained JavaScript object the same way. All are
  module-local symbols now.

- **`Path2D`'s dispatch helpers are no longer class statics**, and
  `DOMMatrix.isMatrix3`, `isMatrix4` and `dump` are no longer reachable: a
  `console.log` helper was public surface that nothing declared.

- **`ImageData.prototype` is no longer declared** as an instance member. It
  described something real in the wrong place -- every class has a
  `prototype`, on its constructor -- so `const p: ImageData = d.prototype`
  compiled clean under `strict` and handed back `undefined`.

- **The blend parsers assert that every arm can be reached.** Both match on
  the argument after lowercasing, so an arm carrying a capital can never
  match -- which has happened before, and the existing assertion against it
  guards table-driven paths only. The test reads the arms out of the source
  rather than listing them, because a list goes stale in the direction that
  hides the defect.

- **The two `blend` constructors agree on their parameter type.**
  `ColorFilter`'s declared `mode: string` type-checked nothing while
  `ImageFilter`'s declared the union, for one shared parser.

- **`SamplingMode`'s mapping exists once** rather than in three places, with
  the tests that pin Mitchell-Netravali pointing at it.

- **The browser build is checked against the Node build in both directions.**
  Every Node export is now either carried by the browser build or named with
  a reason, and an excuse for a name the Node build no longer exports fails
  too. Nothing checked that before, which is how `TextMetrics` came to be
  absent from a list of absences.

- **`new Window(w, h, { visible: false })` opens a hidden window.** It opened a
  visible one that took focus. The window is built hidden so it can be placed
  before it is seen, and showing it afterwards is right -- but it was shown
  unconditionally, so the option was honoured at construction and discarded a
  step later. Every window the windowing suite opens asks to be hidden and
  every one appeared. The call also sat inside the block that positions the
  window, guarded on two queries that can both fail, so a platform answering
  neither would have left a `visible: true` window hidden with nothing to say
  why; visibility is now decided once, from the spec, outside that block.

- **Three comments corrected to describe the code as it stands**: the refusal
  marker's dividing line is _coerces to a number_ against _throws on
  coercion_, not _number_ against _not a number_; `just test` always builds
  the addon rather than only when the file is missing; and `makeMerge` does
  not take a `cropRect` to refuse.

### Internal

- **Nine enum parsers produce this crate's types rather than Skia's.**
  `ColorChannel`, `TileMode`, `BlurStyle`, `GradientColorSpace`, `HueMethod`,
  `StrokeCap`, `StrokeJoin`, `FillRule` and `BlendMode` were parsed into
  `skia_safe`'s enums of the same name, across ten parsers -- `BlendMode` has
  two, `to_blend_mode` and `to_filter_blend_mode`. So the public Rust enum and
  the strings JavaScript accepts were two independent translations with no
  code path in common: drift between them was possible by construction, and
  any agreement was coincidence.

  **Nothing observable changed on either channel**, which is why this is here
  and not above. No string vocabulary moved and every refusal message is
  byte-identical, proven variant by variant before the conversion. The public
  enums are unchanged -- all nine carry the same variants they did at
  `rust-v0.15.0` -- and the parsers themselves are not reachable from Rust at
  any feature set, since `node` is a `pub(crate)` module and `node-addon`
  registers the Neon entry point rather than exporting it. `FillRule` narrows
  the parser's return type and not the vocabulary: Skia's `PathFillType`
  carries two inverse fills that no Canvas name reaches, and the public
  `FillRule` was already two-valued. `to_path_op` had made the same choice
  earlier and says why at its own definition.

- **A short `bun.lock` is now repaired by the release recipe rather than only
  avoided.** Publishing 5.9.0 stopped between the platform packages and the
  main package on a lockfile pinning four of seven targets, and would not clear
  on a retry: bun does not re-resolve a lockfile it considers satisfied, and an
  optional dependency missing from one does not make it unsatisfied. Neither
  `--force` nor `--no-cache` adds the missing entries. The step now resolves
  from `package.json` instead, so a resumed release fixes itself.

- **Which cubic `imageSmoothingQuality = "high"` uses is pinned**, at all three
  call sites and by a rendering test as well as by its coefficients: swapping
  Mitchell for CatmullRom used to pass the entire suite.

- **A single-line draw no longer copies the string it was given.** The
  normalisation that replaces hard breaks returns a `Cow`, so the common case
  borrows -- which its own doc comment had claimed all along.

- **A panic inside a Skia visitor closure aborts the process.** It crosses a
  C++ trampoline that cannot unwind, so `SIGABRT` takes the whole test binary
  and reports nothing about any other test. Two test sites moved their
  assertions after the walk.

[section 10.1]: https://www.w3.org/TR/css-color-4/#cie-lab
[section 12.1]: https://www.w3.org/TR/css-color-4/#color-conversion-code
[csscolorparser-rs#14]: https://github.com/mazznoer/csscolorparser-rs/issues/14
