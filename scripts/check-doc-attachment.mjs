//
// Fails when a change leaves a Rust doc comment attached to a different item
// than the one it documented before the change.
//
// A `///` block documents whatever item follows it. Delete or move the item a
// block was written for, or insert a new item between a block and its item,
// and rustdoc attaches the block to whatever now follows -- joined to that
// item's own doc, or standing in for it -- while the item it described is left
// with none. `missing_docs` is satisfied because a comment exists, and the
// rendered page reads as one block. Eleven of these were on `main` at
// 815ca542, one of them on docs.rs: `css_number`'s description of a CSS
// `<number>` rendered as the documentation of `impl Font`.
//
// THE QUESTION ASKED. For every doc line with text that a change keeps -- a
// line `git diff` pairs between the base and the head -- the item it attaches
// to is found in both, by rustdoc's rule: the first line after it that is not
// a doc line, a blank line, a `//` comment or an attribute. Three outcomes:
//
//   - The same item, its line paired between the two: nothing changed.
//   - At the head an item that already existed at the base, elsewhere: the
//     doc has been left on an older item. That is a deletion or a move of
//     the item it was written for.
//   - At the head an item the change added, while the item the doc
//     described still exists further down with no summary of its own: a new
//     item was inserted between the doc and its item.
//
// Two changes look like the third and are not reported, both told apart from
// the diff and the items rather than from the comment's text. An item added in
// place of one removed is a rename or a reformatted signature, and the doc
// stays with it. An item split in two, its doc rewritten for the new part and
// a summary written for the old one, moved the doc on purpose. No prose is
// read, so no paragraph whose last sentence fits a line can be mistaken for a
// seam: replayed over the 150 commits on `main` before 815ca542 this refuses
// one, the one that put `style_colours`' doc on `srgb`, and it reports each
// of the eleven at the commit that made it. `scripts/check-stacked-docs.mjs`
// reads prose and keeps the case this cannot see: a new block written above an
// existing one, where every kept line stays on the item it was on.
//
// Usage:  node scripts/check-doc-attachment.mjs [--cached | --range <a...b>]
//         --cached      the staged changes against HEAD (the pre-commit case),
//                       the default
//         --range a...b the merge base of a and b against b (a pull
//                       request), or a..b, a against b (a push); both
//                       versions are read from git, not the work tree
//         --self-test   check the rule on built cases; runs nothing else
//

import { execFileSync } from "child_process";
import { mkdtempSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

const args = process.argv.slice(2);
const mode = args.includes("--self-test")
  ? "self-test"
  : args.includes("--range")
    ? "range"
    : "cached";
const range = args[args.indexOf("--range") + 1];

const git = (...a) =>
  execFileSync("git", a, { encoding: "utf8", maxBuffer: 1 << 28 });

// An outer doc line, `///` and not `////`.
const doc = (line) => /^\s*\/\/\/(?!\/)/.test(line);
// A doc line carrying text.
const docText = (line) => doc(line) && /^\s*\/\/\/\s*\S/.test(line);
const lineComment = (line) => /^\s*\/\/(?!\/)/.test(line);
const attribute = (line) => /^\s*#!?\[/.test(line);

// The index of the line the doc line at `i` attaches to, by rustdoc's rule:
// past doc lines, blank lines, `//` comments and attributes, including an
// attribute that spans lines, to the first line that is none of them. `-1`
// when the file ends first.
function itemOf(lines, i) {
  let depth = 0;
  for (let j = i; j < lines.length; j++) {
    const s = lines[j];
    if (depth > 0) {
      depth += count(s, "[") - count(s, "]");
      continue;
    }
    if (doc(s) || !s.trim() || lineComment(s)) continue;
    if (attribute(s)) {
      depth = count(s, "[") - count(s, "]");
      continue;
    }
    return j;
  }
  return -1;
}
const count = (s, c) => s.split(c).length - 1;

// The kind and name an item line declares -- `fn capture`, `struct Captured`
// -- or `null` for a line that declares none this can name.
function itemName(line) {
  const m = line.match(
    /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|default|extern\s+"[^"]*")\s+)*(fn|struct|enum|trait|type|const|static|mod|union)\s+([A-Za-z_][A-Za-z0-9_]*)/,
  );
  return m ? `${m[1]} ${m[2]}` : null;
}

// Lines `git diff -U0` keeps, as base index -> head index (0-based), paired
// in order: every line outside the hunks' removed ranges in the base, against
// every line outside their added ranges in the head.
function keptPairs(diff, baseLength, headLength) {
  const removed = new Set();
  const added = new Set();
  for (const line of diff.split("\n")) {
    const h = line.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/);
    if (!h) continue;
    const [bs, bc, hs, hc] = [
      +h[1],
      h[2] === undefined ? 1 : +h[2],
      +h[3],
      h[4] === undefined ? 1 : +h[4],
    ];
    for (let n = 0; n < bc; n++) removed.add(bs - 1 + n);
    for (let n = 0; n < hc; n++) added.add(hs - 1 + n);
  }
  const baseKept = [];
  for (let n = 0; n < baseLength; n++) if (!removed.has(n)) baseKept.push(n);
  const headKept = [];
  for (let n = 0; n < headLength; n++) if (!added.has(n)) headKept.push(n);
  if (baseKept.length !== headKept.length)
    throw new Error(
      `kept lines do not pair: ${baseKept.length} in the base, ${headKept.length} in the head`,
    );
  const toHead = new Map();
  baseKept.forEach((b, k) => toHead.set(b, headKept[k]));
  return toHead;
}

// Whether the item a doc described, its line replaced by the change, is
// still there below the item the doc now sits on: an added line declaring the
// same kind and name. That is an item inserted between the doc and its item
// in the same hunk that rewrote the item's signature -- `struct Captured`
// put above `fn capture` as `capture` took a new return type. Only added
// lines count, so another item of the same name the change did not touch --
// one `fn new` among many -- is not taken for it.
function resigned(baseLine, head, ih, fromHead) {
  const name = itemName(baseLine);
  if (!name || itemName(head[ih]) === name) return -1;
  for (let j = ih + 1; j < head.length; j++)
    if (!fromHead.has(j) && itemName(head[j]) === name) return j;
  return -1;
}

// Whether the item on line `i` has a summary of its own: a doc comment that
// attaches to it by the same rule and opens on prose. A doc that opens on a
// section heading -- `# Errors` and nothing above it -- is what an item keeps
// when its summary has gone to the item inserted above it, as
// `EncodeOptions::to_internal`'s did when `validate` arrived between them.
// How far above an item its doc comment is looked for, in lines. Chosen, not
// fixed: well past the longest doc comment in the tree, so the search ends in
// another item's code long before it, and only bounding the cost.
const DOC_REACH = 400;

function documented(lines, i) {
  let first = -1;
  for (let d = i - 1; d >= 0 && d >= i - DOC_REACH; d--)
    if (docText(lines[d]) && itemOf(lines, d) === i) first = d;
  return first >= 0 && !/^\s*\/\/\/\s*#/.test(lines[first]);
}

// The reattached runs between two versions of one file.
function reattached(baseText, headText, diff) {
  const base = baseText.split("\n");
  const head = headText.split("\n");
  const toHead = keptPairs(diff, base.length, head.length);
  const fromHead = new Map([...toHead].map(([b, h]) => [h, b]));
  const runs = [];
  for (const [b, h] of toHead) {
    if (!docText(base[b])) continue;
    const ib = itemOf(base, b);
    const ih = itemOf(head, h);
    if (ib < 0 || ih < 0) continue;
    if (toHead.get(ib) === ih) continue; // the same item
    let shape;
    if (fromHead.has(ih)) shape = "left on an older item";
    else {
      // Where the item the doc described is now, past the item inserted
      // above it; -1 for an item replaced in place -- a rename or a new
      // signature -- whose doc stays with it.
      const was = toHead.has(ib)
        ? toHead.get(ib)
        : resigned(base[ib], head, ih, fromHead);
      if (was < 0) continue;
      // An item split in two -- the doc rewritten for the new one, the old
      // one given a summary of its own -- moved the doc on purpose. What is
      // reported is a doc whose item is left without one.
      if (documented(head, was)) continue;
      shape = "an item inserted between";
    }
    const last = runs[runs.length - 1];
    if (last && last.item === ih && last.was === ib) continue;
    runs.push({ line: h + 1, text: head[h].trim(), item: ih, was: ib, shape });
  }
  return runs.map((r) => ({
    line: r.line,
    text: r.text,
    shape: r.shape,
    now: head[r.item].trim(),
    was: base[r.was].trim(),
  }));
}

// Built cases: a base and a head of one file, and how many reattached runs
// the head holds. The first are the shapes the eleven on `main` took, each
// made the way it happened; the rest are changes that must pass.
const CASES = {
  "an item deleted, its doc left on the next": [
    "/// Doc of alpha.\nfn alpha() {}\n\n/// Doc of beta.\nfn beta() {}\n",
    "/// Doc of alpha.\n\n/// Doc of beta.\nfn beta() {}\n",
    1,
  ],
  "an item moved away, its doc left on the next": [
    "/// Doc of gamma.\nfn gamma() {}\n\nfn delta() {}\n",
    "/// Doc of gamma.\n\nfn delta() {}\n\nfn gamma() {}\n",
    1,
  ],
  "a documented item inserted between a doc and its item": [
    "/// Closes a recording.\nfn capture() {}\n",
    "/// Closes a recording.\n/// What capture resolved to.\nstruct Captured {}\n\nfn capture() {}\n",
    1,
  ],
  "an undocumented item inserted between a doc and its item": [
    "    /// Kept for each draw.\n    pub fn laid_out(&self) {}\n",
    "    /// Kept for each draw.\n    pub fn colour_features(&self) {}\n\n    pub fn laid_out(&self) {}\n",
    1,
  ],
  "an impl block inserted between a doc and its function": [
    "/// A number in its shortest form.\nfn css_number() {}\n",
    "/// A number in its shortest form.\nimpl Font {\n    fn weight_css(&self) {}\n}\n\nfn css_number() {}\n",
    1,
  ],
  "a test inserted between a test's doc and its test": [
    "    /// Every spelling the grammar allows.\n    #[test]\n    fn either_end() {}\n",
    "    /// Every spelling the grammar allows.\n    #[test]\n    fn named_colour() {}\n\n    #[test]\n    fn either_end() {}\n",
    1,
  ],
  "a field deleted, its doc left on the next": [
    "struct S {\n    /// The old field.\n    rec: u8,\n    /// How it composites.\n    composite: u8,\n}\n",
    "struct S {\n    /// The old field.\n    /// How it composites.\n    composite: u8,\n}\n",
    1,
  ],
  "an item inserted between as its own signature changed": [
    "/// Closes a recording.\nfn capture(\n    source: &mut Canvas,\n) -> Option<(Content, usize)> {\n}\n",
    "/// Closes a recording.\n/// What capture resolved to.\nstruct Captured {\n    cost: usize,\n}\n\nfn capture(source: &mut Canvas) -> Option<Captured> {\n}\n",
    1,
  ],
  // check-stacked-docs' case, not this one's: every kept line stays on the
  // item it was on, and the block written for `a` is all added lines.
  "a new block written above an existing one": [
    "/// Doc of b.\nfn b() {}\n",
    "/// Doc of a.\n/// Doc of b.\nfn b() {}\n\nfn a() {}\n",
    0,
  ],
  "an item split in two, the old part given a doc of its own": [
    "/// Blurs premultiplied pixels.\n/// In f32.\npub fn blur_bytes() {}\n",
    "/// Blurs premultiplied pixels.\n/// In f32, into `out`.\npub fn blur_bytes_into() {}\n\n" +
      "/// [`blur_bytes_into`], into a buffer of its own.\npub fn blur_bytes() {}\n",
    0,
  ],
  "an item split in two, the old part left with only a section": [
    "    /// Lowers these settings.\n    ///\n    /// # Errors\n    ///\n    /// When it cannot.\n    fn to_internal() {}\n",
    "    /// Lowers these settings.\n    fn validate() {}\n\n    /// # Errors\n    ///\n    /// When it cannot.\n    fn to_internal() {}\n",
    1,
  ],
  "an item renamed in place": [
    "/// Doc of the projection.\nfn get_projection() {}\n",
    "/// Doc of the projection.\nfn projection() {}\n",
    0,
  ],
  "an item renamed beside an untouched one of the old name": [
    "impl A {\n    /// Makes an A.\n    fn new() {}\n}\nimpl B {\n    fn new() {}\n}\n",
    "impl A {\n    /// Makes an A.\n    fn create() {}\n}\nimpl B {\n    fn new() {}\n}\n",
    0,
  ],
  "a signature reformatted over lines": [
    "/// Doc.\nfn f(a: u8, b: u8) {}\n",
    "/// Doc.\nfn f(\n    a: u8,\n    b: u8,\n) {}\n",
    0,
  ],
  "an attribute added between a doc and its item": [
    "/// Doc.\nfn f() {}\n",
    "/// Doc.\n#[inline]\n#[cfg(any(\n    unix,\n    windows\n))]\nfn f() {}\n",
    0,
  ],
  "a paragraph added to a doc": [
    "/// Doc.\nfn f() {}\n",
    "/// Doc.\n///\n/// More about it.\nfn f() {}\n",
    0,
  ],
  "an item moved with its doc": [
    "/// Doc of a.\nfn a() {}\n\n/// Doc of b.\nfn b() {}\n",
    "/// Doc of b.\nfn b() {}\n\n/// Doc of a.\nfn a() {}\n",
    0,
  ],
  "an item and its doc deleted together": [
    "/// Doc of a.\nfn a() {}\n\n/// Doc of b.\nfn b() {}\n",
    "/// Doc of b.\nfn b() {}\n",
    0,
  ],
  "an item deleted with its doc, beside a doc repeating a line of it": [
    "/// A.\n///\n/// # Examples\nfn a() {}\n\n/// B.\n///\n/// # Examples\nfn b() {}\n",
    "/// B.\n///\n/// # Examples\nfn b() {}\n",
    0,
  ],
};

function diffOf(baseText, headText) {
  const dir = mkdtempSync(join(tmpdir(), "doc-attachment-"));
  try {
    writeFileSync(join(dir, "base"), baseText);
    writeFileSync(join(dir, "head"), headText);
    try {
      return git(
        "diff",
        "--no-index",
        "-U0",
        join(dir, "base"),
        join(dir, "head"),
      );
    } catch (e) {
      if (e.status === 1) return e.stdout; // differences found
      throw e;
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (mode === "self-test") {
  const wrong = Object.entries(CASES).filter(
    ([, [base, head, want]]) =>
      reattached(base, head, diffOf(base, head)).length !== want,
  );
  if (!wrong.length) {
    console.log(
      `self-test: the attachment rule holds on all ${Object.keys(CASES).length} built cases`,
    );
    process.exit(0);
  }
  console.error(
    `self-test: the attachment rule is wrong on ${wrong.length} of ${Object.keys(CASES).length} built cases:\n`,
  );
  for (const [name, [base, head, want]] of wrong)
    console.error(
      `  ${name}: ${reattached(base, head, diffOf(base, head)).length} found, ${want} expected`,
    );
  process.exit(1);
}

// The two versions compared, as revisions `git show` can read: the merge base
// and the range's head, or HEAD and the index.
let baseRev, headRev, scope;
if (mode === "range") {
  // `a...b` compares b with its merge base with a, as a pull request is
  // reviewed; `a..b` compares b with a, as a push is.
  const three = range.includes("...");
  const [a, b] = range.split(three ? "..." : "..");
  headRev = b || "HEAD";
  baseRev = three ? git("merge-base", a, headRev).trim() : a;
  scope = range;
} else {
  baseRev = "HEAD";
  headRev = ""; // the index: `git show :path`
  scope = "staged changes";
}
const diffArgs = mode === "range" ? [baseRev, headRev] : ["--cached"];
const files = git(
  "diff",
  "--name-only",
  "--diff-filter=M",
  ...diffArgs,
  "--",
  "*.rs",
)
  .trim()
  .split("\n")
  .filter(Boolean);

const findings = [];
let kept = 0;
for (const file of files) {
  const baseText = git("show", `${baseRev}:${file}`);
  const headText = git("show", `${headRev}:${file}`);
  const diff = git("diff", "-U0", ...diffArgs, "--", file);
  for (const r of reattached(baseText, headText, diff))
    findings.push({ ...r, file });
  kept++;
}

if (!findings.length) {
  console.log(
    `no doc comment moved to another item (${scope}: ${kept} changed Rust file${kept === 1 ? "" : "s"} compared)`,
  );
  process.exit(0);
}

console.error(
  `${findings.length} doc comment${findings.length === 1 ? "" : "s"} now attach${findings.length === 1 ? "es" : ""} to a different item:\n`,
);
for (const f of findings)
  console.error(
    `  ${f.file}:${f.line} (${f.shape})\n    ${f.text}\n    now on:      ${f.now}\n    written for: ${f.was}`,
  );
console.error(
  `
Each is a doc comment this change kept while the item below it changed. rustdoc
attaches a doc comment to the item that follows it, so this one now documents
the item it is on, and the item it was written for has lost it.

Move the comment back above the item it describes, or, if that item is gone,
delete or rewrite it for the item it is on.`,
);
process.exit(1);
