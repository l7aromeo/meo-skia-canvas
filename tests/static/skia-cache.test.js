// @ts-check

"use strict";

// scripts/skia-cache.mjs runs cargo for every recipe that builds Skia. Run
// with no `SKIA_SOURCE_DIR` on a platform with patches, skia-bindings builds
// against rust-skia's unpatched Skia, and every patch test then expects stock
// Skia and passes. So a build through the script refuses there, naming the
// command that sets the variable, unless `SKIA_CACHE_UNPATCHED=1` asks for
// unpatched Skia on purpose.

const assert = require("assert");
const { spawnSync } = require("child_process");
const { join } = require("path");
const { pathToFileURL } = require("url");
const { describe, test } = require("node:test");

const SCRIPT = join(__dirname, "..", "..", "scripts", "skia-cache.mjs");
const SOURCE = join(__dirname, "..", "..", "scripts", "skia-source.mjs");

/** The command the refusal has to name, as `skia-source.mjs env` is used. */
const SETS_IT = `eval "$(node scripts/skia-source.mjs env | sed 's/^/export /')"`;

/**
 * Runs the script with `args` and `vars` over an environment that names no
 * Skia tree and links no cached Skia.
 * @param {string[]} args
 * @param {Record<string, string>} [vars]
 */
function run(args, vars = {}) {
  const env = { ...process.env };
  for (const name of [
    "SKIA_SOURCE_DIR",
    "SKIA_CACHE_UNPATCHED",
    "SKIA_LIBRARY_SEARCH_PATH",
    "SKIA_BUILD_DEFINES",
  ])
    delete env[name];
  return spawnSync(process.execPath, [SCRIPT, ...args], {
    env: { ...env, ...vars },
    encoding: "utf8",
  });
}

describe("skia-cache refuses to build unpatched Skia unasked", async () => {
  // The rule `skia-source.mjs env` applies, so every case below is read the
  // same way on a platform with no patches, where nothing names a tree.
  const { namesSourceTree } = await import(pathToFileURL(SOURCE).href);
  const named = namesSourceTree();

  test("cargo with no SKIA_SOURCE_DIR refuses, naming the fix", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    const result = run(["cargo", "--version"]);
    assert.notStrictEqual(result.status, 0, result.stdout);
    assert.doesNotMatch(result.stdout, /^cargo \d/m, "cargo ran");
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
    assert.ok(result.stderr.includes("SKIA_CACHE_UNPATCHED=1"), result.stderr);
  });

  test("fill with no SKIA_SOURCE_DIR refuses before it builds", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    // A cross target, which fill refuses for a reason of its own: the
    // refusal has to come first, and name the variable.
    const result = run(["fill", "--target", "wasm32-unknown-unknown"]);
    assert.notStrictEqual(result.status, 0);
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
  });

  test("entry with no SKIA_SOURCE_DIR refuses rather than printing none", (t) => {
    if (!named) return t.skip("this platform has no Skia patches");
    const result = run(["entry"]);
    assert.notStrictEqual(result.status, 0, result.stdout);
    assert.ok(result.stderr.includes(SETS_IT), result.stderr);
  });

  test("SKIA_CACHE_UNPATCHED=1 builds unpatched Skia, and says so", () => {
    const result = run(["cargo", "--version"], { SKIA_CACHE_UNPATCHED: "1" });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.match(result.stdout, /^cargo \d/m);
    if (named) assert.match(result.stderr, /unpatched/);
  });

  test("a named tree runs cargo", () => {
    const result = run(["cargo", "--version"], { SKIA_SOURCE_DIR: "/tree" });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.match(result.stdout, /^cargo \d/m);
  });
});

describe("the refusal, case by case", async () => {
  const { unpatchedRefusal } = await import(pathToFileURL(SCRIPT).href);
  /** @type {[Record<string, string>, boolean, RegExp | null, string][]} */
  const cases = [
    [{}, true, /SKIA_SOURCE_DIR is not set/, "no tree named"],
    [{ SKIA_SOURCE_DIR: "" }, true, /SKIA_SOURCE_DIR is empty/, "an empty one"],
    [{ SKIA_SOURCE_DIR: "/tree" }, true, null, "a tree named"],
    [{}, false, null, "a platform or checkout that names no tree"],
    [{ SKIA_CACHE_UNPATCHED: "1" }, true, null, "unpatched asked for"],
    [
      { SKIA_CACHE_UNPATCHED: "yes" },
      true,
      /SKIA_CACHE_UNPATCHED is "yes"/,
      "a value other than 1",
    ],
    [
      { SKIA_CACHE_UNPATCHED: "1", SKIA_SOURCE_DIR: "/tree" },
      true,
      /SKIA_CACHE_UNPATCHED=1 asks for unpatched Skia, but SKIA_SOURCE_DIR names/,
      "unpatched asked for beside a named tree",
    ],
  ];
  for (const [env, namesTree, refused, label] of cases)
    test(label, () => {
      const why = unpatchedRefusal(env, namesTree);
      if (refused === null) assert.strictEqual(why, null);
      else assert.match(why ?? "", refused);
    });
});

// An entry's key names the macOS SDK Skia was compiled against. One SDK has
// several paths -- `MacOSX.sdk`, `MacOSX27.2.sdk` and `MacOSX27.sdk` are
// symlinks to one another -- and `MacOSX.sdk` names whichever SDK the
// installed Xcode carries, so the key is the version the SDK itself records,
// read from the directory a path resolves to.
describe("an entry's SDK is named by its version", async () => {
  const { sdkKey, currentSdk, loggedSdk, entryDir, keyFor, store, linkable } =
    await import(
      pathToFileURL(join(__dirname, "..", "..", "scripts", "skia-binaries.mjs"))
        .href
    );
  const { treeName } = await import(pathToFileURL(SOURCE).href);
  const {
    mkdirSync,
    mkdtempSync,
    realpathSync,
    rmSync,
    symlinkSync,
    writeFileSync,
  } = require("fs");
  const { tmpdir } = require("os");

  const root = realpathSync(mkdtempSync(join(tmpdir(), "skia-cache-sdk-")));
  process.on("exit", () => rmSync(root, { recursive: true, force: true }));

  /**
   * An SDK directory at `path` holding only `SDKSettings.json`, with
   * `settings` as its contents.
   * @param {string} path
   * @param {unknown} settings
   */
  function sdk(path, settings) {
    const dir = join(root, path);
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      join(dir, "SDKSettings.json"),
      typeof settings === "string" ? settings : JSON.stringify(settings),
    );
    return dir;
  }

  const xcode = sdk("Xcode/SDKs/MacOSX.sdk", { Version: "27.2" });
  const versioned = join(root, "Xcode/SDKs/MacOSX27.2.sdk");
  symlinkSync(xcode, versioned);
  const tools = sdk("CommandLineTools/SDKs/MacOSX27.sdk", { Version: "27.2" });
  const older = sdk("CommandLineTools/SDKs/MacOSX26.sdk", { Version: "26.5" });
  const updated = sdk("XcodeNext/SDKs/MacOSX.sdk", { Version: "28.0" });

  test("a directory and a symlink to it give the same key", () => {
    assert.deepStrictEqual(sdkKey(versioned), sdkKey(xcode));
    assert.deepStrictEqual(sdkKey(xcode), { sdk: "MacOSX27.2.sdk" });
  });

  test("two directories of one version under other names give one key", () => {
    assert.deepStrictEqual(sdkKey(tools), sdkKey(xcode));
  });

  test("the version is read, not parsed from the name", () => {
    assert.deepStrictEqual(sdkKey(older), { sdk: "MacOSX26.5.sdk" });
  });

  test("another version under the same name gives another key", () => {
    assert.notDeepStrictEqual(sdkKey(updated), sdkKey(xcode));
  });

  /** @type {[string, string][]} */
  const unreadable = [
    [join(root, "missing/MacOSX.sdk"), "no directory there"],
    [sdk("bare/MacOSX.sdk", {}), "no Version"],
    [sdk("garbled/MacOSX.sdk", "{"), "SDKSettings.json is not JSON"],
    [sdk("number/MacOSX.sdk", { Version: 27.2 }), "a Version that is a number"],
    [
      sdk("escape/MacOSX.sdk", { Version: "../27" }),
      "a Version that is a path",
    ],
  ];
  for (const [path, label] of unreadable)
    test(`an SDK with no readable version gives no key: ${label}`, () => {
      const key = sdkKey(path);
      assert.strictEqual(key.sdk, undefined, JSON.stringify(key));
      assert.ok(key.why?.includes(path), key.why);
    });

  test("a store and a lookup of one SDK spelled two ways find one entry", () => {
    const log = `cargo:warning=clang --sysroot=${versioned} -c a.cpp`;
    const stored = loggedSdk(log, "darwin");
    const looked = currentSdk({ SDKROOT: xcode }, "darwin");
    assert.deepStrictEqual(stored, { sdk: "MacOSX27.2.sdk" });
    const key = { tree: "t", target: "aarch64-apple-darwin", profile: "dev" };
    assert.strictEqual(
      entryDir({ ...key, sdk: stored.sdk, features: [] }, root),
      entryDir({ ...key, sdk: looked.sdk, features: [] }, root),
    );
  });

  test("a store and a lookup refuse an SDK with no readable version", () => {
    const bare = join(root, "bare/MacOSX.sdk");
    assert.match(
      loggedSdk(`--sysroot=${bare}`, "darwin").why ?? "",
      /no version/,
    );
    assert.match(
      currentSdk({ SDKROOT: bare }, "darwin").why ?? "",
      /no version/,
    );
  });

  // `keyFor` and `store` read the platform they run on, and off macOS reach
  // cargo before the SDK matters.
  const onMac = process.platform === "darwin";

  test("a lookup with an unreadable SDK finds no entry, and says why", (t) => {
    if (!onMac) return t.skip("the SDK is part of the key on macOS only");
    const bare = join(root, "bare/MacOSX.sdk");
    const { key, why } = keyFor(["build"], {
      SKIA_SOURCE_DIR: "/tree",
      SDKROOT: bare,
    });
    assert.strictEqual(key, undefined);
    assert.match(why ?? "", /records no version/);
  });

  test("a build compiled against an unreadable SDK is not stored", (t) => {
    if (!onMac) return t.skip("the SDK is part of the key on macOS only");
    const cache = join(root, "cache");
    const build = join(root, "build");
    mkdirSync(build, { recursive: true });
    const sysroot = join(root, "bare/MacOSX.sdk");
    writeFileSync(
      join(build, "output"),
      `${join(cache, treeName())}/ninja\nclang --sysroot=${sysroot} -c a.cpp\n`,
    );
    assert.throws(
      () =>
        store(build, { profile: "dev", features: [] }, () => null, { cache }),
      /is not stored: the SDK at .* records no version/,
    );
  });

  // A build that links an entry has no ninja record to name what it links,
  // so the entry keeps the record of the build that stored it.
  describe("an entry keeps what its build compiled, for the notices", () => {
    const { existsSync, readFileSync, unlinkSync } = require("fs");
    const cache = join(root, "notices-cache");
    const build = join(root, "notices-build");
    const skia = join(build, "out", "skia");
    mkdirSync(skia, { recursive: true });
    writeFileSync(
      join(build, "output"),
      `${join(cache, treeName())}/ninja\nclang --sysroot=${xcode} -c a.cpp\n`,
    );
    writeFileSync(join(skia, "libskia.a"), "skia");
    writeFileSync(join(skia, "skia-defines.txt"), "SK_X\n");
    const compiled = { tree: "/tree", projects: ["skia", "zlib"] };
    const notices = () => compiled;
    const stored = () =>
      store(build, { profile: "dev", features: [] }, () => null, {
        cache,
        notices,
      });

    test("a new entry carries the record, and is linked", () => {
      const { dir, stored: wrote } = stored();
      assert.ok(wrote);
      assert.deepStrictEqual(
        JSON.parse(readFileSync(join(dir, "notices.json"), "utf8")),
        compiled,
      );
      assert.strictEqual(
        linkable(dir, "/tree", () => null),
        null,
      );
    });

    test("an entry without the record has it derived from its seed log, and is linked", () => {
      const { dir } = stored();
      unlinkSync(join(dir, "notices.json"));
      const derived = { tree: "/tree", projects: ["skia", "expat"] };
      /** @type {[string, string][]} */
      const asked = [];
      const derive = (
        /** @type {string} */ tree,
        /** @type {string} */ log,
      ) => {
        asked.push([tree, log]);
        return derived;
      };
      assert.strictEqual(linkable(dir, "/tree", derive), null);
      assert.deepStrictEqual(
        JSON.parse(readFileSync(join(dir, "notices.json"), "utf8")),
        derived,
      );
      // Asked of the entry's own seed log, in the tree the build names.
      assert.strictEqual(asked.length, 1);
      assert.strictEqual(asked[0][0], "/tree");
      assert.match(asked[0][1], /ninja/);
    });

    test("an entry whose record cannot be derived is not linked, and storing again adds it", () => {
      const { dir } = stored();
      unlinkSync(join(dir, "notices.json"));
      assert.match(
        linkable(dir, "/tree", () => null) ?? "",
        /names no gn arguments/,
      );
      assert.match(
        linkable(dir, "/tree", () => {
          throw new Error("gn gen failed");
        }) ?? "",
        /gn could not derive one: gn gen failed/,
      );
      assert.ok(!existsSync(join(dir, "notices.json")));
      const again = stored();
      assert.strictEqual(again.stored, false);
      assert.ok(existsSync(join(dir, "notices.json")));
      assert.strictEqual(
        linkable(dir, "/tree", () => null),
        null,
      );
    });

    test("a build with other libraries is still refused", () => {
      writeFileSync(join(skia, "libskia.a"), "other");
      assert.throws(stored, /already holds other libraries/);
      writeFileSync(join(skia, "libskia.a"), "skia");
    });
  });

  test("a log naming no sysroot gives no key", () => {
    assert.match(loggedSdk("clang -c a.cpp", "darwin").why ?? "", /--sysroot/);
  });

  test("off macOS the SDK is none", () => {
    assert.deepStrictEqual(currentSdk({ SDKROOT: xcode }, "linux"), {
      sdk: "none",
    });
    assert.deepStrictEqual(loggedSdk("", "linux"), { sdk: "none" });
  });
});
