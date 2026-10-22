const assert = require("assert");
const { mkdtempSync, mkdirSync, rmSync, writeFileSync } = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");
const { describe, test, after } = require("node:test");

// What `npm test` says about node_modules before it runs. It installs nothing,
// so its warning has to be right in both directions: silent where nothing is
// known to be wrong -- every CI leg installs with a plain `bun install`, which
// leaves no stamp -- and loud where a stamp proves the lockfile moved since.
describe("npm test's warning about node_modules", () => {
  const dirs = [];
  after(() =>
    dirs.forEach((dir) => rmSync(dir, { recursive: true, force: true })),
  );

  /** A project with package.json, bun.lock and an installed node_modules. */
  const project = () => {
    const dir = mkdtempSync(join(tmpdir(), "install-deps-"));
    dirs.push(dir);
    writeFileSync(join(dir, "package.json"), '{"name":"probe"}\n');
    writeFileSync(join(dir, "bun.lock"), '{"lockfileVersion":1}\n');
    mkdirSync(join(dir, "node_modules"));
    return dir;
  };
  const load = () => import("../../scripts/install-deps.mjs");

  test("is silent for an install that left no stamp", async () => {
    const { lockfileWarning } = await load();
    assert.equal(lockfileWarning(project()), null);
  });

  test("is silent when the stamp matches the lockfile", async () => {
    const { lockfileWarning, inputsHash } = await load();
    const dir = project();
    writeFileSync(
      join(dir, "node_modules", ".meo-skia-canvas-install"),
      `${inputsHash(dir)}\n`,
    );
    assert.equal(lockfileWarning(dir), null);
  });

  test("warns when a stamp exists and the lockfile has moved since", async () => {
    const { lockfileWarning, inputsHash } = await load();
    const dir = project();
    writeFileSync(
      join(dir, "node_modules", ".meo-skia-canvas-install"),
      `${inputsHash(dir)}\n`,
    );
    writeFileSync(
      join(dir, "bun.lock"),
      '{"lockfileVersion":1,"moved":true}\n',
    );
    assert.match(
      lockfileWarning(dir) ?? "",
      /node_modules may not match bun\.lock/,
    );
  });
});
