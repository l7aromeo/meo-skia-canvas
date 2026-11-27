#!/usr/bin/env node
// Fails unless a build with `window` and no GPU backend stops at the error
// that names the fix.
//
// `gui` draws each window through `gpu::Renderer`, which only `metal` or
// `vulkan` defines, so `--features window` alone cannot build. Without the
// `compile_error!` in `src/gpu/mod.rs` it stops at `error[E0432]: unresolved
// import`, which names neither feature. No suite builds a feature set that has
// to fail, so this is what holds that error: it runs
// `cargo check --no-default-features --features window` and refuses a build
// that compiles, one that fails without the named message, and one that
// reports another error beside it.
//
// The message is read from the `compile_error!` itself, so rewording it does
// not leave this checking for the old words.
//
// Usage:  node scripts/check-window-backend.mjs [--self-test] [-- CARGO...]
//
// `CARGO` is the command that runs cargo, `cargo` by default. The justfile
// passes `node scripts/skia-cache.mjs cargo`, so the check links the Skia the
// `--no-default-features` lint already built; `window` adds no skia-safe
// feature, so it is the same entry.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The file whose `compile_error!` a `window` build without a backend hits. */
const SOURCE = join("src", "gpu", "mod.rs");

/**
 * The build that must fail. `--color never` because the output is read as
 * text: CI sets `CARGO_TERM_COLOR=always`, and every line then opens with an
 * escape sequence rather than `error`.
 */
const ARGS = [
  "check",
  "--color",
  "never",
  "--no-default-features",
  "--features",
  "window",
];

/**
 * The messages of every `compile_error!("...")` in `source`, as rustc prints
 * them: a backslash at a line's end drops the newline and the next line's
 * leading whitespace, the one escape these messages use.
 */
export function compileErrors(source) {
  return [
    ...source.matchAll(/compile_error!\(\s*"((?:[^"\\]|\\[\s\S])*)"\s*\)/g),
  ].map(([, literal]) => literal.replace(/\\\n\s*/g, ""));
}

/**
 * What is wrong with a `window`-only build that exited with `status` and
 * printed `stderr`, expecting `message`; none where it stopped at that
 * error alone. Pure, so the self-test can hand it output no build printed.
 */
export function problems({ status, stderr }, message) {
  const build = `\`cargo ${ARGS.join(" ")}\``;
  if (status === 0)
    return [
      `${build} compiled; a window build with no GPU backend must stop at ${SOURCE}'s compile_error!`,
    ];
  const found = [];
  if (!stderr.split("\n").includes(`error: ${message}`)) {
    const first = stderr.split("\n").find((line) => line.startsWith("error"));
    found.push(
      `${build} failed without the named error (${first ?? "no error line printed"})`,
    );
  }
  // A coded error is name resolution or type checking, which the
  // `compile_error!` stops before; one beside it buries the fix again.
  for (const line of stderr.split("\n").filter((l) => /^error\[E\d+\]/.test(l)))
    found.push(`${build} also reported ${line}`);
  return found;
}

function selfTest() {
  const message =
    "the `window` feature needs a GPU backend beside it: `metal` on macOS, `vulkan` on Linux and Windows";
  const named = `error: ${message}\n   --> src/gpu/mod.rs:102:1\n`;
  const unresolved =
    "error[E0432]: unresolved import `crate::gpu::Renderer`\n --> src/gui/window.rs:9:5\n";
  const summary =
    "error: could not compile `meo-skia-canvas` (lib) due to 1 previous error\n";
  const cases = [
    ["the named error alone", 101, named + summary, 0, null],
    ["a build that compiles", 0, "", 1, "compiled"],
    ["the unresolved import alone", 101, unresolved + summary, 2, "E0432"],
    ["the named error and the import", 101, named + unresolved, 1, "also"],
    ["a reworded message", 101, `error: ${message}.\n`, 1, "without"],
    ["a failure printing nothing", 101, "", 1, "no error line"],
    ["the message in a note", 101, `note: ${message}\n`, 1, "without"],
  ];
  let bad = 0;
  for (const [label, status, stderr, expected, mustSay] of cases) {
    const got = problems({ status, stderr }, message);
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
  const source = [
    "compile_error!(",
    `    "the \`window\` feature needs a GPU backend beside it: \`metal\` on macOS, \\`,
    `     \`vulkan\` on Linux and Windows"`,
    ");",
  ].join("\n");
  const read = compileErrors(source);
  if (read.length !== 1 || read[0] !== message) {
    bad++;
    console.error(
      `  self-test FAILED: a continued literal read as ${JSON.stringify(read)}`,
    );
  }
  if (compileErrors("// compile_error! is not called here\n").length !== 0) {
    bad++;
    console.error("  self-test FAILED: prose read as a compile_error!");
  }
  if (bad > 0) process.exit(1);
  console.log(
    `window-backend: self-test, ${cases.length + 2} cases, refuses each wrong build and reads a continued message`,
  );
  process.exit(0);
}

function main() {
  if (process.argv.includes("--self-test")) selfTest();
  const at = process.argv.indexOf("--");
  const [cargo, ...prefix] = at >= 0 ? process.argv.slice(at + 1) : ["cargo"];
  const messages = compileErrors(readFileSync(join(ROOT, SOURCE), "utf8"));
  if (messages.length !== 1) {
    console.error(
      `window-backend: expected one compile_error! in ${SOURCE}, found ${messages.length}`,
    );
    process.exit(1);
  }
  const run = spawnSync(cargo, [...prefix, ...ARGS], {
    cwd: ROOT,
    encoding: "utf8",
    stdio: ["ignore", "inherit", "pipe"],
  });
  if (run.error || run.status === null) {
    console.error(
      `window-backend: ${cargo} did not finish: ${run.error?.message ?? run.signal}`,
    );
    process.exit(1);
  }
  const found = problems(run, messages[0]);
  if (found.length > 0) {
    process.stderr.write(run.stderr);
    for (const problem of found) console.error(`window-backend: ${problem}`);
    process.exit(1);
  }
  console.log(
    `window-backend: a window build with no GPU backend stops at ${SOURCE}'s error`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) main();
