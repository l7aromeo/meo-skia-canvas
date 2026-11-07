#!/usr/bin/env node
// A local cache of compiled, patched Skia, for every worktree to link
// instead of compiling it again. What an entry is, and why it is linked the
// way it is, is in scripts/skia-binaries.mjs.
//
//   node scripts/skia-cache.mjs cargo [+toolchain] <cargo args>  cargo, linking the cached Skia where an entry fits
//   node scripts/skia-cache.mjs fill [cargo feature and profile args]  build Skia once and store it
//   node scripts/skia-cache.mjs store <out-dir> <profile> <features>  store a build that already ran
//   node scripts/skia-cache.mjs entry [cargo feature and profile args]  the entry those args would use
import { execFileSync, spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { cacheEnv, readArgs, store } from "./skia-binaries.mjs";
import { PATCHED, refusal } from "./check-patched-skia.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * Runs cargo with `args`, linking the cached Skia where an entry fits, and
 * says which on stderr. A miss builds as cargo always does.
 */
function runCargo(args) {
  const toolchain = args[0]?.startsWith("+") ? [args[0]] : [];
  const rest = args.slice(toolchain.length);
  const { env, dir, why } = cacheEnv(rest);
  console.error(
    env ? `skia-cache: linking ${dir}` : `skia-cache: compiling Skia (${why})`,
  );
  const child = spawn("cargo", [...toolchain, ...rest], {
    stdio: "inherit",
    env: { ...process.env, ...env },
  });
  child.on("exit", (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    process.exit(code ?? 1);
  });
}

/**
 * Builds the library with `args` from source, the cache unused, and stores
 * the Skia it compiled. cargo names the out-dir it ran skia-bindings' build
 * script in, and the features it built skia-bindings with, in its JSON
 * messages.
 */
function fill(args) {
  const read = readArgs(args);
  if (!read) throw new Error("skia-cache: fill builds for the host only");
  const env = { ...process.env };
  for (const name of ["SKIA_LIBRARY_SEARCH_PATH", "SKIA_BUILD_DEFINES"])
    delete env[name];
  const json = execFileSync(
    "cargo",
    ["build", "--lib", "--message-format=json-render-diagnostics", ...args],
    {
      cwd: ROOT,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "inherit"],
      maxBuffer: 1 << 30,
    },
  );
  const messages = json
    .split("\n")
    .filter((line) => line.startsWith("{"))
    .map((line) => JSON.parse(line));
  const isBindings = (m) => /[/#]skia-bindings[@#]/.test(m.package_id);
  const script = messages.find(
    (m) => m.reason === "build-script-executed" && isBindings(m),
  );
  const library = messages.find(
    (m) => m.reason === "compiler-artifact" && isBindings(m),
  );
  if (!script || !library)
    throw new Error("skia-cache: cargo built no skia-bindings");
  storeChecked(dirname(script.out_dir), read.profile, library.features);
}

/** `store`, vouched for by check-patched-skia's own reading of the log. */
function storeChecked(buildDir, profile, features) {
  const patched = PATCHED[process.platform];
  if (!patched)
    throw new Error(`skia-cache: no patches for ${process.platform}`);
  const { dir, stored } = store(buildDir, { profile, features }, (log) =>
    refusal(log, patched),
  );
  console.log(`skia-cache: ${stored ? "stored" : "already held"} ${dir}`);
}

const main =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (main) {
  const [command, ...args] = process.argv.slice(2);
  try {
    if (command === "cargo") runCargo(args);
    else if (command === "fill") fill(args);
    else if (command === "store") {
      const [buildDir, profile, features] = args;
      if (!buildDir || !profile || !features)
        throw new Error(
          "usage: node scripts/skia-cache.mjs store <out-dir> <profile> <feature,feature,...>",
        );
      storeChecked(buildDir, profile, features.split(","));
    } else if (command === "entry") {
      const { dir, why } = cacheEnv(args);
      console.log(dir ?? `none: ${why}`);
    } else {
      console.error(
        "usage: node scripts/skia-cache.mjs cargo|fill|store|entry ...",
      );
      process.exit(2);
    }
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
