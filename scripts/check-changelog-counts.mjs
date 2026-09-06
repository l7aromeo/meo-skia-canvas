//
// Fails when changelog prose states a number of entries that the entries
// themselves contradict.
//
// The unreleased sections open with a paragraph that counts what follows --
// "Seven entries below break", and in the npm file a cross-reference reading
// "five of that file's seven breaking entries". Both are claims about a list
// that grows underneath them, and both went stale three times in one day:
// "three of the entries below break the crate's public API" survived a split
// that moved those entries into another file; "the crate breaks in three
// places of its own" was written to replace it and inherited the same wrong
// number; and "Six entries below break" was correct until a Breaking entry
// was appended half an hour later.
//
// Nothing else catches it. Prettier is satisfied, the prose reads as
// deliberate, and a reviewer counting seven bullet points has no reason to
// scroll back to a paragraph they already read. The number is only checkable
// against something a hundred lines away, which is exactly the distance at
// which people stop checking.
//
// WHY A COUNT AND NOT THE WHOLE CLAIM. This checks arithmetic, not truth. It
// cannot tell whether an entry belongs under Breaking, whether a change
// really reaches both surfaces, or whether the sentence describes the right
// file -- all three have been wrong here too, and all three need reading.
// What it does remove is the failure that recurs without anyone being
// careless: a number that was right when written.
//
// Only the UNRELEASED block is checked. Released sections are history and
// their counts describe what shipped.
//
import { readFileSync } from "node:fs";

const WORDS = {
  one: 1,
  two: 2,
  three: 3,
  four: 4,
  five: 5,
  six: 6,
  seven: 7,
  eight: 8,
  nine: 9,
  ten: 10,
  eleven: 11,
  twelve: 12,
  thirteen: 13,
  fourteen: 14,
  fifteen: 15,
  sixteen: 16,
  seventeen: 17,
  eighteen: 18,
  nineteen: 19,
  twenty: 20,
};

const numeral = (token) => {
  const word = WORDS[token.toLowerCase()];
  return word ?? (/^\d+$/.test(token) ? Number(token) : null);
};

/** The UNRELEASED block of a changelog, or "" when it has none. */
function unreleased(text) {
  const lines = text.split("\n");
  const start = lines.findIndex((l) => /^## .*UNRELEASED/.test(l));
  if (start === -1) return "";
  const rest = lines.slice(start + 1).findIndex((l) => /^## /.test(l));
  return lines
    .slice(start, rest === -1 ? undefined : start + 1 + rest)
    .join("\n");
}

/** Entries -- lines opening `- **` -- under a `### <name>` heading. */
function entriesUnder(block, section) {
  let inside = false;
  let count = 0;
  for (const line of block.split("\n")) {
    if (/^### /.test(line)) inside = line.slice(4).trim() === section;
    else if (inside && /^- \*\*/.test(line)) count += 1;
  }
  return count;
}

// The prose is free-form, so each pattern is anchored on wording specific
// enough that a match is certainly a claim about a count. A sentence this
// does not recognise is not checked -- the gate is here to catch drift in
// the claims that exist, not to constrain how they may be phrased.
const CLAIMS = [
  {
    // "Seven entries below break" -- about this file's own Breaking list.
    pattern: /(\w+) entries below break/gi,
    subject: "self",
    describe: (n) => `"${n} entries below break"`,
  },
  {
    // "five of that file's seven breaking entries" -- the second number is
    // the *other* file's Breaking list, which is the half that drifts.
    pattern: /(\w+) of that file's (\w+) breaking entries/gi,
    subject: "other",
    describe: (_a, b) => `"of that file's ${b} breaking entries"`,
  },
];

function check(files) {
  const problems = [];
  const blocks = new Map();
  for (const [name, text] of files) blocks.set(name, unreleased(text));

  for (const [name, block] of blocks) {
    // Prose wraps at ~78 columns, so a claim is regularly split across two
    // lines. Fold before matching or every multi-line claim reads as absent.
    const folded = block.replace(/\s+/g, " ");
    const other = [...blocks.keys()].find((k) => k !== name);

    for (const claim of CLAIMS) {
      for (const found of folded.matchAll(claim.pattern)) {
        const token = claim.subject === "self" ? found[1] : found[2];
        const stated = numeral(token);
        if (stated === null) continue;
        const target = claim.subject === "self" ? name : other;
        if (!target) continue;
        const actual = entriesUnder(blocks.get(target), "Breaking");
        if (stated !== actual) {
          problems.push(
            `${name}: ${claim.describe(found[1], found[2])} but ` +
              `${target} has ${actual} entries under Breaking`,
          );
        }
      }
    }
  }
  return problems;
}

// A self-test, because a checker that has never been shown to fire says
// nothing when it is quiet. Both directions: a tree it must pass and a tree
// it must fail, differing only in the number.
function selfTest() {
  const withCount = (n) => `## [UNRELEASED]

**Not decided.** ${n} entries below break, so this is not a patch.

### Breaking

- **One.** Body.
- **Two.** Body.

### Fixed

- **Not counted.** Under a different heading.
`;
  // The claim under test is the *second* number -- the other file's Breaking
  // count -- since that is the one that drifts when the other file grows.
  const referring = (n) => `## [UNRELEASED]

Two are ours; two of that file's ${n} breaking entries are shared.

### Breaking

- **Only one here.** Body.
`;
  const cases = [
    ["a correct count passes", [["a.md", withCount("Two")]], 0],
    ["a stale count fails", [["a.md", withCount("Six")]], 1],
    ["a digit works too", [["a.md", withCount("2")]], 0],
    [
      "a claim split across lines is still read",
      [["a.md", withCount("Two").replace("Two entries", "Two\nentries")]],
      0,
    ],
    [
      "a cross-reference to the other file is checked",
      [
        ["a.md", referring("Two")],
        ["b.md", withCount("Two")],
      ],
      0,
    ],
    [
      "a stale cross-reference fails",
      [
        ["a.md", referring("Nine")],
        ["b.md", withCount("Two")],
      ],
      1,
    ],
    [
      "a file with no UNRELEASED block is skipped",
      [["a.md", "## [1.0.0]\n\n### Breaking\n\n- **x.** y.\n"]],
      0,
    ],
  ];

  let bad = 0;
  for (const [label, files, expected] of cases) {
    const got = check(files).length;
    if (got !== expected) {
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got}`,
      );
      bad += 1;
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `self-test: ${cases.length} cases, a stale count is caught in both the ` +
      `direct and the cross-referencing form`,
  );
}

const args = process.argv.slice(2);
if (args.includes("--self-test")) {
  selfTest();
} else {
  const paths =
    args.length > 0 ? args : ["CHANGELOG-crate.md", "CHANGELOG-npm.md"];
  const problems = check(paths.map((p) => [p, readFileSync(p, "utf8")]));
  if (problems.length > 0) {
    console.error("changelog prose disagrees with the entries it counts:\n");
    for (const p of problems) console.error(`  ${p}`);
    process.exit(1);
  }
  console.log(`changelog counts agree with the entries (${paths.join(", ")})`);
}
