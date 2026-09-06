# meo-skia-canvas -- guidance for agents and contributors

The HTML Canvas 2D API on Skia, shipped twice from one source tree: a Rust crate
and a Node addon (Neon). The same API in each language's spelling, one
implementation underneath.

This file is the authority on how work is done here. Where it names a fact that
lives somewhere else -- a recipe, a workflow, a constant -- it points at that
place rather than restating it, because a copy goes stale in silence and a
reader has no way to tell a stale copy from a maintained one.

---

## The rule that decides everything

**There are two kinds of API here, and they are held to different standards.**

1. **Browser-standard API must match the browser.** Not approximately, and not
   "the specification arguably permits this". If `getImageData`, `fillText` or a
   gradient behaves differently from Chrome, that is a defect until proven to be
   a deliberate, documented divergence. Measure it against a browser rather than
   arguing from the specification text -- the two do come apart, and when they
   do, the section on deliberate divergences below says which one this project
   follows and why.

2. **Everything else is ours, and is held to correctness and long-term
   confidence.** The extensions -- F16/F32 pixel formats, wide-gamut and HDR
   colour spaces, OkLab gradient interpolation, CanvasKit filter parity,
   variable font axes, the `Paragraph` API, the windowing layer -- are features
   of this library. They are not a legacy to be tolerated and they are never
   deleted to make the standard surface tidier. They get the same rigour as the
   standard surface, and where no standard governs them, this tree's own rules
   decide.

Following the standard does not mean removing what the standard does not
mention. Perfecting an extension is as much the job as matching a browser.

**Correctness and performance rank together; memory tuning comes last**, so that
it has a fixed baseline to tune against.

**Do not leave a known defect in place to satisfy a weak argument** -- schedule
pressure, a passing test that does not test it, or "no one hits this". If
something is wrong and cannot be fixed now, say so plainly and record why.

---

## Working agreements

**Nothing is pushed and nothing is released without an explicit instruction.**
Commit freely, gate the work, then stop and report. Pushing, opening a PR,
publishing and tagging are all maintainer decisions, taken one at a time.
Approval for one does not carry to the next.

**The maintainer's working arrangements stay out of the repository.** Commits,
issues, pull requests and code comments describe the code and the reasoning.
How the work was organised is private.

**Nothing an agent produces for its own benefit belongs in this repository.**
Plans, specs, design notes, task lists, scratch analyses, session transcripts,
progress trackers, review write-ups, `.ai/`, `.cursor/`, `.aider*`, `.claude/`,
`docs/superpowers/` -- none of it. Work in a scratch directory outside the repo
and let the commit message carry whatever needs to survive.

This is not a style preference. Such files are written for one moment and are
wrong by the next release; they describe intentions rather than the code that
shipped, and nobody updates them -- so they become confidently misleading
documentation that a future reader, human or agent, cannot tell from the
maintained kind. If a tool recreates such a path, delete it rather than
committing it.

What _does_ belong: the commit message, a CHANGELOG entry, a comment next to the
code that needs explaining, and this file.

### Git safety

**Never run `git reset --hard`, `git checkout --`, `git clean` or any other
destructive git command without stashing first.** Uncommitted work cannot be
recovered afterwards.

```bash
git stash push -m "backup before reset"
git reset --hard <target>
git stash pop   # if it went wrong
```

Prefer the non-destructive form where one exists: `git branch -f` moves a ref
without touching the working tree, where `reset --hard` does both.

---

## Evidence

The recurring failure in this repository is not bad code. It is a claim that
nobody checked, repeated until it is load-bearing.

**Verify a claim before you act on it or pass it on.** A figure recalled from
memory, quoted from a report, or inherited from another agent is a claim, not a
measurement. Run the command and use what it printed. A wrong number in a brief
is the most expensive kind, because the reader cannot tell it from a target and
will spend real time reconciling it.

**A check that cannot fail is not a check.** Before trusting a probe, make it
produce the wrong answer: mutate the thing it guards, add a case that must be
refused, put a version known to be broken in the same run. A green result from
an instrument that was never shown to go red says nothing. This applies to
tests, benchmarks, sweeps and one-off scripts alike.

**Prove a search pattern before you trust what it found -- or what it did
not.** A grep, a regex or a glob is an instrument, and the two ways it fails
point in opposite directions. Too loose, and it matches things you did not mean
and quietly agrees with whatever you already believed. Too tight, and it returns
nothing -- which reads as "the tree is clean" rather than "the pattern is
broken", and that is the direction that ends a sweep early.

So before acting on a result: run the pattern against something it **must**
match, and against something it **must not**. If you cannot make it hit a case
you know is there, the pattern is wrong and the empty result means nothing. If
it hits a case you know is absent, so does everything else it reported.

Four ways this has actually gone wrong here, all in one sweep:

- **The pattern matched the tool's own output.** `grep -n` prefixes each line
  with `file:line:`, and a pattern looking for `\.(rs|js):[0-9]+` matched that
  prefix rather than anything in the file, so every line "matched".
- **A substring where a word was meant.** `grep "eight"` matches `Height`. The
  sweep for a stale count came back full of hits about eight-bit colour. `-w`
  is the difference.
- **A character class that stopped early.** `as [A-Z][A-Za-z]*` does not match
  `FillPath2D`, because the class stops at the digit -- so a survey of declared
  verbs silently omitted every name carrying a number, and the conclusion drawn
  from it was wrong.
- **A count pattern that also matches the negative case.** `[1-9][0-9]* failed`
  looks like it finds failures, and matches `10 passed; 0 failed` on the `10`.

Prefer the tool that understands the structure over the one matching text:
`git grep -w`, `--fixed-strings`, the language's own parser, or a script that
walks the items rather than the lines. When only text will do, **say what the
pattern would print under the opposite hypothesis** before you run it. If the
answer is "the same thing", it is not an instrument.

**Say what you checked and what you did not.** A sweep that reports only
findings cannot be told from one that never ran. Name the negative results and
the parts you judged rather than verified.

**Report outcomes as they are.** If a gate failed, say so and quote the
decisive line. If a step was skipped, say which. A green that does not cover
the change is worth less than a red that does -- see the note on `ci.yml`
skipping its rendering suite below.

---

## Build, test and development

Use `just`. `just --list` names every recipe and the `justfile` is the
authority on what each one does -- read the recipe rather than a description of
it, including the reasoning the recipes carry about feature sets and about
which CI configurations cannot be reproduced on a developer machine.

```bash
just               # list the recipes
just ci            # the full gate; everything must pass before a commit
just test          # both suites, Rust and JavaScript, against the local build
just test-rust     # the Rust half alone
just test-js       # the JavaScript half alone
just build         # debug build of the native module
```

A recipe's name says what it covers. `test` runs both languages because a
recipe called `test` that ran one of them is how the Rust suite went unrun
inside `just ci` for a while, and the halves are `test-rust` and `test-js`.
The two API gates are named for what they check rather than for each other:
`check-rust-api` fails when a public Rust signature leaks a `skia_safe` or
`neon` type, and `check-dts-surface` fails when `lib/index.d.ts` disagrees with
what the built addon exposes. They are unrelated checks and were previously a
word apart, which cost time more than once.

**`npm test` does not test what you just built.** An installed platform package
outranks `lib/skia.node`, so a bare `node --test` loads the _published_ binary
and your change appears to have done nothing. Use `just test`, which sets
`MEO_SKIA_CANVAS_BINARY`, or set it yourself. Nothing announces the difference.

**Never build `--release` unless asked.** Debug builds are faster and are what
development wants.

**A green `ci.yml` on a branch that touches `src` did not run the JavaScript
suite.** That workflow downloads the published binary for the version in
`package.json`, and skips the rendering suite when `src` or `build.rs` has moved
since that version's tag, emitting a notice and staying green. What actually
covers the engine is the `binding (Rust + JS on a fresh build)` job in
`rust-ci.yml`, which compiles from source and runs both halves. Open that job,
not the platform legs, when the question is whether the suite passes.

---

## Errors and refusals

- **Neon FFI boundary:** `cx.throw_error()` or `cx.throw_type_error()`. Never
  panic.
- **Internal Rust:** propagate with `Result<T>` and `?`.
- **Optional values:** `if let Some(...)`, `.unwrap_or()`, `.ok_or()`.

### No `unwrap` or `expect` without a `// SAFETY:` comment

**Every `.unwrap()` and `.expect()` under `src/` must carry a `// SAFETY:`
comment explaining why it cannot fail, or be replaced with real error
handling.** Tests are exempt: there a panic _is_ the failure report, and the
string in `.expect("raw export")` already names the expectation.

A panic crossing the Neon boundary is caught and raised as `Error: internal
error in Neon module`, which JavaScript can catch -- this crate does not set
`panic = "abort"`, so the process usually survives. That is not a reason to
relax the rule: the error is opaque, names no cause, and gives the caller
nothing to handle. An allocation failure is the exception that genuinely aborts.

The rule does not catch every panic. One inside a dependency -- Skia returning
null into a `skia-safe` `unwrap` -- has no call of ours to annotate, so validate
inputs before handing them to a C++ layer that cannot report failure.

```rust
// BAD: an opaque Neon internal error.
let result = some_operation().unwrap();

// GOOD: propagate to JavaScript.
let result = some_operation().ok_or_else(|| "operation failed".to_string())?;

// GOOD: provably safe, with the reason.
// SAFETY: `collection` was set to `Some` on the previous line.
let coll = self.collection.as_ref().unwrap();
```

### Which exception type a refusal takes

Five rules, in priority order. The first that applies wins.

1. **The standard names a `DOMException`** -- raise that, by name. The Canvas
   standard says `addColorStop` throws `IndexSizeError` for an offset outside
   `[0, 1]` and `SyntaxError` for a colour it cannot parse; `arc` and `ellipse`
   throw `IndexSizeError` for a negative radius. Neon can construct an `Error`,
   a `TypeError` and a `RangeError` and nothing else, so the name crosses as
   text -- `cx.throw_error("IndexSizeError: ...")` -- and `lib/classes/neon.js`
   builds the exception. Only names in its `DOM_EXCEPTIONS` set are honoured.
2. **A value outside an enumeration is a `TypeError`** -- WebIDL's rule for an
   enum, whether or not the standard defines that enum. An unrecognised
   `colorSpace`, `colorType`, `chromaSampling` or blend mode is this.
3. **A sequence of the wrong length is a `TypeError`** -- what Chrome raises for
   `new DOMMatrix([1,2,3])`.
4. **A number outside a permitted range or set is a `RangeError`** -- the
   argument is the right kind and its value is not, which is the distinction
   `RangeError` exists for. `bitDepth` taking one of 8, 10 and 12 is this and
   not case 2: the argument is a number, not a spelling.
5. **A value of the wrong kind entirely is a `TypeError`** -- WebIDL's rule when
   interface conversion fails, and what a browser raises for
   `ctx.drawImage(42)`. Distinct from case 2: there the kind is right and the
   spelling is wrong; here it is not the kind the signature names at all.

A bare `cx.throw_error` is for none of these. It gives calling code nothing to
branch on, and outside case 1 -- where the name in the message is the point --
it means the rule above was never chosen.

**Say which rule a site follows where the answer is not obvious**, in a comment
at the throw. These rules drifted once precisely because nothing at the call
sites recorded them.

### A refusal is not always a throw

An unknown _key_ in an options object is additive: the caller passed something
extra and everything they asked for still happens, so it is ignored unless
`SKIA_CANVAS_STRICT` is set. An invalid _value_ is substitutive: the thing the
caller asked for will not happen, and silence leaves them with a window of
unexplained size or a cursor they did not set. That throws, in every mode.

Worth stating because the distinction is invisible from a call site -- one costs
the caller nothing, the other costs them the operation.

---

## Coding style

- Standard Rust style: four-space indent, `snake_case` for
  modules/functions, `CamelCase` for types.
- Write idiomatic, functional Rust. Prefer iterator pipelines over
  `new + for + push`.
- Avoid unnecessary allocations, conversions and copies. `&str` over `String`
  where ownership is not needed. `Arc`/`Rc` for shared immutable data. Borrow
  rather than transfer ownership where you can. `SmallVec` for collections that
  are usually small in hot paths.
- `from_utf8_lossy()` rather than `from_utf8().unwrap()` for untrusted bytes.
- Avoid `unsafe` unless there is no alternative.
- **No trailing `return`.** The last expression is the value. Early `return` is
  fine and preferred where it flattens a guard -- the FFI entry points open with
  a run of `return cx.throw_type_error(...)` checks, and nesting those would
  bury the happy path for nothing.
- **No inline paths.** Import types with `use` at the top of the file; never
  write `crate::core::Error::Generic(...)` in a function body.

### Naming

- **Casing:** `UpperCamelCase` for types, traits and variants; `snake_case` for
  functions, methods, modules and variables; `SCREAMING_SNAKE_CASE` for
  constants and statics.
- **Conversions:** `as_` for cheap borrowed-to-borrowed, `to_` for expensive,
  `into_` for ownership-consuming.
- **Getters:** no `get_` prefix -- `width()`, not `get_width()`. Two exceptions.
  The Neon binding under `src/node` and `src/context/api.rs`, where `get_*` and
  `set_*` are JS property accessors exported in matching pairs and the prefix
  carries the direction. And the Canvas-API facade in `src/canvas.rs` and
  `src/context2d.rs`, where a method mirrors a `getX()` **method** rather than a
  property: the Canvas API has both `transform()` and `getTransform()`, so
  dropping the prefix would collide with a different operation. A plain reader
  mirroring a JS _property_ still takes the bare noun.
- **Tests:** never a `test_` prefix or suffix. `#[test]` already says so.

### No magic values

**A number or byte string that came from a specification gets a named `const`
and a doc comment saying where it came from.** Not because a name is tidier: an
unexplained literal cannot be reviewed. The reader has no way to tell a correct
value from a plausible one, and neither does the person who wrote it six months
later.

This has cost real time. A BMP header carried `0x7357_696E` under a comment
reading `// "sRGB "`. Those four bytes spell `sWin` -- the front of `LCS_sRGB`
welded to the back of `LCS_WINDOWS_COLOR_SPACE`, a value the format does not
define. It shipped and survived review, because readers ignore that field and
every viewer showed the right picture. A named constant whose doc comment states
what `wingdi.h` calls it is a claim that can be checked; a literal under a
comment is a claim that cannot.

- **Name it, and say where it is from** -- the specification, the section, the
  header, the registry. Enough to look up without guessing.
- **Prefer the upstream name over any literal.** `CicpId::SMPTE_EG_432_1 as u8`
  is better than `12` _and_ better than a `const` of our own, because it cannot
  drift -- it is the same definition.
- **Say when a value is fixed rather than chosen.** `cICP`'s matrix byte is 0
  because the PNG specification requires it, not because 0 tested well. A reader
  who cannot tell will eventually tune it.
- **Derive rather than restate.** `(1u32 << 30) as f64`, not `1073741824.0`.
- **Test the value against the standard, not against itself.** Asserting the
  table matches the table proves nothing.

Exempt: 0 and 1 meaning zero and one, array indices, and small integers in an
expression that is its own explanation.

---

## Documentation

- Comments and doc comments that are complete sentences end with a period.
- Write dashes as `--`. `rustfmt` runs with `wrap_comments` and cannot break a
  line on a literal em-dash.
- Types, keywords and symbols in backticks. Product and format names are prose:
  CanvasKit, OpenType, WebP stay bare.

### Comments say what the code does, not what it used to do

A comment describes the code as it stands -- not the code that stood there
before the last change, and not what some other project does. A comment left
describing previous behaviour is worse than none, because nothing marks it as
stale.

**Change the code and change its comments in the same commit.** This is not
housekeeping to be caught later. The class does not rot slowly: it rots at the
moment of the change that invalidates it, and every hour it survives is an hour
someone can read it and believe it. A sweep of this tree found most of its wrong
statements had been written the same day, by the work that had just moved the
code underneath them -- including two in a header whose own author did not
revisit it after a later change of theirs falsified it.

The reason a careful reader still misses these is mechanical: **the comment that
goes stale is usually not in the diff.** A doc block sits above a function; a
default `git diff` shows three lines of context, so an edit inside that function
shows nothing of the prose describing it. Read the whole item you touched, or
widen the context (`git diff -U20`), rather than reviewing the hunk alone.

The cost of not doing it is measured in whole-tree sweeps. One missed comment is
cheap to fix at the keyboard and expensive to find afterwards, because nothing
distinguishes it from the maintained kind and the only way back is to re-read
every area again.

- **Past tense describes the alternative, never this code.** What the code does
  is present tense. Past tense is legal where it names something the code is
  _not_ -- an approach that was tried and what it cost, a bound that did not
  hold -- and earns its place by saying why the current shape is the current
  shape.
- **A reader must be able to tell which is which.** Say what the code does now,
  then what was rejected, in that order, so the reader reaches the true
  statement first. A comment that is _only_ history belongs in the commit
  message, which never goes stale because it is attached to the change.
- **Keep warnings, phrased as constraints.** "Do not widen this to one
  unconditional cubic: a cubic sets `use_cubic` and Skia then ignores the mipmap
  chain" can be checked against the code. The same point told as the story of a
  reverted change cannot.
- **Keep measurements, stated as what the code costs.** "`Surface::read_pixels`
  costs about 430 microseconds whatever the rectangle" is a property of this
  code. "It used to take 601" is a property of a commit.
- **Name the mechanism, not the symptom that found it.** "The page cache is
  shared between threads, so an entry has to be in main memory" survives a
  rewrite of everything around it. "This is the bug from the export crash" does
  not.
- **A comment about the repository goes stale in silence.** A comment that
  states a fact about the tree -- what is tracked, what flag a command carries,
  what another file enforces -- keeps reading correctly long after that fact
  stops being true, because nothing under it changed. If a comment asserts
  something a reader would have to open another file to check, either make it
  checkable from here or expect it to rot. This applies to this file too.

The one thing a comment may reach for outside itself is a name in this tree that
a reader can open: a type, a function, a module.

### Doc comments on the public API

`#![warn(missing_docs)]` is on. The public API is the crate-root modules
re-exported through `prelude`, plus `gui`; the Neon binding is `pub(crate)` and
exempt by construction rather than by convention. If the lint fires on binding
code, the module visibility is wrong, not the docs.

A doc comment says what the item is and what the signature does not: units,
ranges, boundary behaviour, which CSS or Canvas concept it corresponds to.
Restating the name is worse than nothing, because it satisfies the lint while
telling the reader the item was never really documented.

**When you edit or delete an item's doc comment, read the item below it.** A doc
block written above an existing one silently reattaches: rustdoc concatenates
the two and renders both on the following item, leaving the item the first block
described undocumented. Nothing catches it -- `missing_docs` is satisfied
because a comment exists, rustdoc has no opinion about which item a comment
describes, and the rendered page looks deliberate. Deleting an item leaves its
comment behind to reattach the same way. This is a habit rather than a gate,
because neither the gate nor a careful reading of the diff has proved
sufficient.

---

## Commit messages

**The subject line says what, in Conventional Commits form. The body says
why.** Both are needed and they do different jobs: scanning `git log` a year
later, the subject is all you get, and a subject that only gestures at a reason
forces a dig through the diff to recall what the commit actually did.

```
type(scope): what changed, in the imperative

Why it changed. What was wrong, how that was found, what else was tried,
and what is true now that was not before.
```

**Types:** `feat` for a new capability, `fix` for a defect, `perf` for a
measured speed or memory change, `refactor` for a change with no behavioural
difference, `docs`, `test`, `build`, `ci`, `chore`, `revert`. A breaking change
takes a `!` before the colon -- `feat(context)!:` -- and says so in the body.

**The scope names the part of the tree the change touches**, and has to match
it. A commit whose scope says `gradient` and whose diff moves the export path
is worse than one with no scope at all, because the scope is what a later
search filters on. Use the module or subsystem a reader would look for:
`gradient`, `text`, `export`, `decode`, `gpu`, `gui`, `binary`, `dts`,
`justfile`, `workflows`. Omit the scope only when a change genuinely spans the
tree.

Subject line: imperative, no trailing period, under about seventy characters.

The body is where this repository's standards live, and they have not changed:

- Lead with the defect or the gap, in the terms someone hitting it would use,
  not in the terms of the fix.
- Give the evidence: a measurement, a decoded byte, an assertion that failed --
  something checkable, not an assurance.
- Say what was rejected and why, where a reader would otherwise wonder.
- Name what is still not right. A commit fixing one of two problems says so.
- Prose, not bullets. Wrap at 72 columns.

Body length follows from the reasoning rather than from a limit. Most commits
here run twenty to fifty lines; a genuinely small change takes three.

```
fix(gradient): leave the page alone when a gradient paints nothing

A fill covering the whole canvas with a gradient the Canvas standard says
paints nothing cleared the canvas instead. `Context2D::draw_path` discards
recorded content when a fill covers the page opaquely, and `is_opaque` for
a gradient was `!colors.any(|c| c.a < 1.0)` -- vacuously true for an empty
stop list, so a gradient with no colours reported itself opaque while its
shader painted nothing over what that claim had thrown away.
```

---

## Writing for people

- Be concise. Simple sentences. Technical jargon is fine.
- Do not overexplain. Assume the reader is technically proficient.
- No flattering, corporate or marketing language. No weasel words.
- No vague claims the context does not support.

---

## Where output differs from a browser, or extends it

Everything here is intentional or inherited from Skia. If a differential run
against a browser flags one of these, it is not a regression -- read this first.

**Inherited from Skia, and not fixable here.** Glyph antialiasing differs by a
few percent of pixels, confined to stem edges; metrics are unchanged. A contour
of a single `moveTo` no longer survives, so `Path2D.d`, `.edges` and `.bounds`
lose the orphan point -- no pixel effect, since a move-only contour paints
nothing.

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

**Gradient stops interpolate in sRGB, and Chrome does not. Ours is the
specification's answer.** The HTML Standard says the colours "must be linearly
interpolated in the context's color space", which for a default canvas is sRGB.
Chrome interpolates in Oklab whenever a stop is written in `lab()`, `lch()`,
`oklab()`, `oklch()` or `color()`, and its own `color-mix` is the proof. This is
the largest pixel delta against Chrome anywhere in the library, the endpoints
agree exactly, and copying Chrome here would move away from the standard.

This is the one place the first rule at the top of this file is answered against
the browser rather than for it, and the reasoning is written out because that is
what makes it a decision rather than a bug.

---

## Project mechanics

**The target list has one source: `lib/targets.json`.** `PLATFORM_PACKAGES` in
`lib/binary.js` derives from it at load, and `package.json`'s
`optionalDependencies` is generated by `npm run sync-targets`, which the release
recipes run. Editing a generated copy by hand is undone the next time anything
syncs, and getting it wrong fails silently -- resolution finds nothing and falls
back to the download path. `tests/static/binary.test.js` guards it.

**Binaries resolve from optional platform packages.** `lib/binary.js` probes
`meo-skia-canvas-<triplet>` before falling back to the install script's
download. Each platform package declares `os`, `cpu` and `libc`, so a package
manager selects one without running any script. This exists because bun blocks
postinstall scripts unless the consuming project lists the package in
`trustedDependencies`, and that list is not inherited from dependencies.

**Metal exports drain an autorelease pool.** `toBuffer` and `toFile` hand work
to `rayon::spawn_fifo`, and a rayon worker has no autorelease pool, so Metal's
Objective-C allocations accumulated for the life of the process.

### The ABI floors are support commitments

Two of them, and both fail the same way: the binary does not load at all, with
`ERR_DLOPEN_FAILED`, on a machine where everything else works.

|           | ceiling    | set by                                          |
| --------- | ---------- | ----------------------------------------------- |
| glibc     | **2.34**   | the base image in `containers/Dockerfile.glibc` |
| `GLIBCXX` | **3.4.25** | gcc-toolset, via the same file                  |

Neither is a measurement. Each is the lowest value across the platforms this
project supports -- RHEL 8, AWS Lambda and Amazon Linux 2023, RHEL 9. Keep the
margin rather than tightening to whatever the base image happens to give.

libstdc++ is the one that hides: it was invisible behind glibc until glibc was
fixed. gcc-toolset links its own newer libstdc++ statically and leaves only
baseline symbols dynamic, which is what keeps the number low -- that behaviour
is load-bearing, not incidental. `build.yml` asserts both after every Linux
build, and a separate job loads the published Lambda layer and renders through
it. Changing the base image or the toolset changes the commitments.

**Verify container changes locally before CI.** `linux/arm64` containers run
natively on Apple Silicon, and this machine is faster than the runners, so a
full build costs less here than a CI round trip -- and the binary can be
inspected directly with `objdump -p`.

### Releases

The `justfile` recipes are the authority on the steps and carry their own
reasoning; read them rather than a summary. What is worth knowing before
starting:

- **Nothing is released without an explicit instruction**, and the version and
  the changelog are decided by the maintainer.
- **The changelog entry is written before the tag**, by hand, above the previous
  entry. `just release-npm` refuses to proceed without one, because the release
  notes are generated from the tag and reconstructing intent afterwards means
  reading commits instead of remembering why. Prereleases are exempt.
- **npm and the crate are separate channels with separate numbering** and are
  not comparable. Most releases move only npm.
- **The build workflow is the gate**, not the publish step. Until it is green
  nothing is published and the release can still be abandoned. A rebuild goes to
  a new version, never a re-run against the same tag -- the published package
  holds sha256 hashes of the assets that were there before, and replacing them
  breaks the integrity check for everyone installing through the download
  fallback.
- **A draft release stops the rendering suite, and that is not a failure.** A
  draft's assets are not downloadable, so `ci.yml` cannot fetch a binary between
  the tag and the publish. It handles this with a notice and stays green. A red
  `ci.yml` in that window is therefore something else.
- **The Lambda archives embed a packed copy of the module**, so they cannot be
  reused across a version bump without repacking. The platform binaries have no
  such problem.
- **No workflow pulls LFS, and none should.** `docs/assets` is the only LFS path
  and nothing in CI reads it. `tests/assets` is ordinary git, so fixtures arrive
  as real bytes everywhere.

### Upstream

`samizdatco/skia-canvas` sits on the `upstream` remote with its push URL set to
`DISABLED`. It is not a reference and not a destination for work; this tree
decides its own behaviour by the rule at the top of this file. If a fix there is
ever worth taking, cherry-pick it on its own merits -- their `skia-safe` is old
enough that anything shaped by those bindings is a downgrade here.

---

## Before every commit

1. **`just ci` must pass, all of it.** The recipe in the `justfile` is the
   authority on what it runs; do not trust a list of it written anywhere else,
   including here. Note that it runs rustdoc twice and both are gates: once on
   stable, which is what docs.rs will render, and once on the pinned nightly,
   which carries lints stable does not have.

2. **Nothing after `just ci` in the same command line.** A pipe, an `&&` or a
   trailing `echo` replaces the gate's exit status with its own.

3. **Every `unwrap()` and `expect()` you added under `src/` carries a
   `// SAFETY:` comment**, or does not exist.

4. **Re-read the comments around everything you changed**, including the ones
   the diff did not show you -- the doc block above the function, the header of
   the file, and any comment elsewhere that describes the behaviour you moved.
   A comment that is now wrong is a defect in this commit, not a task for
   later.

5. **Then stop.** Report what passed, what you skipped and what you are unsure
   of, and wait.
