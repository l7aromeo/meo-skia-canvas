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

const why =
  process.platform !== "darwin"
    ? "skipped: Metal is macOS only"
    : !fs.existsSync(SANDBOX_EXEC)
      ? "skipped: no sandbox-exec to hide the Metal shader cache with"
      : !new Canvas(4, 4).gpu
        ? "skipped: no GPU engine"
        : null;

// The child's whole run, as JSON on its last line. It finds its compiler service
// as the one that appeared between before and after its first GPU draw.
const child = `
  const { execFileSync, spawn } = require("child_process");
  const { Canvas, ImageData } = require(${JSON.stringify(path.join(__dirname, "../../lib"))});
  const services = () => new Set(
    execFileSync("pgrep", ["-f", "MTLCompilerService.xpc"], { encoding: "utf8" })
      .split("\\n").filter(Boolean));
  const pixel = (canvas) => [...canvas.getContext("2d").getImageData(1, 1, 1, 1).data];
  const before = services();
  const warm = new Canvas(4, 4);
  warm.getContext("2d").fillRect(0, 0, 4, 4);
  pixel(warm);
  const mine = [...services()].filter((pid) => !before.has(pid));
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
  console.log(JSON.stringify({ services: mine.length, first, ms, again: draw() }));
`;

describe("A Metal compile that outlasts a second", () => {
  test(
    "still draws",
    { skip: (!process.env.MEO_SKIA_CANVAS_REQUIRE_METAL && why) || false },
    () => {
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

      // The instrument first: one service found and stopped, and the draw
      // held for the pause. Without these the colour below proves nothing.
      assert.equal(run.services, 1, "the child's own compiler service");
      assert.ok(run.ms >= PAUSE_MS, `held ${run.ms} ms, not ${PAUSE_MS}`);
      assert.deepEqual(run.first, HALF_RED, "the draw that waited");
      assert.deepEqual(run.again, HALF_RED, "the same draw afterwards");
    },
  );
});
