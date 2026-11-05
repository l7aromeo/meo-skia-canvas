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
// curve left; a binary built against it passes every test that does not stall
// the compiler, land a path where the pixel moves or draw a curve at two
// scales. Three things have to hold, and each is read from skia-bindings'
// `output`:
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
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

/// containers/skia-atlas-size.patch, on every platform.
const ATLAS_SIZE = {
  file: "src/gpu/ganesh/ops/AtlasPathRenderer.cpp",
  marker: "fAtlasMaxSize = 512;",
};

/// containers/skia-triangulation-tolerance.patch, on every platform.
const TRIANGULATION = {
  file: "src/gpu/ganesh/ops/TriangulatingPathRenderer.cpp",
  marker: "info->fTolerance == tol",
};

/// Each file a patch for the platform changes, under Skia's source root, with
/// what only the patched file contains. Linux's brotli patch is not here: it
/// changes a build list rather than a compiled file, and a `freetype` build
/// without it fails to link.
export const PATCHED = {
  darwin: [
    {
      // containers/skia-metal-untimed-compile.patch
      file: "src/gpu/ganesh/mtl/GrMtlUtil.mm",
      marker: "GrMtlCompilesWithoutTimeout",
    },
    ATLAS_SIZE,
    TRIANGULATION,
  ],
  linux: [ATLAS_SIZE, TRIANGULATION],
  win32: [
    ATLAS_SIZE,
    TRIANGULATION,
    {
      // containers/skia-dwrite-oblique.patch
      file: "src/ports/SkTypeface_win_dw.cpp",
      marker: "fsSelection.field.Oblique",
    },
  ],
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

const arg = process.argv[2];
if (arg === "--self-test") selfTest();
else if (arg) check(arg);
else {
  console.error(
    "usage: node scripts/check-patched-skia.mjs <target/<profile>/build> | --self-test",
  );
  process.exit(2);
}
