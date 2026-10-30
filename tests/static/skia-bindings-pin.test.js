const assert = require("assert");
const { readFileSync } = require("fs");
const { join } = require("path");
const { describe, test } = require("node:test");

const ROOT = join(__dirname, "..", "..");

/** The version requirement Cargo.toml gives a dependency named `name`. */
function required(manifest, name) {
  const line = manifest
    .split("\n")
    .find((line) => new RegExp(`^${name}\\s*=`).test(line));
  assert.ok(line, `Cargo.toml names ${name}`);
  const found = line.match(
    /^[\w-]+\s*=\s*(?:"([^"]+)"|\{[^}]*version\s*=\s*"([^"]+)")/,
  );
  assert.ok(found, `Cargo.toml gives ${name} a version: ${line}`);
  return found[1] ?? found[2];
}

/** The versions Cargo.lock resolves `name` to, one per package entry. */
function locked(lock, name) {
  return [
    ...lock.matchAll(
      new RegExp(
        `\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`,
        "g",
      ),
    ),
  ].map((m) => m[1]);
}

/**
 * Why the two pins disagree, or null when they agree: the requirements
 * Cargo.toml gives skia-safe and skia-bindings, and the versions the lock
 * resolves each to.
 */
function disagreement(manifest, lock) {
  const safe = required(manifest, "skia-safe");
  const bindings = required(manifest, "skia-bindings");
  if (safe !== bindings)
    return `Cargo.toml requires skia-safe ${safe} but skia-bindings ${bindings}`;
  const [lockedSafe, ...moreSafe] = locked(lock, "skia-safe");
  const [lockedBindings, ...moreBindings] = locked(lock, "skia-bindings");
  if (!lockedSafe || !lockedBindings)
    return "Cargo.lock names no skia-safe or no skia-bindings";
  if (moreSafe.length || moreBindings.length)
    return "Cargo.lock holds more than one skia-safe or skia-bindings";
  if (lockedSafe !== lockedBindings)
    return `Cargo.lock resolves skia-safe ${lockedSafe} but skia-bindings ${lockedBindings}`;
  return null;
}

// skia-bindings is a direct dependency only so that build.rs runs after its
// build script, and it has to be the one skia-safe pins with `=`: a different
// version would be a second package linking `skia`, which Cargo refuses with
// an error that names neither this rule nor the Cargo.toml line it lives on.
describe("skia-bindings is pinned where skia-safe is", () => {
  test("in Cargo.toml and Cargo.lock", () => {
    assert.equal(
      disagreement(
        readFileSync(join(ROOT, "Cargo.toml"), "utf8"),
        readFileSync(join(ROOT, "Cargo.lock"), "utf8"),
      ),
      null,
    );
  });

  const lock = (safe, bindings) =>
    `[[package]]\nname = "skia-bindings"\nversion = "${bindings}"\n\n` +
    `[[package]]\nname = "skia-safe"\nversion = "${safe}"\n`;
  const manifest = (safe, bindings) =>
    `[dependencies]\nskia-safe = { version = "${safe}", features = ["svg"] }\n` +
    `skia-bindings = { version = "${bindings}", default-features = false }\n`;

  test("a requirement that moved alone is caught", () => {
    assert.match(
      disagreement(manifest("0.157.0", "0.156.0"), lock("0.156.0", "0.156.0")),
      /requires skia-safe 0\.157\.0 but skia-bindings 0\.156\.0/,
    );
  });

  test("a lock that resolved them apart is caught", () => {
    assert.match(
      disagreement(manifest("0.156.0", "0.156.0"), lock("0.156.1", "0.156.0")),
      /resolves skia-safe 0\.156\.1 but skia-bindings 0\.156\.0/,
    );
  });

  test("a lock that names neither is not taken as agreeing", () => {
    assert.match(
      disagreement(manifest("0.156.0", "0.156.0"), '[[package]]\nname = "x"\n'),
      /names no skia-safe or no skia-bindings/,
    );
  });

  test("a plain version string is read as well as a table", () => {
    const plain = `skia-safe = "0.156.0"\nskia-bindings = "0.156.0"\n`;
    assert.equal(disagreement(plain, lock("0.156.0", "0.156.0")), null);
  });
});
