const assert = require("assert");
const { describe, test } = require("node:test");
const { PLATFORM_PACKAGES, loadSkiaNode } = require("../../lib/binary");
const manifest = require("../../package.json");

// Targets are named in three places that must agree: the `prebuild` hashes, the
// `optionalDependencies` the loader probes for, and the map inside lib/binary.js. Adding a
// platform to one and forgetting another fails silently — the package resolves nothing and quietly
// falls back to the install script, which is the behaviour all of this exists to replace.
const triplets = Object.keys(manifest.prebuild)
  .filter((asset) => asset.endsWith(".gz"))
  .map((asset) => asset.replace(/\.gz$/, ""));

const declared = Object.values(PLATFORM_PACKAGES).flatMap((byArch) => Object.values(byArch).flat());

describe("native binary resolution", () => {
  test("every prebuilt target has an optional dependency", () => {
    const expected = triplets.map((triplet) => `phyron-skia-canvas-${triplet}`).sort();
    assert.deepStrictEqual(Object.keys(manifest.optionalDependencies || {}).sort(), expected);
  });

  test("every prebuilt target is reachable from the loader", () => {
    const expected = triplets.map((triplet) => `phyron-skia-canvas-${triplet}`).sort();
    assert.deepStrictEqual([...declared].sort(), expected);
  });

  // Exact pins, as sharp and esbuild do: a range would let a consumer resolve a binary built from
  // different sources than the JavaScript wrapping it.
  test("optional dependencies pin the current version", () => {
    for (const [name, range] of Object.entries(manifest.optionalDependencies || {})) {
      assert.strictEqual(range, manifest.version, `${name} must pin ${manifest.version}`);
    }
  });

  test("resolves a usable binary on this host", () => {
    const skiaNode = loadSkiaNode();
    assert.strictEqual(typeof skiaNode.backend, "function");
    assert.ok(Object.keys(skiaNode).length > 0);
  });
});
