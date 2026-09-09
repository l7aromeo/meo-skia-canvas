#!/usr/bin/env node
// A release tag is built from a version in more places than anyone remembers, and every one of
// them has to agree on the prefix. This refuses a tag assembled by hand as `v` plus a version:
// from 6.0.0 the npm channel tags `npm-vX.Y.Z` and the crate tags `rust-vX.Y.Z`, and a bare `v`
// names a tag that does not exist.
//
// Both known defects of this shape were in `build.yml`, cut over from `v` at the rename and
// missed:
//
//   TAG=v$(node -p "require('./package.json').version")
//   PKG_VERSION=v$(cd "$SRC_DIR" && npm pkg get version | tr -d '"')
//
// The upload then failed with "release not found" during the first release cut under the new
// scheme, because nothing had run that path before. The second line is why this cannot key on
// tag-ish words appearing on the same line: it names no tag, assigns to a variable called
// `PKG_VERSION`, and is a tag.
//
// What it keys on instead is the interpolation. A literal `v` immediately followed by an
// expression that names a version is a tag being built; `v${i}` in generated code is an index,
// and `[v${version}]` is a changelog heading, which keeps the bare `v` deliberately and is
// excluded by the bracket and by the word `changelog` on the line.
//
// WHAT THIS CANNOT SEE. Only the tree. The tag shape is also written down in GitHub's own
// settings -- the `github-pages` and `Production` environments each carry a deployment policy
// listing the tag patterns allowed to deploy -- and the 6.0.0 docs deploy was rejected with
// "Tag npm-v6.0.0 is not allowed to deploy to github-pages due to environment protection rules"
// while every file in this repository was correct. No gate reading the repository can catch
// that, and tightening this one will not move the line. When the prefixes change, the
// environments have to be changed by hand:
//
//   gh api repos/<owner>/<repo>/environments/<name>/deployment-branch-policies

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

// The prefixes a constructed tag may carry. `justfile` builds both -- `TAG="npm-v${VERSION}"`
// in `release-npm` and `publish-npm`, `TAG="rust-v${VERSION}"` in `release-crate` -- and
// `NPM_TAG` in `lib/prebuild.mjs` is the same string again for the download path.
const PREFIXES = ["npm-", "rust-"];

// A literal `v` followed immediately by an interpolation, in the three spellings this tree
// uses: `${...}` in shell and in JavaScript templates, `$(...)` for command substitution, and
// `{{ ... }}` for a `just` variable. The character before the `v` is captured so a prefixed
// tag and a changelog heading can be told apart from a bare one.
//
// The command substitution admits one level of nested parentheses, because the defect this
// gate exists for has them: `$(node -p "require('./package.json').version")` closes an inner
// `)` before the word `version`, so a body of `[^)]*` stops short of the only part that
// identifies it as a tag and the case passes. The self-test caught that, which is the whole
// reason the historical defects are in it verbatim rather than paraphrased.
const TAG_BUILD =
  /(.{0,6})v(?:\$\{([^}]*)\}|\$\(((?:[^()]|\([^()]*\))*)\)|\{\{([^}]*)\}\})/g;

// The interpolation has to name a version for this to be a tag rather than an index. Covers
// `${VERSION}`, `${version}`, `$(npm pkg get version)` and `${pkg.version}` alike.
const NAMES_A_VERSION = /version/i;

// A changelog heading is `## ... [v6.0.0] (npm) ...`, bare `v` by design, and the recipes look
// one up by building the same string. Two independent marks, because either alone has a hole:
// the bracket misses `no CHANGELOG entry for v${version}`, and the word misses a heading built
// on a line that does not mention it.
const CHANGELOG_WORD = /changelog/i;

// A comment cannot construct anything. `#` covers YAML, shell and `just`; `//` and `*` cover
// JavaScript. Leading whitespace only -- a `#` mid-line in a shell heredoc is not a comment
// this can recognise, and treating it as one would hide a real construction after it.
const isComment = (line) => /^\s*(?:#|\/\/|\*)/.test(line);

export function findings(text, path) {
  const out = [];
  text.split("\n").forEach((line, i) => {
    if (isComment(line)) return;
    if (CHANGELOG_WORD.test(line)) return;

    for (const m of line.matchAll(TAG_BUILD)) {
      const before = m[1],
        body = m[2] ?? m[3] ?? m[4] ?? "";
      if (!NAMES_A_VERSION.test(body)) continue;
      if (before.endsWith("[")) continue;
      if (PREFIXES.some((p) => before.endsWith(p))) continue;
      out.push({
        path,
        line: i + 1,
        text: line.trim(),
      });
    }
  });
  return out;
}

// Every case states what it must do, so a checker that has stopped refusing anything is visible
// rather than silently green.
//
// They are read from a `.txt` beside this file rather than written here, because half of them
// are hand-built tags by construction and this scan reads its own source: with the cases inline
// the gate refused itself the moment the file became tracked. It passed before that only
// because an untracked file is not in `git ls-files` -- an instrument validated by an accident,
// which is the failure this whole script exists to make harder.
//
// A data file rather than skipping this script's own path. Both are exemptions; this one is
// structural -- the scan reads code and fixtures are not code -- where a named carve-out would
// also hide a genuine construction if one were ever written here.
const CASES = readFileSync(
  new URL("check-release-tags.cases.txt", import.meta.url),
  "utf8",
)
  .split("\n")
  .filter((l) => l.trim() !== "" && !l.startsWith("#"))
  .map((l) => {
    const tab = l.indexOf("\t");
    if (tab === -1) {
      console.error(`release-tags: malformed fixture, no tab: ${l}`);
      process.exit(1);
    }
    return [l.slice(tab + 1), Number(l.slice(0, tab))];
  });

function selfTest() {
  // A fixture file that failed to load, or lost its `must flag` half, leaves a self-test that
  // passes every case it has and proves nothing. It has to contain both answers to be a test at
  // all, so that is asserted rather than assumed.
  const wantFlag = CASES.filter(([, want]) => want > 0).length,
    wantQuiet = CASES.length - wantFlag;
  if (wantFlag === 0 || wantQuiet === 0) {
    console.error(
      `release-tags: fixtures carry ${wantFlag} refusals and ${wantQuiet} passes -- ` +
        `a self-test needs both, so this one cannot fail`,
    );
    process.exit(1);
  }

  let failed = 0;
  for (const [line, want] of CASES) {
    const got = findings(line, "self-test").length;
    if (got !== want) {
      failed += 1;
      console.error(
        `  self-test FAILED: expected ${want}, got ${got} for: ${line}`,
      );
    }
  }
  if (failed) {
    console.error(`release-tags: ${failed} self-test case(s) failed`);
    process.exit(1);
  }

  console.error(
    `release-tags: self-test, ${CASES.length} cases, ` +
      `${wantFlag} hand-built tags refused and ${wantQuiet} passed`,
  );
}

// The tree, minus the files a tag cannot be built in. Read from git rather than by walking the
// filesystem, so an untracked scratch file cannot fail the gate and a tracked one cannot escape
// it.
const SCANNED = /\.(ya?ml|mjs|js|ts|rs)$|(^|\/)justfile$/;

function main() {
  selfTest();

  const files = execFileSync("git", ["ls-files"], { encoding: "utf8" })
    .split("\n")
    .filter((p) => p && SCANNED.test(p));

  // A pattern that has stopped matching returns nothing and reads exactly like a clean tree.
  // The tree is known to contain correct constructions -- `npm-v${VERSION}` in the justfile
  // among them -- so if the scan finds no tag construction at all, the instrument is broken
  // rather than the tree clean.
  let constructions = 0;
  const problems = [];
  for (const path of files) {
    let text;
    try {
      text = readFileSync(path, "utf8");
    } catch {
      continue;
    }
    for (const line of text.split("\n")) {
      if (isComment(line)) continue;
      for (const m of line.matchAll(TAG_BUILD)) {
        const body = m[2] ?? m[3] ?? m[4] ?? "";
        if (NAMES_A_VERSION.test(body)) constructions += 1;
      }
    }
    problems.push(...findings(text, path));
  }

  if (constructions === 0) {
    console.error(
      "release-tags: found no tag construction anywhere, including the ones known to be\n" +
        "  in the justfile -- the scan is broken, not the tree clean",
    );
    process.exit(1);
  }

  if (problems.length) {
    console.error("release tags built by hand as `v` plus a version:\n");
    for (const p of problems) {
      console.error(`  ${p.path}:${p.line}: ${p.text}`);
    }
    console.error(
      `\n  From 6.0.0 the npm channel tags \`npm-vX.Y.Z\` and the crate tags\n` +
        `  \`rust-vX.Y.Z\`. A bare \`v\` names a tag that does not exist, and fails\n` +
        `  at the point of use rather than here -- "release not found" on an upload,\n` +
        `  or a download that 404s.\n`,
    );
    process.exit(1);
  }

  console.error(
    `release-tags: ${constructions} tag constructions across ${files.length} files, ` +
      `every one prefixed`,
  );
}

main();
