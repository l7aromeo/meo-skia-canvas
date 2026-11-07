#!/usr/bin/env node
// A local cache of compiled, patched Skia, for every worktree to link
// instead of compiling it again. What an entry is, and why it is linked the
// way it is, is in scripts/skia-binaries.mjs.
//
//   node scripts/skia-cache.mjs cargo [+toolchain] <cargo args>  cargo, linking the cached Skia where an entry fits
//   node scripts/skia-cache.mjs fill [cargo feature and profile args]  build Skia once and store it
//   node scripts/skia-cache.mjs store <out-dir> <profile> <features>  store a build that already ran
//   node scripts/skia-cache.mjs entry [cargo feature and profile args]  the entry those args would use
//
// `cargo`, `fill` and `entry` refuse where `SKIA_SOURCE_DIR` names no tree
// and `node scripts/skia-source.mjs env` would name one: skia-bindings would
// then build against rust-skia's unpatched Skia, and every patch test would
// expect stock Skia and pass. `SKIA_CACHE_UNPATCHED=1` asks for that build on
// purpose. Every `just` recipe sets the variable through its dotenv command.
import { execFileSync, spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { cacheEnv, readArgs, store } from "./skia-binaries.mjs";
import { PATCHED, refusal } from "./check-patched-skia.mjs";
import { namesSourceTree } from "./skia-source.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// The variable that asks for a build against unpatched Skia, and the one
/// value that asks: anything else set there is a mistake, and refused.
const UNPATCHED = "SKIA_CACHE_UNPATCHED";
const UNPATCHED_ASKED = "1";

/// What sets `SKIA_SOURCE_DIR` outside `just`, as the refusal names it.
const SETS_SOURCE_DIR = `eval "$(node scripts/skia-source.mjs env | sed 's/^/export /')"`;

/**
 * Why a build with `env` would link Skia nobody asked for, or `null` when it
 * would not. `namesTree` is whether this checkout names a tree at all -- see
 * `namesSourceTree` -- and where it does not, skia-bindings' own choice is
 * the right one.
 */
export function unpatchedRefusal(
  env = process.env,
  namesTree = namesSourceTree(),
) {
  const asked = env[UNPATCHED];
  if (asked !== undefined && asked !== UNPATCHED_ASKED)
    return `skia-cache: ${UNPATCHED} is ${JSON.stringify(asked)}; set it to ${UNPATCHED_ASKED} to build unpatched Skia, or unset it.`;
  const dir = env.SKIA_SOURCE_DIR;
  if (asked === UNPATCHED_ASKED)
    return dir
      ? `skia-cache: ${UNPATCHED}=${UNPATCHED_ASKED} asks for unpatched Skia, but SKIA_SOURCE_DIR names ${dir}. Unset one of them.`
      : null;
  if (dir || !namesTree) return null;
  return [
    `skia-cache: SKIA_SOURCE_DIR ${dir === "" ? "is empty" : "is not set"}, so skia-bindings would build against rust-skia's unpatched Skia.`,
    `Run through \`just\`, or first: ${SETS_SOURCE_DIR}`,
    `To build unpatched Skia on purpose, set ${UNPATCHED}=${UNPATCHED_ASKED}.`,
  ].join("\n");
}

/**
 * Throws `unpatchedRefusal`'s reason, and says so on stderr where a build of
 * unpatched Skia was asked for.
 */
function refuseUnpatched() {
  const why = unpatchedRefusal();
  if (why) throw new Error(why);
  if (process.env[UNPATCHED] === UNPATCHED_ASKED && namesSourceTree())
    console.error(
      `skia-cache: building unpatched Skia (${UNPATCHED}=${UNPATCHED_ASKED})`,
    );
}

/**
 * Runs cargo with `args`, linking the cached Skia where an entry fits, and
 * says which on stderr. A miss builds as cargo always does.
 */
function runCargo(args) {
  refuseUnpatched();
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
  refuseUnpatched();
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
      refuseUnpatched();
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
