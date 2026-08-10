// @ts-check

"use strict";

const { readFileSync } = require("fs"),
  { join } = require("path"),
  { assert, describe, test } = require("../runner"),
  runtime = require("../../lib");

const DECLARATIONS = readFileSync(
  join(__dirname, "../../lib/index.d.ts"),
  "utf8",
);

// Names the declaration file introduces in *value* position -- the ones a
// consumer can `new`, call, or test with `instanceof`. Type-only declarations
// (`interface`, `type`) are deliberately excluded: they have no runtime
// counterpart and are not supposed to.
//
// Parsed rather than walked with the compiler API, because TypeScript 7 is the
// Go rewrite and ships no JS API -- `require("typescript")` exposes `version`
// and nothing else. Both forms in use here are matched: `export class|function|
// const` for this package's own API, and the bare `declare var X: { prototype }`
// that the DOM types are written in.
function declaredValues() {
  let names = new Set();

  for (let [, name] of DECLARATIONS.matchAll(
    /^export (?:declare )?(?:class|function|const|let|var) ([A-Za-z_$][\w$]*)/gm,
  )) {
    names.add(name);
  }
  for (let [, name] of DECLARATIONS.matchAll(
    /^declare var ([A-Za-z_$][\w$]*)/gm,
  )) {
    names.add(name);
  }
  return names;
}

// The mechanical form of the rule this package follows: anything declared has
// to exist, and anything that exists has to be declared. Every violation found
// by hand was one of these two -- `DOMPointReadOnly` and `Canvas.contexts`
// declared against nothing, `toFileSync` and `CanvasRenderingContext2D`
// implemented and unreachable from TypeScript.
describe("declarations match the runtime", () => {
  test("the parser finds the exports it is supposed to", () => {
    // Guards the regexes above: if the file is reformatted into a shape they
    // no longer match, the counts collapse and every assertion below passes
    // vacuously.
    let declared = declaredValues();

    assert.ok(
      declared.size > 20,
      `expected the declaration file to declare many values, found ${declared.size}`,
    );
    assert.ok(declared.has("Canvas"), "Canvas should be among them");
    assert.ok(
      declared.has("DOMMatrix"),
      "DOMMatrix, declared in the DOM style, should be among them",
    );
  });

  test("nothing is declared that does not exist", () => {
    let phantoms = [...declaredValues()].filter(
      (name) => runtime[name] === undefined,
    );

    assert.deepStrictEqual(
      phantoms,
      [],
      `declared but missing at runtime: ${phantoms.join(", ")}. ` +
        `Either implement them or drop the declaration -- typechecking clean ` +
        `and then throwing is the worst of both.`,
    );
  });

  test("nothing is exported that is not declared", () => {
    let declared = declaredValues(),
      undeclared = Object.keys(runtime).filter((name) => !declared.has(name));

    assert.deepStrictEqual(
      undeclared,
      [],
      `exported at runtime but undeclared: ${undeclared.join(", ")}. ` +
        `TypeScript consumers cannot reach these.`,
    );
  });
});
