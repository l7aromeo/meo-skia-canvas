#!/usr/bin/env node
// Fails when an integration test asks for the GPU without going through
// `tests/common/mod.rs`, which says whether the canvas is on one.
//
// `set_gpu(true)` falls back to the CPU in silence where no GPU engine
// starts: a build without a GPU feature, or a machine without a device. A
// test that draws a GPU leg and never asks `common::on_gpu`, `has_gpu` or
// `engines` then compares the CPU with itself and passes, and a green run
// cannot be told from one that drew on a GPU. The helpers skip such a leg
// saying so, and fail it where a GPU is required.
//
// Two shapes are refused, line by line in each `tests/*.rs`:
//
// - A GPU leg drawn over a literal pair, `for gpu in [false, true]` or a
//   `const ENGINES: [bool; 2] = [false, true]`. `common::engines()` is the
//   pair, less the GPU where there is none.
// - `gpu: true` or `set_gpu(true)` in a file that calls none of the helpers,
//   unless a comment within the eight lines above says `GPU allowed on
//   purpose` and why: a test whose claim holds on whichever engine answers.
//
// What it does not see: a file that calls a helper in one test and asks for
// the GPU unchecked in another, and a request spelt some other way -- a
// `CanvasOptions` built elsewhere, a `gpu` flag with another name. Its green
// says these two shapes are absent, not that every GPU leg is gated.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/** A GPU leg over a literal pair of engines. */
const LITERAL_PAIR = [
  /\b\w*gpu\w*\s+in\s+\[\s*false\s*,\s*true\s*\]/i,
  /\bconst\s+\w*(?:ENGINE|GPU)\w*\s*:\s*\[bool;\s*2\]\s*=\s*\[\s*false\s*,\s*true\s*\]/,
];

/** A canvas asked for the GPU outright. */
const REQUEST = /\bgpu:\s*true\b|\bset_gpu\(\s*true\s*\)/;

/** A call of a helper in `tests/common/mod.rs` that says where it drew. */
const HELPER = /\bcommon::(?:on_gpu|has_gpu|engines)\b/;

/** The marker that lets a request stand outside the helpers, and how far above it may sit. */
const ON_PURPOSE = "GPU allowed on purpose";
const ON_PURPOSE_REACH = 8;

/** The problems in `text`, one `tests/*.rs` file. */
export function check(text) {
  const lines = text.split("\n");
  const gated = HELPER.test(text);
  const problems = [];
  lines.forEach((line, index) => {
    const at = `line ${index + 1}`;
    if (LITERAL_PAIR.some((pattern) => pattern.test(line))) {
      problems.push(
        `${at}: a GPU leg over [false, true]; use common::engines()`,
      );
    }
    if (gated || !REQUEST.test(line)) return;
    const above = lines.slice(Math.max(0, index - ON_PURPOSE_REACH), index + 1);
    if (!above.some((l) => l.includes(ON_PURPOSE))) {
      problems.push(
        `${at}: asks for the GPU and the file calls no common::on_gpu, has_gpu or engines; ` +
          `gate it, or say "${ON_PURPOSE}" and why`,
      );
    }
  });
  return problems;
}

/** Cases that must be refused or passed, so a checker that stopped refusing shows. */
function selfTest() {
  const cases = [
    ["a loop over the pair", "for gpu in [false, true] {", 1],
    ["a loop over the helper", "for gpu in common::engines() {", 0],
    ["a pair with another name", "for on_gpu in [false,true] {", 1],
    ["a pair of another flag", "for premultiplied in [false, true] {", 0],
    ["a const pair", "const ENGINES: [bool; 2] = [false, true];", 1],
    [
      "a const pair of another flag",
      "const OUTLINES: [bool; 2] = [false, true];",
      0,
    ],
    ["a request in an ungated file", "    canvas.set_gpu(true);", 1],
    ["an option in an ungated file", "            gpu: true,", 1],
    [
      "a request in a gated file",
      "canvas.set_gpu(true);\nif !common::on_gpu(&canvas) {}",
      0,
    ],
    [
      "a request gated by has_gpu",
      "if !common::has_gpu() { return; }\ncanvas.set_gpu(true);",
      0,
    ],
    [
      "a request on purpose",
      "// GPU allowed on purpose: the claim holds on either engine.\ncanvas.set_gpu(true);",
      0,
    ],
    [
      "a marker too far above",
      `// GPU allowed on purpose: far.\n${"\n".repeat(ON_PURPOSE_REACH)}canvas.set_gpu(true);`,
      1,
    ],
    ["a request for the CPU", "canvas.set_gpu(false);", 0],
    ["a request by variable", "canvas.set_gpu(gpu);", 0],
  ];
  const failed = cases.filter(([, text, want]) => check(text).length !== want);
  for (const [name, text, want] of failed) {
    console.error(
      `self-test "${name}": want ${want} problems, got ${JSON.stringify(check(text))}`,
    );
  }
  if (failed.length) process.exit(1);
  console.log(`self-test: ${cases.length} cases`);
}

/** The integration tests: each `tests/*.rs`, the shared module aside. */
function testFiles(dir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith(".rs"))
    .map((name) => join(dir, name));
}

const main = import.meta.url === pathToFileURL(process.argv[1]).href;
if (main) {
  if (process.argv.includes("--self-test")) selfTest();
  const files = testFiles("tests");
  // A listing that came back empty would pass every file it did not read.
  if (files.length === 0) {
    console.error(
      "check-gpu-legs: no tests/*.rs found; run from the repository root",
    );
    process.exit(1);
  }
  let failed = false;
  for (const file of files) {
    for (const problem of check(readFileSync(file, "utf8"))) {
      console.error(`${file} ${problem}`);
      failed = true;
    }
  }
  if (failed) process.exit(1);
  console.log(
    `${files.length} test files: every GPU leg goes through tests/common`,
  );
}
