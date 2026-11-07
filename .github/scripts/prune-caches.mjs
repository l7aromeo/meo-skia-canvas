// Deletes the Actions cache entries nothing will restore again, by four rules,
// each its own function below:
//
// - `stale`: entries a superseded Skia tree left on `main`.
// - `deadRefs`: entries of a pull request that is closed or a branch that is
//   gone. A cache is restored only by its own ref and the refs it is the base
//   of, so these are read by no run again, and they count against the same
//   10 GB until they age out.
// - `orphanBlobs`: buildx's layer blobs and index chunks on `main` that the
//   latest successful containers.yml run on `main` neither wrote nor read.
//   Its GitHub cache backend never removes a blob a later image stopped
//   using.
// - `supersededRust`: a Rust build cache on `main` older than another of the
//   same job's. rust-cache ends each key in a toolchain hash and a lockfile
//   hash, so every lockfile change saves a new entry beside the last, and
//   only the newest is restored.
//
// The first rule, in full:
//
// The patched Skia tree has a name per platform -- `skia-source-<platform>-
// <tree>` from `scripts/skia-source.mjs cache-key`, which changes with the
// locked version and the patches' digest -- and two kinds of entry carry it:
// the tree's own cache, about 470 MiB, and every Rust build cache keyed on it.
// When the patch list moves, both kinds go on occupying the repository's
// 10 GB of cache until they age out, and GitHub evicts the least recently
// used entries to make room, current ones among them.
//
// An entry is stale when it is `main`'s and names a tree that is not its
// platform's current one. A pull request's entries belong to its own patch
// list, and are `deadRefs`' to judge once it closes.
//
// Every rule keeps an entry created since the prune run started, and none
// deletes an entry on `main` that the latest run reading it touched.
//
// Usage: node .github/scripts/prune-caches.mjs [--delete]
// Lists the repository's caches, open pull requests, branches and latest
// successful containers.yml run on `main` through `gh api`, in the repository
// `GITHUB_REPOSITORY` names, prints what each rule would remove with its size,
// and deletes it only with `--delete`. In a workflow run, entries created since
// the run started (`GITHUB_RUN_ID`'s `run_started_at`) are kept; run by hand,
// since now.
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

import { cacheKey } from "../../scripts/skia-source.mjs";

/** The platforms a tree is named for: `process.platform` spellings. */
export const PLATFORMS = ["linux", "darwin", "win32"];

/** The ref whose entries every branch and pull request restores from. */
const MAIN = "refs/heads/main";

/** A tree's name inside a key: `skia-source-<platform>-<tree>`. */
const TREE =
  /skia-source-(linux|darwin|win32)-skia-[0-9A-Za-z.+-]*?-[0-9a-f]{12}/;

/**
 * The entries of `caches` that a superseded tree left on main.
 *
 * `caches` are REST cache entries -- `id`, `key`, `ref`, `created_at` are
 * read -- and `current` maps each platform to its tree's key. An entry
 * created at or after `before`, an ISO time, is kept whatever it names: a run
 * of a newer commit may have saved it for a newer tree than this run's
 * checkout knows, and it is not this run's to judge. Throws unless every
 * platform's key is a whole tree name for that platform: against an empty or
 * garbled one, every entry of that platform, the current tree's included,
 * would read as superseded.
 */
export function stale(caches, current, before) {
  const cutoff = cutoffOf(before);
  for (const platform of PLATFORMS) {
    const key = current[platform];
    const found = typeof key === "string" ? key.match(TREE) : null;
    if (!found || found[0] !== key || found[1] !== platform) {
      throw new Error(
        `no current tree for ${platform} (got ${JSON.stringify(key)}), so nothing is pruned`,
      );
    }
  }
  return caches.filter((entry) => {
    if (entry.ref !== MAIN) return false;
    if (!(Date.parse(entry.created_at) < cutoff)) return false;
    const found = entry.key.match(TREE);
    if (!found) return false;
    return found[0] !== current[found[1]];
  });
}

/** A pull request's merge ref, the one a run of it saves caches under. */
const PULL = /^refs\/pull\/(\d+)\/merge$/;

/** A branch's ref. */
const BRANCH = /^refs\/heads\/(.+)$/;

/** `before` as a time, or a throw: every rule keeps what is newer. */
function cutoffOf(before) {
  const cutoff = Date.parse(before);
  if (!Number.isFinite(cutoff)) {
    throw new Error(
      `no cutoff time (got ${JSON.stringify(before)}), so nothing is pruned`,
    );
  }
  return cutoff;
}

/**
 * The entries of `caches` whose ref is a pull request's merge ref and the pull
 * request is not open, or a branch other than main's and the branch is gone.
 *
 * `openPulls` is a set of the open pull requests' numbers and `branches` a set
 * of the branches' names. Throws unless `branches` holds `main`: a listing that
 * failed or came back empty would otherwise read every branch's entries as
 * dead. A tag's entry, or a ref of any other shape, is kept.
 */
export function deadRefs(caches, { openPulls, branches }, before) {
  const cutoff = cutoffOf(before);
  if (!(openPulls instanceof Set)) {
    throw new Error("no list of open pull requests, so nothing is pruned");
  }
  if (!(branches instanceof Set) || !branches.has("main")) {
    throw new Error(
      "the list of branches does not hold main, so nothing is pruned",
    );
  }
  return caches.filter((entry) => {
    if (entry.ref === MAIN) return false;
    if (!(Date.parse(entry.created_at) < cutoff)) return false;
    const pull = entry.ref.match(PULL);
    if (pull) return !openPulls.has(Number(pull[1]));
    const branch = entry.ref.match(BRANCH);
    if (branch) return !branches.has(branch[1]);
    return false;
  });
}

/// The keys of buildx's GitHub cache backend (`cache-to: type=gha` in
/// containers.yml): a layer blob, and a chunk of the index naming them.
const BUILDKIT = /^(buildkit-blob-|index-buildkit-)/;

/**
 * The buildx entries of `caches` on main that the containers.yml run on main
 * that started at `lastBuild` -- its latest successful one -- did not touch:
 * created and last accessed before it started. A blob that run pulled or wrote
 * is accessed at or after its start, and is kept. Throws without a time for
 * that run, since without one every blob reads as untouched.
 */
export function orphanBlobs(caches, lastBuild, before) {
  const cutoff = cutoffOf(before);
  const built = Date.parse(lastBuild);
  if (!Number.isFinite(built)) {
    throw new Error(
      `no successful containers.yml run on main (got ${JSON.stringify(lastBuild)}), so no blob is pruned`,
    );
  }
  return caches.filter(
    (entry) =>
      entry.ref === MAIN &&
      BUILDKIT.test(entry.key) &&
      Date.parse(entry.created_at) < Math.min(built, cutoff) &&
      Date.parse(entry.last_accessed_at) < built,
  );
}

/// rust-cache's key: `v0-rust-<prefix and job>-<toolchain hash>-<lockfile
/// hash>`, the two hashes eight hex digits each. The job is the part before
/// them.
const RUST = /^(v0-rust-.+)-[0-9a-f]{8}-[0-9a-f]{8}$/;

/**
 * The Rust build caches of `caches` on main that an entry of the same job,
 * created later, supersedes: a new toolchain or lockfile saved it beside them,
 * and rust-cache restores only the newest.
 */
export function supersededRust(caches, before) {
  const cutoff = cutoffOf(before);
  const newest = new Map();
  const jobOf = (entry) =>
    entry.ref === MAIN ? entry.key.match(RUST)?.[1] : undefined;
  for (const entry of caches) {
    const job = jobOf(entry);
    if (!job) continue;
    const held = newest.get(job);
    if (!held || Date.parse(entry.created_at) > Date.parse(held.created_at))
      newest.set(job, entry);
  }
  return caches.filter((entry) => {
    const job = jobOf(entry);
    return (
      job !== undefined &&
      newest.get(job) !== entry &&
      Date.parse(entry.created_at) < cutoff
    );
  });
}

function gh(...args) {
  return execFileSync("gh", ["api", ...args], { encoding: "utf8" });
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const repo = process.env.GITHUB_REPOSITORY;
  if (!repo) {
    console.error(
      "usage: GITHUB_REPOSITORY=<owner/repo> node .github/scripts/prune-caches.mjs [--delete]",
    );
    process.exit(2);
  }
  const current = Object.fromEntries(
    PLATFORMS.map((p) => [p, cacheKey(undefined, p)]),
  );
  for (const [platform, key] of Object.entries(current)) {
    console.log(`current ${platform}: ${key}`);
  }
  // One JSON object per page under `--paginate`, so `--slurp` joins them.
  const pages = (path) =>
    JSON.parse(gh("--paginate", "--slurp", `repos/${repo}/${path}`));
  const caches = pages("actions/caches?per_page=100").flatMap(
    (page) => page.actions_caches,
  );
  const openPulls = new Set(
    pages("pulls?state=open&per_page=100")
      .flat()
      .map((pull) => pull.number),
  );
  const branches = new Set(
    pages("branches?per_page=100")
      .flat()
      .map((branch) => branch.name),
  );
  const lastBuild = JSON.parse(
    gh(
      `repos/${repo}/actions/workflows/containers.yml/runs?branch=main&status=success&per_page=1`,
    ),
  ).workflow_runs[0]?.run_started_at;
  const before = process.env.GITHUB_RUN_ID
    ? JSON.parse(gh(`repos/${repo}/actions/runs/${process.env.GITHUB_RUN_ID}`))
        .run_started_at
    : new Date().toISOString();
  console.log(`keeping entries created since ${before}`);
  console.log(
    `${openPulls.size} open pull requests, ${branches.size} branches, latest successful containers.yml run on main started ${lastBuild ?? "never"}`,
  );
  const rules = [
    ["a superseded Skia tree on main", stale(caches, current, before)],
    [
      "a closed pull request or a deleted branch",
      deadRefs(caches, { openPulls, branches }, before),
    ],
    [
      "a buildx blob the latest containers build left untouched",
      lastBuild ? orphanBlobs(caches, lastBuild, before) : [],
    ],
    [
      "a Rust cache a newer one of its job replaced",
      supersededRust(caches, before),
    ],
  ];
  const mib = (n) => `${Math.round(n / 2 ** 20)} MiB`;
  const size = (entries) =>
    entries.reduce((sum, e) => sum + e.size_in_bytes, 0);
  const all = new Map();
  for (const [why, entries] of rules) {
    console.log(`\n${entries.length} entries, ${mib(size(entries))}: ${why}`);
    for (const entry of entries) {
      console.log(`  ${entry.ref}  ${entry.key} (${mib(entry.size_in_bytes)})`);
      all.set(entry.id, entry);
    }
  }
  const remove = [...all.values()];
  console.log(
    `\n${remove.length} of ${caches.length} entries to remove, ${mib(size(remove))} of ${mib(size(caches))}`,
  );
  for (const entry of remove) {
    if (process.argv.includes("--delete")) {
      gh("--method", "DELETE", `repos/${repo}/actions/caches/${entry.id}`);
    }
  }
  if (!process.argv.includes("--delete") && remove.length) {
    console.log("(not deleted: pass --delete)");
  }
}
