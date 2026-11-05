// The workflows a branch ruleset gates, each with its aggregate: the one job
// whose context the ruleset requires, which always runs and reports what the
// workflow's other jobs did.
//
// A context is a job's `name`, and `check-workflow-gates.mjs` holds each
// aggregate's `name` to its id, so the value here is both. A workflow absent
// from here is not gated; adding one is a deliberate act, because requiring a
// context is.
//
// A module of its own, with no dependencies, so that
// `.github/scripts/retry-starved.mjs` can read it in a job that installs
// nothing.
export const GATED = {
  "ci.yml": "ci",
  "rust-ci.yml": "rust-ci",
  "docs.yml": "docs",
};
