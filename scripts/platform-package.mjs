//
// Builds the per-target npm packages that carry the prebuilt binaries.
//
// These are the same `<triplet>.gz` assets the `install` script downloads from GitHub releases,
// republished so a package manager can select one declaratively. Nothing is compiled here: this
// repackages what the release already contains.
//
//   node scripts/platform-package.mjs build <triplet> <staging-dir>
//   node scripts/platform-package.mjs matrix
//

import zlib from "zlib";
import stream from "stream";
import crypto from "crypto";
import { createWriteStream } from "fs";
import { mkdir, readFile, writeFile, rm } from "fs/promises";
import { resolve } from "path";
import { promisify } from "util";
import https from "follow-redirects/https.js";
import { HttpsProxyAgent } from "https-proxy-agent";

const pipeline = promisify(stream.pipeline);

const PROXY_URL =
  process.env.https_proxy ||
  process.env.HTTPS_PROXY ||
  process.env.http_proxy ||
  process.env.HTTP_PROXY ||
  process.env.npm_config_https_proxy ||
  process.env.npm_config_proxy;

const ROOT = resolve(`${import.meta.dirname}/..`);
const PACKAGE_JSON = `${ROOT}/package.json`;
const REPO_URL = "https://github.com/phyrondev/phyron-skia-canvas";

// `os` and `cpu` are matched by every package manager; `libc` is honoured by npm, pnpm, yarn and
// bun, and is the only way to tell a glibc build from a musl one. Without it an Alpine install
// silently receives a glibc binary that fails to load.
const TARGETS = {
  "darwin-arm64": { os: ["darwin"], cpu: ["arm64"] },
  "linux-x64-glibc": { os: ["linux"], cpu: ["x64"], libc: ["glibc"] },
  "linux-x64-musl": { os: ["linux"], cpu: ["x64"], libc: ["musl"] },
  "linux-arm64-glibc": { os: ["linux"], cpu: ["arm64"], libc: ["glibc"] },
  "linux-arm64-musl": { os: ["linux"], cpu: ["arm64"], libc: ["musl"] },
  "win32-x64": { os: ["win32"], cpu: ["x64"] },
  "win32-arm64": { os: ["win32"], cpu: ["arm64"] },
};

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

// Downloads and expands `<triplet>.gz`, checking it against the hash the release was snapshotted
// with. A silent mismatch would ship a corrupt binary to every consumer on that platform, so this
// refuses rather than warns.
async function fetchBinary(triplet, version, expected, dest) {
  const url = `${REPO_URL}/releases/download/v${version}/${triplet}.gz`,
    agent = PROXY_URL ? new HttpsProxyAgent(PROXY_URL) : undefined,
    sha = new Hasher();

  const body = await new Promise((res, rej) => {
    https
      .get(url, { agent }, (resp) => {
        let { statusCode: status } = resp;
        if (status < 200 || status >= 300) {
          rej(Error(`Failed to load prebuilt binary from "${url}" (HTTP error ${status})`));
        } else {
          res(resp);
        }
      })
      .on("error", rej);
  });

  await pipeline(body, sha, zlib.createGunzip(), createWriteStream(dest));

  if (expected && sha.digest != expected) {
    await rm(dest, { force: true });
    throw Error(
      `Prebuilt library file '${triplet}.gz' failed integrity check\n` +
        `Downloaded: ${url}\nExpected: ${expected}\nReceived: ${sha.digest}`,
    );
  }
}

// The `exports` subpath is what the loader probes. Exposing the binary under a named entry rather
// than as the package main keeps the bare specifier unresolvable, so a half-installed package
// fails at resolution instead of loading something unexpected.
async function build(triplet, stagingDir) {
  const target = TARGETS[triplet];
  if (!target) throw new Error(`unknown target: ${triplet}`);

  const { version, prebuild, license, repository } = await manifest();
  const dir = resolve(stagingDir, `phyron-skia-canvas-${triplet}`);
  await mkdir(dir, { recursive: true });

  await fetchBinary(triplet, version, prebuild[`${triplet}.gz`], `${dir}/skia.node`);

  await writeFile(
    `${dir}/package.json`,
    JSON.stringify(
      {
        name: `phyron-skia-canvas-${triplet}`,
        version,
        description: `Prebuilt phyron-skia-canvas binary for ${triplet}`,
        license,
        repository,
        ...target,
        exports: { "./skia.node": "./skia.node" },
        files: ["skia.node"],
      },
      null,
      2,
    ) + "\n",
  );

  // Carried so each published package states its own terms rather than pointing at another one.
  await writeFile(`${dir}/LICENSE`, await readFile(`${ROOT}/LICENSE`));

  console.log(dir);
}

function matrix() {
  console.log(JSON.stringify(Object.keys(TARGETS)));
}

const [cmd, ...args] = process.argv.slice(2);

if (cmd === "build") {
  const [triplet, stagingDir] = args;
  if (!triplet || !stagingDir) {
    console.error("usage: node scripts/platform-package.mjs build <triplet> <staging-dir>");
    process.exit(1);
  }
  await build(triplet, stagingDir);
} else if (cmd === "matrix") {
  matrix();
} else {
  console.error(`usage: node scripts/platform-package.mjs [build <triplet> <dir> | matrix]`);
  process.exit(1);
}

export { TARGETS, build };
