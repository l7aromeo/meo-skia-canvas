#!/usr/bin/env node
// The facts the three READMEs repeat agree with the one place each is set.
//
// The root README, the npm page (docs/readme/npm.md) and the crates.io page
// (docs/readme/crate.md) each state a few facts that live elsewhere: the Node
// and Rust floors, the platform packages, the Cargo features, the `skia-safe`
// and Skia versions, and the Linux symbol ceilings. A copy goes stale in
// silence when its source moves, so each is read from its source here and
// every statement of it is compared:
//
//   Node floor       `engines.node` in package.json
//   Rust floor       `rust-version` in Cargo.toml
//   Cargo features   `[features]` in Cargo.toml, `default` aside
//   skia-safe        its `version` in Cargo.lock; rust-skia names its minor
//                    for the Skia milestone it binds, so 0.156.x is M156
//   platforms        the keys of lib/targets.json
//   Linux ceilings   `MAX_GLIBC` and `MAX_GLIBCXX` in .github/workflows/build.yml
//
// Only the three READMEs are read for facts: the changelogs name the versions
// each release shipped, and docs/ states the platforms in its own words.
//
// A fact a file must state is also required to be there, so that a sentence
// reworded out of the pattern fails rather than passing unread. The crate pin
// is not here: `crate_pin_files` in the justfile names every file holding one,
// and `check-release-guards` compares them.
//
// The npm and crates.io pages are rendered away from the repository, where a
// relative link leads nowhere, so they link absolutely; and an absolute link
// into this repository's main branch is mapped back to the path it names,
// which must be tracked -- a file for `blob/` and `raw.githubusercontent.com`,
// a directory for `tree/`. check-markdown-links checks relative links and
// follows no URL, so without this an absolute link would go unchecked.
// Anchors on absolute links are not checked.
//
// What this does not read: the prose summary of the platforms ("Linux
// (x64/arm64, glibc and musl) ..."), which names them in no fixed form.
//
// Usage:  node scripts/check-readme-facts.mjs [--self-test]

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { linkTargets } from "./check-markdown-links.mjs";

const ROOT = "README.md";
const NPM = "docs/readme/npm.md";
const CRATE = "docs/readme/crate.md";

/** This repository's main branch, as the absolute links spell it. */
const BLOB =
  /^https:\/\/github\.com\/l7aromeo\/meo-skia-canvas\/(blob|tree)\/main\/([^#?]+)/;
const RAW =
  /^https:\/\/raw\.githubusercontent\.com\/l7aromeo\/meo-skia-canvas\/main\/([^#?]+)/;

/** Number words the READMEs count in, so "seven platform packages" is checked. */
const WORDS = [
  "zero",
  "one",
  "two",
  "three",
  "four",
  "five",
  "six",
  "seven",
  "eight",
  "nine",
  "ten",
];

/** `{a,b}-{c,d}` expanded, as a shell would. */
function expand(token) {
  const m = token.match(/\{([^{}]*)\}/);
  if (!m) return [token];
  return m[1]
    .split(",")
    .flatMap((part) =>
      expand(
        token.slice(0, m.index) + part + token.slice(m.index + m[0].length),
      ),
    );
}

/**
 * Each rule: the files that must state the fact, a pattern capturing every
 * statement of it, and the value each capture must equal.
 */
function rules(facts) {
  return [
    {
      name: "Node floor",
      files: [ROOT, NPM],
      pattern: /Requires Node (\d+) or newer/g,
      want: () => facts.node,
    },
    {
      name: "Rust floor",
      files: [ROOT, CRATE],
      pattern: /Requires Rust (\d+\.\d+) or newer/g,
      want: () => facts.rust,
    },
    {
      name: "skia-safe version",
      files: [ROOT, CRATE],
      pattern: /`skia-safe` (\d+\.\d+\.\d+)/g,
      want: () => facts.skiaSafe,
    },
    {
      name: "Skia milestone",
      files: [ROOT, CRATE],
      pattern: /\bSkia(?:\s|\n)*\[?M(\d+)/g,
      want: () => facts.skiaSafe.split(".")[1],
    },
    {
      name: "glibc ceiling",
      files: [ROOT, NPM],
      pattern: /symbol ceilings,\s+glibc (\d+\.\d+)/g,
      want: () => facts.glibc,
    },
    {
      name: "GLIBCXX ceiling",
      files: [ROOT, NPM],
      pattern: /`GLIBCXX` (\d+\.\d+\.\d+)/g,
      want: () => facts.glibcxx,
    },
    {
      name: "platform count",
      files: [NPM],
      pattern: /\b(\w+) platform packages/g,
      want: () => WORDS[facts.targets.length],
    },
  ];
}

/** The line `index` falls on in `text`. */
const lineOf = (text, index) => text.slice(0, index).split("\n").length;

/**
 * The problems with `files`, a Map of path to text, against `facts`. Pure, so
 * the self-test can hand it a tree and facts that do not exist.
 */
export function check(files, facts) {
  const problems = [];
  const readmes = [ROOT, NPM, CRATE].filter((file) => files.has(file));
  for (const rule of rules(facts)) {
    for (const file of readmes) {
      const text = files.get(file);
      const found = [...text.matchAll(rule.pattern)];
      if (found.length === 0 && rule.files.includes(file)) {
        problems.push(`${file}: states no ${rule.name} (want ${rule.want()})`);
      }
      for (const m of found) {
        if (m[1] !== rule.want()) {
          problems.push(
            `${file}:${lineOf(text, m.index)}: ${rule.name} ${m[1]}, but the source says ${rule.want()}`,
          );
        }
      }
    }
  }

  // The platform packages, as the npm page lists them in code spans.
  const npm = files.get(NPM);
  if (npm !== undefined) {
    const line = npm.split("\n").findIndex((l) => /platform packages/.test(l));
    const listed =
      line < 0
        ? []
        : npm
            .split("\n")
            .slice(line, line + 3)
            .join("\n")
            .split(/\n\s*\n/)[0]
            .matchAll(/`([a-z0-9{},-]+)`/g)
            .map((m) => m[1])
            .flatMap(expand)
            .filter((t) => /^(darwin|linux|win32)-/.test(t))
            .toArray();
    const want = [...facts.targets].sort().join(", ");
    const got = [...new Set(listed)].sort().join(", ");
    if (got !== want)
      problems.push(
        `${NPM}: lists platform packages ${got || "none"}, but lib/targets.json has ${want}`,
      );
  }

  // The Cargo features, as the crates.io page's table lists them.
  const crate = files.get(CRATE);
  if (crate !== undefined) {
    const rows = crate
      .matchAll(/^\| `([a-z0-9-]+)` +\|/gm)
      .map((m) => m[1])
      .toArray();
    const want = [...facts.features].sort().join(", ");
    const got = [...rows].sort().join(", ");
    if (got !== want)
      problems.push(
        `${CRATE}: its features table lists ${got || "none"}, but Cargo.toml has ${want}`,
      );
  }

  // A feature named in a manifest snippet exists.
  for (const file of readmes) {
    const text = files.get(file);
    for (const m of text.matchAll(/features = \[([^\]]*)\]/g)) {
      for (const name of m[1].matchAll(/"([^"]+)"/g).map((n) => n[1])) {
        if (!facts.features.includes(name)) {
          problems.push(
            `${file}:${lineOf(text, m.index)}: feature "${name}" is not in Cargo.toml`,
          );
        }
      }
    }
  }

  // Links: absolute into this repository resolve; the package pages hold no relative ones.
  for (const [file, text] of files) {
    for (const { target, line } of linkTargets(text)) {
      const where = `${file}:${line}`;
      const blob = target.match(BLOB);
      const raw = target.match(RAW);
      if (blob || raw) {
        const kind = blob ? blob[1] : "blob";
        const path = decodeURIComponent(
          (blob ? blob[2] : raw[1]).replace(/\/$/, ""),
        );
        const isDir = facts.dirs.has(path);
        if (kind === "tree" ? !isDir : !facts.tracked.has(path)) {
          problems.push(
            `${where}: "${target}" -- no tracked ${kind === "tree" ? "directory" : "file"} ${path}${kind === "blob" && isDir ? " (a directory takes tree/)" : ""}`,
          );
        }
        continue;
      }
      const packagePage = file === NPM || file === CRATE;
      if (packagePage && !/^([a-z][a-z0-9+.-]*:|#)/i.test(target)) {
        problems.push(
          `${where}: "${target}" is relative, and the package page is read away from the repository`,
        );
      }
    }
  }
  return problems;
}

/** Every directory holding a tracked file. */
function directories(tracked) {
  const dirs = new Set();
  for (const path of tracked) {
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++)
      dirs.add(parts.slice(0, i).join("/"));
  }
  return dirs;
}

/** The facts, each read from its one source. */
function readFacts() {
  const pkg = JSON.parse(readFileSync("package.json", "utf8"));
  const node = pkg.engines?.node?.match(/^>=(\d+)$/)?.[1];
  const cargo = readFileSync("Cargo.toml", "utf8");
  const rust = cargo.match(/^rust-version = "(\d+\.\d+)"$/m)?.[1];
  const featureBlock = cargo.match(/^\[features\]\n([\s\S]*?)^\[/m)?.[1] ?? "";
  const features = featureBlock
    .matchAll(/^([a-z0-9-]+) = /gm)
    .map((m) => m[1])
    .filter((name) => name !== "default")
    .toArray();
  const lock = readFileSync("Cargo.lock", "utf8");
  const skiaSafe = lock.match(
    /^name = "skia-safe"\nversion = "(\d+\.\d+\.\d+)"$/m,
  )?.[1];
  const targets = Object.keys(
    JSON.parse(readFileSync("lib/targets.json", "utf8")),
  );
  const build = readFileSync(".github/workflows/build.yml", "utf8");
  const one = (name) => {
    const values = new Set(
      build.matchAll(new RegExp(`\\b${name}=([\\d.]+)`, "g")).map((m) => m[1]),
    );
    return values.size === 1 ? [...values][0] : undefined;
  };
  const tracked = new Set(
    execFileSync("git", ["ls-files"], { encoding: "utf8" })
      .split("\n")
      .filter(Boolean),
  );
  const facts = {
    node,
    rust,
    features,
    skiaSafe,
    targets,
    glibc: one("MAX_GLIBC"),
    glibcxx: one("MAX_GLIBCXX"),
    tracked,
    dirs: directories(tracked),
  };
  // A source read as nothing would make every statement of it wrong for the
  // wrong reason, or, for the lists, pass an empty table.
  const missing = Object.entries(facts).filter(
    ([, v]) => v === undefined || v.length === 0 || v.size === 0,
  );
  if (missing.length) {
    console.error(
      `readme-facts: read nothing for ${missing.map(([k]) => k).join(", ")} -- the reading is broken, not the READMEs.`,
    );
    process.exit(1);
  }
  return facts;
}

function selfTest() {
  const tracked = new Set([
    "README.md",
    "docs/rust.md",
    "examples/rust/a.rs",
    "CHANGELOG-crate.md",
  ]);
  const facts = {
    node: "22",
    rust: "1.90",
    features: ["metal", "vulkan"],
    skiaSafe: "0.156.0",
    targets: ["darwin-arm64", "linux-x64-glibc", "linux-x64-musl"],
    glibc: "2.34",
    glibcxx: "3.4.25",
    tracked,
    dirs: directories(tracked),
  };
  const R = "https://github.com/l7aromeo/meo-skia-canvas";
  const root =
    "Requires Node 22 or newer.\nRequires Rust 1.90 or newer.\n`skia-safe` 0.156.0, Skia M156.\n" +
    "held to symbol ceilings, glibc 2.34 and `GLIBCXX` 3.4.25.\n";
  const npm =
    "Requires Node 22 or newer.\none of three platform packages, `darwin-arm64` and\n`linux-x64-{glibc,musl}`.\n\n" +
    "held to symbol ceilings, glibc 2.34 and `GLIBCXX` 3.4.25.\n" +
    `[docs](${R}/blob/main/docs/rust.md)\n`;
  const crate =
    "Requires Rust 1.90 or newer.\n`skia-safe` 0.156.0, which pins Skia\n[M156](https://skia.googlesource.com/x).\n" +
    '`meo-skia-canvas = { version = "0.16", features = ["vulkan"] }`\n\n' +
    "| Feature | Notes |\n| --- | --- |\n| `vulkan` | V. |\n| `metal` | M. |\n" +
    `[examples](${R}/tree/main/examples/rust)\n`;
  const tree = (edits = {}) =>
    new Map(
      Object.entries({ [ROOT]: root, [NPM]: npm, [CRATE]: crate, ...edits }),
    );
  const swap = (text, a, b) => {
    if (!text.includes(a)) throw new Error(`self-test: "${a}" not in its page`);
    return text.replace(a, b);
  };
  // [label, files, problems wanted]
  const cases = [
    ["every fact right", tree(), 0],
    ["an old Node floor", tree({ [NPM]: swap(npm, "Node 22", "Node 20") }), 1],
    [
      "no Node floor on the npm page",
      tree({ [NPM]: swap(npm, "Requires Node 22 or newer.", "") }),
      1,
    ],
    [
      "an old Rust floor",
      tree({ [CRATE]: swap(crate, "Rust 1.90", "Rust 1.88") }),
      1,
    ],
    [
      "an old skia-safe",
      tree({
        [ROOT]: swap(root, "`skia-safe` 0.156.0", "`skia-safe` 0.153.3"),
      }),
      1,
    ],
    ["an old milestone", tree({ [CRATE]: swap(crate, "[M156]", "[M153]") }), 1],
    [
      "a wrong glibc ceiling",
      tree({ [ROOT]: swap(root, "glibc 2.34", "glibc 2.28") }),
      1,
    ],
    [
      "a wrong GLIBCXX ceiling",
      tree({ [NPM]: swap(npm, "3.4.25", "3.4.29") }),
      1,
    ],
    [
      "a wrong platform count",
      tree({ [NPM]: swap(npm, "three platform", "seven platform") }),
      1,
    ],
    [
      "a platform missing",
      tree({ [NPM]: swap(npm, "{glibc,musl}", "glibc") }),
      1,
    ],
    [
      "a platform too many",
      tree({ [NPM]: swap(npm, "{glibc,musl}", "{glibc,musl,gnu}") }),
      1,
    ],
    [
      "a feature missing from the table",
      tree({ [CRATE]: swap(crate, "| `metal` | M. |\n", "") }),
      1,
    ],
    [
      "a feature the manifest lacks",
      tree({ [CRATE]: swap(crate, '["vulkan"]', '["vulkan", "gl"]') }),
      1,
    ],
    [
      "a blob link to a missing file",
      tree({ [NPM]: swap(npm, "docs/rust.md", "docs/gone.md") }),
      1,
    ],
    [
      "a blob link to a directory",
      tree({
        [NPM]: swap(npm, "blob/main/docs/rust.md", "blob/main/examples/rust"),
      }),
      1,
    ],
    [
      "a tree link to a file",
      tree({
        [CRATE]: swap(
          crate,
          "tree/main/examples/rust",
          "tree/main/docs/rust.md",
        ),
      }),
      1,
    ],
    [
      "a raw link to a tracked file",
      tree({
        [NPM]:
          npm +
          `![x](https://raw.githubusercontent.com/l7aromeo/meo-skia-canvas/main/README.md)\n`,
      }),
      0,
    ],
    [
      "a raw link to a missing file",
      tree({
        [NPM]:
          npm +
          `![x](https://raw.githubusercontent.com/l7aromeo/meo-skia-canvas/main/gone.png)\n`,
      }),
      1,
    ],
    [
      "a relative link on a package page",
      tree({ [CRATE]: crate + "[log](../../CHANGELOG-crate.md)\n" }),
      1,
    ],
    [
      "a relative link in the root README",
      tree({ [ROOT]: root + "[log](CHANGELOG-crate.md)\n" }),
      0,
    ],
    [
      "an anchor on a package page",
      tree({ [CRATE]: crate + "[up](#cargo-features)\n" }),
      0,
    ],
    [
      "a stale fact outside the READMEs",
      new Map([
        ...tree(),
        ["CHANGELOG-crate.md", "`skia-safe` 0.153.3, Skia M153"],
      ]),
      0,
    ],
    [
      "another host is not followed",
      tree({ [NPM]: npm + "[x](https://example.com/gone.md)\n" }),
      0,
    ],
  ];
  const failed = cases.filter(
    ([, files, want]) => check(files, facts).length !== want,
  );
  for (const [name, files, want] of failed) {
    console.error(
      `self-test "${name}": want ${want} problems, got ${JSON.stringify(check(files, facts))}`,
    );
  }
  if (failed.length) process.exit(1);
  console.log(`self-test: ${cases.length} cases`);
}

function main() {
  const facts = readFacts();
  const files = new Map(
    [...facts.tracked, NPM, CRATE]
      .filter(
        (p, i, all) =>
          p.endsWith(".md") && !p.startsWith("vendor/") && all.indexOf(p) === i,
      )
      .map((p) => [p, readFileSync(p, "utf8")]),
  );
  const problems = check(files, facts);
  if (problems.length) {
    console.error(`readme-facts: ${problems.length} problems:\n`);
    for (const p of problems) console.error(`  ${p}`);
    process.exit(1);
  }
  console.log(
    `readme-facts: Node ${facts.node}, Rust ${facts.rust}, skia-safe ${facts.skiaSafe}, ${facts.targets.length} platforms, ` +
      `${facts.features.length} features, glibc ${facts.glibc} / GLIBCXX ${facts.glibcxx} agree across the READMEs`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.includes("--self-test")) selfTest();
  else main();
}
