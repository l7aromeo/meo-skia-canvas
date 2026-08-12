set shell := ["bash", "-euo", "pipefail", "-c"]

# On Linux, `metal` feature does not compile -- use feature subset.

lib := justfile_directory() / "lib" / "skia.node"
linux_features := "vulkan,window,freetype"
# Must match the fmt job in .github/workflows/rust-ci.yml.
fmt_toolchain := "nightly-2026-08-10"

# Default: show available recipes.
default:
    @just --list

# Aggregate: what CI runs. Uses non-fixing variants.
ci: fmt-check typecheck lint-check check-api test-rust test build

[private]
ensure-deps:
    @test -d node_modules || npm ci --ignore-scripts

# Always builds, never just checks the file exists. A stale `lib/skia.node`
# kept `just test` green for a day after `node-addon` stopped compiling: the
# suite was exercising a binary from before the code was deleted. Cargo is
# incremental, so an unchanged tree costs a second or two.
[private]
ensure-binary: ensure-deps
    npm run build -- dev

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
    npm run typecheck

# Run clippy with autofix (modifies working tree).
lint:
    cargo clippy --fix --allow-dirty --allow-staged --all-targets --features "{{ linux_features }}" -- -D warnings

# Run clippy without fixing (CI-safe).
lint-check:
    cargo clippy --all-targets --features "{{ linux_features }}" -- -D warnings

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
    npm run format

# Verify formatting without writing.
fmt-check: ensure-deps
    cargo +{{ fmt_toolchain }} fmt --all -- --check
    npm run format:check

# Build the native module, debug profile. The everyday one.
build: ensure-deps
    npm run build -- dev

# Build the native module, release profile. What CI ships.
build-release: ensure-deps
    rm -f {{ lib }}
    npm run build

# Build the native module with a hand-picked cargo feature set.
build-custom: ensure-deps
    npm run build -- custom

# Without the override a platform package from node_modules wins over lib/skia.node,
# so `npm run build && npm test` silently exercises the published binary instead of
# the one just compiled.
[doc("Run the test suite against the local build.")]
test: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --test

# The Rust suite. `test` is the JavaScript one; `ci` runs both.
#
# Its absence here was not deliberate: `just ci` checked formatting, types,
# clippy and the JavaScript tests, and never ran `cargo test` at all. The Rust
# suite is the larger of the two.
test-rust:
    cargo test

# Run the test suite in watch mode.
test-watch: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --test --watch

# Run the visual render tests in watch mode.
test-visual: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node --watch-path lib --watch-path tests/visual tests/visual

# Redraw the images the README embeds. Run after anything that could alter
# output, so the pictures keep describing what the library actually does.
[doc("Regenerate the example images in docs/assets/examples.")]
examples: ensure-binary
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node examples/node/report-card.js docs/assets/examples
    MEO_SKIA_CANVAS_BINARY="{{ lib }}" node examples/node/feature-sheet.js docs/assets/examples
    cd docs/assets/examples && \
      for f in report typography images effects; do mv -f "$f.png" "$f@2x.png"; done && \
      rm -f report.jpg report.webp report.pdf report.svg book.pdf

# Uses the same pinned nightly as the fmt job: rustdoc's JSON output is
# unstable, and it is the only form that records which crate a type in a
# signature came from. The HTML renders `skia_safe::Color` as a bare `Color`,
# so grepping it reports success on a tree that leaks.
[doc("Fail if a public signature exposes a skia_safe or neon type.")]
check-api: ensure-deps
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

    if [[ -n "$(git cherry -v 2>/dev/null)" ]]; then
        echo "Error: unpushed commits"
        git --no-pager log --oneline main --not --remotes="*/main"
        exit 1
    fi

    # bump package.json + package-lock.json (npm channel only)
    #
    # Every exit between here and the commit has to put these back, including
    # the ones no branch covers: a Ctrl-C at the prompt, or a `read` that sees
    # EOF and trips `set -e`. Both used to leave a bumped version sitting on a
    # clean-looking tree, which `ci.yml` then chases binaries for at a version
    # no release ever built. The trap is released once the commit lands.
    trap 'git checkout -- package.json package-lock.json 2>/dev/null || true' EXIT INT TERM
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
    #   - `npm ci` cannot resolve them once the main package is published at the new version
    #   - tests/suite/binary.test.js asserts the pins match package.json, so every `npm test`
    #     in build.yml fails, on every platform, before a single binary is uploaded
    #
    # The pins cannot be corrected earlier either: the packages they name do not exist until
    # the binaries are built, which is what this release is for. Absent is the only coherent
    # state in between, and the test skips itself when they are.
    npm pkg delete optionalDependencies
    npm install --ignore-scripts --package-lock-only >/dev/null

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

    git add package.json package-lock.json
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
# Rehearse with `just publish-npm dry` first. That runs every guard for real — clean
# tree, release exists, all binaries attached — and prints which stages would act
# and which are already done, without publishing or committing anything. Worth
# doing: a release is the one path that cannot be undone by re-running it.
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
    trap 'rm -f "$NOTES"' EXIT

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

    # A dry run writes nothing, so a dirty tree is worth reporting but not worth stopping
    # for — rehearsing before you commit is a reasonable thing to want.
    if [[ -n "$(git status --porcelain)" ]]; then
        if [[ "$DRY" == "false" ]]; then
            echo "Error: working tree is not clean; this recipe commits as it goes"
            exit 1
        fi
        echo "  ! working tree is not clean — a real run would stop here"
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

    MAIN_DONE=$(npm view "meo-skia-canvas@${VERSION}" version 2>/dev/null || true)

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
        echo "Dry run complete. Every guard passed; nothing was changed."
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
    npm run snapshot
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
    # `--package-lock-only` is load-bearing. This step exists to write a lockfile, not to populate
    # node_modules, and the distinction is what broke the 4.1.0 release: a plain `npm install`
    # fetches the one platform package matching this host for real. Six of the seven are for other
    # platforms and are only ever recorded from metadata, but the seventh gets downloaded — and
    # seconds after publishing, metadata has propagated while the tarball has not. npm then drops
    # it silently, an optional dependency that fails to install not being an error by definition,
    # and writes a lockfile six entries deep that commits clean and fails `npm ci` everywhere after.
    #
    # Resolving lock-only never requests a tarball, so there is no window to lose. Measured against
    # the published 4.1.1 set with an empty cache: seven packuments, 84K, zero tarballs, and all
    # seven entries byte-identical to the ones a full install produced. This replaces a poll loop
    # that waited on `npm view <pkg> dist.tarball` — metadata, which was never the missing half.
    #
    # `--prefer-online` is the other half of the same problem, reached through the cache rather
    # than the network. The step above runs `npm view meo-skia-canvas-<target>@$VERSION` on every
    # target to decide whether the platform packages are already up — before they are published,
    # so it caches packuments that do not list this version. Resolving against those stale copies
    # finds no matching version for an optional dependency and drops it, silently, for the same
    # reason as above. This is what left 4.2.0-rc.2 with six of seven entries, back when only the
    # host-platform name was probed; now that all seven are, all seven can be cached stale, so the
    # flag matters more rather than less. It revalidates cached metadata instead of trusting it.
    #
    # node_modules is left stale here by design; nothing downstream in this recipe reads it.
    npm run sync-targets
    npm install --ignore-scripts --package-lock-only --prefer-online

    # Verify rather than trust. npm exits 0 either way, so without this a short lockfile looks like
    # success. Kept as a backstop for what the two flags above do not cover: a target published
    # under the wrong version, or missing from the registry entirely, still lands here.
    EXPECTED=$(node -p "Object.keys(require('./lib/targets.json')).length")
    LOCKED=$(node -p "Object.keys(require('./package-lock.json').packages).filter(k => k.includes('meo-skia-canvas-')).length")
    if [[ "$LOCKED" -ne "$EXPECTED" ]]; then
        echo "Error: package-lock.json has ${LOCKED} platform packages, expected ${EXPECTED}"
        echo "       Check all ${EXPECTED} published at ${VERSION}: npm view meo-skia-canvas-<target>@${VERSION} version"
        exit 1
    fi

    if [[ -n "$(git status --porcelain)" ]]; then
        git add package.json package-lock.json
        git commit -m "release: pin the platform packages at ${VERSION}"
        git push origin main
        echo "==> optionalDependencies pinned"
    else
        echo "==> optionalDependencies already pinned"
    fi

    # 5. Main package last.
    gh workflow run publish.yml -R "${REPO}" --ref main
    sleep 10
    RUN=$(gh run list -R "${REPO}" --workflow=publish.yml --limit 1 --json databaseId --jq '.[0].databaseId')
    echo "==> publishing meo-skia-canvas (run ${RUN})"
    gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 20 >/dev/null

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
[doc("crate: bump, tag, push; CI publishes to crates.io.")]
release-crate bump="patch":
    #!/usr/bin/env bash
    set -euo pipefail

    REPO=l7aromeo/meo-skia-canvas

    if [[ -n "$(git status --porcelain)" ]]; then
        echo "Error: working tree is not clean"
        exit 1
    fi

    if [[ -n "$(git cherry -v 2>/dev/null)" ]]; then
        echo "Error: unpushed commits"
        git --no-pager log --oneline main --not --remotes="*/main"
        exit 1
    fi

    if ! cargo set-version --help &>/dev/null; then
        echo "Error: needs cargo-edit — cargo install cargo-edit"
        exit 1
    fi

    cargo set-version --bump {{ bump }}
    VERSION=$(cargo metadata --no-deps --format-version 1 | node -e "
        let s=''; process.stdin.on('data', d => s += d)
          .on('end', () => console.log(JSON.parse(s).packages[0].version))")
    TAG="rust-v${VERSION}"

    if git rev-parse "${TAG}" &>/dev/null; then
        echo "Error: tag ${TAG} already exists"
        git checkout -- Cargo.toml
        exit 1
    fi

    # Same guard as `release`, but matching the crate version rather than the tag: entries are
    # headed `[v4.1.1] (npm) / [v0.3.1] (crate)`, so the changelog never contains the `rust-v`
    # prefix. Prereleases are exempt, as there.
    if [[ "$VERSION" != *-* ]] && ! grep -q "\[v${VERSION}\]" CHANGELOG.md; then
        echo "Error: CHANGELOG.md has no entry for v${VERSION} (crate)"
        echo "       add one above the previous release, then re-run"
        git checkout -- Cargo.toml
        exit 1
    fi

    # Keep Cargo.lock's own entry in step so the next local build does not rewrite it.
    # It is untracked -- .gitignore's `/*.*` rule covers it and no exception lets it
    # through -- so it is deliberately absent from the git calls below. Naming it there
    # is what broke this recipe in both directions: `git checkout` refuses the whole
    # command on an unmatched pathspec, and `git add` refuses an ignored file.
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
        git checkout -- Cargo.toml
        exit 1
    fi

    git add Cargo.toml
    git commit -m "rust: ${VERSION}"
    git tag -a "${TAG}" -m "${TAG}"
    # This tag only, never `--tags`; see the note in `release`.
    git push origin main
    git push origin "${TAG}"

    sleep 10
    RUN=$(gh run list -R "${REPO}" --workflow=crates-io-publish.yml --limit 1 --json databaseId --jq '.[0].databaseId')
    echo "==> publishing ${TAG} (run ${RUN})"
    gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 30 >/dev/null

    echo ""
    echo "Published meo-skia-canvas ${VERSION} to crates.io."
