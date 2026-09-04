import zlib from "zlib";
import stream from "stream";
import crypto from "crypto";
import child_process from "child_process";
import {
  createReadStream,
  createWriteStream,
  existsSync,
  readFileSync,
} from "fs";
import { readFile, rm } from "fs/promises";
import { resolve } from "path";
import { pathToFileURL } from "url";
import { promisify } from "util";
import { family } from "detect-libc";
import https from "follow-redirects/https.js";
import { HttpsProxyAgent } from "https-proxy-agent";

const pipeline = promisify(stream.pipeline);
const exec = promisify(child_process.exec);

const ROOT = resolve(`${import.meta.dirname}/..`);
const REPO_URL = "https://github.com/l7aromeo/meo-skia-canvas";
const BINARY_HOST = `${REPO_URL}/releases/download`;
const BINARY_PATH = `${ROOT}/lib/skia.node`;
const PACKAGE_JSON = `${ROOT}/package.json`;
// The assets `snapshot` records a digest for, beyond the per-target binaries.
const LAMBDA_ARCHIVES = ["aws-lambda-x64.zip", "aws-lambda-arm64.zip"];
const PROXY_URL =
  process.env.https_proxy ||
  process.env.HTTPS_PROXY ||
  process.env.http_proxy ||
  process.env.HTTP_PROXY ||
  process.env.npm_config_https_proxy ||
  process.env.npm_config_proxy;

// `node-addon` registers the `#[neon::main]` entry point so the
// resulting cdylib loads as a Node.js addon. The crate's default
// features are empty (Rust-library-friendly), so the Node build must
// opt in here.
const CARGO_FEATURES = {
  darwin: "node-addon,metal,window",
  linux: "node-addon,vulkan,window,freetype",
  win32: "node-addon,vulkan,window",
}[process.platform];

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

async function config() {
  let package_json = JSON.parse(await readFile(PACKAGE_JSON)),
    { platform, arch } = process,
    libc = await family();

  let { version, prebuild } = package_json,
    triplet = [platform, arch, libc].filter((t) => t).join("-");

  return { version, triplet, prebuild };
}

async function snapshot() {
  // Between a version bump and this call, package.json carries version N alongside the hashes for
  // N-1. That window is not closed by clearing the hashes at bump time, and doing so would make it
  // worse: `download` treats an absent `prebuild` key as a repo copy with nothing to verify
  // against, so an empty manifest is the one state that installs a binary unchecked.
  //
  // Leaving the previous release's hashes in place fails loudly instead. A `git` install in the
  // window either 404s, because release N has no assets yet, or fails the integrity check against
  // N-1's digests -- and both fall back to compiling from source, which is slow and correct. The
  // window is only reachable by installing from the repository at a commit between the bump and
  // the publish; a released package always carries the hashes for its own assets.
  //
  // The REST `/releases/{id}/assets` endpoint exposes each asset's
  // `digest` (`sha256:<hex>`) in every gh version. `gh release view
  // --json assets` only started including the field in gh 2.51+;
  // older gh installs would silently snapshot a map of
  // `{ name: undefined }`, producing an empty `prebuild` object and
  // disabling the consumer-side integrity check.
  let { version } = await config(),
    releases = JSON.parse(
      (await exec(`gh api repos/l7aromeo/meo-skia-canvas/releases --paginate`))
        .stdout,
    ),
    release = releases.find(
      (r) => r.name === `v${version}` || r.tag_name === `v${version}`,
    );
  if (!release) {
    throw new Error(
      `release v${version} not found on l7aromeo/meo-skia-canvas`,
    );
  }
  let assets = JSON.parse(
      (
        await exec(
          `gh api repos/l7aromeo/meo-skia-canvas/releases/${release.id}/assets`,
        )
      ).stdout,
    ),
    hashes = Object.fromEntries(
      assets.map(({ name, digest }) => [name, digest]),
    );
  // GitHub returns no `digest` for an asset it has not finished processing, and
  // `Object.fromEntries` puts `undefined` in for it, which `JSON.stringify` then drops silently --
  // so a short manifest is a normal-looking write, not an error. `publish-npm` counts `.gz` assets
  // on the *release* before snapshotting and checks only that package.json changed afterwards,
  // which is a different quantity and a weaker question. Count what is about to be written.
  // Read here rather than at module load. Only `snapshot` needs the platform list, and `snapshot`
  // runs on a maintainer's machine -- but this module is what the `install` script imports, so a
  // top-level read happens on every install of the package. `lib/targets.json` does ship today, so
  // nothing is broken; the point is that it need not be load-bearing for installing at all. Any
  // future change to `files` would turn a publish-time guard into an import-time crash for
  // consumers.
  //
  // `targets.json` is the single source of the platform list -- the same file `lib/binary.js`
  // derives `PLATFORM_PACKAGES` from and `npm run sync-targets` regenerates
  // `optionalDependencies` from -- so this count cannot drift from the targets themselves.
  let targets = JSON.parse(readFileSync(`${ROOT}/lib/targets.json`, "utf8")),
    expected = Object.keys(targets).length + LAMBDA_ARCHIVES.length,
    written = Object.keys(hashes).length;
  if (written !== expected)
    throw Error(
      `snapshot would write ${written} integrity entries, expected ${expected} (${Object.keys(targets).length} binaries + ${LAMBDA_ARCHIVES.length} Lambda archives).\nGitHub returns no digest for an asset it is still processing, and those entries vanish silently. Wait for the release assets to settle and run this again.`,
    );
  await exec(`npm pkg set prebuild='${JSON.stringify(hashes)}' --json`);
}

// Every upload here passes `--clobber`, and a published release's assets are the ones the
// published npm package pins by sha256. Replacing them breaks the integrity check for
// everyone installing through the download fallback -- a rebuild goes to a new version
// instead. There is deliberately no override: the safe action and the recorded policy are
// the same one.
//
// `!== true` rather than `=== false`: a guard whose job is refusing must not need a positive
// sighting of the unsafe state to fire. A missing field, a changed shape or a null all mean
// "could not confirm this is a draft", and that is the conservative answer.
//
// Separate from `ensureRelease` because it is asked twice -- once for a release that was
// already there, and again by whichever caller loses the race to create one.
function assertDraft(tag, stdout) {
  if (JSON.parse(stdout).isDraft !== true)
    throw Object.assign(
      Error(
        `Release ${tag} is already published -- refusing to replace its assets.\n` +
          `The published package pins the sha256 of the assets attached to it, so replacing them fails the integrity check for every install through the download fallback. Bump the version and build that.`,
      ),
      { published: true },
    );
}

const VIEW_RELEASE = (tag) =>
  `gh release view ${tag} --json isDraft -R l7aromeo/meo-skia-canvas`;

async function ensureRelease(version) {
  let tag = `v${version}`;
  try {
    assertDraft(tag, (await exec(VIEW_RELEASE(tag))).stdout);
  } catch (e) {
    // Rethrown rather than treated as "no such release": the guard above is the whole point, and
    // this catch exists only for a tag that genuinely does not exist yet.
    if (e.published) throw e;
    // Only a genuinely missing release justifies creating one. gh not installed, not
    // authenticated, or unable to reach the network all land here too, and reporting those as
    // "not found" names the wrong cause at the moment someone is trying to find the right one.
    let why = `${e.stderr || e.message || ""}`;
    if (!/release not found|could not find|HTTP 404/i.test(why))
      throw Error(`Could not read release ${tag}: ${why.trim() || e}`, {
        cause: e,
      });
    console.log(`Release ${tag} not found, creating draft...`);
    try {
      await exec(
        `gh release create ${tag} --draft --title ${tag} --generate-notes -R l7aromeo/meo-skia-canvas`,
      );
    } catch (raced) {
      // `build.yml` starts its legs with no ordering between them, so several can read the
      // same 404 and each try to create. Losing that race is the ordinary outcome and not a
      // failure: the release the caller asked for now exists.
      //
      // Re-read rather than assumed, because "already exists" says nothing about `isDraft`.
      // Skipping the second check would let a losing leg upload to a published release --
      // the one thing this function exists to refuse.
      //
      // Both spellings. The conflict arrives as GitHub's own `already_exists`, inside an
      // `HTTP 422: Validation Failed`; matching only the prose form would leave this branch
      // unreachable while reading as though it worked.
      if (!/already[_ ]exists/i.test(`${raced.stderr || raced.message || ""}`))
        throw raced;
      console.log(`Release ${tag} created by another job, re-reading it...`);
      assertDraft(tag, (await exec(VIEW_RELEASE(tag))).stdout);
    }
  }
}

async function upload() {
  let { version, triplet } = await config(),
    artifact = `${ROOT}/${triplet}.gz`;

  try {
    await pipeline(
      createReadStream(BINARY_PATH),
      zlib.createGzip(),
      createWriteStream(artifact),
    );
    await ensureRelease(version);
    await exec(
      `gh release upload v${version} ${artifact} --clobber -R l7aromeo/meo-skia-canvas`,
    );
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
}

// A response the server actually sent. Nothing about asking again would change it, so these are
// the failures that end the download rather than restart it.
const answered = (e) => Object.assign(e, { retryable: false });

// Requests the asset and returns the response stream. The `error` listener is what makes the
// rejection possible at all: without one, a connection that dies before answering emits `error` on
// a request nobody is listening to, and the process either takes an unhandled 'error' event or --
// as here, where the promise simply never settles -- hangs. Neither reaches the caller's `catch`,
// which is where the `--or-compile` fallback lives.
function request(url, agent) {
  return new Promise((res, rej) => {
    let req = https.get(url, { agent }, (resp) => {
      let { statusCode: status } = resp;
      if (status == 404)
        rej(
          answered(
            Error(
              `Prebuilt library not found at "${url}" (HTTP error ${status})`,
            ),
          ),
        );
      else if (status < 200 || status >= 300)
        rej(
          answered(
            Error(
              `Failed to load prebuilt binary from "${url}" (HTTP error ${status})`,
            ),
          ),
        );
      else res(resp);
    });

    req.on("error", rej);
  });
}

// One attempt at the whole transfer, returning the digest of what arrived. The write is inside
// because a connection can also die mid-body: that leaves a truncated skia.node on disk, which the
// caller clears before trying again.
async function attemptDownload(url, agent) {
  let body = await request(url, agent);
  console.log(`Fetched prebuilt library from "${url}"`);

  // write to /lib/skia.node while also hashing the .gz file
  let sha = new Hasher();
  let gunzip = zlib.createGunzip();
  await pipeline(body, sha, gunzip, createWriteStream(BINARY_PATH));

  return sha.digest;
}

// Three in total. A reset connection took a CI job down while the same asset downloaded on three
// other runners in the same minute, and an install has no second runner to compare against: the
// alternative to one retry is an hour of compiling Skia, or a failed install.
const ATTEMPTS = 3;
const RETRY_DELAY_MS = 250;

const wait = (ms) => new Promise((res) => setTimeout(res, ms));

async function download(...args) {
  if (existsSync(BINARY_PATH)) return; // nothing to be done if skia.node already exists

  let { version, triplet, prebuild } = await config(),
    url = `${BINARY_HOST}/v${version}/${triplet}.gz`,
    agent = PROXY_URL ? new HttpsProxyAgent(PROXY_URL) : undefined;

  try {
    let actual;

    for (let attempt = 1; ; attempt++) {
      try {
        actual = await attemptDownload(url, agent);
        break;
      } catch (e) {
        // Whatever a failed attempt left behind is partial by definition, and `download` returns
        // early when a binary is already present -- so clearing it is what lets the next attempt,
        // or a later `npm rebuild`, do anything at all.
        await rm(BINARY_PATH, { force: true });
        if (e.retryable === false || attempt >= ATTEMPTS) throw e;

        console.warn(
          `${e.message} — retrying (${attempt}/${ATTEMPTS - 1})`.trim(),
        );
        await wait(RETRY_DELAY_MS * attempt);
      }
    }

    // Verify against the `prebuild` manifest in package.json, which is committed on `main` and
    // shipped verbatim by `npm publish` -- so every clone, fork, tarball and published package has
    // it. There is deliberately no branch for its absence: the only ways to arrive here without it
    // are `npm pkg delete prebuild` and a hand edit, and neither is a reason to install a binary
    // unchecked. `snapshot` cannot produce it either; `npm pkg set` can only add the key.
    //
    // Outside the loop deliberately. A digest that does not match is an answer about the bytes on
    // the release, not a transfer that went wrong, and asking again would arrive at the same one.
    //
    // A manifest that omits this triplet fails the same way, and for the same reason: it is a
    // manifest that does not cover the bytes just downloaded, not an absence of anything to check.
    // Reading a missing entry as "nothing to check" would install an unverified binary on exactly
    // the platform the release forgot.
    let official = (prebuild || {})[`${triplet}.gz`];
    if (!official) {
      await rm(BINARY_PATH, { force: true });
      throw Error(
        `Prebuilt library file '${triplet}.gz' is not listed in this package's integrity manifest\nDownloaded: ${url}`,
      );
    }
    if (actual != official) {
      await rm(BINARY_PATH, { force: true });
      throw Error(
        `Prebuilt library file '${triplet}.gz' failed integrity check\nDownloaded: ${url}\nExpected: ${official}\nReceived: ${actual}`,
      );
    }
  } catch (e) {
    console.warn(e.message);

    // optionally fall back to compiling locally
    //
    // Throws rather than calling process.exit: exiting from here takes down whatever imported this
    // module, which made the integrity check impossible to cover. `main` turns it back into an
    // exit status for the CLI.
    if (!args.includes("--or-compile") || !existsSync(`${ROOT}/Cargo.toml`))
      throw e;
    else compile("--fallback");
  }
}

function compile(...args) {
  let optimization =
      args.includes("custom") || args.includes("dev") ? "" : "--release",
    customFeatures =
      args.includes("custom") &&
      (args[args.indexOf("custom") + 1] || "").replace(/[^a-z0-9_,-]/g, ""),
    features = `--features "${args.includes("custom") ? customFeatures || "" : CARGO_FEATURES}"`,
    isFallback = args.includes("--fallback"),
    isSrcRepo = existsSync(`${ROOT}/Cargo.toml`);

  if (!isSrcRepo)
    throw Error(
      `Cannot compile from npm version of meo-skia-canvas: clone source from ${REPO_URL}`,
    );
  else if (isFallback) console.log("\nAttempting to rebuild locally...");
  else
    console.warn(
      `cargo build ${[optimization, features].filter((s) => s).join(" ")}`,
    );

  let { status } = child_process.spawnSync(
    // The npm package and the cargo crate are both named `meo-skia-canvas`, so `-nc` would work
    // here too. The explicit `-a cdylib meo-skia-canvas` form is kept so the artifact is named
    // outright rather than inferred from package.json, which the two names drifting apart again
    // would otherwise break silently.
    `cargo-cp-artifact -a cdylib meo-skia-canvas ${BINARY_PATH} -- cargo build --message-format=json-render-diagnostics ${optimization} ${features}`,
    { shell: true, stdio: "inherit" },
  );

  process.exit(status);
}

async function usage() {
  let { triplet, version } = await config();
  console.log("usage: prebuild.mjs <action>");
  console.log("\nactions:");
  console.log(
    `   compile - build /lib/skia.node from source using locally installed rustc`,
  );
  console.log(
    `  download - fetch precompiled /lib/skia.node appropriate for this platform (${triplet})`,
  );
  console.log(
    `    upload - post this platform's skia.node to the ${version} release on GitHub`,
  );
  console.log(
    `  snapshot - add hashes of all uploaded assets to package.json (for publishing)`,
  );
}

async function main() {
  let cmd = process.argv[2],
    args = process.argv.slice(3);

  try {
    await ({ upload, download, snapshot, compile }[cmd] || usage)(...args);
  } catch {
    // Whatever failed has already printed its reason; this only sets the exit status.
    process.exit(1);
  }
}

// Only dispatch when run as a script. Importing this module used to execute a command, which meant
// the download path — the one that fetches a binary and runs it — could not be covered by a test
// without also running it for real.
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}

export {
  config,
  download,
  upload,
  snapshot,
  compile,
  BINARY_PATH,
  BINARY_HOST,
};
