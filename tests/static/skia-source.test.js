const assert = require("assert");
const { spawnSync } = require("child_process");
const { join } = require("path");
const { pathToFileURL } = require("url");
const { describe, test, mock, beforeEach, afterEach } = require("node:test");

const SCRIPT = join(__dirname, "..", "..", "scripts", "skia-source.mjs");

// What a cold cache meets: googlesource answers a burst of fetches with 429s
// that clear within a minute, so each network step of `prepare` is tried
// again after a wait, and says which try failed and why.
describe("skia-source retries a fetch that fails", () => {
  let retry;
  let logged;
  beforeEach(async () => {
    // A URL, not a path: the ESM loader reads Windows' `C:` as a URL scheme.
    ({ retry } = await import(pathToFileURL(SCRIPT).href));
    logged = [];
    mock.method(console, "error", (line) => logged.push(line));
  });
  afterEach(() => mock.restoreAll());

  /** A step that throws `failures` times, then answers "done". */
  const flaky = (failures) => {
    let calls = 0;
    return {
      step: () => {
        calls += 1;
        if (calls <= failures) throw new Error(`HTTP 429 on call ${calls}`);
        return "done";
      },
      calls: () => calls,
    };
  };

  test("a step that succeeds is run once and waits for nothing", () => {
    const waits = [];
    const { step, calls } = flaky(0);
    assert.equal(
      retry("syncing", step, { wait: (s) => waits.push(s) }),
      "done",
    );
    assert.equal(calls(), 1);
    assert.deepEqual(waits, []);
    assert.deepEqual(logged, []);
  });

  test("a step that fails twice waits, says why, and succeeds", () => {
    const waits = [];
    const { step, calls } = flaky(2);
    assert.equal(
      retry("syncing", step, { wait: (s) => waits.push(s) }),
      "done",
    );
    assert.equal(calls(), 3);
    assert.deepEqual(waits, [15, 30]);
    assert.deepEqual(logged, [
      "skia-source: syncing failed on try 1 of 4: HTTP 429 on call 1; trying again in 15 s",
      "skia-source: syncing failed on try 2 of 4: HTTP 429 on call 2; trying again in 30 s",
    ]);
  });

  test("a step that keeps failing stops with its own last error", () => {
    const waits = [];
    const { step, calls } = flaky(Infinity);
    assert.throws(
      () => retry("syncing", step, { wait: (s) => waits.push(s) }),
      /HTTP 429 on call 4/,
    );
    assert.equal(calls(), 4);
    assert.deepEqual(waits, [15, 30, 60]);
    assert.equal(
      logged.at(-1),
      "skia-source: syncing failed on try 4 of 4, the last: HTTP 429 on call 4",
    );
  });
});

// skia-bindings reads a `SKIA_SOURCE_DIR` that is set at all as an offline
// build from it, so an empty one -- what a failed `prepare` once wrote into a
// CI job's environment -- built from no directory and failed reporting gn
// missing. The script names it instead, whatever it was asked to do.
describe("skia-source refuses an empty SKIA_SOURCE_DIR", () => {
  for (const command of ["env", "dir", "prepare"]) {
    test(command, () => {
      const run = spawnSync(process.execPath, [SCRIPT, command], {
        env: { ...process.env, SKIA_SOURCE_DIR: "" },
        encoding: "utf8",
      });
      assert.equal(run.status, 1, run.stderr);
      assert.equal(run.stdout, "");
      assert.match(run.stderr, /SKIA_SOURCE_DIR is set but empty/);
    });
  }

  test("and leaves one that is unset alone", () => {
    const env = { ...process.env };
    delete env.SKIA_SOURCE_DIR;
    const run = spawnSync(process.execPath, [SCRIPT, "dir"], {
      env,
      encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
    assert.match(run.stdout, /skia-bindings[/\\]skia\n$/);
  });
});
