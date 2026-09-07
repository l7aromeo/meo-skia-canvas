#!/usr/bin/env node
// The entry point for `npm test`, and the reason it is not just `node --test`.
//
// `loadSkiaNode` tries the `MEO_SKIA_CANVAS_BINARY` override, then a platform package
// from node_modules, then `lib/skia.node`. A platform package therefore outranks a local
// build, so `npm run build && npm test` ran the whole suite against the *published*
// binary and a freshly compiled change looked like it had done nothing. Measured on one
// checkout: 59,068,312 bytes of debug build beside 27,210,832 bytes of release, and the
// bare runner loaded the second.
//
// So set the override when there is a local build to point it at, and say which binary
// the run is about either way. Selecting silently would fix the wrong binary and keep the
// property that made the original bad -- that nothing on screen tells you which one ran.
//
// Node rather than an env prefix in package.json: `FOO=bar node ...` is shell syntax, and
// npm scripts run under cmd.exe on Windows, where it is not.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url))),
  LOCAL_BUILD = join(ROOT, "lib", "skia.node"),
  OVERRIDE = "MEO_SKIA_CANVAS_BINARY";

const env = { ...process.env };
let against;

if (env[OVERRIDE]) {
  // Already pointed somewhere deliberately -- `just test-js` does this. Leave it, and let
  // `loadOverride` be fatal if the path is wrong rather than second-guessing it here.
  against = `${OVERRIDE}=${env[OVERRIDE]}`;
} else if (existsSync(LOCAL_BUILD)) {
  env[OVERRIDE] = LOCAL_BUILD;
  against = `the local build, ${relative(ROOT, LOCAL_BUILD)}`;
} else {
  // A fresh clone with no build: the installed binary is the only one there is, and
  // testing it is the right answer rather than a refusal.
  against = "the installed binary -- no local build to point at";
}

console.log(`testing against ${against}`);

const { status, error } = spawnSync(
  process.execPath,
  ["--test", ...process.argv.slice(2)],
  { cwd: ROOT, env, stdio: "inherit" },
);

if (error) throw error;
// A signal death leaves `status` null, which must not read as success.
process.exit(status ?? 1);
