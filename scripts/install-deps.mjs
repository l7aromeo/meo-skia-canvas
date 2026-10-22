#!/usr/bin/env node
// Installs a directory's JavaScript dependencies when they are missing or no
// longer match its lockfile.
//
//   node scripts/install-deps.mjs [dir]
//
// The recipes used to install only when `node_modules` did not exist, so a
// warm worktree kept whatever it last installed after a rebase or merge moved
// `bun.lock`, and nothing said so. #231 took nock from 14 to 15, which is an ES
// module reached as `require("nock").default`; a worktree still holding nock
// 14.0.17 failed at tests/suite/media.test.js with `nock is not a function`.
// CI starts cold every time, so it cannot see this.
//
// So an install leaves a stamp in `node_modules`: a hash of `package.json` and
// `bun.lock` as they were when it succeeded. The next call compares and
// reinstalls when either has changed, or when there is no stamp -- which is
// also the case for a tree installed before this existed, so the first run
// after it lands reinstalls once. A matching stamp costs two file reads.
//
// The install is `bun install --frozen-lockfile`, as before: it never rewrites
// the lockfile, and it fails rather than resolving anew when `package.json`
// and `bun.lock` disagree. That is also why no `bun dedupe` follows it -- this
// installs what the lockfile says, and dedupe is for changing it.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** Where an install records what it installed from. */
const STAMP = ".meo-skia-canvas-install";

/** The files an install reads its dependencies from. */
const INPUTS = ["package.json", "bun.lock"];

/** A hash of the install's inputs, or null when one of them is missing. */
export function inputsHash(dir) {
  const hash = createHash("sha256");
  for (const name of INPUTS) {
    const path = join(dir, name);
    if (!existsSync(path)) return null;
    hash.update(`${name}\0`).update(readFileSync(path));
  }
  return hash.digest("hex");
}

/** Why `dir` needs installing, or null when its `node_modules` is current. */
export function staleness(dir) {
  const modules = join(dir, "node_modules");
  if (!existsSync(modules)) return "no node_modules";
  const stamp = join(modules, STAMP);
  if (!existsSync(stamp)) return "no record of what was installed";
  if (readFileSync(stamp, "utf8").trim() !== inputsHash(dir))
    return "package.json or bun.lock changed since the last install";
  return null;
}

/**
 * What `npm test` prints about `dir`'s `node_modules` before running, or null
 * for nothing: `scripts/test.mjs` installs nothing, so this is its only say.
 *
 * Only a stamp that no longer matches is reported. A missing stamp is what a
 * plain `bun install` leaves -- every CI leg installs that way, and so may a
 * developer -- so it says nothing about whether the install is current, and
 * warning on it put a false alarm in every CI run. `staleness` still counts it,
 * because there the cost of being wrong is one install rather than noise.
 */
export function lockfileWarning(dir) {
  const stamp = join(dir, "node_modules", STAMP);
  if (!existsSync(stamp)) return null;
  const why = staleness(dir);
  return why
    ? `warning: node_modules may not match bun.lock (${why}); run \`bun install\``
    : null;
}

// Installing only when run, so that `scripts/test.mjs` can import
// `lockfileWarning` to warn without installing anything.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const dir = resolve(process.argv[2] ?? ".");
  const want = inputsHash(dir);
  if (!want) {
    console.error(`install-deps: ${dir} has no ${INPUTS.join(" and ")}`);
    process.exit(1);
  }
  const why = staleness(dir);
  if (why) {
    console.log(`install-deps: ${dir}: ${why}; installing`);
    const { status, error } = spawnSync(
      "bun",
      ["install", "--frozen-lockfile"],
      {
        cwd: dir,
        stdio: "inherit",
      },
    );
    if (error) throw error;
    // A signal death leaves `status` null, which must not read as success.
    if (status !== 0) process.exit(status ?? 1);
    writeFileSync(join(dir, "node_modules", STAMP), `${want}\n`);
  }
}
