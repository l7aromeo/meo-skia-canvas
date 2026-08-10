# Repository Guidelines for meo-skia-canvas

This file provides guidance to Claude Code and other AI agents working in this repository.

## Project Context

A fork of [phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas), itself a fork of
[skia-canvas](https://github.com/samizdatco/skia-canvas) -- a Node.js native module (Neon/Rust)
implementing the HTML Canvas API on top of Skia. Inherited extensions add F16/F32 pixel formats,
extended color spaces (P3, Rec.2020, HDR10, HLG, linear), OkLab gradient interpolation, CanvasKit
filter parity, variable font axis control, and a `ParagraphBuilder`/`Paragraph` API.

### What this fork changes

- **Binaries resolve from optional platform packages.** `lib/binary.js` probes
  `meo-skia-canvas-<triplet>` before falling back to the `install` script's download. Each platform
  package declares `os`/`cpu`/`libc`, so a package manager selects one without running any script.
  This exists because bun blocks postinstall scripts unless the package is listed in the consuming
  project's `trustedDependencies`, and that list is not inherited from dependencies -- so no
  package depending on this one could fix it for its own users.
- **Metal exports drain an autorelease pool.** `toBuffer`/`saveAs` hand work to `rayon::spawn_fifo`,
  and a rayon worker has no autorelease pool, so Metal's Objective-C allocations accumulated for the
  life of the process.

Both are open upstream as phyrondev#30 and phyrondev#29. Rebase rather than diverge if they land.

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
  aliased `"high"` to `"medium"` (both trilinear). Ported from Chrome, which with Safari is the
  only engine implementing the property at all -- Firefox has none, and the HTML spec declines to
  mandate an algorithm, so there is no "correct" answer to copy other than an engine's.
  `MatrixToScalingOperation` in `cc/paint/paint_op.cc` decomposes the full local-to-device matrix
  and returns `kUpscale` only when both axes grow; `FilterQualityToSkSamplingOptions` in
  `paint_flags.cc` then maps `kHigh` to Mitchell for that case and to trilinear otherwise. Its
  `kDefault`/CatmullRom arm is legacy and the image path never reaches it. The CTM is part of the
  decision, so a 2x `drawImage` under a 0.25x transform is a minification.

  Do not simplify this to one unconditional cubic. A cubic resampler sets `use_cubic`, and Skia
  then ignores the mipmap chain entirely -- zone-plate roughness on a 512-to-64 downscale goes
  65.44 with the scale-aware mapping, 76.22 with Mitchell everywhere, 85.42 with CatmullRom
  everywhere. Only the first matches upstream's minification quality while still giving `"high"`
  something to mean when magnifying.
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

**The two `roundRect` entry points differ, and must keep differing.** `Path2D.roundRect` pins
`add_rrect`'s start index to 0; `ctx.roundRect` goes through `Path::rrect`, which takes Skia's
legacy 6 (CW) / 7 (CCW). That asymmetry is upstream's. Making them agree moves where
`AddPathMode::Extend` attaches, where the current point lands after a `roundRect`, and where dash
phase begins -- it has already been "corrected" once and had to be undone.

### Target list lives in three places

`package.json` `prebuild`, `package.json` `optionalDependencies`, and `PLATFORM_PACKAGES` in
`lib/binary.js` must agree. Adding a target to one and not the others fails silently -- resolution
finds nothing and falls back to the download path, which is what the platform packages replace.
`tests/suite/binary.test.js` guards this.

### Releases

`prebuild` holds sha256 hashes of this repo's own release assets. It is empty until the first
release; run `npm run snapshot` after publishing one, or the integrity check has nothing to verify
against. Platform packages are pinned to the exact package version, so all seven must be published
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

**Anything reading `tests/assets` needs `lfs: true` on checkout.** Without it the fixtures arrive as
pointer text and Skia reports "could not decode the encoded image bytes", which reads like a
rendering bug rather than a missing file. This has already caught out `ci.yml` and
`crates-io-publish.yml`.

**Adding a native export turns `ci.yml` red until the next release.** That workflow downloads the
published binary for the version in `package.json` and runs the current JS against it -- which is
the point, it is the install path under test -- but it means the JS half must keep working with the
*previous* release's binary. Landing a change that alters rendering is fine; the tests were written
against the old behaviour and still pass. Landing one where JS calls a *new* native export is not:
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

| | ceiling | set by |
|---|---|---|
| glibc | **2.34** | the base image in `containers/Dockerfile.glibc` |
| `GLIBCXX` | **3.4.25** | gcc-toolset, via the same file |

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
changes the commitments; #7 tracks the base.

**Verify container changes locally before CI.** `linux/arm64` containers run natively on Apple
Silicon and this machine is faster than the runners, so a full Skia build takes less time here than
a CI round trip -- and the binary can be inspected directly with `objdump -p`. Four separate EL8
gaps were found this way in minutes each, after two had already cost 35-minute CI cycles.

---

## Project-Specific Rules

## CRITICAL: No AI Residue In The Repository

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

What *does* belong: the commit message, a CHANGELOG entry, a comment next to the code that
needs explaining, and this file. If a decision is worth keeping, it goes in one of those.

## CRITICAL: Git Safety

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

## CRITICAL: No Unwrap/Expect Without Safety Comment

**Every `.unwrap()` and `.expect()` MUST have a `// SAFETY:` comment explaining why it cannot fail, OR must be replaced with proper error handling.**

Panics in Neon FFI crash the Node process -- never acceptable without proof of safety. Use:

- `cx.throw_error()` for Neon FFI boundaries.
- `?` for internal Rust error propagation.
- `unwrap_or()` / `unwrap_or_else()` / `unwrap_or_default()` when a fallback exists.
- `if let Some(...)` / `match` for optional values.

```rust
// BAD: panics crash the Node process.
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
just ci            # fmt-check + typecheck + lint-check + test + build
just typecheck     # cargo check (Linux feature subset)
just lint-check    # cargo clippy (Linux feature subset)
just fmt           # cargo fmt + prettier
just build         # debug build of the native module
just build-release # release build of the native module
just test          # node --test against the local build
```

**Note:** the `metal` feature only compiles on macOS, so the recipes use a Linux-safe feature subset (`vulkan,window,freetype`). Override locally if you're on macOS.

**Never use `--release` unless explicitly requested.** Debug builds are faster and sufficient for development.

---

## Coding Style & Naming Conventions

- Follow standard Rust style: four-space indentation, `snake_case` for modules/functions, `CamelCase` for types.
- Write idiomatic Rust. Prefer functional style over imperative style.
- Prefer `collect()`/iterator pipelines over `new + for + push/insert`.
- Avoid unnecessary allocations, conversions, copies.
- Avoid `unsafe` code unless absolutely necessary.
- Avoid `return` statements; structure functions with if/else blocks instead.
- **NO INLINE PATHS**: Always import types at the top of the file using `use` statements. Never use inline paths like `crate::core::Error::Generic(...)` in function bodies.
- Use `SmallVec` for collections that are usually small in hot paths.

### Naming Conventions

- **Casing**: `UpperCamelCase` for types/traits/variants; `snake_case` for functions/methods/modules/variables; `SCREAMING_SNAKE_CASE` for constants/statics.
- **Conversions**: `as_` for cheap borrowed-to-borrowed; `to_` for expensive conversions; `into_` for ownership-consuming conversions.
- **Getters**: No `get_` prefix (use `width()` not `get_width()`).
- **Tests**: NEVER use `test_` prefix/suffix in test function names. The `#[test]` attribute already marks it as a test.

---

## Error Handling

- **Neon FFI boundary**: Return `cx.throw_error()` or `cx.throw_type_error()`. Never panic.
- **Internal Rust**: Propagate errors with `Result<T>` and `?`.
- **Optional values**: Use `if let Some(...)`, `.unwrap_or()`, or `.ok_or()`.
- Every `unwrap()`/`expect()` must have a `// SAFETY:` comment or be replaced.

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
- En-dashes must be written as two dashes: `--`.
- References to types, keywords, symbols must be in backticks: `Foo`.

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

Keep commit messages concise: 2-3 sentences max.

- One sentence: state the problem/change.
- One sentence: state the fix/implementation.
- Optional: one sentence of context if needed.

No bullet points, long explanations, or multiple paragraphs.

---

## Pre-Commit Checklist

1. `just ci` -- runs `fmt-check typecheck lint-check test build`. All must pass.
2. All `unwrap()`/`expect()` calls must have `// SAFETY:` comments or proper error handling.
