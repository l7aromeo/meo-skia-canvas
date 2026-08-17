// @ts-check

//
// What the drawing verbs do with arguments they cannot use.
//
// Nothing pinned this, and it is the contract most exposed by any change to
// how arguments cross into Rust: the coercion is hand-written, the throwing is
// mode-dependent, and most bad input is specified to be *ignored* rather than
// refused. Measured across 28 verbs and 13 categories of bad input, 378 of
// those combinations throw nothing at all, and that silence is as much a
// promise as the errors are.
//
// Rules rather than a snapshot of every message. A snapshot of 351 strings
// pins the wording of errors that are allowed to be reworded, and tells a
// reader nothing about which rule they broke.
//

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, Path2D } = require("../../lib");

// Verb name to the number of arguments it needs, for the ones that take
// nothing but numbers. `fill`, `drawImage`, `setLineDash` and the transform
// family are excluded on purpose: their arguments are paths, images, sequences
// and matrices, so they answer to different rules and have their own tests.
const NUMERIC_VERBS = {
  moveTo: 2,
  lineTo: 2,
  quadraticCurveTo: 4,
  bezierCurveTo: 6,
  conicCurveTo: 5,
  arc: 5,
  arcTo: 5,
  ellipse: 7,
  rect: 4,
  fillRect: 4,
  strokeRect: 4,
  clearRect: 4,
  translate: 2,
  scale: 2,
  rotate: 1,
};

/** The verbs above that exist on a `Path2D` as well as on a context. */
const ON_PATH = new Set([
  "moveTo",
  "lineTo",
  "quadraticCurveTo",
  "bezierCurveTo",
  "conicCurveTo",
  "arc",
  "arcTo",
  "ellipse",
  "rect",
]);

/** Every target a verb can be called on, with the verbs it has. */
function targets() {
  const ctx = new Canvas(100, 100).getContext("2d");
  return [
    { what: "context", it: ctx, has: (verb) => typeof ctx[verb] == "function" },
    { what: "path", it: new Path2D(), has: (verb) => ON_PATH.has(verb) },
  ];
}

/** `n` copies of `value`, as an argument list. */
const filled = (n, value) => Array.from({ length: n }, () => value);

/** What `call` throws, or `null` if it returns. */
function thrown(call) {
  try {
    call();
    return null;
  } catch (error) {
    return error;
  }
}

/** Runs `body` with strict mode forced on or off, restoring it afterwards. */
function withStrict(on, body) {
  // The flag is read once when `lib/classes/neon.js` loads, so this only
  // reaches the checks that consult it live. The Rust side reads the same
  // variable per call, which is the half these assertions are about.
  const was = process.env.SKIA_CANVAS_STRICT;
  process.env.SKIA_CANVAS_STRICT = on ? "1" : "0";
  try {
    body();
  } finally {
    if (was === undefined) delete process.env.SKIA_CANVAS_STRICT;
    else process.env.SKIA_CANVAS_STRICT = was;
  }
}

describe("Arguments", () => {
  test("accepts what it should", () => {
    for (const { what, it, has } of targets()) {
      for (const [verb, arity] of Object.entries(NUMERIC_VERBS)) {
        if (!has(verb)) continue;
        assert.equal(
          thrown(() => it[verb](...filled(arity, 1))),
          null,
          `${what}.${verb} with ${arity} finite numbers`,
        );
        // Strings that name a number are numbers, as `+"5"` is 5. This is not
        // leniency for its own sake -- it is what a browser does, and what the
        // hand-written coercion in `_as_double` exists to reproduce.
        assert.equal(
          thrown(() => it[verb](...filled(arity, "5"))),
          null,
          `${what}.${verb} with numeric strings`,
        );
      }
    }
  });

  test("refuses a call with arguments missing", () => {
    for (const { what, it, has } of targets()) {
      for (const [verb, arity] of Object.entries(NUMERIC_VERBS)) {
        if (!has(verb) || arity === 0) continue;
        const error = thrown(() => it[verb](...filled(arity - 1, 1)));
        assert.ok(error instanceof TypeError, `${what}.${verb} one short`);
        assert.match(String(error.message), /not enough arguments/);
      }
    }
  });

  test("ignores a coordinate it cannot use, rather than refusing it", () => {
    // The Canvas API says a call carrying a non-finite coordinate does
    // nothing. Not an error -- nothing. A drawing that computes a NaN in one
    // frame keeps running, which is why this is the default.
    withStrict(false, () => {
      for (const { what, it, has } of targets()) {
        for (const [verb, arity] of Object.entries(NUMERIC_VERBS)) {
          if (!has(verb)) continue;
          for (const [name, value] of [
            ["NaN", NaN],
            ["Infinity", Infinity],
            ["a word", "nope"],
            ["an object", {}],
          ]) {
            assert.equal(
              thrown(() => it[verb](...filled(arity, value))),
              null,
              `${what}.${verb} with ${name} is a no-op`,
            );
          }
        }
      }
    });
  });

  test("refuses a radius below zero", () => {
    for (const { what, it, has } of targets()) {
      for (const verb of ["arc", "arcTo", "ellipse"]) {
        if (!has(verb)) continue;
        const error = thrown(() =>
          it[verb](...filled(NUMERIC_VERBS[verb], -5)),
        );
        assert.ok(error instanceof RangeError, `${what}.${verb} negative`);
        assert.match(String(error.message), /Radius value must be positive/);
      }
    }
  });

  test("ignores what JavaScript itself refuses to convert", () => {
    // Recorded as it stands rather than as it ought to be. `+Symbol()` and
    // `+1n` both throw in JavaScript, and a browser canvas throws with them --
    // this binding drops the call instead, because the coercion in
    // `_as_double` has no arm for either and an unconvertible argument is
    // indistinguishable from an absent one.
    //
    // Pinned here so that fixing it registers as a deliberate change rather
    // than as a side effect of moving argument handling around.
    for (const { what, it, has } of targets()) {
      for (const [verb, arity] of Object.entries(NUMERIC_VERBS)) {
        if (!has(verb)) continue;
        for (const [name, value] of [
          ["a symbol", Symbol("s")],
          ["a BigInt", 1n],
        ]) {
          assert.equal(
            thrown(() => it[verb](...filled(arity, value))),
            null,
            `${what}.${verb} with ${name} is currently ignored`,
          );
        }
      }
    }
  });

  test("refuses those same values where the coercion happens in JavaScript", () => {
    // `roundRect` coerces its rectangle in `lib/classes/context.js` before
    // anything crosses, so it inherits JavaScript's own conversion and refuses
    // what the verbs above shrug at. Its *radius* argument takes the other
    // route and is ignored, so the same call disagrees with itself depending
    // on which argument is unusable. That is the finding, not the behaviour of
    // either half.
    const ctx = new Canvas(100, 100).getContext("2d");
    for (const value of [Symbol("s"), 1n]) {
      const refused = thrown(() => ctx.roundRect(value, 0, 10, 10));
      assert.ok(refused instanceof TypeError, `roundRect x = ${String(value)}`);
      assert.match(String(refused.message), /Expected a number for `x`/);

      assert.equal(
        thrown(() => ctx.roundRect(0, 0, 10, 10, value)),
        null,
        `roundRect radius = ${String(value)} is ignored`,
      );
    }
  });

  test("draws what a path was given, whatever the arguments looked like", () => {
    // The rules above are about what is refused. This is about what survives:
    // a path built from strings and a path built from numbers are the same
    // path, and a path handed something unusable is unchanged by it.
    const numbers = new Path2D();
    numbers.moveTo(0, 0);
    numbers.lineTo(50, 25);

    const strings = new Path2D();
    strings.moveTo("0", "0");
    strings.lineTo("50", "25");
    assert.equal(strings.d, numbers.d, "numeric strings build the same path");

    const ignored = new Path2D();
    ignored.moveTo(0, 0);
    ignored.lineTo(50, 25);
    ignored.lineTo(NaN, 10);
    ignored.lineTo(Infinity, 10);
    assert.equal(ignored.d, numbers.d, "an unusable segment adds nothing");
  });
});
