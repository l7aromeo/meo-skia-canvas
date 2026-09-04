# Contributing

## Before anything else

Much of this code is inherited from [skia-canvas](https://github.com/samizdatco/skia-canvas), by way
of [phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas).

Send it here. This fork used to point general fixes upstream, which no longer makes sense: phyron is
dormant, and samizdatco is far enough behind on `skia-safe` that this tree does not merge from them
— anything worth taking arrives by cherry-pick. A change with no home upstream and no home here has
no home at all, so if it is right for this tree, open it against this tree.

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

**`npm test` does not run against the binary you built.** An installed platform package outranks
`lib/skia.node`, so a bare `node --test` after `just build-release` loads the _published_ binary and
your change looks like it did nothing -- on one tree that read as 112 pass / 69 fail where
`just test` reported 181 / 0. `just test` sets `MEO_SKIA_CANVAS_BINARY` to the local build, which is
the whole difference; set it yourself if you are invoking Node directly.

**Bun is the package manager, Node is the runtime.** `bun install` is what fills `node_modules`
and `bun.lock` is the only lockfile; there is no `package-lock.json`. The tests and everything this
package ships still run under Node, which is what end users have -- nothing in `lib/` may use a
`Bun.*` API, and `node --test` is the gate that would catch it.

**Building from source takes about an hour** and needs a Rust toolchain plus ninja. Downloading the
prebuilt binary for the current release is the fast path and is what CI does.

The fixtures are ordinary git objects, so a plain clone is enough for the tests. `docs/assets` is
the only LFS path and nothing in the build or the test suite reads it, which makes `git-lfs`
optional -- without it those files arrive as pointer text and the documentation images do not
render locally.

The crate is the other half of this tree, and one feature set does not cover it. `just typecheck`
checks the `vulkan,window,freetype` set on every host -- that one compiles on macOS too, against
MoltenVK -- and `just lint-check` adds the backend and binding for the machine it runs on, `metal`
on a Mac and `vulkan` elsewhere. What is left for CI is the other platform's GPU backend, which
genuinely will not build here: `metal` needs macOS. The crate needs Rust 1.90 or newer, and versions
independently of the npm package.

## Making a change

`just ci` runs what CI runs. Read the recipe for the list rather than a copy of it -- the copy is
what goes stale.

`just install-hooks` puts a pre-commit hook in front of `just precommit`, which is the subset fast
enough to sit there: formatting for both languages, ESLint, and clippy without features, about four
seconds. It leaves out the feature-carrying clippy pass and the test suite, which are what make
`just ci` take minutes. Opt-in and run once per clone -- it writes a single file into `.git/hooks/`
and leaves the git-lfs hooks alone, rather than redirecting `core.hooksPath` and disabling them.

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

If your change pulls in a new dependency, check its licence is permissive and add it to
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) if it ends up compiled into the published binary.
