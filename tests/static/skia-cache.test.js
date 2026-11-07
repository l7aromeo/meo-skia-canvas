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
