// @ts-check

//
// A batch of drawing verbs must draw what the same calls draw.
//
// Each verb is declared once and generates two callers: the entry point a
// method call reaches, and the arm that applies a verb decoded from a batch.
// This is the assertion that keeps them the same drawing -- not the same code,
// which the declaration already guarantees, but the same result through two
// paths with different argument handling on the way in.
//
// It reaches the native module directly because the JavaScript side does not
// record batches yet. When it does, this test stays as it is: it is about the
// boundary, not about who writes the buffer.
//

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, Path2D } = require("../../lib"),
  { loadSkiaNode } = require("../../lib/binary.js");

const native = loadSkiaNode(),
  BOXED = Symbol.for("📦"),
  PATH_VERBS = native.Path2D_verbTable(),
  CONTEXT_VERBS = native.CanvasRenderingContext2D_verbTable();

// Every batch carries a values array beside its numbers, for the arguments a
// buffer of doubles cannot hold. These verbs are all numeric, so it is empty.
/** Records `calls` into one buffer, in the layout the decoder reads. */
function record(table, calls) {
  const slots = [];
  for (const [verb, ...args] of calls) {
    const spec = table[verb];
    assert.ok(spec, `${verb} is in the table`);
    assert.equal(args.length, spec.arity, `${verb} takes ${spec.arity} slots`);
    slots.push(spec.op, ...args);
  }
  return new Float64Array(slots);
}

/** The same calls, made one at a time. */
function directly(target, calls) {
  for (const [verb, ...args] of calls) target[verb](...args);
  return target;
}

describe("Batched verbs", () => {
  test("draw the same path as the calls they stand for", () => {
    const calls = [
      ["moveTo", 10, 10],
      ["lineTo", 50, 50],
      ["quadraticCurveTo", 60, 70, 80, 20],
      ["bezierCurveTo", 10, 20, 30, 40, 50, 60],
      ["conicCurveTo", 20, 30, 40, 50, 0.7],
      ["rect", 5, 5, 20, 20],
      ["arcTo", 10, 10, 40, 40, 8],
      // The flag rides in the record as a number, because everything in the
      // buffer is one -- but the call it stands for is given a boolean, since
      // that is the only thing `bool_arg_or` reads. A number there is ignored
      // today, where a browser would take 1 as true; see the note below.
      ["arc", 40, 40, 15, 0, 3, true],
      ["ellipse", 30, 30, 20, 10, 0.5, 0, 6, false],
      ["closePath"],
    ];

    const batched = new Path2D();
    // Booleans are 1 and 0 in the buffer; everything else passes through.
    const buffer = record(
      PATH_VERBS,
      calls.map((call) =>
        call.map((a) => (a === true ? 1 : a === false ? 0 : a)),
      ),
    );
    native.Path2D_plot(batched[BOXED], buffer, buffer.length, []);

    assert.equal(batched.d, directly(new Path2D(), calls).d);
  });

  test("read the flag as a boolean, as the call it stands for does", () => {
    // Worth its own assertion because the two paths disagree about what
    // counts as true. `bool_arg_or` reads a boolean and ignores anything else,
    // so `arc(..., 1)` sweeps clockwise today -- where a browser, converting
    // to a boolean, would take 1 as true. A batch has only numbers, so its 1
    // means true, and a writer on the JavaScript side has to decide which of
    // those two answers it reproduces.
    const drawn = (flag) => {
      const path = new Path2D();
      const buffer = new Float64Array([
        PATH_VERBS.arc.op,
        40,
        40,
        15,
        0,
        3,
        flag,
      ]);
      native.Path2D_plot(path[BOXED], buffer, buffer.length, []);
      return path.d;
    };
    const called = (ccw) => {
      const path = new Path2D();
      path.arc(40, 40, 15, 0, 3, ccw);
      return path.d;
    };

    assert.equal(drawn(1), called(true), "1 in a record is counter-clockwise");
    assert.equal(drawn(0), called(false), "0 is not");
    assert.notEqual(called(1), called(true), "a number is ignored on a call");
  });

  test("draw the same page as the calls they stand for", () => {
    // On a context the verbs move the transform and the current path, so this
    // compares the pixels rather than a path description.
    const calls = [
      ["save"],
      ["translate", 12, 8],
      ["scale", 2, 2],
      ["rotate", 0.4],
      ["beginPath"],
      ["moveTo", 4, 4],
      ["lineTo", 30, 20],
      ["arc", 20, 20, 6, 0, 6, false],
      ["fillRect", 2, 2, 10, 10],
      ["restore"],
      ["fillRect", 40, 40, 20, 20],
    ];

    const shot = (apply) => {
      const canvas = new Canvas(80, 80);
      const ctx = canvas.getContext("2d");
      apply(ctx);
      return canvas.toBufferSync("raw").toString("base64");
    };

    const batched = shot((ctx) => {
      const buffer = record(
        CONTEXT_VERBS,
        calls.map((call) =>
          call.map((a) => (a === true ? 1 : a === false ? 0 : a)),
        ),
      );
      native.CanvasRenderingContext2D_plot(
        ctx[BOXED],
        buffer,
        buffer.length,
        [],
      );
    });

    assert.equal(
      batched,
      shot((ctx) => directly(ctx, calls)),
    );
  });

  test("skip a record carrying a coordinate that cannot be used", () => {
    // The rule a single call follows: a non-finite coordinate makes the call
    // do nothing rather than fail. A batch cannot report it either, so the
    // record is dropped and the ones around it still land.
    const calls = [
      ["moveTo", 0, 0],
      ["lineTo", 20, 20],
      ["lineTo", NaN, 10],
      ["lineTo", Infinity, 10],
      ["lineTo", 40, 5],
    ];

    const batched = new Path2D();
    const buffer = record(PATH_VERBS, calls);
    native.Path2D_plot(batched[BOXED], buffer, buffer.length, []);

    assert.equal(batched.d, directly(new Path2D(), calls).d);
    assert.match(batched.d, /L40 5/, "the record after the bad ones lands");
  });

  test("refuse a buffer that does not decode", () => {
    const path = new Path2D();
    assert.throws(
      () =>
        native.Path2D_plot(path[BOXED], new Float64Array([255, 0, 0]), 3, []),
      /unknown drawing verb 255/,
    );
    assert.throws(
      // `lineTo` promising two numbers and supplying one.
      () =>
        native.Path2D_plot(
          path[BOXED],
          new Float64Array([PATH_VERBS.lineTo.op, 1]),
          2,
          [],
        ),
      /cut short/,
    );
  });

  test("blame the caller when a recorded verb refuses an argument", () => {
    // A recorded verb refuses its arguments from inside `drawlist.js`, so
    // without trimming, the first line of the stack names this library and a
    // caller has to read past it to find their own call. The unrecorded half
    // has always trimmed itself out -- `argc` and `rustError` both do -- and
    // these paths now do the same.
    let canvas = new Canvas(20, 20),
      ctx = canvas.getContext("2d"),
      path = new Path2D();

    // A method reached directly: the caller's own frame is on top.
    for (let [what, call] of [
      ["a method with too few arguments", () => ctx.fillRect(1, 2)],
      ["a path verb with too few arguments", () => path.lineTo(1)],
      ["a radius that cannot be negative", () => ctx.arc(0, 0, -5, 0, 1)],
    ]) {
      let error;
      try {
        call();
      } catch (e) {
        error = e;
      }
      assert.ok(error, `${what} throws`);
      let top = (error.stack || "").split("\n")[1] || "";
      assert.ok(
        !top.includes("drawlist.js"),
        `${what} blames the caller, not the recorder: ${top.trim()}`,
      );
      assert.ok(
        top.includes("verbs.test.js"),
        `${what} names this file: ${top.trim()}`,
      );
    }

    // A property write goes through `RustClass.prop`, so what belongs on top
    // is the accessor the caller assigned to rather than the dispatch behind
    // it -- which is where the unrecorded half points too.
    let error;
    try {
      ctx.lineCap = 5;
    } catch (e) {
      error = e;
    }
    assert.ok(error, "a property given a value it cannot take throws");
    let top = (error.stack || "").split("\n")[1] || "";
    assert.ok(
      !top.includes("drawlist.js") && top.includes("lineCap"),
      `a refused property names the property: ${top.trim()}`,
    );
  });

  test("describe every verb they can apply", () => {
    // The table is what the JavaScript side will generate its writers from, so
    // a verb missing from it is a verb that silently keeps crossing one call
    // at a time.
    for (const [table, expected] of [
      [PATH_VERBS, ["moveTo", "lineTo", "arc", "ellipse", "closePath"]],
      [CONTEXT_VERBS, ["save", "restore", "translate", "fillRect", "lineTo"]],
    ]) {
      for (const verb of expected) {
        assert.ok(table[verb], `${verb} is declared`);
        assert.equal(
          table[verb].args.length + (table[verb].flag ? 1 : 0),
          table[verb].arity,
          `${verb}'s arity counts its arguments and its flag`,
        );
      }
    }
  });
});
