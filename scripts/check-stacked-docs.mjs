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
// WHAT IT CANNOT SEE, AND WILL NOT BE ABLE TO. Deleting an item and leaving its
// doc comment behind produces the same defect read backwards -- the orphaned
// block reattaches to whatever now follows and is wrong about it. That is
// invisible here in principle rather than by implementation: the change is a
// deletion, so the seam it creates lies between two lines that both already
// existed and nothing adjacent to it was added. `src/context/page.rs:725` was
// an instance, and not even a stacked summary -- a link to a type that no
// longer existed, which is a third shape again. AGENTS.md carries the habit
// that covers all of them: when you edit an item's doc comment, read the item
// below it.
//
// THE TYPESCRIPT HALF INVERTS BOTH OF THOSE, WHICH IS WHY IT GATES THE TREE.
// TypeScript keeps the LAST doc block before a declaration and discards every
// earlier one, so nothing is misattributed and nothing has to be read to know
// something went wrong: the first block is simply not published. And the seam
// is exact -- a line ending `*/` immediately followed by one opening `/**`,
// with no prose to judge and no false positive available. So the JavaScript and
// TypeScript pass runs over the whole tree in every mode, including the
// pre-commit one, where the Rust pass deliberately cannot.
//
// Six of these were on `main` in `lib/index.d.ts`. Twice the block that
// vanished was the better one, and both were explaining an ABSENCE -- why no
// construct signature is declared for `CanvasGradient` and
// `CanvasRenderingContext2D` when `lib.dom.d.ts` has one. A comment explaining
// why something is missing is the kind whose loss nobody notices, because the
// item still looks documented: `notDocumented` is satisfied, a rendered page
// shows a summary, and only someone who came to the page with that exact
// question finds out it was answered somewhere that no longer publishes.
//
// Two blocks separated by a BLANK line are not reported, though TypeScript
// drops the first of those too. A file-level block followed by the first
// declaration's block is that shape and is correct, and telling the two apart
// means reading for `@module` or `@packageDocumentation` -- a judgment, which is
// the thing this half is exact without. Measured at zero occurrences of either
// shape across the 60 tracked files when this was written, so nothing is being
// waved through.
//
// Usage:  node scripts/check-stacked-docs.mjs [--cached | --range <git-range>]
//         --cached          staged changes (the pre-commit case), the default
//         --range a..b      a commit range (the CI case)
//         --all             the whole tree, for triage only; never a gate
//
//         The mode applies to the Rust pass only. The JavaScript and
//         TypeScript pass is tree-wide under all three.
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
//
// A list item is excluded even though it closes a sentence: a summary is prose,
// and a bullet is not one. Without this, any two-item list whose entries end in
// a period and is followed by a paragraph break reports as a seam --
// `src/image.rs`'s validation list did exactly that.
const closes = (line) =>
  /^\s*\/\/\/\s+.*[.!?]\s*$/.test(line) &&
  !/^\s*\/\/\/\s+[-*+]\s/.test(line) &&
  !/^\s*\/\/\/\s+\d+[.)]\s/.test(line);
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

// Every `/** */` block in a source file, as line spans.
//
// Scanned rather than matched line by line, because `*/` and `/**` both occur
// inside string and template literals -- `lib/classes/*.js` builds error
// messages that contain them -- and a line-wise regex reports those as blocks.
// Line comments are skipped for the same reason. `/**/` is an empty block
// comment, not a doc comment, and is not collected.
function docBlocks(text) {
  const blocks = [];
  let line = 1;
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    if (c === "\n") {
      line++;
      i++;
    } else if (c === '"' || c === "'" || c === "`") {
      const quote = c;
      i++;
      while (i < n) {
        if (text[i] === "\\") {
          i += 2;
          continue;
        }
        if (text[i] === "\n") {
          line++;
          // Only a template literal survives a newline; anything else is an
          // unterminated string, and continuing past it would mis-scan the
          // rest of the file rather than the one line that is wrong.
          if (quote !== "`") break;
        }
        if (text[i] === quote) {
          i++;
          break;
        }
        i++;
      }
    } else if (c === "/" && text[i + 1] === "/") {
      while (i < n && text[i] !== "\n") i++;
    } else if (c === "/" && text[i + 1] === "*") {
      const start = line;
      const isDoc = text[i + 2] === "*" && text[i + 3] !== "/";
      i += 2;
      while (i < n && !(text[i] === "*" && text[i + 1] === "/")) {
        if (text[i] === "\n") line++;
        i++;
      }
      i += 2;
      if (isDoc) blocks.push({ start, end: line });
    } else {
      i++;
    }
  }
  return blocks;
}

// The block's first line of prose, for the report. A multi-line block opens on
// a bare `/**`, and printing that names nothing -- the reader needs the summary
// to find which comment is being described.
function summary(lines, block) {
  for (let n = block.start; n <= block.end; n++) {
    const text = lines[n - 1]
      .trim()
      .replace(/^\/\*\*+/, "")
      .replace(/\*\/$/, "")
      .replace(/^\*+/, "")
      .trim();
    if (text) return text;
  }
  return lines[block.start - 1].trim();
}

// Two doc blocks with nothing between them: the first is dropped.
//
// Both ends are required to own their line. A block closing with code after
// `*/`, or opening with code before `/**`, is an inline annotation rather than
// a leading comment, and the pair is not the defect.
function droppedBlocks(text) {
  const lines = text.split("\n");
  const blocks = docBlocks(text);
  const hits = [];
  for (let k = 0; k + 1 < blocks.length; k++) {
    const a = blocks[k];
    const b = blocks[k + 1];
    if (b.start !== a.end + 1) continue;
    if (!lines[a.end - 1].trimEnd().endsWith("*/")) continue;
    if (!lines[b.start - 1].trimStart().startsWith("/**")) continue;
    hits.push({ line: a.start, end: a.end, text: summary(lines, a) });
  }
  return hits;
}

const jsFindings = [];
for (const file of git("ls-files", "*.ts", "*.js", "*.mjs", "*.cjs")
  .trim()
  .split("\n")
  .filter(Boolean)) {
  jsFindings.push(
    ...droppedBlocks(readFileSync(file, "utf8")).map((h) => ({ ...h, file })),
  );
}

if (jsFindings.length) {
  console.error(
    `${jsFindings.length} doc comment${jsFindings.length === 1 ? "" : "s"} will not be published:\n`,
  );
  for (const f of jsFindings)
    console.error(`  ${f.file}:${f.line}-${f.end}\n    ${f.text}`);
  console.error(
    `
Each is a \`/** */\` block immediately followed by another one. TypeScript
attaches the last block before a declaration and discards the rest, so the
block reported above reaches nothing: it is not rendered, and no gate sees it
missing, because the declaration below still has the second block.

Merge the two into one block. If they describe different items, the earlier one
has lost its item -- put it back above the item it describes, or delete it.`,
  );
}

if (!findings.length && !jsFindings.length) {
  console.log(
    `no stacked doc comments introduced (${mode === "all" ? "whole tree" : mode === "cached" ? "staged changes" : range}), none dropped in JavaScript or TypeScript (whole tree)`,
  );
  process.exit(0);
}

if (!findings.length) process.exit(1);

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
