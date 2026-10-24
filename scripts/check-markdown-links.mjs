#!/usr/bin/env node
// Every relative link in the tracked Markdown resolves: to a tracked file or a
// directory holding one, and -- where it names an anchor in a Markdown file --
// to a heading or an explicit `id`/`name` in that file.
//
// Nothing checked this, and a broken link fails silently: it renders, and the
// reader finds the 404 later. Moving a section between files is exactly how one
// breaks, so the check exists before anything is moved.
//
// Anchors are GitHub's: a heading's rendered text, lowercased, with every
// character that is not a letter, a mark, a number, a space, "-" or "_"
// dropped, and each space turned into "-". A repeated heading takes "-1",
// "-2", ... in order. Links to anything other than a Markdown file are checked
// for the file only, since a `#L10` into source is a line, not a heading.
// Links with a scheme (`https:`, `mailto:`) are not followed.
//
// Usage:  node scripts/check-markdown-links.mjs [--self-test]

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { posix } from "node:path";

// Third-party sources are not ours to fix, and their links point into trees we
// do not carry.
const SKIP = /^vendor\//;

/** The text with fenced code blocks and code spans blanked out, lines kept. */
function withoutCode(text) {
  let fence = null;
  const lines = text.split("\n").map((line) => {
    const open = line.match(/^ {0,3}(`{3,}|~{3,})/);
    if (fence) {
      if (open && open[1][0] === fence[0] && open[1].length >= fence.length)
        fence = null;
      return "";
    }
    if (open) {
      fence = open[1];
      return "";
    }
    return line;
  });
  // A code span is a run of backticks closed by a run of the same length,
  // within one paragraph: it may run onto the next line but never across a
  // blank one. Blanked with its line breaks kept, so that line numbers after
  // it stay true.
  return lines
    .join("\n")
    .replace(/(`+)(?:[^`\n]|\n(?![ \t]*\n))*?\1/g, (m) =>
      m.replace(/[^\n]/g, " "),
    );
}

/** Every link target in `text`, with the line it is on. */
export function linkTargets(text) {
  const out = [],
    plain = withoutCode(text);
  plain.split("\n").forEach((line, i) => {
    const at = i + 1;
    // Inline links and images: [text](target "title") and ![alt](target).
    for (const m of line.matchAll(
      /!?\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)/g,
    ))
      out.push({ target: m[1], line: at });
    // Reference definitions: [id]: target
    const def = line.match(/^ {0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$/);
    if (def) out.push({ target: def[1], line: at });
    // HTML attributes, for the pages that use raw HTML.
    for (const m of line.matchAll(/\b(?:href|src)\s*=\s*"([^"]+)"/g))
      out.push({ target: m[1], line: at });
  });
  return out;
}

/** A heading's text as it renders: markup removed, link text kept. */
function rendered(heading) {
  return heading
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "") // images render no text
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1") // [text](url)
    .replace(/\[([^\]]*)\]\[[^\]]*\]/g, "$1") // [text][ref]
    .replace(/<[^>]+>/g, "") // inline HTML
    .replace(/`([^`]*)`/g, "$1") // code spans keep their text
    .replace(/(\*\*|__|\*|_)(?=\S)([^*_]*?\S)\1/g, "$2"); // emphasis markers
}

/** GitHub's anchor for one heading's rendered text. */
export function slug(text) {
  return text
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, "")
    .replace(/ /g, "-");
}

/** Every anchor a Markdown file offers. */
export function anchors(text) {
  const out = new Set(),
    seen = new Map(),
    lines = withoutCodeKeepingHeadings(text).split("\n");
  const add = (base) => {
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    out.add(n === 0 ? base : `${base}-${n}`);
  };
  lines.forEach((line, i) => {
    const atx = line.match(/^ {0,3}#{1,6}(?:\s+(.*?))?\s*$/);
    if (atx)
      return add(slug(rendered((atx[1] ?? "").replace(/\s+#+\s*$/, ""))));
    // A setext heading: a paragraph line underlined with = or -.
    const prev = lines[i - 1];
    if (
      /^ {0,3}(=+|-+)\s*$/.test(line) &&
      prev &&
      prev.trim() &&
      !/^ {0,3}([#>*+-]|\d+[.)])\s/.test(prev) &&
      !/^ {0,3}(=+|-+)\s*$/.test(prev)
    )
      add(slug(rendered(prev.trim())));
  });
  for (const m of text.matchAll(/\b(?:id|name)\s*=\s*"([^"]+)"/g))
    out.add(m[1]);
  return out;
}

// Headings are read from the text with code removed, but a heading's own code
// spans carry its words: blank only fenced blocks here.
function withoutCodeKeepingHeadings(text) {
  let fence = null;
  return text
    .split("\n")
    .map((line) => {
      const open = line.match(/^ {0,3}(`{3,}|~{3,})/);
      if (fence) {
        if (open && open[1][0] === fence[0] && open[1].length >= fence.length)
          fence = null;
        return "";
      }
      if (open) {
        fence = open[1];
        return "";
      }
      return line;
    })
    .join("\n");
}

/**
 * The broken links in `files`, a Map of tracked path to its text, given every
 * tracked path. Pure, so the self-test can hand it a tree that does not exist.
 */
export function check(files, tracked) {
  const problems = [],
    anchorCache = new Map();
  const isTracked = (p) =>
    tracked.has(p) ||
    [...tracked].some((t) => t.startsWith(`${p.replace(/\/$/, "")}/`));
  const anchorsOf = (p) => {
    if (!anchorCache.has(p)) anchorCache.set(p, anchors(files.get(p) ?? ""));
    return anchorCache.get(p);
  };
  for (const [file, text] of files) {
    for (const { target, line } of linkTargets(text)) {
      if (/^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith("//"))
        continue;
      const hash = target.indexOf("#"),
        rawPath = (hash < 0 ? target : target.slice(0, hash)).split("?")[0],
        anchor = hash < 0 ? null : target.slice(hash + 1);
      let path;
      try {
        path = decodeURIComponent(rawPath);
      } catch {
        path = rawPath;
      }
      const resolved =
        path === ""
          ? file
          : path.startsWith("/")
            ? posix.normalize(path.slice(1))
            : posix.normalize(posix.join(posix.dirname(file), path));
      const where = `${file}:${line}`;
      if (resolved.startsWith("../")) {
        problems.push(`${where}: "${target}" points outside the repository`);
        continue;
      }
      if (!isTracked(resolved)) {
        problems.push(`${where}: "${target}" -- no tracked file ${resolved}`);
        continue;
      }
      if (anchor && resolved.endsWith(".md")) {
        let id;
        try {
          id = decodeURIComponent(anchor);
        } catch {
          id = anchor;
        }
        if (!anchorsOf(resolved).has(id))
          problems.push(
            `${where}: "${target}" -- ${resolved} has no anchor #${id}`,
          );
      }
    }
  }
  return problems;
}

function selfTest() {
  const tree = (entries) => {
    const files = new Map(entries.filter(([p]) => p.endsWith(".md")));
    return { files, tracked: new Set(entries.map(([p]) => p)) };
  };
  const B = `# Guide

## Install \`it\` -- quickly!

## Install \`it\` -- quickly!

<a name="custom"></a>

Under
-----
`;
  // [label, files, how many problems, a word the first must contain]
  const cases = [
    [
      "a link to a tracked file passes",
      [
        ["a.md", "[b](b.md)"],
        ["b.md", B],
      ],
      0,
    ],
    ["a link to a missing file fails", [["a.md", "[c](c.md)"]], 1, "c.md"],
    [
      "a relative path resolves from the file's directory",
      [
        ["d/a.md", "[b](../b.md)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "the same path from the wrong directory fails",
      [
        ["d/a.md", "[b](b.md)"],
        ["b.md", B],
      ],
      1,
      "d/b.md",
    ],
    [
      "a link out of the repository fails",
      [["a.md", "[x](../x.md)"]],
      1,
      "outside",
    ],
    [
      "a directory holding a tracked file passes",
      [
        ["a.md", "[d](d/)"],
        ["d/x.png", ""],
      ],
      0,
    ],
    [
      "a heading anchor passes",
      [
        ["a.md", "[b](b.md#guide)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "code and punctuation drop out of a slug",
      [
        ["a.md", "[b](b.md#install-it----quickly)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "a repeated heading is numbered",
      [
        ["a.md", "[b](b.md#install-it----quickly-1)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "an explicit name is an anchor",
      [
        ["a.md", "[b](b.md#custom)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "a setext heading is an anchor",
      [
        ["a.md", "[b](b.md#under)"],
        ["b.md", B],
      ],
      0,
    ],
    [
      "a missing anchor fails",
      [
        ["a.md", "[b](b.md#nowhere)"],
        ["b.md", B],
      ],
      1,
      "#nowhere",
    ],
    [
      "an anchor in the same file is checked",
      [["a.md", "# Top\n\n[up](#top) [down](#bottom)"]],
      1,
      "#bottom",
    ],
    [
      "an anchor into source is a line, not checked",
      [
        ["a.md", "[s](s.rs#L10)"],
        ["s.rs", ""],
      ],
      0,
    ],
    ["a missing image fails", [["a.md", "![x](img.png)"]], 1, "img.png"],
    // A code span may run onto the next line; blanking it must keep the line
    // break, or every later line number in the file is off by one.
    [
      "a code span across a line break keeps later line numbers",
      [["a.md", "a `code\nspan` b\n\n[x](gone.md)"]],
      1,
      "a.md:4",
    ],
    // A code span never crosses a blank line, so a stray backtick cannot pair
    // with one paragraphs later and blank the links in between.
    [
      "a stray backtick does not hide a link in a later paragraph",
      [["a.md", "one ` stray\n\n[x](gone.md)\n\nand ` another"]],
      1,
      "gone.md",
    ],
    [
      "a reference definition is checked",
      [["a.md", "[x][r]\n\n[r]: gone.md"]],
      1,
      "gone.md",
    ],
    [
      "an HTML src is checked",
      [["a.md", '<img src="gone.png">']],
      1,
      "gone.png",
    ],
    [
      "a URL with a scheme is not followed",
      [["a.md", "[x](https://example.com/gone.md) [m](mailto:a@b.c)"]],
      0,
    ],
    [
      "a link inside a fence is not a link",
      [["a.md", "```\n[x](gone.md)\n```"]],
      0,
    ],
    [
      "a link inside a code span is not a link",
      [["a.md", "see `[x](gone.md)`"]],
      0,
    ],
    [
      "a percent-encoded path is decoded",
      [
        ["a.md", "[x](my%20file.md)"],
        ["my file.md", "# T"],
      ],
      0,
    ],
  ];

  let bad = 0;
  for (const [label, entries, expected, mustSay] of cases) {
    const { files, tracked } = tree(entries);
    const got = check(files, tracked);
    if (got.length !== expected) {
      bad++;
      console.error(
        `  self-test FAILED: ${label} -- expected ${expected}, got ${got.length}: ${got.join("; ")}`,
      );
    } else if (mustSay && !got[0].includes(mustSay)) {
      bad++;
      console.error(
        `  self-test FAILED: ${label} -- no problem mentioned "${mustSay}": ${got[0]}`,
      );
    }
  }
  if (bad > 0) process.exit(1);
  console.log(
    `markdown-links: self-test, ${cases.length} cases, fires on each broken shape and on none of the sound ones`,
  );
}

function main() {
  const tracked = new Set(
    execFileSync("git", ["ls-files"], { encoding: "utf8" })
      .split("\n")
      .filter(Boolean),
  );
  const files = new Map(
    [...tracked]
      .filter((p) => p.endsWith(".md") && !SKIP.test(p))
      .map((p) => [p, readFileSync(p, "utf8")]),
  );
  // Asserted non-empty: a pattern that has stopped matching would otherwise
  // report a clean tree.
  const links = [...files.values()].reduce(
    (n, t) => n + linkTargets(t).length,
    0,
  );
  if (files.size === 0 || links === 0) {
    console.error(
      `markdown-links: read ${files.size} files and ${links} links -- the scan is broken, not the tree.`,
    );
    process.exit(1);
  }
  const problems = check(files, tracked);
  if (problems.length) {
    console.error(
      `markdown-links: ${problems.length} broken in ${files.size} files:\n`,
    );
    for (const p of problems) console.error(`  ${p}`);
    process.exit(1);
  }
  console.log(
    `markdown-links: ${links} links in ${files.size} Markdown files, all resolve`,
  );
}

if (process.argv.includes("--self-test")) selfTest();
else main();
