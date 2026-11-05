const assert = require("assert");
const { readFileSync } = require("fs");
const { join } = require("path");
const { describe, test } = require("node:test");

// Which jobs `.github/workflows/retry-starved.yml` re-runs, decided on runs
// recorded from this repository's own CI through the REST API: the run and its
// first attempt's jobs, cut to the fields the decision or a reader needs.
//
// - 41906565134: Rust CI, the macOS clippy leg cancelled after 15 minutes
//   queued, annotated "The job was not acquired by Runner of type hosted even
//   after multiple attempts". Every other job passed; `rust-ci` failed on it.
// - 42039676530: Rust CI, the Graphite bindings job starved the same way, while
//   `test` and `binding` failed on their runners.
// - 42093953518: Rust CI, superseded by a newer push: queued jobs cancelled
//   with no runner, running ones cancelled on theirs.
// - 42093953670: CI, superseded while only its macOS leg was still queued.
// - 42101318652: Rust CI on main, every job green.
const recorded = (id) =>
  JSON.parse(
    readFileSync(
      join(__dirname, "..", "assets", "workflow-runs", `${id}.json`),
      "utf8",
    ),
  );
const load = () => import("../../.github/scripts/retry-starved.mjs");

/** A recorded run with `edit` applied to a copy of it. */
const variant = (id, edit) => {
  const copy = structuredClone(recorded(id));
  edit(copy);
  return copy;
};
const job = (run, name) => run.jobs.find((j) => j.name === name);

describe("re-running jobs no runner took", () => {
  test("the recordings hold what the cases below say they hold", () => {
    const starved = recorded(41906565134);
    assert.equal(starved.run.conclusion, "failure");
    const clippy = job(starved, "clippy macos (metal,vulkan,window)");
    assert.equal(clippy.conclusion, "cancelled");
    assert.equal(clippy.runner_name, "");
    assert.equal(clippy.runner_id, 0);
    // The aggregate failed on the starved job, on a runner of its own, which
    // is what the exception for it exists for.
    const aggregate = job(starved, "rust-ci");
    assert.equal(aggregate.conclusion, "failure");
    assert.notEqual(aggregate.runner_name, "");
    const others = starved.jobs.filter((j) => j !== clippy && j !== aggregate);
    assert.ok(
      others.every((j) => ["success", "skipped"].includes(j.conclusion)),
    );

    const superseded = recorded(42093953670);
    assert.equal(superseded.run.conclusion, "cancelled");
    assert.deepEqual(
      superseded.jobs
        .filter((j) => j.conclusion !== "success")
        .map((j) => [j.name, j.conclusion, j.runner_name === ""]),
      [
        ["Test (macos, Node 24)", "cancelled", true],
        ["ci", "failure", false],
      ],
    );
  });

  test("re-runs a run whose only failure is a starved job", async () => {
    const { decide } = await load();
    const { run, jobs } = recorded(41906565134);
    assert.deepEqual(decide(run, jobs), {
      rerun: true,
      starved: ["clippy macos (metal,vulkan,window)"],
    });
  });

  test("leaves a run alone where a job failed on a runner", async () => {
    const { decide } = await load();
    const { run, jobs } = recorded(42039676530);
    const verdict = decide(run, jobs);
    assert.equal(verdict.rerun, false);
    assert.match(verdict.reason, /test \(freetype,fetch\) \(failure\)/);
    assert.match(verdict.reason, /binding \(Rust \+ JS on a fresh build\)/);
  });

  test("leaves a run alone where a job was cancelled on its runner", async () => {
    // A job stopped at its `timeout-minutes` concludes `cancelled` too, with
    // the runner it ran on named.
    const { decide } = await load();
    const { run, jobs } = variant(41906565134, (r) => {
      const fmt = job(r, "fmt (nightly)");
      fmt.conclusion = "cancelled";
    });
    const verdict = decide(run, jobs);
    assert.equal(verdict.rerun, false);
    assert.match(verdict.reason, /fmt \(nightly\) \(cancelled\)/);
  });

  test("leaves a superseded run alone", async () => {
    const { decide } = await load();
    for (const id of [42093953518, 42093953670]) {
      const { run, jobs } = recorded(id);
      const verdict = decide(run, jobs);
      assert.equal(verdict.rerun, false, id);
      assert.match(verdict.reason, /cancelled as a whole/, id);
    }
  });

  test("leaves a run alone after its first attempt", async () => {
    const { decide } = await load();
    const { run, jobs } = variant(41906565134, (r) => {
      r.run.run_attempt = 2;
    });
    const verdict = decide(run, jobs);
    assert.equal(verdict.rerun, false);
    assert.match(verdict.reason, /attempt 2/);
  });

  test("leaves a green run alone", async () => {
    const { decide } = await load();
    const { run, jobs } = recorded(42101318652);
    assert.deepEqual(decide(run, jobs), {
      rerun: false,
      reason: "no job was cancelled before a runner took it",
    });
  });

  test("excuses only the aggregate of the run's own workflow", async () => {
    // The same jobs reported as ci.yml's: `rust-ci` is not that workflow's
    // aggregate, so its failure is a failure like any other.
    const { decide } = await load();
    const { run, jobs } = variant(41906565134, (r) => {
      r.run.path = ".github/workflows/ci.yml";
    });
    const verdict = decide(run, jobs);
    assert.equal(verdict.rerun, false);
    assert.match(verdict.reason, /rust-ci \(failure\)/);
  });

  test("counts a job with no conclusion as one that did not pass", async () => {
    const { decide } = await load();
    const { run, jobs } = variant(41906565134, (r) => {
      job(r, "fmt (nightly)").conclusion = null;
    });
    assert.equal(decide(run, jobs).rerun, false);
  });
});
