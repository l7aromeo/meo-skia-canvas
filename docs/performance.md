# Performance and memory

What drawing, encoding, decoding and holding a canvas cost, and how to
measure it so the figures mean something. The changes that made a release
faster are one Changed entry in each changelog, and their figures are under
[What a release changed](#what-a-release-changed).

`just bench` runs [`examples/node/benchmark.js`](../examples/node/benchmark.js) through the Node
binding and [`examples/rust/benchmark.rs`](../examples/rust/benchmark.rs) through the crate, over the
same cases in the same order: drawing on the CPU and the GPU on sRGB, Display P3, half-float
Display P3 and half-float and 32-bit float sRGB pages, decoding and encoding every format,
animations, PDF and SVG export, pixel reads, antialiasing and memory. Each writes a dated Markdown and JSON report to `target/bench/` with the
median, p10 and p90 of every case and the binary, commit, machine and load it was taken on, and the
two line up row by row, so their ratio is what the binding costs. `just bench-npm` and
`just bench-crate` run one; `--quick` runs every case once as a smoke test, and `--only PREFIX` the
cases whose id starts with it. Figures are one machine — an Apple M4 Pro on Metal, 1200×900,
macOS 27.2. Down to [What a release changed](#what-a-release-changed), which says where its own figures are from, the npm figures are the medians of five `just bench-npm` passes on c54ae873 (2026-11-06), binary md5 `8819fb77523b31a3df6639012d83f4f2`, and the crate's are one `just bench` pass on 22737b25, whose source is c54ae873's.
**Treat the ratios as the transferable part and the milliseconds as local colour.**

Both build release first, which matters: run the script directly and it measures whatever
`lib/skia.node` happens to be, and `just ci` ends by leaving a dev build there. The two differ by
more than a little — AVIF is 748 ms against 87 — and the report names the binary it measured.

**Drawing.** A mixed vector scene — 300 bezier strokes, 60 shadowed rounded panels, 40 lines of
text — takes 2.8 ms on the GPU against 4.6 on the CPU. What a float canvas costs runs in both
directions, which is why there is no single multiplier:

| workload (CPU)         | `RGBA8888` | `RGBAF16` | `RGBAF32` |
| ---------------------- | ---------- | --------- | --------- |
| mixed vector scene     | 4.6 ms     | 1.00×     | 1.03×     |
| 120 translucent layers | 6.4 ms     | **0.73×** | **0.74×** |
| 120 opaque fills       | 0.5 ms     | 1.29×     | **6.52×** |

Blending translucent layers is _faster_ in float: an eight-bit surface converts through its
transfer function on every layer and a float one does not. Opaque fills go the other way, and
`RGBAF32` falls off a cliff rather than scaling with its byte count. `RGBAF16` stays close to its
memory cost throughout, which makes it the one to reach for unless you need 32-bit precision.

**A canvas drawn into a canvas, behind a clip.** Only a source that has itself drawn a canvas is
rasterized at all; the cost of that is the clip's rather than the source's, so a hundredfold
heavier source does not cost a hundredfold more:

| ops in the source | cpu            | gpu            |
| ----------------- | -------------- | -------------- |
| 200               | 0.04 – 0.05 ms | 0.35 – 0.40 ms |
| 20,000            | 0.06 – 0.07 ms | 0.38 – 0.42 ms |

**Read the ratio, not the tenths.** From JavaScript both pairs sit at the timer's floor, and the
spread above is five runs of the same build rather than a precision. What survives the noise is the
ratio between the two sources, which held between 1.28× and 1.57× on the cpu and between 1.00× and
1.09× on the gpu across those runs — a hundredfold heavier source costing between a quarter and a
half as much again on one backend and next to nothing on the other. The gpu column is the higher and the flatter of
the two for the same reason: each round ends in a read, and waiting for the device costs more than
the drawing and swamps the difference between the sources.

The crate is where the sub-linearity is legible, because nothing there is at the floor: 16.5
microseconds against 41.1 for the same two sources, 2.50× for a hundredfold more work. Sub-linear
rather than flat — replaying the source still walks its picture to cull it.

**Encoding.** One page, and the same page as a thirty-frame animation with one moving element —
the four formats that carry a clock send only the rectangle each frame changed, so a still
background is compressed once rather than thirty times:

| format | one page |    size | 30 frames |    size |
| ------ | -------: | ------: | --------: | ------: |
| JPEG   |  13.5 ms |  802 KB |         — |       — |
| BMP    |  25.5 ms | 4219 KB |         — |       — |
| PDF    |  28.1 ms |  164 KB |         — |       — |
| PNG    |  46.7 ms | 1032 KB |         — |       — |
| SVG    |  46.8 ms |  174 KB |         — |       — |
| GIF    |  50.5 ms |  407 KB |  180.4 ms |  627 KB |
| WebP   |  70.8 ms |  378 KB |  189.9 ms |  570 KB |
| AVIF   |  87.0 ms |  566 KB |  693.1 ms | 1705 KB |
| TIFF   |  89.9 ms | 1033 KB |         — |       — |
| APNG   |  94.2 ms | 1032 KB |  186.3 ms | 1534 KB |

Neither column means much alone — the two do not rank the formats alike, and BMP, the second
fastest, writes the largest file. Five rows need a word. BMP is uncompressed, so it is the size of
the raw buffer. SVG's 47 ms is this scene, which is shadowed; a heavier page, with a pattern,
blurred shapes and gradients, takes 164 ms (`export/svg-heavy/cpu`).

The other three are one idea three times. PNG, APNG and TIFF all sample the page and ask whether
storing a neighbour's difference makes the file smaller, because the answer is a property of the
drawing rather than of the format: on this page filtering does not pay, on a gradient it does not
either, and on a photograph it does. PNG then compresses at Skia's own level rather than a cheaper
one, trading time for bytes. A one-page APNG has no animation chunks
and so _is_ a PNG, which is why the two land within two kilobytes of each other; it costs more time
because it is this crate's writer rather than Skia's. TIFF is deflate with the same question asked
along the row instead of down the page.

Decoding: PNG 4.4 ms, JPEG 5.3, AVIF 67.5 — AVIF both ways is this library's own code, since Skia reads none
of it, and the decode is the one direction that is still single-threaded.

**Loading an SVG in** is a different path from writing one out, and almost none of its cost is
Skia's. Before Skia parses anything the source is walked once, to rewrite absolute lengths, text
positioning attributes, generic and registered families, and the `font-size` cascade:

| document      |    load | per element |
| ------------- | ------: | ----------: |
| 60 elements   | 0.14 ms |      2.3 µs |
| 2000 elements |  3.6 ms |      1.8 µs |

Read the per-element column across a change and the milliseconds as local colour. The two sizes
report nearly the same figure, which says the cost is linear and that fixed setup is already lost
in it at sixty elements — so both rising together is a uniform slowdown, and the large row rising
alone is something that scales worse than linearly.

AVIF is the interesting row, and it buys something: 566 KB at 41.7 dB PSNR where JPEG is 802 KB at
34.9 — smaller _and_ closer to the original. WebP lands at 378 KB and 25.5 dB, which is libwebp
targeting a perceptual metric rather than PSNR on the hardest case for it, antialiased diagonals
and small type. It used to be the slow one by a distance, at 237 ms; a page is now divided into
tiles the encoder can code in parallel, which is 87. Across frames it is still the slowest, and
that part is structural: AV1 predicts each frame from the one before it, so its frames genuinely
cannot be coded in parallel where the other three are. Its own dials move the size far more than
the time:

| AVIF option    | time    | size    |     | AVIF option               | time    | size    |
| -------------- | ------- | ------- | --- | ------------------------- | ------- | ------- |
| `quality` 0.5  | 84.4 ms | 216 KB  |     | `chromaSampling: "4:2:2"` | 78.9 ms | 447 KB  |
| `quality` 0.92 | 87.0 ms | 566 KB  |     | `chromaSampling: "4:2:0"` | 74.5 ms | 372 KB  |
| `quality` 1.0  | 83.8 ms | 2022 KB |     | `lossless: true`          | 88.2 ms | 2367 KB |

Subsampling is cheaper _and_ smaller, but on text and flat panels it costs far more quality than
it saves bytes — right for a photograph, wrong for a chart, hence the `"4:4:4"` default.

**Memory** is 4, 8 and 16 bytes a pixel for a surface — but a canvas only pays that when
something makes it draw a whole one. Reads composite the tiles they touch, so a canvas read one
pixel at a time holds a fraction of its surface, and one read whole holds more than it:

| resident per canvas | read one pixel | read whole page | a full surface |
| ------------------- | -------------: | --------------: | -------------: |
| `RGBA8888`          |         0.4 MB |            7 MB |        4.12 MB |
| `RGBAF16`           |         0.6 MB |           11 MB |        8.24 MB |
| `RGBAF32`           |         1.1 MB |           20 MB |       16.48 MB |

The middle column runs past the surface because a whole-page read materializes the surface _and_
hands a copy of it to the caller, so the arithmetic is paid twice over.

Twenty 1200×900 canvases held at once, resident memory either side of the loop, in a process that
does nothing else, median of three. The right-hand column is arithmetic — width × height × bytes a
pixel — and the other two are measurements, quoted to the digit they hold still in: five passes of
the `RGBAF16` whole-page figure gave 10.86 to 10.87 MB. It needs repeating before it is
believed, either way: a single pass reads whatever the allocator happened to do, and has come back
at 2.91 MB for the eight-bit case and at a negative number for `RGBAF32`.

**Antialiasing coverage is where the GPU and the CPU disagree**, and neither GPU path matches the
raster one. A rectangle narrower than a pixel should darken it in proportion to how much of it the
rectangle covers, which is arithmetic rather than taste, so each renderer can be scored against it.
Sweeping the width from 0.05 to 1 pixel: the CPU renderer is exact to within a level; 4𝗑 MSAA
quantizes to quarters — 0, 64, 127, 191, 255 at 0.05, 0.25, 0.5, 0.75 and 1 pixel — so a shape
thinner than half a sample drops out entirely; shader-based AA is smooth but computes coverage from
a distance field and reads systematically low, putting 159 where a half-covered black edge should
read 127. Total error over the sweep runs 9.5, 307 and 422.5 levels respectively. The quantization is
a property of the sample count rather than of the driver; the totals are this machine. The default
is the closer of the two GPU options; if coverage has to match the CPU exactly, render on the CPU.

Two caveats. **Benchmark on a release build or not at all.** Most rows barely move — that work is
inside Skia and is optimized either way — but AVIF is **748 ms on a dev build against 87 on
release** and GIF **2112 against 51**, because both reach their codec through this crate's own
per-pixel work, and that is Rust: a colour conversion for one, a k-means palette for the other. And **the GPU row moves more between runs than the CPU row**, between 2.74 and 2.91 ms
across the five passes, where the CPU row held between 4.53 and 4.65.

## What a release changed

The changes that made 7.0.0 and 0.17.0 faster are one entry in each
changelog, "A string costs less to measure again ...", which links the PRs.
Its figures are below, against 6.0.2: medians on an Apple M4 Pro, release
builds of 6.0.2 and of 78960e8e, except a new string's first and second draws,
which are of c54ae873, after
[#603](https://github.com/l7aromeo/meo-skia-canvas/pull/603) changed what laying
a string out costs. Four runs of each build are interleaved in pairs, 11 rounds
a run, with no other build or test running. A time is the median of the
four runs' medians, and a ratio the median of the four pairs' ratios.

| what                                                                            | per  | 6.0.2    | 7.0.0    | 7.0.0 / 6.0.2 |
| ------------------------------------------------------------------------------- | ---- | -------- | -------- | ------------- |
| `measureText("Hello world")` in 16px, width read                                | call | 6.95 µs  | 3.92 µs  | 0.565×        |
| the same, width and the four ink values read                                    | call | 6.89 µs  | 5.69 µs  | 0.824×        |
| `fillText` of a label already drawn, recorded                                   | draw | 10.77 µs | 0.49 µs  | 0.046×        |
| `fillText` of a new string, its second draw                                     | draw | 8.69 µs  | 9.79 µs  | 1.131×        |
| `fillText` of a string never drawn before                                       | draw | 8.66 µs  | 8.76 µs  | 1.009×        |
| 200 rects, each in its own hex colour, on an sRGB canvas, exported              | page | 200.4 µs | 192.0 µs | 0.956×        |
| the same, each in a `color(display-p3 …)`                                       | page | 296.7 µs | 234.3 µs | 0.795×        |
| a `fillRect` recorded                                                           | rect | 0.192 µs | 0.120 µs | 0.622×        |
| a 40×24 rect under `blur(4px)` recorded, CPU                                    | rect | 1.48 µs  | 0.94 µs  | 0.636×        |
| 2,000 of those on 800×600, exported, CPU                                        | page | 12.4 ms  | 4.41 ms  | 0.354×        |
| 500 15×12 rects under `drop-shadow(2px 2px 3px black)`, 400×300 sRGB, read once | rect | 22.48 µs | 8.06 µs  | 0.358×        |

A string's second draw costs more than in 6.0.2, and its first about the same.
The entry's claim for drawing again starts at the third draw.

Three of its changes took a page of many filtered draws or groups from time
growing with the square of their count to time growing with the count. Per
draw, measured the same way, at two page sizes each:

| page                                                              | draws a page | 6.0.2    | 7.0.0    |
| ----------------------------------------------------------------- | ------------ | -------- | -------- |
| 15×12 rects under a drop shadow, F16 Display P3, 400×300 ([#462]) | 50           | 29.02 µs | 16.47 µs |
|                                                                   | 500          | 24.59 µs | 14.73 µs |
| 3×3 rects under a drop shadow, GPU, 520×520 ([#465])              | 4,096        | 232.2 µs | 105.5 µs |
|                                                                   | 16,384       | 1,876 µs | 104.4 µs |
| `saveLayer` groups of one 3×3 rect, GPU, 520×520 ([#469])         | 4,096        | 158.2 µs | 26.2 µs  |
|                                                                   | 16,384       | 680.4 µs | 26.2 µs  |

7.0.0's cost a draw holds as a page grows; 6.0.2's grows with it on the GPU.
A page of 16,384 rects under a drop shadow takes about 1.7 s against 31 s, and
16,384 groups 0.43 s against 11.1 s. On the F16 page 6.0.2's cost a draw holds
too: #462 measured its 26 ms against 1.7 s for 50 rects against its parent
commit, not against 6.0.2.

[#462]: https://github.com/l7aromeo/meo-skia-canvas/pull/462
[#465]: https://github.com/l7aromeo/meo-skia-canvas/pull/465
[#469]: https://github.com/l7aromeo/meo-skia-canvas/pull/469

**A read of a GPU canvas** waits on the GPU once
([#497](https://github.com/l7aromeo/meo-skia-canvas/issues/497)): the copy is
asked for through Skia's asynchronous read, and the one wait that answers for
the copy answers for the draws too. On Metal the draws are committed first, in
a command buffer of their own, and the status of every buffer Skia committed is
read after that wait; elsewhere the copy goes in the buffer the draws are in.

What happens to the bytes then depends on the read:

- **Unpremultiplied from an eight-bit page**, which is what `getImageData` asks
  of an eight-bit canvas, is converted through the exact table, never through
  Skia's conversion, which rounds down. On the GPU engine, from 160×120 pixels,
  the table is looked up on the GPU, by a pass drawn into a target of its own
  whose bytes are read as they stand. A smaller read converts on the CPU as each
  row is copied out, on one thread, and across the thread pool from 512×512.
- **A read that converts nothing** is copied as Skia hands it back, on one
  thread up to a mebibyte, across the pool above that.
- **The buffer the copy lands in** is kept and reused by the next read of its
  size, in the npm package's macOS and Linux binaries. A crate build's Skia,
  which this repository does not patch, makes one for every read.

Measured the same way, a fill and then a read of the GPU canvas:

| read after a draw                         | 6.0.2    | 7.0.0    | 7.0.0 / 6.0.2 |
| ----------------------------------------- | -------- | -------- | ------------- |
| 8x8, of a 1200×900 canvas                 | 195.2 µs | 175.4 µs | 0.908×        |
| all of a 64x48 canvas                     | 229.9 µs | 195.6 µs | 0.844×        |
| all of a 160x120 canvas, the pass's least | 252.2 µs | 239.4 µs | 0.952×        |
| all of a 1200×900 canvas                  | 521.8 µs | 388.9 µs | 0.743×        |
| the same, premultiplied                   | 908.0 µs | 367.9 µs | 0.405×        |
| 8x8 again, with nothing drawn since       | 2.13 µs  | 1.93 µs  | 0.905×        |

The draws go first because of Apple's GPU clock. Read after read, a copy
committed in the buffer its draws are in holds the GPU at its lowest state, and
the same passes take longer for it; with the draws first it rises.
powermetrics' `gpu_power` sampler, every 50 ms over 20 s of back-to-back whole
1200x900 reads after an 8x8 draw, measured when the draws were moved to a
buffer of their own:

| whole 1200x900 reads         | median read | GPU active frequency, median (p10–p90) |
| ---------------------------- | ----------- | -------------------------------------- |
| draws and copy in one buffer | 724 µs      | 338 MHz (338–338)                      |
| draws first                  | 684 µs      | 426 MHz (351–474)                      |
| 6.0.2                        | 620 µs      | 672 MHz (482–763)                      |

7.0.2 and 0.17.2 have one speed entry in each changelog, "Drawing under
`blur()` costs less on the CPU", which links the PRs. Its figures are below,
against 6.0.2: medians on an Apple M4 Pro, release builds of the npm package
at 6.0.2 and at f86212f3. Six runs of each build, the order rotated, 11 rounds
a run, each run a process of its own. A time is the median of the six runs'
medians, and a ratio the ratio of those medians. The same build of f86212f3,
run again as a third arm, came within 2.2% of the first on every row.

| what                                                                   | per  | 6.0.2    | 7.0.2    | 7.0.2 / 6.0.2 |
| ---------------------------------------------------------------------- | ---- | -------- | -------- | ------------- |
| 50 15×12 ellipses under `blur(3px)`, black, 400×300, read once, CPU    | draw | 7.50 µs  | 6.10 µs  | 0.813×        |
| 500 of them                                                            | draw | 3.76 µs  | 3.06 µs  | 0.814×        |
| 50 in colour                                                           | draw | 7.57 µs  | 6.08 µs  | 0.803×        |
| 500 in colour                                                          | draw | 3.79 µs  | 3.04 µs  | 0.802×        |
| 50 `fillText` of 40px "Ag" under `blur(1px)`, 400×300, read whole, CPU | page | 248.4 µs | 193.6 µs | 0.779×        |
| the same under `blur(3px)`                                             | page | 268.7 µs | 207.9 µs | 0.774×        |
| 20px under `blur(3px)`                                                 | page | 246.1 µs | 181.5 µs | 0.738×        |
| 80px under `blur(3px)`                                                 | page | 308.0 µs | 272.7 µs | 0.885×        |
| 40px under `blur(3px)`, each draw moved by a fraction of a pixel       | page | 271.1 µs | 234.5 µs | 0.865×        |
| 40px under `blur(10px)`                                                | page | 343.2 µs | 1,072 µs | 3.12×         |

Text under a blur of 5 device pixels or more takes the filter chain rather
than the coverage route the entry's changes reach, and costs about three times
what it did in 6.0.2 ([#660]).

[#660]: https://github.com/l7aromeo/meo-skia-canvas/issues/660
