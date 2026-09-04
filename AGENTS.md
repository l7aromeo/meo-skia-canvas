# Repository Guidelines for meo-skia-canvas

This file provides guidance to Claude Code and other AI agents working in this repository.

---

## No AI Residue In The Repository

**Nothing an agent produces for its own benefit belongs in this repository.** Plans, specs,
design notes, task lists, scratch analyses, session transcripts, progress trackers, review
write-ups, `.ai/`, `.cursor/`, `.aider*`, `.claude/`, `docs/superpowers/` — none of it. Work
in a scratch directory outside the repo and let the commit message carry whatever needs to
survive.

This is not a style preference. Such files are written for one moment and are wrong by the
next release, they describe intentions rather than the code that shipped, and nobody updates
them — so they become confidently misleading documentation that a future reader (human or
agent) has no way to distinguish from the maintained kind.

`docs/superpowers/` was inherited from phyron and carried three of them for months
(`811917c`, `b8eadb1`, `349a0e6`). Removed. If a tool recreates that path, delete it rather
than committing it.

What _does_ belong: the commit message, a CHANGELOG entry, a comment next to the code that
needs explaining, and this file. If a decision is worth keeping, it goes in one of those.

---

## Git Safety

**NEVER use `git reset --hard`, `git checkout --`, `git clean`, or any destructive git command without FIRST running `git stash`!**

Uncommitted working tree changes CANNOT be recovered after a hard reset. Always stash first:

```bash
git stash push -m "backup before reset"
git reset --hard <target>
# If something went wrong:
git stash pop
```

This applies to ALL destructive git operations. When in doubt, stash first.

---

## No Unwrap/Expect Without A Safety Comment

**Every `.unwrap()` and `.expect()` in library code MUST have a `// SAFETY:` comment explaining why it cannot fail, OR must be replaced with proper error handling.**

Library code means everything under `src/`. Tests are exempt: there a panic _is_
the failure report, `.unwrap()` is how a test fails, and the string in
`.expect("raw export")` already names what was expected. Requiring a comment
per call there would add hundreds of lines saying nothing -- what a test owes
the reader is a message that names the expectation, not a justification for
panicking.

A panic crossing the Neon FFI boundary aborts the operation. Neon catches it and raises `Error: internal error in Neon module`, which JavaScript can catch -- this crate does not set `panic = "abort"`, so the process usually survives. That is not a reason to relax the rule: the error is opaque, names no cause, and cannot be handled meaningfully by the caller. An allocation failure is the exception that genuinely aborts the process, and no `catch` can reach it.

Note also that this rule does not catch every panic. A panic inside a dependency -- Skia returning null into a `skia-safe` `unwrap`, for instance -- has no `.unwrap()` of ours to annotate, so validate inputs before handing them to a C++ layer that cannot report failure. Use:

- `cx.throw_error()` for Neon FFI boundaries.
- `?` for internal Rust error propagation.
- `unwrap_or()` / `unwrap_or_else()` / `unwrap_or_default()` when a fallback exists.
- `if let Some(...)` / `match` for optional values.

```rust
// BAD: panics turn into an opaque Neon internal error.
let result = some_operation().unwrap();

// GOOD: propagate error to JS.
let result = some_operation()
    .ok_or_else(|| "operation failed".to_string())?;

// GOOD: provably safe with documented reason.
// SAFETY: `collection` was set to `Some` on the previous line.
let coll = self.collection.as_ref().unwrap();
```

---

## Build, Test, and Development Commands

Use `just`:

```bash
just               # show available recipes
just ci            # the full gate -- see the pre-commit checklist for what is in it
just typecheck     # cargo check (Linux feature subset)
just lint-check    # cargo clippy (Linux feature subset)
just docs          # rustdoc and TypeDoc, both fatal on a warning
just fmt           # cargo fmt + prettier
just build         # debug build of the native module
just build-release # release build of the native module
just test          # node --test against the local build
```

**Note:** the `metal` feature only compiles on macOS, so the recipes use a Linux-safe feature subset (`vulkan,window,freetype`). Override locally if you're on macOS.

The recipes carry reasoning of their own, and it is not repeated here. `lint-check` explains why one
feature set does not lint the whole crate, and which of CI's three configurations cannot compile on
a developer machine at all -- which is the answer to "why can I not reproduce that job locally".
Read the recipe before working around it.

**Never use `--release` unless explicitly requested.** Debug builds are faster and sufficient for development.

---

## Project Context

A fork of [skia-canvas](https://github.com/samizdatco/skia-canvas), by way of
[phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas) -- a Node.js native module
(Neon/Rust) implementing the HTML Canvas API on top of Skia. Inherited extensions add F16/F32 pixel formats,
extended color spaces (P3, Rec.2020, HDR10, HLG, linear), OkLab gradient interpolation, CanvasKit
filter parity, variable font axis control, and a `ParagraphBuilder`/`Paragraph` API.

### What this fork changes

- **Binaries resolve from optional platform packages.** `lib/binary.js` probes
  `meo-skia-canvas-<triplet>` before falling back to the `install` script's download. Each platform
  package declares `os`/`cpu`/`libc`, so a package manager selects one without running any script.
  This exists because bun blocks postinstall scripts unless the package is listed in the consuming
  project's `trustedDependencies`, and that list is not inherited from dependencies -- so no
  package depending on this one could fix it for its own users.
- **Metal exports drain an autorelease pool.** `toBuffer`/`toFile` hand work to `rayon::spawn_fifo`,
  and a rayon worker has no autorelease pool, so Metal's Objective-C allocations accumulated for the
  life of the process.

### Which upstream, and what to do with it

`samizdatco/skia-canvas`. The `upstream` remote points there; its push URL is set to `DISABLED`,
because nothing here is ever pushed to it.

Samizdatco is behind this tree, measured 2026-09-04 with
`git rev-list --left-right --count upstream/main...main`:

| upstream                 | ahead of `main` | behind |
| ------------------------ | --------------: | -----: |
| `samizdatco/skia-canvas` |               0 |    794 |

Zero ahead means there is nothing to take today. The count itself is stale the moment it is
written -- run the command rather than quoting the table.

Phyron has no remote in this checkout, so its distance is not tracked and the command above cannot
report it. That is deliberate -- it is dormant outright, so the two changes once open there as
phyrondev#30 and phyrondev#29 have nowhere to land, and there is nothing to rebase onto or hold a
patch back for. Add the remote if that ever changes.

Samizdatco will ship again, and when it does, take it by cherry-pick rather than merge. They are on
`skia-safe` 0.88 against this tree's 0.99, so their `Cargo.toml` and anything shaped by the older
bindings is a downgrade. What is worth reading in one of their releases is a canvas-API or rendering
fix, which ports on its own.

Neither remote is a place to send work. This is not a staging area for a patch that belongs
elsewhere -- if a change is right for this tree, it lands here.

### Where output differs from upstream, on purpose

Measured against samizdatco `v3.0.8` (commit `042312a`, a direct ancestor of this history, so
`git diff 042312a..HEAD` is the whole fork). Everything below is intentional or inherited. If a
differential run flags one of these, it is not a regression -- read this before "fixing" it.

**Inherited from the Skia M130 -> M150 bump.** Not ours, and not fixable here.

- _Glyph antialiasing._ Text renders 1.8-4.1% of pixels differently, with the large deltas confined
  to stem edges. Metrics are unchanged; only rasterisation moved.
- _Zero-segment contours are gone._ A lone `moveTo` immediately followed by another no longer
  survives, so `Path2D.d`, `.edges` and `.bounds` lose the orphan point. `PathBuilder` allows one
  move per contour, `PathBuilder::new_path` re-collapses even a correct `Path`, `Path::raw` rejects
  the verb sequence outright, and Skia's own SVG parser discards it before any of our code runs.
  No pixel effect -- a move-only contour paints nothing.

**Deliberate, and worth keeping.**

- _`imageSmoothingQuality = "high"` picks its sampler from the device-space scale_, where upstream
  aliased `"high"` to `"medium"` (both trilinear). The Chrome mapping this ports, and the reason one
  unconditional cubic is wrong twice over, are documented on `ScalingOperation` and
  `SamplingFilter::sampling_for` in `src/node/filter.rs`. The measurement behind that choice lives
  here because no single call site owns it: zone-plate roughness on a 512-to-64 downscale is 65.44
  with the scale-aware mapping, 76.22 with Mitchell everywhere, and 85.42 with CatmullRom
  everywhere.

- _Solid colours keep float alpha_ rather than being truncated to `u8` before painting, so
  `globalAlpha = 0.5` yields an alpha byte of 128 where upstream gave 127. This accounts for the
  pervasive one-step differences in any pixel comparison against upstream.
- _`simplify()` and `unwind()` no longer mutate the receiver's fill type._ Upstream flipped the
  receiver to even-odd as a side effect, which changed later `contains()` answers.
- _`"modulate"` is accepted_ by `globalCompositeOperation`. Not a Canvas operator; upstream ignored
  it, as the spec requires for an invalid value.
- _`saveLayer` composites one 8-bit step darker_ than the equivalent `globalAlpha` fill -- 126
  against 127 for 50% black on white, exact at 0 and 1. Skia rasterises the layer to 8 bits before
  blending it.

**The two `roundRect` entry points differ, and must keep differing.** The asymmetry is upstream's,
and what the start corner decides is documented at both call sites -- `Context2D::round_rect` in
`src/context2d.rs`, and the `roundRect` accessor in `src/context/api.rs`. Worth knowing before you
read either: it has already been "corrected" once and had to be undone.

### The target list has one source

`lib/targets.json`. `PLATFORM_PACKAGES` in `lib/binary.js` is derived from it at load, and
`package.json` `optionalDependencies` is generated from it by `npm run sync-targets`, which the
release recipes run. So a target is added in one place and the rest follow; editing the generated
copies by hand puts them back the next time anything syncs.

Getting that wrong fails silently -- resolution finds nothing and falls back to the download path,
which is what the platform packages replace. `tests/static/binary.test.js` guards it.

### Releases

`prebuild` holds sha256 hashes of this repo's own release assets -- the seven binaries and the two
Lambda archives. `npm run snapshot` writes them, and `just publish-npm` runs it; without that the
integrity check has nothing to verify against. Platform packages are pinned to the exact package version, so all seven must be published
before the main package on every release.

Use `just publish-npm`, which runs that order and waits on each stage. Rehearse with `just publish-npm dry`
first -- it runs every guard for real and reports what it would do without changing anything.

#### The changelog is written before the tag

By hand, above the previous entry, using the heading format already in the file. `just release-npm`
refuses to proceed without an entry for the version it is cutting, because the GitHub release notes
are generated from the tag and reconstructing intent afterwards means reading commits instead of
remembering why. Prereleases are exempt.

The two channels are numbered separately and are not comparable: npm picks up
`phyron-skia-canvas`'s numbering at `3.6.0`, while the crate starts at `0.2.0`. A change touching
only the build container is an npm release with no crate release, which is the common case.

#### Six things that have cost real time

**A draft release makes CI look broken.** `prebuild.mjs` downloads over a public URL, so the
rendering suite cannot run until the release is undrafted, and it reports as an ordinary failure.
`just publish-npm` undrafts first, which mostly removes the trap.

**Never re-run `build.yml` against a published version.** It uploads with `--clobber` to the tag in
`package.json`, and the published npm package holds sha256 hashes of the assets that were there
before. Replacing them breaks the integrity check for everyone installing through the download
fallback. Rebuilds go to a new version.

**`aws-lambda-*.zip` embeds a packed copy of the module**, so those two archives cannot be reused
across a version bump without repacking -- the inner `package.json` carries the old version. The
seven `.gz` binaries have no such problem; nothing in them encodes the npm version.

**No workflow pulls LFS, and none should.** `docs/assets` is the only LFS path, and nothing in CI
reads it -- the README links those images by URL. `lfs: true` on a checkout therefore buys 22 MB of
documentation screenshots per job and nothing else, across the eleven checkouts that would carry it.
`tests/assets` is ordinary git, so fixtures arrive as real bytes everywhere, including the two files
`Cargo.toml`'s `include` ships inside the crate. `.gitattributes` carries the reasoning. If Skia
reports "could not decode the encoded image bytes", the fixture is missing or corrupt -- it is no
longer a checkout setting.

**Adding a native export turns `ci.yml` red until the next release.** That workflow downloads the
published binary for the version in `package.json` and runs the current JS against it -- which is
the point, it is the install path under test -- but it means the JS half must keep working with the
_previous_ release's binary. Landing a change that alters rendering is fine; the tests were written
against the old behaviour and still pass. Landing one where JS calls a _new_ native export is not:
`Canvas.colorType` reaching for `Canvas_get_colorType` produced `TypeError: Cannot read properties
of undefined` on every leg, cascading through everything that touches `getImageData`. Expected, and
it clears when the release publishes binaries that have the export. `build.yml` is the gate that
actually compiles and tests the new binary; treat a red `ci.yml` between a native change and its
release as this, but confirm the failure is the missing export rather than something real.

**`npm test` does not test what you just built.** An installed platform package outranks
`lib/skia.node`, so after `npm run build` a bare `node --test` still loads the published binary and
the change looks like it did nothing. Use `just test`, which sets `MEO_SKIA_CANVAS_BINARY`, or set
it yourself. The gap is not subtle once you look for it -- on the same working tree, `node --test`
reported 112 pass / 69 fail against the published binary while `just test` reported 181 / 0 -- but
nothing announces it, and an entire differential run was once measured against the wrong binary and
read as "the fix did not land".

### The ABI floors are support commitments

There are **two**, and both fail the same way -- the binary does not load at all, with
`ERR_DLOPEN_FAILED`, on a machine where everything else works:

|           | ceiling    | set by                                          |
| --------- | ---------- | ----------------------------------------------- |
| glibc     | **2.34**   | the base image in `containers/Dockerfile.glibc` |
| `GLIBCXX` | **3.4.25** | gcc-toolset, via the same file                  |

Neither is a measurement. Each is the lowest value across the platforms this project supports:
RHEL 8 (glibc 2.28 / GLIBCXX 3.4.25, to 2029), AWS Lambda and Amazon Linux 2023 (2.34 / 3.4.33, to
2028), RHEL 9 (2.34 / 3.4.29, to 2032). AlmaLinux 8 currently yields 2.28 and 3.4.21, so both pass
with margin -- keep the margin rather than tightening to whatever the base happens to give.

libstdc++ is the one that hides. It was invisible behind glibc until glibc was fixed, and 4.0.0
would have failed on RHEL 9 for this reason alone. gcc-toolset links its own newer libstdc++
statically and leaves only baseline symbols dynamic, which is what keeps the number low; that
behaviour is load-bearing, not incidental.

`build.yml` asserts both after every Linux build, and a separate job loads the published AWS layer
on `public.ecr.aws/lambda/nodejs:22` and renders through it. Changing the base image or the toolset
changes the commitments.

**Verify container changes locally before CI.** `linux/arm64` containers run natively on Apple
Silicon and this machine is faster than the runners, so a full Skia build takes less time here than
a CI round trip -- and the binary can be inspected directly with `objdump -p`. Four separate EL8
gaps were found this way in minutes each, after two had already cost 35-minute CI cycles.

---

## Coding Style & Naming Conventions

- Follow standard Rust style: four-space indentation, `snake_case` for modules/functions, `CamelCase` for types.
- Write idiomatic Rust. Prefer functional style over imperative style.
- Prefer `collect()`/iterator pipelines over `new + for + push/insert`.
- Avoid unnecessary allocations, conversions, copies.
- Avoid `unsafe` code unless absolutely necessary.
- **No trailing `return`.** The last expression of a function is its value; `return x;` on the way out is noise. Early `return` is fine and preferred where it flattens a guard — the FFI entry points open with a run of `return cx.throw_type_error(...)` argument checks, and nesting those into `if/else` would bury the happy path several levels deep for nothing.
- **NO INLINE PATHS**: Always import types at the top of the file using `use` statements. Never use inline paths like `crate::core::Error::Generic(...)` in function bodies.
- Use `SmallVec` for collections that are usually small in hot paths.

### No magic values

**A number or byte string that came from a specification gets a named `const` and a doc comment
saying where it came from.** Not because a name is tidier, but because an unexplained literal cannot
be reviewed: the reader has no way to tell a correct value from a plausible one, and neither does
the person who wrote it six months later.

This has already cost real time. A BMP header carried `0x7357_696E` under a comment reading
`// "sRGB "`. Those four bytes spell `sWin` — the front of `LCS_sRGB` welded to the back of
`LCS_WINDOWS_COLOR_SPACE`, a value the format does not define. It shipped, it survived review, and
nothing caught it, because readers ignore that field and every viewer showed the right picture. A
named constant whose doc comment states which value it is and what `wingdi.h` calls it is a claim
that can be checked; a literal under a comment is a claim that cannot.

What this asks for:

- **Name it, and say where it is from.** The specification, the section, the header file, the
  registry — enough that a reader can look it up without guessing.
- **Prefer the upstream name over any literal at all.** ITU-T H.273 numbers the colour primaries,
  and `skia_safe`'s `named_primaries::CicpId` is `#[repr(u8)]` with those exact discriminants, so
  `CicpId::SMPTE_EG_432_1 as u8` is better than `12` _and_ better than a `const` of our own — it
  cannot drift, because it is the same definition. Reach for a `const` only where nothing upstream
  names the value.
- **Say when a value is fixed rather than chosen.** `cICP`'s matrix byte is 0 because the PNG
  specification requires it, not because 0 tested well. A reader who cannot tell the difference will
  eventually "tune" it.
- **Derive rather than restate.** A scale factor is `(1u32 << 30) as f64`, not `1073741824.0`. A
  header length is the sum of its parts where the parts are already named.
- **Test the value against the standard, not against itself.** Asserting the table matches the table
  proves nothing. `assert_eq!(cicp.primaries, 12)` next to a comment naming H.273 Table 2 is a
  second, independent statement of the same fact, and that is what catches a wrong row.

Exempt: 0 and 1 where they mean nothing but zero and one, array indices, and the small integers in
an arithmetic expression that is itself the explanation (`* 4` for bytes per pixel where the line
above says RGBA8).

### Naming Conventions

- **Casing**: `UpperCamelCase` for types/traits/variants; `snake_case` for functions/methods/modules/variables; `SCREAMING_SNAKE_CASE` for constants/statics.
- **Conversions**: `as_` for cheap borrowed-to-borrowed; `to_` for expensive conversions; `into_` for ownership-consuming conversions.
- **Getters**: No `get_` prefix (use `width()` not `get_width()`). This governs Rust APIs, with two exceptions.
  - The Neon binding under `src/node` and `src/context/api.rs`, where `get_*`/`set_*` free functions are JS property accessors exported in matching pairs (`CanvasRenderingContext2D_get_size`); there the prefix carries the accessor's direction and dropping it would break the pairing with `set_*`.
  - The Canvas-API facade in `src/canvas.rs` and `src/context2d.rs`, where a method mirrors a `getX()` **method** on `CanvasRenderingContext2D` -- `get_transform`, `get_line_dash`, `get_image_data`. These are not properties with a bare-noun equivalent: the Canvas API has both `transform()` (concatenate a matrix) and `getTransform()` (read it back), so dropping the prefix would collide with a different operation. The facade exists so JavaScript knowledge transfers; renaming these breaks the one thing it is for. A plain state reader that mirrors a JS _property_ still takes the bare noun (`filter()`, `text_decoration()`).
- **Tests**: NEVER use `test_` prefix/suffix in test function names. The `#[test]` attribute already marks it as a test.

---

## Error Handling

- **Neon FFI boundary**: Return `cx.throw_error()` or `cx.throw_type_error()`. Never panic.
- **Internal Rust**: Propagate errors with `Result<T>` and `?`.
- **Optional values**: Use `if let Some(...)`, `.unwrap_or()`, or `.ok_or()`.
- Every `unwrap()`/`expect()` under `src/` must have a `// SAFETY:` comment or be replaced; tests are exempt.

---

## Performance Best Practices

### Memory Management

- Avoid unnecessary cloning in hot paths.
- Use `Arc`/`Rc` for shared immutable data.
- Prefer borrowing over ownership transfer when possible.

### String Handling

- Use `&str` instead of `String` where ownership is not needed.
- Avoid `.to_string()` for temporary values.
- Use `from_utf8_lossy()` instead of `from_utf8().unwrap()` for untrusted bytes.

---

## Documentation Guidelines

- All code comments containing complete sentences must end with a period.
- All doc comments must end with a period (unless headlines).
- En-dashes and em-dashes must be written as two dashes: `--`. `rustfmt` runs with `wrap_comments`, and a literal `—` is one character it cannot break a line on.
- References to types, keywords, symbols must be in backticks: `Foo`. Product and format names are prose, not symbols: CanvasKit, OpenType, WebP stay bare.

### Comments say what the code does, not what it used to do

A comment describes the code as it stands. Not what it replaced, not what it did before the last
change, not what some other project's version does.

Change the code and the comment changes with it, in the same commit. A comment left describing the
previous behaviour is worse than none, because nothing marks it as stale and a reader has no way to
tell it from the maintained kind -- it reads as a statement about the code in front of them.

- **No `was`, `had been`, `used to`, `previously`, `before this`.** If a sentence needs one of those
  to parse, it is history, and history goes in the commit message. That is what the log is for and
  it never goes stale, because it is attached to the change rather than to the file.
- **Keep the warnings, phrased as constraints.** "Do not widen this to one unconditional cubic: a
  cubic sets `use_cubic` and Skia then ignores the mipmap chain" tells the next reader what will
  break and can be checked against the code. Rewriting the same point as the story of a change that
  was reverted once cannot.
- **Keep the measurements, stated as what the code costs.** "`Surface::read_pixels` costs about 430
  microseconds whatever the rectangle" is a property of this code. "It used to take 601" is a
  property of a commit.
- **Name the mechanism, not the symptom that found it.** "The page cache is shared between threads,
  so an entry has to be in main memory" survives a rewrite of everything around it. "This is the
  bug from the export crash" does not.

The one thing a comment may reach for outside itself is a name in this tree that a reader can open
-- a type, a function, a module. A bare reference to a release, an issue number or another project
is not something the code can be checked against.

### Doc comments are required on the public API

`#![warn(missing_docs)]` is on. The public API is the crate-root modules re-exported through `prelude`, plus `gui`; the Neon binding (`node`, `context`, `gpu`) is `pub(crate)` and therefore exempt by construction rather than by convention. If the lint fires on binding code, the module visibility is wrong, not the docs.

What a doc comment is for here: what the item is and what a caller needs to know that the signature does not say -- units, ranges, what happens at the boundary, which CSS or Canvas concept it corresponds to. Restating the name is worse than nothing, because it satisfies the lint while telling the reader that the item was never really documented.

---

## Writing Instructions

These apply to user communication and documentation:

- Be concise. Use simple sentences. Technical jargon is fine.
- Do not overexplain basic concepts. Assume the user is technically proficient.
- Avoid flattering, corporate, or marketing language.
- Avoid vague/generic claims not substantiated by context.
- Avoid weasel words.

---

## Commit Messages

Explain why, not what. The diff already says what changed; a message that
restates it has said nothing. What it cannot say is what was wrong, how that
was found, what else was tried, and what is now true that was not before.

Length follows from that rather than from a limit. Most commits here run
twenty to fifty lines of body because that is what the reasoning took; a
genuinely small change takes three. Neither is a target.

- Lead with the defect or the gap, in the terms someone hitting it would
  use, not in the terms of the fix.
- Give the evidence. A measurement, a decoded byte, an assertion that
  failed -- something checkable, not an assurance.
- Say what was rejected and why, where a reader would otherwise wonder. The
  alternative that looks obvious and is wrong is worth a sentence.
- Name what is still not right. A commit that fixes one of two problems
  should say so.
- Prose, not bullets. Bullets fragment an argument that has to hold
  together; these four are a checklist for what to cover, not a template
  for the message.
- Wrap at 72 columns.

---

## Pre-Commit Checklist

1. `just ci` -- runs `fmt-check typecheck lint-check check-api docs licenses
test-rust test build`. All must pass. The recipe is the authority; this
   line has twice drifted behind it, first missing `check-api` and
   `test-rust` and later `docs` and `licenses`, and a reader who trusts it
   concludes a gate did not run when it did.

   What the less obvious ones are for: `check-api` proves no `skia_safe` or
   `neon` type reaches a public signature, `docs` fails on any rustdoc or
   TypeDoc warning, `licenses` fails on a copyleft or unlicensed crate, and
   `test-rust` is the suite the plain `test` recipe does not cover.

   Note that `ci` runs rustdoc **twice**, and both are gates: `docs-rust` on
   stable, which is what docs.rs will render, and `check-api` on the pinned
   nightly, which is newer and carries lints stable does not have.

2. All `unwrap()`/`expect()` calls under `src/` must have `// SAFETY:` comments or proper error handling.
