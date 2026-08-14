const assert = require("assert");
const { existsSync, mkdirSync, rmSync, writeFileSync } = require("fs");
const { join } = require("path");
const { describe, test, before, after } = require("node:test");
const {
  PLATFORM_PACKAGES,
  loadSkiaNode,
  verifyPlatformPackage,
  BINARY_OVERRIDE,
} = require("../../lib/binary");
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
  test(
    "every target the loader probes has an optional dependency",
    { skip: optional.length === 0 && "platform packages not published yet" },
    () => {
      assert.deepStrictEqual(optional, declared);
    },
  );

  test(
    "every released binary has a platform package",
    { skip: prebuilt.length === 0 && "no release snapshotted yet" },
    () => {
      assert.deepStrictEqual(prebuilt, declared);
    },
  );

  // Exact pins, as sharp and esbuild do: a range would let a consumer resolve a binary built from
  // different sources than the JavaScript wrapping it.
  test("optional dependencies pin the current version", () => {
    for (const [name, range] of Object.entries(
      manifest.optionalDependencies || {},
    )) {
      assert.strictEqual(
        range,
        manifest.version,
        `${name} must pin ${manifest.version}`,
      );
    }
  });

  // Skipped where no binary exists at all: before the first release there is nothing to download
  // and nothing to resolve, and failing here would report that as a defect in the loader.
  const resolvable = existsSync(join(__dirname, "../../lib/skia.node"));

  test(
    "resolves a usable binary on this host",
    { skip: !resolvable && "no native binary present" },
    () => {
      const skiaNode = loadSkiaNode();
      assert.strictEqual(typeof skiaNode.backend, "function");
      assert.ok(Object.keys(skiaNode).length > 0);
    },
  );

  // A platform package always beats lib/skia.node, so without an override a freshly
  // compiled binary is invisible to every require. The variable is the way to test a
  // build, and it has to fail loudly: silently falling back would restore exactly the
  // wrong-binary problem it exists to prevent.
  test("an override pointing nowhere is fatal, not a silent fallback", () => {
    const previous = process.env[BINARY_OVERRIDE];
    process.env[BINARY_OVERRIDE] = join(__dirname, "no-such-binary.node");
    try {
      assert.throws(() => loadSkiaNode(), {
        message: /does not exist/,
      });
    } finally {
      if (previous === undefined) delete process.env[BINARY_OVERRIDE];
      else process.env[BINARY_OVERRIDE] = previous;
    }
  });

  test(
    "an override pointing at a real binary is used",
    { skip: !resolvable && "no native binary present" },
    () => {
      const previous = process.env[BINARY_OVERRIDE];
      process.env[BINARY_OVERRIDE] = join(__dirname, "../../lib/skia.node");
      try {
        const skiaNode = loadSkiaNode();
        assert.strictEqual(typeof skiaNode.backend, "function");
      } finally {
        if (previous === undefined) delete process.env[BINARY_OVERRIDE];
        else process.env[BINARY_OVERRIDE] = previous;
      }
    },
  );

  // A platform package carries the compiled half of an API whose JavaScript half ships here, and
  // nothing checked that the two were built together. A stale one resolves and loads, and the
  // first symptom is a missing native accessor surfacing three frames away as
  // "Cannot read properties of undefined (reading 'get')" -- which names neither the binary nor
  // the version that would explain it.
  //
  // Reproduced against real package directories rather than a stubbed `require`: the check reads
  // the manifest sitting beside the resolved binary, and that lookup is the part that has to work.
  describe("platform package version", () => {
    const SANDBOX = join(
      __dirname,
      "../../.test-sandbox",
      `${process.pid}-pkg`,
    );

    // Writes the two files a platform package is: the binary the loader resolves, and the manifest
    // beside it. The binary is never dlopen'd here -- the check runs before the load, which is the
    // point of it.
    const fakePackage = (name, manifestBody) => {
      const dir = join(SANDBOX, name);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "package.json"), JSON.stringify(manifestBody));
      writeFileSync(join(dir, "skia.node"), "");
      return join(dir, "skia.node");
    };

    before(() => mkdirSync(SANDBOX, { recursive: true }));
    after(() => rmSync(SANDBOX, { recursive: true, force: true }));

    test("refuses a package built for a different release", () => {
      const binary = fakePackage("stale", {
        name: "meo-skia-canvas-stale",
        version: "4.1.1",
      });

      assert.throws(
        () => verifyPlatformPackage(binary, "meo-skia-canvas-stale"),
        {
          message: new RegExp(`4\\.1\\.1[\\s\\S]*${manifest.version}`),
        },
      );
    });

    test("accepts a package built for this release", () => {
      const binary = fakePackage("current", {
        name: "meo-skia-canvas-current",
        version: manifest.version,
      });

      assert.doesNotThrow(() =>
        verifyPlatformPackage(binary, "meo-skia-canvas-current"),
      );
    });

    // Absence of a version is not evidence of a mismatch. A hand-vendored binary or a manifest
    // this cannot parse still loads: refusing it would break working installs to enforce a check
    // that has nothing to compare.
    test("loads a package that declares no version rather than refusing it", () => {
      const binary = fakePackage("undeclared", {
        name: "meo-skia-canvas-undeclared",
      });

      assert.doesNotThrow(() =>
        verifyPlatformPackage(binary, "meo-skia-canvas-undeclared"),
      );
    });
  });

  // browser.d.ts re-exports from index.d.ts rather than redeclaring, so the
  // shapes cannot drift -- but the membership of the list is maintained by hand
  // and has to keep matching what browser.js actually exports.
  //
  // `Canvas` is declared there rather than re-exported, because the browser
  // build narrows it: three image formats instead of fourteen, an
  // `ArrayBuffer` where Node returns a `Buffer`, and no synchronous exports.
  // So the comparison takes the re-export block *and* anything declared with
  // `export const`, which is what a narrowed class looks like in a `.d.ts`.
  test("the browser build's types list the values it exports", () => {
    const { readFileSync } = require("fs");
    const read = (rel) => readFileSync(join(__dirname, "../..", rel), "utf8");

    const runtime = read("lib/browser.js")
      .split("module.exports = {")[1]
      .split("};")[0]
      .split(",")
      .map((line) => line.trim())
      .filter(Boolean)
      .sort();

    // The value re-export block, not the `export type` one below it.
    const source = read("lib/browser.d.ts");
    const reExported = source
      .split("export {")[1]
      .split("}")[0]
      .split(",")
      .map((line) => line.trim())
      .filter(Boolean);

    const declaredValues = [...source.matchAll(/^export const (\w+)/gm)].map(
      (match) => match[1],
    );

    const declared = [...reExported, ...declaredValues].sort();

    assert.deepStrictEqual(declared, runtime);
  });
});
