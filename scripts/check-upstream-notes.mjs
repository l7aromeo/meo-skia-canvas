#!/usr/bin/env node
// Every workaround for a dependency's defect is a bet that the defect is still there. This
// refuses a bet nobody can re-check: the marker has to name the dependency and the version it
// was seen in, say whether an issue exists, say whether this tree works around it, and give
// something to run.
//
// The convention is in AGENTS.md under "Upstream defects and the workarounds for them". The
// grammar checked here:
//
//   UPSTREAM: <dependency> <version> -- <issue | unfiled> -- <worked around | not worked around>
//   Re-check: <a command, a test name, or the assertion that would move>
//
// A marker with no version is the failure this exists to stop, because "Skia is broken" cannot
// be re-tested against anything.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const MARKER = "UPSTREAM:",
  // `0.153.3`, `M150`, `0.30`, or a commit: enough to identify what was looked at.
  VERSION = /^(v?\d[\w.+-]*|[Mm]\d+|master@[0-9a-f]{7,40})$/,
  ISSUE = /^(unfiled|(?:[\w.-]+\/[\w.-]+)?#\d+)$/,
  DISPOSITION = ["worked around", "not worked around"];

// The line carrying the marker, and the `Re-check:` that must follow it, with comment syntax
// stripped. Handles `//`, `///`, `//!`, `#` and `*`.
const uncomment = (line) =>
  line.replace(/^\s*(?:\/{2,3}!?|#+|\*)\s?/, "").trimEnd();

export function findings(text, path) {
  const lines = text.split("\n"),
    out = [];

  for (const [i, raw] of lines.entries()) {
    const body = uncomment(raw);
    // The marker has to *open* the comment. Prose that mentions it -- the
    // convention's own description in AGENTS.md says `git grep UPSTREAM:` --
    // is talking about the marker rather than being one.
    if (!body.startsWith(MARKER)) continue;

    const where = `${path}:${i + 1}`,
      rest = body.slice(MARKER.length).trim(),
      fields = rest.split(/\s+--\s+/).map((f) => f.trim());

    if (fields.length !== 3) {
      out.push(`${where}: expected three ' -- ' fields, got ${fields.length}`);
      continue;
    }

    const [origin, issue, disposition] = fields,
      words = origin.split(/\s+/);

    if (words.length < 2 || !VERSION.test(words.at(-1)))
      out.push(
        `${where}: "${origin}" does not end in a version -- name the release the defect was seen in`,
      );
    if (!ISSUE.test(issue))
      out.push(
        `${where}: "${issue}" is not an issue reference or the word "unfiled"`,
      );
    if (!DISPOSITION.includes(disposition))
      out.push(
        `${where}: "${disposition}" is not one of: ${DISPOSITION.join(", ")}`,
      );

    const next = uncomment(lines[i + 1] ?? "");
    if (!next.startsWith("Re-check:"))
      out.push(`${where}: no "Re-check:" line follows the marker`);
    else if (!next.slice("Re-check:".length).trim())
      out.push(`${where}: the "Re-check:" line says nothing`);
  }

  return out;
}

// A checker that has never been shown to refuse anything is indistinguishable from one that
// cannot. Each case here must produce exactly the complaint named.
function selfTest() {
  const good = [
    "// UPSTREAM: skia-safe 0.153.3 -- rust-skia/rust-skia#1326 -- worked around",
    "// Re-check: cargo test the_thing",
  ].join("\n");

  const cases = [
    [good, 0, "a well-formed marker"],
    [
      "// UPSTREAM: Skia -- unfiled -- worked around\n// Re-check: x",
      1,
      "no version",
    ],
    [
      good.replace("rust-skia/rust-skia#1326", "soon"),
      1,
      "issue is not a reference",
    ],
    [good.replace("worked around", "mitigated"), 1, "unknown disposition"],
    [good.split("\n")[0], 1, "no Re-check line"],
    [good.replace("cargo test the_thing", ""), 1, "empty Re-check"],
    [
      "// UPSTREAM: skia-safe 0.153.3 -- unfiled\n// Re-check: x",
      1,
      "two fields",
    ],
    ["// nothing to see here", 0, "an ordinary comment"],
    // The convention documents itself, and prose about the marker must not be
    // read as one -- this case is why the marker has to open the comment.
    [
      "so `git grep UPSTREAM:` finds the whole class",
      0,
      "prose naming the marker",
    ],
    [
      "// see UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around",
      0,
      "marker not at the start",
    ],
  ];

  let failed = 0;
  for (const [text, want, what] of cases) {
    const got = findings(text, "self-test").length;
    if (got !== want) {
      failed++;
      console.error(
        `  self-test FAILED (${what}): expected ${want}, got ${got}`,
      );
    }
  }
  if (failed) {
    console.error(`upstream-notes: ${failed} self-test case(s) failed`);
    process.exit(1);
  }
  console.log(
    `upstream-notes: self-test, ${cases.length} cases, refuses each malformed shape`,
  );
}

function main() {
  selfTest();

  const tracked = execFileSync("git", ["ls-files"], { encoding: "utf8" })
    .split("\n")
    .filter(Boolean)
    // Binary and vendored paths carry no markers and reading them is waste.
    .filter((p) => !/^(docs\/assets|tests\/assets)\//.test(p));

  const problems = [];
  let marked = 0;

  for (const path of tracked) {
    let text;
    try {
      text = readFileSync(path, "utf8");
    } catch {
      continue; // unreadable as UTF-8: cannot carry a marker
    }
    if (!text.includes(MARKER)) continue;
    // Count what parses as a marker, not what mentions the word. AGENTS.md
    // both documents the marker and quotes it in prose, and only one of those
    // is a marker.
    marked += text
      .split("\n")
      .filter((l) => uncomment(l).startsWith(MARKER)).length;
    problems.push(...findings(text, path));
  }

  if (problems.length) {
    console.error("upstream notes that cannot be re-checked:\n");
    for (const p of problems) console.error(`  ${p}`);
    console.error(
      `\nThe convention is in AGENTS.md, "Upstream defects and the workarounds for them".`,
    );
    process.exit(1);
  }

  console.log(
    `upstream notes: ${marked} markers, each naming a version and a way to re-check`,
  );
}

if (import.meta.url === `file://${process.argv[1]}`) main();
