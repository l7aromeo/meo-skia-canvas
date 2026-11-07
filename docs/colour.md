# Colour and precision

How a canvas holds, composites and writes colour: the spaces it takes, what
a float `colorType` buys, what a narrow one does not, and what each file
format carries. Where the output differs from Chrome's on purpose, and why,
is in [browser-differences.md](browser-differences.md).

A canvas composites in the space you name and exports in it, rather than compositing in sRGB and
converting at the end:

```js
let canvas = new Canvas(1920, 1080, {
  colorType: "RGBAF16", // case-sensitive; an unrecognized name throws
  colorSpace: "display-p3", // or rec2020, srgb-linear, rec2020-pq, ...
});
```

Fifteen names across eight spaces — sRGB, Display P3, Rec. 2020, HDR10 (PQ), HLG and linear variants
— on both surfaces. `getImageData` reads back in any of them, and the CSS Color 4 functions parse:
`lab()`, `lch()`, `oklab()`, `oklch()`, `hwb()`, `color(<space> r g b / a)` and the relative form
`color(from <origin> <space> ...)`, and so does CSS Color 5's `color-mix()`. The same sRGB red, read four ways: `srgb` 255,0,0 · `display-p3` 234,51,35 · `rec2020` 210,84,46 · `rec2020-pq`
136,83,56.

**A file read back carries the space it was written in.** PNG, JPEG, WebP, APNG, ICO, BMP and AVIF
all do, and a matrix of formats against spaces pins it on both surfaces rather than on one — the
decode existed twice, once for the crate and once for the addon, and only one copy relabelled a BMP.
GIF round-trips by narrowing rather than by carrying: it is converted to sRGB on the way out, so a
file that says nothing about its colour is telling the truth. TIFF is export-only, this build of
Skia having no decoder for it. And BMP refuses `rec2020-pq` and `rec2020-hlg` rather than
approximate them, because a `BITMAPV4HEADER` states a transfer function as one exponent per channel
and neither of those is a power law.

**A float `colorType` composites in float**, not merely reads back in it. Sixty fills at 0.6% alpha
land on `0.30308` (`RGBAF32`) and `0.30298` (`RGBAF16`) against an arithmetic answer of `0.30308`.
At eight bits every layer rounds to a whole level and the error compounds: `0.23922` on the CPU, and
`0.36078` on the GPU, which misses in the other direction. It costs twice the memory for `RGBAF16`
and four times for `RGBAF32`; the time cost depends entirely on what you draw, and is measured under
[Performance and memory](performance.md). Such a canvas renders on the raster backend
whatever `gpu` says, because Skia's Ganesh backend, which the GPU engine runs on, rounds each draw's
colour to eight bits before blending it: an `RGBAF16` page reads `0.37695` there on Metal, further
from right than eight bits on either engine. `canvas.engine` reports which engine took it.

**A `colorType` narrower than four bytes a pixel is an output format, not a compositing format.**
Skia will build a surface in any of them; compositing in one is what costs. An opaque format loses
transparency — `Gray8`, `RGB565`, `R8UNorm` and `R8G8UNorm` turn the transparent clear black and
resolve every blend against it — and an alpha-only one loses colour, `Alpha8`, `A16Float` and
`A16UNorm` reading every colour back as black. So a canvas composites at four bytes a pixel whatever
its `colorType` says, and choosing a narrow one changes the pixels the canvas hands back rather than
the memory it holds. `ARGB4444` is the only narrower type that keeps both, and it is not used
either, because four bits a channel quantises every intermediate blend and not just the output.
Two things are true at once here, and measuring only
one of them is easy: the buffer `getImageData` hands back _is_ sized by the type and really is
smaller, while the canvas behind it is not. Twenty 1200×900 canvases, reading a single pixel so the
figure is the surface alone, cost 0.34 MB each at `rgba`, `Gray8` and `RGB565` alike, against 0.58
for `RGBAF16` and 1.10 for `RGBAF32`: the float types are composited in, being wider, and the narrow
ones are not.
Read the same twenty whole and they cost 8.80, 6.36 and 7.56 MB, which is the returned buffer
shrinking and not the canvas. Whether the canvas behind it is smaller depends on whether that format
is one a canvas composites in at all, which today is the three float ones and nothing else.

From Rust, `Canvas::compositing_color_type()` answers this per canvas rather than leaving it to be
inferred — see [`docs/rust.md`](rust.md). There is no JavaScript equivalent.

A `Gray8` canvas is not a greyscale canvas, either. It stores colour and converts on the way out:
paint it red and the byte reads 54, the Rec. 709 luminance of red computed at readback, while
`{colorType: "rgba"}` on the same pixel still gives back `255,0,0`. A `#ff8000` fill on `RGB565`
reads `G` = 128 rather than the ~130 a real RGB565 surface owes, and the PNG a `Gray8` canvas writes
is byte-identical to the `rgba` one.

The `rec2020-pq` and `rec2020-hlg` spaces build a canvas with that transfer function and tag exports
with it, which is what a Rec. 2020 pipeline wants. On a float `colorType` the values are not clamped
to 1.0 — scene values of 1, 2, 4 and 8 store as 0.580, 0.734, 0.896 and 1.055, so a component above
one encodes further up the PQ curve, whose top is 10,000 nits rather than SDR white. An eight-bit
canvas saturates instead, at 255.

Whether that reaches a file depends on the container. **AVIF is encoded here rather than by Skia**,
at 8, 10 or 12 bits, and carries the Rec. 2020 primaries and the PQ or HLG transfer both in the AV1
sequence header and in the `colr` box — so a PQ AVIF is an HDR file. None of the formats Skia encodes
— PNG, JPEG, WebP — is an HDR container; for those this is correctly tagged Rec. 2020 output for a
pipeline that takes the raw buffer elsewhere.

Reading a still one back returns what was written. Round-tripping `color(srgb 0.8 0.2 0.1)` through a
12-bit AVIF moves it by 0.005 from an sRGB canvas, 0.007 from Display P3 and 0.010 from Rec. 2020 —
the sRGB figure is what twelve-bit quantisation costs on its own, and the other two are within it.

Animations and files with no `colr` box read back too, from the description AV1 carries in its own
sequence header. A container that states its colour still outranks that — MIAF makes the box
authoritative wherever one is present — so the bitstream answers only for a file that says nothing
else, which includes every plain sRGB AVIF this crate writes, since those carry no `colr` box at all.

An animation is read the same way, by the same rule. It states its colour in the sample entry inside
`stsd` rather than among the item properties a still image uses, and that is read too — so a
sequence whose container describes it, or whose container says nothing and whose bitstream does, both
arrive in the right space.

## Eight-bit canvases round the product once

On an eight-bit canvas, sRGB's included, a translucent colour drawn over
nothing is stored as the exact product of its colour and its alpha, rounded
once, on either engine. Chrome's CPU canvas rounds the colour and the alpha
to bytes first.

Over earlier content the backdrop's share is taken at the alpha rounded to a
byte, as Chrome's is, so the stored value can be a level off the exact
composite `c·a + d·(1 − a)` rounded once. `#dc2828` at `globalAlpha` 0.99 over
white is 220.35, 42.15 exactly and a fill stores 221, 43, 43: white's share at
252/255 is 3 where the exact share is 2.55. A drawn image, a canvas and a
`saveLayer` composite by a rule of their own and part from a fill by up to a
level. A float `colorType` composites in float and has neither rounding.
[browser-differences.md](browser-differences.md) has each rule, its error
over a sweep, and where they part from Chrome.
