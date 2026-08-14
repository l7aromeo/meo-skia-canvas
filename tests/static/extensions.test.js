// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  {
    OURS,
    OURS_CODE,
    DOM_PATH,
    exportedTypes,
    extensionsOf,
    members,
    stripComments,
    MARK,
  } = require("../support/dom-diff");

// Two views of one file, line for line: `LINES` carries the doc comments a
// marker is written in, `CODE` carries the declarations. Structure is read
// from `CODE` because a doc example holds braces and angle brackets of its
// own, and counting those as declaration syntax truncates whatever follows.
const LINES = OURS.split("\n");
const CODE = OURS_CODE.split("\n");

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

/** Every interface or class the file declares, exported or not. */
function allDeclaredTypes() {
  return [
    ...OURS_CODE.matchAll(
      /^(?:export )?(?:declare )?(?:class|interface) ([A-Za-z_$][\w$]*)/gm,
    ),
  ].map((m) => m[1]);
}

/**
 * Declaration line of a type, whether or not it carries `export`.
 *
 * The `export` prefix used to be required here, which silently excused every
 * type written in the DOM house style -- a bare `interface X` beside a
 * `declare var X`. `bodyOf` then found no body, `lineOfMember` returned -1,
 * and the marking assertions skipped the member instead of failing on it.
 * Path2D's nineteen extensions went unmarked that way.
 */
function lineOfType(name) {
  return CODE.findIndex((l) =>
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

  for (; end < CODE.length; end++) {
    for (let c of CODE[end]) {
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
      ).test(CODE[i])
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
      let start = lineOfType(name);
      if (start < 0) continue;

      for (let i = start; i < CODE.length; i++) {
        if (
          i > start &&
          /^(?:export )?(?:declare )?(?:class|interface) /.test(CODE[i])
        )
          break;
        let match = CODE[i].match(
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

// The scanners above count delimiters to find where a declaration ends. Prose
// contains delimiters too, and a doc comment that swallowed the rest of a type
// would take its members out of every assertion above -- passing them by not
// seeing them, which is the one failure mode a marking test must not have.
describe("declaration scanning", () => {
  const SOURCE = (docs) => `
export interface Sample {
  before: number;
${docs}
  after: number;
  method(arg?: string): void;
}
`;

  const membersOf = (docs) => [...members(SOURCE(docs), "Sample")];

  test("a doc comment cannot hide the members after it", () => {
    const bare = membersOf("");
    assert.deepStrictEqual(bare, ["before", "after", "method"]);

    for (const [what, docs] of [
      // `i < 24` opened a bracket `maskNested` never closed, and everything
      // after it was blanked. This is the one that shipped.
      ["a less-than in an example", "  /** for (let i = 0; i < 24; i++) {} */"],
      // A `}` on its own ends `block`'s brace scan early, cutting the body
      // off at the comment.
      ["an unbalanced closing brace", "  /** like this: } */"],
      ["an unbalanced opening brace", "  /** like this: { */"],
      ["an unbalanced paren", "  /** see foo( for details */"],
      ["a greater-than", "  /** when a > b */"],
      ["a line comment", "  // i < 24 and a stray ( here"],
      [
        "a multi-line block",
        "  /**\n   * for (const i of xs) {\n   *   f(i < n)\n   * }\n   */",
      ],
    ]) {
      assert.deepStrictEqual(membersOf(docs), bare, what);
    }
  });

  test("a string literal is not read as a comment or a bracket", () => {
    // `//` inside a string is not a comment, and `>` inside one is not a
    // closing bracket. Blanking either would take the rest of the type with
    // it.
    const src = `
export interface Sample {
  url: "https://example.test/a";
  compare: "a > b";
  quoted: 'it\\'s fine';
  after: number;
}
`;
    assert.deepStrictEqual(
      [...members(src, "Sample")],
      ["url", "compare", "quoted", "after"],
    );
  });

  test("blanking a comment keeps every line in place", () => {
    // The marking tests read structure from the stripped copy and doc text
    // from the original, by line number. They agree only while stripping
    // changes no line's position or length.
    const raw = SOURCE("  /**\n   * two lines\n   */");
    const code = stripComments(raw);
    assert.equal(code.length, raw.length);
    assert.deepStrictEqual(
      code.split("\n").map((l) => l.length),
      raw.split("\n").map((l) => l.length),
    );
    assert.ok(!code.includes("two lines"), "the prose is gone");
    assert.ok(code.includes("after: number;"), "the code is not");
  });
});
