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
npm ci --ignore-scripts
node lib/prebuild.mjs download   # or `just build-release` to build from source
npm test
```

Two things bite people on a fresh clone:

**Install `git-lfs` first.** The image and font fixtures are stored in LFS. Without it they check
out as pointer text and roughly two dozen tests fail with `Could not decode image data`, which looks
nothing like the real cause.

**Building from source takes about an hour** and needs a Rust toolchain plus ninja. Downloading the
prebuilt binary for the current release is the fast path and is what CI does.

## Making a change

`just ci` runs what CI runs: `fmt-check typecheck lint-check test build`.

Rust conventions live in [AGENTS.md](AGENTS.md) — the short version is idiomatic Rust, no `unwrap`
or `expect` without a `// SAFETY:` comment explaining why it cannot fail, and no panics across the
Neon boundary, because a panic there takes down the Node process.

Commit messages are two or three sentences: what was wrong, what changed, and context only if it is
needed. No bullet lists.

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
