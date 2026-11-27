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
// byte, then checks the entry's log -- see `bindRefusal` there. It also keeps
// what that build compiled, `notices.json`, which the third-party notices of a
// build linking it are written from.
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
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildScriptLogs } from "./cargo-build-dir.mjs";
import { record, recordFromLog } from "./notices.mjs";
import { cacheRoot, treeName } from "./skia-source.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/// The directory under the cache root that holds every entry. Not named
/// `skia-*`: `prepare` removes each `skia-*` name it does not recognise as a
/// tree -- see `removeSuperseded` in scripts/skia-source.mjs.
export const BINARIES = "binaries";

/// The library skia-bindings compiles from its own sources in every build,
/// cached or not, and so never takes from an entry.
export const BINDINGS_LIBRARY = "libskia-bindings.a";

/// The file skia-bindings saves the build's definitions to, beside the
/// libraries (`save_definitions`, skia_bindgen.rs), which `SKIA_BUILD_DEFINES`
/// takes back.
const DEFINES = "skia-defines.txt";

/// The manifest's layout. A cache written by another layout is not read:
/// entries of layout 1 name no compiler.
export const MANIFEST_VERSION = 2;

/// What an entry's build compiled, for the notices of a build that links
/// it: see `record` in scripts/notices.mjs.
export const NOTICES_RECORD = "notices.json";

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

/// The file in a macOS SDK directory that records the SDK's version, as its
/// `Version` field.
const SDK_SETTINGS = "SDKSettings.json";

/// What an SDK's `Version` has to look like to name an entry: dotted
/// numbers, so that the key cannot reach outside its directory.
const SDK_VERSION = /^\d+(\.\d+)*$/;

/**
 * The key naming the macOS SDK at `path` -- `{ sdk: "MacOSX27.2.sdk" }`, by
 * the version the SDK records -- or `{ why }` where it records none.
 *
 * One SDK has several paths: `MacOSX.sdk`, `MacOSX27.2.sdk` and
 * `MacOSX27.sdk` are symlinks to one another, and the Command Line Tools
 * carry a copy of their own. `MacOSX.sdk` also names whichever SDK the
 * installed Xcode carries, so a directory's name can match after an update
 * that replaced what it names. The version is read from the SDK itself,
 * through any symlinks, and never parsed from a name: `MacOSX27.sdk` is 27.2.
 * `xcrun --sdk <path> --show-sdk-version` is not used: it cannot locate a
 * Command Line Tools SDK while Xcode is the selected developer directory.
 */
export function sdkKey(path) {
  let version;
  try {
    ({ Version: version } = JSON.parse(
      readFileSync(join(path, SDK_SETTINGS), "utf8"),
    ));
  } catch (error) {
    return { why: `the SDK at ${path} records no version: ${error.message}` };
  }
  if (typeof version !== "string" || !SDK_VERSION.test(version))
    return {
      why: `the SDK at ${path} records no version: ${SDK_SETTINGS} gives Version ${JSON.stringify(version)}`,
    };
  return { sdk: `MacOSX${version}.sdk` };
}

/**
 * The SDK skia-bindings compiles against on macOS, as `sdkKey` names it:
 * `SDKROOT` where it is set, and otherwise the one xcrun gives. Skia rebuilt
 * against another SDK is another build. `{ sdk: "none" }` elsewhere.
 */
export function currentSdk(env = process.env, platform = process.platform) {
  if (platform !== "darwin") return { sdk: "none" };
  return sdkKey(
    env.SDKROOT ||
      execFileSync("xcrun", ["--sdk", "macosx", "--show-sdk-path"], {
        encoding: "utf8",
      }).trim(),
  );
}

/** The SDK a build's log says Skia was compiled against, as `currentSdk`. */
export function loggedSdk(log, platform = process.platform) {
  if (platform !== "darwin") return { sdk: "none" };
  const sysroot = log.match(/--sysroot=([^",\s]+)/);
  return sysroot ? sdkKey(sysroot[1]) : { why: "the log names no --sysroot" };
}

/// The variables skia-bindings takes Skia's C and C++ compilers from, the
/// first one set winning, and the command it runs where none is:
/// `BuildConfiguration::from_features` in skia-bindings 0.156.0's
/// build_support/skia/config.rs. Its build script reruns when any of them
/// changes.
const COMPILERS = [
  { role: "cc", names: ["CLANGCC", "CC"], otherwise: "clang" },
  { role: "cxx", names: ["CLANGCXX", "CXX"], otherwise: "clang++" },
];

/** The first line `command --version` prints, or `null` where it fails. */
function versionLine(command) {
  try {
    const out = execFileSync(command, ["--version"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return out.split("\n")[0].trim() || null;
  } catch {
    return null;
  }
}

/**
 * The compilers skia-bindings would compile Skia with under `env`, each by
 * the first line its `--version` prints, as `{ compiler: { cc, cxx } }`, or
 * `{ why }` where one cannot be named. `version` runs a command with
 * `--version`; a test replaces it.
 *
 * The line names the compiler's release -- `Apple clang version 21.0.0
 * (clang-2100.4.1.1)` -- so an Xcode or Command Line Tools update that
 * changes clang changes the key, whether or not it changes the SDK. A
 * command with arguments, such as `CC="clang -O0"` or `ccache clang`, is not
 * named: what the arguments do to the objects is not in the line.
 */
export function compilerKey(env = process.env, version = versionLine) {
  const compiler = {};
  for (const { role, names, otherwise } of COMPILERS) {
    const name = names.find((n) => env[n] !== undefined);
    const command = name ? env[name] : otherwise;
    if (command === "") return { why: `${name} is empty` };
    if (/\s/.test(command))
      return {
        why: `${name} is ${JSON.stringify(command)}, a command with arguments, which no version names`,
      };
    const line = version(command);
    if (!line) return { why: `\`${command} --version\` named no version` };
    // A line with no letter or digit keeps only its dots in the entry's
    // directory name -- see `compilerName` -- and `..` is the directory
    // above.
    if (!NAMES_SOMETHING.test(line))
      return {
        why: `\`${command} --version\` printed ${JSON.stringify(line)}, which names no version`,
      };
    compiler[role] = line;
  }
  return { compiler };
}

/// What a version line keeps in an entry's directory name. Every other run
/// of characters, a path separator of any platform among them, becomes one
/// `-`, so that the name cannot reach outside its directory -- given a line
/// that holds a letter or a digit, which `compilerKey` requires. The
/// manifest keeps the lines whole.
const NAME_KEEPS = /[^A-Za-z0-9.]+/g;

/// What a version line has to hold for `compilerKey` to name a compiler by
/// it: a letter or a digit, without which the name `compilerName` makes of
/// it is dots alone, and `.` or `..` is no directory of its own.
const NAMES_SOMETHING = /[A-Za-z0-9]/;

/**
 * The directory name of `compiler`, as `compilerKey` gives it: the version
 * line once where both compilers print the same one, as clang and clang++
 * of one release do.
 */
function compilerName({ cc, cxx }) {
  const name = (line) => line.replace(NAME_KEEPS, "-").replace(/^-|-$/g, "");
  return cc === cxx ? name(cc) : `${name(cc)}+${name(cxx)}`;
}

/** Whether two compilers, as `compilerKey` gives them, are the same. */
function sameCompiler(a, b) {
  return a?.cc === b?.cc && a?.cxx === b?.cxx;
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
 * or `null` where cargo cannot say. `env` is cargo's environment.
 *
 * Asked offline first. Where that fails -- a cargo home that holds no
 * registry yet, as on a fresh CI runner, where the tree fails before the
 * build has fetched anything -- the lock file's crates for the target are
 * fetched and the tree asked again. The build fetches those same crates
 * moments later, so the fetch costs it nothing. Where there is no network,
 * or `CARGO_NET_OFFLINE` is set, the fetch fails and the answer stays `null`.
 */
export function skiaFeatures(featureArgs, root = ROOT, env = process.env) {
  const tree = () =>
    execFileSync(
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
      { cwd: root, env, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    )
      .trim()
      .split(",")
      .filter(Boolean)
      .sort();
  try {
    return tree();
  } catch {
    // Fetched below.
  }
  const at = featureArgs.indexOf("--target");
  const target = at >= 0 ? featureArgs[at + 1] : hostTarget();
  try {
    execFileSync("cargo", ["fetch", "--locked", "--target", target], {
      cwd: root,
      env,
      stdio: "ignore",
    });
    return tree();
  } catch {
    return null;
  }
}

/**
 * The directory of the entry for `key` under `cache`: the tree, then where,
 * by what and how it was compiled, then skia-bindings' features.
 */
export function entryDir(
  { tree, target, sdk, compiler, profile, features },
  cache = cacheRoot(),
) {
  return join(
    cache,
    BINARIES,
    tree,
    target,
    sdk,
    compilerName(compiler),
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
  const { sdk, why } = currentSdk(env);
  if (!sdk) return { why };
  const { compiler, why: unnamed } = compilerKey(env);
  if (!compiler) return { why: unnamed };
  const features = skiaFeatures(read.features, root);
  if (!features) return { why: "cargo tree could not resolve the features" };
  return {
    key: {
      tree: treeName(root),
      target: hostTarget(),
      sdk,
      compiler,
      profile: read.profile,
      features,
    },
  };
}

/**
 * Why the entry at `dir` cannot be linked by a build whose compiler is
 * `compiler`, as `compilerKey` gives it, or `null` where it can. Two version
 * lines can share a directory name, so the compiler the manifest names
 * whole has to be this one. A build
 * that links an entry runs no ninja of its own, so its notices come from the
 * record the entry keeps. An entry stored before entries kept one has it
 * derived here from the gn arguments its seed build printed -- `derive`,
 * given the Skia tree `tree` and that log -- and written, so it is linked as
 * before; one whose record cannot be derived is not linked, and the build
 * that compiles instead stores one.
 */
export function linkable(dir, tree, compiler, derive = recordFromLog) {
  const manifest = readManifest(dir);
  if (!manifest) return `no entry at ${dir}`;
  if (!sameCompiler(manifest.compiler, compiler))
    return `${dir} was compiled by ${JSON.stringify(manifest.compiler)}`;
  if (existsSync(join(dir, NOTICES_RECORD))) return null;
  let derived;
  try {
    derived = tree ? derive(tree, seedLog(dir)) : null;
  } catch (error) {
    return `${dir} holds no ${NOTICES_RECORD}, and gn could not derive one: ${error.message}`;
  }
  if (!derived)
    return `${dir} holds no ${NOTICES_RECORD}, and its seed log names no gn arguments to derive one from`;
  const partial = join(dir, `${NOTICES_RECORD}.partial-${process.pid}`);
  writeFileSync(partial, `${JSON.stringify(derived, null, 2)}\n`);
  renameSync(partial, join(dir, NOTICES_RECORD));
  return null;
}

/**
 * The variables that link the entry for `args`, and that entry, or none
 * where there is none -- with the reason, for the caller to print, and the
 * key where `args` have one, under which the build that compiles instead
 * can be stored.
 */
export function cacheEnv(args, env = process.env, root = ROOT) {
  const { key, why } = keyFor(args, env, root);
  if (!key) return { why };
  const dir = entryDir(key);
  const unlinkable = linkable(dir, env.SKIA_SOURCE_DIR, key.compiler);
  if (unlinkable) return { why: unlinkable, key };
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

/// What every ar archive opens with, and the size of the header before each
/// member: `ARMAG` and `struct ar_hdr` in `<ar.h>`.
const AR_MAGIC = "!<arch>\n";
const AR_HEADER = 60;

/// Where `struct ar_hdr` keeps a member's name and its size in bytes, as
/// `[start, end)` within the header: `ar_name[16]`, then `ar_date[12]`,
/// `ar_uid[6]`, `ar_gid[6]` and `ar_mode[8]` before `ar_size[10]`.
const AR_NAME = [0, 16];
const AR_SIZE = [48, 58];

/// BSD's mark for a name kept after the header, its length following:
/// `AR_EFMT1` in `<ar.h>`. Apple's `ar` and `libtool` write every name so.
const BSD_LONG_NAME = "#1/";

/// System V's table of the names too long for a header, named `//`; a member
/// named `/<offset>` has its name there, ended by `/\n`.
const SYSV_NAMES = "//";

/// The members that index the others' symbols, rebuilt whenever the
/// archive is written: BSD's `__.SYMDEF` family and System V's `/` and
/// `/SYM64/`. Apple's `ranlib` records the time it ran in one, so two
/// archives of the same objects differ there.
const SYMBOL_INDEX = /^(?:__\.SYMDEF(?:_64)?(?: SORTED)?|\/|\/SYM64\/)$/;

/**
 * The members of the ar archive `bytes`, in order, each `{ name, digest }`
 * with the digest over its contents alone -- not its header, whose date
 * changes with every build -- and the symbol index left out. `null` where
 * `bytes` is not an archive this reads.
 */
export function archiveMembers(bytes) {
  if (bytes.toString("latin1", 0, AR_MAGIC.length) !== AR_MAGIC) return null;
  const members = [];
  let names = null;
  for (let at = AR_MAGIC.length; at < bytes.length;) {
    if (at + AR_HEADER > bytes.length) return null;
    const field = ([from, to]) =>
      bytes.toString("latin1", at + from, at + to).trimEnd();
    const sized = field(AR_SIZE);
    if (!/^\d+$/.test(sized)) return null;
    const size = Number(sized);
    const start = at + AR_HEADER;
    if (start + size > bytes.length) return null;
    let name = field(AR_NAME);
    let data = bytes.subarray(start, start + size);
    if (name === SYSV_NAMES) names = data.toString("latin1");
    else if (name.startsWith(BSD_LONG_NAME)) {
      const length = Number(name.slice(BSD_LONG_NAME.length));
      if (!Number.isInteger(length) || length > size) return null;
      name = data.toString("latin1", 0, length).replace(/\0+$/, "");
      data = data.subarray(length);
    } else if (/^\/\d+$/.test(name)) {
      if (names === null) return null;
      name = names.slice(Number(name.slice(1))).match(/^[^/\n\0]*/)[0];
    } else if (
      name.length > 1 &&
      name.endsWith("/") &&
      !SYMBOL_INDEX.test(name)
    )
      name = name.slice(0, -1);
    if (name !== SYSV_NAMES && !SYMBOL_INDEX.test(name))
      members.push({
        name,
        digest: `sha256:${createHash("sha256").update(data).digest("hex")}`,
      });
    // Each member starts on an even offset.
    at = start + size + (size % 2);
  }
  return members;
}

/**
 * Where the archive `built` holds other objects than `held`, naming the
 * first member that differs, or `null` where they hold the same objects in
 * the same order.
 */
export function archiveDifference(held, built) {
  const [was, is] = [archiveMembers(held), archiveMembers(built)];
  if (!was || !is)
    return `${was ? "this build's" : "the entry's"} is not an ar archive`;
  for (let i = 0; i < Math.max(was.length, is.length); i += 1) {
    if (!was[i]) return `this build's has ${is[i].name} as well`;
    if (!is[i]) return `this build's has no ${was[i].name}`;
    if (was[i].name !== is[i].name)
      return `member ${i + 1} is ${was[i].name} in the entry and ${is[i].name} in this build`;
    if (was[i].digest !== is[i].digest) return `its ${was[i].name} differs`;
  }
  return null;
}

/**
 * Where the build in `skia`, about to be stored as `built`, differs from the
 * entry in `dir` that `held` describes, or `null` where it holds what the
 * entry holds. Two builds of one key never agree byte for byte: `ar` records
 * each member's date and `ranlib` its own time, so archives are compared by
 * their members, the definitions file by its bytes.
 */
function entryDifference(dir, held, skia, built) {
  if (!sameCompiler(held.compiler, built.compiler))
    return `it was compiled by ${JSON.stringify(held.compiler)}`;
  const names = Object.keys(built.libraries);
  const heldNames = Object.keys(held.libraries);
  const extra = names.find((name) => !heldNames.includes(name));
  if (extra) return `this build has ${extra}, which the entry does not`;
  const missing = heldNames.find((name) => !names.includes(name));
  if (missing) return `this build has no ${missing}`;
  for (const name of names) {
    if (held.libraries[name] === built.libraries[name]) continue;
    const why = archiveDifference(
      readFileSync(join(dir, name)),
      readFileSync(join(skia, name)),
    );
    if (why) return `${name}: ${why}`;
  }
  if (
    !readFileSync(join(dir, DEFINES)).equals(readFileSync(join(skia, DEFINES)))
  )
    return `its ${DEFINES} differs`;
  return null;
}

/**
 * Stores the build in `buildDir` -- the unit directory skia-bindings' build
 * script ran in, see scripts/cargo-build-dir.mjs -- as the entry for
 * `profile`, `features` and `compiler`, once `vouch` finds nothing wrong with
 * its log. Written beside the entry and renamed into place, so a reader
 * never finds half an entry.
 *
 * `compiler` is what `compilerKey` named when the build started, at `since`,
 * a time in milliseconds as `Date.now` gives it. A build whose script did
 * not run after that -- one cargo found fresh -- is refused: its log does
 * not say which clang compiled it, and an update since would key it under
 * the wrong one.
 *
 * An entry already there holding the same objects is kept, given this
 * build's notices record where it has none. One holding others is refused,
 * naming the first difference, since two builds of one key ought to agree.
 */
export function store(
  buildDir,
  { profile, features, compiler, since },
  vouch,
  { cache = cacheRoot(), root = ROOT, notices = record } = {},
) {
  const logs = buildScriptLogs(buildDir);
  if (!(statSync(logs.output).mtimeMs >= since))
    throw new Error(
      `skia-cache: ${buildDir} is not stored: its build script did not run in this build, so which compiler built it is not known`,
    );
  const log = [
    readFileSync(logs.output, "utf8"),
    existsSync(logs.stderr) ? readFileSync(logs.stderr, "utf8") : "",
  ].join("\n");
  const why = vouch(log);
  if (why) throw new Error(`skia-cache: ${buildDir} is not stored: ${why}`);
  const tree = treeName(root);
  if (!log.includes(`${join(cache, tree)}/`))
    throw new Error(
      `skia-cache: ${buildDir} was not built from ${tree}, this worktree's tree`,
    );
  const { sdk, why: noSdk } = loggedSdk(log);
  if (!sdk) throw new Error(`skia-cache: ${buildDir} is not stored: ${noSdk}`);
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
    compiler,
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
  const noticesRecord = `${JSON.stringify(notices(skia), null, 2)}\n`;
  const existing = readManifest(dir);
  if (existing) {
    const differs = entryDifference(dir, existing, skia, manifest);
    if (differs)
      throw new Error(
        `skia-cache: ${dir} already holds other libraries for the same key: ${differs}`,
      );
    // The same objects, from an entry stored before entries carried their
    // notices record: the record is this build's, so it is theirs.
    if (!existsSync(join(dir, NOTICES_RECORD))) {
      const partialRecord = join(
        dir,
        `${NOTICES_RECORD}.partial-${process.pid}`,
      );
      writeFileSync(partialRecord, noticesRecord);
      renameSync(partialRecord, join(dir, NOTICES_RECORD));
    }
    return { dir, stored: false };
  }
  const partial = `${dir}.partial-${process.pid}`;
  rmSync(partial, { recursive: true, force: true });
  mkdirSync(partial, { recursive: true });
  try {
    for (const name of [...libraries, DEFINES])
      copyFileSync(join(skia, name), join(partial, name));
    copyFileSync(logs.output, join(partial, "seed-output"));
    if (existsSync(logs.stderr))
      copyFileSync(logs.stderr, join(partial, "seed-stderr"));
    // Hashed from the copies, so the manifest names the bytes the entry holds.
    for (const name of libraries)
      if (digest(join(partial, name)) !== manifest.libraries[name])
        throw new Error(`skia-cache: ${name} changed while it was copied`);
    writeFileSync(join(partial, NOTICES_RECORD), noticesRecord);
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
