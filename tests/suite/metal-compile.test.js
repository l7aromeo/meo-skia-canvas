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
// as the one that appeared between before and after its first GPU draw. What it
// saw is reported too, so a run where that precondition fails -- a runner
// whose Metal device compiles somewhere else -- says what it found instead.
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
  const mine = [...services()].filter((pid) => !before.has(pid));
  const diagnosis = { pid: process.pid, engine: warm.engine, warmMs,
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
