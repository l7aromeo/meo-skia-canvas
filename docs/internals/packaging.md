# Packaging

How the npm package finds its native binary, and what a Linux binary needs from
the system it loads on. The rules these facts support are in
[AGENTS.md](../../AGENTS.md#project-mechanics).

## Platform packages

**Binaries resolve from optional platform packages.** `lib/binary.js` probes
`meo-skia-canvas-<triplet>` before falling back to the install script's
download. Each platform package declares `os`, `cpu` and `libc`, so a package
manager selects one without running any script. This exists because bun blocks
postinstall scripts unless the consuming project lists the package in
`trustedDependencies`, and that list is not inherited from dependencies.

## The ABI floors

Two of them, and both fail the same way: the binary does not load at all, with
`ERR_DLOPEN_FAILED`, on a machine where everything else works.

|           | ceiling    | set by                                          |
| --------- | ---------- | ----------------------------------------------- |
| glibc     | **2.34**   | the base image in `containers/Dockerfile.glibc` |
| `GLIBCXX` | **3.4.25** | gcc-toolset, via the same file                  |

Neither is a measurement. Each is the lowest value across the platforms this
project supports -- RHEL 8, AWS Lambda and Amazon Linux 2023, RHEL 9.

libstdc++ is the one that hides: it was invisible behind glibc until glibc was
fixed. gcc-toolset links its own newer libstdc++ statically and leaves only
baseline symbols dynamic, which is what keeps the number low -- that behaviour
is load-bearing, not incidental. `build.yml` asserts both after every Linux
build, and a separate job loads the published Lambda layer and renders through
it.

## Skia on macOS is built from a patched source

The darwin binary does not use rust-skia's prebuilt Skia. skia-bindings
replaces Skia's `@available(macOS 10.15, *)` checks in `GrMtlUtil.mm` and
`GrMtlPipelineStateBuilder.mm` with `false`, and so sends every Metal shader
compile and pipeline creation through a wrapper that waits one second and then
drops the draw. Under load, a process's first draw of a new pipeline comes back
missing -- a transparent export, a dropped blend -- with nothing reported but a
line on stderr. `containers/skia-metal-untimed-compile.patch` asks the OS
version at run time instead, which is what upstream Skia's check does.

The same tree carries `containers/skia-atlas-size.patch`, as Linux's does,
which caps Ganesh's atlas path renderer at a 512-by-512 atlas: on Metal and on
Vulkan, a path the renderer places at an atlas row of 512 or more can have a
pixel's coverage moved a step up or down. The UPSTREAM note beside it in `PATCHES` says what is and
is not known.

Both also carry `containers/skia-triangulation-tolerance.patch`, which makes
Ganesh's triangulating path renderer reuse a cached triangulation only at the
tolerance a draw asks for. It reused one made at any tolerance under three
times the draw's, finer or coarser, so a curve drawn after a draw of the same
path at another scale -- an export at density 1 after one at density 3, or at
density 2 after one at density 1 -- had its edges covered by up to 128 levels
otherwise than on a fresh context.

Both also carry `containers/rust-skia-graphite-bindings.patch`, which is not a
Skia patch: it adds to rust-skia's own sources the three Graphite bindings that
a Graphite engine for float pages needs -- a caching `ImageProvider`, without
which a Recorder drops every raster image;
`ContextOptions::fInternalMultisampleCount`; and reading a Graphite image back
through the Context. That engine is built but selected for no canvas:
`RenderingEngine::for_canvas` asks only for the default engine, Ganesh. Applying
the patch to the tree changes nothing a build compiles. A build takes the
patched crates only through the `.cargo/config.toml` that
`node scripts/skia-source.mjs cargo-patch` writes, which points
`[patch.crates-io]` at the tree's skia-bindings, skia-safe and skia-svg-macros,
and `cargo-patch --remove` takes it away.

- **The gate.** The patched skia-bindings alone prints `meo_graphite_bindings`,
  which `graphite_bindings` in `build.rs` reads through `links = "skia"` and
  turns into `cfg(meo_graphite)`. Code calling the bindings sits behind that
  cfg, so a build against crates.io's skia-bindings -- every published crate
  build -- compiles none of it, and no feature a dependent can turn on reaches
  it. The crate's float canvases draw on the CPU until rust-skia binds these
  itself.
- **The lock.** With the config, Cargo drops those three packages' registry
  source and checksum from `Cargo.lock`. A build that takes the patch restores
  the lock afterwards, and `just check-cargo-lock`, in `precommit` and `ci`,
  refuses one left changed.
- **What CI holds.** Every `rust-ci.yml` job but one builds without the config,
  and the clippy job asserts skia-bindings resolved from crates.io. The
  `graphite bindings (patched, Metal)` job builds with it, runs
  `tests/graphite_bindings.rs`, and greps for the notice `build.rs` prints when
  the cfg turns on, so a gate that never opens cannot pass as one that is shut.

Both also carry `containers/skia-async-transfer-reuse.patch`, which asks for
the transfer buffer of Skia's asynchronous read -- every `getImageData` on a
GPU page -- as `kDynamic` rather than `kStream`, so that Ganesh caches the
buffer and the next read of that size takes it again rather than allocating
one. Skia keeps `kStream` for a crash of Chromium's GPU process in a Windows
test (skbug.com/40042671), whose cause is not known; the UPSTREAM note beside
it in `PATCHES` says why Metal and Vulkan get the same memory either way.

Published Windows binaries are built against rust-skia's prebuilt Skia, which
carries none of these patches, so on Windows the atlas can move a pixel and a
curve is drawn by what the context drew before. `WITHHELD` in
`scripts/skia-source.mjs` records each patch a platform goes without and why,
and the transfer-buffer patch stays off Windows on purpose even once the
others reach it.

**Build through `just` on macOS and Linux.** The justfile's `dotenv-command`,
`scripts/skia-source.mjs env`, sets `SKIA_SOURCE_DIR` for every recipe on macOS
and Linux and on Windows not at all, and `ensure-deps` prepares the tree it
names: rust-skia at the Cargo.lock tag, Skia's DEPS and gn, and the platform's
patches from `PATCHES` applied with `-F0` -- the five above on macOS, and on
Linux `containers/skia-brotli-sources.patch` and the four after the Metal
timeout. The brotli
patch adds the two sources Skia's build list leaves out since M156, without
which a `freetype` build fails to link on `kCmdLut` and
`BrotliDecoderEnsureStaticInit`. It is
made once into `$XDG_CACHE_HOME/meo-skia-canvas`, under a name that carries the
version and a digest of the platform's patches: fetched, about 1 GB, when the
cache holds no tree for the version, and otherwise derived from one -- copied
copy-on-write, its sources restored by git and checked against the tag and
the commits Skia's DEPS pins, then patched -- in seconds, with nothing
downloaded. A tree that fails a check is named on stderr and passed over. Once a new tree
is ready the others are removed, except any still in use, since the cache is
shared by every worktree on the machine. A tree is in use when a worktree's own
patch list names it, when a skia-bindings out-dir in a worktree's target was
built from it, or when a `just` holds it. `env`, the justfile's
`dotenv-command`, holds its tree for the `just` that ran it, so a recipe keeps
its tree while it runs even if the worktree's patch list changes under it. A bare
`cargo build` or `bun run build` outside `just` sets nothing, downloads the
prebuilt, and builds a binary with the timeout in it. On Linux, where no
prebuilt carries `freetype` and skia-bindings builds Skia from its own unpatched
source, `build.rs` compiles the missing brotli files from that source instead,
and does nothing where the build list already has them.

**The release cannot ship that binary.** `build.yml`'s mac job refuses a build
whose skia-bindings output does not say it built offline from that tree, and
its linux job one built from a rust-skia checkout without Linux's atlas,
triangulation and transfer-buffer patches.
On the mac job, `tests/suite/metal-compile.test.js`, required there by
`MEO_SKIA_CANVAS_REQUIRE_METAL`, stops the process's own compiler service for
three seconds under a sandbox that hides the Metal shader cache, and reads a
transparent pixel from a timed build. The structural check is the one that
holds on the hosted runner: a macOS virtual machine shares one compiler service
between processes, so the test has none of its own to stop there, and it skips
on the Apple Paravirtual device alone. An Apple Silicon Mac on macOS 27 starts
a service per process and runs it, and any other machine without one fails it.

**`PATCHES` is the only list of patches.** Two places in CI cannot build
through `just`, and ask the script instead of keeping a list of their own: the
linux job's patch step runs `node scripts/skia-source.mjs apply rust-skia`,
the same loop `prepare` patches a tree with, and each Skia cache key -- in
`build.yml`, `rust-ci.yml` and the skia-source action -- is the output of
`node scripts/skia-source.mjs cache-key`, the platform and the tree's own name.
A copy built green when it was short of a patch, which is how one merged with
`PATCHES` alone. `just check-skia-patches`, run by `precommit`, `ci` and
`ci.yml`, refuses a workflow or action that names a `containers/*.patch` file
outside a comment, a Skia cache keyed any other way, a release patch step that
does not run `apply`, a patch `scripts/check-patched-skia.mjs` has no marker
for and does not name in `UNVERIFIED`, and a patch a platform neither takes
nor withholds.
