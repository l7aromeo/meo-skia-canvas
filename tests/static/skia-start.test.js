// @ts-check

"use strict";

// Every raster surface, raster image and codec in src is made through
// src/started.rs, which starts Skia first. `SkGraphics::Init` picks the raster
// pipeline's CPU-specific stages and is process-wide, so a raw constructor
// anywhere else lets code -- a unit test above all -- reach Skia's pixels
// before anything has started it. On x86 that made a test's result depend on
// whether some other test had made a canvas first. This refuses the raw
// constructors outside that one module.

const assert = require("assert");
const { readFileSync, readdirSync, statSync } = require("fs");
const { join, relative, sep } = require("path");
const { describe, test } = require("node:test");

const ROOT = join(__dirname, "..", "..");

/** The one module allowed to call Skia's constructors directly. */
const ALLOWED = join("src", "started.rs");

/** Calls of Skia's raster surface, raster image and codec constructors. */
const RAW_CALLS = [
  "surfaces::raster(",
  "surfaces::raster_n32_premul(",
  "surfaces::wrap_pixels(",
  "images::raster_from_data(",
  "images::raster_from_pixmap(",
  "images::deferred_from_encoded_data(",
  "Codec::from_data(",
];

/** An import of one of them by its bare name, which the calls above miss. */
const RAW_IMPORT =
  /use skia_safe::.*(?:surfaces|images)::\{?[^;]*\b(?:raster|raster_n32_premul|wrap_pixels|raster_from_data|raster_from_pixmap|deferred_from_encoded_data)\b/;

/** Each line of `text` that makes a Skia surface, image or codec raw. */
function rawLines(text) {
  return text
    .split("\n")
    .map((line, i) => ({ line: i + 1, text: line }))
    .filter(
      ({ text }) =>
        !/^\s*\/\//.test(text) &&
        (RAW_CALLS.some((call) => text.includes(call)) ||
          RAW_IMPORT.test(text)),
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

describe("Skia is started before anything reaches it", () => {
  test("the scan finds a raw constructor, and passes a wrapped one", () => {
    // The instrument first: each form it exists to catch, and the forms it
    // must leave alone, so an empty result below means a clean tree rather
    // than a pattern that has stopped matching.
    for (const raw of [
      "    let s = surfaces::raster(&info, None, None);",
      "    let s = skia_safe::surfaces::raster_n32_premul((1, 1));",
      "    let i = images::raster_from_data(&info, data, 4);",
      "    let c = Codec::from_data(data);",
      "use skia_safe::images::raster_from_data;",
      "use skia_safe::surfaces::{raster, wrap_pixels};",
    ])
      assert.equal(rawLines(raw).length, 1, raw);
    for (const fine of [
      "    let s = started::raster(&info, None, None);",
      "/// [`surfaces::raster`], once Skia is started.",
      "    // surfaces::raster(&info, None, None) is what this replaces",
      "use skia_safe::{images, surfaces};",
    ])
      assert.equal(rawLines(fine).length, 0, fine);
  });

  test("nothing in src makes a Skia surface, image or codec raw", () => {
    const files = rustFiles(join(ROOT, "src"));
    assert.ok(files.length > 50, `found only ${files.length} files in src`);
    assert.ok(files.includes(ALLOWED), `${ALLOWED} is the one allowed`);
    const found = files
      .filter((file) => file !== ALLOWED)
      .flatMap((file) =>
        rawLines(readFileSync(join(ROOT, file), "utf8")).map(
          ({ line, text }) =>
            `${file.split(sep).join("/")}:${line}: ${text.trim()}`,
        ),
      );
    assert.deepEqual(
      found,
      [],
      "make these through src/started.rs, which starts Skia first",
    );
  });
});
