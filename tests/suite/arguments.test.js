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

const { execFileSync } = require("child_process"),
  { assert, describe, test } = require("../runner"),
  { Canvas, Path2D } = require("../../lib");

/** Every property that takes a number and nothing else. */
const NUMERIC_PROPERTIES = [
  "lineWidth",
  "miterLimit",
  "lineDashOffset",
  "globalAlpha",
  "shadowBlur",
  "shadowOffsetX",
  "shadowOffsetY",
];

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

  test("refuses a radius below zero, with the type the standard names", () => {
    // `arc`, `arcTo` and `ellipse` throw an `IndexSizeError` DOMException:
    // *"Throws an "IndexSizeError" DOMException if the given radius is
    // negative."* `arcTo` is not in the issue that prompted this, but the
    // standard gives it the same clause and one check serves all three.
    for (const { what, it, has } of targets()) {
      for (const verb of ["arc", "arcTo", "ellipse"]) {
        if (!has(verb)) continue;
        const error = thrown(() =>
          it[verb](...filled(NUMERIC_VERBS[verb], -5)),
        );
        assert.ok(
          error instanceof DOMException,
          `${what}.${verb} negative is a DOMException`,
        );
        assert.equal(error.name, "IndexSizeError", `${what}.${verb} name`);
        assert.match(String(error.message), /Radius value must be positive/);
      }
    }
  });

  test("and roundRect keeps the RangeError its own clause names", () => {
    // Not an oversight and not a candidate for the change above. `roundRect`
    // was added to the standard later and is specified differently: *"If any
    // of the radii are negative, then throw a RangeError."* A plain
    // `RangeError`, where its three siblings throw a DOMException.
    //
    // Pinned because "make the radius errors consistent" is the obvious next
    // edit and it would be wrong: the rule is whatever each operation's own
    // clause names, not one family per concept.
    for (const { what, it, has } of targets()) {
      if (!has("roundRect")) continue;
      const error = thrown(() => it.roundRect(0, 0, 10, 10, -5));
      assert.ok(error instanceof RangeError, `${what}.roundRect negative`);
      assert.ok(
        !(error instanceof DOMException),
        `${what}.roundRect is not a DOMException`,
      );
    }
  });

  test("refuses what JavaScript itself refuses to convert", () => {
    // `+Symbol()` and `+1n` both throw, and a browser canvas throws with
    // them. This binding used to drop the call: `_as_double` returned `None`
    // for a value with no numeric conversion and for one that converted to a
    // non-finite number alike, so an unusable argument was indistinguishable
    // from an ignorable one and every reader gave both the same answer.
    //
    // They are separated now, and the two answers differ because the Canvas
    // API asks for different things. A non-finite coordinate is ignored --
    // pinned by the sibling test below. A value that is not a number at all
    // is a `TypeError`, whatever strict mode says, because strict mode
    // decides whether an *ignorable* value is announced and this one is not
    // ignorable.
    for (const { what, it, has } of targets()) {
      for (const [verb, arity] of Object.entries(NUMERIC_VERBS)) {
        if (!has(verb)) continue;
        for (const [name, value] of [
          ["a symbol", Symbol("s")],
          ["a BigInt", 1n],
        ]) {
          const error = thrown(() => it[verb](...filled(arity, value)));
          assert.ok(
            error instanceof TypeError,
            `${what}.${verb} with ${name} is refused`,
          );
        }
      }
    }
  });

  test("refuses those same values, and ignores the radius", () => {
    // `roundRect` refused these long before its siblings did, and the
    // disagreement that used to be the finding here is gone: the verbs
    // above refuse them now too.
    //
    // Not in JavaScript, though. `lib/classes/context.js` hands `x`, `y`,
    // `w` and `h` to the native call untouched. The `everyFinite` test it
    // makes first only decides whether the call can be recorded, and it
    // answers false for a symbol or a bigint on `typeof` alone, without
    // coercing either -- so what refuses below is the binding's own
    // argument check, which names the argument and its position.
    //
    // The radius is what is left. It takes the other route, through
    // `css.radii`, and is ignored. Kept as a separate assertion rather than
    // folded in, because it is the one argument of this call that a browser
    // and this binding still answer differently.
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

  test("refuses a value where the property takes a string", () => {
    // These have never followed the Canvas API's "ignore what you cannot
    // use" rule -- `string_arg` throws, and has since before any of this was
    // recorded. Pinned because recording them made it possible to lose:
    // a record pointing at a slot with no string in it is dropped by the
    // decoder, which looks exactly like the property being left alone.
    const ctx = new Canvas(100, 100).getContext("2d");
    for (const [property, value] of [
      ["lineCap", 5],
      ["lineJoin", {}],
      ["textAlign", null],
      ["direction", undefined],
      ["globalCompositeOperation", 7],
      ["lineDashFit", []],
      ["imageSmoothingQuality", 0],
    ]) {
      const error = thrown(() => {
        ctx[property] = value;
      });
      assert.ok(error instanceof TypeError, `${property} = ${String(value)}`);
      assert.equal(error.message, `Expected a string for \`${property}\``);
    }
  });

  test("ignores a value where the property parses one first", () => {
    // The other half of the same rule. A property whose CSS is parsed in
    // JavaScript is handed whatever that parse made of it, which for a name
    // the property does not have is nothing at all -- so these are left
    // alone rather than refused, and the recorded verb has to decline the
    // value rather than refuse it.
    const ctx = new Canvas(100, 100).getContext("2d");
    for (const [property, before] of [
      ["fillStyle", "#000000"],
      ["fontStretch", "normal"],
    ]) {
      assert.equal(ctx[property], before, `${property} starts where expected`);
      assert.equal(
        thrown(() => (ctx[property] = "not a value")),
        null,
      );
      assert.equal(ctx[property], before, `${property} was left alone`);
    }
  });

  test("leaves a numeric property alone when it cannot use the value", () => {
    // The Canvas API's rule, and the default here: a property given
    // something it cannot use keeps what it had.
    withStrict(false, () => {
      const ctx = new Canvas(100, 100).getContext("2d");
      for (const property of NUMERIC_PROPERTIES) {
        const before = ctx[property];
        // `null` and the booleans are not in this list: the coercion here
        // reproduces JavaScript's own, where `+null` is 0 and `+true` is 1,
        // so those are numbers the property can use and does.
        for (const value of [NaN, Infinity, -Infinity, "nope", {}, undefined]) {
          assert.equal(
            thrown(() => {
              ctx[property] = value;
            }),
            null,
            `${property} = ${String(value)} threw`,
          );
          assert.equal(ctx[property], before, `${property} moved`);
        }

        // A `Symbol` and a `BigInt` are not in that list: they have no
        // numeric conversion, so assigning one is a `TypeError` here as it
        // is in a browser, where the values above merely fail to be usable
        // and leave the property where it was.
        for (const value of [Symbol("s"), 1n]) {
          const error = thrown(() => {
            ctx[property] = value;
          });
          assert.ok(
            error instanceof TypeError,
            `${property} = ${String(value)} is refused`,
          );
          assert.equal(ctx[property], before, `${property} moved`);
        }
      }
    });
  });

  test("and says so instead when strict mode asks it to", () => {
    // Strict mode is the opt-in "tell me about arguments I got wrong", and
    // it has to answer for all of these or for none: `lineWidth` and
    // `miterLimit` used to be the two that stayed silent while their five
    // neighbours spoke, which is worse than either rule on its own.
    //
    // A second process, because the flag is read when the module loads.
    const script = `
      const { Canvas } = require(${JSON.stringify(require.resolve("../../lib"))});
      const ctx = new Canvas(10, 10).getContext("2d");
      const said = {};
      for (const property of ${JSON.stringify(NUMERIC_PROPERTIES)}) {
        try { ctx[property] = NaN; said[property] = null }
        catch (error) { said[property] = error.message }
      }
      console.log(JSON.stringify(said));
    `;
    const said = JSON.parse(
      execFileSync(process.execPath, ["-e", script], {
        encoding: "utf8",
        env: { ...process.env, SKIA_CANVAS_STRICT: "1" },
      }),
    );
    for (const property of NUMERIC_PROPERTIES) {
      assert.match(
        String(said[property]),
        new RegExp(`Expected a number for \`${property}\``),
        `${property} said nothing`,
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
