//
// Fails when a commit introduces a doc comment block written above an existing
// one rather than above its own item.
//
// rustdoc concatenates two adjacent `///` blocks with no item between them, so
// both land on the item that follows and the item the first block described is
// left with no documentation at all. Nine of these were found in seven files,
// one of them live on `main` -- `export` is public, so `Pages::write` renders
// on docs.rs opening with `spans_every_page`'s summary. One file carried three
// summaries on a single item, in reverse order of the functions below them.
//
// Nothing else catches it. `missing_docs` is satisfied because a comment
// exists, and rustdoc has no opinion about which item a comment describes, so
// `just ci` is green and a human reviewer reads the block as the item's own.
// Five of the nine were introduced within two days, and one while fixing
// another.
//
// WHY THIS IS SCOPED TO THE DIFF AND NOT THE TREE. The detector is a heuristic
// over comment text, not a proof, and it cannot be made exact. A stacked
// summary and a paragraph's closing sentence are textually identical -- both a
// capitalised sentence, closing a line, directly after another sentence,
// directly before a blank doc line. What separates them is whether the sentence
// is *about the item below*, which is comprehension.
//
// Measured rather than assumed. On a tree with all nine present, the best
// structural discriminator available -- paragraphs remaining in the block --
// gave real cases at 1, 2 and 5 and prose at 1, 2 and 8. Complete overlap.
// Widening the pattern to catch a summary that wraps across lines took the
// false positives from three to twenty.
//
// So a tree-wide gate would cost four reworded comments, three of them
// load-bearing, and buy nothing. Scoped to the diff it enforces "do not add
// another one", which is the failure that actually happens. This is not an
// exemption list: nothing is listed, and a pre-existing candidate that a commit
// touches is reported like any other.
//
// Usage:  node scripts/check-stacked-docs.mjs [--cached | --range <git-range>]
//         --cached          staged changes (the pre-commit case), the default
//         --range a..b      a commit range (the CI case)
//         --all             the whole tree, for triage only; never a gate
//

import { execFileSync } from "child_process";
import { readFileSync } from "fs";

const args = process.argv.slice(2);
const mode = args.includes("--all")
  ? "all"
  : args.includes("--range")
    ? "range"
    : "cached";
const range = args[args.indexOf("--range") + 1];

const git = (...a) => execFileSync("git", a, { encoding: "utf8" });

// A doc line carrying text, whose text ends a sentence.
const closes = (line) => /^\s*\/\/\/\s+.*[.!?]\s*$/.test(line);
// A doc line with nothing on it -- the paragraph break a second block opens with.
const blank = (line) => /^\s*\/\/\/\s*$/.test(line);

// Any doc line, text or blank -- used to find the extent of a doc run.
const doc = (line) => /^\s*\/\/\/(\s|$)/.test(line);

// The seam: two sentence-closing doc lines in a row, the second followed by a
// blank doc line. The candidate is the second -- the summary that drifted.
//
// `span` is the whole contiguous run of doc lines the seam sits in, and it is
// what the diff is tested against rather than the seam alone. That distinction
// is the difference between a working check and one that never fires: when a
// new block is written ABOVE an existing one -- which is how all nine of the
// known cases happened -- the drifted summary is the OLD block's first line,
// and the diff does not touch it. Only the lines above it are added. Scoping to
// the seam line reports nothing, forever, for the exact defect this exists to
// find, while still firing for prose appended below a block, which is not what
// goes wrong. Found by MSC A on their own implementation of this, and confirmed
// on mine by injecting both arrangements: `above` passed and `adjacent` failed.
function candidates(text) {
  const lines = text.split("\n");
  const hits = [];
  for (let i = 0; i + 2 < lines.length; i++) {
    if (!(closes(lines[i]) && closes(lines[i + 1]) && blank(lines[i + 2])))
      continue;
    let from = i;
    while (from > 0 && doc(lines[from - 1])) from--;
    let to = i + 2;
    while (to + 1 < lines.length && doc(lines[to + 1])) to++;
    hits.push({
      line: i + 2,
      text: lines[i + 1].trim(),
      span: [from + 1, to + 1],
    });
  }
  return hits;
}

// Added line numbers per file, from a zero-context diff.
function addedLines(diffArgs) {
  const out = git("diff", "-U0", ...diffArgs, "--", "*.rs");
  const byFile = new Map();
  let file = null;
  for (const line of out.split("\n")) {
    const f = line.match(/^\+\+\+ b\/(.+)$/);
    if (f) {
      file = f[1];
      byFile.set(file, new Set());
      continue;
    }
    const h = line.match(/^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/);
    if (h && file) {
      const start = +h[1];
      const count = h[2] === undefined ? 1 : +h[2];
      for (let n = start; n < start + count; n++) byFile.get(file).add(n);
    }
  }
  return byFile;
}

let findings = [];
if (mode === "all") {
  const files = git("ls-files", "*.rs").trim().split("\n").filter(Boolean);
  for (const f of files)
    findings.push(
      ...candidates(readFileSync(f, "utf8")).map((c) => ({ ...c, file: f })),
    );
} else {
  const diffArgs = mode === "cached" ? ["--cached"] : [range];
  for (const [file, added] of addedLines(diffArgs)) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue; // deleted in this change
    }
    findings.push(
      ...candidates(text)
        .filter((c) => {
          for (let n = c.span[0]; n <= c.span[1]; n++)
            if (added.has(n)) return true;
          return false;
        })
        .map((c) => ({ ...c, file })),
    );
  }
}

if (!findings.length) {
  console.log(
    `no stacked doc comments introduced (${mode === "all" ? "whole tree" : mode === "cached" ? "staged changes" : range})`,
  );
  process.exit(0);
}

console.error(
  `${findings.length} doc comment${findings.length === 1 ? "" : "s"} may have stacked onto the wrong item:\n`,
);
for (const f of findings) console.error(`  ${f.file}:${f.line}\n    ${f.text}`);
console.error(
  `
Each is a sentence that closes a doc block, directly followed by a blank doc
line, directly after another sentence that closes one. That is the shape of a
second block written above an existing one: rustdoc joins them, both land on the
item below, and the item the first block described ends up with none.

Read the item below the block. If the summary belongs to it, this is a false
positive -- a paragraph whose last sentence happens to fit one line -- and the
fix is to say so in review, not to reword good prose. If it belongs to a
different item, move it there.`,
);
process.exit(1);
