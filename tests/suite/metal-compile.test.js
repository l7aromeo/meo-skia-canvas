// @ts-check

"use strict";

// Skia's Metal backend can wait at most one second for each shader library and
// render pipeline it asks the system to compile, and drops the draw when the
// wait runs out. Upstream Skia waits that way only before macOS 10.15;
// skia-bindings made it wait everywhere, and
// containers/skia-metal-untimed-compile.patch restores the version check.
//
// This proves it of the binary under test. A child process hides the per-user
// Metal shader cache from itself with `sandbox-exec`, so its first draw of a
// new pipeline needs a real compile, and then stops its own MTLCompilerService
// -- the system starts one per client process -- for longer than the timeout.
// A timed build drops the draw and reads transparent; the patched one waits
// for the compiler and reads the colour.
//
// The child finds its service in its own launchd domain, which lists the
// instance started for it, rather than as the service that appeared while it
// drew: other test files' children start services of their own at the same
// time, and one of those must never be stopped.
//
// It needs macOS, the GPU engine and `sandbox-exec`, and skips without them --
// unless `MEO_SKIA_CANVAS_REQUIRE_METAL` is set, as on the release build, where
// a skip would let an unpatched binary through.
//
// One machine is skipped even then: a macOS virtual machine, whose Metal
// device is the Apple Paravirtual device. Its processes share a compiler
// service that was running before the child and outlives it, so there is no
// service of the child's own to stop, and stopping the shared one would stall
// every other process compiling through it. That is the only case
// `sharedService` names; anything else without a service of its own fails. On
// such a machine, which is what build.yml's mac runner is,
// scripts/check-patched-skia.mjs proves the patched source structurally.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib"),
  { execFileSync } = require("child_process"),
  fs = require("fs"),
  path = require("path");

const SANDBOX_EXEC = "/usr/bin/sandbox-exec";
// Three times the one-second timeout, so a timed compile cannot finish inside
// the pause however the scheduler slices it.
const PAUSE_MS = 3000;
// One red pixel drawn at half alpha, read back unpremultiplied.
const HALF_RED = [255, 0, 0, 128];
/** What Metal names the GPU of a macOS virtual machine, as `engine.device`
 * reports it inside a longer name: "Integrated GPU (Apple Paravirtual
 * device)" on GitHub's macos-15-arm64 runner. */
const PARAVIRTUAL_DEVICE = "Apple Paravirtual device";

/**
 * The compiler services launchd lists in a process's own domain, from
 * `launchctl print pid/<pid>`, that were not running before it drew.
 *
 * A service started for a client is listed there by its pid, under an
 * instance name, `com.apple.MTLCompilerService.<UUID>`; an entry with pid 0
 * is not running. One already running before the draw is not the child's own
 * whatever its domain says -- on a virtual machine that would be the shared
 * service.
 *
 * @param {string} domain
 * @param {string[]} before
 * @returns {string[]}
 */
function ownServices(domain, before) {
  return [
    ...domain.matchAll(
      /^\s*([1-9]\d*)\s.*\scom\.apple\.MTLCompilerService(\.\S+)?\s*$/gm,
    ),
  ]
    .map((match) => match[1])
    .filter((pid) => !before.includes(pid));
}

/**
 * The `services` block of `launchctl print pid/<pid>`, or the whole output
 * where it has none.
 *
 * @param {string} domain
 * @returns {string}
 */
function servicesBlock(domain) {
  return (domain.match(/^\s*services = \{[\s\S]*?^\s*\}/m) ?? [domain])[0];
}

/**
 * Why the child had no compiler service of its own to stop, when that is the
 * shared service of a virtual machine, or `null` -- in which case a missing
 * service is a failure.
 *
 * All three must hold: the device is the paravirtual one, no service appeared
 * for the child, and the services running before it are the ones running
 * after, at least one of them. A paravirtual machine that does start one per
 * process runs the test; a real GPU without one fails it.
 *
 * @param {{ engine?: { device?: string }, servicesBefore: string[], servicesAfter: string[] }} seen
 * @returns {string | null}
 */
function sharedService({ engine, servicesBefore, servicesAfter }) {
  const before = new Set(servicesBefore),
    after = new Set(servicesAfter);
  const unchanged =
    before.size > 0 &&
    before.size === after.size &&
    [...before].every((pid) => after.has(pid));
  if (!engine?.device?.includes(PARAVIRTUAL_DEVICE) || !unchanged) return null;
  return (
    `skipped: ${engine.device} compiles through the shared service ` +
    `${[...before].join(", ")}, which this test cannot stop alone; ` +
    "scripts/check-patched-skia.mjs proves the patched source instead"
  );
}

const why =
  process.platform !== "darwin"
    ? "skipped: Metal is macOS only"
    : !fs.existsSync(SANDBOX_EXEC)
      ? "skipped: no sandbox-exec to hide the Metal shader cache with"
      : !new Canvas(4, 4).gpu
        ? "skipped: no GPU engine"
        : null;

// The child's whole run, as JSON on its last line. It finds its compiler service
// by `ownServices` after its first GPU draw. What it saw is reported too, so a
// run where that precondition fails -- a runner whose Metal device compiles
// somewhere else -- says what it found instead.
const child = `
  const { execFileSync, spawn } = require("child_process");
  // \`pgrep\`, not \`ps\`: the sandbox refuses to run \`ps\`. It exits 1 when
  // nothing matches, which here means no compiler service is running yet.
  const pgrep = (args) => {
    try {
      return execFileSync("pgrep", args, { encoding: "utf8" }).split("\\n").filter(Boolean);
    } catch (error) {
      if (error.status === 1) return [];
      throw error;
    }
  };
  const compilers = () => pgrep(["-il", "compiler|mtl|metal"]);
  const { Canvas, ImageData } = require(${JSON.stringify(path.join(__dirname, "../../lib"))});
  const services = () => new Set(pgrep(["-f", "MTLCompilerService.xpc"]));
  const pixel = (canvas) => [...canvas.getContext("2d").getImageData(1, 1, 1, 1).data];
  const before = services();
  const seenBefore = compilers();
  const warmStart = Date.now();
  const warm = new Canvas(4, 4);
  warm.getContext("2d").fillRect(0, 0, 4, 4);
  pixel(warm);
  const warmMs = Date.now() - warmStart;
  const ownServices = ${ownServices};
  // A domain that cannot be read lists no service, which the test then
  // reports with the error rather than dying here without a diagnosis.
  let domain = "", domainError;
  try {
    domain = execFileSync("launchctl", ["print", "pid/" + process.pid], {
      encoding: "utf8" });
  } catch (error) {
    domainError = String(error.message).split("\\n")[0];
  }
  const mine = ownServices(domain, [...before]);
  const servicesBlock = ${servicesBlock};
  const diagnosis = { pid: process.pid, engine: warm.engine, warmMs, mine,
    domainError,
    // What launchd listed, whenever it did not give one service: the format
    // is not documented, so a failure carries the text it was read from.
    domain: mine.length === 1 ? undefined : servicesBlock(domain),
    compilersBefore: seenBefore, compilersAfter: compilers(),
    servicesBefore: [...before], servicesAfter: [...services()] };
  if (mine.length === 1) {
    process.kill(Number(mine[0]), "SIGSTOP");
    spawn("sh", ["-c", "sleep ${PAUSE_MS / 1000}; kill -CONT " + mine[0]], {
      detached: true, stdio: "ignore" }).unref();
  }
  const draw = () => {
    const dot = new Canvas(1, 1);
    dot.getContext("2d").putImageData(
      new ImageData(new Uint8ClampedArray([255, 0, 0, 255]), 1, 1), 0, 0);
    const canvas = new Canvas(4, 4), ctx = canvas.getContext("2d");
    ctx.globalAlpha = 0.5;
    ctx.drawImage(dot, 1, 1);
    return pixel(canvas);
  };
  const start = Date.now();
  const first = draw();
  const ms = Date.now() - start;
  console.log(JSON.stringify({ services: mine.length, first, ms, again: draw(), diagnosis }));
`;

describe("A Metal compile that outlasts a second", () => {
  test(
    "still draws",
    { skip: (!process.env.MEO_SKIA_CANVAS_REQUIRE_METAL && why) || false },
    (t) => {
      assert.equal(why, null, `required, but ${why}`);
      const cache = path.join(
        fs.realpathSync(
          execFileSync("getconf", ["DARWIN_USER_CACHE_DIR"], {
            encoding: "utf8",
          }).trim(),
        ),
        "com.apple.metal",
      );
      const profile =
        "(version 1)(allow default)" +
        `(deny file-read* file-write* (subpath ${JSON.stringify(cache)}))`;
      const output = execFileSync(
        SANDBOX_EXEC,
        ["-p", profile, process.execPath, "-e", child],
        { encoding: "utf8" },
      );
      const run = JSON.parse(output.trim().split("\n").at(-1) ?? "");

      const shared = run.services === 0 && sharedService(run.diagnosis);
      if (shared) return t.skip(shared);

      // The instrument first: one service found and stopped, and the draw
      // held for the pause. Without these the colour below proves nothing.
      assert.equal(
        run.services,
        1,
        `the child's own compiler service; it saw ${JSON.stringify(run.diagnosis)}`,
      );
      assert.ok(run.ms >= PAUSE_MS, `held ${run.ms} ms, not ${PAUSE_MS}`);
      assert.deepEqual(run.first, HALF_RED, "the draw that waited");
      assert.deepEqual(run.again, HALF_RED, "the same draw afterwards");
    },
  );

  test("finds only the compiler service started for the child", () => {
    // The services block of `launchctl print pid/<pid>` as macOS 27 printed
    // it for a sandboxed child after its first draw, whose instance is 59511.
    const domain = [
      "\tservices = {",
      "\t\t       0      - \tcom.apple.MTLCompilerService",
      "\t\t   59511      - \tcom.apple.MTLCompilerService.5725A1F5-A544-4DB6-A893-0C85E08FDD90",
      "\t}",
      "",
      "\tservice stubs = {",
      "\t\tcom.apple.StreamingUnzipService",
      "\t}",
    ].join("\n");
    /** `domain` with `line` listed after the child's instance. */
    const beside = (line) =>
      domain.replace(/^(.*59511.*)$/m, (own) => `${own}\n${line}`);
    assert.deepEqual(ownServices(domain, []), ["59511"], "its instance");
    assert.equal(servicesBlock(domain), domain.split("\n\n")[0], "the block");
    assert.equal(servicesBlock("no block"), "no block", "the whole output");

    // Two services listed, only the child's own counted.
    assert.deepEqual(
      ownServices(
        beside(
          "\t\t   59600      - \tcom.apple.StreamingUnzipService.0B7A3C21-5D2E-4F7A-9C11-2E3D4F5A6B7C",
        ),
        [],
      ),
      ["59511"],
      "another kind of service running beside it",
    );
    assert.deepEqual(
      ownServices(
        beside(
          "\t\t   37387      - \tcom.apple.MTLCompilerService.9E1D2C3B-4A5F-4E6D-8C7B-1A2B3C4D5E6F",
        ),
        ["37387"],
      ),
      ["59511"],
      "a compiler service that was running before the draw",
    );

    assert.deepEqual(
      ownServices(domain, ["59511"]),
      [],
      "an instance running before the draw, as a shared one is",
    );
    assert.deepEqual(
      ownServices(domain.replace("59511", "    0"), []),
      [],
      "an instance not running",
    );
    assert.deepEqual(
      ownServices(domain.replace(/^.*59511.*$/m, ""), ["40001"]),
      [],
      "no instance listed, whatever else is running",
    );
    assert.deepEqual(
      ownServices(
        domain.replace("MTLCompilerService.5725", "MTLCompilerServiceX.5725"),
        [],
      ),
      [],
      "another service whose name begins the same",
    );
  });

  test("skips only a virtual machine's shared compiler service", () => {
    const paravirtual = { device: `Integrated GPU (${PARAVIRTUAL_DEVICE})` };
    const real = { device: "Apple M2 Pro" };
    for (const [seen, skipped, what] of [
      [
        {
          engine: paravirtual,
          servicesBefore: ["37387"],
          servicesAfter: ["37387"],
        },
        true,
        "a shared service on the paravirtual device",
      ],
      [
        { engine: real, servicesBefore: ["37387"], servicesAfter: ["37387"] },
        false,
        "a real GPU with no service of the child's own",
      ],
      [
        {
          engine: paravirtual,
          servicesBefore: ["37387"],
          servicesAfter: ["37387", "40001"],
        },
        false,
        "the paravirtual device starting a service for the child",
      ],
      [
        { engine: paravirtual, servicesBefore: ["37387"], servicesAfter: [] },
        false,
        "the shared service gone afterwards",
      ],
      [
        {
          engine: paravirtual,
          servicesBefore: ["37387"],
          servicesAfter: ["40001"],
        },
        false,
        "the shared service replaced by another",
      ],
      [
        { engine: paravirtual, servicesBefore: [], servicesAfter: [] },
        false,
        "no service at all",
      ],
      [
        { servicesBefore: ["37387"], servicesAfter: ["37387"] },
        false,
        "no engine reported",
      ],
    ])
      assert.equal(sharedService(seen) !== null, skipped, what);
  });
});
