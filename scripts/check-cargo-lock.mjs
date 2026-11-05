#!/usr/bin/env node
// Refuses a Cargo.lock whose skia packages lost their registry source.
//
// `node scripts/skia-source.mjs cargo-patch` builds skia-bindings, skia-safe
// and skia-svg-macros from the prepared rust-skia checkout, which carries the
// Graphite bindings patch, and Cargo then drops those packages' `source` and
// `checksum` from Cargo.lock. That lock names a path on one machine: committed,
// it breaks every other checkout and `cargo publish`, which must build against
// crates.io's skia-bindings -- see `graphite_bindings` in build.rs. A build that
// takes the patch restores the lock afterwards; this is what catches one that
// did not.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

/** The packages `cargo-patch` builds from the checkout instead of crates.io. */
export const PATCHED = ["skia-bindings", "skia-safe", "skia-svg-macros"];

/** The source a crates.io package carries in Cargo.lock. */
const REGISTRY =
  'source = "registry+https://github.com/rust-lang/crates.io-index"';

/** The packages of `PATCHED` that `lock` holds without a crates.io source. */
export function unregistered(lock) {
  return lock
    .split("[[package]]")
    .slice(1)
    .map((block) => ({
      name: block.match(/^name = "([^"]+)"$/m)?.[1],
      registry: block.split("\n").some((line) => line.trim() === REGISTRY),
    }))
    .filter(({ name, registry }) => PATCHED.includes(name) && !registry)
    .map(({ name }) => name);
}

// A checker that has never been shown to refuse anything is indistinguishable
// from one that cannot.
function selfTest() {
  const entry = (name, source = true) =>
    `[[package]]\nname = "${name}"\nversion = "0.156.0"\n${source ? `${REGISTRY}\nchecksum = "00"\n` : ""}dependencies = []\n`;
  const cases = [
    [
      PATCHED.map((name) => entry(name)).join("\n"),
      [],
      "every package from crates.io",
    ],
    [
      entry("skia-bindings", false) + entry("skia-safe"),
      ["skia-bindings"],
      "skia-bindings from a path",
    ],
    [
      PATCHED.map((name) => entry(name, false)).join("\n"),
      PATCHED,
      "all three from a path",
    ],
    [entry("skia-bindings-extra", false), [], "a package only named like one"],
  ];
  for (const [lock, want, name] of cases) {
    const got = unregistered(lock);
    if (JSON.stringify(got) !== JSON.stringify(want))
      throw new Error(
        `self-test "${name}": got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`,
      );
  }
  return cases.length;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const checked = selfTest();
  const root = join(dirname(fileURLToPath(import.meta.url)), "..");
  const lock = readFileSync(join(root, "Cargo.lock"), "utf8");
  // The lock must name every package this guards, or a rename upstream would
  // leave it checking nothing.
  const missing = PATCHED.filter((name) => !lock.includes(`name = "${name}"`));
  if (missing.length > 0) {
    console.error(
      `check-cargo-lock: Cargo.lock names no ${missing.join(", ")}; nothing to check`,
    );
    process.exit(1);
  }
  const bad = unregistered(lock);
  if (bad.length > 0) {
    console.error(
      `check-cargo-lock: Cargo.lock holds ${bad.join(", ")} without a crates.io source, as a build ` +
        "with `skia-source.mjs cargo-patch` leaves it. Restore it with `git checkout Cargo.lock`.",
    );
    process.exit(1);
  }
  console.log(
    `cargo-lock: self-test, ${checked} cases; ${PATCHED.join(", ")} all from crates.io`,
  );
}
