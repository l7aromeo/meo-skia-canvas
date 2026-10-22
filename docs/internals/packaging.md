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
