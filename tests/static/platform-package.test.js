const assert = require("assert");
const zlib = require("zlib");
const { createHash } = require("crypto");
const { spawnSync } = require("child_process");
const {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} = require("fs");
const { join } = require("path");
const { describe, test, before, after } = require("node:test");

const sha256 = (buf) =>
  `sha256:${createHash("sha256").update(buf).digest("hex")}`;

// `platform-package.mjs build` makes each platform package from the release's `<triplet>.gz`.
// `publish-npm` publishes those packages while the release is still a draft, so that a failure
// leaves nothing public (#620), and a draft's assets cannot be downloaded without a token that
// sees drafts -- so the workflow fetches them in a job of its own and `build` reads them from a
// directory. What it still has to do is refuse a binary that is not the one the release was
// snapshotted with, and refuse one it has no recorded hash for.
//
// Run on a copy of the script beside a package.json whose `prebuild` the test writes. Any request
// to the network goes through a proxy on a port nothing listens on, so a `build` that downloads
// rather than reads fails here instead of reaching GitHub.
describe("platform-package build", () => {
  const sandbox = join(
    __dirname,
    "../../.test-sandbox",
    `${process.pid}-platform-package`,
  );
  const assetsDir = join(sandbox, "release");
  const staging = join(sandbox, "staging");
  const binary = Buffer.from("a stand-in for skia.node");
  const gz = zlib.gzipSync(binary);
  let triplet;
  let manifest;

  before(() => {
    for (const file of [
      "scripts/platform-package.mjs",
      "lib/targets.json",
      "lib/binary.js",
      "package.json",
      "LICENSE",
    ])
      cpSync(join(__dirname, "../..", file), join(sandbox, file));
    manifest = JSON.parse(readFileSync(join(sandbox, "package.json")));
    triplet = Object.keys(
      JSON.parse(readFileSync(join(sandbox, "lib/targets.json"))),
    )[0];
    mkdirSync(assetsDir, { recursive: true });
    writeFileSync(join(assetsDir, `${triplet}.gz`), gz);
  });

  after(() => rmSync(sandbox, { recursive: true, force: true }));

  const build = (prebuild) => {
    writeFileSync(
      join(sandbox, "package.json"),
      JSON.stringify({ ...manifest, prebuild }, null, 2),
    );
    rmSync(staging, { recursive: true, force: true });
    const done = spawnSync(
      process.execPath,
      [
        join(sandbox, "scripts/platform-package.mjs"),
        "build",
        triplet,
        staging,
        assetsDir,
      ],
      {
        cwd: sandbox,
        env: { ...process.env, https_proxy: "http://127.0.0.1:9" },
        encoding: "utf8",
      },
    );
    return {
      status: done.status,
      output: `${done.stdout}${done.stderr}`,
      dir: join(staging, `meo-skia-canvas-${triplet}`),
    };
  };

  test("stages the binary it was handed, as the release recorded it", () => {
    const { status, output, dir } = build({ [`${triplet}.gz`]: sha256(gz) });
    assert.strictEqual(status, 0, output);
    assert.deepStrictEqual(readFileSync(join(dir, "skia.node")), binary);
    const pkg = JSON.parse(readFileSync(join(dir, "package.json")));
    assert.strictEqual(pkg.name, `meo-skia-canvas-${triplet}`);
    assert.strictEqual(pkg.version, manifest.version);
  });

  test("refuses a binary whose hash is not the one recorded", () => {
    const { status, output, dir } = build({
      [`${triplet}.gz`]: sha256(Buffer.from("another binary")),
    });
    assert.strictEqual(status, 1, output);
    assert.match(output, /failed integrity check/);
    assert.ok(!existsSync(join(dir, "skia.node")), "left the binary staged");
  });

  test("refuses a target the release has no recorded hash for", () => {
    const { status, output, dir } = build({});
    assert.strictEqual(status, 1, output);
    assert.match(output, /records no hash for/);
    assert.ok(!existsSync(join(dir, "skia.node")), "left the binary staged");
  });
});
