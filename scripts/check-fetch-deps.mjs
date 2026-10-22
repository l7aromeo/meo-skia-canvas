#!/usr/bin/env node
// The `fetch` feature makes two promises about the dependency graph, and nothing
// else would notice either one breaking.
//
// 1. It is opt-in. Without it, no HTTP client and no TLS stack is linked -- not
//    into a Rust caller's build, and not into the npm addon, which keeps its own
//    loader. A dependency that started pulling ureq or rustls in would add a TLS
//    stack to every build and say nothing.
// 2. With it, TLS is rustls on ring. reqwest's `rustls` feature forces aws-lc,
//    whose build needs CMake and a C compiler that ring does not, and a change
//    to ureq's features or to Cargo's unification could bring it in the same way.
//
// Both are read from `cargo tree --target all`, so a crate that links on one
// platform only is seen from every platform, with every other feature enabled:
// the graph a caller can actually get.

import { execFileSync } from "node:child_process";

// Crates that are an HTTP client or a TLS stack. The first promise is that none
// of them is in the graph without `fetch`.
const NETWORK = [
  "ureq",
  "ureq-proto",
  "rustls",
  "rustls-webpki",
  "ring",
  "webpki-roots",
];

// The prefix of every aws-lc crate: `aws-lc-rs`, `aws-lc-sys`, `aws-lc-fips-sys`.
const AWS_LC = "aws-lc";

// Every feature but `fetch`, and every feature. Read from Cargo.toml rather than
// listed here, so a new feature is covered without editing this file.
function features() {
  const meta = JSON.parse(
    execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
      encoding: "utf8",
    }),
  );
  const pkg = meta.packages.find((p) => p.name === "meo-skia-canvas");
  const all = Object.keys(pkg.features).filter((f) => f !== "default");
  if (!all.includes("fetch"))
    throw Error("Cargo.toml declares no `fetch` feature");
  return { without: all.filter((f) => f !== "fetch"), all };
}

// The crate names in the normal-dependency graph for `features`, with the
// default features on as well: a caller who names no features gets those, so
// a `fetch` made default has to show up here.
function graph(list) {
  const out = execFileSync(
    "cargo",
    [
      "tree",
      "--locked",
      "-e",
      "normal",
      "--target",
      "all",
      "--prefix",
      "none",
      "--format",
      "{p}",
      "--features",
      list.join(","),
    ],
    { encoding: "utf8" },
  );
  return new Set(
    out
      .split("\n")
      .map((line) => line.split(" ")[0])
      .filter(Boolean),
  );
}

// What is wrong with the two graphs, as one line each.
export function violations(without, withFetch) {
  const found = [];
  const leaked = NETWORK.filter((name) => without.has(name));
  if (leaked.length)
    found.push(`without \`fetch\`, the graph holds ${leaked.join(", ")}`);
  for (const name of NETWORK.slice(0, 2))
    if (!withFetch.has(name))
      found.push(
        `with \`fetch\`, the graph has no ${name}: the check is not reading the feature`,
      );
  const awsLc = [...withFetch].filter((name) => name.startsWith(AWS_LC));
  if (awsLc.length)
    found.push(`with \`fetch\`, the graph holds ${awsLc.join(", ")}`);
  return found;
}

function selfTest() {
  const clean = new Set(["skia-safe", "sha2"]);
  const fetch = new Set(["skia-safe", "ureq", "ureq-proto", "rustls", "ring"]);
  const cases = [
    ["sound graphs", clean, fetch, 0],
    ["ring without fetch", new Set([...clean, "ring"]), fetch, 1],
    ["aws-lc-sys with fetch", clean, new Set([...fetch, "aws-lc-sys"]), 1],
    [
      "aws-lc-rs and aws-lc-sys",
      clean,
      new Set([...fetch, "aws-lc-rs", "aws-lc-sys"]),
      1,
    ],
    // A graph read without the feature reports nothing wrong, so a check
    // that lost its features would pass; it must not.
    ["fetch graph lacking ureq", clean, clean, 2],
  ];
  const wrong = cases.filter(([, a, b, n]) => violations(a, b).length !== n);
  if (wrong.length) {
    console.error(
      `check-fetch-deps self-test failed: ${wrong.map(([name]) => name).join(", ")}`,
    );
    process.exit(1);
  }
  return cases.length;
}

const passed = selfTest();
const { without, all } = features();
const found = violations(graph(without), graph(all));
if (found.length) {
  for (const line of found) console.error(`check-fetch-deps: ${line}`);
  process.exit(1);
}
console.log(
  `check-fetch-deps: no ${NETWORK.join("/")} without \`fetch\` (${without.join(",")}), ` +
    `no ${AWS_LC}-* with it, over every target; self-test ${passed} of ${passed}`,
);
