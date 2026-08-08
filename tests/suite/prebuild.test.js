const assert = require("assert");
const zlib = require("zlib");
const { createHash } = require("crypto");
const { existsSync, readFileSync, rmSync, writeFileSync, mkdirSync, cpSync } = require("fs");
const { join } = require("path");
const { describe, test, before, after, beforeEach, afterEach } = require("node:test");

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
const sha256 = (buf) => `sha256:${createHash("sha256").update(buf).digest("hex")}`;

describe("prebuild download", () => {
  let assetPath;
  let manifestPath;

  before(async () => {
    nock = require("nock");
    mkdirSync(SANDBOX, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(SANDBOX, "lib"), { recursive: true });
    cpSync(join(__dirname, "../../package.json"), join(SANDBOX, "package.json"));
    rmSync(join(SANDBOX, "lib/skia.node"), { force: true });

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

  const serve = async (body, status = 200) => {
    const { triplet, version } = await prebuild.config();
    nock("https://github.com")
      .get(`/l7aromeo/meo-skia-canvas/releases/download/v${version}/${triplet}.gz`)
      .reply(status, body);
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
    assert.ok(!existsSync(assetPath), "a failed download must not leave a partial binary in place");
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
});
