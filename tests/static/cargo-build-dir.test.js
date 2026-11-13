// @ts-check

"use strict";

// scripts/cargo-build-dir.mjs finds where a build script ran under a target
// directory. cargo 1.100 moved that from `build/<package>-<hash>/output` to
// `build/<package>/<hash>/run/stdout`, and every reader that matched the old
// names found nothing on it: the crate-consumer jobs reported that no
// skia-bindings build ran, after one had. The trees below are the two
// layouts as cargo 1.99.1 and 1.100.0 leave them for a package with a build
// script -- the unit the script ran in, the unit that compiled it, and the
// package's own objects -- read from real builds of a one-file package.

const assert = require("assert");
const { mkdirSync, mkdtempSync, rmSync, writeFileSync } = require("fs");
const { tmpdir } = require("os");
const { dirname, join } = require("path");
const { pathToFileURL } = require("url");
const { after, describe, test } = require("node:test");

const MODULE = join(__dirname, "..", "..", "scripts", "cargo-build-dir.mjs");

/** @returns {Promise<typeof import("../../scripts/cargo-build-dir.mjs")>} */
const load = () => import(pathToFileURL(MODULE).href);

const root = mkdtempSync(join(tmpdir(), "cargo-build-dir-"));
after(() => rmSync(root, { recursive: true, force: true }));

/**
 * Writes `files`, each a path relative to `dir` and its contents.
 * @param {string} dir
 * @param {Record<string, string>} files
 */
function tree(dir, files) {
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, path)), { recursive: true });
    writeFileSync(join(dir, path), contents);
  }
  return dir;
}

/** cargo 1.99.1's layout: a package's units side by side, `<pkg>-<hash>`. */
function older(target, pkg, log) {
  const build = join(target, "debug", "build");
  tree(build, {
    [`${pkg}-a1594c39e752f34a/build-script-build`]: "the compiled script",
    [`${pkg}-c58b19fd0059d1cd/output`]: log,
    [`${pkg}-c58b19fd0059d1cd/root-output`]: join(
      build,
      `${pkg}-c58b19fd0059d1cd`,
      "out",
    ),
    [`${pkg}-c58b19fd0059d1cd/out/.keep`]: "",
  });
  return build;
}

/** cargo 1.100.0's layout: a package's units under it, `<pkg>/<hash>`. */
function newer(target, pkg, log, profile = join("debug")) {
  const build = join(target, profile, "build");
  tree(build, {
    [`${pkg}/c4b58dbcbf7eb046/fingerprint/build-script-build-script-build`]:
      "the compiled script",
    [`${pkg}/2a85ba7ebffd33a0/out/${pkg}.d`]: "the package's own objects",
    [`${pkg}/505c60ccf0d9343c/run/stdout`]: log,
    [`${pkg}/505c60ccf0d9343c/run/stderr`]: "",
    [`${pkg}/505c60ccf0d9343c/run/root-output`]: join(
      build,
      pkg,
      "505c60ccf0d9343c",
      "out",
    ),
    [`${pkg}/505c60ccf0d9343c/out/.keep`]: "",
  });
  return build;
}

describe("cargo-build-dir", () => {
  test("finds the unit a build script ran in, in cargo 1.99's layout", async () => {
    const { buildScriptRuns, buildScriptLogs } = await load();
    const build = older(join(root, "older"), "skia-bindings", "OLDER LOG");
    const runs = buildScriptRuns(build, "skia-bindings");
    assert.deepStrictEqual(runs, [
      join(build, "skia-bindings-c58b19fd0059d1cd"),
    ]);
    assert.strictEqual(
      buildScriptLogs(runs[0]).output,
      join(build, "skia-bindings-c58b19fd0059d1cd", "output"),
    );
  });

  test("finds the unit a build script ran in, in cargo 1.100's layout", async () => {
    const { buildScriptRuns, buildScriptLogs } = await load();
    const build = newer(join(root, "newer"), "skia-bindings", "NEWER LOG");
    const runs = buildScriptRuns(build, "skia-bindings");
    assert.deepStrictEqual(runs, [
      join(build, "skia-bindings", "505c60ccf0d9343c"),
    ]);
    assert.deepStrictEqual(buildScriptLogs(runs[0]), {
      output: join(build, "skia-bindings", "505c60ccf0d9343c", "run", "stdout"),
      stderr: join(build, "skia-bindings", "505c60ccf0d9343c", "run", "stderr"),
    });
  });

  test("leaves out units no build script ran in", async () => {
    // The compiled script and the package's own objects share the package's
    // name in both layouts; only the run wrote `root-output`.
    const { buildScriptRuns } = await load();
    const build = join(root, "units", "debug", "build");
    tree(build, {
      "skia-bindings-a1594c39e752f34a/build-script-build": "compiled",
      "skia-bindings/2a85ba7ebffd33a0/out/skia_bindings.d": "objects",
    });
    assert.deepStrictEqual(buildScriptRuns(build, "skia-bindings"), []);
  });

  test("does not take a package whose name extends the one asked for", async () => {
    // `meo-skia-canvas-<hash>` is the crate's own; `skia-bindings-extra-<hash>`
    // would match a bare prefix of `skia-bindings-`.
    const { buildScriptRuns } = await load();
    const target = join(root, "names");
    const build = older(target, "skia-bindings-extra", "NOT OURS");
    newer(target, "skia-bindings-extra", "NOT OURS EITHER");
    assert.deepStrictEqual(buildScriptRuns(build, "skia-bindings"), []);
  });

  test("lists the build directories of the host and of named targets", async () => {
    const { buildScriptRuns, profileBuildDirs } = await load();
    const target = join(root, "profiles");
    newer(target, "skia-bindings", "HOST");
    newer(
      target,
      "skia-bindings",
      "CROSS",
      join("x86_64-unknown-linux-musl", "release"),
    );
    const found = profileBuildDirs(target)
      .flatMap((build) => buildScriptRuns(build, "skia-bindings"))
      .map((dir) => dir.slice(target.length + 1));
    assert.deepStrictEqual(found.sort(), [
      join("debug", "build", "skia-bindings", "505c60ccf0d9343c"),
      join(
        "x86_64-unknown-linux-musl",
        "release",
        "build",
        "skia-bindings",
        "505c60ccf0d9343c",
      ),
    ]);
  });

  test("a target directory that does not exist has no runs", async () => {
    const { buildScriptRuns, profileBuildDirs } = await load();
    const missing = join(root, "missing");
    assert.deepStrictEqual(profileBuildDirs(missing), []);
    assert.deepStrictEqual(
      buildScriptRuns(join(missing, "debug", "build"), "skia-bindings"),
      [],
    );
  });
});
