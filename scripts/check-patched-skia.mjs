#!/usr/bin/env node
// Whether a build compiled Skia from the patched source, read from what
// skia-bindings' build script printed.
//
//   node scripts/check-patched-skia.mjs <target/<profile>/build>
//   node scripts/check-patched-skia.mjs --self-test
//
// rust-skia's prebuilt Skia waits one second for each Metal shader compile
// and drops the draw when it runs out, its atlas path renderer can move a
// pixel of a path's coverage a step up or down on Metal and on Vulkan, and its
// triangulating path renderer reuses a finer triangulation a larger draw of a
// curve left, and its asynchronous read allocates a fresh transfer buffer for
// every `getImageData`; a binary built against it passes every test that does
// not stall the compiler, land a path where the pixel moves, draw a curve at
// two scales or look at which buffer a read kept. Three things have to hold,
// and each is read from skia-bindings' `output`:
//
// 1. It built from a source tree and did not try a download: offline from
//    `SKIA_SOURCE_DIR` -- "STARTING OFFLINE BUILD" -- or, from a rust-skia
//    checkout, which has no crates.io package hash to download against, from
//    the tree inside it -- "STARTING A FULL BUILD".
// 2. ninja compiled each file one of this platform's patches changes -- see
//    `PATCHED`. A build without a GPU backend never compiles them, and has
//    nothing for this to vouch for.
// 3. Each file it compiled, resolved against the directory ninja ran in, is
//    the patched one: it holds what only the patch puts there. A build from
//    an unpatched tree passes the first two and fails this.
//
// The self-test also holds each marker to its patch: it must be in what the
// patch writes and nowhere in what it replaces or keeps, or an unpatched file
// would pass the third check.
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// containers/skia-atlas-size.patch, on every platform.
const ATLAS_SIZE = {
  patch: "containers/skia-atlas-size.patch",
  file: "src/gpu/ganesh/ops/AtlasPathRenderer.cpp",
  marker: "fAtlasMaxSize = 512;",
};

/// containers/skia-triangulation-tolerance.patch, on every platform.
const TRIANGULATION = {
  patch: "containers/skia-triangulation-tolerance.patch",
  file: "src/gpu/ganesh/ops/TriangulatingPathRenderer.cpp",
  marker: "info->fTolerance == tol",
};

/// containers/skia-async-transfer-reuse.patch, on macOS and Linux. The
/// marker names the enum's scope because the unpatched file already holds a
/// bare `kDynamic_GrAccessPattern`, in the comment the patch replaces.
const ASYNC_TRANSFER_REUSE = {
  patch: "containers/skia-async-transfer-reuse.patch",
  file: "src/gpu/ganesh/SurfaceContext.cpp",
  marker: "GrAccessPattern::kDynamic_GrAccessPattern",
};

/// Each file a patch for the platform changes, under Skia's source root, with
/// what only the patched file contains, and the patch that puts it there.
/// `just check-skia-patches` refuses a patch in `PATCHES` that is neither
/// here nor in `UNVERIFIED`.
export const PATCHED = {
  darwin: [
    {
      patch: "containers/skia-metal-untimed-compile.patch",
      file: "src/gpu/ganesh/mtl/GrMtlUtil.mm",
      marker: "GrMtlCompilesWithoutTimeout",
    },
    ATLAS_SIZE,
    TRIANGULATION,
    ASYNC_TRANSFER_REUSE,
  ],
  linux: [ATLAS_SIZE, TRIANGULATION, ASYNC_TRANSFER_REUSE],
  win32: [
    ATLAS_SIZE,
    TRIANGULATION,
    {
      patch: "containers/skia-dwrite-oblique.patch",
      file: "src/ports/SkTypeface_win_dw.cpp",
      marker: "fsSelection.field.Oblique",
    },
  ],
};

/// The patches a build's output cannot vouch for, and why. Each still fails
/// its build loudly without it, or changes nothing a build compiles.
export const UNVERIFIED = {
  "containers/skia-brotli-sources.patch":
    "it changes a build list rather than a compiled file, and a `freetype` build without it fails to link",
  "containers/rust-skia-graphite-bindings.patch":
    "it changes rust-skia's bindings rather than Skia, and `cfg(meo_graphite)` is what says whether a build took it",
};

/**
 * The ninja line that names `file` compiled, capturing the path it gave: with
 * either separator, since one on Windows may give `\`.
 */
function compiledLine(file) {
  const escaped = file
    .split("/")
    .map((part) => part.replace(/\./g, "\\."))
    .join("[\\\\/]");
  return new RegExp(`compile (\\S+[\\\\/]${escaped})`);
}
// A CRLF line end needs nothing here: `$` in multiline mode matches before
// `\r` as before `\n`, both being line terminators to JavaScript.
const NINJA_DIR = /^ninja: Entering directory `(.*)'$/m;

const FROM_SOURCE = ["STARTING OFFLINE BUILD", "STARTING A FULL BUILD"];

/**
 * Why `output` does not show Skia built with `patched`, or `null` when it
 * does.
 */
export function refusal(output, patched) {
  if (!FROM_SOURCE.some((line) => output.includes(line)))
    return "skia-bindings did not build from a source tree";
  if (output.includes("DOWNLOAD AND INSTALL"))
    return "skia-bindings tried to download prebuilt Skia";
  const ninja = output.match(NINJA_DIR)?.[1];
  for (const { file, marker } of patched) {
    const compiled = output.match(compiledLine(file))?.[1];
    if (!compiled) return `ninja did not compile ${file}`;
    if (!ninja) return "the output does not say where ninja ran";
    // `/` resolves on every platform, where `\` is a separator on Windows
    // alone.
    const path = resolve(ninja, compiled.replaceAll("\\", "/"));
    if (!existsSync(path)) return `the compiled ${path} is gone`;
    if (!readFileSync(path, "utf8").includes(marker))
      return `${path} is not the patched source`;
  }
  return null;
}

/// Where a patch file names Skia's sources: rust-skia's checkout keeps them
/// under `skia-bindings/skia`.
const SKIA_IN_RUST_SKIA = "skia-bindings/skia/";

/**
 * Why `marker` does not show that `patchText` was applied to `file`, or
 * `null` when it does: the marker has to be in the lines the patch writes
 * there and in none it replaces or keeps as context, which is all of the
 * unpatched file a patch shows.
 */
export function markerRefusal(patchText, file, marker) {
  const sections = patchText.split(/^(?=--- )/m);
  const section = sections.find((text) =>
    text.startsWith(`--- a/${SKIA_IN_RUST_SKIA}${file}\n`),
  );
  if (!section) return `the patch does not change ${file}`;
  const lines = section
    .split("\n")
    .filter((line) => !/^(---|\+\+\+|@@)/.test(line));
  const before = lines.filter((line) => !line.startsWith("+")).join("\n");
  const after = lines.filter((line) => !line.startsWith("-")).join("\n");
  if (!after.includes(marker)) return `the patch does not write ${marker}`;
  if (before.includes(marker))
    return `the unpatched ${file} already holds ${marker}`;
  return null;
}

function check(buildDir) {
  const patched = PATCHED[process.platform];
  if (!patched) {
    console.error(`check-patched-skia: no patches for ${process.platform}`);
    process.exit(1);
  }
  const outputs = readdirSync(buildDir)
    .filter((name) => name.startsWith("skia-bindings-"))
    .map((name) => join(buildDir, name, "output"))
    .filter(existsSync);
  if (!outputs.length) {
    console.error(
      `check-patched-skia: no skia-bindings output under ${buildDir}`,
    );
    process.exit(1);
  }
  const problems = outputs
    .map((path) => [path, refusal(readFileSync(path, "utf8"), patched)])
    .filter(([, why]) => why);
  for (const [path, why] of problems) console.error(`${path}: ${why}`);
  if (problems.length) process.exit(1);
  console.log(
    `check-patched-skia: ${outputs.length} skia-bindings build(s) compiled the patched Skia source`,
  );
}

/** The self-test's cases for one platform's `patched` list, under `root`. */
function cases(root, patched) {
  const ninja = join(root, "out", "skia");
  const relative = (file) => `../../skia/${file}`;
  const files = Object.fromEntries(
    patched.map(({ file, marker }) => [file, `before ${marker} after`]),
  );
  const good = [
    "STARTING OFFLINE BUILD",
    `ninja: Entering directory \`${ninja}'`,
    ...patched.map(
      ({ file }, i) => `[${i + 1}/${patched.length}] compile ${relative(file)}`,
    ),
  ].join("\n");
  const crlf = good.replaceAll("\n", "\r\n");
  const backslashed = good.replace(
    /compile (\S+)/g,
    (_, path) => `compile ${path.replaceAll("/", "\\")}`,
  );
  return [
    [files, good, true, "an offline build of every patched file"],
    [files, crlf, true, "an offline build with CRLF line ends"],
    [files, backslashed, true, "an offline build naming files with `\\`"],
    ...patched.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      crlf,
      false,
      `an offline build with CRLF line ends of an unpatched ${file}`,
    ]),
    [
      files,
      good.replace("STARTING OFFLINE BUILD", "STARTING A FULL BUILD"),
      true,
      "a full build of every patched file",
    ],
    ...patched.map(({ file }) => [
      { ...files, [file]: "unpatched" },
      good,
      false,
      `an offline build of an unpatched ${file}`,
    ]),
    ...patched.map(({ file }) => [
      files,
      good
        .split("\n")
        .filter((line) => !line.endsWith(relative(file)))
        .join("\n"),
      false,
      `no ${file} compiled`,
    ]),
    [
      files,
      good.replace(
        "STARTING OFFLINE BUILD",
        "STARTING BIND AGAINST SYSTEM SKIA",
      ),
      false,
      "a bind against a system Skia",
    ],
    [
      files,
      good.replace(
        "STARTING OFFLINE BUILD",
        "TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES",
      ),
      false,
      "a download",
    ],
    [
      files,
      `${good}\nDOWNLOAD AND INSTALL FAILED: curl`,
      false,
      "a download tried alongside",
    ],
    [
      files,
      good.replace(/^ninja: Entering.*$/m, ""),
      false,
      "no ninja directory",
    ],
  ];
}

function selfTest() {
  const root = join(tmpdir(), `check-patched-skia-${process.pid}`);
  mkdirSync(join(root, "out", "skia"), { recursive: true });
  let count = 0;
  const patchText = (patch) => readFileSync(join(ROOT, patch), "utf8");
  const markerCase = (what, text, file, marker, accepted) => {
    if ((markerRefusal(text, file, marker) === null) !== accepted) {
      console.error(
        `check-patched-skia self-test: ${what} was ${accepted ? "refused" : "accepted"}`,
      );
      process.exit(1);
    }
    count += 1;
  };
  for (const [platform, patched] of Object.entries(PATCHED))
    for (const { patch, file, marker } of patched)
      markerCase(
        `${platform}'s marker for ${file}`,
        patchText(patch),
        file,
        marker,
        true,
      );
  markerCase(
    "a marker the unpatched file already holds",
    patchText(ASYNC_TRANSFER_REUSE.patch),
    ASYNC_TRANSFER_REUSE.file,
    "kDynamic_GrAccessPattern",
    false,
  );
  markerCase(
    "a marker the patch does not write",
    patchText(ATLAS_SIZE.patch),
    ATLAS_SIZE.file,
    "fAtlasMaxSize = 1024;",
    false,
  );
  // Two files, the second given the marker: the first is read from its own
  // lines, not from the patch's.
  const twoFiles = [
    `--- a/${SKIA_IN_RUST_SKIA}src/b.cpp`,
    `+++ b/${SKIA_IN_RUST_SKIA}src/b.cpp`,
    "@@ -1 +1 @@",
    "-old",
    "+new",
    `--- a/${SKIA_IN_RUST_SKIA}src/a.cpp`,
    `+++ b/${SKIA_IN_RUST_SKIA}src/a.cpp`,
    "@@ -1 +1 @@",
    "-old",
    "+MARKER",
    "",
  ].join("\n");
  markerCase(
    "a marker in the file that holds it",
    twoFiles,
    "src/a.cpp",
    "MARKER",
    true,
  );
  markerCase(
    "a marker in another file of the patch",
    twoFiles,
    "src/b.cpp",
    "MARKER",
    false,
  );
  markerCase(
    "a file the patch does not change",
    twoFiles,
    "src/c.cpp",
    "MARKER",
    false,
  );
  try {
    for (const [platform, patched] of Object.entries(PATCHED)) {
      for (const [files, text, accepted, what] of cases(root, patched)) {
        for (const [file, contents] of Object.entries(files)) {
          const path = join(root, "skia", file);
          mkdirSync(join(path, ".."), { recursive: true });
          writeFileSync(path, contents);
        }
        if ((refusal(text, patched) === null) !== accepted) {
          console.error(
            `check-patched-skia self-test: ${platform}, ${what} was ${accepted ? "refused" : "accepted"}`,
          );
          process.exit(1);
        }
        count += 1;
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  console.log(
    `check-patched-skia: self-test, ${count} cases, each unpatched shape refused`,
  );
}

// Run as a script only: `scripts/check-skia-patches.mjs` imports `PATCHED`
// and `UNVERIFIED`.
const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
const arg = process.argv[2];
if (!main);
else if (arg === "--self-test") selfTest();
else if (arg) check(arg);
else {
  console.error(
    "usage: node scripts/check-patched-skia.mjs <target/<profile>/build> | --self-test",
  );
  process.exit(2);
}
