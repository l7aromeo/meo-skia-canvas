const assert = require("assert");
const { readFileSync } = require("fs");
const { join } = require("path");
const { describe, test } = require("node:test");

// Which Actions cache entries `.github/scripts/prune-caches.mjs` deletes,
// decided on the repository's cache list as the API returned it on
// 2026-11-06: 44 entries, 10.23 GiB. Main held one source tree per platform
// that is current, Rust caches keyed before any key carried a tree, and
// pull-request and branch entries for other patch lists.
const recorded = () =>
  JSON.parse(
    readFileSync(
      join(__dirname, "..", "assets", "workflow-runs", "caches.json"),
      "utf8",
    ),
  ).actions_caches;
const load = () => import("../../.github/scripts/prune-caches.mjs");

/** The trees current when the list was recorded. */
const CURRENT = {
  linux: "skia-source-linux-skia-0.156.0-981c74032fd1",
  darwin: "skia-source-darwin-skia-0.156.0-81002cd180ff",
  win32: "skia-source-win32-skia-0.156.0-9165c96ba5ed",
};
const keys = (entries) => entries.map((e) => e.key).sort();

describe("pruning caches a superseded Skia tree left", () => {
  test("the recording holds what the cases below say it holds", () => {
    const caches = recorded();
    const main = caches.filter((e) => e.ref === "refs/heads/main");
    assert.ok(main.some((e) => e.key === CURRENT.linux));
    assert.ok(main.some((e) => e.key === CURRENT.darwin));
    // Another patch list's trees, held by a pull request and a branch.
    assert.ok(
      caches.some(
        (e) =>
          e.ref === "refs/pull/570/merge" &&
          e.key === "skia-source-linux-skia-0.156.0-f01e39a87d09",
      ),
    );
    assert.ok(
      caches.some(
        (e) =>
          e.ref === "refs/heads/fix/face-load-order" &&
          e.key.startsWith("skia-source-win32-"),
      ),
    );
    // Rust caches keyed without a tree.
    assert.ok(main.some((e) => e.key.startsWith("v0-rust-binding-")));
  });

  test("leaves everything alone while main's trees are current", async () => {
    const { stale } = await load();
    assert.deepEqual(stale(recorded(), CURRENT), []);
  });

  test("names main's tree once its platform's patches move", async () => {
    const { stale } = await load();
    const moved = {
      ...CURRENT,
      linux: "skia-source-linux-skia-0.156.0-000000000000",
    };
    assert.deepEqual(keys(stale(recorded(), moved)), [CURRENT.linux]);
  });

  test("names a main Rust cache keyed on a superseded tree", async () => {
    const { stale } = await load();
    const rust = (tree) => ({
      id: 1,
      key: `v0-rust-${tree}-test-freetype-fetch-test-Linux-x64-01b81e85-d028a097`,
      ref: "refs/heads/main",
      size_in_bytes: 1,
    });
    const old = rust("skia-source-linux-skia-0.156.0-0123456789ab");
    const now = rust(CURRENT.linux);
    assert.deepEqual(keys(stale([old, now], CURRENT)), [old.key]);
  });

  test("leaves a pull request's and a branch's trees to expire", async () => {
    const { stale } = await load();
    const others = recorded().filter((e) => e.ref !== "refs/heads/main");
    const moved = {
      linux: "skia-source-linux-skia-0.156.0-000000000000",
      darwin: "skia-source-darwin-skia-0.156.0-000000000000",
      win32: "skia-source-win32-skia-0.156.0-000000000000",
    };
    assert.deepEqual(stale(others, moved), []);
  });

  test("leaves an entry naming no tree alone", async () => {
    const { stale } = await load();
    const untreed = recorded().filter(
      (e) => e.ref === "refs/heads/main" && !e.key.includes("skia-source-"),
    );
    assert.ok(untreed.length > 0);
    assert.deepEqual(stale(untreed, CURRENT), []);
  });

  test("names a tree whose Skia version moved, whatever its digest", async () => {
    const { stale } = await load();
    const moved = {
      ...CURRENT,
      darwin: CURRENT.darwin.replace("0.156.0", "0.157.0"),
    };
    assert.deepEqual(keys(stale(recorded(), moved)), [CURRENT.darwin]);
  });

  test("refuses to prune against a missing or malformed current tree", async () => {
    // The key comes from `cacheKey`; were it empty or cut short, every entry
    // of that platform, the live tree's among them, would read as stale.
    const { stale } = await load();
    for (const linux of [
      "",
      undefined,
      "skia-source-linux-",
      "skia-source-darwin-skia-0.156.0-81002cd180ff",
      `${CURRENT.linux}-extra`,
    ]) {
      assert.throws(
        () => stale(recorded(), { ...CURRENT, linux }),
        /no current tree for linux/,
        String(linux),
      );
    }
  });

  test("tells one platform's tree from another's", async () => {
    // Linux's key under macOS's current tree id is still Linux's, and stale
    // only against Linux's.
    const { stale } = await load();
    const entry = {
      id: 1,
      key: "skia-source-linux-skia-0.156.0-81002cd180ff",
      ref: "refs/heads/main",
      size_in_bytes: 1,
    };
    assert.deepEqual(keys(stale([entry], CURRENT)), [entry.key]);
    assert.deepEqual(stale([entry], { ...CURRENT, linux: entry.key }), []);
  });
});
