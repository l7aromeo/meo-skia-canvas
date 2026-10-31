#!/usr/bin/env node
// Whether a build compiled Skia from the patched source, read from what
// skia-bindings' build script printed.
//
//   node scripts/check-patched-skia.mjs <target/<profile>/build>
//   node scripts/check-patched-skia.mjs --self-test
//
// rust-skia's prebuilt Skia waits one second for each Metal shader compile
// and drops the draw when it runs out, and its atlas path renderer can lose a
// pixel of a path's coverage on Metal; a binary built against it passes every
// test that does not stall the compiler or land a path where the pixel goes.
// Three things have to hold, and each is read from skia-bindings' `output`:
//
// 1. It built offline from a source tree -- "STARTING OFFLINE BUILD" -- and
//    did not try a download.
// 2. ninja compiled each file a macOS patch changes. A build without Metal
//    never compiles `GrMtlUtil.mm`, and has nothing for this to vouch for.
// 3. Each file it compiled, resolved against the directory ninja ran in, is
//    the patched one: it holds what only the patch puts there -- see
//    `PATCHED`. An offline build from an unpatched tree passes the first two
//    and fails this.
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

/// Each file a macOS patch changes, under Skia's source root, with what only
/// the patched file contains.
export const PATCHED = [
  {
    // containers/skia-metal-untimed-compile.patch
    file: "src/gpu/ganesh/mtl/GrMtlUtil.mm",
    marker: "GrMtlCompilesWithoutTimeout",
  },
  {
    // containers/skia-atlas-size.patch
    file: "src/gpu/ganesh/ops/AtlasPathRenderer.cpp",
    marker: "fAtlasMaxSize = 512;",
  },
];

/** The ninja line that names `file` compiled, capturing the path it gave. */
function compiledLine(file) {
  const escaped = file.replace(/[./]/g, (c) => `\\${c}`);
  return new RegExp(`compile (\\S+\\/${escaped})`);
}
const NINJA_DIR = /^ninja: Entering directory `(.*)'$/m;

/** Why `output` does not show a patched Skia, or `null` when it does. */
export function refusal(output) {
  if (!output.includes("STARTING OFFLINE BUILD"))
    return "skia-bindings did not build offline from a source tree";
  if (output.includes("DOWNLOAD AND INSTALL"))
    return "skia-bindings tried to download prebuilt Skia";
  const ninja = output.match(NINJA_DIR)?.[1];
  for (const { file, marker } of PATCHED) {
    const compiled = output.match(compiledLine(file))?.[1];
    if (!compiled) return `ninja did not compile ${file}`;
    if (!ninja) return "the output does not say where ninja ran";
    const path = resolve(ninja, compiled);
    if (!existsSync(path)) return `the compiled ${path} is gone`;
    if (!readFileSync(path, "utf8").includes(marker))
      return `${path} is not the patched source`;
  }
  return null;
}

function check(buildDir) {
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
    .map((path) => [path, refusal(readFileSync(path, "utf8"))])
    .filter(([, why]) => why);
  for (const [path, why] of problems) console.error(`${path}: ${why}`);
  if (problems.length) process.exit(1);
  console.log(
    `check-patched-skia: ${outputs.length} skia-bindings build(s) compiled the patched Skia source`,
  );
}

function selfTest() {
  const root = join(tmpdir(), `check-patched-skia-${process.pid}`);
  const ninja = join(root, "out", "skia");
  mkdirSync(ninja, { recursive: true });
  const source = (file) => join(root, "skia", file);
  const relative = (file) => `../../skia/${file}`;
  const write = (files) => {
    for (const { file } of PATCHED) {
      mkdirSync(join(source(file), ".."), { recursive: true });
      writeFileSync(source(file), files[file]);
    }
  };
  const patched = Object.fromEntries(
    PATCHED.map(({ file, marker }) => [file, `before ${marker} after`]),
  );
  const good = [
    "STARTING OFFLINE BUILD",
    `ninja: Entering directory \`${ninja}'`,
    ...PATCHED.map(
      ({ file }, i) => `[${i + 1}/${PATCHED.length}] compile ${relative(file)}`,
    ),
  ].join("\n");
  const cases = [
    [patched, good, true, "an offline build of every patched file"],
    ...PATCHED.map(({ file }) => [
      { ...patched, [file]: "unpatched" },
      good,
      false,
      `an offline build of an unpatched ${file}`,
    ]),
    ...PATCHED.map(({ file }) => [
      patched,
      good
        .split("\n")
        .filter((line) => !line.endsWith(relative(file)))
        .join("\n"),
      false,
      `no ${file} compiled`,
    ]),
    [
      patched,
      good.replace(
        "STARTING OFFLINE BUILD",
        "TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES",
      ),
      false,
      "a download",
    ],
    [
      patched,
      `${good}\nDOWNLOAD AND INSTALL FAILED: curl`,
      false,
      "a download tried alongside",
    ],
    [
      patched,
      good.replace(/^ninja: Entering.*$/m, ""),
      false,
      "no ninja directory",
    ],
  ];
  try {
    for (const [files, text, accepted, what] of cases) {
      write(files);
      if ((refusal(text) === null) !== accepted) {
        console.error(
          `check-patched-skia self-test: ${what} was ${accepted ? "refused" : "accepted"}`,
        );
        process.exit(1);
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  console.log(
    `check-patched-skia: self-test, ${cases.length} cases, each unpatched shape refused`,
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
