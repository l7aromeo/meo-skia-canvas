// Deletes the Actions cache entries a superseded Skia tree left on `main`.
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
// platform's current one. Nothing else is touched: a pull request's entries
// belong to its own patch list and expire with it, and an entry naming no
// tree -- a Rust cache of a job that builds no Skia -- is not this script's
// to judge.
//
// Usage: node .github/scripts/prune-caches.mjs [--delete]
// Lists the repository's caches through `gh api`, in the repository
// `GITHUB_REPOSITORY` names, prints what is stale, and deletes it only with
// `--delete`.
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
 * `caches` are REST cache entries -- `id`, `key`, `ref` are read -- and
 * `current` maps each platform to its tree's key. Throws unless every
 * platform's key is a whole tree name for that platform: against an empty or
 * garbled one, every entry of that platform, the current tree's included,
 * would read as superseded.
 */
export function stale(caches, current) {
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
    const found = entry.key.match(TREE);
    if (!found) return false;
    return found[0] !== current[found[1]];
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
  const caches = JSON.parse(
    gh("--paginate", "--slurp", `repos/${repo}/actions/caches?per_page=100`),
  ).flatMap((page) => page.actions_caches);
  const remove = stale(caches, current);
  const mib = (n) => `${Math.round(n / 2 ** 20)} MiB`;
  const total = remove.reduce((sum, entry) => sum + entry.size_in_bytes, 0);
  console.log(
    `${remove.length} of ${caches.length} entries are stale, ${mib(total)}`,
  );
  for (const entry of remove) {
    console.log(`  ${entry.key} (${mib(entry.size_in_bytes)})`);
    if (process.argv.includes("--delete")) {
      gh("--method", "DELETE", `repos/${repo}/actions/caches/${entry.id}`);
    }
  }
  if (!process.argv.includes("--delete") && remove.length) {
    console.log("(not deleted: pass --delete)");
  }
}
