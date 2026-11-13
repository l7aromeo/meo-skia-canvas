# Third-party notices

The published `skia.node` is a statically linked binary, and so are the patched Skia libraries in
the `skia-binaries` archives a crate build downloads. Several projects compiled into them require
their copyright notices to travel with binary distributions. Those notices are generated with each
binary, for exactly what that binary links, and ship beside it. This file is the account of where
they ship, how they are made, and what the licence audit found. It does not reproduce the texts
itself.

## Where the notices ship

Each notices file is Markdown. It names what it covers, then gives one section per project with the
project's licence files verbatim, as its source holds them.

- **Each platform's `skia.node`.** The npm release asset `<target>.gz` has `<target>.gz.notices.md`
  beside it, and each platform package carries the same text as `THIRD-PARTY-NOTICES.md`. The
  Lambda layer carries its binary's file under that name too. npm-v7.1.0's assets were given
  theirs after the release; releases after it carry them from the start.
- **The main npm package.** It ships `THIRD-PARTY-NOTICES.md` as the union of every target's
  file, because installing it can install any of those binaries. `publish.yml` writes it over this
  file before staging, and `scripts/stage-npm-package.mjs` refuses a package whose notices are not
  generated ones.
- **Each `skia-binaries` archive.** It carries `THIRD-PARTY-NOTICES.md` at its root, beside
  `key.txt` and `tag.txt`, and the release has the same bytes as a separate asset,
  `<archive name without .tar.gz>.notices.md`. skia-bindings unpacks the file with the libraries
  and links only the libraries. The archives of `skia-binaries-0.156.0-3f566dd7198c` were
  published before archives carried the file and have the separate asset only.

The crate itself is source. A Rust program built from it links Skia from one of those archives, or
from a Skia it compiled, and the notices that program owes when it ships a binary are its own.

## How they are made

`scripts/notices.mjs` writes every one of these files, and decides each part from a record of the
build rather than from a list kept by hand:

- **Skia and the projects its build compiled** come from Skia's own dependency record: ninja's for a
  build that compiled Skia, or `gn desc` over the gn arguments a cached build was made with, or over
  the build directory where the build's ninja keeps no record it can print -- the musl image builds
  with samurai, which has no `deps` tool. Each
  text is read from the Skia tree that build compiled. Linux builds compile FreeType and zlib from
  this repository's `vendor/freetype` and `vendor/zlib` submodules in place of Skia's copies, so
  those have sections of their own naming the commit.
- **Libraries a Linux binary links statically from the build image**, which is fontconfig, have
  their texts in `licences/fontconfig-<version>`, for the release `containers/install-fontlibs.sh`
  builds. Its `source.json` names the tarball and the sha256 the recipe verifies it by. A recipe
  moved to a release with no text there stops the notices, in the release build and in
  `just check-notices`, until the text moves with it. The images do not keep the tarball's own
  COPYING yet, so the text is not compared byte for byte against the one in the image; that comes
  with the next rebuild of the images.
- **Rust crates** are the ones `cargo tree` resolves for that target with the release features,
  normal dependencies only. Each crate's text comes from its package. For a crate whose package
  ships no licence file, it comes from `licences/crates/<name>-<version>`, with a `source.json`
  saying where it was taken from. A crate with neither stops the notices, and with them the
  release build. No generic text stands in.

Two licences ask more of a binary's documentation than their text: each requires a statement of
its own, and a sentence quoted inside the licence is not that statement. FreeType's FTL requires a
binary's documentation to state that it is based in part on the work of the FreeType Team, and
credits the FreeType Project in the form FTL.TXT gives, dated by the release's own `freetype.h`;
FreeType is dual-licensed, so the section also says it is used under the FTL and not the GPLv2.
libjpeg-turbo's IJG clause requires a binary that links it statically to state that it is "based in
part on the work of the Independent JPEG Group". Each notices file makes these statements in the
project's section, above its licence texts, and the script refuses to write or join a file whose
section lacks one.

`just check-notices` runs the script's self-test and checks that every crate each target in
`lib/targets.json` links has a text, and that `licences/crates` holds nothing else.
`node scripts/notices.mjs check <file>...` checks notices files already written for those
statements.

## The licence audit

Everything compiled into a binary is under a permissive licence, and no component is copyleft.
Audited 2026-11-09 with `just licenses` over the **252** crate versions that link into a released
binary — the union across every target in `lib/targets.json`, normal dependencies only. The terms
found were 0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, MIT,
Unicode-3.0, Unlicense and Zlib.

The union, not this machine's build. A crate can link on one platform and not another, so a
host-scoped audit describes whoever ran it: this file said 168 while a Linux binary linked 201, and
`rustix` and `linux-raw-sys` — Linux-only, and the sole carriers of the LLVM exception above — were
named nowhere.

The crate's `fetch` feature is not in that union, because the addon does not enable it, so nothing
it brings is in a released binary. A Rust program that enables it links an HTTP client and a TLS
stack, among them `ring` (Apache-2.0 and ISC), `rustls-webpki` and `untrusted` (ISC), `subtle`
(BSD-3-Clause) and `webpki-roots`, whose certificate data is CDLA-Permissive-2.0 — notices that
program owes when it ships a binary.

Counts here are easy to get wrong in two directions at once, so the recipe pins how this one is
taken. A bare `cargo metadata` reports more, because it counts build and dev dependencies and every
platform's targets — and _fewer_, because it resolves only the default features and so misses
everything reached through `node-addon`, `metal` and `window`. It is neither a subset nor a
superset. The count is also of name-and-version pairs rather than names, because fifteen crates
appear in the graph at two versions.

`cargo metadata --all-features` reports every package Cargo knows about, including build and dev
dependencies and every platform's targets, which is **371** against the **252** that link. Either
number is defensible; quoting one and computing the other is how a count goes stale. `just
licenses` reads both numbers back out of this file and fails when what it counted disagrees.

Skia's C and C++ dependencies are invisible to Cargo, so no `cargo` tool reports them. The audit
above is of Rust crates; the notices for the rest come from Skia's build record, as described
above.

Read for what each licence asks beyond its text, on 2026-11-13: FreeType's FTL and libjpeg-turbo's
IJG clause require the statements above. libpng and zlib say an acknowledgement "would be
appreciated" and do not require one. Apache-2.0 asks a binary to carry the attribution notices of a
`NOTICE` file, and no project or crate here ships one. Fontconfig's licence forbids using its
authors' names in advertising and asks for nothing to be said. Every other licence asks only that
its text travel with the binary.

AV1 is patent-encumbered, and libaom, which the AVIF encoder and decoder are built on, ships a grant
rather than a warranty. Its `PATENTS` file is the Alliance for Open Media Patent License 1.0: a
royalty-free licence to the AOM patents needed to implement the specification, terminated for
anyone who brings a patent claim over AV1. It is not a licence to the code, which is BSD-2-Clause,
and it asks nothing more of a downstream user. It travels in the notices with libaom's licence.

## This project

`meo-skia-canvas` is MIT licensed — see [LICENSE](LICENSE). It is a fork of
[phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas), itself a fork of
[skia-canvas](https://github.com/samizdatco/skia-canvas), both MIT. The upstream copyright notices
are retained in `LICENSE` as those terms require.
