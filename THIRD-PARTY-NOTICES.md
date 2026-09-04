# Third-party notices

The published `skia.node` is a statically linked binary. Several of the projects compiled into it
require their copyright notices to travel with binary distributions, so those notices are collected
here rather than left in the source trees they came from.

Everything below is under a permissive licence, and no component is copyleft. Audited 2026-09-04
with `just licenses` over the **167** crate versions that link into a released binary — the
`node-addon`, `metal` and `window` feature set, normal dependencies only. The terms found were
0BSD, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, MIT, Unicode-3.0, Unlicense and Zlib.

Counts here are easy to get wrong in two directions at once, so the recipe pins how this one is
taken. A bare `cargo metadata` reports more, because it counts build and dev dependencies and every
platform's targets — and _fewer_, because it resolves only the default features and so misses
everything reached through `node-addon`, `metal` and `window`. It is neither a subset nor a
superset. The count is also of name-and-version pairs rather than names, because six crates appear
in the graph at two versions. An earlier version of this file said 135 without saying which
measurement it meant, and it went stale with no way to tell what to re-run.

Most of the graph is `MIT OR Apache-2.0`, taken under MIT. The sections below are the components
whose terms are _not_ satisfied by that alone — a BSD notice that has to travel with a binary, an
Apache attribution, a credit clause, or a patent grant worth knowing about.

## Skia

Included in every binary, via [`skia-safe`](https://github.com/rust-skia/rust-skia).

> Copyright (c) 2011 Google Inc. All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification, are permitted
> provided that the following conditions are met:
>
> - Redistributions of source code must retain the above copyright notice, this list of conditions
>   and the following disclaimer.
> - Redistributions in binary form must reproduce the above copyright notice, this list of
>   conditions and the following disclaimer in the documentation and/or other materials provided
>   with the distribution.
> - Neither the name of Google Inc. nor the names of its contributors may be used to endorse or
>   promote products derived from this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
> IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
> FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
> CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
> DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
> IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
> OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Skia itself bundles further components — among them libpng, libwebp, libjpeg-turbo and HarfBuzz —
each under its own permissive licence. Their notices ship with the Skia source at
<https://skia.googlesource.com/skia/+/main/LICENSE> and in the third-party directories referenced
there.

## FreeType

Linked into the **Linux** binaries only, where the build enables the `freetype` feature. macOS and
Windows builds use the platform's own font stack and do not include it.

FreeType is dual-licensed under the FreeType Licence (BSD-style with a credit clause) and GPLv2.
This project uses it under the FreeType Licence, which requires the following credit:

> Portions of this software are copyright © The FreeType Project (www.freetype.org).
> All rights reserved.

## zlib

Linked into the Linux binaries via Chromium's zlib fork, under the zlib licence.

> This software is provided 'as-is', without any express or implied warranty. In no event will the
> authors be held liable for any damages arising from the use of this software.

## Expat

Skia's SVG parser is built on Expat, so it is linked into every binary that has the `svg` feature
— which is all of them. Pinned by Skia's `DEPS` at `libexpat@6154446`, and its presence in the
shipped binary is not inferred from that file: `strings lib/skia.node` finds `EXPAT_MALLOC_DEBUG`.

Expat is under the licence that shares its name, which is MIT's text under an older title.

> Copyright (c) 1998-2000 Thai Open Source Software Center Ltd and Clark Cooper
> Copyright (c) 2001-2025 Expat maintainers
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
> associated documentation files (the "Software"), to deal in the Software without restriction,
> including without limitation the rights to use, copy, modify, merge, publish, distribute,
> sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.

## Wuffs

Skia decodes GIF through Wuffs, so it too is in every binary — `strings` finds `Wuffs` in the
shipped one. Pinned at `wuffs-mirror-release-c@e3f919c`, and licensed Apache-2.0, which asks the
same as the crates in the Apache section below: attribution travels with a redistribution, and the
patent grant ends if you sue over the work.

> Copyright the Wuffs Authors, licensed under the Apache License, Version 2.0
> (http://www.apache.org/licenses/LICENSE-2.0).

Neither was in this file at all — not even under the pointer to Skia's third-party directories
above, which is how libpng, libwebp, libjpeg-turbo, HarfBuzz and ICU are handled. They were found
by looking in the shipped binary rather than in `cargo metadata`, which is the gap worth naming:
the count below is of Rust crates, and Skia carries a dozen C and C++ libraries that Cargo cannot
see at all. No `cargo` tool will ever remind anyone to add them.

That leaves this file taking two positions at once — the text reproduced for Skia, FreeType, zlib,
Expat and Wuffs, and a URL for the rest. Reproduction is the safer reading of a BSD or libpng
notice, which asks to be included "in the documentation and/or other materials provided with the
distribution"; a link is not obviously that. Worth settling in one direction before a release.

## libaom, via libaom-sys

Both halves of AV1: the encoder behind `toBuffer("avif")` and the decoder behind `loadImage` of an
`.avif`, compiled in and called through the bindings in `src/encode/aom.rs` and
`src/decode/aom.rs`. This is the Alliance for Open Media's own C library, and it is the reference
the format is defined against. BSD-2-Clause, so the notice has to travel with a binary.

It replaced rav1e, which encoded AVIF up to `0.6.0` and is no longer in the tree — `v_frame` and
`av1-grain` left with it. rav1e cannot code losslessly, and having one library read the
specification for both directions is worth more than having a pure-Rust one read half of it. The
practical cost is that a build now needs a C toolchain on every target.

Reached directly rather than through the `aom-decode` wrapper, which cannot be built without its
`avif` feature — the `#[cfg(feature = "avif")]` guarding that feature's error variants sits inside
a `quick_error!` invocation, and the macro drops it, so the crate fails to compile with
`default-features = false`. That feature pulls in `avif-parse`, which is MPL-2.0 and which
`just licenses` refuses. Since `src/decode/avif.rs` parses the container itself, the wrapper had
nothing left to offer but the decoder handle.

> Copyright (c) 2016, Alliance for Open Media. All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification, are permitted
> provided that the following conditions are met:
>
> 1. Redistributions of source code must retain the above copyright notice, this list of conditions
>    and the following disclaimer.
> 2. Redistributions in binary form must reproduce the above copyright notice, this list of
>    conditions and the following disclaimer in the documentation and/or other materials provided
>    with the distribution.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
> IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
> FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
> CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
> DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
> IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
> OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

**AV1 is patent-encumbered, and libaom ships a grant rather than a warranty.** Its `PATENTS` file is
the Alliance for Open Media Patent License 1.0, which grants a royalty-free licence to the AOM
patents needed to implement the specification, and terminates that grant for anyone who brings a
patent claim over AV1. It is not a licence to the code — BSD-2-Clause is — and it imposes no
obligation on downstream users of this project beyond that termination condition. It is noted here
because a patent grant is the kind of thing an audit should not have to discover on its own; the
full text ships with the libaom source.

## avif-serialize

Writes the ISOBMFF container the AV1 payload sits in. BSD-3-Clause, whose third clause is the
no-endorsement one.

> Copyright (c) 2020, Cloudflare, Inc.
> All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification, are permitted
> provided that the following conditions are met:
>
> 1. Redistributions of source code must retain the above copyright notice, this list of conditions
>    and the following disclaimer.
> 2. Redistributions in binary form must reproduce the above copyright notice, this list of
>    conditions and the following disclaimer in the documentation and/or other materials provided
>    with the distribution.
> 3. Neither the name of the copyright holder nor the names of its contributors may be used to
>    endorse or promote products derived from this software without specific prior written
>    permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
> IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
> FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
> CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
> DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
> IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
> OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## Other BSD-licensed crates

The same two notices in substance, from other copyright holders, for components reached through the
rest of the graph rather than through the image formats:

| crate                             | licence                                  | used for                                 |
| --------------------------------- | ---------------------------------------- | ---------------------------------------- |
| `brotli`                          | BSD-3-Clause **and** MIT                 | font decompression, via `allsorts`       |
| `brotli-decompressor`             | BSD-3-Clause or MIT                      | the decoding half of the above           |
| `alloc-no-stdlib`, `alloc-stdlib` | BSD-3-Clause                             | allocator shims for those two            |
| `encoding_rs`                     | (Apache-2.0 or MIT) **and** BSD-3-Clause | legacy text encodings, via `winit`       |
| `glyph-names`                     | BSD-3-Clause                             | mapping glyph names to code points       |
| `libloading`                      | ISC                                      | loading the platform's graphics driver   |
| `unicode-ident`                   | (MIT or Apache-2.0) **and** Unicode-3.0  | identifier tables, from the Unicode data |

Each ships its licence text with its source. The bolded `and`s are not a choice: both sets of terms
apply at once, so the BSD notice travels even where MIT would otherwise have been enough.

## Apache-2.0 components

Five crates offer Apache-2.0 alone, and one requires it alongside MIT. Apache-2.0 asks that
attribution and any `NOTICE` file travel with a redistribution, and grants patent rights that
terminate on a patent claim against the work.

| crate                                                                                   | used for                         |
| --------------------------------------------------------------------------------------- | -------------------------------- |
| `allsorts`                                                                              | font parsing and shaping         |
| `winit`                                                                                 | the window and event loop        |
| `spin_sleep`                                                                            | frame pacing                     |
| `unicode-canonical-combining-class`, `unicode-general-category`, `unicode-joining-type` | text segmentation tables         |
| `dpi` (Apache-2.0 **and** MIT)                                                          | logical and physical pixel units |

None ships a `NOTICE` file; their attribution is the crate name and licence text carried with their
source.

## Image format encoders

Everything this project encodes that Skia cannot. All `MIT OR Apache-2.0` except `tiff`, which is
MIT, so all are taken under MIT and need no notice beyond this one. Listed because a reader
auditing the image pipeline should be able to see it in one place.

| crate       | licence           | format                               |
| ----------- | ----------------- | ------------------------------------ |
| `gif`       | MIT or Apache-2.0 | GIF                                  |
| `png`       | MIT or Apache-2.0 | APNG, and the payloads inside an ICO |
| `quantette` | MIT or Apache-2.0 | palette reduction for GIF            |
| `tiff`      | MIT               | TIFF                                 |

BMP and ICO are written by hand in `src/encode` and pull in nothing.

## Rust dependencies

The Cargo graph is permissive throughout. `just licenses` regenerates the breakdown over the
packages that actually link — which is the set this file describes, and not what a bare
`cargo metadata` returns.

The difference matters. `cargo metadata --all-features` reports every package Cargo knows about,
including build and dev dependencies and every platform's targets, which is **342** against the
**167** that link. Either number is defensible; quoting one and computing the other is how the count
in this file went stale without anyone noticing. `just licenses` now reads both numbers back out of
this file and fails when what it counted disagrees, so the two cannot drift apart again silently.

A per-crate listing with full licence texts can be produced with
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) or
[`cargo-license`](https://github.com/onur/cargo-license).

## This project

`meo-skia-canvas` is MIT licensed — see [LICENSE](LICENSE). It is a fork of
[phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas), itself a fork of
[skia-canvas](https://github.com/samizdatco/skia-canvas), both MIT. The upstream copyright notices
are retained in `LICENSE` as those terms require.
