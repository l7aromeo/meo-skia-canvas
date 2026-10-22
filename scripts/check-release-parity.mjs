#!/usr/bin/env node
//
// Both release recipes refuse to run while a parity gap has no issue filed:
// each calls `just check-parity --require-filed` before it bumps a version.
// The justfile says so in a comment, and a comment is a claim nothing checks
// -- an edit that drops the step, comments it out, loses the flag or moves it
// below the bump would leave every other gate green and the next release free
// to ship an unrecorded gap. This reads the recipes and holds the claim.
//
// `--self-test` feeds it mutated copies of the real justfile, so the cases
// are the recipes as they stand rather than a fixture that could drift from
// them, and one copy left unmutated, which has to pass.

import { readFileSync } from "node:fs";

/** The release recipes, each with the line that bumps its version. */
const RECIPES = [
  { name: "release-npm", bump: /^\s*npm version\b/ },
  { name: "release-crate", bump: /^\s*cargo set-version\b/ },
];

/** The step that has to come first. */
const STEP = /^\s*just check-parity\b.*\s--require-filed\b/;

/** The body of recipe `name`: its indented lines, up to the next recipe. */
function body(justfile, name) {
  const lines = justfile.split("\n");
  const start = lines.findIndex(
    (l) => l.startsWith(`${name} `) || l.startsWith(`${name}:`),
  );
  if (start === -1) return null;
  const out = [];
  for (const line of lines.slice(start + 1)) {
    if (line !== "" && !/^\s/.test(line)) break;
    out.push(line);
  }
  return out;
}

/** Every way `justfile` breaks the rule, as sentences; empty when it holds. */
export function check(justfile) {
  const problems = [];
  for (const { name, bump } of RECIPES) {
    const lines = body(justfile, name);
    if (lines === null) {
      problems.push(`${name}: no such recipe, so the guard cannot see it`);
      continue;
    }
    const step = lines.findIndex((l) => STEP.test(l));
    const bumped = lines.findIndex((l) => bump.test(l));
    if (bumped === -1) {
      problems.push(
        `${name}: no line matching ${bump}, so where it bumps is unknown`,
      );
    }
    if (step === -1) {
      problems.push(
        `${name}: never runs \`just check-parity --require-filed\``,
      );
    } else if (bumped !== -1 && step > bumped) {
      problems.push(
        `${name}: runs \`check-parity --require-filed\` after it bumps the version`,
      );
    }
  }
  return problems;
}

function selfTest(real) {
  const mutate = (name, edit) => {
    const lines = real.split("\n");
    const start = lines.findIndex((l) => l.startsWith(`${name} `));
    const at = lines.findIndex((l, i) => i > start && STEP.test(l));
    return edit(lines, at).join("\n");
  };
  const cases = [
    ["the justfile as it stands", real, false],
    ...RECIPES.flatMap(({ name, bump }) => [
      [
        `${name} with the step deleted`,
        mutate(name, (l, at) => l.filter((_, i) => i !== at)),
        true,
      ],
      [
        `${name} with the step commented out`,
        mutate(name, (l, at) =>
          l.map((x, i) => (i === at ? x.replace("just", "# just") : x)),
        ),
        true,
      ],
      [
        `${name} without the flag`,
        mutate(name, (l, at) =>
          l.map((x, i) => (i === at ? x.replace(" --require-filed", "") : x)),
        ),
        true,
      ],
      [
        `${name} with the step moved below the bump`,
        mutate(name, (l, at) => {
          const step = l[at];
          const rest = l.filter((_, i) => i !== at);
          const b = rest.findIndex((x, i) => i > at && bump.test(x));
          return [...rest.slice(0, b + 1), step, ...rest.slice(b + 1)];
        }),
        true,
      ],
    ]),
  ];
  let bad = 0;
  for (const [label, text, refused] of cases) {
    if (check(text).length > 0 !== refused) {
      console.error(
        `  self-test FAILED: ${label} was ${refused ? "accepted" : "refused"}`,
      );
      bad += 1;
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `release parity: self-test, ${cases.length} cases, every mutation refused`,
  );
}

const justfile = readFileSync("justfile", "utf8");
if (process.argv.includes("--self-test")) {
  selfTest(justfile);
} else {
  const problems = check(justfile);
  if (problems.length > 0) {
    console.error(
      "release parity: a release recipe can run with a gap unfiled",
    );
    for (const p of problems) console.error(`  ${p}`);
    process.exit(1);
  }
  console.log(
    `release parity: ${RECIPES.map((r) => r.name).join(" and ")} refuse an unfiled gap before bumping`,
  );
}
