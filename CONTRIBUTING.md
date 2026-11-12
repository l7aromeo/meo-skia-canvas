# Contributing

## Before anything else

Much of this code is inherited from [skia-canvas](https://github.com/samizdatco/skia-canvas), by way
of [phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas).

Send it here. This tree does not merge from the projects it forked from: a fix there is
cherry-picked on its own merits and rewritten against this tree's `skia-safe`, whose version
`Cargo.toml` names, since theirs may differ. If a change is right for this tree, open it against
this tree.

If your change would also help samizdatco's users, sending it to them as well is welcome. It is not
a condition of it landing here.

## Getting set up

```bash
git clone --recurse-submodules https://github.com/l7aromeo/meo-skia-canvas
cd meo-skia-canvas
bun install --frozen-lockfile
node lib/prebuild.mjs download   # or `just build-release` to build from source
just test
```

Two things bite people on a fresh clone:

**A bare `node --test` does not run against the binary you built.** An installed platform package
outranks `lib/skia.node`, so `node --test` after `just build-release` loads the _published_ binary
and your change looks like it did nothing -- on one tree that read as 112 pass / 69 fail where
`just test` reported 181 / 0. `just test` and `npm test` both point
`MEO_SKIA_CANVAS_BINARY` at the local build, and `npm test` prints which binary the run is against;
set the variable yourself if you are invoking Node directly.

**Bun is the package manager, Node is the runtime.** `bun install` is what fills `node_modules`
and `bun.lock` is the JavaScript lockfile; there is no `package-lock.json`. `Cargo.lock` is tracked
too, and is the other half of the same idea -- the dependency counts in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) describe that resolution, and `just licenses` fails
when they disagree. The tests and everything this
package ships still run under Node, which is what end users have -- nothing in `lib/` may use a
`Bun.*` API, and `node --test` is the gate that would catch it.

One more bites a tree that is not fresh: a rebase or merge can move `bun.lock` under installed
packages. The `just` recipes notice and reinstall: `scripts/install-deps.mjs` keeps a hash of
`package.json` and `bun.lock` in `node_modules` and installs again when either changes. `npm test`
installs nothing, but warns when a tree a recipe installed has moved on since; after a plain `bun
install` there is no hash to compare and it says nothing, and a bare `node --test` never checks, so
run `bun install` first.

**Building from source takes about an hour** and needs a Rust toolchain, ninja and CMake 3.9 or
newer (libaom is built with it), and on x86_64 `yasm` or a NASM older than 3.0, because libaom's
configure rejects NASM 3.x. Downloading the prebuilt binary for the current release is the fast
path and is what CI does.

Everything tracked is an ordinary git object, `docs/assets` included, so a plain clone is enough
for the tests and the documentation images render locally without anything extra. `git-lfs` is
still worth having installed for reading history: commits made before the switch hold pointers,
and without it those check out as pointer text rather than as images.

The crate is the other half of this tree, and one feature set does not cover it. `just typecheck`
checks the `vulkan,window,freetype` set on every host -- that one compiles on macOS too, against
MoltenVK -- and `just lint-check` adds the backend and binding for the machine it runs on, `metal`
on a Mac and `vulkan` elsewhere, and that backend alone, without `window`. What is left for CI is the other platform's GPU backend, which
genuinely will not build here: `metal` needs macOS. The crate needs Rust 1.90 or newer, and versions
independently of the npm package.

## Making a change

`just ci` runs what CI runs. Read the recipe for the list rather than a copy of it -- the copy is
what goes stale. `just prepush` is all of it but the two test suites and the build, and is what
to run before a push: those three are what make `just ci` slow, and CI runs them on every
platform. On a Mac it type-checks the crate with this machine's feature set rather than the
Linux one, which would need a Skia build of its own here; `just ci` checks the Linux set.

A change that reaches a package adds its changelog entry as a file of its own under
`changelog.d/`, not as an edit to `CHANGELOG-npm.md` or `CHANGELOG-crate.md`;
[changelog.d/README.md](changelog.d/README.md) has the form.

`just install-hooks` puts a pre-commit hook in front of `just precommit`, which is the subset fast
enough to sit there: formatting for both languages, ESLint, and clippy twice -- without features,
and with this platform's GPU backend alone, which is the only pass that compiles backend-only code.
About 10 s when no Rust source has changed, about a minute when one has. It leaves out the
full-feature clippy pass and the test suite, which are what make `just ci` take minutes. Opt-in and run once per clone -- it writes a single file into `.git/hooks/`
and leaves alone the git-lfs hooks a pre-switch clone still carries, rather than redirecting
`core.hooksPath` and disabling them.

Rust conventions live in [AGENTS.md](AGENTS.md) — the short version is idiomatic Rust, no `unwrap`
or `expect` without a `// SAFETY:` comment explaining why it cannot fail, and no panics across the
Neon boundary, because a panic there takes down the Node process.

Commit messages explain why, not what -- the diff already says what changed. Length follows from the
reasoning rather than from a limit: most here run twenty to fifty lines because that is what the
argument took, and a genuinely small change takes three. Prose, not bullet lists. [AGENTS.md](AGENTS.md)
has the full guidance.

## Adding a platform target

Targets are listed once, in [`lib/targets.json`](lib/targets.json). The loader map and the release
tooling are both derived from it, and `npm run sync-targets` regenerates `optionalDependencies`.
Adding a target anywhere else will pass tests and then silently fall back to the download path.

A new target also needs its npm package to exist before `optionalDependencies` can reference it —
see the release notes in AGENTS.md.

## Licensing of contributions

Contributions are accepted under the [MIT licence](LICENSE), the same terms as the project. By
opening a pull request you confirm you have the right to submit the work under those terms.

If your change pulls in a new dependency, check its licence is permissive. Its notice is generated
with each binary, as [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) describes; `just check-notices`
fails when a crate that links ships no licence file, and the fix is its text under
`licences/crates/<name>-<version>` with a `source.json` saying where it came from.
