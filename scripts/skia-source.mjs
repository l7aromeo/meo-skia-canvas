#!/usr/bin/env node
// The Skia source tree macOS and Linux builds compile from, with this
// platform's patches from `PATCHES` applied.
//
//   node scripts/skia-source.mjs env       `SKIA_SOURCE_DIR=<dir>` on a platform with patches, nothing elsewhere
//   node scripts/skia-source.mjs dir       the directory, prepared or not
//   node scripts/skia-source.mjs prepare   make it once, from a tree already here or the network; print the directory
//
// Against a local rust-skia checkout (`just use-local-skia`) both `env` and
// `prepare` do nothing: skia-bindings builds the checkout's own Skia.
//
// `env` is the justfile's `dotenv-command`, so every recipe on macOS and Linux
// builds skia-bindings offline from this tree, and on Windows, which has no
// patches, as skia-bindings always has. It only names the directory -- it runs
// on every `just` invocation, `just --list` included, so it must never
// download. `prepare` is what downloads, and `ensure-deps` runs it.
//
// The tree is rust-skia at the tag of the skia-bindings version in Cargo.lock,
// whose Skia submodule is pinned by that tag, with Skia's own DEPS synced and gn
// fetched at the revision `bin/fetch-gn` names. It lives once per version and
// patch list under `$XDG_CACHE_HOME/meo-skia-canvas`, so a new version or a changed
// patch gets a new directory and a stale one cannot be reused. Once a new one
// is ready, the others are removed unless a worktree of this repository still
// builds from one -- the cache is shared by every worktree on the machine.
//
// Every tree for a version holds the same checkout, dependencies and gn, and
// differs from the others only in the patches applied to tracked files. So a
// new tree is made from one already in the cache where one passes `derive`'s
// checks -- copied, its sources restored by git, then patched -- and fetched,
// about 1 GB, only where none does.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  constants,
  cpSync,
  existsSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
/**
 * The patches applied to the tree, by `process.platform`, in order and
 * relative to a checkout of this repository. A platform with none builds
 * skia-bindings as it always has, and `env` names no tree there.
 */
// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_path_covers_a_crowded_flush_as_it_covers_a_small_one with `metal` on macOS, on a tree prepared without this patch; 28 pixels read a step above or below a small flush of the same stars
//
// A path Ganesh's atlas path renderer places at an atlas row of 512 or more
// can have a pixel's coverage moved a step up or down -- about 50 of 255
// levels of alpha -- and which ones do depends on the atlas's layout. On a
// page of 2000 small stars, 28 pixels read 51 levels above or below what a
// flush of the stars reaching them draws, 19 above and 9 below; an eyelash
// drawn after blurred shapes whose masks were rasterised in the same flush
// lost its tip. Seen on Metal (an Apple GPU), and the eyelash on Vulkan too
// (lavapipe, CI's Linux runner), so it is the renderer's and not a driver's.
// Why row 512 is unknown. The patch caps the atlas at 512 by 512 so no path
// is placed past it; a full atlas starts another sharing its texture, and a
// path with a side past 512 pixels goes to the next path renderer rather than
// the atlas.
const ATLAS_SIZE = join("containers", "skia-atlas-size.patch");

export const PATCHES = {
  darwin: [
    // The Metal compile timeout -- see the UPSTREAM note in the justfile.
    join("containers", "skia-metal-untimed-compile.patch"),
    ATLAS_SIZE,
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
    // Re-check: cargo test a_blurred_mask_in_the_same_flush_moves_another_by_a_level with `metal`; it pins the defect and fails once the caustic stops moving
    //
    // The same renderer, with the cap above in place: a blurred fill moves
    // by one level at a few dozen pixels where another shape's blurred mask
    // is computed in the same flush, and not where that mask is already
    // cached, so a frame can read a level apart with what the GPU drew
    // before it. Shown with the atlas renderer declining every path, which
    // makes it go; ruled out are the shared stencil attachment's loads,
    // atlas texture sharing, atlas growth, path-mask caching and scratch
    // textures. The cause is not known. Declining the atlas would fix it at
    // a cost in every page's anti-aliasing against the CPU engine's, which is
    // why it is not taken. Seen on Metal only; not looked for elsewhere.
  ],
  linux: [
    // UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
    // Re-check: link a Linux build with `freetype` from a tree prepared without this patch, with build.rs's shim returning at once; it fails on undefined `kCmdLut` and `BrotliDecoderEnsureStaticInit`
    //
    // Skia's `third_party/brotli/BUILD.gn` leaves out `c/dec/prefix.c` and
    // `c/dec/static_init.c`, which define `kCmdLut` and
    // `BrotliDecoderEnsureStaticInit` since the brotli roll in M156's DEPS.
    // Only `freetype-woff2` links brotli and skia-bindings drops it on macOS,
    // so the patch is Linux's alone. build.rs carries the same fix for builds
    // that do not come through here, and stands aside where this patch has
    // applied.
    join("containers", "skia-brotli-sources.patch"),
    ATLAS_SIZE,
  ],
};

/** This platform's patches, or none. */
function patches(platform = process.platform) {
  return PATCHES[platform] ?? [];
}

/**
 * Whether the Cargo.toml at `root` builds skia-bindings from a local rust-skia
 * checkout, as `just use-local-skia` makes it. skia-bindings then builds that
 * checkout's own Skia, syncing its DEPS itself, and this script neither
 * prepares nor names a tree: one named here would be built offline in its
 * place.
 */
export function usesLocalSkia(root = ROOT) {
  return /^skia-bindings\s*=\s*\{\s*path\s*=/m.test(
    readFileSync(join(root, "Cargo.toml"), "utf8"),
  );
}
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

/**
 * The cache directory name of the tree a checkout at `root` builds from: the
 * locked version and a digest of this platform's patches, in order. A list of
 * one hashes as that patch alone did.
 */
function treeName(root = ROOT) {
  const hash = createHash("sha256");
  for (const patch of patches()) hash.update(readFileSync(join(root, patch)));
  const digest = hash.digest("hex").slice(0, PATCH_DIGEST_LENGTH);
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
 * behind costs disk, one removed from under a worktree costs it a fresh tree.
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

/**
 * The paths, relative to the rust-skia checkout, of the files `patchText`
 * creates rather than changes: those its hunks take from `/dev/null`.
 */
export function patchCreates(patchText) {
  return [...patchText.matchAll(/^--- \/dev\/null\n\+\+\+ b\/(\S+)/gm)].map(
    ([, path]) => path,
  );
}

/**
 * Reads the dependencies Skia's DEPS pins, as git-sync-deps reads them: the
 * file is Python, executed with `Var` defined, and each entry that names a
 * repository `@` a commit is a checkout under Skia's root.
 */
const PINS = `
import json, sys
scope = {}
exec("def Var(x): return vars[x]\\n" + open(sys.argv[1]).read(), scope)
print(json.dumps({d: v.split("@", 1)[1] for d, v in scope["deps"].items()
                  if isinstance(v, str) and "@" in v}))
`;

/**
 * The Python to run DEPS and Skia's scripts with: `python3` where there is
 * one, and otherwise `python`, the name Windows installs it under -- where
 * `python3` is often the Store's placeholder, which exits without running
 * anything.
 */
let python;
function pythonCommand() {
  python ??=
    ["python3", "python"].find((name) => {
      try {
        execFileSync(name, ["--version"], { stdio: "ignore" });
        return true;
      } catch {
        return false;
      }
    }) ?? "python3";
  return python;
}

/** `git` in `cwd`, its output trimmed, or `null` when it fails. */
function git(cwd, ...args) {
  try {
    return execFileSync("git", args, {
      cwd,
      env: childEnv(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

/**
 * Why the tree at `tree` is not a whole preparation of rust-skia `version`,
 * or `null` when it is: its checkout at the version's tag, Skia's gn in
 * place, every dependency DEPS pins checked out at its pin and unedited, and
 * none of the files `created` names, which a patch would have to make.
 *
 * What it does not check is the tracked sources, which a patched tree has
 * changed on purpose: [`restoredRefusal`] does, once they are restored.
 */
export function baseRefusal(tree, version, created = []) {
  const ready = join(tree, READY);
  if (!existsSync(ready) || readFileSync(ready, "utf8").trim() !== version)
    return `it is not a finished tree for ${version}`;
  const checkout = join(tree, "rust-skia");
  const head = git(checkout, "rev-parse", "HEAD");
  const tag = git(checkout, "rev-parse", `refs/tags/${version}^{commit}`);
  if (!head || head !== tag) return `its checkout is not at the ${version} tag`;
  const skia = sourceDir(tree);
  if (!existsSync(join(skia, "bin", "gn"))) return "it has no gn";
  let pins;
  try {
    pins = JSON.parse(
      execFileSync(pythonCommand(), ["-c", PINS, join(skia, "DEPS")], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      }),
    );
  } catch (error) {
    // The last line of what Python said, which names the error, or why it
    // did not start.
    const said = `${error.stderr ?? ""}`.trim().split("\n").pop();
    return `its DEPS could not be read: ${said || error.message}`;
  }
  for (const [directory, pin] of Object.entries(pins)) {
    const dependency = join(skia, directory);
    if (git(dependency, "rev-parse", "HEAD") !== pin)
      return `${directory} is not at the commit DEPS pins`;
    if (git(dependency, "status", "--porcelain", "--untracked-files=no") !== "")
      return `${directory} has been edited`;
  }
  const present = created.find((path) => existsSync(join(checkout, path)));
  if (present) return `${present} exists, which a patch creates`;
  return null;
}

/**
 * Why the copy at `tree`, its sources restored, is not the checkout a fresh
 * clone would make, or `null` when it is: no tracked file of rust-skia or of
 * its Skia submodule differs from its commit, and Skia is at the commit
 * rust-skia records for it.
 */
export function restoredRefusal(tree) {
  const checkout = join(tree, "rust-skia");
  const skia = sourceDir(tree);
  // First, because rust-skia's own status reports a moved submodule as a
  // change, which would name the symptom rather than the cause.
  const recorded = git(checkout, "rev-parse", "HEAD:skia-bindings/skia");
  if (!recorded || git(skia, "rev-parse", "HEAD") !== recorded)
    return "Skia is not at the commit rust-skia records";
  for (const repository of [checkout, skia])
    if (git(repository, "status", "--porcelain", "--untracked-files=no") !== "")
      return `${repository} still differs from its commit`;
  return null;
}

/**
 * Copies `from` to `to` sharing its blocks where the filesystem can -- a
 * clone on APFS, a reflink where Linux has them -- and in full where it
 * cannot.
 *
 * `cp` where there is one, because it clones a tree in one call where Node
 * copies it a file at a time; Node's copy on Windows, which has no `cp`,
 * asking for a clone and taking a full copy where none can be made. Links
 * are copied as links either way: rust-skia's checkout holds one that points
 * at a directory above it.
 */
export function copyTree(from, to, platform = process.platform) {
  if (platform === "win32") {
    cpSync(from, to, {
      recursive: true,
      verbatimSymlinks: true,
      mode: constants.COPYFILE_FICLONE,
    });
    return;
  }
  const args =
    platform === "darwin"
      ? ["-c", "-R", from, to]
      : ["-a", "--reflink=auto", from, to];
  execFileSync("cp", args, { stdio: ["ignore", process.stderr.fd, "inherit"] });
}

/**
 * Makes `partial` from another tree for `version` already under `cache`
 * rather than from the network, and returns that tree's name, or `null` when
 * none would do -- each refused with a line on stderr saying why.
 *
 * Every tree for a version has the same checkout, dependencies and gn; they
 * differ only in the patches applied to tracked files. So a base is copied,
 * its tracked sources restored by git to their commits, and the result
 * checked again before the caller patches it. The newest base is tried
 * first.
 */
export function derive(cache, partial, version, created = []) {
  const name = basename(partial).replace(/\.partial$/, "");
  let names;
  try {
    names = readdirSync(cache);
  } catch {
    return null;
  }
  const bases = names
    .filter(
      (base) =>
        base.startsWith(`skia-${version}-`) &&
        base !== name &&
        !base.endsWith(".partial"),
    )
    .map((base) => [base, statSync(join(cache, base)).mtimeMs])
    .sort((a, b) => b[1] - a[1])
    .map(([base]) => base);
  for (const base of bases) {
    const from = join(cache, base);
    const before = baseRefusal(from, version, created);
    if (before) {
      console.error(`skia-source: not deriving from ${base}: ${before}`);
      continue;
    }
    try {
      copyTree(from, partial);
      rmSync(join(partial, READY), { force: true });
      run("git", ["checkout", "--", "."], { cwd: sourceDir(partial) });
      run("git", ["checkout", "--", "."], { cwd: join(partial, "rust-skia") });
    } catch (error) {
      rmSync(partial, { recursive: true, force: true });
      console.error(`skia-source: copying ${base} failed: ${error.message}`);
      continue;
    }
    const after = restoredRefusal(partial);
    if (after) {
      rmSync(partial, { recursive: true, force: true });
      console.error(`skia-source: not deriving from ${base}: ${after}`);
      continue;
    }
    return base;
  }
  return null;
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
  if (usesLocalSkia()) return null;
  const tree = treeDir();
  if (existsSync(join(tree, READY))) return sourceDir(tree);

  const partial = `${tree}.partial`;
  rmSync(partial, { recursive: true, force: true });
  const checkout = join(partial, "rust-skia");
  const created = patches().flatMap((patch) =>
    patchCreates(readFileSync(join(ROOT, patch), "utf8")),
  );
  const base = derive(cacheRoot(), partial, lockedVersion(), created);
  if (base) console.error(`skia-source: derived ${treeName()} from ${base}`);
  else fetchTree(partial, checkout);

  // `-F0`: a hunk that no longer matches fails here rather than being placed
  // by guesswork.
  for (const patch of patches())
    run("patch", ["-p1", "-F0", "-d", checkout, "-i", join(ROOT, patch)]);
  writeFileSync(join(partial, READY), `${lockedVersion()}\n`);
  renameSync(partial, tree);

  // This checkout is one of the worktrees, so the new tree is kept with them.
  removeSuperseded(cacheRoot(), treesInUse());
  return sourceDir(tree);
}

/**
 * Fetches rust-skia at the locked tag into `checkout`, with Skia's DEPS and
 * gn: about 1 GB, said so on stderr before it starts, because nothing else
 * would show a build that is waiting on it.
 */
function fetchTree(partial, checkout) {
  console.error(
    `skia-source: no tree for ${lockedVersion()} to derive ${treeName()} ` +
      "from; fetching rust-skia, Skia's dependencies and gn, about 1 GB",
  );
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
    run(pythonCommand(), ["skia/tools/git-sync-deps"], {
      cwd: join(checkout, "skia-bindings"),
      env: {
        GIT_SYNC_DEPS_PATH: "skia/DEPS",
        GIT_SYNC_DEPS_SKIP_EMSDK: "1",
      },
    }),
  );
  retry("fetching gn", () =>
    run(pythonCommand(), ["bin/fetch-gn"], { cwd: sourceDir(partial) }),
  );
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
    if (patches().length > 0 && !usesLocalSkia())
      console.log(`SKIA_SOURCE_DIR=${sourceDir()}`);
  } else if (command === "dir") {
    console.log(sourceDir());
  } else if (command === "prepare") {
    const dir = prepare();
    if (dir !== null) console.log(dir);
  } else {
    console.error("usage: node scripts/skia-source.mjs env|dir|prepare");
    process.exit(2);
  }
}
