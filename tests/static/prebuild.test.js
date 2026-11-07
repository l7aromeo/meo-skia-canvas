const assert = require("assert");
const zlib = require("zlib");
const { Readable } = require("stream");
const { createHash } = require("crypto");
const {
  existsSync,
  readFileSync,
  rmSync,
  writeFileSync,
  mkdirSync,
  cpSync,
} = require("fs");
const { join } = require("path");
const {
  describe,
  test,
  before,
  after,
  beforeEach,
  afterEach,
} = require("node:test");

// The install path fetches a binary over the network and then hands it to `dlopen`. Nothing here
// was covered before: the module ran a CLI command on import, so importing it to test the download
// also performed one. It exports its functions now, and these run against an intercepted registry
// rather than GitHub.
//
// This matters more than the line count suggests. The dependency bumps that touch this code —
// follow-redirects, https-proxy-agent, detect-libc — are used *only* here, so a green suite that
// skipped it told you nothing about them.
let nock;
let prebuild;

// Runs against a copy of lib/, so a test that writes skia.node cannot clobber a real binary.
// Kept inside the repository rather than in the system temp directory: the copy imports
// detect-libc and follow-redirects, and Node resolves those by walking up to node_modules.
const SANDBOX = join(__dirname, "../../.test-sandbox", String(process.pid));

const gzipped = (bytes) => zlib.gzipSync(Buffer.from(bytes));
const sha256 = (buf) =>
  `sha256:${createHash("sha256").update(buf).digest("hex")}`;

describe("prebuild download", () => {
  let assetPath;
  let manifestPath;

  before(async () => {
    // nock is an ES module, and `require` hands back its namespace; the
    // function is the default export.
    nock = require("nock").default;
    mkdirSync(SANDBOX, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(SANDBOX, "lib"), {
      recursive: true,
    });
    cpSync(
      join(__dirname, "../../package.json"),
      join(SANDBOX, "package.json"),
    );
    rmSync(join(SANDBOX, "lib/skia.node"), { force: true });
    // The sandbox is shaped like the published package, so `download` does nothing where a
    // platform package resolves -- and walking up from here finds the repository's own. A
    // package directory per probed name, exporting nothing, stops that walk: each name then
    // fails to resolve, as on a platform with no package, and every case below downloads.
    const probed =
      (require(join(SANDBOX, "lib/binary.js")).PLATFORM_PACKAGES[
        process.platform
      ] || {})[process.arch] || [];
    for (const name of probed) {
      const dir = join(SANDBOX, "node_modules", name);
      mkdirSync(dir, { recursive: true });
      writeFileSync(
        join(dir, "package.json"),
        JSON.stringify({ name, version: "0.0.0", exports: {} }),
      );
    }

    manifestPath = join(SANDBOX, "package.json");
    assetPath = join(SANDBOX, "lib/skia.node");

    prebuild = await import(`file://${join(SANDBOX, "lib/prebuild.mjs")}`);
    nock.disableNetConnect();
  });

  after(() => {
    nock.enableNetConnect();
    nock.cleanAll();
    rmSync(SANDBOX, { recursive: true, force: true });
  });

  beforeEach(() => {
    rmSync(assetPath, { force: true });
  });

  afterEach(() => {
    nock.cleanAll();
  });

  const setHashes = (hashes) => {
    const pkg = JSON.parse(readFileSync(manifestPath, "utf8"));
    pkg.prebuild = hashes;
    writeFileSync(manifestPath, JSON.stringify(pkg, null, 2));
  };

  const asset = async () => {
    const { triplet, version } = await prebuild.config();
    return {
      triplet,
      path: `/l7aromeo/meo-skia-canvas/releases/download/npm-v${version}/${triplet}.gz`,
    };
  };

  const serve = async (body, status = 200, times = 1) => {
    const { triplet, path } = await asset();
    nock("https://github.com").get(path).times(times).reply(status, body);
    return triplet;
  };

  // A response that starts arriving and then dies. `nock.replyWithError` is not this: it fails the
  // request before any response exists, which is the refused connection tested further down.
  // Destroying the body stream part-way produces a real ECONNRESET on the response, and reaches
  // `download` through `pipeline`.
  const serveHalfATransfer = async () => {
    const { triplet, path } = await asset();
    nock("https://github.com")
      .get(path)
      .reply(200, () => {
        let started = false;
        return new Readable({
          read() {
            if (started)
              this.destroy(
                Object.assign(Error("socket hang up"), { code: "ECONNRESET" }),
              );
            else this.push(Buffer.alloc(16));
            started = true;
          },
        });
      });
    return triplet;
  };

  test("names the asset by platform, arch and libc", async () => {
    const { triplet } = await prebuild.config();
    const [platform, arch] = triplet.split("-");

    assert.strictEqual(platform, process.platform);
    assert.strictEqual(arch, process.arch);
    // The third segment only exists on Linux, and is the whole reason per-target packages are
    // needed: a glibc binary loaded on musl fails at dlopen with nothing useful to say.
    if (process.platform === "linux") {
      assert.match(triplet, /-(glibc|musl)$/);
    }
  });

  test("writes the decompressed binary when the hash matches", async () => {
    const payload = Buffer.from("not-really-a-binary");
    const archive = gzipped(payload);
    const triplet = await serve(archive);
    setHashes({ [`${triplet}.gz`]: sha256(archive) });

    await prebuild.download();

    assert.ok(existsSync(assetPath), "expected lib/skia.node to be written");
    assert.deepStrictEqual(readFileSync(assetPath), payload);
  });

  // The check that stands between a tampered release asset and `dlopen`.
  test("refuses a binary whose hash does not match, and leaves nothing behind", async () => {
    const archive = gzipped("tampered-payload");
    const triplet = await serve(archive);
    setHashes({ [`${triplet}.gz`]: `sha256:${"0".repeat(64)}` });

    await assert.rejects(() => prebuild.download(), /integrity check/i);
    assert.ok(
      !existsSync(assetPath),
      "a failed download must not leave a partial binary in place",
    );
  });

  // A manifest that lists other triplets but not this one is not "nothing to check" -- it is a
  // manifest that does not cover the bytes just downloaded. Accepting them would install an
  // unverified binary on exactly the platform the release forgot.
  test("refuses a binary whose triplet is absent from the manifest", async () => {
    const archive = gzipped("unverified-payload");
    await serve(archive);
    setHashes({ "some-other-triplet.gz": sha256(archive) });

    await assert.rejects(() => prebuild.download(), /not listed/i);
    assert.ok(
      !existsSync(assetPath),
      "an unverifiable download must not leave a binary in place",
    );
  });

  // `snapshot` warns that a gh older than 2.51 writes `{ name: undefined }`, which survives
  // JSON.stringify as `{}`. That produced a manifest which verified nothing at all, silently.
  test("refuses an empty manifest rather than treating it as nothing to check", async () => {
    const archive = gzipped("unverified-payload");
    await serve(archive);
    setHashes({});

    await assert.rejects(() => prebuild.download(), /not listed/i);
    assert.ok(!existsSync(assetPath));
  });

  // There is no such thing as a copy with nothing to verify against. package.json carries
  // `prebuild` on `main` and `npm publish` ships it verbatim, so a missing manifest means someone
  // removed it -- which is not a reason to install a binary unchecked.
  test("refuses when the package carries no manifest at all", async () => {
    await serve(gzipped("unverified-payload"));
    setHashes(undefined);

    await assert.rejects(() => prebuild.download(), /not listed/i);
    assert.ok(!existsSync(assetPath));
  });

  test("skips the download when a binary is already present", async () => {
    writeFileSync(assetPath, "existing");
    // No interceptor is registered, so any request would fail against disableNetConnect.
    await prebuild.download();
    assert.strictEqual(readFileSync(assetPath, "utf8"), "existing");
  });

  test("reports a missing asset rather than writing an empty file", async () => {
    await serve("Not Found", 404);
    setHashes({});

    await assert.rejects(() => prebuild.download(), /404|not found/i);
    assert.ok(!existsSync(assetPath));
  });

  // A reset connection is the failure this path actually sees. It took a CI job down while the
  // same asset downloaded fine on three other runners, and it reaches users too: `install` runs
  // this with `--or-compile`, so the fallback is only reachable if the failure arrives as a
  // rejection rather than as an unhandled 'error' event on the request.
  describe("a connection that fails mid-request", () => {
    test("is retried, and the download succeeds when a later attempt lands", async () => {
      const payload = Buffer.from("not-really-a-binary");
      const archive = gzipped(payload);
      const triplet = await serveHalfATransfer();
      await serve(archive);
      setHashes({ [`${triplet}.gz`]: sha256(archive) });

      await prebuild.download();

      // Not merely present: the 16 bytes the dead attempt wrote had to be cleared first, or the
      // early return for an existing binary would have kept the truncated file forever.
      assert.deepStrictEqual(readFileSync(assetPath), payload);
    });

    // The regression test for the defect itself. Nothing listened for `error` on the request, so a
    // connection that failed before answering never rejected the download, and the `--or-compile`
    // fallback that exists for exactly this moment was never reached. With the listener removed
    // this test fails: the request's error has nowhere to go and surfaces uncaught, instead of as
    // the rejection asserted below.
    //
    // No interceptor is registered, so nock refuses the connection and the failure arrives where a
    // refused connection really does: on the request, before any response.
    test("rejects rather than taking the process down", async () => {
      setHashes({});

      await assert.rejects(
        () => prebuild.download(),
        /disallowed net connect|ENETUNREACH/i,
      );
      assert.ok(!existsSync(assetPath));
    });
  });

  // Retrying is for connections that failed to deliver an answer. A 404 and a bad digest are
  // answers -- repeating either one just spends the user's time arriving at the same place.
  describe("what is not retried", () => {
    test("a missing asset is asked for once", async () => {
      await serve("Not Found", 404, 2);
      setHashes({});

      await assert.rejects(() => prebuild.download(), /404|not found/i);
      assert.strictEqual(nock.pendingMocks().length, 1);
    });

    test("a failed integrity check is not asked for again", async () => {
      const archive = gzipped("tampered-payload");
      const triplet = await serve(archive, 200, 2);
      setHashes({ [`${triplet}.gz`]: `sha256:${"0".repeat(64)}` });

      await assert.rejects(() => prebuild.download(), /integrity check/i);
      assert.strictEqual(nock.pendingMocks().length, 1);
    });
  });
});

// `compile` hands the copy to cargo-cp-artifact, which copies only when the
// artifact is newer than the file already at the destination. A binary put in
// lib/ after the artifact was built -- a release build, then a dev one that
// cargo finds up to date -- is newer, so the dev build used to leave the
// release binary in place, and a suite run after it tested the wrong build.
// The stand-in below copies by the same rule, so this fails unless `compile`
// clears the destination first.
describe("prebuild compile", () => {
  const sandbox = join(
    __dirname,
    "../../.test-sandbox",
    `compile-${process.pid}`,
  );
  const bin = join(sandbox, "bin");
  const artifact = join(sandbox, "artifact.node");
  let path;

  before(() => {
    mkdirSync(bin, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(sandbox, "lib"), {
      recursive: true,
    });
    cpSync(
      join(__dirname, "../../package.json"),
      join(sandbox, "package.json"),
    );
    writeFileSync(join(sandbox, "Cargo.toml"), "");
    // Called as `cargo-cp-artifact -a cdylib <name> <destination> -- cargo ...`.
    writeFileSync(
      join(bin, "cargo-cp-artifact"),
      `#!/bin/sh\nif [ ! -e "$4" ] || [ "${artifact}" -nt "$4" ]; then cp "${artifact}" "$4"; fi\n`,
      { mode: 0o755 },
    );
    path = process.env.PATH;
    process.env.PATH = `${bin}:${path}`;
  });

  after(() => {
    process.env.PATH = path;
    rmSync(sandbox, { recursive: true, force: true });
  });

  test(
    "replaces a binary newer than the artifact it builds",
    {
      skip:
        process.platform === "win32" ? "the stand-in is a shell script" : false,
    },
    async () => {
      const destination = join(sandbox, "lib/skia.node");
      writeFileSync(artifact, "the build cargo just made");
      writeFileSync(destination, "a release binary copied in later");
      const later = new Date(Date.now() + 60_000);
      require("fs").utimesSync(destination, later, later);

      const { compile } = await import(
        `file://${join(sandbox, "lib/prebuild.mjs")}`
      );
      assert.strictEqual(compile("dev"), 0);
      assert.strictEqual(
        readFileSync(destination, "utf8"),
        "the build cargo just made",
      );
    },
  );
});

// The install script on a platform whose package already holds the binary. `install` runs
// `download --or-compile`, and a platform package installs its binary where `lib/binary.js`
// resolves it, not at `lib/skia.node` -- so the early return for an existing `lib/skia.node`
// never saw it, and an install with no route to GitHub failed although npm had delivered the
// binary. Each case runs the script as `install` does, in a sandbox shaped like the published
// package (no `Cargo.toml`), with every proxy variable pointed at a port nothing listens on:
// a request shows as ECONNREFUSED and a failed install, no request as a clean exit.
describe("prebuild download beside a platform package", () => {
  const { spawnSync } = require("child_process");
  const sandbox = join(
    __dirname,
    "../../.test-sandbox",
    `${process.pid}-packaged`,
  );
  const binary = join(sandbox, "lib/skia.node");
  const refused = "http://127.0.0.1:9";
  let candidates;

  // A package directory for each name `lib/binary.js` probes on this host, in the sandbox's own
  // node_modules, so the repository's installed platform package -- found by walking up -- cannot
  // decide the case. Without `./skia.node` in its exports a candidate fails to resolve with
  // ERR_PACKAGE_PATH_NOT_EXPORTED, which `lib/binary.js` reads as absent; with it, it resolves.
  const shadow = (resolving) => {
    for (const name of candidates) {
      const dir = join(sandbox, "node_modules", name);
      rmSync(dir, { recursive: true, force: true });
      mkdirSync(dir, { recursive: true });
      const pkg = { name, version: "0.0.0", exports: {} };
      if (resolving) {
        pkg.exports = { "./skia.node": "./skia.node" };
        writeFileSync(join(dir, "skia.node"), "a platform package's binary");
      }
      writeFileSync(join(dir, "package.json"), JSON.stringify(pkg));
    }
  };

  const install = (...args) => {
    const env = { ...process.env };
    for (const key of [
      "https_proxy",
      "HTTPS_PROXY",
      "http_proxy",
      "HTTP_PROXY",
      "npm_config_https_proxy",
      "npm_config_proxy",
    ])
      env[key] = refused;
    const run = spawnSync(
      process.execPath,
      [join(sandbox, "lib/prebuild.mjs"), "download", ...args],
      { env, encoding: "utf8" },
    );
    return { status: run.status, output: `${run.stdout}${run.stderr}` };
  };

  before(() => {
    mkdirSync(sandbox, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(sandbox, "lib"), {
      recursive: true,
    });
    cpSync(
      join(__dirname, "../../package.json"),
      join(sandbox, "package.json"),
    );
    candidates =
      (require(join(sandbox, "lib/binary.js")).PLATFORM_PACKAGES[
        process.platform
      ] || {})[process.arch] || [];
  });

  after(() => {
    rmSync(sandbox, { recursive: true, force: true });
  });

  beforeEach(() => {
    rmSync(binary, { force: true });
    rmSync(join(sandbox, "Cargo.toml"), { force: true });
  });

  test("does nothing where the platform package resolves", () => {
    assert.ok(candidates.length > 0, "a platform package for this host");
    shadow(true);
    const { status, output } = install("--or-compile");
    assert.strictEqual(status, 0, output);
    assert.doesNotMatch(output, /ECONNREFUSED|Fetched/);
    assert.ok(!existsSync(binary), "downloaded beside the package");
  });

  test("downloads where no platform package resolves", () => {
    shadow(false);
    const { status, output } = install("--or-compile");
    assert.notStrictEqual(status, 0, output);
    assert.match(output, /ECONNREFUSED/);
  });

  // `download` says why before it throws, since `--or-compile` may yet recover, and the CLI's
  // exit path prints whatever an action threw: the reason has to reach the terminal once, not
  // twice. A retry's line names its retry, so what is counted is the final reason alone.
  test("says why a download failed once", () => {
    shadow(false);
    const { status, output } = install();
    assert.notStrictEqual(status, 0, output);
    const reasons = output
      .split("\n")
      .filter((line) => /ECONNREFUSED/.test(line) && !/retrying/.test(line));
    assert.strictEqual(reasons.length, 1, output);
  });

  // A source checkout asks for `lib/skia.node` itself: CI pins its suite to the binary this
  // downloads, and so does `npm test`, both with the platform packages installed.
  test("downloads in a source checkout even beside a platform package", () => {
    shadow(true);
    writeFileSync(join(sandbox, "Cargo.toml"), "");
    const { status, output } = install();
    assert.notStrictEqual(status, 0, output);
    assert.match(output, /ECONNREFUSED/);
  });
});

// `snapshot` records the sha256 of every asset on this version's release, and `publish-npm` stops
// on it. It looked the release up in a listing of every release, which `exec` holds to 1 MiB of
// output and which passed that at npm-v7.0.0 -- 1,141,154 bytes -- so the release stopped with
// ERR_CHILD_PROCESS_STDIO_MAXBUFFER, and the CLI's exit path printed nothing at all (#617).
// `publish-npm` also runs it while the release is still a draft, so that a failure leaves nothing
// public (#620), and GitHub's by-tag endpoint answers 404 for a draft.
// `gh` and `npm` are stand-ins on PATH here: what `snapshot` asks of `gh`, and what it hands
// `npm pkg set`, is the whole of what is checked, and nothing reaches the network.
describe("prebuild snapshot", () => {
  const { spawnSync } = require("child_process");
  const sandbox = join(
    __dirname,
    "../../.test-sandbox",
    `${process.pid}-snapshot`,
  );
  const bin = join(sandbox, "bin");
  const npmArgs = join(sandbox, "npm-args");
  const releaseId = 4242;
  const skip =
    process.platform === "win32" ? "the stand-ins are shell scripts" : false;
  let tag;
  let assets;
  let path;

  // Answers what `snapshot` asks of `gh`, as GitHub would. A listing of every release is answered
  // with more than the 1 MiB `exec` allows, as GitHub's is. `GH_DRAFT` makes the release a draft,
  // which the REST by-tag endpoint answers 404 for and `gh release view` still finds. `GH_FAIL`
  // makes every call fail as `gh` does.
  const gh = () => `#!/usr/bin/env node
const args = process.argv.slice(2).join(" ");
if (process.env.GH_FAIL) {
  process.stderr.write("HTTP 404: Not Found (https://api.github.com/" + args + ")\\n");
  process.exit(1);
}
const write = (value) => process.stdout.write(JSON.stringify(value));
if (args.includes("--paginate")) {
  const filler = "x".repeat(4096);
  const releases = Array.from({ length: 300 }, (_, i) => ({ id: i, tag_name: "npm-v0.0." + i, body: filler }));
  releases.push({ id: ${releaseId}, tag_name: ${JSON.stringify("TAG")} });
  write(releases);
} else if (args.startsWith("release view " + ${JSON.stringify("TAG")} + " ") && args.includes("--json databaseId")) {
  write({ databaseId: ${releaseId} });
} else if (args.endsWith("/releases/tags/" + ${JSON.stringify("TAG")})) {
  if (process.env.GH_DRAFT) {
    process.stderr.write("gh: Not Found (HTTP 404)\\n");
    process.exit(1);
  }
  write({ id: ${releaseId}, tag_name: ${JSON.stringify("TAG")} });
} else if (args.endsWith("/releases/${releaseId}/assets")) {
  write(${JSON.stringify("ASSETS")});
} else {
  process.stderr.write("unexpected gh call: " + args + "\\n");
  process.exit(1);
}
`;

  before(() => {
    mkdirSync(bin, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(sandbox, "lib"), {
      recursive: true,
    });
    cpSync(
      join(__dirname, "../../package.json"),
      join(sandbox, "package.json"),
    );
    tag = `npm-v${JSON.parse(readFileSync(join(sandbox, "package.json"))).version}`;
    // One asset for each target and each Lambda archive, as `snapshot` counts them.
    const names = [
      ...Object.keys(require(join(sandbox, "lib/targets.json"))).map(
        (triplet) => `${triplet}.gz`,
      ),
      "aws-lambda-x64.zip",
      "aws-lambda-arm64.zip",
    ];
    assets = names.map((name) => ({ name, digest: sha256(name) }));
    writeFileSync(
      join(bin, "gh"),
      gh()
        .replaceAll(JSON.stringify("TAG"), JSON.stringify(tag))
        .replace(JSON.stringify("ASSETS"), JSON.stringify(assets)),
      { mode: 0o755 },
    );
    writeFileSync(
      join(bin, "npm"),
      `#!/bin/sh\nprintf '%s\\n' "$@" > "${npmArgs}"\n`,
      { mode: 0o755 },
    );
    path = process.env.PATH;
    process.env.PATH = `${bin}:${path}`;
  });

  after(() => {
    process.env.PATH = path;
    rmSync(sandbox, { recursive: true, force: true });
  });

  beforeEach(() => rmSync(npmArgs, { force: true }));

  // The CLI, as `publish-npm` runs it, with the stand-ins first on PATH.
  const run = (action, env = {}) => {
    const done = spawnSync(
      process.execPath,
      [join(sandbox, "lib/prebuild.mjs"), action],
      {
        cwd: sandbox,
        env: { ...process.env, ...env },
        encoding: "utf8",
      },
    );
    return { status: done.status, output: `${done.stdout}${done.stderr}` };
  };

  // In this process rather than through the CLI, so that the failure, if any, arrives as the
  // error it is rather than as an exit status. Returns the manifest handed to `npm pkg set`.
  const snapshotHere = async (env = {}) => {
    const { snapshot } = await import(
      `file://${join(sandbox, "lib/prebuild.mjs")}`
    );
    const cwd = process.cwd();
    const saved = { ...process.env };
    process.chdir(sandbox);
    Object.assign(process.env, env);
    try {
      await snapshot();
    } finally {
      process.chdir(cwd);
      for (const key of Object.keys(env))
        if (key in saved) process.env[key] = saved[key];
        else delete process.env[key];
    }
    const args = readFileSync(npmArgs, "utf8").trim().split("\n");
    assert.deepStrictEqual(args.slice(0, 2), ["pkg", "set"]);
    return JSON.parse(args[2].replace(/^prebuild=/, ""));
  };
  const digests = () =>
    Object.fromEntries(assets.map(({ name, digest }) => [name, digest]));

  test(
    "records its release's digests however long the list of releases",
    { skip },
    async () => {
      assert.deepStrictEqual(await snapshotHere(), digests());
    },
  );

  test("records a draft release's digests", { skip }, async () => {
    assert.deepStrictEqual(await snapshotHere({ GH_DRAFT: "1" }), digests());
  });

  // A failure anywhere in an action reaches the terminal: the exit path printed nothing, so a
  // release stopped at `snapshot` with exit status 1 and no reason.
  test("says why an action failed", { skip }, () => {
    const snapshot = run("snapshot", { GH_FAIL: "1" });
    assert.strictEqual(snapshot.status, 1, snapshot.output);
    assert.match(snapshot.output, /HTTP 404: Not Found/);

    // `compile` refuses without a Cargo.toml, as in the published package.
    const compile = run("compile");
    assert.strictEqual(compile.status, 1, compile.output);
    assert.match(compile.output, /Cannot compile from npm version/);
  });
});
