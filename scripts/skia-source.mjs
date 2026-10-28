#!/usr/bin/env node
// The Skia source tree macOS builds compile from, with
// `containers/skia-metal-untimed-compile.patch` applied.
//
//   node scripts/skia-source.mjs env       `SKIA_SOURCE_DIR=<dir>` on macOS, nothing elsewhere
//   node scripts/skia-source.mjs dir       the directory, prepared or not
//   node scripts/skia-source.mjs prepare   fetch, sync and patch it once; print the directory
//
// `env` is the justfile's `dotenv-command`, so every recipe on macOS builds
// skia-bindings offline from this tree and none elsewhere. It only names the
// directory -- it runs on every `just` invocation, `just --list` included, so it
// must never download. `prepare` is what downloads, and `ensure-deps` runs it.
//
// The tree is rust-skia at the tag of the skia-bindings version in Cargo.lock,
// whose Skia submodule is pinned by that tag, with Skia's own DEPS synced and gn
// fetched at the revision `bin/fetch-gn` names. It lives once per version and
// patch under `$XDG_CACHE_HOME/meo-skia-canvas`, so a new version or a changed
// patch gets a new directory and a stale one cannot be reused; the superseded
// ones are removed after a new one is ready.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const PATCH = join(ROOT, "containers", "skia-metal-untimed-compile.patch");
const RUST_SKIA = "https://github.com/rust-skia/rust-skia.git";
/// Written last, so a directory without it is a preparation that did not finish.
const READY = ".ready";
/// Hex digits of the patch's sha256 in the directory name: enough that two
/// patches never share one, short enough to read.
const PATCH_DIGEST_LENGTH = 12;

/** The skia-bindings version Cargo.lock resolves, which is the tag to fetch. */
function lockedVersion() {
  const lock = readFileSync(join(ROOT, "Cargo.lock"), "utf8");
  const found = lock.match(
    /\[\[package\]\]\nname = "skia-bindings"\nversion = "([^"]+)"/,
  );
  if (!found) throw new Error("skia-source: Cargo.lock has no skia-bindings");
  return found[1];
}

function cacheRoot() {
  const base = process.env.XDG_CACHE_HOME || join(homedir(), ".cache");
  return join(base, "meo-skia-canvas");
}

function treeDir() {
  const digest = createHash("sha256")
    .update(readFileSync(PATCH))
    .digest("hex")
    .slice(0, PATCH_DIGEST_LENGTH);
  return join(cacheRoot(), `skia-${lockedVersion()}-${digest}`);
}

/** The directory `SKIA_SOURCE_DIR` names: Skia itself, inside the checkout. */
function sourceDir(tree = treeDir()) {
  return join(tree, "rust-skia", "skia-bindings", "skia");
}

// The tools' output goes to stderr, so the directory is the only thing
// `prepare` prints to stdout and a caller can take it whole.
function run(command, args, options = {}) {
  execFileSync(command, args, {
    stdio: ["ignore", process.stderr.fd, "inherit"],
    ...options,
  });
}

function prepare() {
  const tree = treeDir();
  if (existsSync(join(tree, READY))) return sourceDir(tree);

  const partial = `${tree}.partial`;
  rmSync(partial, { recursive: true, force: true });
  const checkout = join(partial, "rust-skia");
  run("git", [
    "clone",
    "--quiet",
    "--depth",
    "1",
    "--branch",
    lockedVersion(),
    "--recurse-submodules",
    "--shallow-submodules",
    "-j8",
    RUST_SKIA,
    checkout,
  ]);
  // The same invocation skia-bindings makes for a full build
  // (build_support/skia/config.rs): DEPS read from skia-bindings, emsdk skipped.
  run("python3", ["skia/tools/git-sync-deps"], {
    cwd: join(checkout, "skia-bindings"),
    env: {
      ...process.env,
      GIT_SYNC_DEPS_PATH: "skia/DEPS",
      GIT_SYNC_DEPS_SKIP_EMSDK: "1",
    },
  });
  run("python3", ["bin/fetch-gn"], { cwd: sourceDir(partial) });
  // `-F0`: a hunk that no longer matches fails here rather than being placed
  // by guesswork.
  run("patch", ["-p1", "-F0", "-d", checkout, "-i", PATCH]);
  writeFileSync(join(partial, READY), `${lockedVersion()}\n`);
  renameSync(partial, tree);

  for (const name of readdirSync(cacheRoot())) {
    const other = join(cacheRoot(), name);
    if (name.startsWith("skia-") && other !== tree)
      rmSync(other, { recursive: true, force: true });
  }
  return sourceDir(tree);
}

const command = process.argv[2];
if (command === "env") {
  if (process.platform === "darwin")
    console.log(`SKIA_SOURCE_DIR=${sourceDir()}`);
} else if (command === "dir") {
  console.log(sourceDir());
} else if (command === "prepare") {
  console.log(prepare());
} else {
  console.error("usage: node scripts/skia-source.mjs env|dir|prepare");
  process.exit(2);
}
