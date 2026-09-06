set shell := ["bash", "-euo", "pipefail", "-c"]

# On Linux, `metal` feature does not compile -- use feature subset.

lib := justfile_directory() / "lib" / "skia.node"
linux_features := "vulkan,window,freetype"
# The GPU backend this machine can actually compile, with the binding on top,
# which is the pair the `clippy` matrix in rust-ci.yml runs for this platform.
# Kept apart from `linux_features` because that set is missing `node-addon`
# and carries `freetype`, so linting with it leaves the binding unlinted.
host_features := if os() == "macos" { "metal,window,node-addon" } else { "vulkan,window,node-addon" }
# Must match the fmt job in .github/workflows/rust-ci.yml.
fmt_toolchain := "nightly-2026-09-04"

# Default: show available recipes.
default:
    @just --list

# Aggregate: what CI runs. Uses non-fixing variants.
#
# `licenses` is in here because it was not, and the count in
# THIRD-PARTY-NOTICES.md went stale with nobody to notice -- it claimed 135
# packages long after the graph had moved. The recipe exits non-zero on a
# copyleft or unlicensed crate, so this also fails the build rather than
# waiting for someone to read the output.
#
# `check-docs` is here for the same reason, having run only from `precommit`
# and from `rust-ci.yml`. The hook is opt-in, so a contributor who declined it
# and ran the documented full gate got a weaker answer than the pull request
# would. Second in the list because it costs milliseconds and its JavaScript
# and TypeScript half now reads the tree rather than the diff -- so it can fail
# on something the current change did not touch, and that is worth learning
# before the push rather than after.
[doc("Aggregate: everything CI runs, in non-fixing variants.")]
ci: fmt-check (check-docs "origin/main") typecheck lint-check check-api check-api-surface docs licenses test-rust test build

[private]
ensure-deps:
    @test -d node_modules || bun install --frozen-lockfile

# Always builds, never just checks the file exists. A stale `lib/skia.node`
# kept `just test` green for a day after `node-addon` stopped compiling: the
# suite was exercising a binary from before the code was deleted. Cargo is
# incremental, so an unchanged tree costs a second or two.
[private]
ensure-binary: ensure-deps
    bun run build -- dev

# Rust and TypeScript both, like `fmt`: the declaration files in lib/ are what the
# package ships as its `types`, and nothing else checks them.
#
# Not `check`: the `-check` suffix on every other recipe here means "the variant that
# reports instead of rewriting", and a bare `check` reads as the same idea one word short.
#
# `just --list` shows the last comment line before a recipe, so anything with more to say
# than one line carries an explicit [doc] -- otherwise the listing quotes a stray tail.
[doc("Type-check Rust and the shipped TypeScript declarations.")]
typecheck: ensure-deps
    cargo check --all-targets --features "{{ linux_features }}"
    bun run typecheck

# What the pre-commit hook runs: the checks that are fast enough to sit in
# front of every commit.
#
# Not the whole gate, deliberately. `lint-check` runs clippy twice and the
# second pass carries `{{ host_features }}`, which is most of its ten
# seconds; `test` and `build` are minutes. A hook that costs that much stops
# being run -- `--no-verify` is one flag away -- and a hook nobody runs
# enforces nothing. These four cost about six seconds together and catch what
# is actually forgotten: formatting, and a lint that fails the build.
#
# CI remains the authority. This only moves the failure earlier.
#
# `install-hooks` is what puts this in front of a commit; it is opt-in and
# run once per clone.
[doc("The pre-commit subset: formatting both languages, ESLint, featureless clippy.")]
precommit: ensure-deps check-docs
    cargo +{{ fmt_toolchain }} fmt --all -- --check
    bun run format:check
    bun run lint
    cargo clippy --all-targets --no-default-features -- -D warnings

# Catch a doc comment stacked above another instead of above its own item.
#
# rustdoc concatenates the two and lands both on the following item, leaving
# the item the first block described undocumented. `missing_docs` is satisfied
# because a comment exists, rustdoc has no opinion about which item a comment
# describes, and the rendered page looks deliberate -- so nothing else in the
# gate sees it. Nine were found across seven files, five introduced within two
# days, one while fixing another.
#
# The Rust half is scoped to the staged diff rather than the tree, and that is
# not a convenience. Tree-wide it cannot work: a stacked summary and the
# closing sentence of a paragraph are textually identical, and what separates
# them is whether the sentence describes the item below, which is
# comprehension. Measured against paragraph count and the two overlap
# completely. So the property enforced is "do not add another one", which is
# the failure that actually happens. Nothing is exempted, because nothing is
# listed.
#
# The JavaScript and TypeScript half does gate the tree, because there the
# defect inverts. TypeScript keeps the last doc block before a declaration and
# drops every earlier one, so nothing is misattributed and the earlier block is
# simply not published -- and the seam is exact, a line ending `*/` immediately
# followed by one opening `/**`, with no prose to judge. Six were on `main` in
# `lib/index.d.ts`, twice discarding the block that explained why a construct
# signature is absent while leaving the item still looking documented.
#
# `base` decides which question the Rust half is asked, and the three callers
# want two different ones. `precommit` passes nothing and gets the staged diff,
# because that is what is about to become a commit. `ci` passes `origin/main`
# and gets the range a pull request will be reviewed over -- a contributor who
# declined the opt-in hook runs `ci` to find out what CI will say, and the
# staged diff is empty on a clean tree, which is the normal state after a
# commit. Asked that way the Rust half examined nothing and reported success.
# It now prints how many files and added lines each pass read, so a zero reads
# as a zero rather than as coverage. `rust-ci.yml` asks the range form itself,
# against the pull request's own base.
#
# The range is skipped rather than fatal when the base ref is missing, which is
# a shallow clone or a fork with no `origin`. The JavaScript half is tree-wide
# under every one of them and is unaffected by any of this.
#
# The self-test runs first and checks the scanner rather than the tree: it
# appends a known stacked pair to every tracked file and asserts the count
# back. The scanner was blind in two of sixty files and said so with a green
# tick, which is the one failure a tree-wide gate cannot survive.
[doc("Fail on a stacked doc comment: staged or ranged Rust, all JavaScript and TypeScript.")]
check-docs base="":
    #!/usr/bin/env bash
    set -euo pipefail
    node scripts/check-stacked-docs.mjs --self-test
    if [[ -z "{{ base }}" ]]; then
        node scripts/check-stacked-docs.mjs --cached
    elif git rev-parse --verify --quiet "{{ base }}" >/dev/null; then
        node scripts/check-stacked-docs.mjs --range "{{ base }}...HEAD"
    else
        echo "==> {{ base }} is not in this clone, so the range check is skipped."
        node scripts/check-stacked-docs.mjs --cached
    fi

# Install the pre-commit hook. Opt-in, and run once per clone.
#
# Writes one file into `.git/hooks/` rather than setting `core.hooksPath`,
# which is what husky and lefthook do: this repository already has four
# hooks there from git-lfs -- post-checkout, post-commit, post-merge and
# pre-push -- and redirecting the path would silently stop all of them.
# `docs/assets` is still LFS, so that is not a cost worth paying for a
# formatting check.
#
# Not installed automatically. A `prepare` script would do it on every
# `bun install`, but this project routes around lifecycle scripts on
# purpose -- the platform packages exist because bun blocks them -- and
# adding one back to install a convenience is the wrong trade.
[doc("Install the pre-commit hook. Opt-in, run once per clone.")]
install-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    hooks=$(git rev-parse --git-path hooks)
    printf '%s\n' \
        '#!/bin/sh' \
        '# Installed by `just install-hooks`. Delete this file to stop it.' \
        'exec just precommit' \
        > "$hooks/pre-commit"
    chmod +x "$hooks/pre-commit"
    echo "installed $hooks/pre-commit"
    echo "untouched: $(ls "$hooks" | grep -v sample | grep -v '^pre-commit$' | tr '\n' ' ')"

# Run clippy and ESLint with autofix (modifies working tree).
#
# Both languages, the way `fmt` covers both: the split here is by what the
# recipe does -- fix, against the `-check` pair that only reports -- rather
# than by which language it does it to.
[doc("Run clippy and ESLint with autofix (modifies the working tree).")]
lint: ensure-deps
    cargo clippy --fix --allow-dirty --allow-staged --all-targets --no-default-features -- -D warnings
    cargo clippy --fix --allow-dirty --allow-staged --all-targets --features "{{ host_features }}" -- -D warnings
    bun run lint:fix

# Run clippy without fixing (CI-safe).
#
# Two passes, because one feature set does not lint the crate. The matrix in
# rust-ci.yml runs three -- no features, and each platform's GPU backend with
# the binding -- and only one of those was reachable here, on a set that
# happened to include neither. `ThreadBound` is built solely by the two GPU
# engines, so with none compiled it is dead code and `-D warnings` refuses it:
# a red CI job on a branch whose local gate was green.
#
# The third of CI's three is the other platform's backend, which does not
# compile here at all -- that one is what CI is for.
[doc("Run clippy and ESLint without fixing. Two clippy passes: no features, then this host's.")]
lint-check: ensure-deps
    cargo clippy --all-targets --no-default-features -- -D warnings
    cargo clippy --all-targets --features "{{ host_features }}" -- -D warnings
    bun run lint

# Rust and JavaScript both: `just ci` checks both, so fixing only one half still fails.
#
# Rust uses the same pinned nightly as the fmt job in rust-ci.yml. rustfmt.toml
# turns on unstable options -- wrap_comments above all -- which stable silently
# ignores, so `cargo fmt` on stable reports clean against weaker rules than CI
# applies and the difference only surfaces on push. Keep this in lockstep with
# the toolchain in that workflow.
[doc("Format Rust and JavaScript (rewrites the working tree).")]
fmt: ensure-deps
    cargo +{{ fmt_toolchain }} fmt --all
    bun run format

# Verify formatting without writing.
fmt-check: ensure-deps
    cargo +{{ fmt_toolchain }} fmt --all -- --check
    bun run format:check

# Build the native module, debug profile. The everyday one.
build: ensure-deps
    bun run build -- dev

# Build the native module, release profile. What CI ships.
build-release: ensure-deps
    rm -f {{ lib }}
    bun run build

# Build the native module with a hand-picked cargo feature set.
build-custom: ensure-deps
    bun run build -- custom

# Without the override a platform package from node_modules wins over lib/skia.node,
# so `bun run build && bun run test` silently exercises the published binary instead of
# the one just compiled.
[doc("Run the test suite against the local build.")]
test: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --test

# The Rust suite. `test` is the JavaScript one; `ci` runs both.
#
# Carries a feature set for the same reason `check-api` does. A bare
# `cargo test` builds with default features, where `gui` does not exist -- so
# every test under it was skipped rather than run, including the ones pinning
# the event JSON the JavaScript side parses. Eighteen of them, reporting
# nothing.
#
# Its absence here was not deliberate: `just ci` checked formatting, types,
# clippy and the JavaScript tests, and never ran `cargo test` at all. The Rust
# suite is the larger of the two.
[doc("The Rust suite. `test` is the JavaScript one; `ci` runs both.")]
test-rust:
    cargo test --features "{{ if os() == "macos" { "metal,window,freetype" } else { linux_features } }}"

# Run the test suite in watch mode.
test-watch: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --test --watch

# Run the visual render tests in watch mode.
test-visual: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --watch-path lib --watch-path tests/visual tests/visual

# The illustrations on the API pages, as opposed to the example images
# below. These were inherited with no way to reproduce them -- nothing could
# check that they still matched the library, so a change to `trim` or
# `simplify` would have left the page showing the old behaviour forever.
#
# Release, for the reason given on `examples` below.
[doc("Regenerate the API illustrations and the brand banners.")]
docs-assets: build-release
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node docs/generate/path2d.js
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node docs/generate/context.js
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node docs/generate/brand.js

# Redraw the images the README embeds. Run after anything that could alter
# output, so the pictures keep describing what the library actually does.
#
# Release rather than the everyday dev binary, which `ensure-binary` builds.
# Optimization does not change what Skia draws, so either profile produces
# the same pictures -- what it changes is how long they take, and this recipe
# runs the slowest thing the library does. A dev build encodes the AVIF at
# 2810 milliseconds against 239, which is most of the wait for a set of
# images that are then committed and looked at rather than measured.
#
# It also stops the recipe replacing a release binary with a debug one behind
# whoever built it, which `ensure-binary` does silently.
[doc("Regenerate the showcase images in docs/assets/gallery.")]
examples: build-release
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node examples/node/report-card.js docs/assets/gallery
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node examples/node/feature-sheet.js docs/assets/gallery
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node examples/node/animated-eye.js docs/assets/gallery
    cd docs/assets/gallery && \
      for f in report typography images effects; do mv -f "$f.png" "$f@2x.png"; done && \
      rm -f report.jpg report.webp report.pdf report.svg book.pdf

# The licence audit THIRD-PARTY-NOTICES.md states, over the packages that
# actually link rather than everything Cargo knows about. A bare
# `cargo metadata` counts build and dev dependencies and every platform's
# targets, which is a larger number -- and, resolving only the default
# features, it also *omits* the crates reached through `node-addon`, `metal`
# and `window`, so it is not a superset either. Both halves ask for the same
# feature set, and they match on name and version rather than name alone,
# because six crates appear in the graph at two versions.
#
# Prints the per-licence tally and then anything that is not plainly
# permissive, which should stay empty. An `OR`-licensed crate is taken under
# whichever arm suits, so only a bare copyleft term is a finding.
[doc("Audit the licences of every crate that links into a release binary.")]
licenses:
    #!/usr/bin/env bash
    set -euo pipefail
    features="node-addon,{{ if os() == "macos" { "metal,window" } else { linux_features } }}"
    cargo tree --locked --prefix none -e normal --no-default-features --features "$features" \
      | awk '$2 ~ /^v/ {print $1 " " substr($2,2)}' | sort -u > /tmp/meo-links.txt
    cargo metadata --locked --format-version 1 --all-features | python3 -c "
    import json, re, sys, collections
    ships = {tuple(l.split()) for l in open('/tmp/meo-links.txt') if l.strip()}
    meta = json.load(sys.stdin)['packages']
    pkgs = [p for p in meta if (p['name'], p['version']) in ships]
    missing = ships - {(p['name'], p['version']) for p in pkgs}
    counts = collections.Counter(str(p.get('license')) for p in pkgs)
    print(f'{len(pkgs)} packages link into a release binary')
    for licence, n in counts.most_common():
        print(f'{n:4d}  {licence}')
    copyleft = [(p['name'], p.get('license')) for p in pkgs
                if any(k in str(p.get('license')).upper()
                       for k in ('GPL', 'MPL', 'CDDL', 'EUPL', 'NONE'))
                and ' OR ' not in str(p.get('license'))]
    print()
    print('copyleft or unlicensed:', copyleft or 'none')
    if missing:
        print('NOT FOUND IN METADATA:', sorted(missing))
    # The count is only useful if the prose quoting it is the same number, and
    # it has been wrong before: the file said 135 long after the graph moved,
    # then carried 167 and 189 in one document. Both figures are read back out
    # of the sentences that state them and compared with what was just counted,
    # so a graph change fails here rather than in a later reader's head.
    notices = open('THIRD-PARTY-NOTICES.md').read()
    stale = []
    for pattern, actual, what in (
        (r'over the \*\*(\d+)\*\* crate versions', len(pkgs), 'crates that link'),
        (r'which is \*\*(\d+)\*\* against', len(meta), 'packages in cargo metadata'),
    ):
        found = re.search(pattern, notices)
        if not found:
            stale.append(f'THIRD-PARTY-NOTICES.md no longer states the {what} where this looked')
        elif int(found.group(1)) != actual:
            stale.append(f'THIRD-PARTY-NOTICES.md says {found.group(1)} {what}, counted {actual}')
    for line in stale:
        print(line)
    # Non-zero on any of the three, because this runs in 'ci' now and a check
    # that only prints is a check nobody reads.
    if copyleft or missing or stale:
        sys.exit(1)
    "

# Declared dependencies nothing imports. `cargo machete` greps the sources for
# each crate's name rather than asking the compiler, so it can be fooled by a
# crate reached only through a macro -- check what it reports before deleting.
# It found two that were genuinely dead: `crossbeam`, which left the build
# entirely (rayon pulls its own `crossbeam-*` internals), and `once_cell`,
# which stayed in the tree via `dashmap` and `neon` but was not ours to
# declare.
#
# Not in `ci`: it needs `cargo install cargo-machete`, and a checklist that
# fails on a missing tool is one people learn to skip.
[doc("Report dependencies declared in Cargo.toml that nothing imports.")]
unused:
    cargo machete

# What the `docs (rustdoc)` job builds, and what docs.rs will render, so the
# feature set has to stay in step with `[package.metadata.docs.rs]` in
# Cargo.toml -- `vulkan` there, `metal` here on a Mac, because the other one
# does not compile.
#
# This is not the only rustdoc in `ci`: `check-api` runs a second one, on a
# newer pinned nightly, and that one is a gate too -- see the note above it.
[doc("Reference docs for both halves: cargo doc and TypeDoc.")]
docs: docs-rust docs-js

[doc("Fail on a rustdoc warning -- broken intra-doc links above all.")]
docs-rust:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --no-default-features \
      --features "{{ if os() == "macos" { "metal,window" } else { "vulkan,window" } }}"

# The declarations are the published contract, so they get the same treatment
# the crate's `#![warn(missing_docs)]` gives its Rust half. A broken link or a
# type that escapes a signature unexported fails the build; the count of
# undocumented members ratchets downward and may not climb.
#
# Its own `node_modules` because the root is on TypeScript 7, whose main entry
# point exports no compiler API at all -- see scripts/typedoc/README.md.
# `ensure-deps` as well as its own install, because the declarations name
# `Buffer` and import from `stream`, and those types come from `@types/node` in
# the *root* `node_modules`. This recipe worked anyway for as long as it has
# existed, on any machine that had run the root install once -- and failed on a
# fresh clone and in CI, on eighteen errors, the first time it ran anywhere that
# had not.
[doc("Build the JavaScript API reference from lib/*.d.ts.")]
docs-js: ensure-deps
    @test -d scripts/typedoc/node_modules || bun install --cwd scripts/typedoc --frozen-lockfile
    node scripts/typedoc/build.mjs

# `lib/index.d.ts` is a claim about the runtime, and nothing else checks that
# the claim holds. `tsc` believes the declarations; the test suite exercises
# the runtime; each half is internally consistent and neither asks whether the
# two describe the same library. Thirteen defects of that shape were shipping
# when this comparison was first run by hand.
#
# Both directions fail, because both have shipped: a member reachable and
# undeclared is invisible to a TypeScript caller, and a member declared and
# absent is a promise to them that nothing keeps.
#
# Its own `node_modules` for the reason `docs-js` has one -- the root is on
# TypeScript 7, whose entry point exports no compiler API. `ensure-deps` for
# the same reason too: the walk has to load `@types/node`'s `events.d.ts` from
# the *root* install, because `Image` and `Window` take their base from
# "stream", and without it the whole emitter surface reads as undeclared.
# `ensure-binary` because the runtime half introspects the built addon.
[doc("Check lib/index.d.ts against the runtime surface of the built addon.")]
check-api-surface: ensure-deps ensure-binary
    @test -d scripts/api-surface/node_modules || bun install --cwd scripts/api-surface --frozen-lockfile
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node scripts/api-surface/check.mjs "{{ justfile_directory() }}"

# Uses the same pinned nightly as the fmt job: rustdoc's JSON output is
# unstable, and it is the only form that records which crate a type in a
# signature came from. The HTML renders `skia_safe::Color` as a bare `Color`,
# so grepping it reports success on a tree that leaks.
#
# `-D warnings` because this rustdoc is newer than the one `docs-rust` uses --
# 1.100 nightly against 1.98 stable -- and lints the older one does not have
# were being printed here and read by nobody. `redundant_explicit_links` sat
# in `App::run` that way, reported on every `just ci` and fatal on none of
# them. Two rustdocs and only one gate is the same gap that let a link to a
# `pub(crate)` item reach CI.
[doc("Fail if a public signature exposes a skia_safe or neon type.")]
check-api: ensure-deps
    RUSTDOCFLAGS="-D warnings" \
      cargo +{{ fmt_toolchain }} rustdoc --no-default-features \
      --features "{{ if os() == "macos" { "metal,window" } else { linux_features } }}" \
      -- -Z unstable-options --output-format json
    node scripts/check-public-api.mjs target/doc/meo_skia_canvas.json

# Depends on build-release on purpose. A dev binary leaves the Rust glue
# unoptimized, which moves the per-call overhead without touching Skia, so the
# ratios come out right and the milliseconds do not.
[doc("Measure timing and memory against the release binary.")]
bench: build-release
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --expose-gc examples/node/benchmark.js

# Remove the compiled binary.
clean:
    rm -f {{ lib }}

# Remove the binary, node_modules, and all cargo build output.
clean-all: clean
    rm -rf node_modules
    rm -rf target/debug target/release
    cargo clean

# Print the skia-safe version from Cargo.toml.
skia-version:
    @grep -m 1 '^skia-safe' Cargo.toml | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?'

# Patch Cargo.toml to build against a local rust-skia checkout.
use-local-skia:
    echo '' >> Cargo.toml
    echo '[patch.crates-io]' >> Cargo.toml
    echo 'skia-safe = { path = "../rust-skia/skia-safe" }' >> Cargo.toml
    echo 'skia-bindings = { path = "../rust-skia/skia-bindings" }' >> Cargo.toml

# `bump` is passed to `npm version`, so anything it accepts works:
#
#   just release-npm patch
#   just release-npm preminor --preid rc
#
# Variadic, because just splits the command line on whitespace and a
# single-parameter recipe takes only the first word -- `--preid` was then read
# as another recipe name, and the release failed before it started.
#
# Use a prerelease to exercise the full pipeline — binaries, the glibc floor
# assertion, the Lambda layer check, and `just publish-npm` itself — without
# taking the `latest` tag. `publish.yml` sends anything with a `-` in the
# version to the `next` dist-tag, so a plain `npm install` is unaffected.
#
# The cargo crate `meo-skia-canvas` (in Cargo.toml) versions independently from
# the npm package `meo-skia-canvas` (in package.json). This recipe only
# touches the npm channel; the crate goes out via `just release-crate`.
[doc("npm step 1: bump, commit, tag, push, open a draft release.")]
release-npm *bump="patch":
    #!/usr/bin/env bash
    set -euo pipefail

    # Passed to every `gh` call, as in `publish`. Without it gh needs a default repository, and a
    # clone with more than one remote does not have one — which failed the v4.1.0 release *after*
    # the tag had already been pushed, leaving the tag up and no release under it.
    REPO=l7aromeo/meo-skia-canvas

    if [[ -n "$(git status --porcelain)" ]]; then
        echo "Error: working tree is not clean"
        exit 1
    fi

    # One expression decides and reports, because two disagreed: `git cherry`
    # needs an upstream, and without one it exits 129 with the usage error on
    # stderr -- which `2>/dev/null` discards, leaving empty output that reads as
    # "nothing unpushed". A fresh clone whose `main` was created without tracking
    # is in exactly that state, and can still push. This form asks the question
    # against the remotes directly and needs no tracking branch.
    UNPUSHED=$(git --no-pager log --oneline main --not --remotes="*/main")
    if [[ -n "$UNPUSHED" ]]; then
        echo "Error: unpushed commits"
        echo "$UNPUSHED"
        exit 1
    fi

    # bump package.json + bun.lock (npm channel only)
    #
    # Every exit between here and the commit has to put these back, including
    # the ones no branch covers: a Ctrl-C at the prompt, or a `read` that sees
    # EOF and trips `set -e`. Both used to leave a bumped version sitting on a
    # clean-looking tree, which `ci.yml` then chases binaries for at a version
    # no release ever built. The trap is released once the commit lands.
    trap 'git checkout HEAD -- package.json bun.lock 2>/dev/null || true' EXIT INT TERM
    npm version {{ bump }} --no-git-tag-version
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"

    # The changelog is written by hand, before the bump, and nothing used to check it. Write the
    # entry first: the release notes come from it, and reconstructing what changed after tagging
    # means reading commits instead of remembering intent. Prereleases are exempt — they exist to
    # exercise the pipeline, not to be read.
    if [[ "$VERSION" != *-* ]] && ! grep -q "\[${TAG}\]" CHANGELOG.md; then
        echo "Error: CHANGELOG.md has no entry for ${TAG}"
        echo "       add one above the previous release, then re-run"
        exit 1
    fi

    # Drop the platform pins for the duration of the release. They point at the *previous*
    # version from here until `just publish-npm` runs sync-targets, and while they do:
    #
    #   - a frozen install cannot resolve them once the main package is published at the new version
    #   - tests/suite/binary.test.js asserts the pins match package.json, so every `bun run test`
    #     in build.yml fails, on every platform, before a single binary is uploaded
    #
    # The pins cannot be corrected earlier either: the packages they name do not exist until
    # the binaries are built, which is what this release is for. Absent is the only coherent
    # state in between, and the test skips itself when they are.
    npm pkg delete optionalDependencies
    bun install --lockfile-only >/dev/null

    if gh release view "${TAG}" -R "${REPO}" --json id &>/dev/null; then
        echo "Error: release ${TAG} already exists"
        exit 1
    fi

    # Any semver prerelease, not just `-rc`: `npm version preminor` with no `--preid`
    # produces `4.1.0-0`, which is every bit as much a prerelease and was previously
    # published as a normal release.
    PRERELEASE=""
    [[ "$VERSION" == *-* ]] && PRERELEASE="--prerelease"

    echo ""
    echo "  version: ${VERSION} (npm only; cargo crate version untouched)"
    echo "  tag:     ${TAG}"
    echo ""
    read -rp "Create release ${TAG}? [y/N] " confirm
    if [[ "$confirm" != "y" ]]; then
        echo "Aborted."
        exit 1
    fi

    git add package.json bun.lock
    git commit -m "${VERSION}"
    # Committed: the bump is now the intended state, so stop undoing it.
    trap - EXIT INT TERM
    git tag -a "${TAG}" -m "${TAG}"
    # This tag only, never `--tags`: the clone inherited ~90 tags from upstream,
    # including a `v3.6.0` pointing at a different commit than ours.
    git push origin main
    git push origin "${TAG}"
    gh release create "${TAG}" -R "${REPO}" ${PRERELEASE} --draft --generate-notes

    # build.yml is dispatch-only. No push, tag or release event starts it, so creating the
    # release is not enough on its own and this step used to be left to whoever remembered
    # it. Dispatched against the tag, not main, so the binaries come from exactly what was
    # released rather than from whatever landed on main while the build was queued.
    gh workflow run build.yml -R "${REPO}" --ref "${TAG}"
    sleep 10
    RUN=$(gh run list -R "${REPO}" --workflow=build.yml --limit 1 --json databaseId --jq '.[0].databaseId')

    echo ""
    echo "Draft release ${TAG} created; binaries building in run ${RUN}."
    echo "  https://github.com/${REPO}/actions/runs/${RUN}"
    echo ""
    echo "Watch it:      gh run watch ${RUN} -R ${REPO}"
    echo "When it ends:  just publish-npm"

# Publish the whole eight-package set, in the only order that works.
#
# The main package pins each platform package by exact version, so publishing it
# first would point at versions that do not exist. And the platform packages are
# built from the release assets, so those have to be reachable before either. The
# order is therefore forced:
#
#   undraft            assets are not downloadable while the release is a draft,
#                      which also keeps CI's rendering suite from running
#   snapshot           sha256 of every asset into package.json, committed, so the
#                      published package can verify what it downloads
#   platform packages  all 7, and they must all land before the next step
#   sync-targets       optionalDependencies pinned to this version, committed
#   main package       last, so its pins resolve
#
# Every stage is skipped when already done, so a failed run can be re-run rather
# than unpicked. All `gh` calls pass `-R` explicitly so the recipe does not depend
# on which remote gh treats as default.
#
# Rehearse with `just publish-npm dry` first. It runs every check that comes before
# anything is published -- clean tree, release exists, all binaries attached, which
# platform packages are already up -- and prints which stages would act, without
# publishing or committing anything. Worth doing: a release is the one path that
# cannot be undone by re-running it.
#
# What it cannot reach is the verification each stage does after acting, because
# there is nothing to verify until something has been done: `snapshot`'s entry
# count, and the `MISSING_FROM_LOCK` check over the lockfile stage 4 writes -- the
# backstop that exists because 4.1.0 and 4.2.0-rc.2 both shipped a short one. So a
# clean dry run says the release is startable, not that it will succeed.
[doc("npm step 2: publish all 8 packages, in the only order that works.")]
publish-npm dry="false":
    #!/usr/bin/env bash
    set -euo pipefail

    REPO=l7aromeo/meo-skia-canvas
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    DRY="{{ dry }}"

    # The release notes are the changelog entry, extracted once here so the dry run
    # fails on a missing section rather than the real run discovering it mid-publish.
    # Prereleases keep the notes `just release-npm` generated: it does not require a
    # changelog entry for them, so there may be none to find.
    NOTES=$(mktemp)
    NPM_ERR=$(mktemp)
    trap 'rm -f "$NOTES" "$NPM_ERR"' EXIT

    if [[ "$VERSION" != *-* ]]; then
        node -e '
            const fs = require("fs");
            const version = process.argv[1];
            const lines = fs.readFileSync("CHANGELOG.md", "utf8").split("\n");
            const start = lines.findIndex(
                (l) => l.startsWith("## ") && l.includes(`[v${version}]`),
            );
            if (start === -1) {
                console.error(`no CHANGELOG entry for v${version}`);
                process.exit(1);
            }
            let end = lines.findIndex((l, i) => i > start && l.startsWith("## "));
            if (end === -1) end = lines.length;
            process.stdout.write(lines.slice(start + 1, end).join("\n").trim() + "\n");
        ' "$VERSION" > "$NOTES"
    fi

    if [[ "$DRY" != "false" ]]; then
        echo "DRY RUN — every check below runs for real; nothing is published or committed."
        echo ""
    fi

    # The guard is about *unrelated* work, not about cleanliness. `package.json` and
    # `bun.lock` are the two files this recipe writes and commits itself, at stages 2
    # and 4, so a tree dirty only in those is not somebody's half-finished edit -- it
    # is this recipe's own output from a run that died before its commit. Refusing it
    # is what makes stage 4 unable to resume: `sync-targets` and `bun install` write
    # both files, `MISSING_FROM_LOCK` runs between the write and the commit, and an
    # interruption anywhere in there leaves a tree that every re-run then bounces off.
    # npm's validation queue held a platform package on 5.8.0 and put the release in
    # exactly that state; clearing it took a manual stash.
    #
    # A dry run writes nothing, so it reports either kind and stops for neither --
    # rehearsing before you commit is a reasonable thing to want.
    #
    # The three codes are the ones this recipe's own writes produce and no others: an
    # unstaged write is ` M`, a write a dying run had already staged is `M `, and both
    # at once is `MM`. Matching any two characters instead would take `UU`, `AA`, `DD`,
    # `D ` and `??` for this recipe's output -- so a `package.json` left in conflict by
    # a merge would read as a resumable state, and stage 4's `git add package.json`
    # would commit the conflict markers under a release message.
    #
    # Consequence worth knowing when resuming: stage 2 decides whether to commit by
    # asking whether `package.json` is dirty, and cannot tell a pending `sync-targets`
    # write from a fresh snapshot. Resuming with stage 4's output already on disk
    # therefore commits it under stage 2's message. The content is right and the final
    # state is right; the message names the wrong stage.
    DIRTY=$(git status --porcelain)
    if [[ -n "$DIRTY" ]]; then
        UNRELATED=$(printf '%s\n' "$DIRTY" | grep -vE '^( M|M |MM) (package\.json|bun\.lock)$' || true)
        if [[ -n "$UNRELATED" ]]; then
            if [[ "$DRY" == "false" ]]; then
                echo "Error: working tree is not clean; this recipe commits as it goes"
                printf '%s\n' "$UNRELATED"
                exit 1
            fi
            echo "  ! working tree is not clean -- a real run would stop here"
        else
            echo "  ! resuming over this recipe's own uncommitted files:"
            printf '%s\n' "$DIRTY" | sed 's/^/      /'
        fi
    fi

    # Draft releases aren't reachable by tag; list all and find by name.
    RELEASE_ID=$(gh api "repos/${REPO}/releases" --paginate --jq ".[] | select(.name==\"${TAG}\") | .id")
    if [[ -z "$RELEASE_ID" ]]; then
        echo "Error: release ${TAG} not found on ${REPO}"
        exit 1
    fi

    # Refuse to start on a half-built release: publishing a partial set is worse
    # than publishing none, and the snapshot below would record whatever is there.
    EXPECTED=$(node -p "Object.keys(require('./lib/targets.json')).length")
    HAVE=$(gh api "repos/${REPO}/releases/${RELEASE_ID}" --jq '[.assets[].name | select(endswith(".gz"))] | length')
    if [[ "$HAVE" -ne "$EXPECTED" ]]; then
        echo "Error: release ${TAG} has ${HAVE} of ${EXPECTED} binaries; wait for the build"
        exit 1
    fi

    DRAFT=$(gh api "repos/${REPO}/releases/${RELEASE_ID}" --jq '.draft')

    # Every target, not one of them standing for the rest. This probed only
    # darwin-arm64 until a v5.0.0 publish left win32-x64 unpublished when its
    # matrix job wedged in the runner queue: the sentinel was up, so a re-run
    # would have skipped this step entirely and then pinned a win32-x64@5.0.0
    # that did not exist -- into a version npm does not let you replace.
    TARGETS=$(node -p "Object.keys(require('./lib/targets.json')).join(' ')")
    PLATFORM_MISSING=""
    for t in $TARGETS; do
        npm view "meo-skia-canvas-${t}@${VERSION}" version &>/dev/null \
            || PLATFORM_MISSING="${PLATFORM_MISSING}${t} "
    done
    PLATFORM_MISSING="${PLATFORM_MISSING% }"
    PLATFORM_HAVE=$(( EXPECTED - $(echo $PLATFORM_MISSING | wc -w) ))

    # Asked of the package, not of the version, because a versioned query cannot
    # answer this. `npm view meo-skia-canvas@5.8.0` returns `E404` when the version
    # is absent *and* when the registry is reachable but not the one this release
    # belongs to -- a misconfigured `.npmrc`, a corporate proxy, an internal
    # Verdaccio that does not proxy this package. Measured against
    # `--registry https://example.com`: E404 for the versioned query, and the real
    # registry answers it. Reading that as "not published yet" dispatches
    # `publish.yml` at a version already on npm, which is the hard 403 the stage 5
    # guard exists to avoid.
    #
    # A 404 for the *package* is not an ordinary answer the way a 404 for a *version*
    # is: this package exists, so whatever returned it is not the registry to publish
    # against. One call therefore decides all three outcomes -- the version list, a
    # wrong registry, and no answer at all -- where the versioned form collapses the
    # last two into the first.
    if MAIN_VERSIONS=$(npm view "meo-skia-canvas" versions --json 2>"$NPM_ERR"); then
        # A package with one published version answers with a bare string rather
        # than a list, so the membership test cannot assume an array.
        MAIN_DONE=$(node -e '
            const all = JSON.parse(process.argv[1]);
            const want = process.argv[2];
            const list = Array.isArray(all) ? all : [all];
            process.stdout.write(list.includes(want) ? want : "");
        ' "$MAIN_VERSIONS" "$VERSION")
    else
        echo "Error: could not ask npm which versions of meo-skia-canvas exist,"
        echo "       so whether ${VERSION} is already published is unknown."
        sed 's/^/       /' "$NPM_ERR"
        exit 1
    fi

    echo ""
    echo "  version:   ${VERSION}"
    echo "  release:   ${TAG} (${HAVE}/${EXPECTED} binaries, draft=${DRAFT})"
    echo ""
    echo "  would set notes:        $([[ -s "$NOTES" ]] && echo "yes, $(wc -l < "$NOTES" | tr -d ' ') lines from CHANGELOG.md" || echo "no, prerelease keeps generated notes")"
    echo "  would undraft:          $([[ "$DRAFT" == "true" ]] && echo yes || echo "no, already published")"
    echo "  would snapshot hashes:  yes"
    echo "  would publish platform: $(
        if [[ "$PLATFORM_HAVE" -eq 0 ]]; then echo "yes, ${EXPECTED} packages"
        elif [[ -z "$PLATFORM_MISSING" ]]; then echo "no, all ${EXPECTED} already at ${VERSION}"
        else echo "STOP — ${PLATFORM_HAVE}/${EXPECTED} published, missing: ${PLATFORM_MISSING}"
        fi)"
    echo "  would publish main:     $([[ -z "$MAIN_DONE" ]] && echo yes || echo "no, already at ${VERSION}")"
    echo ""

    if [[ "$DRY" != "false" ]]; then
        echo "Dry run complete. Nothing was changed, and the release is startable:"
        echo "every check that runs before anything is published passed. What each"
        echo "stage verifies after acting -- the snapshot's entry count, and the"
        echo "lockfile pins -- is only reachable by publishing."
        exit 0
    fi

    echo "This publishes ${VERSION} to npm as $((EXPECTED + 1)) packages. npm does not let you"
    echo "reuse a version number afterwards."
    echo ""
    read -rp "Publish ${TAG}? [y/N] " confirm
    [[ "$confirm" == "y" ]] || { echo "Aborted."; exit 1; }

    # 1. Undraft, so the assets become downloadable, and set the notes from the changelog.
    #
    #    `just release-npm` creates the release with --generate-notes, which produces a bare
    #    compare link. The entry written before the tag is the actual release note, and
    #    copying it across by hand was a manual step on the one path that cannot be undone
    #    -- it was missed on 4.1.0 and again on 4.1.1. Prereleases keep the generated notes;
    #    `release` does not require a changelog entry for them, so there may be none.
    if [[ -s "$NOTES" ]]; then
        gh release edit "${TAG}" -R "${REPO}" --notes-file "$NOTES" >/dev/null
        echo "==> release notes set from CHANGELOG.md ($(wc -l < "$NOTES" | tr -d ' ') lines)"
    fi

    if [[ "$(gh api "repos/${REPO}/releases/${RELEASE_ID}" --jq '.draft')" == "true" ]]; then
        gh api -X PATCH "repos/${REPO}/releases/${RELEASE_ID}" -F draft=false --silent
        echo "==> release ${TAG} undrafted"
    else
        echo "==> release ${TAG} already published"
    fi

    # 2. Record the asset hashes. Must be committed before anything is published:
    #    the tarball on npm is what verifies the binary a user downloads.
    bun run snapshot
    if [[ -n "$(git status --porcelain package.json)" ]]; then
        git add package.json
        git commit -m "release: snapshot the ${TAG} binary hashes"
        git push origin main
        echo "==> hashes snapshotted"
    else
        echo "==> hashes already current"
    fi

    # 3. The 7 platform packages. Waited on, not fired and forgotten — step 4 pins
    #    exact versions and would pin ones that do not exist yet.
    #
    #    A partial set stops the release rather than resuming it. Re-dispatching
    #    the workflow cannot fix one missing target: `npm publish` over a version
    #    that already exists is a hard 403, and the matrix is `fail-fast: true`,
    #    so the run dies on whichever already-published target it reaches first.
    #    Re-running the one wedged job is the fix, and that is a decision for the
    #    operator to take with the run in front of them.
    if [[ -z "$PLATFORM_MISSING" ]]; then
        echo "==> all ${EXPECTED} platform packages already at ${VERSION}"
    elif [[ "$PLATFORM_HAVE" -gt 0 ]]; then
        echo "Error: ${PLATFORM_HAVE} of ${EXPECTED} platform packages are at ${VERSION}."
        echo "       Missing: ${PLATFORM_MISSING}"
        echo ""
        echo "       Do not re-dispatch the workflow — it would republish the ${PLATFORM_HAVE}"
        echo "       that already exist and fail 403 on the first. Re-run the individual job:"
        echo ""
        echo "         gh run list -R ${REPO} --workflow=publish-platform-packages.yml --limit 1"
        echo "         gh run rerun <run-id> -R ${REPO} --job <job-id>"
        echo ""
        echo "       Then re-run this recipe; it resumes from here."
        exit 1
    else
        gh workflow run publish-platform-packages.yml -R "${REPO}" --ref main
        sleep 10
        RUN=$(gh run list -R "${REPO}" --workflow=publish-platform-packages.yml --limit 1 --json databaseId --jq '.[0].databaseId')
        echo "==> publishing platform packages (run ${RUN})"
        gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 20 >/dev/null
    fi

    # 4. Point the main package at them.
    #
    # `--lockfile-only` is load-bearing. This step exists to write a lockfile, not to populate
    # node_modules, and the distinction is what broke the 4.1.0 release: a full install fetches the
    # one platform package matching this host for real. Six of the seven are for other platforms
    # and are only ever recorded from metadata, but the seventh gets downloaded — and seconds after
    # publishing, metadata has propagated while the tarball has not. The installer then drops it
    # silently, an optional dependency that fails to install not being an error by definition, and
    # writes a lockfile six entries deep that commits clean and fails every install afterwards.
    #
    # Resolving lock-only never requests a tarball, so there is no window to lose.
    #
    # `--no-cache` is the other half of the same problem, reached through the cache rather than the
    # network. The step above runs `npm view meo-skia-canvas-<target>@$VERSION` on every target to
    # decide whether the platform packages are already up — before they are published, so it caches
    # manifests that do not list this version. Resolving against those stale copies finds no
    # matching version for an optional dependency and drops it, silently, for the same reason as
    # above. This is what left 4.2.0-rc.2 with six of seven entries, back when only the
    # host-platform name was probed; now that all seven are, all seven can be cached stale, so the
    # flag matters more rather than less. It ignores the manifest cache instead of trusting it.
    #
    # `--no-cache` keeps a short lockfile from being written. It cannot repair one that already
    # is, and that is the state a resumed release arrives in: the first attempt wrote four entries
    # of seven, and every retry afterwards reported "no changes" and left it at four. Bun does not
    # re-resolve a lockfile it considers satisfied, and an optional dependency that is absent from
    # it is not a dissatisfaction -- so neither `--force` nor `--no-cache` nor both together add
    # the missing three. Measured against a restored four-entry lockfile: plain, `--no-cache`,
    # and `--force --no-cache` all leave it at four; not loading it reaches seven.
    #
    # `BUN_CONFIG_SKIP_LOAD_LOCKFILE` therefore makes this step idempotent rather than
    # write-once. Resolution starts from `package.json`, which `sync-targets` has just written, so
    # the result is the same file a clean release produces -- verified byte-identical to the
    # committed 5.9.0 lockfile -- and a resumed release repairs itself instead of needing the file
    # deleted by hand. The lockfile is still saved; only loading it is skipped.
    #
    # node_modules is left stale here by design; nothing downstream in this recipe reads it.
    bun run sync-targets
    BUN_CONFIG_SKIP_LOAD_LOCKFILE=1 bun install --lockfile-only --no-cache

    # Verify rather than trust. The installer exits 0 either way, so without this a short lockfile
    # looks like success. This is the backstop for what the two flags above do not cover: a target
    # published under the wrong version, or missing from the registry entirely, still lands here.
    #
    # Matched by exact `name@version` rather than counted, which the entry count could not do: seven
    # entries at the wrong version satisfy a count and fail every install afterwards.
    MISSING_FROM_LOCK=""
    for t in $TARGETS; do
        grep -q "\"meo-skia-canvas-${t}@${VERSION}\"" bun.lock \
            || MISSING_FROM_LOCK="${MISSING_FROM_LOCK}${t} "
    done
    if [[ -n "$MISSING_FROM_LOCK" ]]; then
        echo "Error: bun.lock does not pin ${MISSING_FROM_LOCK% } at ${VERSION}"
        echo "       Check they published: npm view meo-skia-canvas-<target>@${VERSION} version"
        exit 1
    fi

    # A release commit here can carry the wrong message, and this is where someone
    # debugging one is reading. Stage 2 decides whether to commit by asking whether
    # `package.json` is dirty, and cannot tell a pending `sync-targets` write from a
    # fresh snapshot -- so a run resumed with this stage's output already on disk has
    # it committed under stage 2's message instead. Content and final state are right;
    # only the message names the wrong stage. See the entry guard for why resuming over
    # these two files is allowed at all.
    if [[ -n "$(git status --porcelain)" ]]; then
        git add package.json bun.lock
        git commit -m "release: pin the platform packages at ${VERSION}"
        git push origin main
        echo "==> optionalDependencies pinned"
    else
        echo "==> optionalDependencies already pinned"
    fi

    # 5. Main package last.
    #
    # Guarded on the same `MAIN_DONE` the dry run prints, rather than on a fresh
    # `npm view`, so the rehearsal and the run cannot reach different answers --
    # which is the whole of what was wrong here. Nothing between that query and
    # this line publishes the main package, so there is no staleness to prefer a
    # re-query for; stage 3 leans on `PLATFORM_MISSING` the same way.
    #
    # Skipping matters because `npm publish` over an existing version is a hard
    # 403, so an unguarded dispatch turns a re-run of a finished release into a
    # red workflow run and a recipe that exits 1 -- on a recipe whose preamble
    # promises that every stage is skipped when already done.
    if [[ -n "$MAIN_DONE" ]]; then
        echo "==> meo-skia-canvas already at ${VERSION}"
    else
        gh workflow run publish.yml -R "${REPO}" --ref main
        sleep 10
        RUN=$(gh run list -R "${REPO}" --workflow=publish.yml --limit 1 --json databaseId --jq '.[0].databaseId')
        echo "==> publishing meo-skia-canvas (run ${RUN})"
        gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 20 >/dev/null
    fi

    echo ""
    echo "Published meo-skia-canvas@${VERSION} and ${EXPECTED} platform packages."
    echo "The cargo crate versions separately; see 'just release-crate'."

# Bump the cargo crate, tag it, and let CI publish to crates.io (bump: patch|minor|major).
#
# The two channels version independently: the npm package continues the upstream
# skia-canvas lineage, the crate started fresh at 0.1.0. Only the ergonomics are
# shared. This side needs no separate publish step — the crate has no prebuilt
# binaries to wait on, so `crates-io-publish.yml` triggers straight off the tag.
#
# Rehearse first with the workflow's dry_run input, which packs the crate and runs
# the native API contract test without contacting the registry:
#   gh workflow run crates-io-publish.yml -R l7aromeo/meo-skia-canvas -f dry_run=true
#
# Returns as soon as the tag is pushed, like `release-npm`. Pass anything but
# `false` as the second argument to block until the workflow finishes instead:
#   just release-crate minor wait
[doc("crate: bump, tag, push; CI publishes to crates.io.")]
release-crate bump="patch" wait="false":
    #!/usr/bin/env bash
    set -euo pipefail

    REPO=l7aromeo/meo-skia-canvas

    if [[ -n "$(git status --porcelain)" ]]; then
        echo "Error: working tree is not clean"
        exit 1
    fi

    # One expression decides and reports, because two disagreed: `git cherry`
    # needs an upstream, and without one it exits 129 with the usage error on
    # stderr -- which `2>/dev/null` discards, leaving empty output that reads as
    # "nothing unpushed". A fresh clone whose `main` was created without tracking
    # is in exactly that state, and can still push. This form asks the question
    # against the remotes directly and needs no tracking branch.
    UNPUSHED=$(git --no-pager log --oneline main --not --remotes="*/main")
    if [[ -n "$UNPUSHED" ]]; then
        echo "Error: unpushed commits"
        echo "$UNPUSHED"
        exit 1
    fi

    if ! cargo set-version --help &>/dev/null; then
        echo "Error: needs cargo-edit — cargo install cargo-edit"
        exit 1
    fi

    # Every exit between here and the commit has to put these back, including the
    # ones no branch covers: a Ctrl-C at the prompt, a `read` that sees EOF and
    # trips `set -e`, and a `git commit` the pre-commit hook refuses. An error
    # branch cannot do it -- none of them runs when the shell dies at the prompt,
    # and a bump left behind sits on a tree that looks clean to everything except
    # `git status`, at a version no release ever built.
    #
    # `HEAD --`, not `--`: the latter restores the working tree from the index, so
    # once `git add` has run it copies the bump back over itself and leaves it
    # staged. That window is small and it is reachable here in particular, because
    # `just precommit` runs inside `git commit` and a clippy or formatting failure
    # lands in exactly it.
    #
    # Both files, because `cargo set-version` writes both. `release-npm` carries
    # the same trap over `package.json` and `bun.lock`. Released once the commit
    # lands.
    trap 'git checkout HEAD -- Cargo.toml Cargo.lock 2>/dev/null || true' EXIT INT TERM
    cargo set-version --bump {{ bump }}
    VERSION=$(cargo metadata --no-deps --format-version 1 | node -e "
        let s=''; process.stdin.on('data', d => s += d)
          .on('end', () => console.log(JSON.parse(s).packages[0].version))")
    TAG="rust-v${VERSION}"

    if git rev-parse "${TAG}" &>/dev/null; then
        echo "Error: tag ${TAG} already exists"
        exit 1
    fi

    # Same guard as `release`, but matching the crate version rather than the tag: entries are
    # headed `[v4.1.1] (npm) / [v0.3.1] (crate)`, so the changelog never contains the `rust-v`
    # prefix. Prereleases are exempt, as there.
    if [[ "$VERSION" != *-* ]] && ! grep -q "\[v${VERSION}\]" CHANGELOG.md; then
        echo "Error: CHANGELOG.md has no entry for v${VERSION} (crate)"
        echo "       add one above the previous release, then re-run"
        exit 1
    fi

    # Keep Cargo.lock's own entry in step, and commit it with Cargo.toml. `cargo
    # set-version` writes both, and `cargo package` rewrites the lock to match the
    # manifest and then refuses to proceed on a dirty tree -- so a tag carrying a
    # bumped Cargo.toml beside a stale Cargo.lock fails packaging in CI, before
    # anything is published. It is tracked: `.gitignore` covers it under `/*.*` and
    # then re-admits it with `!/Cargo.lock`, so both `git add` and `git checkout`
    # accept the pathspec.
    cargo update -p meo-skia-canvas

    echo ""
    echo "  crate version: ${VERSION} (cargo only; npm package untouched)"
    echo "  tag:           ${TAG}"
    echo ""
    echo "Pushing ${TAG} publishes to crates.io. The version cannot be reused."
    echo ""
    read -rp "Release ${TAG}? [y/N] " confirm
    if [[ "$confirm" != "y" ]]; then
        echo "Aborted."
        exit 1
    fi

    git add Cargo.toml Cargo.lock
    git commit -m "rust: ${VERSION}"
    # Committed: the bump is now the intended state, so stop undoing it.
    trap - EXIT INT TERM
    git tag -a "${TAG}" -m "${TAG}"
    # This tag only, never `--tags`; see the note in `release`.
    git push origin main
    git push origin "${TAG}"

    sleep 10
    RUN=$(gh run list -R "${REPO}" --workflow=crates-io-publish.yml --limit 1 --json databaseId --jq '.[0].databaseId')

    # The tag push is what starts the publish, so watching it is reporting, not
    # driving: closing the terminal here changes nothing about whether the crate
    # goes out. This used to block on `gh run watch` while `release-npm` returned
    # straight away, which made two halves of the same release behave differently
    # for no reason a caller could see.
    if [[ "{{ wait }}" == "false" ]]; then
        echo ""
        echo "Tag ${TAG} pushed; crates.io publish running in ${RUN}."
        echo "  https://github.com/${REPO}/actions/runs/${RUN}"
        echo ""
        echo "Watch it:  gh run watch ${RUN} -R ${REPO}"
        echo "Confirm:   cargo search meo-skia-canvas"
        echo ""
        echo "It can still fail — cargo publish rebuilds the crate to verify it."
        exit 0
    fi

    echo "==> publishing ${TAG} (run ${RUN})"
    gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 30 >/dev/null

    echo ""
    echo "Published meo-skia-canvas ${VERSION} to crates.io."
