#!/usr/bin/env node
// Points every archived version's page at its equivalent under `latest/`.
//
// Thirteen published references of 164 pages each are, to a search engine, one
// document at thirteen addresses. Left alone that splits the site's standing
// across all of them and can rank a two-year-old page above the current one.
//
// A canonical link rather than `noindex`, because the two answer different
// questions. `noindex` hides the old page; a canonical says "this and that are
// the same document, count that one". Hiding is wrong here: someone searching
// for a symbol as it existed in 5.6 should still find that page. This is what
// versioned documentation sites generally do, and it is the reason `robots.txt`
// no longer disallows the version directories -- a page that is not crawled is
// a page whose canonical is never read, so the two mechanisms cancelled.
//
// WHAT IT WILL NOT DO. A page is left alone when `latest/` has no file at the
// same path. That is the case for a page documenting something since removed,
// and pointing it at a URL that 404s would be worse than leaving it: the
// canonical would be ignored anyway, and the reader following it would land on
// nothing. Those pages stay self-canonical, which is the truthful answer --
// they really are the only copy of themselves.
//
// Idempotent. A page that already carries a canonical is left untouched and
// counted, so a republish rewrites only the version directory just added
// rather than the whole store.
//
// Usage: node canonicalise.mjs <site-dir> <origin>
//        node canonicalise.mjs --self-test

import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";

const VERSION_DIR = /^v\d+\.\d+\.\d+$/;

/// Every `.html` under `dir`, as paths relative to it.
function pages(dir, base = dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...pages(full, base));
    else if (entry.name.endsWith(".html")) out.push(relative(base, full));
  }
  return out;
}

// Inserted immediately after `<head>` rather than before `</head>`: the closing
// tag is optional in HTML and TypeDoc's output is minified enough that matching
// it reliably is more work than matching the opening one, which is always
// present and always first.
const CANONICAL = /<link\s+rel="canonical"/i;

export function inject(html, href) {
  if (CANONICAL.test(html)) return null;
  const at = html.indexOf("<head>");
  if (at === -1) return null;
  const cut = at + "<head>".length;
  return (
    html.slice(0, cut) +
    `<link rel="canonical" href="${href}">` +
    html.slice(cut)
  );
}

function selfTest() {
  const cases = [
    ["<html><head><title>x</title></head>", true, "injects after head"],
    [
      '<html><head><link rel="canonical" href="a"></head>',
      false,
      "leaves a page that already has one",
    ],
    ['<html><head><link REL="Canonical" href="a">', false, "case-insensitive"],
    ["<html><body>no head</body></html>", false, "refuses a page with no head"],
  ];
  let failed = 0;
  for (const [html, expect, what] of cases) {
    const got = inject(html, "https://example.test/latest/x.html") !== null;
    if (got !== expect) {
      failed += 1;
      console.error(
        `  self-test FAILED (${what}): expected ${expect}, got ${got}`,
      );
    }
  }

  // The injected link has to survive being read back, or a second pass would
  // add another one on every republish.
  const once = inject(
    "<html><head><title>x</title></head>",
    "https://e.test/a",
  );
  if (inject(once, "https://e.test/a") !== null) {
    failed += 1;
    console.error("  self-test FAILED: a second pass adds a second canonical");
  }

  // End to end on a real directory, because the walk and the `latest/` lookup
  // are where this can be wrong in a way no string case shows.
  const root = mkdtempSync(join(tmpdir(), "canon-"));
  mkdirSync(join(root, "v1.0.0", "classes"), { recursive: true });
  mkdirSync(join(root, "latest", "classes"), { recursive: true });
  const page = "<html><head><title>t</title></head><body>b</body></html>";
  writeFileSync(join(root, "v1.0.0", "index.html"), page);
  writeFileSync(join(root, "latest", "index.html"), page);
  // Present in the archive and gone from latest: must be left alone.
  writeFileSync(join(root, "v1.0.0", "classes", "Removed.html"), page);
  const result = run(root, "https://e.test", { quiet: true });
  if (result.linked !== 1 || result.orphans !== 1) {
    failed += 1;
    console.error(
      `  self-test FAILED (walk): expected 1 linked and 1 orphan, ` +
        `got ${result.linked} and ${result.orphans}`,
    );
  }
  const written = readFileSync(join(root, "v1.0.0", "index.html"), "utf8");
  if (
    !written.includes('rel="canonical" href="https://e.test/latest/index.html"')
  ) {
    failed += 1;
    console.error(
      "  self-test FAILED: the written href is not the latest/ path",
    );
  }
  if (
    readFileSync(join(root, "v1.0.0", "classes", "Removed.html"), "utf8") !==
    page
  ) {
    failed += 1;
    console.error(
      "  self-test FAILED: a page with no counterpart was rewritten",
    );
  }

  if (failed) {
    console.error(`canonicalise: ${failed} self-test case(s) failed`);
    process.exit(1);
  }
  console.error(
    "canonicalise: self-test, 4 shapes plus a walk, refuses a second pass and " +
      "leaves a page latest/ does not have",
  );
}

function run(site, origin, { quiet = false } = {}) {
  const latest = join(site, "latest");
  if (!existsSync(latest)) {
    console.error("canonicalise: no latest/ directory; nothing to point at");
    process.exit(1);
  }

  const archives = readdirSync(site, { withFileTypes: true })
    .filter((e) => e.isDirectory() && VERSION_DIR.test(e.name))
    .map((e) => e.name);

  let linked = 0,
    already = 0,
    orphans = 0;

  for (const version of archives) {
    for (const page of pages(join(site, version))) {
      const target = join(latest, page);
      if (!existsSync(target)) {
        orphans += 1;
        continue;
      }
      const file = join(site, version, page);
      const html = readFileSync(file, "utf8");
      const href = `${origin}/latest/${page.split(/[\\/]/).join("/")}`;
      const next = inject(html, href);
      if (next === null) {
        already += 1;
        continue;
      }
      writeFileSync(file, next);
      linked += 1;
    }
  }

  if (!quiet) {
    console.error(
      `canonicalise: ${linked} page(s) pointed at latest/, ${already} already ` +
        `carried one, ${orphans} left alone with no counterpart there ` +
        `(${archives.length} archived version(s))`,
    );
  }
  return { linked, already, orphans };
}

if (process.argv[2] === "--self-test") {
  selfTest();
} else {
  const [, , site, origin] = process.argv;
  if (!site || !origin) {
    console.error("Usage: node canonicalise.mjs <site-dir> <origin>");
    process.exit(1);
  }
  selfTest();
  run(site, origin);
}
