// The entries of the local cache of compiled, patched Skia: where each is,
// what it holds, and which one a cargo invocation would link. The commands
// that use them are scripts/skia-cache.mjs; this module runs nothing itself,
// so that scripts/check-patched-skia.mjs can read entries without importing
// those.
//
// skia-bindings compiles Skia with ninja into its out-dir, about 1,730 steps,
// and an out-dir does not survive a copy to a worktree at another depth: its
// generated build files reach the source through relative paths. Where
// `SKIA_LIBRARY_SEARCH_PATH` is set beside `SKIA_SOURCE_DIR`, its build script
// copies the libraries from that directory instead and runs no ninja: it binds
// against them, generating `bindings.rs` from the patched source's headers and
// compiling its own `src/bindings.cpp`, with the definitions in
// `SKIA_BUILD_DEFINES` (skia-bindings 0.156.0, `build.rs` lines 58-72). So an
// entry holds the libraries a build compiled and the definitions it saved
// beside them, and is linked by setting those two variables.
//
// rust-skia's own prebuilt-binaries route is not used. It is reached only
// with `SKIA_SOURCE_DIR` unset, keys an archive by rust-skia's commit rather
// than by this repository's patches, verifies nothing it unpacks, and falls
// back to compiling crates.io's unpatched Skia when the archive is missing.
//
// An entry is stored only from a build `check-patched-skia` passes, and keeps
// that build's log, so that a build linking the entry can be checked against
// it: `check-patched-skia` matches the linked libraries to an entry byte for
// byte, then checks the entry's log -- see `bindRefusal` there.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { cacheRoot, treeName } from "./skia-source.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// The directory under the cache root that holds every entry. Not named
/// `skia-*`: `prepare` removes each `skia-*` name it does not recognise as a
/// tree -- see `removeSuperseded` in scripts/skia-source.mjs.
export const BINARIES = "binaries";

/// The library skia-bindings compiles from its own sources in every build,
/// cached or not, and so never takes from an entry.
const BINDINGS_LIBRARY = "libskia-bindings.a";

/// The file skia-bindings saves the build's definitions to, beside the
/// libraries (`save_definitions`, skia_bindgen.rs), which `SKIA_BUILD_DEFINES`
/// takes back.
const DEFINES = "skia-defines.txt";

/// The manifest's layout. A cache written by another layout is not read.
const MANIFEST_VERSION = 1;

/// Variables under which an entry would not be what the build compiles:
/// skia-bindings reads each into the Skia it builds, and none is part of an
/// entry's key. Set, the cache is not used.
const UNCACHED_WITH = [
  "SKIA_DEBUG",
  "SKIA_GN_ARGS",
  "SKIA_USE_SYSTEM_LIBRARIES",
  "SKIA_LIBRARY_SEARCH_PATH",
  "SKIA_BUILD_DEFINES",
  "SKIA_NINJA_COMMAND",
  "SKIA_GN_COMMAND",
];

/** `sha256:<hex>` of the file at `path`. */
export function digest(path) {
  return `sha256:${createHash("sha256").update(readFileSync(path)).digest("hex")}`;
}

/** The libraries in a skia-bindings `out/skia` that an entry carries. */
export function cachedLibraries(dir) {
  return readdirSync(dir)
    .filter((name) => /^lib.+\.a$/.test(name) && name !== BINDINGS_LIBRARY)
    .sort();
}

/**
 * The host target triple, which skia-bindings builds for when cargo is given
 * no `--target`.
 */
function hostTarget() {
  const info = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const host = info.match(/^host: (\S+)$/m);
  if (!host) throw new Error("skia-cache: rustc -vV names no host");
  return host[1];
}

/**
 * The SDK skia-bindings compiles against on macOS, by its directory's name
 * (`MacOSX27.2.sdk`): `SDKROOT` where it is set, and otherwise the one xcrun
 * gives. Skia rebuilt against another SDK is another build. `none` elsewhere.
 */
function currentSdk() {
  if (process.platform !== "darwin") return "none";
  const path =
    process.env.SDKROOT ||
    execFileSync("xcrun", ["--sdk", "macosx", "--show-sdk-path"], {
      encoding: "utf8",
    }).trim();
  return basename(path);
}

/** The SDK a build's log says Skia was compiled against, as `currentSdk`. */
function loggedSdk(log) {
  if (process.platform !== "darwin") return "none";
  const sysroot = log.match(/--sysroot=([^",\s]+)/);
  return sysroot ? basename(sysroot[1]) : null;
}

/// cargo's flags that choose features, and those that take a value.
const FEATURE_FLAGS = new Set([
  "--features",
  "-F",
  "--no-default-features",
  "--all-features",
]);
const TAKES_VALUE = new Set(["--features", "-F", "--profile"]);

/**
 * What `args`, given to cargo, choose: the feature flags, as cargo tree takes
 * them, and the profile skia-bindings' build script runs in. `null` where the
 * cache cannot say what they build: a cross target or another manifest.
 */
export function readArgs(args) {
  const features = [];
  let profile = "dev";
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === "--") break;
    if (arg === "--target" || arg.startsWith("--target=")) return null;
    if (arg === "--manifest-path" || arg.startsWith("--manifest-path="))
      return null;
    if (arg === "--release" || arg === "-r") profile = "release";
    else if (arg.startsWith("--profile=")) profile = arg.slice(10);
    else if (arg === "--profile") profile = args[++i];
    else if (FEATURE_FLAGS.has(arg)) {
      features.push(arg);
      if (TAKES_VALUE.has(arg)) features.push(args[++i]);
    } else if (/^--features=|^-F./.test(arg)) features.push(arg);
  }
  // `test` inherits `dev` and `bench` `release`, overrides included -- see
  // `[profile.dev.package.skia-bindings]` in Cargo.toml.
  const inherits = { test: "dev", bench: "release" };
  return { features, profile: inherits[profile] ?? profile };
}

/**
 * The features cargo resolves for skia-bindings under `featureArgs`, sorted,
 * or `null` where cargo cannot say without the network.
 */
export function skiaFeatures(featureArgs, root = ROOT) {
  try {
    const line = execFileSync(
      "cargo",
      [
        "tree",
        "--offline",
        "--edges",
        "features",
        "--invert",
        "skia-bindings",
        "--depth",
        "0",
        "--format",
        "{f}",
        ...featureArgs,
      ],
      { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    ).trim();
    return line.split(",").filter(Boolean).sort();
  } catch {
    return null;
  }
}

/**
 * The directory of the entry for `key` under `cache`: the tree, then where
 * and how it was compiled, then skia-bindings' features.
 */
export function entryDir(
  { tree, target, sdk, profile, features },
  cache = cacheRoot(),
) {
  return join(
    cache,
    BINARIES,
    tree,
    target,
    sdk,
    profile,
    features.join("+") || "none",
  );
}

/** The manifest of the entry in `dir`, or `null` where there is none. */
export function readManifest(dir) {
  try {
    const manifest = JSON.parse(readFileSync(join(dir, "manifest.json")));
    return manifest.version === MANIFEST_VERSION ? manifest : null;
  } catch {
    return null;
  }
}

/** Every entry under `cache` for `tree`, as `[dir, manifest]`. */
export function entriesFor(tree, cache = cacheRoot()) {
  const found = [];
  const walk = (dir) => {
    let names;
    try {
      names = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    const manifest = readManifest(dir);
    if (manifest) found.push([dir, manifest]);
    for (const entry of names)
      if (entry.isDirectory() && !entry.name.includes(".partial"))
        walk(join(dir, entry.name));
  };
  walk(join(cache, BINARIES, tree));
  return found;
}

/**
 * The key cargo given `args` would build Skia under in this worktree, or the
 * reason there is none.
 */
export function keyFor(args, env = process.env, root = ROOT) {
  const unsafe = UNCACHED_WITH.find((name) => env[name] !== undefined);
  if (unsafe) return { why: `${unsafe} is set` };
  if (!env.SKIA_SOURCE_DIR) return { why: "SKIA_SOURCE_DIR is not set" };
  const read = readArgs(args);
  if (!read) return { why: "a cross target or another manifest" };
  const features = skiaFeatures(read.features, root);
  if (!features) return { why: "cargo tree could not resolve the features" };
  return {
    key: {
      tree: treeName(root),
      target: hostTarget(),
      sdk: currentSdk(),
      profile: read.profile,
      features,
    },
  };
}

/**
 * The variables that link the entry for `args`, and that entry, or `null`
 * where there is none -- with the reason, for the caller to print.
 */
export function cacheEnv(args, env = process.env, root = ROOT) {
  const { key, why } = keyFor(args, env, root);
  if (!key) return { why };
  const dir = entryDir(key);
  const manifest = readManifest(dir);
  if (!manifest) return { why: `no entry at ${dir}` };
  return {
    dir,
    env: {
      SKIA_LIBRARY_SEARCH_PATH: dir,
      // One definition a line in the file; skia-bindings splits on whitespace.
      SKIA_BUILD_DEFINES: readFileSync(join(dir, DEFINES), "utf8")
        .split(/\s+/)
        .filter(Boolean)
        .join(" "),
    },
  };
}

/**
 * Stores the build in `buildDir` -- a `target/<profile>/build/skia-bindings-*`
 * directory -- as the entry for `profile` and `features`, once `vouch` finds
 * nothing wrong with its log. Written beside the entry and renamed into
 * place, so a reader never finds half an entry. An entry already there with
 * the same libraries is kept; one with others is refused, since two builds of
 * one key ought to agree.
 */
export function store(
  buildDir,
  { profile, features },
  vouch,
  { cache = cacheRoot(), root = ROOT } = {},
) {
  const log = [
    readFileSync(join(buildDir, "output"), "utf8"),
    existsSync(join(buildDir, "stderr"))
      ? readFileSync(join(buildDir, "stderr"), "utf8")
      : "",
  ].join("\n");
  const why = vouch(log);
  if (why) throw new Error(`skia-cache: ${buildDir} is not stored: ${why}`);
  const tree = treeName(root);
  if (!log.includes(`${join(cache, tree)}/`))
    throw new Error(
      `skia-cache: ${buildDir} was not built from ${tree}, this worktree's tree`,
    );
  const sdk = loggedSdk(log);
  if (!sdk) throw new Error(`skia-cache: ${buildDir}'s log names no --sysroot`);
  const skia = join(buildDir, "out", "skia");
  const libraries = cachedLibraries(skia);
  if (!libraries.includes("libskia.a"))
    throw new Error(`skia-cache: ${skia} holds no libskia.a`);
  if (!existsSync(join(skia, DEFINES)))
    throw new Error(`skia-cache: ${skia} holds no ${DEFINES}`);
  const key = {
    tree,
    target: hostTarget(),
    sdk,
    profile,
    features: [...features].sort(),
  };
  const dir = entryDir(key, cache);
  const manifest = {
    version: MANIFEST_VERSION,
    ...key,
    libraries: Object.fromEntries(
      libraries.map((name) => [name, digest(join(skia, name))]),
    ),
    seed: resolve(buildDir),
  };
  const existing = readManifest(dir);
  if (existing) {
    if (
      JSON.stringify(existing.libraries) === JSON.stringify(manifest.libraries)
    )
      return { dir, stored: false };
    throw new Error(
      `skia-cache: ${dir} already holds other libraries for the same key`,
    );
  }
  const partial = `${dir}.partial-${process.pid}`;
  rmSync(partial, { recursive: true, force: true });
  mkdirSync(partial, { recursive: true });
  try {
    for (const name of [...libraries, DEFINES])
      copyFileSync(join(skia, name), join(partial, name));
    writeFileSync(
      join(partial, "seed-output"),
      readFileSync(join(buildDir, "output")),
    );
    if (existsSync(join(buildDir, "stderr")))
      copyFileSync(join(buildDir, "stderr"), join(partial, "seed-stderr"));
    // Hashed from the copies, so the manifest names the bytes the entry holds.
    for (const name of libraries)
      if (digest(join(partial, name)) !== manifest.libraries[name])
        throw new Error(`skia-cache: ${name} changed while it was copied`);
    writeFileSync(
      join(partial, "manifest.json"),
      `${JSON.stringify(manifest, null, 2)}\n`,
    );
    renameSync(partial, dir);
  } catch (error) {
    rmSync(partial, { recursive: true, force: true });
    throw error;
  }
  return { dir, stored: true };
}

/** What a stored entry's seed build printed, both streams, as `buildLog` reads them. */
export function seedLog(dir) {
  const stderr = join(dir, "seed-stderr");
  return [
    readFileSync(join(dir, "seed-output"), "utf8"),
    existsSync(stderr) ? readFileSync(stderr, "utf8") : "",
  ].join("\n");
}
