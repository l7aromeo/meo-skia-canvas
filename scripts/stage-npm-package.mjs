#!/usr/bin/env node
// Puts the npm channel's README and changelog where `npm pack` takes them, and
// checks the generated third-party notices are there, in
// the checkout it is run from. `publish.yml` runs it before publishing and
// `build.yml` before packing the Lambda layer, so the two tarballs cannot
// carry different files.
//
// npm always packs the README.md at the package root, and has no field that
// names one elsewhere; `package.json`'s `files` names `CHANGELOG.md`, which in
// a fresh checkout is the index of the three changelogs. So the npm page,
// docs/readme/npm.md, is copied over the root README -- which is written for
// the repository and links relatively -- and `CHANGELOG-npm.md` is moved over
// the index.
//
// Each destination is then checked against what it must hold, whether or not
// the step before it ran, so that a reordered or dropped line fails here
// rather than shipping the wrong file. The changelog is checked by its H1, the
// one line that differs between the three; the README by its whole text,
// since the root README and the npm page open alike.
//
// Usage:  node scripts/stage-npm-package.mjs

import { copyFileSync, existsSync, readFileSync, renameSync } from "node:fs";

/** The npm changelog's first line. */
const CHANGELOG_H1 = "# Changelog — the npm addon";
const NPM_README = "docs/readme/npm.md";
/** The notices file, and the section every generated one opens with. */
const NOTICES = "THIRD-PARTY-NOTICES.md";
const GENERATED_MARK = "<!-- notice: skia:skia -->";

const fail = (message) => {
  console.error(`::error::${message}`);
  process.exit(1);
};

if (!existsSync("CHANGELOG-npm.md"))
  fail(
    "no CHANGELOG-npm.md here to stage; run this from the package root, once.",
  );
renameSync("CHANGELOG-npm.md", "CHANGELOG.md");
copyFileSync(NPM_README, "README.md");

const h1 = readFileSync("CHANGELOG.md", "utf8").split("\n")[0];
if (h1 !== CHANGELOG_H1)
  fail(`CHANGELOG.md is not the npm changelog: it opens "${h1}"`);
if (readFileSync("README.md", "utf8") !== readFileSync(NPM_README, "utf8"))
  fail(`README.md is not ${NPM_README}`);
// The notices are generated for the binaries this tarball installs, and
// put here by the step before this one: the repository's own
// THIRD-PARTY-NOTICES.md is an account of them, not the texts they owe.
const notices = existsSync(NOTICES) ? readFileSync(NOTICES, "utf8") : "";
if (!notices.includes(GENERATED_MARK))
  fail(
    `${NOTICES} is not the generated notices: run scripts/notices.mjs (package, or binary for one platform's tarball) before staging`,
  );
console.log(
  `staged: CHANGELOG.md carries "${h1}", README.md is ${NPM_README}, ${NOTICES} is generated`,
);
