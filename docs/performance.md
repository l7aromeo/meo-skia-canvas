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

The crate is where the sub-linearity is legible, because nothing there is at the floor: 16.7
microseconds against 39.8 for the same two sources, 2.38× for a hundredfold more work. Sub-linear
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
changelog, "A string costs less to measure again ...", which links the PRs.
Its figures are below, against 6.0.2: medians on an Apple M4 Pro, release
builds of 6.0.2 and of 78960e8e, four runs of each interleaved in pairs, 11
rounds a run, with no other build or test running. A time is the median of the
four runs' medians, and a ratio the median of the four pairs' ratios.

| what                                                                            | per  | 6.0.2    | 7.0.0    | 7.0.0 / 6.0.2 |
| ------------------------------------------------------------------------------- | ---- | -------- | -------- | ------------- |
| `measureText("Hello world")` in 16px, width read                                | call | 6.95 µs  | 3.92 µs  | 0.565×        |
| the same, width and the four ink values read                                    | call | 6.89 µs  | 5.69 µs  | 0.824×        |
| `fillText` of a label already drawn, recorded                                   | draw | 10.77 µs | 0.49 µs  | 0.046×        |
| `fillText` of a new string, its second draw                                     | draw | 8.81 µs  | 10.31 µs | 1.171×        |
| `fillText` of a string never drawn before                                       | draw | 8.83 µs  | 9.13 µs  | 1.030×        |
| 200 rects, each in its own hex colour, on an sRGB canvas, exported              | page | 200.4 µs | 192.0 µs | 0.956×        |
| the same, each in a `color(display-p3 …)`                                       | page | 296.7 µs | 234.3 µs | 0.795×        |
| a `fillRect` recorded                                                           | rect | 0.192 µs | 0.120 µs | 0.622×        |
| a 40×24 rect under `blur(4px)` recorded, CPU                                    | rect | 1.48 µs  | 0.94 µs  | 0.636×        |
| 2,000 of those on 800×600, exported, CPU                                        | page | 12.4 ms  | 4.41 ms  | 0.354×        |
| a 200×200 rect under `blur(2px)` on 800×600, drawn and read, GPU                | draw | 381.3 µs | 299.0 µs | 0.779×        |
| the same under `blur(100px)`, GPU                                               | draw | 374.5 µs | 305.1 µs | 0.818×        |
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
