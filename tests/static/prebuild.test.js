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
    nock = require("nock");
    mkdirSync(SANDBOX, { recursive: true });
    cpSync(join(__dirname, "../../lib"), join(SANDBOX, "lib"), {
      recursive: true,
    });
    cpSync(
      join(__dirname, "../../package.json"),
      join(SANDBOX, "package.json"),
    );
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

  const asset = async () => {
    const { triplet, version } = await prebuild.config();
    return {
      triplet,
      path: `/l7aromeo/meo-skia-canvas/releases/download/v${version}/${triplet}.gz`,
    };
  };

  const serve = async (body, status = 200, times = 1) => {
    const { triplet, path } = await asset();
    nock("https://github.com").get(path).times(times).reply(status, body);
    return triplet;
  };

  // A response that starts arriving and then dies. `nock.replyWithError` would be the obvious way
  // to model a reset, but on nock 14 it consumes the interceptor and emits nothing at all -- the
  // request simply hangs, which tests the test harness rather than this code. Destroying the body
  // stream produces a real ECONNRESET on the response, and reaches `download` through `pipeline`.
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
    // connection that failed before answering settled nothing: with the listener removed this does
    // not fail, it hangs -- and an install hangs with it, never reaching the `--or-compile`
    // fallback that exists for exactly this moment.
    //
    // No interceptor is registered, so nock refuses the connection and the failure arrives where a
    // refused connection really does: on the request, before any response.
    test("rejects rather than leaving the caller waiting forever", async () => {
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
