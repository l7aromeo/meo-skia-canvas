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

**The install script downloads only where no platform package resolves.**
`download` in `lib/prebuild.mjs` asks `lib/binary.js`'s own
`resolvePlatformPackage`, so the two cannot disagree, and in the published
package returns without a request when one does: an install with no route to
GitHub then succeeds wherever npm delivered the binary. A source checkout
downloads regardless, because CI and `npm test` load `lib/skia.node` in place
of the installed platform package.

**Each binary carries its own third-party notices.** build.yml writes
`lib/skia.node.notices.md` beside each release build with `just
notices-release`, and `lib/prebuild.mjs`'s upload refuses a binary without
one and uploads it as `<asset>.notices.md` beside the `.gz`.
`scripts/platform-package.mjs` puts that text in the platform package as
`THIRD-PARTY-NOTICES.md`, and publish.yml joins every target's into the main
package's with `notices.mjs package`, which refuses a target missing. What
the files hold and how they are derived is in
[THIRD-PARTY-NOTICES.md](../../THIRD-PARTY-NOTICES.md).

**The fontconfig a Linux binary links is named by its recipe.**
`scripts/notices.mjs` reads `FONTCONFIG_VERSION` from
`containers/install-fontlibs.sh` and takes the text from
`licences/fontconfig-<version>`, refusing a version with none there. The
images keep only `libfontconfig.a`, not the tarball's COPYING, so the
committed text is checked against the tarball the recipe pins by sha256 and
not against the image itself. Keeping the COPYING in
`/opt/skia-static-libs` and comparing it in build.yml waits for the next
rebuild of the images: a rebuild takes whatever the floating base tags and
package repositories hold that day, which is a toolchain change decided on
its own.

## The ABI floors

Two of them, and both fail the same way: the binary does not load at all, with
`ERR_DLOPEN_FAILED`, on a machine where everything else works.

|           | ceiling    | set by                                          |
| --------- | ---------- | ----------------------------------------------- |
| glibc     | **2.28**   | the base image in `containers/Dockerfile.glibc` |
| `GLIBCXX` | **3.4.25** | gcc-toolset, via the same file                  |

Neither is a measurement. Each is the lowest value across the platforms this
project supports: RHEL 8 sets both, glibc 2.28 and `GLIBCXX` 3.4.25, beside
Ubuntu 20.04 and Debian 11 at glibc 2.31, and RHEL 9, AWS Lambda and Amazon
Linux 2023 at 2.34. The ceilings are `MAX_GLIBC` and `MAX_GLIBCXX` in
`build.yml` and in `containers/abi-probe.sh`.

libstdc++ is the one that hides: it was invisible behind glibc until glibc was
fixed. gcc-toolset links its own newer libstdc++ statically and leaves only
baseline symbols dynamic, which is what keeps the number low -- that behaviour
is load-bearing, not incidental. `build.yml` asserts both after every Linux
build, then loads that build in AlmaLinux 8 and renders through it, and a
separate job loads the published Lambda layer and renders through it. Each of
those loads, and each job's test step, names the binary the job built, and the
two loads fail if the addon they loaded is any other file: an installed
platform package outranks `lib/skia.node`, so a bare `require` tests the
published binary instead.

## Skia is built from a patched source

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

macOS and Linux also carry `containers/skia-async-transfer-reuse.patch`, which
asks for the transfer buffer of Skia's asynchronous read -- every `getImageData`
on a GPU page -- as `kDynamic` rather than `kStream`, so that Ganesh caches the
buffer and the next read of that size takes it again rather than allocating
one. Skia keeps `kStream` for a crash of Chromium's GPU process in a Windows
test (skbug.com/40042671), whose cause is not known; the UPSTREAM note beside
it in `PATCHES` says why Metal and Vulkan get the same memory either way.

Every platform carries `containers/skia-svg-synthetic-slant.patch`, which makes
Skia's SVG text synthesise the slant a `font-style` asks a family with no
slanted face for, as canvas text does, and parse `oblique <angle>`. Its
UPSTREAM note in `scripts/skia-source.mjs` says how.

Windows binaries are built from a patched source too, carrying the atlas-size
cap, the triangulation tolerance, the Graphite bindings and
`containers/skia-dwrite-oblique.patch`. The last makes Skia's DirectWrite port
read `OS/2` `fsSelection` bit 9 as its FreeType port does: DirectWrite reports
a face flagged oblique only by that bit as upright, so the family's upright
face and its oblique one are reported alike. Text chooses between such faces
from their tables either way; the patch makes the reported slant right, which
`FontLibrary.family().styles` and an SVG document's `font-style` go by.
`WITHHELD` in `scripts/skia-source.mjs` records each patch a platform goes
without and why: Windows goes without the transfer-buffer patch on purpose, for
the crash above, and without the Metal timeout and the brotli sources, which
change nothing it builds.

**Build through `just`.** The justfile's `dotenv-command`,
`scripts/skia-source.mjs env`, sets `SKIA_SOURCE_DIR` for every recipe, and
`ensure-deps` prepares the tree it names: rust-skia at the Cargo.lock tag,
Skia's DEPS and gn, and the platform's patches from `PATCHES` applied with
`-F0` -- on macOS the Metal timeout, the atlas-size cap, the triangulation
tolerance, the Graphite bindings, the transfer buffer and the SVG slant; on
Linux `containers/skia-brotli-sources.patch` and the last five of those; and on
Windows the atlas-size cap, the triangulation tolerance, the Graphite bindings,
the DirectWrite oblique patch and the SVG slant. `env`
single-quotes a directory holding a backslash, a space, a quote or `$`, which
just's dotenv reader would otherwise take as an escape or the value's end; a
plain path stays bare, so `export $(... env)` still works where it did. The brotli
patch adds the two sources Skia's build list leaves out since M156, without
which a Linux build, which always embeds FreeType and its WOFF2 decoder, fails
to link on `kCmdLut` and
`BrotliDecoderEnsureStaticInit`. It is
made once into `$XDG_CACHE_HOME/meo-skia-canvas`, under a name that carries the
version and a digest of the platform's patches: fetched, about 1 GB, when the
cache holds no tree for the version, and otherwise derived from one -- copied
copy-on-write, its sources restored by git and checked against the tag and
the commits Skia's DEPS pins, then patched -- in seconds, with nothing
downloaded. A tree that fails a check is named on stderr and passed over. One
prepare makes a tree at a time: it holds `locks/<tree>` in the cache, with its
PID and start time, from the removal of the tree's `.partial` to its rename, so
a second `just` naming the same tree waits for the first -- saying for which
process, for 30 minutes at most -- and then uses the tree it made. A lock whose
holder has ended, or whose PID now runs a process started at another time, is
taken over. A tree directory with no `.ready`, which only a prepare that ended
early leaves, is removed and made again. Once a new tree
is ready the others are removed, except any still in use, since the cache is
shared by every worktree on the machine. A tree is in use when a worktree's own
patch list names it, when a skia-bindings out-dir in a worktree's target was
built from it, or when a `just` holds it. `env`, the justfile's
`dotenv-command`, holds its tree for the `just` that ran it, so a recipe keeps
its tree while it runs even if the worktree's patch list changes under it. A bare
`cargo build` or `bun run build` outside `just` sets nothing, downloads the
prebuilt, and builds a binary with the timeout in it. On Linux, where no
prebuilt carries FreeType and skia-bindings builds Skia from its own unpatched
source, `build.rs` compiles the missing brotli files from that source instead,
and does nothing where the build list already has them.

**The release cannot ship that binary.** `build.yml`'s mac and windows jobs
refuse a build whose skia-bindings output does not say it built offline from
that tree, and its linux job one built from a rust-skia checkout without
Linux's atlas, triangulation and transfer-buffer patches. The windows job
caches only the source tree, as the mac job does, and compiles Skia on every
run.
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

## Compiled Skia is cached for every worktree

A build compiles Skia once per feature set -- about 1,730 ninja steps -- and an
out-dir cannot be copied to another worktree to save it:
skia-bindings' generated build files reach the source tree by paths relative
to the out-dir, so a copy at another directory depth regenerates and rebuilds
everything. So compiled Skia is kept once, in
`$XDG_CACHE_HOME/meo-skia-canvas/binaries`, and linked from there.

**How a build links it.** With `SKIA_LIBRARY_SEARCH_PATH` set beside
`SKIA_SOURCE_DIR`, skia-bindings' build script copies Skia's libraries from
that directory and runs no ninja: it prints `STARTING BIND AGAINST SYSTEM
SKIA`, generates `bindings.rs` from the patched tree's headers with the
definitions in `SKIA_BUILD_DEFINES`, and compiles only its own bindings
sources. An entry holds what that needs: the libraries a build compiled and
the `skia-defines.txt` it saved beside them. Measured on an M4 Pro, the build
script took 8 seconds this way, and `just build` 40 seconds from an empty
target.

**Which entry.** One per tree, target, SDK, profile and skia-bindings feature
set, the features being what `cargo tree` resolves for a call's flags. The
SDK is named by the version its `SDKSettings.json` records, as
`MacOSX27.2.sdk`, so every path to one SDK -- `MacOSX.sdk`, the symlinks
beside it, the Command Line Tools' copy -- finds one entry; an SDK that
records no version is never cached. Xcode's and the Command Line Tools'
copies of one version share an entry: measured on 27.2, they compile Skia's
CoreText and Metal sources to the same bytes. The
tree is `treeName`, so a change of patches misses the cache rather than
linking a stale entry, and a superseded tree's entries are removed with the
tree. Every cargo call in the justfile that builds skia-bindings goes through
`node scripts/skia-cache.mjs cargo` -- `skia_cargo` -- which chooses per call,
since one recipe can build several feature sets. It says on stderr whether it
linked an entry or is compiling Skia, and why. `bun run build` reaches it
through `lib/prebuild.mjs`. A plain `cargo` call, or one with no entry for its
features, compiles Skia as before.

**What the entry compiled.** An entry also keeps `notices.json`: the Skia
tree and the projects its build compiled, from ninja's dependency record,
which the third-party notices of a build that links it are written from. An
entry stored before entries kept one has it derived when a lookup finds it,
by `gn desc` over the arguments its seed log printed, and is linked as
before -- the same set as ninja's, measured on three entries whose seed
builds survived. An entry whose log names no arguments, or whose tree has no
`gn` to ask, is not linked, and the build that compiles instead stores the
record. Run with no `SKIA_SOURCE_DIR` where
`skia-source.mjs env` would name a tree, the script refuses and names the
command that sets it, rather than let skia-bindings build against rust-skia's
unpatched Skia; `SKIA_CACHE_UNPATCHED=1` asks for that build on purpose. `just skia-cache-fill <cargo flags>` builds
a feature set from source and stores it. An entry is about 70 MB.

**Not rust-skia's prebuilt binaries.** skia-bindings also downloads archives,
`file://` URLs included, but only with `SKIA_SOURCE_DIR` unset, keyed by
rust-skia's commit rather than by these patches, with nothing verifying what
it unpacks, and with a fall back to compiling crates.io's unpatched Skia when
the archive is missing.

**What still vouches for the patches.** An entry is stored only from a build
`scripts/check-patched-skia.mjs` passes, from the worktree's own tree, and it
keeps that build's log. A build that linked an entry passes the same check
when every library it linked is, byte for byte, one an entry of the
worktree's tree holds, and that entry's log passes: patched where it was
compiled, the same bytes where it was linked. A library changed after it was
stored, or an entry from another tree, fails. The check holds a build with no
Ganesh -- the clippy pass with no features -- to the patched files it
compiles, which are the SVG slant and the face match.

## Patched Skia for crate builds

A crate build from crates.io compiles nothing of this repository's: its
skia-bindings comes from the registry and, with `SKIA_SOURCE_DIR` unset,
downloads Skia by URL. So the patched Skia reaches it as archives in the
layout skia-bindings downloads, uploaded to a release of this repository for
`SKIA_BINARIES_URL` to name. What a consumer sets is in
[docs/rust.md](../rust.md#drawing-what-the-npm-package-draws); this is how
the archives are made and how a build takes them.

**The release.** One per patch set: `skia-binaries-<version>-<digest>`
(`releaseName` in `scripts/skia-pack.mjs`), the digest over every hosted
platform's tree. Each platform takes patches of its own, so each has a tree
of its own (`treeName`), and the release follows all of them: a change to
one platform's patches is a new release and a new URL, and an archive once
uploaded describes one patch set for good. It is a prerelease and never
marked latest. Archives are never replaced: the URL carries no hash a
consumer's build could check, so a replaced archive would change what an
existing build links with nothing to show for it.

**The key.** skia-bindings downloads `skia-binaries-{key}.tar.gz`, `{key}`
being `<commit>-<target>-<features>[-static]` (`key` in
`scripts/skia-pack.mjs`, after skia-bindings' `binaries::key`). The commit is
the 20 hex digits of the rust-skia commit crates.io's skia-bindings was
packaged from, read from that package's `.cargo_vcs_info.json`, and not
anything of ours: a consumer's build computes it from the same file. The
features are skia-bindings' own, sorted, those it drops for a target left
out -- FreeType's on Apple and Windows -- and `-static` follows the C
runtime the consumer's target links.

**The archive.** `skia-binaries/`, holding the static libraries
skia-bindings builds for the key's features -- a download unpacks every one
and fails on one missing -- `bindings.rs`, `icudtl.dat` on Windows with
`textlayout`, and `tag.txt` and `key.txt`, as skia-bindings' own export
writes them -- and `THIRD-PARTY-NOTICES.md`, the licences of what those
libraries hold, from the build's own record (`archiveNotices` in
`scripts/notices.mjs`). skia-bindings unpacks it with the rest and links only
the libraries: measured with a `file://` archive, a consumer's build-script
output is the same with the file and without it. `skia-pack upload` puts the
same bytes beside the archive as `<archive name without .tar.gz>.notices.md`,
uploaded first, so the notices can be read without unpacking anything; the
asset name is a contract other projects read. `skia-pack pack` makes one from a build directory, and only from
a build `scripts/check-patched-skia.mjs` passes, compiled from the platform's
patched files with no download in its log, whose build-script stdout links
those libraries as skia-bindings prints them: one run of consecutive lines, in
its order, `static=` before each name except on Windows. The lines around that
run name what the target links from the system -- expat twice on a musl
build -- which a download links again where it is used, so none is packed.

**Which archives.** `skia-pack plan` lists them: for each target in
`lib/targets.json`, each combination of the crate features that change
skia-bindings' build there (`CRATE_FEATURE_SETS`: `metal` on macOS, `vulkan`
on Linux and Windows), and each C runtime a consumer links (`crtVariants`) --
22 archives, one build each, keyed by what `cargo tree` resolves. Every
Linux key carries FreeType's features, since the crate embeds FreeType on
Linux whatever a consumer sets. Two runtimes on Windows, where MSVC links
the C runtime dynamically unless asked, and on musl, where Rust links it
statically unless asked: the npm build is the one that asks there. A
consumer's own skia-safe feature on top of the crate's has no archive, and
its download is a 404.

**`skia-binaries.txt`.** Generated by `skia-pack manifest` from `PATCHES` and
build.rs's `SKIA_PATCHES`, the same on every host: the release, the URL, and
for each hosted target the tree its archives are built from and the patch
cfgs they carry. It ships in the crate: a downloaded build has no source for
build.rs's marker scan to read, so its cfgs and the tree `skia_patches()`
reports come from here, and build.rs refuses a URL naming another release
or a target not listed (`build/skia_mode.rs` says when). `just
check-skia-pack` refuses a manifest that `PATCHES` and build.rs no longer
give, and a doc in `DOCS_WITH_THE_LINE` whose `SKIA_BINARIES_URL` line is
not the manifest's.

**The marker.** `containers/skia-patch-marker.patch` defines
`meo_skia_canvas_needs_its_patched_skia_see_SKIA_BINARIES_URL` in
`SkGraphics.cpp`, a function no other Skia has, and a build with
`meo_skia_marker` references it where it starts Skia. A build that took the
archives' cfgs but linked other Skia -- its download fell back to compiling
crates.io's -- fails to link, naming it.

**The workflow.** `.github/workflows/skia-binaries.yml`, dispatched, and run
on a pull request that changes it or `skia-pack`. `plan` lists the archives
and, with `publish` on, which the release already holds; a held archive's
cell builds nothing, so a re-run fills in what is missing. Each cell
compiles the crate in release for its target -- macOS and Windows from
`skia-source prepare`'s tree, Linux in build.yml's containers from a
rust-skia checkout at the packaged commit, through `just use-local-skia` --
and `skia-pack build --expect` packs it, refusing an archive under any key
but the planned one. With `publish` on, each cell then uploads its archive
with `skia-pack upload`, which leaves a name the release holds with the same
bytes and refuses one with other bytes -- a step of the cell's own, since the
arm64 Alpine container runs no JavaScript action to hand an artifact on --
and `complete` holds the release to the plan with `skia-pack published`.

**Build scripts and the C runtime.** Every cell builds for its host without
`--target`, as build.yml does, so the images' RUSTFLAGS reach skia-bindings'
build script: given `--target`, cargo passes RUSTFLAGS to the target alone.
That matters on musl, where a build script linked statically -- musl's
default -- cannot load libclang for bindgen. The same rule is why a static
musl archive needs nightly: the target has to link the C runtime statically
and the build script must not, and only `-Zhost-config` sets the two apart.
Those four cells use the justfile's pinned nightly, with the image's flags
as `host.rustflags` and `+crt-static` in RUSTFLAGS, which then reach the
target alone.

**The consumer's build.** rust-ci.yml's `crate consumer` job builds the
crate as crates.io's users do, skia-bindings from the registry and
`SKIA_SOURCE_DIR` unset: patched on Linux, macOS and Windows with the docs'
line in a `.cargo/config.toml`, and stock on macOS, setting nothing, where
rust-skia hosts the default feature set's archive. `skia-pack consumer`
refuses a build that compiled Skia instead of downloading it, or whose patch
cfgs are not the ones its Skia carries.

**The order of a patch change.** A pull request that changes a patch
changes a tree and so the release, so it regenerates `skia-binaries.txt` and
the docs' line, and its `crate consumer` patched legs fail on `published`
until the new release is complete. Dispatching skia-binaries.yml with
`publish` on makes the archives: they are named by the patches' digest, so
the same patches on any branch name the same release. `just release-crate`
refuses until `skia-pack published` passes, since the published crate
carries the URL.
