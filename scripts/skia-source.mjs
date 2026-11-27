#!/usr/bin/env node
// The Skia source tree macOS, Linux and Windows builds compile from, with
// this platform's patches from `PATCHES` applied.
//
//   node scripts/skia-source.mjs env       `SKIA_SOURCE_DIR=<dir>` on a platform with patches, nothing elsewhere
//   node scripts/skia-source.mjs dir       the directory, prepared or not
//   node scripts/skia-source.mjs prepare   make it once, from a tree already here or the network; print the directory
//   node scripts/skia-source.mjs apply <checkout>   apply this platform's patches to a rust-skia checkout
//   node scripts/skia-source.mjs cache-key  the key CI caches this platform's tree under, alone on stdout
//
// `apply` and `cache-key` are how CI reaches `PATCHES` where it does not build
// through `just`: the Linux release build patches its own rust-skia checkout,
// and a cache key has to be known before the tree it names is restored.
// `scripts/check-skia-patches.mjs` refuses a workflow that names a patch file
// instead.
//
// Against a local rust-skia checkout (`just use-local-skia`) both `env` and
// `prepare` do nothing: skia-bindings builds the checkout's own Skia.
//
// `env` is the justfile's `dotenv-command`, so every recipe on a platform with
// patches builds skia-bindings offline from this tree. It names the directory
// and holds the tree for the `just` that ran it -- see `hold` -- and it runs on
// every `just` invocation, `just --list` included, so it must never download.
// `prepare` is what downloads, and `ensure-deps` runs it.
//
// The tree is rust-skia at the tag of the skia-bindings version in Cargo.lock,
// whose Skia submodule is pinned by that tag, with Skia's own DEPS synced and gn
// fetched at the revision `bin/fetch-gn` names. It lives once per version and
// patch list under `$XDG_CACHE_HOME/meo-skia-canvas`, so a new version or a changed
// patch gets a new directory and a stale one cannot be reused. `prepare` makes
// one under its lock -- see `lockTree` -- so two worktrees' `just` make it once.
// Once a new one is ready, the others are removed unless something still uses
// one -- the cache is shared by every worktree on the machine: a worktree whose
// patch list names it, a skia-bindings out-dir in a worktree's target that was
// built from it, or a `just` holding it -- see `treesToKeep`.
//
// Every tree for a version holds the same checkout, dependencies and gn, and
// differs from the others only in the patches applied to tracked files. So a
// new tree is made from one already in the cache where one passes `derive`'s
// checks -- copied, its sources restored by git, then patched -- and fetched,
// about 1 GB, only where none does.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  constants,
  cpSync,
  existsSync,
  linkSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildScriptLogs,
  buildScriptRuns,
  profileBuildDirs,
} from "./cargo-build-dir.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
/**
 * The patches applied to the tree, by `process.platform`, in order and
 * relative to a checkout of this repository. A platform with none builds
 * skia-bindings as it always has, and `env` names no tree there.
 */
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_path_covers_its_pixels_wherever_the_atlas_places_it -- --include-ignored against crates.io's Skia, where it is ignored for this; the rounded rectangle reads alphas a step off a flush holding it alone after 1, 2, 3, 6 and 7 copies on Metal
//
// Ganesh's atlas path renderer maps a path's points into atlas space with
// the path's atlas location added into the matrix's translate, and evaluates
// its curves on the GPU in those coordinates. Float rounding at the
// magnitude of the atlas location then decides where a vertex lands, so a
// path's coverage moved a step -- about 50 of 255 levels of alpha -- with
// where the flush packed it: on a page of 2000 small stars, 28 pixels read
// above or below what a flush of the stars reaching them draws, and an
// eyelash drawn after blurred shapes lost its tip. Seen on Metal and on
// lavapipe (Mesa 25.0.7, arm64), so it is the renderer's and not a
// driver's. The patch maps each path relative to its own device top-left,
// rounds every point it writes to a 2^-12 pixel grid, and only then adds
// the whole-pixel location; the shader evaluates curves relative to their
// first point and rounds the same way. Every atlas coordinate is under 2048,
// so the sums are exact and a path's vertices are the same at every
// location. The atlas's own size mattered too: its vertex stage maps to
// normalized device coordinates as x * 2 / size - 1, inexact for a size that
// is not a power of two, so on lavapipe (Mesa 26.0.8, x86_64) a crowded flush
// and a small one, whose atlases differ in size, read 10 pixels apart with
// the rounding and 2 without. The patch sizes each side of the atlas to a
// power of two, and they read the same. Each part is the one that matters
// on one driver: without the rounding Metal reads the crowded page 28 pixels
// off, and power-of-two sides on the unpatched mapping changed nothing
// there; lavapipe passes it with power-of-two sides alone. The atlas's
// backing store is already rounded up past 1024 to 1536 or 2048, so only a
// side between 1025 and 1536 grows, to 2048: at worst 1.78 times the pixels.
const ATLAS_PLACEMENT = join("containers", "skia-atlas-placement.patch");
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_curve_after_a_larger_draw_of_it_draws_alike_where_skia_is_patched against crates.io's Skia, as the crates.io publish runs it; it holds that build to moving bytes, 416 by up to 128 levels on lavapipe, and fails there once Skia reuses a triangulation only at its own tolerance
//
// Ganesh's triangulating path renderer keeps a path's triangles in the
// context's thread-safe cache, keyed by the path's shape and not by the
// transform, and reused an entry made at any tolerance under three times the
// draw's. A draw of a path at another scale left a triangulation, finer or up
// to three times coarser, that a later draw of it then took, so under 4x MSAA
// the same draw covered its edges by up to two samples' worth -- 128 levels --
// according to what the context had drawn before. The patch reuses an entry only at the draw's own
// tolerance.
const TRIANGULATION = join("containers", "skia-triangulation-tolerance.patch");
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: look in rust-skia's skia-safe/src/gpu/graphite for an `ImageProvider`, `fInternalMultisampleCount` and the image form of `asyncRescaleAndReadPixels`; once all three are bound, drop this patch and build against crates.io's
//
// Not a Skia patch: it adds to rust-skia's own sources the three Graphite
// bindings float pages need -- a caching `ImageProvider`, without which a
// Recorder drops every raster image; `ContextOptions::fInternalMultisampleCount`,
// for the raster-atlas route; and reading a Graphite image back through the
// Context. A build takes them only through the `[patch.crates-io]` config
// `cargo-patch` writes, and the code calling them only through
// `cfg(meo_graphite)`, which the patched skia-bindings alone turns on: a build
// against crates.io's skia-bindings compiles none of it.
const GRAPHITE_BINDINGS = join(
  "containers",
  "rust-skia-graphite-bindings.patch",
);
// UPSTREAM: skia-safe 0.156.0 -- https://issues.skia.org/issues/40042671 -- worked around
// Re-check: cargo test a_read_leaves_its_transfer_buffer_where_skia_keeps_one against crates.io's Skia, as the crates.io publish runs it; it holds that build to keeping no buffer, and fails there once `SurfaceContext::transferPixels` stops asking for `kStream_GrAccessPattern`
//
// Every `getImageData` on a GPU page goes through Skia's asynchronous read,
// whose transfer buffer `SurfaceContext::transferPixels` asks for as
// `kStream`. `GrResourceProvider::createBuffer` makes such a buffer fresh
// and never caches it, so each read allocates a Metal buffer and frees it:
// about 12 microseconds of a 64x48 read on an M4 Pro. The patch asks for
// `kDynamic`, which `createBuffer` bins by size and finds again, as Metal's
// and Vulkan's synchronous `onReadPixels` already do with theirs. Skia keeps
// `kStream` for the bug above: the switch crashed Chromium's GPU process in
// one web test on a Windows bot, with no cause found. On Metal and Vulkan the
// two patterns get the same memory -- `GrMtlBuffer` gives every non-static
// buffer the same storage mode, and `GrVkBuffer` the same mappable memory to
// every GPU-to-CPU transfer -- so the patch changes only whether the buffer
// is kept. Only GL, which this tree does not build, also changes its usage
// hint. A cached buffer stays out of reach while its read is held, because
// `GrClientMappedBufferManager` keeps a reference until it unmaps it.
const ASYNC_TRANSFER_REUSE = join(
  "containers",
  "skia-async-transfer-reuse.patch",
);

// The Metal compile timeout -- see the UPSTREAM note in the justfile.
const METAL_UNTIMED_COMPILE = join(
  "containers",
  "skia-metal-untimed-compile.patch",
);

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: link a Linux build with `freetype` from a tree prepared without this patch, with build.rs's shim returning at once; it fails on undefined `kCmdLut` and `BrotliDecoderEnsureStaticInit`
//
// Skia's `third_party/brotli/BUILD.gn` leaves out `c/dec/prefix.c` and
// `c/dec/static_init.c`, which define `kCmdLut` and
// `BrotliDecoderEnsureStaticInit` since the brotli roll in M156's DEPS.
// Only `freetype-woff2` links brotli and skia-bindings drops it on macOS,
// so the patch is Linux's alone. build.rs carries the same fix for builds
// that do not come through here, and stands aside where this patch has
// applied.
const BROTLI_SOURCES = join("containers", "skia-brotli-sources.patch");

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: node scripts/test.mjs tests/suite/text.test.js on Windows, against a binary built from a tree prepared without this patch; "a face flagged oblique is reported slanted" fails
//
// Skia's DirectWrite port takes a face's slant from DirectWrite alone
// (`DWriteFontTypeface::GetStyle`), and DirectWrite reports a face
// flagged oblique only by `OS/2` `fsSelection` bit 9 as NORMAL. Skia's
// FreeType port reads the bit itself and reports the face oblique, so on
// Windows such a face matched as its family's upright one: an oblique
// request got the upright face sheared rather than the oblique face CSS
// Fonts 4 matches. The patch reads the bit as the FreeType port does.
// Text chooses between such faces from their own tables now --
// `FontLibrary::slanted` in src/node/font_library.rs -- so what the patch
// still earns is the slant reported: what `FontLibrary.family().styles`
// lists, and what an SVG document's `font-style` is matched against.
const DWRITE_OBLIQUE = join("containers", "skia-dwrite-oblique.patch");

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: node scripts/test.mjs tests/suite/image.test.js against a binary built from a tree prepared without this patch; "a document's slanted text in a family with no slanted face is slanted as canvas text is" fails
//
// Skia's SVG text draws with `SkFont(typeface, size)` and never sets the
// font's skew (`ResolveFont` in modules/svg/src/SkSVGText.cpp), so an
// `italic` or `oblique` in a family with no slanted face draws upright, where
// CSS Fonts 4 section 5.2 synthesises the slant. And its attribute parser
// reads `font-style` as four keywords, so `oblique 10deg` fails to parse and
// takes the inherited style. The patch parses the angle and keeps it in an
// inherited presentation attribute of its own, `fFontObliqueAngle`, which
// `font-style` sets with the slant. It does not widen `SkSVGFontStyle`:
// skia-safe builds that struct field by field, and a field it does not name
// fails its build. `ResolveFont` skews a face that does not lean -- by its
// reported slant or by `OS/2` `fsSelection` bit 9 -- a quarter for `italic`
// and to the tangent of the angle for `oblique`, as canvas text does. An
// oblique of zero or a negative angle is matched as upright and sheared.
const SVG_SYNTHETIC_SLANT = join(
  "containers",
  "skia-svg-synthetic-slant.patch",
);

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: node scripts/test.mjs tests/suite/image.test.js against a binary built from a tree prepared without this patch; "keeps a wide colour on a wide canvas" fails
//
// Skia's SVG module holds a paint as `SkSVGColorType`, an eight-bit sRGB
// `SkColor`, so a colour outside sRGB is clipped before it is drawn: on a
// display-p3 canvas `color(display-p3 0 1 0)` read 117, 251, 76 where Chrome
// keeps 0, 255, 0. The patch keeps an `SkColor4f` in extended sRGB beside
// `SkSVGColor`'s eight-bit one, and its parser reads `color(srgb r g b / a)`
// unclamped, which is the form `colour_for_skia` in src/image.rs writes a
// paint, a gradient stop and a flood colour in when the build carries the
// patch. Paints, gradient stops and `feFlood` draw from the wide colour, and
// `feFlood` multiplies the colour's alpha by `flood-opacity`, as Filter
// Effects 1 section 9.13.2 says, where Skia replaced it. It does not change
// `SkSVGColorType`: skia-safe reads `SkSVGColor`'s eight-bit `fColor` field
// by name, so the field stays, and the `color` property stays eight-bit.
//
// In a linearRGB primitive, the initial one, Skia took `flood-color` and
// `lighting-color` as linear values and encoded them once more, so #808080
// flooded and lit at 188 where Chrome 156 draws 128 (#826).
// `SkSVGFe::resolvePrimitiveColor` takes both into linear light. A flood is
// a colour filter on the source rather than a shader, so it reaches the page
// in float rather than through an eight-bit layer. The lighting filter's
// light is float, an `SkV3` on the 0-255 scale, so its `SkColor` entry
// points -- the CanvasKit-parity filters in src/filter.rs -- light exactly as
// before, and `SkSVGLighting` beside them takes an `SkColor4f` for the SVG
// module, declared where it is called so that no public header changes.
// `flatten` still writes the light as an `SkColor`, rounding a float one:
// this library flattens no image filter -- it serialises a colour filter
// alone, as a cache key -- and reads no flattened picture back, so only a
// caller using skia-safe directly against these archives reaches it.
const SVG_WIDE_COLOUR = join("containers", "skia-svg-wide-colour.patch");

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_line_paints_no_fill against a build from a tree prepared without this patch; it holds that build to the hairline, and fails there once Skia draws no fill
//
// Skia's SVG module draws a `<line>` with `SkCanvas::drawLine` for every
// paint the shape takes, its fill included, and `drawLine` strokes whatever
// paint it is given, so the initial black fill drew as a hairline along the
// line: alone where there is no stroke, and through a translucent one
// (#825). A line has no interior, and Chrome paints its fill nowhere. The
// patch makes `SkSVGLine::onDraw` draw nothing for a fill paint.
const SVG_LINE_FILL = join("containers", "skia-svg-line-fill.patch");

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: node scripts/test.mjs tests/suite/text.test.js against a binary built from a tree prepared without this patch; "a family's italic face is drawn for italic whatever its weight" fails
//
// `SkFontStyleSet::matchStyleCSS3` (src/core/SkFontMgr.cpp) scores a
// face's slant and then shifts it by 8 bits before adding a weight score
// that reaches 1000, so a face far closer in weight outranks one whose
// slant is the one asked for -- an italic request took a family's upright
// 400 face over its italic 900 one, and on a platform that reports a face
// flagged by `OS/2` `fsSelection` bit 9 oblique, its oblique 400 face over
// a variable italic face whose default instance is 100. CSS Fonts 4
// section 5.2 narrows by style before weight, and the comment above that
// shift says the slant "takes priority over all valid weights". The patch
// shifts by enough bits to hold the whole weight score.
const FONT_MATCH_SLANT_FIRST = join(
  "containers",
  "skia-font-match-slant-first.patch",
);

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test text_on_the_gpu_draws_as_the_cpu_draws_it_whatever_came_before against crates.io's Skia, as the crates.io publish runs it; it holds that build to GPU text that differs from the CPU's, and fails there once Ganesh stops drawing canvas text as distance fields
//
// Ganesh draws a glyph run whose device size is from 162 to 324 pixels as
// distance fields (`SubRunControl` in src/text/gpu/SubRunControl.cpp), and a
// distance-field glyph's coverage moves by a level or two with where in the
// atlas it was placed, which follows from what the context drew before:
// the same `fillText` exported after other text read differently from one
// exported first, on its own thread or another's. The blob cache also keys
// such a run without its matrix (`TextBlob::Key::Make`), so a path blob
// cached at a larger scale stood in for a later draw at a smaller one, up
// to 200 levels off. The patch turns distance fields off unless a surface
// asks for device-independent fonts, which none of this crate's do: below 254
// device pixels a glyph is a direct mask from the scaler the CPU raster
// uses, the same bytes as the CPU page, and at 254 or more a path, as a run
// above 324 already was.
const TEXT_DISTANCE_FIELDS_OFF = join(
  "containers",
  "skia-text-distance-fields-off.patch",
);

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test colour_bitmaps_after_a_larger_draw_draw_afresh against crates.io's Skia on a GPU machine; it holds that build to a paragraph's colour bitmaps drawn from an earlier scale's masks, and fails there once a transformed mask is reused only at the matrix it was made for
//
// A glyph with no outline -- a colour bitmap such as an emoji -- at 254
// device pixels or more is drawn from a transformed mask, a bitmap made for
// one matrix, and `TransformedMaskSubRun::canReuse`
// (src/text/gpu/SubRunContainer.cpp) let a cached blob of them be drawn at
// any other scale. With the blob cache keying such a run without its matrix,
// emoji drawn large at one scale were drawn from those masks at every other,
// up to 117 levels off a fresh draw. The patch reuses the masks only at the
// scale, rotation and skew they were made for, compared exactly; translation
// alone still reuses, and a blob only rebuilds when that matrix changes. Not
// covered: a sub-run read back from a buffer, as a slug is, has no matrix to
// compare and keeps the old rule. This crate makes no slugs. Without the
// patch, `KEYS_OWN_COLOUR_BLOBS` in src/node/typography.rs draws such runs
// from blobs Ganesh keys by their matrix, on every route but the Paragraph
// API, whose blobs SkParagraph builds; that route is why the patch stays.
const TRANSFORMED_MASK_REUSE = join(
  "containers",
  "skia-transformed-mask-reuse.patch",
);

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test the_dividing_blend_modes_draw_their_formulas_with_exact_division against crates.io's Skia, as the crates.io publish runs it; it holds that build to bytes off the exact quotient, and fails there once these stages divide exactly
//
// The CPU raster's colour-dodge, colour-burn, hue, saturation, color and
// luminosity stages (src/opts/SkRasterPipeline_opts.h) divide through
// `rcp_fast`, which outside the scalar and SSE2 builds is the instruction's
// reciprocal estimate: `_mm256_rcp_ps` at the AVX2 level, `_mm512_rcp14_ps`
// at AVX-512, NEON's estimate and one refinement step on arm64.
// `SkGraphics::Init` moves an x86 machine onto its widest level, so the same
// page drew a level apart in places on an AMD EPYC with AVX-512 and one
// without (#642). The patch divides with `/`, correctly rounded at every
// level. Skia builds with `-ffp-contract=off`, and an explicit `mad` is a
// fused multiply-add at both the AVX2 and the AVX-512 level, so every other
// operation in these stages is the same correctly rounded one at those two
// levels too, and the bytes are the same between them. The SSE level computes
// an explicit `mad` in two roundings (`fuses_mad` in tests/blend_division.rs),
// so a machine without AVX2 is not held to those bytes; how far it lands from
// them has not been measured.
//
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
// Re-check: cargo test a_runtime_blender_cannot_fuse_a_multiply_add
//
// No route in the crate's own code gives the patched stages' bytes. A runtime
// blender dividing with `/` cannot fuse a `mad` as the stages do, since SkSL
// declares `fma` in its GPU module (src/sksl/sksl_gpu.sksl) and not for a
// runtime effect. Measured on arm64 against tests/blend_division.rs's
// reference, which fuses where the stages do: over an image, 35 of its 36
// mode-and-input cases drew no pixel off it, and one drew a pixel a level
// off (saturation over the mixed pixels from seed 642). Under an
// anti-aliased edge, over a backdrop whose alpha varies, 31 to 128 of 16384
// pixels a level off the patched stages, in every mode. A blender would draw
// the same bytes at every SIMD level, SSE's included, which the patch does
// not; taking it would change the patched build's pixels.
const EXACT_BLEND_DIVISION = join(
  "containers",
  "skia-exact-blend-division.patch",
);

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_paint_alpha_scales_an_image_with_each_product_rounded_once against crates.io's Skia on x86-64, as the crates.io publish runs it; it holds that build to bytes off the rounded product, and fails there once the stage rounds exactly
//
// The CPU raster's eight-bit pipeline (src/opts/SkRasterPipeline_opts.h)
// scales a colour by a paint's alpha in `scale_1_float`, through `div255`:
// on NEON the exact two-step division by 255, elsewhere `(v + 255) >> 8`,
// which the file's own comment says is a level off about a quarter of the
// time. So an image drawn at an alpha below one landed apart on x86 and
// arm64: over every premultiplied source alpha, every opaque backdrop and
// every paint alpha, 16,945,540 of 67,108,864 channels on an x86-64 machine
// were up to two levels off the product rounded once and composited, where
// arm64 was off none (#828). The patch adds `scale_1_float_accurate`, the
// same stage through `div255_accurate`, and appends it where a paint's alpha
// is applied: the raster-pipeline sprite blitter, a shader's paint colour
// and `drawAtlas`. Coverage keeps `scale_1_float`, which
// `LOWP_EXACT_COVERAGE` makes exact on its own, and the N32 sprite rows,
// `SkBlendARGB32` and its SIMD forms, are not changed.
const LOWP_PAINT_ALPHA = join("containers", "skia-lowp-paint-alpha.patch");
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test coverage_scales_and_lerps_with_each_product_rounded_once against crates.io's Skia on x86-64, as the crates.io publish runs it; it holds that build to bytes off the rounded products, and fails there once every route rounds exactly
//
// The CPU raster's eight-bit pipeline (src/opts/SkRasterPipeline_opts.h)
// applies an antialiased edge's, a mask's and a clip's coverage through
// `div255`: on NEON the exact two-step division by 255, elsewhere
// `(v + 255) >> 8`, a level off about a quarter of the time. Before a blend
// that takes coverage as alpha -- source-over among them -- it scales the
// colour in `scale_1_float`, `scale_u8`, `scale_native` or `scale_565`; after
// one that cannot -- `copy`, `source-in`, `multiply`, a runtime blender -- it
// lerps from the backdrop through `lerp`. So the same edge landed apart on x86
// and arm64: a colour glyph written into an SVG document and drawn back landed
// up to two levels from the canvas's on x86-64 and one on arm64 (#836). The
// patch divides each of those through `div255_accurate`. Only partly covered
// pixels take these stages, and the blends keep their own division:
// source-over's is already exact, and `multiply`'s, `source-atop`'s, `xor`'s
// and the other modes' stay `div255` on x86. `scale_1_float_accurate`, which
// `LOWP_PAINT_ALPHA` adds for a paint's alpha, is the same arithmetic as
// `scale_1_float` once this applies; both patches keep it, so each can be
// dropped alone.
const LOWP_EXACT_COVERAGE = join(
  "containers",
  "skia-lowp-exact-coverage.patch",
);
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_glyph_advances_its_units_times_the_size against crates.io's Skia on Linux; it holds that build to FreeType's 16.16 excess, and fails there once FreeType's advance is exact
//
// Skia's FreeType port (src/ports/SkFontHost_FreeType.cpp) takes a glyph's
// unhinted advance from `linearHoriAdvance`: its units times a 16.16 scale
// FreeType rounds once, so a 1000-unit em at 16px advances 16 + 2/65536, and
// the error grows with every glyph on a line (#695). The patch scales the
// glyph's unscaled advance by the size the face is set to -- the 26.6 one
// the outlines are drawn at -- and rounds once to 16.16, as HarfBuzz scales
// one. A variable face's advance still takes its `HVAR` delta rounded to a
// whole unit, which is FreeType's `FT_LOAD_NO_SCALE` reading and not
// Chrome's.
const FREETYPE_ADVANCE = join("containers", "skia-freetype-advance.patch");

// Not a fix: one function, `meo_skia_canvas_needs_its_patched_skia_see_SKIA_BINARIES_URL`,
// in a file every build compiles. A crate build that names this repository's
// archives in SKIA_BINARIES_URL calls it (`cfg(meo_skia_marker)`), so one that
// fell back to crates.io's Skia fails to link instead of drawing with stock
// Skia, and says in the symbol's name what to check.
const PATCH_MARKER = join("containers", "skia-patch-marker.patch");

export const PATCHES = {
  darwin: [
    METAL_UNTIMED_COMPILE,
    ATLAS_PLACEMENT,
    TRIANGULATION,
    GRAPHITE_BINDINGS,
    ASYNC_TRANSFER_REUSE,
    SVG_SYNTHETIC_SLANT,
    SVG_WIDE_COLOUR,
    SVG_LINE_FILL,
    FONT_MATCH_SLANT_FIRST,
    TEXT_DISTANCE_FIELDS_OFF,
    TRANSFORMED_MASK_REUSE,
    EXACT_BLEND_DIVISION,
    LOWP_PAINT_ALPHA,
    LOWP_EXACT_COVERAGE,
    PATCH_MARKER,
  ],
  linux: [
    BROTLI_SOURCES,
    ATLAS_PLACEMENT,
    TRIANGULATION,
    GRAPHITE_BINDINGS,
    ASYNC_TRANSFER_REUSE,
    SVG_SYNTHETIC_SLANT,
    SVG_WIDE_COLOUR,
    SVG_LINE_FILL,
    FONT_MATCH_SLANT_FIRST,
    TEXT_DISTANCE_FIELDS_OFF,
    TRANSFORMED_MASK_REUSE,
    EXACT_BLEND_DIVISION,
    LOWP_PAINT_ALPHA,
    LOWP_EXACT_COVERAGE,
    FREETYPE_ADVANCE,
    PATCH_MARKER,
  ],
  // No brotli patch: the published Windows build has no `freetype`, so
  // nothing links brotli, and a `freetype` build here takes build.rs's own
  // fix for it as one outside this script does.
  win32: [
    ATLAS_PLACEMENT,
    TRIANGULATION,
    GRAPHITE_BINDINGS,
    DWRITE_OBLIQUE,
    SVG_SYNTHETIC_SLANT,
    SVG_WIDE_COLOUR,
    SVG_LINE_FILL,
    FONT_MATCH_SLANT_FIRST,
    TEXT_DISTANCE_FIELDS_OFF,
    TRANSFORMED_MASK_REUSE,
    EXACT_BLEND_DIVISION,
    LOWP_PAINT_ALPHA,
    LOWP_EXACT_COVERAGE,
    PATCH_MARKER,
  ],
};

/**
 * The patches a platform's tree goes without, each with why. Every patch
 * `PATCHES` names for one platform is, on each other, either in its list
 * there or here -- `scripts/check-skia-patches.mjs` refuses one that is
 * neither, so a platform cannot go without a patch by omission.
 */
export const WITHHELD = {
  darwin: {
    [BROTLI_SOURCES]:
      "skia-bindings drops brotli on macOS, so there is nothing for it to link",
    [DWRITE_OBLIQUE]:
      "it changes Skia's DirectWrite port, which only Windows builds",
    [FREETYPE_ADVANCE]:
      "it changes Skia's FreeType port, which macOS builds without -- skia-safe takes `embed-freetype` on Linux only",
  },
  linux: {
    [METAL_UNTIMED_COMPILE]:
      "it changes Metal's code, which Linux does not build",
    [DWRITE_OBLIQUE]:
      "it changes Skia's DirectWrite port, which only Windows builds",
  },
  win32: {
    [METAL_UNTIMED_COMPILE]:
      "it changes Metal's code, which Windows does not build",
    [BROTLI_SOURCES]:
      "it fixes a `freetype` build's link, and Windows builds without `freetype` (`CARGO_FEATURES` in lib/prebuild.mjs)",
    [ASYNC_TRANSFER_REUSE]:
      "skbug.com/40042671: the switch crashed Chromium's GPU process on Windows, with no cause found",
    [FREETYPE_ADVANCE]:
      "it changes Skia's FreeType port, which Windows builds without (`CARGO_FEATURES` in lib/prebuild.mjs)",
  },
};

/** This platform's patches, or none. */
function patches(platform = process.platform) {
  return PATCHES[platform] ?? [];
}

/**
 * Whether the Cargo.toml at `root` builds skia-bindings from a local rust-skia
 * checkout, as `just use-local-skia` makes it. skia-bindings then builds that
 * checkout's own Skia, syncing its DEPS itself, and this script neither
 * prepares nor names a tree: one named here would be built offline in its
 * place.
 */
export function usesLocalSkia(root = ROOT) {
  return /^skia-bindings\s*=\s*\{\s*path\s*=/m.test(
    readFileSync(join(root, "Cargo.toml"), "utf8"),
  );
}

/**
 * Whether `env` names a tree for the checkout at `root`: on a platform with
 * patches, unless it builds a local rust-skia checkout. Where it does, a
 * build with no `SKIA_SOURCE_DIR` builds unpatched Skia, which
 * scripts/skia-cache.mjs refuses unless asked for it.
 */
export function namesSourceTree(root = ROOT, platform = process.platform) {
  return patches(platform).length > 0 && !usesLocalSkia(root);
}
const RUST_SKIA = "https://github.com/rust-skia/rust-skia.git";
/// Written last, so a directory without it is a preparation that did not finish.
const READY = ".ready";
/// Hex digits of the patch's sha256 in the directory name: enough that two
/// patches never share one, short enough to read.
const PATCH_DIGEST_LENGTH = 12;
/// Seconds to wait before each further try of a fetch that failed, so four
/// tries in all. googlesource answers a burst of git-sync-deps' parallel
/// fetches with 503s and 429s that clear within a minute, and a cold cache
/// that failed on one of them failed the build: the waits double from 15 s,
/// so the last try is nearly two minutes after the first.
const RETRY_DELAYS_SECONDS = [15, 30, 60];

/**
 * The skia-bindings version `root`'s Cargo.lock resolves, which is the tag to
 * fetch.
 */
function lockedVersion(root = ROOT) {
  const lock = readFileSync(join(root, "Cargo.lock"), "utf8");
  const found = lock.match(
    /\[\[package\]\]\nname = "skia-bindings"\nversion = "([^"]+)"/,
  );
  if (!found) throw new Error("skia-source: Cargo.lock has no skia-bindings");
  return found[1];
}

/**
 * Where prepared trees are kept, and the compiled Skia built from them under
 * `binaries` -- see scripts/skia-cache.mjs.
 */
export function cacheRoot() {
  const base = process.env.XDG_CACHE_HOME || join(homedir(), ".cache");
  return join(base, "meo-skia-canvas");
}

/**
 * The cache directory name of the tree a checkout at `root` builds from: the
 * locked version and a digest of this platform's patches, in order. A list of
 * one hashes as that patch alone did.
 */
export function treeName(root = ROOT, list = patches()) {
  const hash = createHash("sha256");
  for (const patch of list) hash.update(readFileSync(join(root, patch)));
  const digest = hash.digest("hex").slice(0, PATCH_DIGEST_LENGTH);
  return `skia-${lockedVersion(root)}-${digest}`;
}

function treeDir() {
  return join(cacheRoot(), treeName());
}

/**
 * The names of git's variables that tie a command to one repository, as git
 * itself lists them: the ones it clears before running a command inside a
 * submodule.
 */
let repositoryVariables;

/**
 * This process's environment, less the variables that tie git to one
 * repository, for every tool `prepare` runs, with git told to write files as
 * they are committed.
 *
 * `git commit` runs its hooks with `GIT_DIR` and `GIT_INDEX_FILE` naming the
 * repository being committed to, and git honours them over the directory a
 * command runs in. `just precommit` runs `prepare`, so without this the clone
 * and git-sync-deps' `remote set-url`, `fetch` and `checkout` act on that
 * repository rather than on the checkout -- its index, origin and object
 * store, not Skia's.
 *
 * Skia has no `.gitattributes`, so a clone or a restore following the
 * machine's config wrote its sources with CRLF where that config sets
 * `core.autocrlf` true, as Git for Windows installs it, or marks every file
 * text with `core.eol` crlf: not the bytes a tree's name stands for, and this
 * repository's LF patches then fail at `-F0`. `GIT_CONFIG_COUNT` is one of the
 * variables removed above, so these are the only entries it counts.
 */
function childEnv() {
  repositoryVariables ??= new Set(
    execFileSync("git", ["rev-parse", "--local-env-vars"], {
      encoding: "utf8",
    }).split("\n"),
  );
  return {
    ...Object.fromEntries(
      Object.entries(process.env).filter(
        ([name]) => !repositoryVariables.has(name),
      ),
    ),
    GIT_CONFIG_COUNT: "2",
    GIT_CONFIG_KEY_0: "core.autocrlf",
    GIT_CONFIG_VALUE_0: "false",
    GIT_CONFIG_KEY_1: "core.eol",
    GIT_CONFIG_VALUE_1: "lf",
  };
}

/** The first line of a `.cargo/config.toml` `cargo-patch` wrote, by which it
 * tells its own file from one a developer wrote and will not replace. */
export const CARGO_PATCH_HEADER =
  "# Written by `node scripts/skia-source.mjs cargo-patch`; removed by `cargo-patch --remove`.";

/** The crates `cargo-patch` builds from the prepared checkout, by their
 * directories in it. */
const PATCHED_CRATES = ["skia-bindings", "skia-safe", "skia-svg-macros"];

/**
 * The `.cargo/config.toml` that builds skia-bindings, skia-safe and their
 * macros from the rust-skia checkout in `tree`, which carries
 * `GRAPHITE_BINDINGS`. Cargo then drops those packages' registry source from
 * `Cargo.lock`, which is why it is opt-in rather than written by `prepare`: a
 * build that takes it restores the lock afterwards, and the hook refuses a
 * commit of one changed this way.
 */
export function cargoPatchConfig(tree) {
  const crates = PATCHED_CRATES.map(
    (crate) =>
      `${crate} = { path = ${JSON.stringify(join(tree, "rust-skia", crate))} }`,
  );
  return [CARGO_PATCH_HEADER, "[patch.crates-io]", ...crates, ""].join("\n");
}

/**
 * Writes `cargoPatchConfig` for this worktree's prepared tree to
 * `<root>/.cargo/config.toml`, or with `remove` deletes one this wrote. Refuses
 * a file it did not write, rather than replacing a developer's own.
 */
export function cargoPatch(root, tree, { remove = false } = {}) {
  const path = join(root, ".cargo", "config.toml");
  const ours =
    existsSync(path) &&
    readFileSync(path, "utf8").split("\n")[0] === CARGO_PATCH_HEADER;
  if (existsSync(path) && !ours)
    throw new Error(
      `${path} was not written by cargo-patch; move it aside first`,
    );
  if (remove) {
    if (ours) rmSync(path);
    return null;
  }
  if (!existsSync(join(tree, READY)))
    throw new Error(
      `${tree} is not prepared; run \`node scripts/skia-source.mjs prepare\` first`,
    );
  mkdirSync(join(root, ".cargo"), { recursive: true });
  writeFileSync(path, cargoPatchConfig(tree));
  return path;
}

/** The directory `SKIA_SOURCE_DIR` names: Skia itself, inside the checkout. */
function sourceDir(tree = treeDir()) {
  return join(tree, "rust-skia", "skia-bindings", "skia");
}

// The tools' output goes to stderr, so the directory is the only thing
// `prepare` prints to stdout and a caller can take it whole. `env` is added to
// `childEnv()`, never a replacement for it.
export function run(command, args, { env = {}, ...options } = {}) {
  execFileSync(command, args, {
    stdio: ["ignore", process.stderr.fd, "inherit"],
    ...options,
    env: { ...childEnv(), ...env },
  });
}

/**
 * The name of the tree the worktree at `worktree` builds from, as its own
 * `scripts/skia-source.mjs` names it -- its own patch list, which can differ
 * from this one's: a branch that adds a patch names its tree by a digest
 * `PATCHES` here does not know, and one without it names its tree by fewer
 * patches than this list holds. A worktree with no script of its own predates
 * the cache, and is named with this script's list where its lock and patches
 * allow, and otherwise names none: `null`. Throws where its own script names
 * no tree.
 */
function worktreeTreeName(worktree) {
  const script = join(worktree, "scripts", "skia-source.mjs");
  if (!existsSync(script)) {
    try {
      return treeName(worktree);
    } catch {
      return null;
    }
  }
  const dir = execFileSync(process.execPath, [script, "dir"], {
    cwd: worktree,
    env: childEnv(),
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();
  const name = relative(cacheRoot(), dir).split(sep)[0];
  if (!name.startsWith("skia-")) throw new Error(`${dir} is outside the cache`);
  return name;
}

/**
 * The names of the trees a skia-bindings out-dir in `worktree`'s target was
 * built from: each stdout that skia-bindings' build script wrote, see
 * scripts/cargo-build-dir.mjs, names its source in a `cargo:rerun-if-changed=`
 * line, and one under `cache` names the tree. A target holds an out-dir per
 * configuration it has built, and keeps linking against it, so its tree is in
 * use whatever the worktree's patch list names now. Both `debug`-style profiles
 * and a target triple's are read.
 *
 * Only that line is read. The `ninja: Entering directory` line names the
 * out-dir under the worktree's own target, not the source, and the compile
 * lines name the source by a path relative to the out-dir -- both are ninja's
 * progress output rather than anything cargo acts on, and a worktree whose
 * directory reads like a tree name would match a looser search. The
 * `rerun-if-changed` line is absolute, is what cargo watches, and is
 * anchored here on the cache's own path.
 */
export function treesBuiltFrom(worktree, cache = cacheRoot()) {
  const prefixes = [cache, existsSync(cache) ? realpathSync(cache) : cache].map(
    (dir) => `cargo:rerun-if-changed=${dir}${sep}`,
  );
  const names = new Set();
  const target = join(worktree, "target");
  for (const build of profileBuildDirs(target)) {
    for (const run of buildScriptRuns(build, "skia-bindings")) {
      let text;
      try {
        text = readFileSync(buildScriptLogs(run).output, "utf8");
      } catch (error) {
        if (error.code === "ENOENT") continue;
        throw error;
      }
      for (const line of text.split("\n")) {
        const prefix = prefixes.find((p) => line.startsWith(p));
        if (!prefix) continue;
        const name = line.slice(prefix.length).split(sep)[0];
        if (name.startsWith("skia-")) names.add(name);
      }
    }
  }
  return names;
}

/**
 * The names of the trees some worktree of the repository at `root` builds
 * from, or null when they cannot all be known: the worktrees cannot be
 * listed, one whose own script is there names no tree, or one's target cannot
 * be read. A worktree builds from the tree its own script names and from each
 * tree an out-dir in its target was built from -- see `treesBuiltFrom`. A
 * worktree whose checkout is gone, or that predates the script and has no lock
 * or patch to name a tree by, names none of the first kind.
 */
export function treesInUse(root = ROOT) {
  let listing;
  try {
    listing = execFileSync("git", ["worktree", "list", "--porcelain"], {
      cwd: root,
      env: childEnv(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    return null;
  }
  const prefix = "worktree ";
  const names = new Set();
  for (const line of listing.split("\n")) {
    if (!line.startsWith(prefix)) continue;
    const worktree = line.slice(prefix.length);
    if (!existsSync(worktree)) continue;
    try {
      const name = worktreeTreeName(worktree);
      if (name) names.add(name);
      for (const built of treesBuiltFrom(worktree)) names.add(built);
    } catch {
      // Unknown is not unused: a tree removed from under a worktree costs it
      // a fresh one, and one left behind costs only disk.
      return null;
    }
  }
  return names;
}

/** The directory under `cache` holding `hold`'s markers, one per tree. */
function holdsDir(cache) {
  return join(cache, "holds");
}

/**
 * When the process `pid` started, as `ps` prints it, or "" where that cannot
 * be asked. A marker records it so that a holder's PID used again by another
 * process is not taken for the holder.
 */
function startTime(pid) {
  if (process.platform === "win32") return "";
  try {
    return execFileSync("ps", ["-o", "lstart=", "-p", String(pid)], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "";
  }
}

/** Whether the process `pid`, started at `started` where known, still runs. */
function running(pid, started) {
  try {
    process.kill(pid, 0);
  } catch (error) {
    // EPERM is a process that exists and is not ours to signal.
    if (error.code !== "EPERM") return false;
  }
  return started === "" || startTime(pid) === started;
}

/**
 * Holds the tree `name` under `cache` for as long as the process `pid` runs:
 * `env` holds its tree for the `just` it is the `dotenv-command` of, which is
 * its parent, so every recipe of that `just` -- and a shell that exported its
 * output -- builds from a tree `prepare` in another worktree will not remove.
 * Nothing releases a hold; it ends when its holder does.
 */
export function hold(name, pid = process.ppid, cache = cacheRoot()) {
  const dir = join(holdsDir(cache), name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, String(pid)), `${startTime(pid)}\n`);
}

/**
 * The names of the trees under `cache` some running process holds -- see
 * `hold` -- removing the markers of holders that have ended.
 */
export function treesHeld(cache = cacheRoot()) {
  const names = new Set();
  let trees;
  try {
    trees = readdirSync(holdsDir(cache));
  } catch (error) {
    if (error.code === "ENOENT") return names;
    throw error;
  }
  for (const name of trees) {
    const dir = join(holdsDir(cache), name);
    for (const pid of readdirSync(dir)) {
      const marker = join(dir, pid);
      if (running(Number(pid), readFileSync(marker, "utf8").trim()))
        names.add(name);
      else rmSync(marker, { force: true });
    }
    if (!names.has(name)) rmSync(dir, { recursive: true, force: true });
  }
  return names;
}

/**
 * The names of the trees under `cache` that `prepare` keeps: those some
 * worktree of the repository at `root` builds from (`treesInUse`) and those a
 * running build holds (`treesHeld`). Null when the first cannot be known.
 */
export function treesToKeep(root = ROOT, cache = cacheRoot()) {
  const inUse = treesInUse(root);
  if (inUse === null) return null;
  for (const name of treesHeld(cache)) inUse.add(name);
  return inUse;
}

/**
 * Removes every tree under `cache` that `inUse` does not name, the partial
 * preparation of each, and the Skia compiled from each under `binaries` -- see
 * scripts/skia-cache.mjs. Nothing is removed when `inUse` is null: a tree left
 * behind costs disk, one removed from under a worktree costs it a fresh tree.
 */
export function removeSuperseded(cache, inUse) {
  if (inUse === null) {
    console.error(
      "skia-source: this repository's worktrees could not be listed, so no " +
        `superseded tree under ${cache} was removed`,
    );
    return;
  }
  for (const name of readdirSync(cache)) {
    if (name.startsWith("skia-") && !inUse.has(name.replace(/\.partial$/, "")))
      rmSync(join(cache, name), { recursive: true, force: true });
  }
  // The compiled Skia of a removed tree goes with it: nothing builds from
  // that tree again.
  const binaries = join(cache, "binaries");
  if (existsSync(binaries))
    for (const name of readdirSync(binaries))
      if (!inUse.has(name))
        rmSync(join(binaries, name), { recursive: true, force: true });
}

/**
 * The paths, relative to the rust-skia checkout, of the files `patchText`
 * creates rather than changes: those its hunks take from `/dev/null`.
 */
export function patchCreates(patchText) {
  return [...patchText.matchAll(/^--- \/dev\/null\n\+\+\+ b\/(\S+)/gm)].map(
    ([, path]) => path,
  );
}

/**
 * Reads the dependencies Skia's DEPS pins, as git-sync-deps reads them: the
 * file is Python, executed with `Var` defined, and each entry that names a
 * repository `@` a commit is a checkout under Skia's root.
 */
const PINS = `
import json, sys
scope = {}
exec("def Var(x): return vars[x]\\n" + open(sys.argv[1]).read(), scope)
print(json.dumps({d: v.split("@", 1)[1] for d, v in scope["deps"].items()
                  if isinstance(v, str) and "@" in v}))
`;

/**
 * The Python to run DEPS and Skia's scripts with: `python3` where there is
 * one, and otherwise `python`, the name Windows installs it under -- where
 * `python3` is often the Store's placeholder, which exits without running
 * anything.
 */
let python;
function pythonCommand() {
  python ??=
    ["python3", "python"].find((name) => {
      try {
        execFileSync(name, ["--version"], { stdio: "ignore" });
        return true;
      } catch {
        return false;
      }
    }) ?? "python3";
  return python;
}

/** `git` in `cwd`, its output trimmed, or `null` when it fails. */
function git(cwd, ...args) {
  try {
    return execFileSync("git", args, {
      cwd,
      env: childEnv(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

/**
 * The file `bin/fetch-gn` writes gn to on `platform`: `gn.exe` on Windows,
 * which skia-bindings then runs as `bin/gn`, and `gn` elsewhere.
 */
export function gnName(platform = process.platform) {
  return platform === "win32" ? "gn.exe" : "gn";
}

/**
 * Why the tree at `tree` is not a whole preparation of rust-skia `version`,
 * or `null` when it is: its checkout at the version's tag, Skia's gn in
 * place, every dependency DEPS pins checked out at its pin and unedited, and
 * none of the files `created` names, which a patch would have to make.
 *
 * What it does not check is the tracked sources, which a patched tree has
 * changed on purpose: [`restoredRefusal`] does, once they are restored.
 */
export function baseRefusal(
  tree,
  version,
  created = [],
  platform = process.platform,
) {
  const ready = join(tree, READY);
  if (!existsSync(ready) || readFileSync(ready, "utf8").trim() !== version)
    return `it is not a finished tree for ${version}`;
  const checkout = join(tree, "rust-skia");
  const head = git(checkout, "rev-parse", "HEAD");
  const tag = git(checkout, "rev-parse", `refs/tags/${version}^{commit}`);
  if (!head || head !== tag) return `its checkout is not at the ${version} tag`;
  const skia = sourceDir(tree);
  if (!existsSync(join(skia, "bin", gnName(platform)))) return "it has no gn";
  let pins;
  try {
    pins = JSON.parse(
      execFileSync(pythonCommand(), ["-c", PINS, join(skia, "DEPS")], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      }),
    );
  } catch (error) {
    // The last line of what Python said, which names the error, or why it
    // did not start.
    const said = `${error.stderr ?? ""}`.trim().split("\n").pop();
    return `its DEPS could not be read: ${said || error.message}`;
  }
  for (const [directory, pin] of Object.entries(pins)) {
    const dependency = join(skia, directory);
    if (git(dependency, "rev-parse", "HEAD") !== pin)
      return `${directory} is not at the commit DEPS pins`;
    if (git(dependency, "status", "--porcelain", "--untracked-files=no") !== "")
      return `${directory} has been edited`;
  }
  const present = created.find((path) => existsSync(join(checkout, path)));
  if (present) return `${present} exists, which a patch creates`;
  return null;
}

/**
 * Why the copy at `tree`, its sources restored, is not the checkout a fresh
 * clone would make, or `null` when it is: no tracked file of rust-skia or of
 * its Skia submodule differs from its commit, and Skia is at the commit
 * rust-skia records for it.
 */
export function restoredRefusal(tree) {
  const checkout = join(tree, "rust-skia");
  const skia = sourceDir(tree);
  // First, because rust-skia's own status reports a moved submodule as a
  // change, which would name the symptom rather than the cause.
  const recorded = git(checkout, "rev-parse", "HEAD:skia-bindings/skia");
  if (!recorded || git(skia, "rev-parse", "HEAD") !== recorded)
    return "Skia is not at the commit rust-skia records";
  for (const repository of [checkout, skia])
    if (git(repository, "status", "--porcelain", "--untracked-files=no") !== "")
      return `${repository} still differs from its commit`;
  return null;
}

/**
 * Copies `from` to `to` sharing its blocks where the filesystem can -- a
 * clone on APFS, a reflink where Linux has them -- and in full where it
 * cannot.
 *
 * `cp` where there is one, because it clones a tree in one call where Node
 * copies it a file at a time; Node's copy on Windows, which has no `cp`,
 * asking for a clone and taking a full copy where none can be made. Links
 * are copied as links either way: rust-skia's checkout holds one that points
 * at a directory above it.
 */
export function copyTree(from, to, platform = process.platform) {
  if (platform === "win32") {
    cpSync(from, to, {
      recursive: true,
      verbatimSymlinks: true,
      mode: constants.COPYFILE_FICLONE,
    });
    return;
  }
  const args =
    platform === "darwin"
      ? ["-c", "-R", from, to]
      : ["-a", "--reflink=auto", from, to];
  execFileSync("cp", args, { stdio: ["ignore", process.stderr.fd, "inherit"] });
}

/**
 * Makes `partial` from another tree for `version` already under `cache`
 * rather than from the network, and returns that tree's name, or `null` when
 * none would do -- each refused with a line on stderr saying why.
 *
 * Every tree for a version has the same checkout, dependencies and gn; they
 * differ only in the patches applied to tracked files. So a base is copied,
 * its tracked sources restored by git to their commits, and the result
 * checked again before the caller patches it. The newest base is tried
 * first.
 */
export function derive(cache, partial, version, created = []) {
  const name = basename(partial).replace(/\.partial$/, "");
  let names;
  try {
    names = readdirSync(cache);
  } catch {
    return null;
  }
  const bases = names
    .filter(
      (base) =>
        base.startsWith(`skia-${version}-`) &&
        base !== name &&
        !base.endsWith(".partial"),
    )
    .map((base) => [base, statSync(join(cache, base)).mtimeMs])
    .sort((a, b) => b[1] - a[1])
    .map(([base]) => base);
  for (const base of bases) {
    const from = join(cache, base);
    const before = baseRefusal(from, version, created);
    if (before) {
      console.error(`skia-source: not deriving from ${base}: ${before}`);
      continue;
    }
    try {
      copyTree(from, partial);
      rmSync(join(partial, READY), { force: true });
      run("git", ["checkout", "--", "."], { cwd: sourceDir(partial) });
      run("git", ["checkout", "--", "."], { cwd: join(partial, "rust-skia") });
    } catch (error) {
      rmSync(partial, { recursive: true, force: true });
      console.error(`skia-source: copying ${base} failed: ${error.message}`);
      continue;
    }
    const after = restoredRefusal(partial);
    if (after) {
      rmSync(partial, { recursive: true, force: true });
      console.error(`skia-source: not deriving from ${base}: ${after}`);
      continue;
    }
    return base;
  }
  return null;
}

/** Blocks the thread for `seconds`: the script is synchronous throughout. */
function sleep(seconds) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, seconds * 1000);
}

/**
 * Runs `step`, and again after each of `delays` seconds while it throws,
 * saying on stderr which try of `what` failed and why. The last failure is
 * thrown, so one that persists still stops the caller with its own error.
 */
export function retry(
  what,
  step,
  { delays = RETRY_DELAYS_SECONDS, wait = sleep } = {},
) {
  const tries = delays.length + 1;
  for (let attempt = 1; ; attempt++) {
    try {
      return step();
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      if (attempt === tries) {
        console.error(
          `skia-source: ${what} failed on try ${attempt} of ${tries}, the last: ${reason}`,
        );
        throw error;
      }
      const delay = delays[attempt - 1];
      console.error(
        `skia-source: ${what} failed on try ${attempt} of ${tries}: ${reason}; trying again in ${delay} s`,
      );
      wait(delay);
    }
  }
}

/**
 * Makes the tree the checkout at `root` names under `cache`, once, and
 * returns its Skia source directory, or null against a local rust-skia
 * checkout. A tree marked `.ready` is returned as it is. Otherwise, under the
 * tree's lock, the tree is derived from another of its version -- or fetched,
 * by `fetch`, where none will do -- into `<name>.partial`, patched with `list`,
 * marked ready and renamed into place.
 *
 * A second prepare of the same tree waits for the lock and then finds the
 * tree ready. A tree directory without `.ready` holds nothing anyone can
 * build from -- every prepare that ends early leaves one, and a rename onto
 * it fails -- so it is removed and made again. `wait` bounds the wait for the
 * lock, in seconds. `at` names each step to a test that has to stop a prepare
 * between two of them; a prepare passes nothing.
 */
export function prepare({
  root = ROOT,
  cache = cacheRoot(),
  list = patches(),
  fetch = fetchTree,
  wait = LOCK_WAIT_SECONDS,
  at = () => {},
} = {}) {
  if (usesLocalSkia(root)) return null;
  const version = lockedVersion(root);
  const name = treeName(root, list);
  const tree = join(cache, name);
  if (existsSync(join(tree, READY))) return sourceDir(tree);

  const unlock = lockTree(cache, name, { wait });
  try {
    // Another prepare may have made it while this one waited.
    if (existsSync(join(tree, READY))) return sourceDir(tree);
    if (existsSync(tree)) {
      console.error(
        `skia-source: removing ${name}, which an earlier prepare left unfinished`,
      );
      rmSync(tree, { recursive: true, force: true });
    }
    const partial = `${tree}.partial`;
    rmSync(partial, { recursive: true, force: true });
    at("removed");
    const checkout = join(partial, "rust-skia");
    const created = list.flatMap((patch) =>
      patchCreates(readFileSync(join(root, patch), "utf8")),
    );
    const base = derive(cache, partial, version, created);
    if (base) console.error(`skia-source: derived ${name} from ${base}`);
    else fetch(partial, checkout, version, name);
    at("derived");

    applyPatches(checkout, list, root);
    at("patched");
    writeFileSync(join(partial, READY), `${version}\n`);
    renameSync(partial, tree);
  } finally {
    unlock();
  }

  // The worktrees name the trees of their own platform's patches, this
  // checkout among them, so a tree made from another `list` is none of
  // theirs: it is kept by name, or it would be removed as soon as it was
  // made, and the path returned would name nothing.
  const keep = treesToKeep(root, cache);
  keep?.add(name);
  removeSuperseded(cache, keep);
  return sourceDir(tree);
}

/**
 * Applies `list` -- this platform's patches from `PATCHES`, by default -- in
 * its order to the rust-skia checkout at `checkout`, with paths read from
 * `root`. `prepare` patches a tree with it, and `apply` the rust-skia checkout
 * the Linux release build compiles from, so there is one list and one way of
 * applying it. `-F0`: a hunk that no longer matches fails rather than being
 * placed by guesswork, and the error names the patch it is in. `--forward`: a
 * patch already applied fails too, where without it macOS's `patch` takes one
 * as reversed, with no terminal to ask, and undoes it with a zero exit.
 */
export function applyPatches(checkout, list = patches(), root = ROOT) {
  for (const patch of list) {
    try {
      run("patch", [
        "-p1",
        "-F0",
        "--forward",
        "-d",
        checkout,
        "-i",
        join(root, patch),
      ]);
    } catch (error) {
      throw new Error(
        `skia-source: ${patch} does not apply to ${checkout}: ${error.message}`,
        { cause: error },
      );
    }
  }
}

/**
 * The key a CI cache of a platform's tree is saved and restored under: the
 * platform and the tree's name, which carries the locked version and the
 * digest of that platform's patches, so one runner can name every platform's
 * key. The keys follow `PATCHES` because they are made from it, rather than
 * from a list of patch files a workflow would have to keep in step.
 */
export function cacheKey(root = ROOT, platform = process.platform) {
  return `skia-source-${platform}-${treeName(root, patches(platform))}`;
}

/**
 * How long `prepare` waits for another prepare of the same tree before it
 * gives up: a fetch is about 1 GB, so a holder can be at work for minutes.
 */
const LOCK_WAIT_SECONDS = 30 * 60;
/** How often a waiting `prepare` looks again. */
const LOCK_POLL_SECONDS = 1;
/**
 * How long a lock file with no owner in it can stand before it counts as its
 * holder's, who ended between creating and writing it: the write follows the
 * create at once, so a few seconds is far past any live holder's.
 */
const LOCK_UNWRITTEN_SECONDS = 10;

function locksDir(cache) {
  return join(cache, "locks");
}

/** What a lock holds: its holder's PID and start time, as `running` takes them. */
function lockOwner(pid = process.pid) {
  return `${pid}\n${startTime(pid)}\n`;
}

/** When the file at `path` was last written, in ms, or now where it is gone. */
function mtime(path) {
  try {
    return statSync(path).mtimeMs;
  } catch (error) {
    if (error.code === "ENOENT") return Date.now();
    throw error;
  }
}

/** The lock file at `path`, read, or null where there is none to read. */
function readLock(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

/**
 * Takes the lock on the tree `name` under `cache` -- `<cache>/locks/<name>`,
 * created only where absent, holding this process's PID and start time -- and
 * returns the function that releases it.
 *
 * Two prepares of one tree work in one `<name>.partial`, so without it they
 * remove, copy, patch and rename one another's work (#566). A holder that has
 * ended, or whose PID now runs a process started at another time, holds
 * nothing: its lock is moved aside and taken. Moving it aside rather than
 * removing it means two waiters taking over at once cannot remove each
 * other's new lock: the one that finds a live lock where it moved the stale
 * one puts it back. A live holder is waited for, for `wait` seconds at most.
 */
export function lockTree(
  cache,
  name,
  { wait = LOCK_WAIT_SECONDS, poll = LOCK_POLL_SECONDS } = {},
) {
  mkdirSync(locksDir(cache), { recursive: true });
  const path = join(locksDir(cache), name);
  const mine = lockOwner();
  const end = Date.now() + wait * 1000;
  let said = false;
  for (;;) {
    try {
      writeFileSync(path, mine, { flag: "wx" });
      return () => {
        if (readLock(path) === mine) rmSync(path, { force: true });
      };
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
    }
    const held = readLock(path);
    if (held === null) continue;
    const [pid, started = ""] = held.split("\n");
    const ended = /^\d+$/.test(pid)
      ? !running(Number(pid), started)
      : Date.now() - mtime(path) > LOCK_UNWRITTEN_SECONDS * 1000;
    if (ended) {
      const aside = `${path}.${process.pid}`;
      try {
        renameSync(path, aside);
      } catch (error) {
        if (error.code === "ENOENT") continue;
        throw error;
      }
      if (readLock(aside) === held) {
        console.error(
          `skia-source: took over the lock on ${name} from ${pid}, which has ended`,
        );
        rmSync(aside, { force: true });
      } else {
        // A live holder's lock, which took the stale one's place between the
        // read and the move: put back where nothing has taken its place since.
        try {
          linkSync(aside, path);
        } catch (error) {
          if (error.code !== "EEXIST") throw error;
        }
        rmSync(aside, { force: true });
      }
      continue;
    }
    if (Date.now() >= end)
      throw new Error(
        `skia-source: ${name} is still being prepared by ${pid} (started ${started || "at an unknown time"}) after ${wait} s; if that process is not preparing it, remove ${path}`,
      );
    if (!said) {
      console.error(
        `skia-source: waiting for ${pid} (started ${started || "at an unknown time"}), which is preparing ${name}`,
      );
      said = true;
    }
    sleep(poll);
  }
}

/**
 * Fetches rust-skia at the locked tag into `checkout`, with Skia's DEPS and
 * gn: about 1 GB, said so on stderr before it starts, because nothing else
 * would show a build that is waiting on it.
 */
function fetchTree(
  partial,
  checkout,
  version = lockedVersion(),
  name = treeName(),
) {
  console.error(
    `skia-source: no tree for ${version} to derive ${name} ` +
      "from; fetching rust-skia, Skia's dependencies and gn, about 1 GB",
  );
  // A clone that failed part way leaves a directory git will not clone into,
  // so each try starts from none.
  retry("cloning rust-skia", () => {
    rmSync(checkout, { recursive: true, force: true });
    run("git", [
      "clone",
      "--quiet",
      "--depth",
      "1",
      "--branch",
      version,
      "--recurse-submodules",
      "--shallow-submodules",
      "-j8",
      RUST_SKIA,
      checkout,
    ]);
  });
  // The same invocation skia-bindings makes for a full build
  // (build_support/skia/config.rs): DEPS read from skia-bindings, emsdk skipped.
  // It fetches into what an earlier try left and checks each dependency out
  // at its pinned revision, so a try resumes rather than starting over.
  retry("syncing Skia's DEPS", () =>
    run(pythonCommand(), ["skia/tools/git-sync-deps"], {
      cwd: join(checkout, "skia-bindings"),
      env: {
        GIT_SYNC_DEPS_PATH: "skia/DEPS",
        GIT_SYNC_DEPS_SKIP_EMSDK: "1",
      },
    }),
  );
  retry("fetching gn", () =>
    run(pythonCommand(), ["bin/fetch-gn"], { cwd: sourceDir(partial) }),
  );
}

/**
 * `name=value` as one line of the dotenv format just reads `env`'s output in,
 * and that `eval "export $(...)"` reads in a shell: bare where the value holds
 * nothing either treats specially, and otherwise in single quotes, each quote
 * inside written as `'\''`. Unquoted, a backslash starts an escape -- just
 * refused `C:\Users\...` at the `\U`, failing every recipe on Windows -- and
 * a space ends the value.
 */
export function dotenvLine(name, value) {
  if (!/[\s\\'"$#`]/.test(value)) return `${name}=${value}`;
  return `${name}='${value.replaceAll("'", "'\\''")}'`;
}

// Run as a command, not when a test imports `retry`. Both sides resolved, as
// `import.meta.url` is: under /tmp on macOS the two spellings differ.
if (
  process.argv[1] &&
  realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))
) {
  // skia-bindings takes a `SKIA_SOURCE_DIR` that is set at all as an offline
  // build from that directory, an empty one included, and then fails reporting
  // gn missing. One set but empty is always a mistake upstream of here, so it is
  // named rather than passed on.
  if (process.env.SKIA_SOURCE_DIR === "") {
    console.error(
      "skia-source: SKIA_SOURCE_DIR is set but empty, which skia-bindings reads " +
        "as an offline build from no directory. Unset it, or set it to the " +
        "directory `node scripts/skia-source.mjs prepare` prints.",
    );
    process.exit(1);
  }

  const command = process.argv[2];
  if (command === "env") {
    if (namesSourceTree()) {
      console.log(dotenvLine("SKIA_SOURCE_DIR", sourceDir()));
      // A hold that cannot be written leaves the tree as unprotected as it was
      // before holds existed, which is no reason to stop the recipe.
      try {
        hold(treeName());
      } catch (error) {
        console.error(
          `skia-source: ${treeName()} is not held: ${error.message}`,
        );
      }
    }
  } else if (command === "dir") {
    console.log(sourceDir());
  } else if (command === "prepare") {
    const dir = prepare();
    if (dir !== null) console.log(dir);
  } else if (command === "apply") {
    const checkout = process.argv[3];
    if (!checkout) {
      console.error(
        "usage: node scripts/skia-source.mjs apply <rust-skia checkout>",
      );
      process.exit(2);
    }
    try {
      applyPatches(checkout);
    } catch (error) {
      console.error(error.message);
      process.exit(1);
    }
  } else if (command === "cache-key") {
    console.log(cacheKey());
  } else if (command === "cargo-patch") {
    const remove = process.argv[3] === "--remove";
    const path = cargoPatch(ROOT, treeDir(), { remove });
    if (path !== null) console.log(path);
  } else {
    console.error(
      "usage: node scripts/skia-source.mjs env|dir|prepare|apply <checkout>|cache-key|cargo-patch [--remove]",
    );
    process.exit(2);
  }
}
