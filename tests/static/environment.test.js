// @ts-check

"use strict";

// Nothing in src writes the process environment. A write races every
// concurrent read of it, including a `getenv` inside a C library on a thread
// this addon did not start, and Node has threads of its own running before an
// addon loads, so no point in the addon's life is early enough to make one
// safe. Reading the environment is fine; this refuses the writes.

const assert = require("assert");
const { readFileSync, readdirSync, statSync } = require("fs");
const { join, relative, sep } = require("path");
const { describe, test } = require("node:test");

const ROOT = join(__dirname, "..", "..");

/** A call of Rust's or libc's environment writers. */
const WRITE_CALL = /\b(?:set_var|remove_var|setenv|unsetenv|putenv)\s*\(/;

/** An import of one of Rust's by its bare name, which the call above misses. */
const WRITE_IMPORT = /use std::env::.*\b(?:set_var|remove_var)\b/;

/** Each line of `text` that writes the environment. */
function writeLines(text) {
  return text
    .split("\n")
    .map((line, i) => ({ line: i + 1, text: line }))
    .filter(
      ({ text }) =>
        !/^\s*\/\//.test(text) &&
        (WRITE_CALL.test(text) || WRITE_IMPORT.test(text)),
    );
}

/** Every `.rs` file under `dir`, relative to the repository root. */
function rustFiles(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return rustFiles(path);
    return path.endsWith(".rs") ? [relative(ROOT, path)] : [];
  });
}

describe("nothing writes the process environment", () => {
  test("the scan finds an environment write, and passes a read", () => {
    // The instrument first: each form it exists to catch, and the forms it
    // must leave alone, so an empty result below means a clean tree rather
    // than a pattern that has stopped matching.
    for (const write of [
      '        unsafe { std::env::set_var("FONTCONFIG_PATH", bundled) };',
      '    unsafe { env::remove_var("SKIA_CANVAS_STRICT") };',
      '    unsafe { set_var ("A", "b") };',
      "    libc::setenv(name.as_ptr(), value.as_ptr(), 1);",
      "    libc::unsetenv(name.as_ptr());",
      "    libc::putenv(entry);",
      "use std::env::set_var;",
      "use std::env::{remove_var, var_os};",
    ])
      assert.equal(writeLines(write).length, 1, write);
    for (const fine of [
      '    if let Ok(value) = std::env::var("SKIA_CANVAS_THREADS")',
      '        || std::env::var_os("FONTCONFIG_FILE").is_some();',
      "        face.set_variation(axis, value);",
      "            .set_variation_design_position(position);",
      '    // std::env::set_var("A", "b") would race a getenv',
      "/// `std::env::set_var` races any concurrent `getenv`.",
      "use std::env::var_os;",
    ])
      assert.equal(writeLines(fine).length, 0, fine);
  });

  test("nothing in src writes the process environment", () => {
    const files = rustFiles(join(ROOT, "src"));
    assert.ok(files.length > 50, `found only ${files.length} files in src`);
    const found = files.flatMap((file) =>
      writeLines(readFileSync(join(ROOT, file), "utf8")).map(
        ({ line, text }) =>
          `${file.split(sep).join("/")}:${line}: ${text.trim()}`,
      ),
    );
    assert.deepEqual(found, [], "read the environment; never write it");
  });
});
