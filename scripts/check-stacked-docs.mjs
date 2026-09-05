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
//         --self-test       check the scanner, not the tree; runs nothing else
//
//         The mode applies to the Rust pass only. The JavaScript and
//         TypeScript pass is tree-wide under all three, and every run prints
//         how much each pass read -- a green line saying "0 files" is the one
//         thing this check cannot otherwise distinguish from coverage.
//
// WHAT `--self-test` IS FOR. The scanner was blind in two of sixty files and
// reported success: a quote inside a regular expression opened a string that
// never closed, and the newline that ended it was counted twice, so every line
// number below it drifted and the closing-line check stopped matching. A gate
// that reads the whole tree is worth having only if it can read the whole
// tree, and nothing in a green run distinguishes "found nothing" from "saw
// nothing". So the self-test appends a known stacked pair to every tracked
// file and asserts the count back, and runs a list of hazards that a scanner
// missing one of its states loses its place on. Both halves are needed: with
// both original defects present two files went blind, but with either one
// alone the tree is clean, because each masks the other.
//

import { execFileSync } from "child_process";
import { readFileSync } from "fs";

const args = process.argv.slice(2);
const mode = args.includes("--self-test")
  ? "self-test"
  : args.includes("--all")
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
// What the Rust pass looked at, printed with the verdict. Without it, the
// same green line means "examined nine hundred lines and found nothing" and
// "examined nothing": `check-docs` is `--cached`, and after a commit the
// staged diff is empty, so `just ci` on a clean tree asks the Rust half a
// question about no lines at all.
let rustScope = "";
if (mode === "all") {
  const files = git("ls-files", "*.rs").trim().split("\n").filter(Boolean);
  for (const f of files)
    findings.push(
      ...candidates(readFileSync(f, "utf8")).map((c) => ({ ...c, file: f })),
    );
  rustScope = `${files.length} file${files.length === 1 ? "" : "s"}`;
} else if (mode !== "self-test") {
  // The self-test checks the scanner rather than the tree, so the Rust pass
  // has nothing to contribute and its diff modes do not apply.
  const diffArgs = mode === "cached" ? ["--cached"] : [range];
  const byFile = addedLines(diffArgs);
  let addedCount = 0;
  for (const added of byFile.values()) addedCount += added.size;
  rustScope = `${byFile.size} file${byFile.size === 1 ? "" : "s"}, ${addedCount} added line${addedCount === 1 ? "" : "s"}`;
  for (const [file, added] of byFile) {
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
// inside string, template and regular-expression literals -- `lib/classes/*.js`
// builds error messages that contain them, and `css.js` matches quotes with
// `/^(['"])(.*?)\1$/` -- and a line-wise regex reports those as blocks. Line
// comments are skipped for the same reason. `/**/` is an empty block comment,
// not a doc comment, and is not collected.
//
// A regular-expression literal needs its own state rather than falling through
// to the character-by-character branch, because the quotes and slashes inside
// one would otherwise open a string that swallows the rest of the file. `/` is
// division when the previous significant character could end an expression and
// a literal otherwise -- with the keyword list below as the exception, because
// `return`, `typeof` and their neighbours end in a word character and cannot
// end an expression. Without it `return /a\`b/.test(s)` read the `/` as
// division and the backtick as a template literal, which then ran to the end
// of the file: the tree carries `return /.../` in two files today and a
// backtick in one of them was one edit away. `--self-test` is what holds it: it proves the
// scanner still finds an injected pair in every tracked file, so a heuristic
// that starts guessing wrong shows up as a count below the file total rather
// than as silence.
// THE TWO MISTAKES ARE NOT SYMMETRIC, which is why the lists below can be
// heuristics and why there are no hazards guarding the other direction.
//
// Reading a literal as division is the dangerous one: the quote or backtick
// inside it then opens a string or a template, and a template does not stop
// at a newline, so the scanner can go blind to the end of the file.
//
// Reading division as a literal costs at most the rest of one line. The
// literal scan breaks at the first newline, `/*` is claimed by the block
// comment branch before this one is reached, and `droppedBlocks` only ever
// considers a `/**` that starts its line and a `*/` that ends one. So a
// false literal cannot swallow either end of a pair. Attempting to write a
// hazard for it proved this rather than the reverse: making the condition
// list accept every identifier changed no result at all.
//
// Keywords after which `/` opens a literal rather than dividing. Each ends in
// a word character, which is what the `prev` test alone gets wrong.
const EXPRESSION_KEYWORDS = new Set([
  "await",
  "case",
  "delete",
  "do",
  "else",
  "in",
  "instanceof",
  "new",
  "of",
  "return",
  "throw",
  "typeof",
  "void",
  "yield",
]);

// Statements of the form `keyword (condition) statement`, where a `/` after
// the closing paren opens a literal rather than dividing. The paren is what
// makes them different from the keyword list above: `)` ends an expression
// everywhere else, so `(a + b) / 2` has to stay division.
const CONDITION_KEYWORDS = new Set(["for", "if", "while", "with"]);

// Whether the `)` immediately before `at` closes one of those conditions.
//
// Walks back to the matching `(` counting depth, then asks `wordBefore` what
// introduced it. A paren inside a string or a regular expression in the
// condition can throw the count off; the result of getting it wrong is that a
// literal is read as division, which is what happens without this at all. The
// scan is bounded so a file with thousands of parens costs nothing noticeable.
function closesCondition(text, at) {
  let end = at;
  while (end > 0 && /\s/.test(text[end - 1])) end--;
  if (text[end - 1] !== ")") return false;
  let depth = 0;
  for (let k = end - 1; k >= 0 && end - k < 4096; k--) {
    if (text[k] === ")") depth++;
    else if (text[k] === "(") {
      depth--;
      if (depth === 0) return CONDITION_KEYWORDS.has(wordBefore(text, k));
    }
  }
  return false;
}

// The identifier immediately before `at`, ignoring whitespace.
function wordBefore(text, at) {
  let end = at;
  while (end > 0 && /\s/.test(text[end - 1])) end--;
  let start = end;
  while (start > 0 && /[\w$]/.test(text[start - 1])) start--;
  return text.slice(start, end);
}

function docBlocks(text) {
  const blocks = [];
  let line = 1;
  let i = 0;
  const n = text.length;
  // The last non-whitespace character outside a comment, for the call above.
  let prev = "";
  while (i < n) {
    const c = text[i];
    if (c === "\n") {
      line++;
      i++;
      prev = "\n";
    } else if (c === '"' || c === "'" || c === "`") {
      const quote = c;
      i++;
      while (i < n) {
        if (text[i] === "\\") {
          // An escape may cover a newline -- a line continuation in a quoted
          // string is exactly that -- and skipping it uncounted drifts every
          // line number below it.
          if (text[i + 1] === "\n") line++;
          i += 2;
          continue;
        }
        if (text[i] === "\n") {
          // Only a template literal survives a newline; anything else is an
          // unterminated string, and continuing past it would mis-scan the
          // rest of the file rather than the one line that is wrong. The
          // outer loop counts this newline, so it is not counted here.
          if (quote !== "`") break;
          line++;
        }
        if (text[i] === quote) {
          i++;
          break;
        }
        i++;
      }
      prev = quote;
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
      prev = "/";
    } else if (
      c === "/" &&
      (!/[\w$)\]]/.test(prev) ||
        EXPRESSION_KEYWORDS.has(wordBefore(text, i)) ||
        closesCondition(text, i))
    ) {
      // A regular-expression literal: to the next unescaped `/` that is not
      // inside a character class, which is where `[/*]` would otherwise end
      // it early. An unterminated one stops at the newline for the same
      // reason a quoted string does.
      i++;
      let inClass = false;
      while (i < n) {
        const d = text[i];
        if (d === "\\") {
          if (text[i + 1] === "\n") line++;
          i += 2;
          continue;
        }
        if (d === "\n") break;
        if (d === "[") inClass = true;
        else if (d === "]") inClass = false;
        else if (d === "/" && !inClass) {
          i++;
          break;
        }
        i++;
      }
      prev = "/";
    } else {
      if (!/\s/.test(c)) prev = c;
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

// Tracked JavaScript and TypeScript, from the repository root. `git ls-files`
// resolves its globs against the working directory, so run from `src/` it
// returns nothing and this pass would report a clean tree having read no
// files. Every caller -- the recipe, the hook, `rust-ci.yml` -- runs at the
// root, but the count printed with the verdict is what makes the difference
// visible if one ever does not.
const jsFiles = git("ls-files", "*.ts", "*.js", "*.mjs", "*.cjs")
  .trim()
  .split("\n")
  .filter(Boolean);

// Proof that the scanner can still see a stacked pair in every file it is
// asked about. Appended rather than inserted at a line, so it is sensitive to
// any state the scanner has entered and not left anywhere above -- an
// unterminated template opened by a backtick inside a regular expression runs
// to the end of the file, and this is what catches that.
//
// This exists because the scanner was blind in two of sixty files and said so
// with a green tick. A tree-wide gate is worth having only if it reads the
// tree, and nothing else here can tell "found nothing" from "saw nothing".
const SELF_TEST_PAIR =
  "\n/** A block the self-test appends. */\n" +
  "/** A second one, directly under it. */\nconst __stackedDocSelfTest = 1;\n";

// Sources that trip a scanner written without one of the states above. The
// tree is not enough on its own: with both of the defects this replaced
// present, two of sixty files went blind, but with either one alone the tree
// is clean, because each mechanism masks the other. Mutation-tested -- remove
// the regular-expression branch and the first four fail; take the newline
// count back out of the escape skip and the last one does.
const HAZARDS = {
  "quote inside a regular expression": "const re = /^(['\"])(.*?)\\1$/;\n",
  "backtick inside a regular expression": "const re = /Expected \\`x\\`/;\n",
  "lone backtick in a regular expression": "const re = /a\\`b/;\n",
  "comment opener in a character class": "const re = /[/*]+/;\n",
  "escaped newline in a quoted string": "const s = 'one \\\n two';\n",
  // Malformed rather than merely awkward, and here for the line count rather
  // than for itself: the newline that ends an unterminated string is counted
  // by the outer loop, and counting it twice drifts every line below it.
  "an apostrophe with no partner": "const s = 'it;\nconst t = 2;\n",
  "division, which is not a regular expression":
    "const half = width / 2, rest = height / 2;\n",
  "a regular expression after `return`":
    "function f(s) {\n  return /a\\`b/.test(s);\n}\n",
  "a regular expression after `typeof`": "const t = typeof /a\\`b/;\n",
  "a regular expression after a condition":
    "function f(s) {\n  if (s) /a\\`b/.test(s);\n}\n",
  "template literal holding a block terminator": "const t = `\n*/\n`;\n",
  "template literal holding a block opener": "const t = `\n/**\n`;\n",
};

// Whether the appended pair is reported, at the line it was appended to.
//
// Asserting the line rather than the count, because a line number counted
// wrongly is the defect this check was built after and both blocks of a pair
// drift by the same amount -- adjacency alone survives it. MSC B made that
// argument and their harness asserted the line before this one did.
//
// Honest about what it buys for THIS assertion: nothing measurable.
// `droppedBlocks` indexes the real `lines` array with the scanner's number
// when it checks that the first block closes on `*/`, so a drift slides that
// guard off the pair and the hit disappears rather than moving. Three
// constructions between two people ended with the count failing first. What
// the line buys is the dependency: it no longer rests on that guard staying
// where it is.
//
// The claim is about an appended pair and does not generalise, which is worth
// knowing before someone reaches for it as a property of `droppedBlocks`.
// Given a scanner that miscounts lines -- which no version here does, and the
// construction below needs one built on purpose -- a drift can preserve the
// finding count and move the position: a file whose seams are two independent
// regions loses one and gains another, and the totals cancel. MSC B built one
// against a mutant of this file with the newline count removed from the escape
// skip: a real 3-5 block reported as 5-5 with the summary `*/`, one finding
// either way. A single run of blocks cannot do it, because the window slides
// off the end and the count always loses one.
//
// So the position is what the appended-pair assertion would catch if a
// miscount were ever reintroduced, and it is not a behaviour of the gate as it
// stands. Comparing the gate's output against known content would watch for
// that directly and is a third check; naming it here is deliberate, because
// building one nobody needs is how three hazards that could not fail came to
// be written.
function seesAppendedPair(before) {
  const at = before.split("\n").length; // SELF_TEST_PAIR opens with a newline
  return droppedBlocks(before + SELF_TEST_PAIR).some((h) => h.line === at + 1);
}

if (mode === "self-test") {
  const blind = jsFiles.filter(
    (file) => !seesAppendedPair(readFileSync(file, "utf8")),
  );
  const missed = Object.keys(HAZARDS).filter(
    (name) => !seesAppendedPair(HAZARDS[name]),
  );
  if (blind.length === 0 && missed.length === 0) {
    console.log(
      `self-test: an appended stacked pair is found in ${jsFiles.length} of ` +
        `${jsFiles.length} tracked files, and past all ` +
        `${Object.keys(HAZARDS).length} hazards`,
    );
    process.exit(0);
  }
  if (missed.length) {
    console.error(
      `self-test: the scanner loses its place on ${missed.length} of ` +
        `${Object.keys(HAZARDS).length} hazards:\n`,
    );
    for (const name of missed) console.error(`  ${name}`);
    console.error("");
  }
  if (blind.length === 0) {
    console.error(
      `The tree itself is clean -- ${jsFiles.length} of ${jsFiles.length} ` +
        "files still report an appended pair -- which is why the hazards are " +
        "here. A tree that happens to contain nothing that trips the scanner " +
        "proves nothing about the scanner.",
    );
    process.exit(1);
  }
  console.error(
    `self-test: the scanner is blind in ${blind.length} of ${jsFiles.length} tracked files:\n`,
  );
  for (const file of blind) console.error(`  ${file}`);
  console.error(
    `
Each of these carries something the scanner mis-reads -- a quote or a backtick
inside a regular expression, an escape spanning a newline -- and it leaves the
rest of that file invisible to the check while the check still reports success.

Find what the scanner enters and does not leave. Do not narrow this test to the
files that pass: the point of it is that the count is the file count.`,
  );
  process.exit(1);
}

const jsFindings = [];
for (const file of jsFiles) {
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
  const scope =
    mode === "all"
      ? "whole tree"
      : mode === "cached"
        ? "staged changes"
        : range;
  console.log(
    `no stacked doc comments introduced (${scope}: ${rustScope}), ` +
      `none dropped in JavaScript or TypeScript (${jsFiles.length} files)`,
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
