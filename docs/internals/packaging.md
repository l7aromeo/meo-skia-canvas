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

The same tree carries `containers/skia-atlas-size.patch`, which caps Ganesh's
atlas path renderer at a 512-by-512 atlas: on Metal, a path the renderer places
at an atlas row of 512 or more can lose a pixel of its coverage. The UPSTREAM
note beside it in `PATCHES` says what is and is not known.

**Build through `just` on macOS and Linux.** The justfile's `dotenv-command`,
`scripts/skia-source.mjs env`, sets `SKIA_SOURCE_DIR` for every recipe on macOS
and Linux and on Windows not at all, and `ensure-deps` prepares the tree it
names: rust-skia at the Cargo.lock tag, Skia's DEPS and gn, and the platform's
patches from `PATCHES` applied with `-F0` -- the two above on macOS, and
on Linux `containers/skia-brotli-sources.patch`, which adds the two brotli
sources Skia's build list leaves out since M156 and without which a `freetype`
build fails to link on `kCmdLut` and `BrotliDecoderEnsureStaticInit`. It is
fetched once, about 1 GB, into `$XDG_CACHE_HOME/meo-skia-canvas`, under a name
that carries the version and a digest of the platform's patches. Once a new tree
is ready the others are removed, except any a worktree of the repository still
locks: the cache is shared by every worktree on the machine. A bare
`cargo build` or `bun run build` outside `just` sets nothing, downloads the
prebuilt, and builds a binary with the timeout in it. On Linux, where no
prebuilt carries `freetype` and skia-bindings builds Skia from its own unpatched
source, `build.rs` compiles the missing brotli files from that source instead,
and does nothing where the build list already has them.

**The release cannot ship that binary.** `build.yml`'s mac job refuses a build
whose skia-bindings output does not say it built offline from that tree, and
`tests/suite/metal-compile.test.js`, required there by
`MEO_SKIA_CANVAS_REQUIRE_METAL`, stops the process's own compiler service for
three seconds under a sandbox that hides the Metal shader cache, and reads a
transparent pixel from a timed build. The structural check is the one that
holds on the hosted runner: a macOS virtual machine shares one compiler service
between processes, so the test has none of its own to stop there, and it skips
on the Apple Paravirtual device alone. An Apple Silicon Mac on macOS 27 starts
a service per process and runs it, and any other machine without one fails it.
