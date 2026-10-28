#!/usr/bin/env node
// Whether a build compiled Skia from the patched source, read from what
// skia-bindings' build script printed.
//
//   node scripts/check-patched-skia.mjs <target/<profile>/build>
//   node scripts/check-patched-skia.mjs --self-test
//
// rust-skia's prebuilt Skia waits one second for each Metal shader compile
// and drops the draw when it runs out, and a binary built against it passes
// every test that does not stall the compiler. Three things have to hold, and
// each is read from skia-bindings' `output`:
//
// 1. It built offline from a source tree -- "STARTING OFFLINE BUILD" -- and
//    did not try a download.
// 2. ninja compiled `GrMtlUtil.mm`. A build without Metal never does, and
//    has nothing for this to vouch for.
// 3. The file it compiled, resolved against the directory ninja ran in, is
//    the patched one: it defines `GrMtlCompilesWithoutTimeout`, which
//    containers/skia-metal-untimed-compile.patch adds. An offline build from
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

/// What only the patched `GrMtlUtil.mm` contains.
const PATCHED = "GrMtlCompilesWithoutTimeout";
const COMPILED = /compile (\S+\/src\/gpu\/ganesh\/mtl\/GrMtlUtil\.mm)/;
const NINJA_DIR = /^ninja: Entering directory `(.*)'$/m;

/** Why `output` does not show a patched Skia, or `null` when it does. */
export function refusal(output) {
  if (!output.includes("STARTING OFFLINE BUILD"))
    return "skia-bindings did not build offline from a source tree";
  if (output.includes("DOWNLOAD AND INSTALL"))
    return "skia-bindings tried to download prebuilt Skia";
  const compiled = output.match(COMPILED)?.[1];
  if (!compiled) return "ninja did not compile GrMtlUtil.mm";
  const ninja = output.match(NINJA_DIR)?.[1];
  if (!ninja) return "the output does not say where ninja ran";
  const file = resolve(ninja, compiled);
  if (!existsSync(file)) return `the compiled ${file} is gone`;
  if (!readFileSync(file, "utf8").includes(PATCHED))
    return `${file} is not the patched source`;
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
  const mtl = join(root, "skia", "src", "gpu", "ganesh", "mtl");
  mkdirSync(ninja, { recursive: true });
  mkdirSync(mtl, { recursive: true });
  const relative = "../../skia/src/gpu/ganesh/mtl/GrMtlUtil.mm";
  const output = (lines) => lines.join("\n");
  const good = output([
    "STARTING OFFLINE BUILD",
    `ninja: Entering directory \`${ninja}'`,
    `[1/2] compile ${relative}`,
  ]);
  const cases = [
    [`if (${PATCHED}()) {`, good, true, "an offline build of the patched file"],
    ["if (false) {", good, false, "an offline build of the unpatched file"],
    [
      `if (${PATCHED}()) {`,
      good.replace(
        "STARTING OFFLINE BUILD",
        "TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES",
      ),
      false,
      "a download",
    ],
    [
      `if (${PATCHED}()) {`,
      `${good}\nDOWNLOAD AND INSTALL FAILED: curl`,
      false,
      "a download tried alongside",
    ],
    [
      `if (${PATCHED}()) {`,
      good.replace(`[1/2] compile ${relative}`, ""),
      false,
      "no Metal source compiled",
    ],
    [
      `if (${PATCHED}()) {`,
      good.replace(/^ninja: Entering.*$/m, ""),
      false,
      "no ninja directory",
    ],
  ];
  try {
    for (const [source, text, accepted, what] of cases) {
      writeFileSync(join(mtl, "GrMtlUtil.mm"), source);
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
