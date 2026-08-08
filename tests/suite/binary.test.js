const assert = require("assert");
const { existsSync } = require("fs");
const { join } = require("path");
const { describe, test } = require("node:test");
const { PLATFORM_PACKAGES, loadSkiaNode } = require("../../lib/binary");
const manifest = require("../../package.json");

// Targets are named in three places that must agree: the map in lib/binary.js, the
// `optionalDependencies` it probes for, and the `prebuild` hashes. Adding a platform to one and
// forgetting another fails silently — resolution finds nothing and falls back to the install
// script, which is the behaviour all of this exists to replace.
//
// The loader map is the source of truth rather than `prebuild`, which is legitimately empty until
// this repository cuts its first release and `npm run snapshot` fills it in.
const declared = Object.values(PLATFORM_PACKAGES)
  .flatMap((byArch) => Object.values(byArch).flat())
  .sort();

const prebuilt = Object.keys(manifest.prebuild || {})
  .filter((asset) => asset.endsWith(".gz"))
  .map((asset) => `meo-skia-canvas-${asset.replace(/\.gz$/, "")}`)
  .sort();

// `optionalDependencies` cannot be declared before the platform packages exist on the registry:
// npm records the declaration but resolves no `packages` entry for a name it cannot fetch, and
// `npm ci` then refuses the lockfile as out of sync. They are added in the release that follows
// the first platform-package publish, so their absence is a valid bootstrap state and not drift.
const optional = Object.keys(manifest.optionalDependencies || {}).sort();

describe("native binary resolution", () => {
  test("every target the loader probes has an optional dependency", { skip: optional.length === 0 && "platform packages not published yet" }, () => {
    assert.deepStrictEqual(optional, declared);
  });

  test("every released binary has a platform package", { skip: prebuilt.length === 0 && "no release snapshotted yet" }, () => {
    assert.deepStrictEqual(prebuilt, declared);
  });

  // Exact pins, as sharp and esbuild do: a range would let a consumer resolve a binary built from
  // different sources than the JavaScript wrapping it.
  test("optional dependencies pin the current version", () => {
    for (const [name, range] of Object.entries(manifest.optionalDependencies || {})) {
      assert.strictEqual(range, manifest.version, `${name} must pin ${manifest.version}`);
    }
  });

  // Skipped where no binary exists at all: before the first release there is nothing to download
  // and nothing to resolve, and failing here would report that as a defect in the loader.
  const resolvable = existsSync(join(__dirname, "../../lib/skia.node"));

  test("resolves a usable binary on this host", { skip: !resolvable && "no native binary present" }, () => {
    const skiaNode = loadSkiaNode();
    assert.strictEqual(typeof skiaNode.backend, "function");
    assert.ok(Object.keys(skiaNode).length > 0);
  });
});
