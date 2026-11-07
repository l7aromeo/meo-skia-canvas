# Testing, probing and measuring

How to make a test, a probe or a benchmark say what it appears to say. The
rules are in [AGENTS.md's Evidence section](../../AGENTS.md#evidence); this is
how they are kept in practice, and the specific ways each has gone wrong here.

## Which binary a test runs

**A bare `node --test` does not test what you just built.** `loadSkiaNode`
tries the `MEO_SKIA_CANVAS_BINARY` override, then a platform package from
`node_modules`, then `lib/skia.node`, so an installed platform package outranks
a local build: the run loads the published binary and a change appears to have
done nothing. Nothing announces it.

`just test` sets `MEO_SKIA_CANVAS_BINARY` to the local build. `npm test` runs
`scripts/test.mjs`, which does the same when `lib/skia.node` exists, falls back
to the installed binary when it does not, and prints which one the run is
against. Read that line before reading the results.

`lib/skia.node` is whatever was built last. `just ci` ends with a debug build,
and a `just build-release` beforehand is replaced by it, so a benchmark run
after a green gate measures a debug binary unless it builds release itself.
After a release build, a later debug build copies its library into
`lib/skia.node` only when cargo recompiles; check a JavaScript run's skipped
count, since the GPU-discard tests skip on a release binary.

## Running the Rust suite outside `just`

`just` sets `SKIA_SOURCE_DIR` from `node scripts/skia-source.mjs env`, which
points `skia-bindings` at the patched Skia source this tree builds. A bare
`cargo test` has none, links a Skia without those patches, and tests that
depend on them fail on main's own behaviour -- a red that reads as a
regression in the change under test. Changing the variable's value reruns
`skia-bindings`' build and recompiles Skia. Outside `just`, through `eval`,
since `env` quotes a directory with a backslash or a space in it:

```bash
eval "export $(node scripts/skia-source.mjs env)"
cargo test --features metal,window,freetype,fetch    # the set `just test-rust` uses on macOS
```

`node scripts/skia-cache.mjs cargo test ...` in place of `cargo test` links
the Skia a build already compiled, where the local cache holds it for those
features, rather than compiling it -- see
[packaging.md](packaging.md#compiled-skia-is-cached-for-every-worktree).

A bare `cargo test` also has no GPU: the GPU legs need the platform's feature
(`metal`, or `vulkan` on Linux), and without it they skip or fall back. Add
`--no-fail-fast` when the library's own tests are red, or cargo stops at the
first failing test binary and the integration tests never run.

## The GPU engine

`set_gpu(true)` falls back to the CPU engine where no GPU starts, silently. A
test of GPU behaviour checks `engine_kind()` and skips, saying so, rather than
passing on the CPU. A test that pins one engine's output hides the other:
cover the other engine with a property that holds on both, never by asserting
a defect persists there.

## Controls and mutants

**A red run proves the assertion that fired, and only that one.** To show a
test covers a change, remove the change and watch each assertion fail in turn,
or name the ones that only guard. Check that the old tree gives the wrong
values the commit message quotes.

**A mutation must prove it applied.** Apply it with a check that the text it
replaces occurs exactly once, rather than with a bare `sed`, and confirm the
mutated code is reached by the state the test reads. A mutant that survives
is three claims: it applied, it was reached, and the tests cannot see it.
Sometimes the third means the code is wrong, not the tests.

**A strong control is a result that cannot occur unless something is wrong.**
Carry a case that must disagree on the same instrument. "At least N" passes on
phantom members: assert exact membership, no duplicates, and an input that
must be refused. Name the property no control guards.

**Predict each candidate before comparing.** If the old and new code would give
the same answer on an input, that input tests nothing: a red channel of 0
doubled, an alpha already 1, a grey on which both conversions agree.

**Take a reference from the arithmetic, not from a neighbour.** No expected
byte exactly on a half-level: 127.5 rounded up on macOS and down on Linux. A
second measurement counts only if it shares no code and no assumption with
the first.

**Pin an identity between two renders** -- equal buffers, a ratio, a slope --
rather than an absolute pixel count, and keep an absolute only where it is the
point, with its scene recorded.

## Search patterns

A grep, a regex or a glob is an instrument, and it fails in both directions:
too loose, it agrees with whatever you believed; too tight, it returns nothing,
which reads as "the tree is clean". Before acting on a result, run the pattern
against something it must match and something it must not, and say what it
would print under the opposite hypothesis.

Ways this has gone wrong here, four of them in one sweep:

- **The pattern matched the tool's own output.** `grep -n` prefixes each line
  with `file:line:`, and a pattern for `\.(rs|js):[0-9]+` matched the prefix,
  so every line "matched".
- **A substring where a word was meant.** `grep "eight"` matches `Height`; a
  sweep for a stale count came back full of eight-bit colour. Use `-w`.
- **A character class that stopped early.** `as [A-Z][A-Za-z]*` does not match
  `FillPath2D`, so a survey of declared verbs left out every name with a digit.
- **A count pattern that also matches the negative case.** `[1-9][0-9]* failed`
  matches `10 passed; 0 failed` on the `10`.
- **`\b` under `git grep -E`.** Git's ERE engine ignores it without error:
  `git grep -E '\brtl\b'` found 0 where `git grep -w rtl` found 13. Use `-w` or
  `-P`.
- **A pathspec `*` stops at `/`.** `-- 'crates/*/tests'` found 0 where
  `-- 'crates'` found 15. Widen the path, then narrow the results.
- **`\|` under `grep -E` is a literal pipe**, so an alternation written for
  basic `grep` matches nothing.
- **A version is a regex of wildcards.** `grep 0.5.0` matched `640×500`. Use
  `-F`, and match the form the file writes it in.
- **`−` (U+2212) and `-` are different characters**, and prose uses the first.

Prefer the tool that understands the structure: `git grep -w`, `-F`, the
language's own parser, or a script that walks items rather than lines.

## Benchmarks

`just bench` runs the npm and crate benchmarks over the same cases and writes a
dated report. For a comparison of your own:

- **Time release builds**, built from the commits being compared, and record
  each binary's checksum so the numbers can be tied to what produced them.
  Confirm the two binaries differ.
- **Interleave**: alternate the builds within one run, and flip the order each
  round, so that neither always runs first. Absolute timings drift by several
  times over a session; pairs taken together do not.
- **Repeat and report the spread**: at least five rounds, the median of the
  per-round ratios, and their minimum and maximum. Name the denominator.
- **Run one variant per process.** Variants in one process share polymorphic
  call sites and contaminate each other. Call `global.gc()` before a timed loop.
- **Reset the canvas between cases.** Filter, shadow and dash state leak from
  one case into the next; `ctx.reset()` clears them.
- **Measure memory with `process.resourceUsage().maxRSS`.** A `setInterval`
  sampler never fires during a synchronous loop.
- **Feature-detect with `in`.** A missing property reads as `undefined`, fast,
  and once scored a build "1823% faster".
- **Measure the batch, not the call.** A lone recorded setter and a mixed loop
  rank differently; time what a program does.

A ratio of minimums over regions of different length reads high; use the best
of several whole trials. A fixed cost dilutes a ratio bound, so say what an
allowance corresponds to.

## Comparing with Chrome

The baseline is a browser, measured, not read from the specification -- see
[AGENTS.md](../../AGENTS.md#the-rule-that-decides-everything).

- **Use `chrome-headless-shell`** (Chrome for Testing) with a temporary
  `--user-data-dir`, `--force-device-scale-factor=1` and a
  `--virtual-time-budget`, and write measurements into the DOM for
  `--dump-dom`, or take `--screenshot` for ink.
- **Serve the page over HTTP.** A `file://` page renders a snapshot without
  running its script, and an `@font-face` from `file://` fails silently, so
  Chrome measures the fallback font. Inline the font and require
  `document.fonts.check` to pass.
- **Read colours as text where there is a textual form.**
  `color-mix(in <space>, a, b)` set on an element's `color` and read back
  with `getComputedStyle` gives Chrome's interpolation exactly, in every
  space and hue method.
- **Name every default** the page relies on: an element's display, a border
  width with no style, a unitless line height and a radial gradient's extent
  all differ between engines and read as defects.
- **Don't diff rasterised pixels across engines.** Compare geometry, colours
  sampled at points derived from the geometry, ink spans against a stated
  threshold, and the contents of laid-out lines.
- **Chrome's GPU canvas** (`--enable-gpu-rasterization --use-angle=metal`):
  draw each case into a fresh canvas that is never read itself, and read it
  through `drawImage` onto a CPU canvas made with `willReadFrequently`. Reading
  a canvas with `getImageData` moves it to the CPU, and later draws then give
  CPU results. Prove the GPU path with a case known to differ first: a
  whole-canvas fill of a tie grey stores a byte low on the GPU and not on the
  CPU.
