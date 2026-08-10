// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  {
    OURS,
    DOM_PATH,
    exportedTypes,
    extensionsOf,
    MARK,
  } = require("../support/dom-diff");

const LINES = OURS.split("\n");

/** The doc comment immediately above `line`, or "" when there is none. */
function docAbove(line) {
  let block = [];
  for (let i = line - 1; i >= 0; i--) {
    let text = LINES[i].trim();
    if (text.endsWith("*/") || text.startsWith("*") || text.startsWith("/**")) {
      block.unshift(text);
      if (text.startsWith("/**")) break;
    } else break;
  }
  return block.join("\n");
}

function lineOfType(name) {
  return LINES.findIndex((l) =>
    new RegExp(`^export (?:declare )?(?:class|interface) ${name}\\b`).test(l),
  );
}

/** Every interface or class the file declares, exported or not. */
function allDeclaredTypes() {
  return [
    ...OURS.matchAll(
      /^(?:export )?(?:declare )?(?:class|interface) ([A-Za-z_$][\w$]*)/gm,
    ),
  ].map((m) => m[1]);
}

function lineOfAnyType(name) {
  return LINES.findIndex((l) =>
    new RegExp(`^(?:export )?(?:declare )?(?:class|interface) ${name}\\b`).test(
      l,
    ),
  );
}

/**
 * Line range of a type's body, so a member is looked up inside its own
 * declaration. Searching the whole file finds the first name that matches
 * anywhere -- `colorType` appears on several options bags long before the one
 * on `Canvas` -- and every such hit is a false report.
 */
function bodyOf(name) {
  let head = lineOfType(name);
  if (head < 0) return null;

  let depth = 0,
    started = false,
    end = head;

  for (; end < LINES.length; end++) {
    for (let c of LINES[end]) {
      if (c === "{") {
        depth++;
        started = true;
      } else if (c === "}") depth--;
    }
    if (started && depth === 0) break;
  }
  return { start: head, end };
}

/** Declaration line of `member` within `name`'s own body. */
function lineOfMember(name, member) {
  let body = bodyOf(name);
  if (!body) return -1;

  for (let i = body.start; i <= body.end; i++) {
    if (
      new RegExp(
        `^\\s+(?:readonly\\s+|static\\s+|get\\s+|set\\s+)*${member}\\s*[(:?<]`,
      ).test(LINES[i])
    ) {
      return i;
    }
  }
  return -1;
}

// docs/api/index.md marks anything beyond the standard with 🧪. The types
// carried the other half of that convention already -- the `[MDN Reference]`
// links -- but nothing said which members are this project's own, so hover
// gave no way to tell a real Canvas method from an extension.
//
// Kept honest by diffing against lib.dom.d.ts rather than by a hand-kept list,
// which would drift the moment someone adds a method.
describe("extension marking", () => {
  test("the DOM reference is available to diff against", () => {
    // Without it every type looks wholly non-standard and the assertions below
    // would pass while checking nothing.
    assert.ok(DOM_PATH, "lib.dom.d.ts should resolve from node_modules");
    assert.ok(exportedTypes().length > 20, "should find the exported types");
  });

  test("every non-standard type is marked", () => {
    let unmarked = exportedTypes().filter((name) => {
      if (!extensionsOf(name).wholeType) return false;
      let line = lineOfType(name);
      return line >= 0 && !docAbove(line).includes(MARK);
    });

    assert.deepStrictEqual(
      unmarked,
      [],
      `types with no browser equivalent and no ${MARK} marker: ${unmarked.join(", ")}`,
    );
  });

  test("every member added to a standard type is marked", () => {
    let unmarked = [];

    for (let name of exportedTypes()) {
      let { wholeType, members } = extensionsOf(name);
      if (wholeType) continue;

      for (let member of members) {
        let line = lineOfMember(name, member);
        if (line >= 0 && !docAbove(line).includes(MARK)) {
          unmarked.push(`${name}.${member}`);
        }
      }
    }

    assert.deepStrictEqual(
      unmarked,
      [],
      `members absent from the browser API and unmarked: ${unmarked.join(", ")}`,
    );
  });

  // The inverse, and the one that catches a marker drifting onto real Canvas
  // API: if a member exists in lib.dom, saying it is an extension is a lie.
  test("nothing standard is marked as an extension", () => {
    let wrong = [];

    // Every interface in the file, not just the exported ones: the context is
    // decomposed into mixins exactly as lib.dom does it, so `fillRect` lives
    // in `CanvasRect` rather than in `CanvasRenderingContext2D`. Checking only
    // exported types never visits them, and a marker landing on real Canvas
    // API would go unnoticed.
    for (let name of allDeclaredTypes()) {
      let { wholeType, members } = extensionsOf(name);
      if (wholeType) continue;

      let extensions = new Set(members);
      let start = lineOfAnyType(name);
      if (start < 0) continue;

      for (let i = start; i < LINES.length; i++) {
        if (
          i > start &&
          /^(?:export )?(?:declare )?(?:class|interface) /.test(LINES[i])
        )
          break;
        let match = LINES[i].match(
          /^\s+(?:readonly\s+|static\s+|get\s+|set\s+)*([A-Za-z_$][\w$]*)\s*[(:?<]/,
        );
        if (!match) continue;

        let member = match[1];
        if (!extensions.has(member) && docAbove(i).includes(MARK)) {
          wrong.push(`${name}.${member}`);
        }
      }
    }

    assert.deepStrictEqual(
      wrong,
      [],
      `marked as extensions but present in lib.dom: ${wrong.join(", ")}`,
    );
  });
});
