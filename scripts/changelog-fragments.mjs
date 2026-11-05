// Changelog entries kept as one file per pull request, assembled into the
// changelogs when a release is prepared.
//
// A pull request that reaches a package writes `changelog.d/npm/<n>.md`,
// `changelog.d/crate/<n>.md` or both, `<n>` being its number, rather than
// editing CHANGELOG-npm.md and CHANGELOG-crate.md. Edited directly, those two
// files were shared by almost every pull request, so each merge left the rest
// needing `main` merged in and a fresh CI run before they could merge.
//
// A fragment holds sections and entries in the changelog's own form:
//
//     ### Fixed
//
//     - **A drop shadow keeps its blur's tail.** ([#467](https://github.com/l7aromeo/meo-skia-canvas/pull/467))
//
// A speed change with no change in output joins the release's one entry saying
// what costs less instead of writing a line. It names that entry by the start
// of its title and gives its own links:
//
//     ### Changed
//
//     - Joins **Text costs less** ([#601](https://github.com/l7aromeo/meo-skia-canvas/pull/601))
//
// `assemble <tag>` writes a release's fragments into its channel's changelog
// and removes them: `npm-v7.0.0` for CHANGELOG-npm.md, `rust-v0.17.0` for
// CHANGELOG-crate.md, since the two channels number their releases apart.
// Under the release's heading -- made, if the changelog has none -- the
// hand-written opening stays as it is. Each section takes its fragments'
// entries above the ones it already holds, the highest-numbered fragment
// first, so the newest change leads as the changelogs have it; a section
// missing until now is placed in `SECTIONS` order. A joining fragment's links
// are merged into the one entry of its section whose title starts with the
// words it names, in ascending order with none twice; an entry it cannot find,
// or finds twice, stops the assembly.
//
// `check` reads every fragment and refuses one that is malformed, takes a
// section the changelogs do not have, or joins an entry the release it is for
// does not hold. The shape of each entry is `check-changelog-entries.mjs`'s
// rule, run on the entries in place, so a fragment is held to exactly what the
// changelog is.
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { check as checkEntries } from "./check-changelog-entries.mjs";

/** Each channel's fragment directory, changelog, tag prefix and heading tag. */
export const CHANNELS = {
  npm: { file: "CHANGELOG-npm.md", tag: "npm-v", label: "npm" },
  crate: { file: "CHANGELOG-crate.md", tag: "rust-v", label: "crate" },
};

/** Where the fragments live, a directory per channel beneath it. */
export const FRAGMENTS = "changelog.d";

/**
 * The sections a release can hold, in the order a release lists them: the
 * order every release from npm 7.0.0 and crate 0.17.0 has used.
 */
export const SECTIONS = [
  "Breaking",
  "Security",
  "Fixed",
  "Added",
  "Changed",
  "Known, and not fixed here",
];

/** A fragment's file name: its pull request's number. */
const NAME = /^(\d+)\.md$/;

/** A joining line: `- Joins **<start of a title>** (<links>)`. */
const JOIN = /^- Joins \*\*(.+?)\*\* \((.+)\)$/;

/** One `[#N](url)` link, as an entry carries it. */
const LINK = /\[#(\d+)\]\(([^)]+)\)/g;

/** An entry's closing links: ` ([#a](url), [#b](url))` at the end of its title. */
const TRAILING_LINKS = / \((\[#\d+\]\([^)]+\)(?:, \[#\d+\]\([^)]+\))*)\)$/;

/** The links in `text`, as `{ n, link }` with `link` the whole `[#N](url)`. */
const links = (text) =>
  [...text.matchAll(LINK)].map((m) => ({ n: Number(m[1]), link: m[0] }));

/**
 * The entries of a section's lines: each the title line and any indented
 * lines under it, blank lines dropped.
 */
function entriesOf(lines) {
  const entries = [];
  for (const line of lines) {
    if (line.startsWith("- ")) entries.push([line]);
    else if (line.trim() !== "" && entries.length) entries.at(-1).push(line);
  }
  return entries;
}

/**
 * A fragment's sections, entries and joins, and its problems.
 *
 * Returns `{ sections: Map<section, entry[]>, joins, problems }`, an entry
 * being its lines and a join `{ section, prefix, links, line }`.
 */
export function parseFragment(text) {
  const sections = new Map();
  const joins = [];
  const problems = [];
  let section = null;
  const body = new Map();
  text.split("\n").forEach((raw, index) => {
    const line = index + 1;
    if (raw.startsWith("### ")) {
      section = raw.slice(4).trim();
      if (!SECTIONS.includes(section)) {
        problems.push(
          `line ${line}: "${section}" is not a changelog section (${SECTIONS.join(", ")})`,
        );
      }
      if (body.has(section)) {
        problems.push(`line ${line}: ${section} opens twice`);
      }
      body.set(section, body.get(section) ?? []);
      return;
    }
    if (raw.trim() === "" && section === null) return;
    if (section === null) {
      problems.push(`line ${line}: text before the first section`);
      return;
    }
    const join = raw.match(JOIN);
    if (join) {
      const found = links(join[2]);
      if (found.length === 0) {
        problems.push(`line ${line}: a join gives no links`);
      }
      joins.push({ section, prefix: join[1], links: found, line });
      return;
    }
    if (raw.startsWith("- Joins")) {
      problems.push(
        `line ${line}: a join reads \`- Joins **<start of the title>** (<links>)\``,
      );
      return;
    }
    body.get(section).push({ raw, line });
  });
  for (const [name, lines] of body) {
    sections.set(name, entriesOf(lines.map((l) => l.raw)));
  }
  // The shape rule is the changelog's own, run on the fragment set under a
  // release heading it applies to; its line numbers are moved back to the
  // fragment's. A join stands in as an entry that passes, so that it neither
  // fails the rule nor leaves a gap that reads as a loose list.
  const heading = "## 📦 ⟩ [v999.0.0] (fragment) ⟩ Unreleased\n";
  const passing =
    "- **A join.** ([#1](https://github.com/l7aromeo/meo-skia-canvas/pull/1))";
  const shifted = text
    .split("\n")
    .map((raw) => (JOIN.test(raw) ? passing : raw))
    .join("\n");
  for (const problem of checkEntries(heading + shifted, [0, 0, 0])) {
    problems.push(
      problem.replace(/^line (\d+)/, (_, n) => `line ${Number(n) - 1}`),
    );
  }
  if (sections.size === 0 && joins.length === 0) {
    problems.push("holds no section");
  }
  return { sections, joins, problems };
}

/** The heading line of a channel's release `version`, as written today. */
const headingFor = (channel, version) =>
  `## 📦 ⟩ [v${version}] (${CHANNELS[channel].label}) ⟩ Unreleased`;

/** Whether `line` is the heading of `version` in `channel`, dated or not. */
const isHeading = (line, channel, version) =>
  line.startsWith("## ") &&
  line.includes(`[v${version}] (${CHANNELS[channel].label})`);

/**
 * `text`, a changelog, with `fragments` written into the release `version`.
 *
 * `fragments` are `{ n, text }`. Throws an `Error` naming the problem when a
 * fragment is malformed or a join finds no entry, or two.
 */
export function assemble(text, channel, version, fragments) {
  const lines = text.split("\n");
  let start = lines.findIndex((l) => isHeading(l, channel, version));
  if (start === -1) {
    // A release the changelog has no heading for goes above the newest one.
    const newest = lines.findIndex((l) => l.startsWith("## "));
    const at = newest === -1 ? lines.length : newest;
    lines.splice(at, 0, headingFor(channel, version), "");
    start = at;
  }
  let end = lines.findIndex((l, i) => i > start && l.startsWith("## "));
  if (end === -1) end = lines.length;

  // The release block: the heading, the hand-written opening up to the first
  // section, then the sections.
  const block = lines.slice(start + 1, end);
  const first = block.findIndex((l) => l.startsWith("### "));
  const opening = first === -1 ? block : block.slice(0, first);
  const held = new Map();
  if (first !== -1) {
    let name = null;
    for (const line of block.slice(first)) {
      if (line.startsWith("### ")) {
        name = line.slice(4).trim();
        if (!SECTIONS.includes(name)) {
          throw new Error(
            `${CHANNELS[channel].file}: [v${version}] holds a section "${name}" the assembly does not know`,
          );
        }
        held.set(name, []);
      } else {
        held.get(name).push(line);
      }
    }
  }
  const sections = new Map(
    [...held].map(([name, body]) => [name, entriesOf(body)]),
  );

  const parsed = [...fragments]
    .sort((a, b) => b.n - a.n)
    .map((fragment) => ({ ...fragment, ...parseFragment(fragment.text) }));
  for (const { n, problems } of parsed) {
    if (problems.length) {
      throw new Error(`fragment ${n}: ${problems.join("; ")}`);
    }
  }
  // Fragments above what each section held, the highest number first, and a
  // fragment's own entries in the order it gives them.
  for (const name of SECTIONS) {
    const added = parsed.flatMap((f) => f.sections.get(name) ?? []);
    if (added.length === 0) continue;
    sections.set(name, [...added, ...(sections.get(name) ?? [])]);
  }
  for (const { n, joins } of [...parsed].reverse()) {
    for (const join of joins) {
      const prefix = `- **${join.prefix}`;
      const found = (sections.get(join.section) ?? []).filter((entry) =>
        entry[0].startsWith(prefix),
      );
      if (found.length !== 1) {
        throw new Error(
          `fragment ${n} line ${join.line}: joins "${join.prefix}", which ` +
            (found.length === 0
              ? `no ${join.section} entry of [v${version}] begins with`
              : `${found.length} ${join.section} entries of [v${version}] begin with`),
        );
      }
      const entry = found[0];
      const trailing = entry[0].match(TRAILING_LINKS);
      if (!trailing) {
        throw new Error(
          `fragment ${n} line ${join.line}: the entry it joins does not end in its links`,
        );
      }
      const merged = new Map(
        [...links(trailing[1]), ...join.links].map((l) => [l.n, l.link]),
      );
      const list = [...merged]
        .sort(([a], [b]) => a - b)
        .map(([, link]) => link)
        .join(", ");
      entry[0] = entry[0].slice(0, trailing.index) + ` (${list})`;
    }
  }

  const out = [lines[start], ...opening];
  for (const name of SECTIONS) {
    const entries = sections.get(name);
    if (!entries) continue;
    out.push(`### ${name}`, "", ...entries.flat(), "");
  }
  lines.splice(start, end - start, ...out);
  return lines.join("\n");
}

/** The fragments in `dir`, as `{ n, file, text }`, and the files misnamed. */
export function readFragments(dir) {
  let names;
  try {
    names = readdirSync(dir);
  } catch (error) {
    if (error.code === "ENOENT") return { fragments: [], misnamed: [] };
    throw error;
  }
  const fragments = [];
  const misnamed = [];
  for (const name of names.sort()) {
    const match = name.match(NAME);
    if (!match) {
      misnamed.push(name);
      continue;
    }
    const file = join(dir, name);
    fragments.push({
      n: Number(match[1]),
      file,
      text: readFileSync(file, "utf8"),
    });
  }
  return { fragments, misnamed };
}

/** The channel and version a release tag names, or `null`. */
export function parseTag(tag) {
  for (const [channel, { tag: prefix }] of Object.entries(CHANNELS)) {
    if (tag.startsWith(prefix)) {
      const version = tag.slice(prefix.length);
      if (/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) {
        return { channel, version };
      }
    }
  }
  return null;
}

/** The version of a channel's newest release heading that is not yet dated. */
function unreleased(text, channel) {
  const label = CHANNELS[channel].label;
  const heading = text
    .split("\n")
    .find((l) => l.startsWith("## ") && l.includes(`(${label})`));
  if (!heading || !heading.endsWith("Unreleased")) return null;
  return heading.match(/\[v([^\]]+)\]/)?.[1] ?? null;
}

/**
 * Every problem with the fragments under `root`, as sentences naming the file.
 * A join is resolved against the release its channel's changelog has open, or
 * against the other fragments alone when none is open.
 */
export function checkAll(root) {
  const problems = [];
  for (const [channel, { file }] of Object.entries(CHANNELS)) {
    const dir = join(root, FRAGMENTS, channel);
    const { fragments, misnamed } = readFragments(dir);
    for (const name of misnamed) {
      if (name === "README.md") continue;
      problems.push(
        `${join(FRAGMENTS, channel, name)}: a fragment is named for its pull request, \`<n>.md\``,
      );
    }
    for (const { file: path, text } of fragments) {
      for (const problem of parseFragment(text).problems) {
        problems.push(`${path.slice(root.length + 1)}: ${problem}`);
      }
    }
    if (problems.length || fragments.length === 0) continue;
    const changelog = readFileSync(join(root, file), "utf8");
    const version = unreleased(changelog, channel) ?? "999.0.0";
    try {
      assemble(changelog, channel, version, fragments);
    } catch (error) {
      problems.push(`${join(FRAGMENTS, channel)}: ${error.message}`);
    }
  }
  return problems;
}

/**
 * A release's entries taken back out into fragments: the changelog with
 * `version`'s sections cut to what `held` keeps, and fragments for the rest.
 *
 * Entries are numbered so that newest-first gives back the order they stand
 * in. The release's first Changed entry -- the combined one -- keeps its first
 * link, and each other link comes as a join, numbered so that the joins
 * arrive in descending link order and only the assembly's sort puts them
 * back. `held` is `"none"`, `"half"` (each section keeps its later half, so
 * fragments must land above it) or `"all"` (only the joins are fragments, as
 * when pull requests join an entry the changelog already has).
 */
function takeApart(text, channel, version, held) {
  const lines = text.split("\n");
  const start = lines.findIndex((l) => isHeading(l, channel, version));
  const end = lines.findIndex((l, i) => i > start && l.startsWith("## "));
  const block = lines.slice(start + 1, end);
  const first = block.findIndex((l) => l.startsWith("### "));
  const entries = [];
  let section = null;
  for (const line of block.slice(first)) {
    if (line.startsWith("### ")) section = line.slice(4);
    else if (line.startsWith("- ")) entries.push({ section, lines: [line] });
    else if (line.trim() !== "") entries.at(-1).lines.push(line);
  }
  const combined = entries.find((e) => e.section === "Changed");
  const [, opening, title, list] = combined.lines[0].match(
    /^(- \*\*(.+?)\*\*) \((.+)\)$/,
  );
  const all = list.split(", ");
  const cut = `${opening} (${all[0]})`;
  const keep = new Set();
  if (held === "all") entries.forEach((e) => keep.add(e));
  if (held === "half") {
    for (const name of SECTIONS) {
      const of = entries.filter((e) => e.section === name);
      of.slice(Math.ceil(of.length / 2)).forEach((e) => keep.add(e));
    }
  }
  const kept = [];
  entries.forEach((e, i) => {
    if (!keep.has(e)) return;
    if (!kept.includes(`### ${e.section}`)) kept.push(`### ${e.section}`, "");
    kept.push(...(e === combined ? [cut, ...e.lines.slice(1)] : e.lines));
    if (entries[i + 1]?.section !== e.section) kept.push("");
  });
  const base = [
    ...lines.slice(0, start + 1),
    ...block.slice(0, first),
    ...kept,
    ...lines.slice(end),
  ].join("\n");
  const prefix = title.split(" ").slice(0, 3).join(" ");
  const fragments = all.slice(1).map((link, i) => ({
    n: all.length - i,
    text: `### Changed\n\n- Joins **${prefix}** (${link})\n`,
  }));
  let n = all.length + entries.length + 1;
  for (const e of entries) {
    if (keep.has(e)) continue;
    const lines = e === combined ? [cut, ...e.lines.slice(1)] : e.lines;
    fragments.push({
      n: n--,
      text: `### ${e.section}\n\n${lines.join("\n")}\n`,
    });
  }
  return { base, fragments };
}

/** Cases that must be refused or passed, so a script that stopped refusing shows. */
function selfTest() {
  const here = join(dirname(fileURLToPath(import.meta.url)), "..");
  const failed = [];
  const expect = (name, ok) => {
    if (!ok) failed.push(name);
  };
  const url = (n, kind = "pull") =>
    `[#${n}](https://github.com/l7aromeo/meo-skia-canvas/${kind}/${n})`;

  // The first releases in the one-line form, taken apart into fragments and
  // put back: the assembly has to give back their bytes exactly. Their
  // headings stay when they are dated, so these hold for as long as the files
  // keep them.
  const releases = [
    ["npm", "7.0.0"],
    ["crate", "0.17.0"],
  ];
  let rounds = 0;
  for (const [channel, version] of releases) {
    const text = readFileSync(join(here, CHANNELS[channel].file), "utf8");
    if (!text.split("\n").some((l) => isHeading(l, channel, version))) {
      expect(`${CHANNELS[channel].file} holds [v${version}]`, false);
      continue;
    }
    for (const held of ["none", "half", "all"]) {
      const { base, fragments } = takeApart(text, channel, version, held);
      const out = assemble(base, channel, version, fragments);
      expect(
        `${CHANNELS[channel].file} [v${version}], ${held} held`,
        out === text,
      );
      rounds += 1;
    }
  }

  const refused = (name, text, pattern) => {
    const { problems } = parseFragment(text);
    expect(
      name,
      problems.some((p) => pattern.test(p)),
    );
  };
  const passes = (name, text) => {
    const { problems } = parseFragment(text);
    expect(`${name}: ${problems.join("; ")}`, problems.length === 0);
  };
  passes("an entry", `### Fixed\n\n- **A fill is exact.** (${url(440)})\n`);
  passes(
    "a Breaking entry and its line",
    `### Breaking\n\n- **X is refused.** (${url(311)})\n  Spell it as the standard does.\n`,
  );
  passes(
    "two sections, an entry and a join",
    `### Fixed\n\n- **A fill is exact.** (${url(440)})\n\n### Changed\n\n- Joins **Text costs less** (${url(601)})\n- **Skia M156 to M157.** (${url(602)})\n`,
  );
  refused(
    "a section the changelogs do not have",
    `### Removed\n\n- **X.** (${url(1)})\n`,
    /not a changelog section/,
  );
  refused(
    "text before a section",
    `Hello\n\n### Fixed\n\n- **X.** (${url(1)})\n`,
    /before the first section/,
  );
  refused(
    "an entry linking nothing",
    `### Fixed\n\n- **X is exact.**\n`,
    /links no issue/,
  );
  refused(
    "an entry of two lines",
    `### Fixed\n\n- **X.** (${url(1)})\n  And more.\n`,
    /runs to 2 lines/,
  );
  refused(
    "a loose list",
    `### Fixed\n\n- **X.** (${url(1)})\n\n- **Y.** (${url(2)})\n`,
    /blank line inside/,
  );
  refused(
    "a join without its title in bold",
    `### Changed\n\n- Joins Text costs less (${url(1)})\n`,
    /a join reads/,
  );
  refused(
    "a join giving no links",
    `### Changed\n\n- Joins **Text costs less** (none)\n`,
    /no links/,
  );
  refused("an empty fragment", "\n", /holds no section/);
  refused(
    "a section opened twice",
    `### Fixed\n\n- **X.** (${url(1)})\n\n### Fixed\n\n- **Y.** (${url(2)})\n`,
    /opens twice/,
  );

  const throws = (name, fn, pattern) => {
    try {
      fn();
      expect(name, false);
    } catch (error) {
      expect(`${name}: ${error.message}`, pattern.test(error.message));
    }
  };
  const page = (body) =>
    `# Changelog\n\n${headingFor("npm", "9.0.0")}\n\n${body}\n## 📦 ⟩ [v8.0.0] (npm) ⟩ May 1, 2026\n\n### Fixed\n\n- **Old.** (${url(1)})\n`;
  const speed = `### Changed\n\n- **Text costs less to draw.** (${url(10)})\n- **Text costs less to measure.** (${url(11)})\n`;
  throws(
    "a join finding no entry",
    () =>
      assemble(page(speed), "npm", "9.0.0", [
        {
          n: 20,
          text: `### Changed\n\n- Joins **Paths cost less** (${url(20)})\n`,
        },
      ]),
    /no Changed entry/,
  );
  throws(
    "a join finding two",
    () =>
      assemble(page(speed), "npm", "9.0.0", [
        {
          n: 20,
          text: `### Changed\n\n- Joins **Text costs less** (${url(20)})\n`,
        },
      ]),
    /2 Changed entries/,
  );
  const joined = assemble(page(speed), "npm", "9.0.0", [
    {
      n: 21,
      text: `### Changed\n\n- Joins **Text costs less to draw** (${url(5, "issues")}, ${url(21)})\n`,
    },
  ]);
  expect(
    "a join's links merged in ascending order",
    joined.includes(
      `- **Text costs less to draw.** (${url(5, "issues")}, ${url(10)}, ${url(21)})`,
    ),
  );
  const fresh = assemble(page(""), "npm", "10.0.0", [
    { n: 31, text: `### Fixed\n\n- **B.** (${url(31)})\n` },
    {
      n: 30,
      text: `### Breaking\n\n- **A is refused.** (${url(30)})\n  Write it otherwise.\n`,
    },
  ]);
  expect(
    "a release without a heading gets one above the newest, its sections in order",
    fresh.includes(
      `${headingFor("npm", "10.0.0")}\n\n### Breaking\n\n- **A is refused.** (${url(30)})\n  Write it otherwise.\n\n### Fixed\n\n- **B.** (${url(31)})\n\n${headingFor("npm", "9.0.0")}`,
    ),
  );
  expect(
    "a tag names its channel and version",
    JSON.stringify(parseTag("npm-v7.0.0")) ===
      '{"channel":"npm","version":"7.0.0"}' &&
      JSON.stringify(parseTag("rust-v0.17.0")) ===
        '{"channel":"crate","version":"0.17.0"}' &&
      parseTag("v7.0.0") === null &&
      parseTag("npm-v7") === null,
  );

  // The command line, on a copy of the tree: a misnamed fragment is refused,
  // and an assembly writes the changelog and removes exactly what it used.
  const root = mkdtempSync(join(tmpdir(), "changelog-fragments-"));
  try {
    for (const { file } of Object.values(CHANNELS)) {
      writeFileSync(
        join(root, file),
        page(speed).replaceAll(
          "(npm)",
          file.includes("crate") ? "(crate)" : "(npm)",
        ),
      );
    }
    const dir = join(root, FRAGMENTS, "npm");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "README.md"), "Not a fragment.\n");
    writeFileSync(join(dir, "fix.md"), `### Fixed\n\n- **X.** (${url(1)})\n`);
    writeFileSync(join(dir, "40.md"), `### Fixed\n\n- **C.** (${url(40)})\n`);
    const run = (...args) => {
      try {
        return {
          code: 0,
          out: execFileSync(
            process.execPath,
            [fileURLToPath(import.meta.url), ...args],
            { cwd: root, encoding: "utf8", stdio: "pipe" },
          ),
        };
      } catch (error) {
        return { code: error.status, out: `${error.stdout}${error.stderr}` };
      }
    };
    const misnamed = run("check");
    expect(
      "a misnamed fragment is refused",
      misnamed.code === 1 && /fix\.md/.test(misnamed.out),
    );
    unlinkSync(join(dir, "fix.md"));
    expect("a well-formed set passes check", run("check").code === 0);
    const done = run("assemble", "npm-v9.0.0");
    const written = readFileSync(join(root, CHANNELS.npm.file), "utf8");
    expect(
      "assemble writes the entry and removes the fragment, not the README",
      done.code === 0 &&
        written.includes(`### Fixed\n\n- **C.** (${url(40)})\n`) &&
        !existsSync(join(dir, "40.md")) &&
        existsSync(join(dir, "README.md")),
    );
    expect(
      "a tag naming no channel is refused",
      run("assemble", "7.0.0").code === 2,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }

  if (failed.length) {
    for (const name of failed) console.error(`  self-test FAILED: ${name}`);
    process.exit(1);
  }
  console.log(
    `self-test: ${rounds} round trips of the open releases give back their bytes, and every refusal holds`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [command, tag] = process.argv.slice(2);
  const root = process.cwd();
  if (command === "--self-test") {
    selfTest();
  } else if (command === "check") {
    const problems = checkAll(root);
    for (const problem of problems) console.error(problem);
    if (problems.length) process.exit(1);
    console.log("changelog fragments: every one well-formed, every join found");
  } else if (command === "assemble" && parseTag(tag ?? "")) {
    const { channel, version } = parseTag(tag);
    const { file } = CHANNELS[channel];
    const { fragments, misnamed } = readFragments(
      join(root, FRAGMENTS, channel),
    );
    const stray = misnamed.filter((name) => name !== "README.md");
    if (stray.length) {
      console.error(
        `${join(FRAGMENTS, channel)}: misnamed: ${stray.join(", ")}`,
      );
      process.exit(1);
    }
    const path = join(root, file);
    try {
      writeFileSync(
        path,
        assemble(readFileSync(path, "utf8"), channel, version, fragments),
      );
    } catch (error) {
      console.error(error.message);
      process.exit(1);
    }
    for (const { file: used } of fragments) unlinkSync(used);
    console.log(
      `${file}: [v${version}] takes ${fragments.length} fragment${fragments.length === 1 ? "" : "s"}, now removed`,
    );
  } else {
    console.error(
      "usage: node scripts/changelog-fragments.mjs check\n" +
        "       node scripts/changelog-fragments.mjs assemble <npm-vX.Y.Z | rust-vX.Y.Z>",
    );
    process.exit(2);
  }
}
