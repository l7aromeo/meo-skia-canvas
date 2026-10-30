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
// patch gets a new directory and a stale one cannot be reused. Once a new one
// is ready, the others are removed unless a worktree of this repository still
// builds from one -- the cache is shared by every worktree on the machine.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
/** The patch applied to the tree, relative to a checkout of this repository. */
const PATCH = join("containers", "skia-metal-untimed-compile.patch");
const RUST_SKIA = "https://github.com/rust-skia/rust-skia.git";
/// Written last, so a directory without it is a preparation that did not finish.
const READY = ".ready";
/// Hex digits of the patch's sha256 in the directory name: enough that two
/// patches never share one, short enough to read.
const PATCH_DIGEST_LENGTH = 12;
/// Seconds to wait before each further try of a fetch that failed, so four
/// tries in all. googlesource answers a burst of git-sync-deps' parallel
/// fetches with 503s and 429s that clear within a minute, and a cold cache
/// that failed on one of them failed the build: the waits double from 15 s,
/// so the last try is nearly two minutes after the first.
const RETRY_DELAYS_SECONDS = [15, 30, 60];

/**
 * The skia-bindings version `root`'s Cargo.lock resolves, which is the tag to
 * fetch.
 */
function lockedVersion(root = ROOT) {
  const lock = readFileSync(join(root, "Cargo.lock"), "utf8");
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

/** The cache directory name of the tree a checkout at `root` builds from. */
function treeName(root = ROOT) {
  const digest = createHash("sha256")
    .update(readFileSync(join(root, PATCH)))
    .digest("hex")
    .slice(0, PATCH_DIGEST_LENGTH);
  return `skia-${lockedVersion(root)}-${digest}`;
}

function treeDir() {
  return join(cacheRoot(), treeName());
}

/**
 * The names of git's variables that tie a command to one repository, as git
 * itself lists them: the ones it clears before running a command inside a
 * submodule.
 */
let repositoryVariables;

/**
 * This process's environment, less the variables that tie git to one
 * repository, for every tool `prepare` runs.
 *
 * `git commit` runs its hooks with `GIT_DIR` and `GIT_INDEX_FILE` naming the
 * repository being committed to, and git honours them over the directory a
 * command runs in. `just precommit` runs `prepare`, so without this the clone
 * and git-sync-deps' `remote set-url`, `fetch` and `checkout` act on that
 * repository rather than on the checkout -- its index, origin and object
 * store, not Skia's.
 */
function childEnv() {
  repositoryVariables ??= new Set(
    execFileSync("git", ["rev-parse", "--local-env-vars"], {
      encoding: "utf8",
    }).split("\n"),
  );
  return Object.fromEntries(
    Object.entries(process.env).filter(
      ([name]) => !repositoryVariables.has(name),
    ),
  );
}

/** The directory `SKIA_SOURCE_DIR` names: Skia itself, inside the checkout. */
function sourceDir(tree = treeDir()) {
  return join(tree, "rust-skia", "skia-bindings", "skia");
}

// The tools' output goes to stderr, so the directory is the only thing
// `prepare` prints to stdout and a caller can take it whole. `env` is added to
// `childEnv()`, never a replacement for it.
export function run(command, args, { env = {}, ...options } = {}) {
  execFileSync(command, args, {
    stdio: ["ignore", process.stderr.fd, "inherit"],
    ...options,
    env: { ...childEnv(), ...env },
  });
}

/**
 * The names of the trees some worktree of the repository at `root` builds
 * from, or null when its worktrees cannot be listed. A worktree whose checkout
 * is gone, or that has no lock or patch to name a tree by, names none.
 */
export function treesInUse(root = ROOT) {
  let listing;
  try {
    listing = execFileSync("git", ["worktree", "list", "--porcelain"], {
      cwd: root,
      env: childEnv(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    return null;
  }
  const prefix = "worktree ";
  return new Set(
    listing
      .split("\n")
      .filter((line) => line.startsWith(prefix))
      .flatMap((line) => {
        try {
          return [treeName(line.slice(prefix.length))];
        } catch {
          return [];
        }
      }),
  );
}

/**
 * Removes every tree under `cache` that `inUse` does not name, and the partial
 * preparation of each. Nothing is removed when `inUse` is null: a tree left
 * behind costs disk, one removed from under a worktree costs it a fresh fetch.
 */
export function removeSuperseded(cache, inUse) {
  if (inUse === null) {
    console.error(
      "skia-source: this repository's worktrees could not be listed, so no " +
        `superseded tree under ${cache} was removed`,
    );
    return;
  }
  for (const name of readdirSync(cache)) {
    if (name.startsWith("skia-") && !inUse.has(name.replace(/\.partial$/, "")))
      rmSync(join(cache, name), { recursive: true, force: true });
  }
}

/** Blocks the thread for `seconds`: the script is synchronous throughout. */
function sleep(seconds) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, seconds * 1000);
}

/**
 * Runs `step`, and again after each of `delays` seconds while it throws,
 * saying on stderr which try of `what` failed and why. The last failure is
 * thrown, so one that persists still stops the caller with its own error.
 */
export function retry(
  what,
  step,
  { delays = RETRY_DELAYS_SECONDS, wait = sleep } = {},
) {
  const tries = delays.length + 1;
  for (let attempt = 1; ; attempt++) {
    try {
      return step();
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      if (attempt === tries) {
        console.error(
          `skia-source: ${what} failed on try ${attempt} of ${tries}, the last: ${reason}`,
        );
        throw error;
      }
      const delay = delays[attempt - 1];
      console.error(
        `skia-source: ${what} failed on try ${attempt} of ${tries}: ${reason}; trying again in ${delay} s`,
      );
      wait(delay);
    }
  }
}

function prepare() {
  const tree = treeDir();
  if (existsSync(join(tree, READY))) return sourceDir(tree);

  const partial = `${tree}.partial`;
  rmSync(partial, { recursive: true, force: true });
  const checkout = join(partial, "rust-skia");
  // A clone that failed part way leaves a directory git will not clone into,
  // so each try starts from none.
  retry("cloning rust-skia", () => {
    rmSync(checkout, { recursive: true, force: true });
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
  });
  // The same invocation skia-bindings makes for a full build
  // (build_support/skia/config.rs): DEPS read from skia-bindings, emsdk skipped.
  // It fetches into what an earlier try left and checks each dependency out
  // at its pinned revision, so a try resumes rather than starting over.
  retry("syncing Skia's DEPS", () =>
    run("python3", ["skia/tools/git-sync-deps"], {
      cwd: join(checkout, "skia-bindings"),
      env: {
        GIT_SYNC_DEPS_PATH: "skia/DEPS",
        GIT_SYNC_DEPS_SKIP_EMSDK: "1",
      },
    }),
  );
  retry("fetching gn", () =>
    run("python3", ["bin/fetch-gn"], { cwd: sourceDir(partial) }),
  );
  // `-F0`: a hunk that no longer matches fails here rather than being placed
  // by guesswork.
  run("patch", ["-p1", "-F0", "-d", checkout, "-i", join(ROOT, PATCH)]);
  writeFileSync(join(partial, READY), `${lockedVersion()}\n`);
  renameSync(partial, tree);

  // This checkout is one of the worktrees, so the new tree is kept with them.
  removeSuperseded(cacheRoot(), treesInUse());
  return sourceDir(tree);
}

// Run as a command, not when a test imports `retry`. Both sides resolved, as
// `import.meta.url` is: under /tmp on macOS the two spellings differ.
if (
  process.argv[1] &&
  realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))
) {
  // skia-bindings takes a `SKIA_SOURCE_DIR` that is set at all as an offline
  // build from that directory, an empty one included, and then fails reporting
  // gn missing. One set but empty is always a mistake upstream of here, so it is
  // named rather than passed on.
  if (process.env.SKIA_SOURCE_DIR === "") {
    console.error(
      "skia-source: SKIA_SOURCE_DIR is set but empty, which skia-bindings reads " +
        "as an offline build from no directory. Unset it, or set it to the " +
        "directory `node scripts/skia-source.mjs prepare` prints.",
    );
    process.exit(1);
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
}
