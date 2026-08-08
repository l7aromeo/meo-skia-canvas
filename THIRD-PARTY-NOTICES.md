# Third-party notices

The published `skia.node` is a statically linked binary. Several of the projects compiled into it
require their copyright notices to travel with binary distributions, so those notices are collected
here rather than left in the source trees they came from.

Everything below is under a permissive licence. No component is copyleft: an audit of the 135
packages in the Cargo dependency graph found only MIT, Apache-2.0, BSD-3-Clause, ISC, Zlib, 0BSD and
Unlicense terms.

## Skia

Included in every binary, via [`skia-safe`](https://github.com/rust-skia/rust-skia).

> Copyright (c) 2011 Google Inc. All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification, are permitted
> provided that the following conditions are met:
>
> * Redistributions of source code must retain the above copyright notice, this list of conditions
>   and the following disclaimer.
> * Redistributions in binary form must reproduce the above copyright notice, this list of
>   conditions and the following disclaimer in the documentation and/or other materials provided
>   with the distribution.
> * Neither the name of Google Inc. nor the names of its contributors may be used to endorse or
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

## Rust dependencies

The Cargo graph is permissive throughout. To regenerate the breakdown:

```bash
cargo metadata --format-version 1 \
  | python3 -c "import json,sys,collections; \
      c=collections.Counter(p.get('license') or 'UNKNOWN' for p in json.load(sys.stdin)['packages']); \
      [print(f'{n:4d}  {l}') for l,n in c.most_common()]"
```

A per-crate listing with full licence texts can be produced with
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) or
[`cargo-license`](https://github.com/onur/cargo-license).

## This project

`meo-skia-canvas` is MIT licensed — see [LICENSE](LICENSE). It is a fork of
[phyron-skia-canvas](https://github.com/phyrondev/phyron-skia-canvas), itself a fork of
[skia-canvas](https://github.com/samizdatco/skia-canvas), both MIT. The upstream copyright notices
are retained in `LICENSE` as those terms require.
