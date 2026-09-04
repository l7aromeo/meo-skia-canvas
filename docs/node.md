# Node.js -- `meo-skia-canvas`

This document covers the Node addon path. For the Rust crate, see the project [README](../README.md) and [`docs/rust.md`](rust.md).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/hero-dark@2x.png">
  <img alt="meo-skia-canvas" src="assets/brand/hero@2x.png">
</picture>

<div align="center">
  <a href="getting-started.md">Getting Started</a> <span>&nbsp;&nbsp;·&nbsp;&nbsp;</span>
  <a href="https://www.jsdocs.io/package/meo-skia-canvas">Documentation</a> <span>&nbsp;&nbsp;·&nbsp;&nbsp;</span>
  <a href="../CHANGELOG.md">Release Notes</a>  <span>&nbsp;&nbsp;·&nbsp;&nbsp;</span>
  <a href="https://github.com/l7aromeo/meo-skia-canvas/issues">Discussion Forum</a>
</div>

> The links above point at the upstream skia-canvas project, whose API this package follows. Report
> issues specific to this fork at
> [l7aromeo/meo-skia-canvas](https://github.com/l7aromeo/meo-skia-canvas/issues) instead.

---

Skia Canvas is a Node.js implementation of the HTML Canvas drawing [API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API) for both on- and off-screen rendering. Since it uses Google's [Skia](https://skia.org) graphics engine, its output is very similar to Chrome's [`<canvas>`](https://html.spec.whatwg.org/multipage/canvas.html) element -- though it's also capable of things the browser's Canvas still can't achieve.

In particular, Skia Canvas:

- generates images in vector (PDF & SVG) as well as bitmap (JPEG, PNG, & WEBP) formats
- can draw to interactive GUI [windows][window] and provides a browser-like [event][win_bind] framework
- can save images to [files][toFile], encode to [dataURL][toURL] strings, and return [Buffers][toBuffer] or [Sharp][sharp] objects
- uses native threads in a [user-configurable][multithreading] worker pool for asynchronous rendering and file I/O
- can create [multiple 'pages'][newPage] on a given canvas and then [output][toFile] them as a single, multi-page PDF or an image-sequence saved to multiple files
- can [simplify][p2d_simplify], [blunt][p2d_round], [combine][bool-ops], [excerpt][p2d_trim], and [atomize][p2d_points] Bézier paths using [efficient](https://www.youtube.com/watch?v=OmfliNQsk88) boolean operations or point-by-point [interpolation][p2d_interpolate]
- provides [3D perspective][createProjection()] transformations in addition to [scaling][scale()], [rotation][rotate()], and [translation][translate()]
- can fill shapes with vector-based [Textures][createTexture()] in addition to bitmap-based [Patterns][createPattern()] and supports line-drawing with custom [markers][lineDashMarker]
- supports the full set of [CSS filter][filter] image processing operators
- offers rich typographic control including:
  - multi-line, [word-wrapped][textwrap] text
  - line-by-line [text metrics][c2d_measuretext]
  - small-caps, ligatures, and other opentype features accessible using standard [font-variant][fontvariant] syntax
  - proportional [letter-spacing][letterSpacing], [word-spacing][wordSpacing], and [leading][c2d_font]
  - support for [variable fonts][VariableFonts] and transparent mapping of weight values
  - use of non-system fonts [loaded][fontlibrary-use] from local files
- can be used for server-side image rendering on standard Linux hosts and 'serverless' platforms like Vercel and AWS Lambda

## Installation

If you're running on a supported platform, installation should be as simple as:

```bash
npm install meo-skia-canvas
```

This will download a pre-compiled library from the project's most recent [release](https://github.com/l7aromeo/meo-skia-canvas/releases).

### `pnpm`

If you use the `pnpm` package manager, it will not download `meo-skia-canvas`'s platform-native binary unless you explicitly allow it. You can do this interactively via the 'approve builds' command (note that you need to press `<space>` to toggle the selection and then `<enter>` to proceed):

```bash
pnpm install meo-skia-canvas
pnpm approve-builds
```

In non-interactive scenarios (like building via CI), you can approve the build step when you add `meo-skia-canvas` to your project:

```bash
pnpm install meo-skia-canvas --allow-build=meo-skia-canvas
```

Alternatively, you can add a [`pnpm.onlyBuiltDependencies`](https://pnpm.io/9.x/package_json#pnpmonlybuiltdependencies) entry to your `package.json` file to mark the build-step as allowed:

```json
{
  "pnpm": {
    "onlyBuiltDependencies": ["meo-skia-canvas"]
  }
}
```

## Platform Support

Skia Canvas runs on Linux, macOS, or Windows as well as serverless platforms like Vercel and AWS Lambda. Precompiled versions of the library's native code are downloaded automatically when you install it via npm: Linux and Windows on `x64` and `arm64`, macOS on `arm64` only. There is no Intel macOS build — an Intel Mac has to build from source.

The underlying Rust library uses [N-API][node_napi] v8, so it runs on every [currently supported](https://nodejs.org/en/about/previous-releases) Node.js release. **Node 22 is the floor** — that is what `engines` declares and what CI tests, since Node 20 reached end of life on 2026-04-30.

### Linux

The library is compatible with Linux systems using [glibc](https://www.gnu.org/software/libc/) 2.28 or later as well as Alpine Linux and the [musl](https://musl.libc.org) C library it favors. It will make use of the system's `fontconfig` settings in `/etc/fonts` if they exist but will otherwise fall back to using a [placeholder configuration](https://github.com/l7aromeo/meo-skia-canvas/blob/main/lib/fonts/fonts.conf), looking for installed fonts at commonly used Linux paths.

### Docker

If you are setting up a [Dockerfile](https://nodejs.org/en/docs/guides/nodejs-docker-webapp/) that uses [`node`](https://hub.docker.com/_/node) as its basis, the simplest approach is to set your `FROM` image to one of the (Debian-derived) defaults like `node:lts`, `node:22`, `node:24-bookworm`, or simply:

```dockerfile
FROM node
```

If you wish to use Alpine as the underlying distribution, you can start with something along the lines of:

```dockerfile
FROM node:alpine
```

### AWS Lambda

Skia Canvas depends on libraries that aren't present in the standard Lambda [runtime](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtimes.html). You can add these to your function by uploading a '[layer](https://docs.aws.amazon.com/lambda/latest/dg/chapter-layers.html)' (a zip file containing the required libraries and `node_modules` directory) and configuring your function to use it.

<details><summary>

**Detailed AWS instructions**

</summary>

#### Adding the Skia Canvas layer to your AWS account

1. Look in the **Assets** section of Skia Canvas's [current release](https://github.com/l7aromeo/meo-skia-canvas/releases/latest) and download the `aws-lambda-x64.zip` or `aws-lambda-arm64.zip` file (depending on your architecture) but don't decompress it
2. Go to the AWS Lambda [Layers console](https://console.aws.amazon.com/lambda/home/#/layers) and click the **Create Layer** button, then fill in the fields:

- **Name**: `meo-skia-canvas` (or whatever you want)
- **Description**: you might want to note the Skia Canvas version here
- **Compatible architectures**: select **x86_64** or **arm64** depending on which zip you chose
- **Compatible runtimes**: select **Node.js 22.x** (the oldest runtime this package supports)

3. Click the **Choose file** button and select the zip file you downloaded in Step 1, then click **Create**

Alternatively, you can use the [`aws` command line tool](https://github.com/aws/aws-cli) to create the layer. This bash script will fetch the meo-skia-canvas version of your choice and make it available to your Lambda functions.

```sh
#!/usr/bin/env bash
VERSION=5.7.0 # an example: any release from 4.1.0 on works, see the releases page for the latest
PLATFORM=arm64 # arm64 or x64

curl -sLO https://github.com/l7aromeo/meo-skia-canvas/releases/download/v${VERSION}/aws-lambda-${PLATFORM}.zip
aws lambda publish-layer-version \
    --layer-name "meo-skia-canvas" \
    --description "Skia Canvas ${VERSION} layer" \
    --zip-file "fileb://aws-lambda-${PLATFORM}.zip" \
    --compatible-runtimes "nodejs22.x" \
    --compatible-architectures "${PLATFORM/#x/x86_}"
```

#### Using the layer in a Lambda function

You can now use this layer in any function you create in the [Functions console](https://console.aws.amazon.com/lambda/home/#/functions). After creating a new function, click the **Add a Layer** button and you can select your newly created Skia Canvas layer from the **Custom Layers** layer source.

Note that the layer only includes Skia Canvas and its dependencies -- any other npm modules you want to use will need to be bundled into your function. To prevent the `meo-skia-canvas` module from being doubly-included, make sure you add it to the `devDependencies` section (**not** the regular `dependencies` section) of your package.json file.

</details>

### Next.js / Webpack

If you are using a framework like Next.js that bundles your server-side code with Webpack, you'll need to mark `meo-skia-canvas` as an 'external', otherwise its platform-native binary file will be excluded from the final build. Try adding these options to your `next.config.ts` file:

```js
const nextConfig: NextConfig = {
  serverExternalPackages: ['meo-skia-canvas'],
  webpack: (config, options) => {
    if (options.isServer){
      config.externals = [
        ...config.externals,
        {'meo-skia-canvas': 'commonjs meo-skia-canvas'},
      ]
    }
    return config
  }
};
```

## Compiling from Source

If prebuilt binaries aren't available for your system you'll need to compile the portions of this library that directly interface with Skia.

Start by installing:

1. A recent version of `git` (older versions have difficulties with Skia's submodules)
2. The [Rust compiler](https://www.rust-lang.org/tools/install) and cargo package manager using [`rustup`](https://rust-lang.github.io/rustup/)
3. A C compiler toolchain (either LLVM/Clang or MSVC)
4. Python 3 (used by Skia's [build process](https://skia.org/docs/user/build/))
5. The [Ninja](https://ninja-build.org) build system
6. On Linux: Fontconfig and OpenSSL

[Detailed instructions](https://github.com/rust-skia/rust-skia#building) for setting up these dependencies on different operating systems can be found in the 'Building' section of the Rust Skia documentation. The Dockerfiles in the [containers](https://github.com/l7aromeo/meo-skia-canvas/tree/main/containers) directory may also be useful for identifying needed dependencies. Once all the necessary compilers and libraries are present, running `npm run build` will give you a usable library (after a fairly lengthy compilation process).

## Development

This project uses [just](https://github.com/casey/just) as its command runner. **`just --list` is the list** — every recipe carries its own description, and a table here would be a copy that goes stale while the recipes move. Four are worth knowing before you read it:

- `just ci` is the full gate, and it is longer than it looks. Run it before opening a pull request.
- `just precommit` is the subset fast enough to sit in front of every commit, about six seconds. `just install-hooks` puts it there; it is opt-in and run once per clone.
- `just test` runs the suite **against your local build**. A bare `npm test` does not — an installed platform package outranks `lib/skia.node`, so Node loads the published binary instead.
- `just build` is a debug build and `just build-release` is what CI ships. Benchmark on the release one or not at all.

The two release channels are independent. `just release-npm` touches `package.json` only and leaves `Cargo.toml` alone; the crate is versioned separately by `just release-crate`. `bump` is whatever `npm version` accepts — `patch` (the default), `minor`, `major`, or a prerelease such as `just release-npm preminor --preid rc`. Prereleases publish to the `next` dist-tag, so a plain `npm install` is unaffected.

`release-npm` opens a **draft** release so CI can attach the platform binaries. Once that finishes, `just publish-npm` undrafts it and publishes the seven platform packages and the main one, in that order. Rehearse with `just publish-npm dry` first — it runs every guard for real without publishing anything.

## Global Settings

> There are a handful of settings that can only be configured at launch and will apply to all the canvases you create in your script. The sections below describe the different [environment variables][node_env] you can set to make global changes. You can either set them as part of your command line invocation, or place them in a `.env` file in your project directory and use Node 20's [`--env-file` argument][node_env_arg] to load them all at once.

### Multithreading

When rendering canvases in the background (e.g., by using the asynchronous [toFile][toFile] or [toBuffer][toBuffer] methods), tasks are spawned in a thread pool managed by the [rayon][rayon] library. By default it will create up to as many threads as your CPU has cores. You can see this default value by inspecting any [Canvas][canvas] object's [`engine.threads`][engine] property. If you wish to override this default, you can set the `SKIA_CANVAS_THREADS` environment variable to your preferred value.

For example, you can limit your asynchronous processing to two simultaneous tasks by running your script with:

```bash
SKIA_CANVAS_THREADS=2 node my-canvas-script.js
```

### Argument Validation

There are a number of situations where the browser API will react to invalid arguments by silently ignoring the method call rather than throwing an error. For example, these lines will simply have no effect:

```js
ctx.fillRect(0, 0, 100, "october");
ctx.lineTo(NaN, 0);
```

Skia Canvas does its best to emulate these quirks, but allows you to opt into a stricter mode in which it will throw TypeErrors in these situations (which can be useful for debugging).

Set the `SKIA_CANVAS_STRICT` environment variable to `1` or `true` to enable this mode.

## Example Usage

### Generating image files

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas;

let sweep = ctx.createConicGradient(Math.PI * 1.2, width / 2, height / 2);
sweep.addColorStop(0, "red");
sweep.addColorStop(0.25, "orange");
sweep.addColorStop(0.5, "yellow");
sweep.addColorStop(0.75, "green");
sweep.addColorStop(1, "red");
ctx.strokeStyle = sweep;
ctx.lineWidth = 100;
ctx.strokeRect(100, 100, 200, 200);

// render to multiple destinations using a background thread
async function render() {
  // save a 'retina' image...
  await canvas.toFile("rainbox.png", { density: 2 });
  // ...or use a shorthand for canvas.toBuffer("png")
  let pngData = await canvas.png;
  // ...or embed it in a string
  let pngEmbed = `<img src="${await canvas.toDataURL("png")}">`;
}
render();

// ...or write the file synchronously from the main thread
canvas.toFileSync("rainbox.pdf");
```

### Multi-page sequences

```js
import { Canvas } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas;

for (const color of ["orange", "yellow", "green", "skyblue", "purple"]) {
  ctx = canvas.newPage();
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, width, height);
  ctx.fillStyle = "white";
  ctx.arc(width / 2, height / 2, 40, 0, 2 * Math.PI);
  ctx.fill();
}

async function render() {
  // save to a multi-page PDF file
  await canvas.toFile("all-pages.pdf");

  // save to files named `page-01.png`, `page-02.png`, etc.
  await canvas.toFile("page-{2}.png");

  // just the middle three pages
  await canvas.toFile("middle.pdf", { pageRange: [2, 4] });
}
render();
```

`pageRange` is numbered from 1 and includes both ends, and negative numbers count from the end, so
`[2, -1]` is everything after the first page. It applies wherever a format gathers pages — PDF,
TIFF, ICO and the four animated formats — and to a filename template, which then writes only the
pages named.

For an animation that is worth splitting in two, it saves drawing the pages twice. A file carries
one loop count, so an introduction that plays once followed by a cycle that repeats forever cannot
be a single file; two calls over the same canvas give each half its own:

```js
const intro = await canvas.toBuffer("webp", {
  fps: 30,
  pageRange: [1, 20],
  loop: 1,
});
const cycle = await canvas.toBuffer("webp", {
  fps: 30,
  pageRange: [21, 60],
  loop: 0,
});
```

### Rendering to a window

```js
import { Window } from "meo-skia-canvas";

let win = new Window(300, 300);
win.title = "Canvas Window";
win.on("draw", (e) => {
  let ctx = e.target.canvas.getContext("2d");
  ctx.lineWidth = 25 + 25 * Math.cos(e.frame / 10);
  ctx.beginPath();
  ctx.arc(150, 150, 50, 0, 2 * Math.PI);
  ctx.stroke();

  ctx.beginPath();
  ctx.arc(150, 150, 10, 0, 2 * Math.PI);
  ctx.stroke();
  ctx.fill();
});
```

### Integrating with [Sharp.js][sharp]

```js
import sharp from "sharp";
import { Canvas, loadImage } from "meo-skia-canvas";

let canvas = new Canvas(400, 400),
  ctx = canvas.getContext("2d"),
  { width, height } = canvas,
  [x, y] = [width / 2, height / 2];

ctx.fillStyle = "red";
ctx.fillRect(0, 0, x, y);
ctx.fillStyle = "orange";
ctx.fillRect(x, y, x, y);

// Render the canvas to a Sharp object on a background thread then desaturate
await canvas
  .toSharp()
  .modulate({ saturation: 0.25 })
  .jpeg()
  .toFile("faded.jpg");

// Convert an ImageData to a Sharp object and save a grayscale version
let imgData = ctx.getImageData(0, 0, width, height, {
  matte: "white",
  density: 2,
});
await imgData.toSharp().grayscale().png().toFile("black-and-white.png");

// Create an image using Sharp then draw it to the canvas as an Image object
let sharpImage = sharp({
  create: { width: x, height: y, channels: 4, background: "skyblue" },
});
let canvasImage = await loadImage(sharpImage);
ctx.drawImage(canvasImage, x, 0);
await canvas.toFile("mosaic.png");
```

## Benchmarks

Measured on **2026-08-12**, Apple M4 Pro (14 cores) · macOS 26.6 · Node 26.4.0, against
`canvas@3.2.3`, `@napi-rs/canvas@1.0.5`, `canvaskit-wasm@0.41.1` and `skia-canvas@3.0.8`.

The harness is samizdatco's [canvas-benchmarks], run unmodified apart from adding this fork as its
own entry so upstream stays in the comparison. Each test is written once against a library-agnostic
adapter and drawn from a seeded RNG, so every library renders the identical scene; each measurement
runs in a fresh process. **One machine, one GPU** — the ordering transfers, the milliseconds do not.

Every figure is milliseconds per iteration, lower is better. `meo (async)` starts all iterations at
once and resolves them on the worker pool, which is the mode a server would use; the other columns
are serial.

|                     | canvaskit-wasm | canvas | @napi-rs/canvas | skia-canvas | meo-skia-canvas | meo (async) |
| ------------------- | -------------- | ------ | --------------- | ----------- | --------------- | ----------- |
| Simple house        | 16.0           | 14.7   | **13.3**        | 14.7        | 14.4            | 1.4         |
| Complex shapes      | **23.9**       | 69.0   | 43.8            | 34.0        | 34.0            | 3.5         |
| Bezier curves       | 403.2          | 438.7  | 202.1           | 126.3       | **124.2**       | 15.7        |
| Gradients           | 64.4           | 46.8   | **42.1**        | 44.8        | 44.7            | 4.5         |
| Basic text          | 17.1           | 21.7   | **16.9**        | 18.5        | 18.3            | 2.3         |
| SVG to PNG          | —              | 111.9  | 76.3            | **51.1**    | 51.8            | 5.7         |
| SVG to SVG          | —              | 30.1   | 31.3            | 3.3         | **3.2**         | 2.9         |
| SVG to PDF          | —              | 24.3   | —               | 5.1         | **4.8**         | 1.1         |
| Scale/rotate images | 139.8          | 265.7  | 105.1           | 92.9        | **89.6**        | 9.6         |
| Get/put ImageData   | —              | 63.7   | 62.3            | **55.2**    | 55.4            | 47.5        |

`—` marks a test the library cannot run: `canvaskit-wasm` has no SVG import or `ImageData`
round-trip, and `@napi-rs/canvas` does not export PDF. `canvaskit-wasm` wins Complex shapes with an
asterisk — it renders the shapes but positions them incorrectly.

**This fork tracks upstream `skia-canvas` within measurement noise**, between −6.4% and +1.3% across
the ten tests. That is the expected result and the reason there is no performance section here
claiming otherwise: this fork changed correctness, not hot paths. Where it differs from the other
libraries, it differs for the same reasons upstream does.

### Startup

|                 | first import |
| --------------- | ------------ |
| meo-skia-canvas | 13.9 ms      |
| canvaskit-wasm  | 17.7 ms      |
| canvas          | 40.3 ms      |
| @napi-rs/canvas | 74.6 ms      |

Upstream `skia-canvas` is absent from that table on purpose. The harness cannot measure it: its own
`src/format.js` imports `skia-canvas` at module scope, so the library is already in Node's module
cache before the timer starts and the test reports a cache hit — 0.35 ms here, against 15.35 ms for
a genuine first import in the same process. Measured directly instead, a fresh `require` of each
package's CommonJS entry costs **11.7–13.8 ms** for `skia-canvas` and **20.5–20.9 ms** for this
fork. The `dlopen` of the native binary is identical at 2.9 ms; the difference is this fork's larger
JavaScript module graph — the filter, shader and paragraph classes upstream does not ship. It
matters for short-lived processes such as Lambda and not at all for a long-running server.

For this library's own numbers — GPU against CPU, what a float `colorType` costs, encode times and
memory per canvas — run `just bench`, or see [Performance and memory] in the README.

[canvas-benchmarks]: https://github.com/samizdatco/canvas-benchmarks
[Performance and memory]: ../README.md#performance-and-memory

## Acknowledgements

This project is deeply indebted to the work of the [Rust Skia project](https://github.com/rust-skia/rust-skia) whose Skia bindings provide a safe and idiomatic interface to the mess of C++ that lies underneath. Many thanks to the developers of [node-canvas](https://github.com/Automattic/node-canvas) for their terrific set of unit tests. In the absence of an [Acid Test](https://www.acidtests.org) for canvas, these routines were invaluable.

### Notable contributors

- [@mpaparno](https://github.com/mpaparno) contributed support for SVG rendering, raw image-buffer handling, WEBP import/export and numerous bug fixes
- [@Salmondx](https://github.com/Salmondx) developed the initial Raw image loading & rendering routines
- [@lucasmerlin](https://github.com/lucasmerlin) helped get GPU rendering working on Vulkan
- [@cprecioso](https://github.com/cprecioso) & [@saantonandre](https://github.com/saantonandre) corrected and expanded upon the TypeScript type definitions
- [@meihuanyu](https://github.com/meihuanyu) contributed filter & path rendering fixes

## Copyright

© 2020-2026 [Samizdat Drafting Co.](https://samizdat.co) and contributors.

[bool-ops]: api/path2d.md#complement-difference-intersect-union-and-xor
[c2d_font]: api/context.md#font
[c2d_measuretext]: api/context.md#measuretext
[canvas]: https://www.jsdocs.io/package/meo-skia-canvas
[createProjection()]: api/context.md#createprojection
[createTexture()]: api/context.md#createtexture
[engine]: https://www.jsdocs.io/package/meo-skia-canvas
[fontlibrary-use]: https://www.jsdocs.io/package/meo-skia-canvas
[fontvariant]: api/context.md#fontvariant
[lineDashMarker]: api/context.md#linedashmarker
[newPage]: https://www.jsdocs.io/package/meo-skia-canvas
[p2d_interpolate]: api/path2d.md#interpolate
[p2d_points]: api/path2d.md#points
[p2d_round]: api/path2d.md#round
[p2d_simplify]: api/path2d.md#simplify
[p2d_trim]: api/path2d.md#trim
[toFile]: https://www.jsdocs.io/package/meo-skia-canvas
[textwrap]: api/context.md#textwrap
[toBuffer]: https://www.jsdocs.io/package/meo-skia-canvas
[toURL]: https://www.jsdocs.io/package/meo-skia-canvas
[win_bind]: https://www.jsdocs.io/package/meo-skia-canvas
[window]: https://www.jsdocs.io/package/meo-skia-canvas
[multithreading]: getting-started.md#multithreading
[node_napi]: https://nodejs.org/api/n-api.html#node-api-version-matrix
[node_env]: https://nodejs.org/en/learn/command-line/how-to-read-environment-variables-from-nodejs
[node_env_arg]: https://nodejs.org/dist/latest-v22.x/docs/api/cli.html#--env-fileconfig
[rayon]: https://crates.io/crates/rayon
[sharp]: https://sharp.pixelplumbing.com
[VariableFonts]: https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_Fonts/Variable_Fonts_Guide
[filter]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/filter
[letterSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/letterSpacing
[wordSpacing]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/wordSpacing
[createPattern()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/createPattern
[rotate()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/rotate
[scale()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/scale
[translate()]: https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/translate
