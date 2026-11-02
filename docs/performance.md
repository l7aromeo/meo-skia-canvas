# Performance and memory

What drawing, encoding, decoding and holding a canvas cost, and how to
measure it so the figures mean something. The changes that made a release
faster are one Changed entry in each changelog, and their figures are under
[What a release changed](#what-a-release-changed).

`just bench` runs [`examples/node/benchmark.js`](../examples/node/benchmark.js) through the Node
binding and [`examples/rust/benchmark.rs`](../examples/rust/benchmark.rs) through the crate, over the
same cases in the same order: drawing on the CPU and the GPU on sRGB, Display P3 and half-float
Display P3 pages, decoding and encoding every format, animations, PDF and SVG export, pixel reads,
antialiasing and memory. Each writes a dated Markdown and JSON report to `target/bench/` with the
median, p10 and p90 of every case and the binary, commit, machine and load it was taken on, and the
two line up row by row, so their ratio is what the binding costs. `just bench-npm` and
`just bench-crate` run one; `--quick` runs every case once as a smoke test, and `--only PREFIX` the
cases whose id starts with it. Figures are one machine — an Apple M4 Pro on Metal, 1200×900.
**Treat the ratios as the transferable part and the milliseconds as local colour.**

Both build release first, which matters: run the script directly and it measures whatever
`lib/skia.node` happens to be, and `just ci` ends by leaving a dev build there. The two differ by
more than a little — AVIF is 788 ms against 90 — and the report names the binary it measured.

**Drawing.** A mixed vector scene — 300 bezier strokes, 60 shadowed rounded panels, 40 lines of
text — takes 2.4 ms on the GPU against 4.1 on the CPU. What a float canvas costs runs in both
directions, which is why there is no single multiplier:

| workload (CPU)         | `RGBA8888` | `RGBAF16` | `RGBAF32` |
| ---------------------- | ---------- | --------- | --------- |
| mixed vector scene     | 4.2 ms     | 1.11×     | 1.15×     |
| 120 translucent layers | 6.4 ms     | **0.74×** | **0.75×** |
| 120 opaque fills       | 0.5 ms     | 1.34×     | **7.33×** |

Blending translucent layers is _faster_ in float: an eight-bit surface converts through its
transfer function on every layer and a float one does not. Opaque fills go the other way, and
`RGBAF32` falls off a cliff rather than scaling with its byte count. `RGBAF16` stays close to its
memory cost throughout, which makes it the one to reach for unless you need 32-bit precision.

**A canvas drawn into a canvas, behind a clip.** Only a source that has itself drawn a canvas is
rasterized at all; the cost of that is the clip's rather than the source's, so a hundredfold
heavier source does not cost a hundredfold more:

| ops in the source | cpu          | gpu          |
| ----------------- | ------------ | ------------ |
| 200               | 0.0 – 0.1 ms | 0.3 – 0.4 ms |
| 20,000            | 0.1 ms       | 0.3 – 0.4 ms |

**Read the ratio, not the tenths.** From JavaScript both pairs sit at the timer's floor, and the
spread above is five runs of the same build rather than a precision. What survives the noise is the
ratio between the two sources, which held between 1.41× and 1.64× on the cpu and between 1.01× and
1.21× on the gpu across those runs — a hundredfold heavier source costing about half as much again
on one backend and nothing measurable on the other. The gpu column is the higher and the flatter of
the two for the same reason: each round ends in a read, and waiting for the device costs more than
the drawing and swamps the difference between the sources.

The crate is where the sub-linearity is legible, because nothing there is at the floor: 16.6
microseconds against 43.5 for the same two sources, 2.62× for a hundredfold more work. Sub-linear
rather than flat — replaying the source still walks its picture to cull it.

**Encoding.** One page, and the same page as a thirty-frame animation with one moving element —
the four formats that carry a clock send only the rectangle each frame changed, so a still
background is compressed once rather than thirty times:

| format | one page |    size | 30 frames |    size |
| ------ | -------: | ------: | --------: | ------: |
| JPEG   |  13.3 ms |  802 KB |         — |       — |
| BMP    |  25.1 ms | 4219 KB |         — |       — |
| PDF    |  27.6 ms |  164 KB |         — |       — |
| PNG    |  45.5 ms | 1031 KB |         — |       — |
| SVG    |  46.8 ms |  175 KB |         — |       — |
| GIF    |  65.1 ms |  492 KB |  291.3 ms |  724 KB |
| WebP   |  72.6 ms |  378 KB |  195.9 ms |  570 KB |
| TIFF   |  81.9 ms | 1034 KB |         — |       — |
| APNG   |  82.9 ms | 1033 KB |  178.9 ms | 1535 KB |
| AVIF   |  91.1 ms |  566 KB |  720.0 ms | 1705 KB |

Neither column means much alone — the fastest encoder here writes the largest file and the slowest
writes the smallest. Five rows need a word. BMP is uncompressed, so it is the size of the raw
buffer. SVG's 46 ms is this scene, which is shadowed — a page SVG can describe whole takes 8 ms.

The other three are one idea three times. PNG, APNG and TIFF all sample the page and ask whether
storing a neighbour's difference makes the file smaller, because the answer is a property of the
drawing rather than of the format: on this page filtering does not pay, on a gradient it does not
either, and on a photograph it does. PNG then compresses at Skia's own level rather than a cheaper
one, which here is 45 ms and 1031 KB against 37 and 1090. A one-page APNG has no animation chunks
and so _is_ a PNG, which is why the two land within two kilobytes of each other; it costs more time
because it is this crate's writer rather than Skia's. TIFF is deflate with the same question asked
along the row instead of down the page.

Decoding: PNG 5.1 ms, JPEG 5.5, AVIF 71.1 — AVIF both ways is this library's own code, since Skia reads none
of it, and the decode is the one direction that is still single-threaded.

**Loading an SVG in** is a different path from writing one out, and almost none of its cost is
Skia's. Before Skia parses anything the source is walked once, to rewrite absolute lengths, text
positioning attributes, generic and registered families, and the `font-size` cascade:

| document      |   load | per element |
| ------------- | -----: | ----------: |
| 60 elements   | 0.1 ms |      2.1 µs |
| 2000 elements | 3.1 ms |      1.6 µs |

Read the per-element column across a change and the milliseconds as local colour. The two sizes
report nearly the same figure, which says the cost is linear and that fixed setup is already lost
in it at sixty elements — so both rising together is a uniform slowdown, and the large row rising
alone is something that scales worse than linearly.

AVIF is the interesting row, and it buys something: 566 KB at 41.7 dB PSNR where JPEG is 802 KB at
34.9 — smaller _and_ closer to the original. WebP lands at 378 KB and 25.5 dB, which is libwebp
targeting a perceptual metric rather than PSNR on the hardest case for it, antialiased diagonals
and small type. It used to be the slow one by a distance, at 237 ms; a page is now divided into
tiles the encoder can code in parallel, which is 90. Across frames it is still the slowest, and
that part is structural: AV1 predicts each frame from the one before it, so its frames genuinely
cannot be coded in parallel where the other three are. Its own dials move both axes at once:

| AVIF option    | time    | size    |     | AVIF option               | time    | size    |
| -------------- | ------- | ------- | --- | ------------------------- | ------- | ------- |
| `quality` 0.5  | 84.0 ms | 217 KB  |     | `chromaSampling: "4:2:2"` | 82.4 ms | 447 KB  |
| `quality` 0.92 | 89.9 ms | 566 KB  |     | `chromaSampling: "4:2:0"` | 78.2 ms | 372 KB  |
| `quality` 1.0  | 94.9 ms | 2021 KB |     | `lossless: true`          | 91.6 ms | 2365 KB |

Subsampling is cheaper _and_ smaller, but on text and flat panels it costs far more quality than
it saves bytes — right for a photograph, wrong for a chart, hence the `"4:4:4"` default.

**Memory** is 4, 8 and 16 bytes a pixel for a surface — but a canvas only pays that when
something makes it draw a whole one. Reads composite the tiles they touch, so a canvas read one
pixel at a time holds a fraction of its surface, and one read whole holds more than it:

| resident per canvas | read one pixel | read whole page | a full surface |
| ------------------- | -------------: | --------------: | -------------: |
| `RGBA8888`          |         0.3 MB |            9 MB |        4.12 MB |
| `RGBAF16`           |         0.6 MB |           14 MB |        8.24 MB |
| `RGBAF32`           |         1.1 MB |           23 MB |       16.48 MB |

The middle column runs past the surface because a whole-page read materializes the surface _and_
hands a copy of it to the caller, so the arithmetic is paid twice over.

Twenty 1200×900 canvases held at once, resident memory either side of the loop, in a process that
does nothing else, median of three. The right-hand column is arithmetic — width × height × bytes a
pixel — and the other two are measurements, quoted to the digit they hold still in: three runs of
the `RGBAF16` whole-page figure gave 12.9, 13.3 and 13.7 MB. It needs repeating before it is
believed, either way: a single pass reads whatever the allocator happened to do, and has come back
at 2.91 MB for the eight-bit case and at a negative number for `RGBAF32`.

**Antialiasing coverage is where the GPU and the CPU disagree**, and neither GPU path matches the
raster one. A rectangle narrower than a pixel should darken it in proportion to how much of it the
rectangle covers, which is arithmetic rather than taste, so each renderer can be scored against it.
Sweeping the width from 0.05 to 1 pixel: the CPU renderer is exact to within a level; 4𝗑 MSAA
quantizes to quarters — 0, 64, 127, 191, 255 at 0.05, 0.25, 0.5, 0.75 and 1 pixel — so a shape
thinner than half a sample drops out entirely; shader-based AA is smooth but computes coverage from
a distance field and reads systematically low, putting 159 where a half-covered black edge should
read 127. Total error over the sweep runs 10, 307 and 423 levels respectively. The quantization is
a property of the sample count rather than of the driver; the totals are this machine. The default
is the closer of the two GPU options; if coverage has to match the CPU exactly, render on the CPU.

Two caveats. **Benchmark on a release build or not at all.** Most rows barely move — that work is
inside Skia and is optimized either way — but AVIF is **788 ms on a dev build against 90 on
release** and GIF **3881 against 63**, because both reach their codec through this crate's own
per-pixel work, and that is Rust: a colour conversion for one, a k-means palette for the other. And **the GPU row is the least reproducible**, moving between 2.9 and 3.6 ms
across runs where the CPU row held between 4.3 and 4.7.

## What a release changed

The changes that made 7.0.0 and 0.17.0 faster are one entry in each
changelog, "Text costs less to lay out, draw and measure ...", which links
the PRs. Its figures are timed in the release-prep window and kept here:

[RE-TIME at release prep: the kept-line, unchanged-setting, colour-lookup,
blurred-shape and GPU-read figures from that entry, against 6.0.2.]

Three of its changes took a page of many filtered draws or layers from time
growing with the square of their count to time growing with the count. As
measured in their pull requests, before the release-prep timing:

- An F16 or F32 canvas outside sRGB filters what a filtered draw covers, not
  the whole canvas: 50 rects under a drop shadow drew in about 26 ms rather
  than 1.7 s on F16 Display P3
  ([#462](https://github.com/l7aromeo/meo-skia-canvas/pull/462)).
- On the GPU, 16,384 rects under a drop shadow drew and read back in 1.3 s
  rather than 31 s ([#465](https://github.com/l7aromeo/meo-skia-canvas/pull/465)).
- On the GPU, 16,384 `saveLayer` groups of one rect drew and read back in
  0.43 s rather than 10.7 s
  ([#469](https://github.com/l7aromeo/meo-skia-canvas/pull/469)).
