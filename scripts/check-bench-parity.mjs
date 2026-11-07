#!/usr/bin/env node
// The npm benchmark and the crate benchmark run the same cases, with the same
// ids, in the same order.
//
// `examples/node/benchmark.js` and `examples/rust/benchmark.rs` each write a
// report a row a case, and the point of having two is comparing them row by
// row: their ratio is what the Node binding costs. A case one side gains and
// the other does not leaves a row with nothing beside it, which reads as a
// case nobody thought to measure rather than one that was forgotten. So each
// prints its ids with `--list`, timing nothing, and this compares the two.
//
// Usage:  node scripts/check-bench-parity.mjs [--self-test] [--features LIST]
//
// `--features` is the cargo feature list the crate's side is built with --
// the recipe passes `test-rust`'s, so the example reuses the build the Rust
// suite makes rather than starting one of its own.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/**
 * What is wrong between `npm` and `crate`, two lists of case ids, as one
 * message a problem; none where they agree. Pure, so the self-test can hand
 * it lists that never came from a benchmark.
 */
export function problems(npm, crate) {
  const found = [];
  for (const [side, ids] of [
    ["npm", npm],
    ["crate", crate],
  ]) {
    // An empty list agrees with an empty list, which is what a benchmark
    // that failed to print anything would hand this.
    if (ids.length === 0) found.push(`the ${side} benchmark lists no cases`);
    const seen = new Set();
    for (const id of ids) {
      if (seen.has(id)) found.push(`${side} lists ${id} twice`);
      seen.add(id);
    }
  }
  const crateIds = new Set(crate);
  const npmIds = new Set(npm);
  for (const id of npm)
    if (!crateIds.has(id)) found.push(`${id} is in npm and not in crate`);
  for (const id of crate)
    if (!npmIds.has(id)) found.push(`${id} is in crate and not in npm`);
  // Same set, different order: the reports would line up only after a sort,
  // and a reader comparing them side by side does not sort.
  if (found.length === 0) {
    const at = npm.findIndex((id, i) => id !== crate[i]);
    if (at >= 0)
      found.push(
        `the lists part at case ${at + 1}: npm has ${npm[at]}, crate has ${crate[at]}`,
      );
  }
  return found;
}

function selfTest() {
  const ids = ["draw/scene/cpu/srgb", "draw/scene/gpu/srgb", "encode/png/cpu"];
  const cases = [
    ["the same list", ids, ids, 0, null],
    ["one missing from crate", ids, ids.slice(0, 2), 1, "not in crate"],
    ["one missing from npm", ids.slice(1), ids, 1, "not in npm"],
    [
      "the same ids, two swapped",
      ids,
      [ids[1], ids[0], ids[2]],
      1,
      "part at case 1",
    ],
    ["a duplicate", [...ids, ids[0]], [...ids, ids[0]], 2, "twice"],
    ["an empty npm list", [], ids, 4, "lists no cases"],
    ["both empty", [], [], 2, "lists no cases"],
    [
      "one renamed",
      ids,
      [ids[0], ids[1], "encode/png/gpu"],
      2,
      "encode/png/gpu",
    ],
  ];
  let bad = 0;
  for (const [label, npm, crate, expected, mustSay] of cases) {
    const got = problems(npm, crate);
    if (got.length !== expected) {
      bad++;
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got.length}: ${got.join("; ")}`,
      );
    } else if (mustSay && !got.some((p) => p.includes(mustSay))) {
      bad++;
      console.error(
        `  self-test FAILED: ${label} -- no problem mentioned "${mustSay}": ${got[0]}`,
      );
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `bench-parity: self-test, ${cases.length} cases, fires on each mismatch and on none of the matching lists`,
  );
  process.exit(0);
}

if (process.argv.includes("--self-test")) selfTest();

const lines = (text) =>
  text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
const at = process.argv.indexOf("--features");
const features = at >= 0 ? ["--features", process.argv[at + 1]] : [];

const npm = lines(
  execFileSync(process.execPath, ["examples/node/benchmark.js", "--list"], {
    encoding: "utf8",
  }),
);
// Through scripts/skia-cache.mjs, which links Skia compiled already where the local cache holds it
// for these features.
const crate = lines(
  execFileSync(
    process.execPath,
    [
      fileURLToPath(new URL("skia-cache.mjs", import.meta.url)),
      "cargo",
      "run",
      "--quiet",
      "--example",
      "benchmark",
      ...features,
      "--",
      "--list",
    ],
    { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
  ),
);
const found = problems(npm, crate);
if (found.length > 0) {
  for (const problem of found) console.error(`  ${problem}`);
  console.error(
    `bench-parity: the npm and crate benchmarks disagree in ${found.length} place(s)`,
  );
  process.exit(1);
}
console.log(`bench-parity: ${npm.length} cases, the same in both benchmarks`);
