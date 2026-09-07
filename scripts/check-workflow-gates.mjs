// Every job in a gated workflow is covered by that workflow's aggregate.
//
// A branch ruleset requires *contexts*, and a context that never reports is
// unmet rather than absent -- so a pull request waits forever on a check from
// a job that legitimately did not run. The fix is one aggregate per workflow
// that always runs and reports what its jobs did, with only the aggregates
// required. That works exactly as long as each aggregate's `needs` names every
// other job in its file.
//
// Nothing in YAML enforces that. A job added later and not added to `needs` is
// silently outside the gate: it can fail while the aggregate reports success,
// because the aggregate never waited for it. That is the same shape as a count
// agreeing with a table both sides of which are wrong, and it fails in the
// direction that reads as healthy.
//
// So it is checked here, against the parsed workflow rather than a grep. Both
// halves are real hazards and neither implies the other: a missing dependency
// leaves a job unguarded, and an aggregate without `always()` is itself skipped
// when a dependency skips -- which is the original defect wearing the fix's
// clothes.
import { execFileSync } from "child_process";
import { readFileSync } from "fs";
import { parse } from "yaml";

// Workflow file -> the job id whose context a ruleset should require.
// A workflow absent from here is not gated and is not checked; adding one is
// a deliberate act, because requiring a context is.
const GATED = {
  "ci.yml": "ci",
  "rust-ci.yml": "rust-ci",
  "docs.yml": "docs",
};

const DIR = ".github/workflows";

// What each workflow must watch, whatever else it watches.
//
// This is the half that stops the lists failing open. While GitHub owned the
// filtering, a list that was too narrow meant the workflow did not run and the
// merge blocked -- loud, if crude. Now a narrow list means the jobs skip and
// the aggregate reports success, which is silent and wrong. So each workflow
// declares a set of files it cannot legitimately ignore, and a tracked file in
// that set but outside its list is a failure here.
//
// It cannot decide what a workflow *should* watch in general; nothing can. It
// pins the part that is not a judgement call -- a Rust build depends on Rust
// sources and on the files that pin its dependencies -- and that was enough to
// find two: `build.rs` compiles into every build and `Cargo.lock` pins every
// version, and neither was in the list this replaces.
const MUST_COVER = {
  "rust-ci.txt": {
    reason: "they compile into the Rust build or pin what it builds against",
    specs: [":(glob)**/*.rs", "Cargo.toml", "Cargo.lock", "rustfmt.toml"],
  },
  "docs.txt": {
    reason: "the published reference is generated from them",
    specs: [":(glob)lib/**/*.d.ts", ":(glob)scripts/typedoc/**"],
  },
  "ci.txt": {
    reason: "the packaging and install path is exercised through them",
    specs: [":(glob)lib/**", ":(glob)tests/**", "package.json", "bun.lock"],
  },
};

/** Tracked files matching `specs`, as a sorted array. */
export function trackedFiles(specs) {
  if (specs.length === 0) return [];
  const out = execFileSync("git", ["ls-files", "--", ...specs], {
    encoding: "utf8",
  });
  return out.split("\n").filter(Boolean).sort();
}

/** The pathspecs in a list file, comments and blank lines dropped. */
export function readSpecs(text) {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l !== "" && !l.startsWith("#"));
}

/** Files a workflow must watch and does not. */
export function auditCoverage(list, mustSpecs, watchSpecs, ls = trackedFiles) {
  const watched = new Set(ls(watchSpecs));
  const uncovered = ls(mustSpecs).filter((f) => !watched.has(f));
  if (uncovered.length === 0) return [];
  return [
    `${list} does not watch ${uncovered.map((f) => `\`${f}\``).join(", ")}, ` +
      `and a change to one of those would skip this workflow's jobs while its ` +
      `aggregate reported success`,
  ];
}

/** Problems with one workflow's aggregate, as a list of sentences. */
export function auditWorkflow(file, aggregateId, text) {
  const problems = [];
  const doc = parse(text);
  const jobs = doc?.jobs;
  if (jobs === undefined || jobs === null) {
    return [`${file} declares no jobs, so its aggregate cannot cover anything`];
  }
  const ids = Object.keys(jobs);
  const aggregate = jobs[aggregateId];
  if (aggregate === undefined) {
    return [
      `${file} has no job \`${aggregateId}\`, which is the context a ruleset ` +
        `requires for this workflow`,
    ];
  }

  // `always()` is what makes the aggregate report when a dependency skipped.
  // Without it GitHub skips the aggregate too, and the required context goes
  // missing exactly when it is needed.
  const condition = String(aggregate.if ?? "");
  if (!condition.includes("always()")) {
    problems.push(
      `${file}: \`${aggregateId}\` has \`if: ${condition || "(none)"}\`, which ` +
        `does not call always() -- it will be skipped along with its ` +
        `dependencies and report nothing`,
    );
  }

  const needs = new Set(
    Array.isArray(aggregate.needs)
      ? aggregate.needs
      : aggregate.needs === undefined
        ? []
        : [aggregate.needs],
  );
  const uncovered = ids.filter((id) => id !== aggregateId && !needs.has(id));
  if (uncovered.length > 0) {
    problems.push(
      `${file}: \`${aggregateId}\` does not depend on ${uncovered
        .map((id) => `\`${id}\``)
        .join(
          ", ",
        )} -- ${uncovered.length === 1 ? "that job" : "those jobs"} ` +
        `can fail while the required context reports success`,
    );
  }

  // A name in `needs` that is not a job fails the workflow at parse time on
  // GitHub's side, but it reads here as coverage that does not exist.
  const phantom = [...needs].filter((id) => !ids.includes(id));
  if (phantom.length > 0) {
    problems.push(
      `${file}: \`${aggregateId}\` depends on ${phantom
        .map((id) => `\`${id}\``)
        .join(
          ", ",
        )}, which ${phantom.length === 1 ? "is not a job" : "are not jobs"} ` +
        `in this workflow`,
    );
  }
  return problems;
}

function selfTest() {
  let bad = 0;
  const check = (label, file, id, text, expected) => {
    const got = auditWorkflow(file, id, text).length;
    if (got !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got}`,
      );
      bad += 1;
    }
  };
  const gate = (needs, cond = "always()") =>
    `jobs:\n  a:\n    runs-on: x\n  b:\n    runs-on: x\n  g:\n    if: ${cond}\n    needs: [${needs}]\n    runs-on: x\n`;

  check("a complete aggregate passes", "w.yml", "g", gate("a, b"), 0);
  check("a job missing from needs is named", "w.yml", "g", gate("a"), 1);
  check("two missing jobs are one problem", "w.yml", "g", gate(""), 1);
  check(
    "a needs entry that is not a job is named",
    "w.yml",
    "g",
    gate("a, b, ghost"),
    1,
  );
  check(
    "an aggregate without always() is named",
    "w.yml",
    "g",
    gate("a, b", "success()"),
    1,
  );
  check(
    "no condition at all is named",
    "w.yml",
    "g",
    "jobs:\n  a:\n    runs-on: x\n  g:\n    needs: [a]\n    runs-on: x\n",
    1,
  );
  check(
    "both faults at once are reported separately",
    "w.yml",
    "g",
    gate("a", "success()"),
    2,
  );
  check(
    "a missing aggregate is named",
    "w.yml",
    "g",
    "jobs:\n  a:\n    runs-on: x\n",
    1,
  );
  check("a workflow with no jobs is named", "w.yml", "g", "on: push\n", 1);
  check(
    "a single needs written as a scalar is understood",
    "w.yml",
    "g",
    "jobs:\n  a:\n    runs-on: x\n  g:\n    if: always()\n    needs: a\n    runs-on: x\n",
    0,
  );

  // The coverage half. `ls` is injected rather than defaulted to the real
  // repository: a case that consults the tree passes or fails for reasons it
  // does not state, which is how a self-test stops testing anything.
  const ls = (files) => (specs) =>
    specs.includes("MUST") ? files.must : files.watched;
  const cover = (label, must, watched, expected) => {
    const got = auditCoverage(
      "l.txt",
      ["MUST"],
      ["WATCH"],
      ls({ must, watched }),
    ).length;
    if (got !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got}`,
      );
      bad += 1;
    }
  };
  cover("a fully covered list passes", ["a.rs"], ["a.rs", "b.rs"], 0);
  cover("an unwatched required file is named", ["a.rs", "b.rs"], ["a.rs"], 1);
  cover("several are one problem", ["a.rs", "b.rs"], [], 1);
  cover("nothing required is vacuously covered", [], ["a.rs"], 0);
  cover("watching more than required is fine", ["a.rs"], ["a.rs", "z.txt"], 0);

  const n = 15;
  if (bad === 0) {
    console.log(
      `self-test: ${n} cases, an uncovered job, a phantom dependency, a ` +
        `missing always() and a required path outside a list are each caught, ` +
        `and a complete workflow passes`,
    );
  }
  return bad;
}

if (process.argv.includes("--self-test")) {
  process.exit(selfTest() === 0 ? 0 : 1);
}

const problems = [
  ...Object.entries(GATED).flatMap(([file, id]) =>
    auditWorkflow(file, id, readFileSync(`${DIR}/${file}`, "utf8")),
  ),
  ...Object.entries(MUST_COVER).flatMap(([list, { specs }]) =>
    auditCoverage(
      list,
      specs,
      readSpecs(readFileSync(`${DIR}/paths/${list}`, "utf8")),
    ),
  ),
];

if (problems.length > 0) {
  console.error("workflow gating is incomplete:\n");
  for (const p of problems) console.error(`  ${p}`);
  console.error(
    "\nAn aggregate is the only context a ruleset requires for its workflow, " +
      "so a job outside it is ungated and a path outside its list skips it " +
      "silently.",
  );
  process.exit(1);
}
console.log(
  `workflow aggregates cover every job, and every path list covers what it ` +
    `must (${Object.keys(GATED).join(", ")})`,
);
