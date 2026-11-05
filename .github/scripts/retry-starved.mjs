// Re-runs, once, a workflow run's jobs that were cancelled because no runner
// ever took them.
//
// GitHub cancels a job it cannot place on a hosted runner after 15 to 30
// minutes in the queue, with the annotation "The job was not acquired by
// Runner of type hosted even after multiple attempts" -- macOS arm64 runners
// most often. Such a job concludes `cancelled` with an empty `runner_name` and
// no steps, and the workflow's aggregate then fails on it, so a pull request
// goes red for a reason nothing in it caused.
//
// Every job still queued in a superseded run has the same shape, so the run's
// own conclusion tells them apart. A run cancelled as a whole -- by
// `concurrency` when a newer push arrives, or by hand -- concludes `cancelled`.
// A run in which only starved jobs were cancelled concludes `failure`, because
// its aggregate failed on them.
//
// The re-run is `rerun-failed-jobs`, which re-runs every failed or cancelled
// job and everything that depends on one, so it is asked for only when nothing
// else failed. A job that failed on a runner is a result: re-running it would
// either hide a flaky failure or repeat a real one, and a fix pushed for it
// runs everything again anyway. The aggregate is the exception. It fails
// because of the starved job and depends on it, so the re-run includes it
// whatever it reported.
//
// Usage: node .github/scripts/retry-starved.mjs <run id> [--dry-run]
// Reads the run and its first attempt's jobs through `gh api`, in the
// repository `GITHUB_REPOSITORY` names, prints the decision, and asks for the
// re-run unless `--dry-run` is given.
import { execFileSync } from "node:child_process";
import { basename } from "node:path";
import { pathToFileURL } from "node:url";

import { GATED } from "../../scripts/workflow-aggregates.mjs";

/** Job conclusions that are not a failure of the job. */
const PASSED = new Set(["success", "skipped", "neutral"]);

/** The attempt a run starts as; a re-run is attempt 2 or later. */
const FIRST_ATTEMPT = 1;

/** Whether a job was cancelled before any runner took it. */
export function isStarved(job) {
  return job.conclusion === "cancelled" && !job.runner_name;
}

/**
 * What to do about one completed run.
 *
 * `run` is the REST workflow-run object -- `conclusion`, `run_attempt` and
 * `path` are read -- and `jobs` are its first attempt's jobs. Returns
 * `{ rerun: true, starved }` naming the starved jobs, or `{ rerun: false,
 * reason }`.
 */
export function decide(run, jobs) {
  if (run.run_attempt !== FIRST_ATTEMPT) {
    return {
      rerun: false,
      reason: `this is attempt ${run.run_attempt}: retried once already, or re-run by hand`,
    };
  }
  if (run.conclusion === "cancelled") {
    return {
      rerun: false,
      reason:
        "the run was cancelled as a whole: superseded by a newer run, or stopped by hand",
    };
  }
  const starved = jobs.filter(isStarved);
  if (starved.length === 0) {
    return {
      rerun: false,
      reason: "no job was cancelled before a runner took it",
    };
  }
  const aggregate = GATED[basename(run.path ?? "")];
  const failed = jobs.filter(
    (job) =>
      !isStarved(job) && job.name !== aggregate && !PASSED.has(job.conclusion),
  );
  if (failed.length > 0) {
    const names = failed.map((job) => `${job.name} (${job.conclusion})`);
    return {
      rerun: false,
      reason: `a job that ran did not pass, so the run is not retried: ${names.join(", ")}`,
    };
  }
  return { rerun: true, starved: starved.map((job) => job.name) };
}

function gh(...args) {
  return execFileSync("gh", ["api", ...args], { encoding: "utf8" });
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [id] = process.argv.slice(2).filter((a) => !a.startsWith("--"));
  const dryRun = process.argv.includes("--dry-run");
  const repo = process.env.GITHUB_REPOSITORY;
  if (!/^\d+$/.test(id ?? "") || !repo) {
    console.error(
      "usage: GITHUB_REPOSITORY=<owner/repo> node .github/scripts/retry-starved.mjs <run id> [--dry-run]",
    );
    process.exit(2);
  }
  const base = `repos/${repo}/actions/runs/${id}`;
  const run = JSON.parse(gh(base));
  // One JSON array per page under `--paginate`, so `--slurp` joins them.
  const jobs = JSON.parse(
    gh(
      "--paginate",
      "--slurp",
      `${base}/attempts/${FIRST_ATTEMPT}/jobs?per_page=100`,
    ),
  ).flatMap((page) => page.jobs);
  const verdict = decide(run, jobs);
  console.log(
    `${run.name} run ${id}, attempt ${run.run_attempt}, ${run.conclusion}`,
  );
  if (!verdict.rerun) {
    console.log(`not retried: ${verdict.reason}`);
  } else {
    console.log(
      `starved: ${verdict.starved.join(", ")}\n` +
        (dryRun ? "would re-run them (dry run)" : "re-running them"),
    );
    if (!dryRun) gh("--method", "POST", `${base}/rerun-failed-jobs`);
  }
}
