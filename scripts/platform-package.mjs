//
// Builds the per-target npm packages that carry the prebuilt binaries.
//
// These are the same `<triplet>.gz` assets the `install` script downloads from GitHub releases,
// republished so a package manager can select one declaratively. Nothing is compiled here: this
// repackages what the release already contains.
//
//   node scripts/platform-package.mjs build <triplet> <staging-dir> <assets-dir>
//       <assets-dir> holds `<triplet>.gz` and `<triplet>.gz.notices.md`, the
//       release's binary and the third-party notices it owes
//   node scripts/platform-package.mjs matrix
//

import zlib from "zlib";
import stream from "stream";
import crypto from "crypto";
import { createReadStream, createWriteStream, existsSync } from "fs";
import { mkdir, readFile, writeFile, rm } from "fs/promises";
import { resolve } from "path";
import { promisify } from "util";
import { createRequire } from "module";

const pipeline = promisify(stream.pipeline);

const ROOT = resolve(`${import.meta.dirname}/..`);
const PACKAGE_JSON = `${ROOT}/package.json`;
/// The notices file every package and archive carries, under the name
/// scripts/skia-pack.mjs gives it in an archive.
const NOTICES = "THIRD-PARTY-NOTICES.md";

// The canonical target table, shared with lib/binary.js so a platform can only be added in one
// place. `os` and `cpu` are matched by every package manager; `libc` is honoured by npm, pnpm,
// yarn and bun, and is the only way to tell a glibc build from a musl one. Without it an Alpine
// install silently receives a glibc binary that fails to load.
const require_ = createRequire(import.meta.url);
const TARGETS = require_("../lib/targets.json");
const { packageName } = require_("../lib/binary.js");

class Hasher extends stream.Transform {
  #digest;
  constructor(options) {
    super(options);
    this.hash = crypto.createHash("sha256");
  }
  _transform(chunk, encoding, callback) {
    this.hash.update(chunk);
    this.push(chunk);
    callback();
  }
  get digest() {
    this.#digest = this.#digest || `sha256:${this.hash.digest("hex")}`;
    return this.#digest;
  }
}

async function manifest() {
  return JSON.parse(await readFile(PACKAGE_JSON));
}

// Expands `<triplet>.gz` from `assetsDir`, checking it against the hash the release was
// snapshotted with. A silent mismatch would ship a corrupt binary to every consumer on that
// platform, so this refuses rather than warns -- and refuses a target with no recorded hash, since
// nothing would then say whether the file is the one the release holds.
//
// Read from a directory rather than downloaded, because the release is still a draft when its
// platform packages are published (#620), and a draft's assets are not downloadable without a
// token that sees drafts. The workflow fetches them in a job of their own and hands them here.
async function expandBinary(triplet, assetsDir, expected, dest) {
  const source = resolve(assetsDir, `${triplet}.gz`),
    sha = new Hasher();
  if (!expected)
    throw Error(
      `package.json records no hash for '${triplet}.gz', so the release has not been snapshotted for this version. Run \`node lib/prebuild.mjs snapshot\` and commit it first.`,
    );

  await pipeline(
    createReadStream(source),
    sha,
    zlib.createGunzip(),
    createWriteStream(dest),
  );

  if (sha.digest != expected) {
    await rm(dest, { force: true });
    throw Error(
      `Prebuilt library file '${triplet}.gz' failed integrity check\n` +
        `Read: ${source}\nExpected: ${expected}\nReceived: ${sha.digest}`,
    );
  }
}

// The `exports` subpath is what the loader probes. Exposing the binary under a named entry rather
// than as the package main keeps the bare specifier unresolvable, so a half-installed package
// fails at resolution instead of loading something unexpected.
async function build(triplet, stagingDir, assetsDir) {
  const target = TARGETS[triplet];
  if (!target) throw new Error(`unknown target: ${triplet}`);

  // The notices the binary owes, uploaded beside it by the build that made
  // it: nothing is staged without them.
  const notices = resolve(assetsDir, `${triplet}.gz.notices.md`);
  if (!existsSync(notices))
    throw new Error(
      `${notices} is missing: the release holds no notices for ${triplet}, and its package would ship a binary without the third-party notices it owes`,
    );

  const { version, prebuild, license, repository } = await manifest();
  const dir = resolve(stagingDir, packageName(triplet));
  await mkdir(dir, { recursive: true });

  await expandBinary(
    triplet,
    assetsDir,
    prebuild?.[`${triplet}.gz`],
    `${dir}/skia.node`,
  );

  await writeFile(
    `${dir}/package.json`,
    JSON.stringify(
      {
        name: packageName(triplet),
        version,
        description: `Prebuilt meo-skia-canvas binary for ${triplet}`,
        license,
        repository,
        ...target,
        exports: { "./skia.node": "./skia.node" },
        files: ["skia.node", NOTICES],
      },
      null,
      2,
    ) + "\n",
  );

  // Carried so each published package states its own terms rather than pointing at another one.
  await writeFile(`${dir}/LICENSE`, await readFile(`${ROOT}/LICENSE`));

  await writeFile(`${dir}/${NOTICES}`, await readFile(notices));

  console.log(dir);
}

function matrix() {
  console.log(JSON.stringify(Object.keys(TARGETS)));
}

// Writes `optionalDependencies` from the target table. Generated rather than hand-maintained so a
// platform added to targets.json cannot be forgotten here.
//
// Run this only once the platform packages for `version` exist on the registry: the declaration is
// recorded but no lockfile entry resolves for a name that cannot be looked up, and a frozen install
// then rejects the lockfile as out of sync. Metadata is all that has to be there — refresh with
// `bun install --lockfile-only`, which resolves every entry without downloading a tarball and so
// cannot race the one package that matches the current host. A full install can, and did.
async function sync() {
  const pkg = await manifest();
  pkg.optionalDependencies = Object.fromEntries(
    Object.keys(TARGETS).map((triplet) => [packageName(triplet), pkg.version]),
  );
  await writeFile(PACKAGE_JSON, JSON.stringify(pkg, null, 2) + "\n");
  console.log(
    `optionalDependencies synced to ${Object.keys(TARGETS).length} targets at ${pkg.version}`,
  );
  console.log("run `bun install --lockfile-only` to refresh the lockfile");
}

const [cmd, ...args] = process.argv.slice(2);

if (cmd === "build") {
  const [triplet, stagingDir, assetsDir] = args;
  if (!triplet || !stagingDir || !assetsDir) {
    console.error(
      "usage: node scripts/platform-package.mjs build <triplet> <staging-dir> <assets-dir>",
    );
    process.exit(1);
  }
  await build(triplet, stagingDir, assetsDir);
} else if (cmd === "matrix") {
  matrix();
} else if (cmd === "sync") {
  await sync();
} else {
  console.error(
    `usage: node scripts/platform-package.mjs [build <triplet> <dir> <assets-dir> | matrix | sync]`,
  );
  process.exit(1);
}

export { TARGETS, build, sync };
