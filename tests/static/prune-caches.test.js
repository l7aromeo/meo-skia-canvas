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
/** A run that started after every recorded entry was created. */
const BEFORE = "2026-11-07T00:00:00Z";

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
    assert.deepEqual(stale(recorded(), CURRENT, BEFORE), []);
  });

  test("names main's tree once its platform's patches move", async () => {
    const { stale } = await load();
    const moved = {
      ...CURRENT,
      linux: "skia-source-linux-skia-0.156.0-000000000000",
    };
    assert.deepEqual(keys(stale(recorded(), moved, BEFORE)), [CURRENT.linux]);
  });

  test("names a main Rust cache keyed on a superseded tree", async () => {
    const { stale } = await load();
    const rust = (tree) => ({
      id: 1,
      key: `v0-rust-${tree}-test-freetype-fetch-test-Linux-x64-01b81e85-d028a097`,
      ref: "refs/heads/main",
      size_in_bytes: 1,
      created_at: "2026-11-06T00:00:00Z",
    });
    const old = rust("skia-source-linux-skia-0.156.0-0123456789ab");
    const now = rust(CURRENT.linux);
    assert.deepEqual(keys(stale([old, now], CURRENT, BEFORE)), [old.key]);
  });

  test("leaves a pull request's and a branch's trees to expire", async () => {
    const { stale } = await load();
    const others = recorded().filter((e) => e.ref !== "refs/heads/main");
    const moved = {
      linux: "skia-source-linux-skia-0.156.0-000000000000",
      darwin: "skia-source-darwin-skia-0.156.0-000000000000",
      win32: "skia-source-win32-skia-0.156.0-000000000000",
    };
    assert.deepEqual(stale(others, moved, BEFORE), []);
  });

  test("leaves an entry naming no tree alone", async () => {
    const { stale } = await load();
    const untreed = recorded().filter(
      (e) => e.ref === "refs/heads/main" && !e.key.includes("skia-source-"),
    );
    assert.ok(untreed.length > 0);
    assert.deepEqual(stale(untreed, CURRENT, BEFORE), []);
  });

  test("names a tree whose Skia version moved, whatever its digest", async () => {
    const { stale } = await load();
    const moved = {
      ...CURRENT,
      darwin: CURRENT.darwin.replace("0.156.0", "0.157.0"),
    };
    assert.deepEqual(keys(stale(recorded(), moved, BEFORE)), [CURRENT.darwin]);
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
        () => stale(recorded(), { ...CURRENT, linux }, BEFORE),
        /no current tree for linux/,
        String(linux),
      );
    }
  });

  test("keeps an entry created after the run started, whatever it names", async () => {
    // A newer commit's run may save entries for a tree this checkout calls
    // superseded; the older run's prune must not delete them.
    const { stale } = await load();
    const entry = (created_at) => ({
      id: 1,
      key: "skia-source-linux-skia-0.156.0-0123456789ab",
      ref: "refs/heads/main",
      size_in_bytes: 1,
      created_at,
    });
    assert.deepEqual(
      stale([entry("2026-11-07T00:00:01Z")], CURRENT, BEFORE),
      [],
    );
    assert.equal(
      stale([entry("2026-11-06T23:59:59Z")], CURRENT, BEFORE).length,
      1,
    );
    assert.throws(() => stale([], CURRENT, undefined), /no cutoff time/);
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
      created_at: "2026-11-06T00:00:00Z",
    };
    assert.deepEqual(keys(stale([entry], CURRENT, BEFORE)), [entry.key]);
    assert.deepEqual(
      stale([entry], { ...CURRENT, linux: entry.key }, BEFORE),
      [],
    );
  });
});

// The repository's caches, open pull requests, branches and latest successful
// containers.yml run on main, as the API returned them on 2026-11-07: 85
// entries, 10,205 MiB, over the 10 GB the repository is allowed.
const today = () =>
  JSON.parse(
    readFileSync(
      join(
        __dirname,
        "..",
        "assets",
        "workflow-runs",
        "caches-2026-11-07.json",
      ),
      "utf8",
    ),
  );
const TODAY = "2026-11-07T23:59:59Z";
const mib = (entries) =>
  Math.round(entries.reduce((sum, e) => sum + e.size_in_bytes, 0) / 2 ** 20);
const live = (recorded) => ({
  openPulls: new Set(recorded.openPulls),
  branches: new Set(recorded.branches),
});
const entry = (fields) => ({
  id: 1,
  key: "k",
  ref: "refs/heads/main",
  size_in_bytes: 1,
  created_at: "2026-11-01T00:00:00Z",
  last_accessed_at: "2026-11-01T00:00:00Z",
  ...fields,
});

describe("pruning entries nothing will restore", () => {
  test("a closed pull request's and a deleted branch's entries go", async () => {
    const { deadRefs } = await load();
    const recorded = today();
    const dead = deadRefs(recorded.actions_caches, live(recorded), TODAY);
    assert.deepEqual([...new Set(dead.map((e) => e.ref))].sort(), [
      "refs/heads/fix/kept-churn",
      "refs/pull/608/merge",
    ]);
    assert.equal(dead.length, 12);
    assert.equal(mib(dead), 923);
  });

  test("an open pull request's, a live branch's, a tag's and main's stay", async () => {
    const { deadRefs } = await load();
    const refs = {
      openPulls: new Set([629]),
      branches: new Set(["main", "fix/live"]),
    };
    const kept = [
      entry({ ref: "refs/pull/629/merge" }),
      entry({ ref: "refs/heads/fix/live" }),
      entry({ ref: "refs/tags/npm-v7.0.0" }),
      entry({ ref: "refs/heads/main" }),
      // Created after the run started: a newer run's, not this one's to judge.
      entry({ ref: "refs/pull/1/merge", created_at: "2026-11-08T00:00:00Z" }),
    ];
    assert.deepEqual(deadRefs(kept, refs, TODAY), []);
    const gone = [
      entry({ ref: "refs/pull/628/merge" }),
      entry({ ref: "refs/heads/fix/gone" }),
    ];
    assert.equal(deadRefs(gone, refs, TODAY).length, 2);
  });

  test("refuses to judge refs against a listing without main", async () => {
    // A listing that failed or came back empty would read every branch dead.
    const { deadRefs } = await load();
    const caches = [entry({ ref: "refs/heads/fix/live" })];
    for (const branches of [new Set(), new Set(["fix/live"]), undefined]) {
      assert.throws(
        () => deadRefs(caches, { openPulls: new Set(), branches }, TODAY),
        /does not hold main/,
      );
    }
    assert.throws(
      () => deadRefs(caches, { branches: new Set(["main"]) }, TODAY),
      /no list of open pull requests/,
    );
  });

  test("buildx blobs the latest containers build left untouched go", async () => {
    const { orphanBlobs } = await load();
    const recorded = today();
    const orphans = orphanBlobs(
      recorded.actions_caches,
      recorded.lastBuild,
      TODAY,
    );
    assert.equal(orphans.length, 17);
    assert.equal(mib(orphans), 832);
    assert.ok(orphans.every((e) => e.ref === "refs/heads/main"));
  });

  test("a blob that build read or wrote, or a later one's, stays", async () => {
    const { orphanBlobs } = await load();
    const built = "2026-11-06T18:01:08Z";
    const blob = (fields) =>
      entry({ key: "buildkit-blob-1-sha256:00", ...fields });
    const kept = [
      blob({ last_accessed_at: built }),
      blob({
        created_at: "2026-11-06T18:12:00Z",
        last_accessed_at: "2026-11-06T18:12:00Z",
      }),
      blob({ ref: "refs/pull/1/merge" }),
      entry({ key: "docker.io--tonistiigi--binfmt-latest-linux-x64" }),
      entry({ key: "v0-rust-docs-docs-Linux-x64-01b81e85-d028a097" }),
    ];
    assert.deepEqual(orphanBlobs(kept, built, TODAY), []);
    const gone = [blob({}), entry({ key: "index-buildkit-1-f921bd05#1" })];
    assert.equal(orphanBlobs(gone, built, TODAY).length, 2);
    assert.throws(() => orphanBlobs(gone, undefined, TODAY), /no successful/);
  });

  test("an older Rust cache of a job goes, its newest stays", async () => {
    const { supersededRust } = await load();
    const rust = (lock, created_at, ref = "refs/heads/main") =>
      entry({
        key: `v0-rust-msrv-1.90-msrv-Linux-x64-b75ed0f5-${lock}`,
        created_at,
        ref,
      });
    const old = rust("00000000", "2026-11-01T00:00:00Z");
    const now = rust("d028a097", "2026-11-05T00:00:00Z");
    const other = entry({
      key: "v0-rust-docs-docs-Linux-x64-01b81e85-00000000",
    });
    assert.deepEqual(supersededRust([old, now, other], TODAY), [old]);
    // A toolchain change supersedes as a lockfile change does.
    const toolchain = entry({
      key: "v0-rust-msrv-1.90-msrv-Linux-x64-ffffffff-d028a097",
      created_at: "2026-11-06T00:00:00Z",
    });
    assert.deepEqual(
      supersededRust([old, now, toolchain], TODAY)
        .map((e) => e.key)
        .sort(),
      [now.key, old.key].sort(),
    );
    // Another ref's, or one created since the run started, is not judged.
    assert.deepEqual(
      supersededRust(
        [rust("00000000", "2026-11-01T00:00:00Z", "refs/pull/1/merge"), now],
        TODAY,
      ),
      [],
    );
    assert.deepEqual(
      supersededRust(
        [rust("00000000", "2026-11-01T00:00:00Z"), now],
        "2026-11-01T00:00:00Z",
      ),
      [],
    );
    // Today's cache holds one Rust entry a job.
    assert.deepEqual(supersededRust(today().actions_caches, TODAY), []);
  });

  test("no rule removes an entry on main that its latest runs touched", async () => {
    const { stale, deadRefs, orphanBlobs, supersededRust } = await load();
    const recorded = today();
    const caches = recorded.actions_caches;
    const current = Object.fromEntries(
      caches
        .filter(
          (e) => e.ref === "refs/heads/main" && /^skia-source-/.test(e.key),
        )
        .map((e) => [e.key.split("-")[2], e.key]),
    );
    const removed = [
      ...stale(caches, current, TODAY),
      ...deadRefs(caches, live(recorded), TODAY),
      ...orphanBlobs(caches, recorded.lastBuild, TODAY),
      ...supersededRust(caches, TODAY),
    ];
    // Every entry on main a run read or wrote since the latest containers
    // build started -- that build's blobs, rust-ci's and the source trees.
    const touched = caches.filter(
      (e) =>
        e.ref === "refs/heads/main" &&
        Date.parse(e.last_accessed_at) >= Date.parse(recorded.lastBuild),
    );
    assert.ok(touched.length > 40, `${touched.length} touched`);
    const gone = new Set(removed.map((e) => e.id));
    assert.deepEqual(
      touched.filter((e) => gone.has(e.id)).map((e) => e.key),
      [],
    );
    assert.equal(
      mib([...new Map(removed.map((e) => [e.id, e])).values()]),
      1755,
    );
  });
});
