// @ts-check

//
// Every declared verb, through both paths, compared.
//
// A verb is declared once in Rust and reaches the drawing two ways: called
// one at a time across the boundary, or recorded into a batch and decoded.
// The declaration makes them the same code; this makes them the same drawing.
//
// It is generated from the table Rust publishes rather than written out, so a
// verb added later is covered the day it is declared -- and a verb added
// without a sample value below fails this test rather than going untested.
//

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, Path2D } = require("../../lib"),
  { loadSkiaNode } = require("../../lib/binary.js");

const native = loadSkiaNode(),
  BOXED = Symbol.for("📦");

// What to pass an argument that is not a number, by the verb that takes it.
// A text argument means something specific -- "round" is a line cap, not a
// colour -- so the values are named here rather than guessed.
const TEXT_VALUES = {
  set_lineCap: ["round"],
  set_lineJoin: ["bevel"],
  set_globalCompositeOperation: ["multiply"],
  set_fillStyleText: ["#3182ce"],
  set_strokeStyleText: ["rgba(20 40 60 / 0.5)"],
  set_shadowColorText: ["#0f0a"],
  set_textAlign: ["center"],
  set_textBaseline: ["middle"],
  set_imageSmoothingQuality: ["low"],
  fillPath2D: ["evenodd"],
  clipPath2D: ["nonzero"],
  set_direction: ["rtl"],
  set_lineDashFit: ["move"],
};

/** A dash pattern, for a verb that takes a list of numbers. */
const sampleNumbers = () => [4, 2, 6];

/** A path with something in it, for a verb that takes one. */
function samplePath() {
  const path = new Path2D();
  path.moveTo(2, 2);
  path.lineTo(30, 24);
  path.lineTo(8, 26);
  path.closePath();
  return path;
}

/** Arguments for `verb`: numbers count up, text comes from the table above. */
function sampleArgs(verb, spec) {
  const texts = TEXT_VALUES[verb] ? [...TEXT_VALUES[verb]] : [];
  const args = spec.args.map((arg, i) => {
    if (arg.kind === "handle") return samplePath();
    if (arg.kind === "numbers") return sampleNumbers();
    if (arg.kind === "text") {
      assert.ok(
        texts.length,
        `${verb} takes a string; add one to TEXT_VALUES in this test`,
      );
      return texts.shift();
    }
    // Small, positive and distinct: positive because a radius may not be
    // negative, distinct so an argument landing in the wrong slot shows.
    return 4 + i * 7;
  });
  if (spec.flag) args.push(true);
  return args;
}

/** The value a record carries for `arg`, and what travels beside it. */
function encode(args, spec, slots) {
  return args.map((value, i) => {
    if (spec.args[i] && spec.args[i].kind === "text")
      return slots.push(value) - 1;
    if (typeof value === "boolean") return value ? 1 : 0;
    return value;
  });
}

describe("The JavaScript/Rust boundary", () => {
  test("draws the same path whether a verb is called or recorded", () => {
    const table = native.Path2D_verbTable();
    assert.ok(Object.keys(table).length >= 10, "the table is published");

    for (const [verb, spec] of Object.entries(table)) {
      const args = sampleArgs(verb, spec);

      // Recorded: the public method, which writes into the batch.
      const recorded = new Path2D();
      recorded.moveTo(1, 1);
      recorded[verb](...args);

      // Called: the exported entry point, reached directly so nothing is
      // batched on the way.
      const called = new Path2D();
      called.moveTo(1, 1);
      called.d; // drain the moveTo before going around the recorder
      native[`Path2D_${verb}`](called[BOXED], ...args);

      assert.equal(recorded.d, called.d, `${verb} draws the same either way`);
    }
  });

  test("draws the same page whether a verb is called or recorded", () => {
    const table = native.CanvasRenderingContext2D_verbTable();
    assert.ok(Object.keys(table).length >= 28, "the table is published");

    for (const [verb, spec] of Object.entries(table)) {
      const args = sampleArgs(verb, spec);
      // A verb declared for the string form of a property is reached through
      // the property itself, which is what a caller writes.
      const property = verb.startsWith("set_") ? verb.slice(4) : null;
      const shot = (apply) => {
        const canvas = new Canvas(60, 60);
        const ctx = canvas.getContext("2d");
        ctx.fillStyle = "#123456";
        apply(ctx);
        // Something to see whatever the verb changed.
        ctx.fillRect(5, 5, 30, 30);
        ctx.beginPath();
        ctx.moveTo(2, 2);
        ctx.lineTo(50, 40);
        ctx.stroke();
        return canvas.toBufferSync("raw").toString("base64");
      };

      // How a caller reaches this verb: a property, one of the wrappers that
      // choose between shapes, or the method of the same name.
      const REACHED_BY = {
        fillPage: (ctx) => ctx.fill(),
        fillPageEvenOdd: (ctx) => ctx.fill("evenodd"),
        strokePage: (ctx) => ctx.stroke(),
        fillPath2D: (ctx, [path, rule]) => ctx.fill(path, rule),
        strokePath2D: (ctx, [path]) => ctx.stroke(path),
        clipPage: (ctx) => ctx.clip(),
        clipPageEvenOdd: (ctx) => ctx.clip("evenodd"),
        clipPath2D: (ctx, [path, rule]) => ctx.clip(path, rule),
        transformNumbers: (ctx, args) => ctx.transform(...args),
        setTransformNumbers: (ctx, args) => ctx.setTransform(...args),
        roundRectUniform: (ctx, args) => ctx.roundRect(...args),
        setLineDash: (ctx, [segments]) => ctx.setLineDash(segments),
      };
      const recorded = shot((ctx) => {
        if (REACHED_BY[verb]) REACHED_BY[verb](ctx, args);
        else if (property) ctx[property.replace(/Text$/, "")] = args[0];
        else ctx[verb](...args);
      });

      const called = shot((ctx) => {
        ctx.lineWidth; // drain anything the setup recorded
        native[`CanvasRenderingContext2D_${verb}`](
          ctx[BOXED],
          ...args.map((a) => (a instanceof Path2D ? a[BOXED] : a)),
        );
      });

      assert.equal(recorded, called, `${verb} draws the same either way`);
    }
  });

  test("refuses what a recorded verb cannot represent", () => {
    // A wrapper that chooses between verbs must not widen what the call
    // accepts. `fill` takes two rules; anything else reaches the hand-written
    // path and is refused there, and recording it instead would turn a typo
    // into a silent winding fill.
    const ctx = new Canvas(20, 20).getContext("2d");
    const path = new Path2D();
    path.rect(0, 0, 5, 5);

    for (const call of [
      () => ctx.fill("bogus"),
      () => ctx.fill(path, "bogus"),
      () => ctx.fill(42),
      () => ctx.fill({}, "nonzero"),
      () => ctx.stroke(42),
    ]) {
      assert.throws(call, TypeError);
    }

    // And the shapes that are representable still work.
    assert.equal(
      undefined,
      ctx.fill(path, "evenodd"),
      "a rule the API defines is taken",
    );
  });

  test("hands over a batch exactly once, in order", () => {
    // Interleaving matters: a verb that cannot be recorded crosses
    // immediately, and it has to land after the recorded ones in front of it
    // rather than jumping the queue.
    const canvas = new Canvas(40, 40);
    const ctx = canvas.getContext("2d");

    ctx.fillStyle = "#ff0000"; // recorded
    ctx.fillRect(0, 0, 40, 40); // recorded
    ctx.fillStyle = "#00ff00"; // recorded
    ctx.save(); // NOT recorded: crosses, so it must drain first
    ctx.fillRect(0, 0, 20, 20); // recorded
    ctx.restore();

    const pixels = canvas.toBufferSync("raw");
    const at = (x, y) => [
      ...pixels.subarray((y * 40 + x) * 4, (y * 40 + x) * 4 + 3),
    ];
    assert.deepEqual(
      at(5, 5),
      [0, 255, 0],
      "the second colour reached the small rect",
    );
    assert.deepEqual(
      at(30, 30),
      [255, 0, 0],
      "the first reached the large one",
    );
  });

  test("keeps a recorded drawing whole when a read interrupts it", () => {
    // A read in the middle of building drains the batch. What follows has to
    // continue the same path rather than start a new one.
    const interrupted = new Path2D();
    interrupted.moveTo(0, 0);
    interrupted.lineTo(10, 10);
    interrupted.bounds; // drains
    interrupted.lineTo(20, 0);
    interrupted.closePath();

    const uninterrupted = new Path2D();
    uninterrupted.moveTo(0, 0);
    uninterrupted.lineTo(10, 10);
    uninterrupted.lineTo(20, 0);
    uninterrupted.closePath();

    assert.equal(interrupted.d, uninterrupted.d);
  });

  test("records nothing for a call it refuses", () => {
    // A refused call must leave the batch as it was: the slots it reserved
    // cannot be left holding whatever was there before.
    const path = new Path2D();
    path.moveTo(0, 0);
    path.lineTo(10, 10);
    assert.throws(
      () => path.arc(5, 5, -1, 0, 3),
      /Radius value must be positive/,
    );
    path.lineTo(20, 20);

    const clean = new Path2D();
    clean.moveTo(0, 0);
    clean.lineTo(10, 10);
    clean.lineTo(20, 20);

    assert.equal(path.d, clean.d, "the refused arc left no trace");
  });

  test("keeps two objects' batches apart", () => {
    // One arena serves every object, so recording into a second one has to
    // hand over the first rather than mixing them.
    const first = new Path2D();
    const second = new Path2D();
    first.moveTo(0, 0);
    second.moveTo(100, 100);
    first.lineTo(10, 10);
    second.lineTo(110, 110);

    assert.equal(first.d, "M0 0L10 10");
    assert.equal(second.d, "M100 100L110 110");
  });

  test("reads what a record points at as it was when the call was made", () => {
    // A record cannot hold a path, so it holds the handle of the `Path2D` and
    // the path is read out when the batch is decoded. Everything between
    // those two moments belongs to the caller, who may draw into that same
    // object again -- and `fill(path)` means the path as it was, not as it
    // ends up.
    const canvas = new Canvas(100, 100);
    const ctx = canvas.getContext("2d");

    const path = new Path2D();
    path.rect(0, 0, 10, 10);
    ctx.fill(path);
    // Neither of these is a recorded verb, so neither reaches the arena of
    // its own accord: both cross straight into Rust and change the path
    // there, while the fill in front of them is still only written down.
    path.addPath(new Path2D("M40 40h20v20h-20Z"));
    path.d = "M0 0h100v100h-100Z";

    const pixels = canvas.toBufferSync("raw");
    const alpha = (x, y) => pixels[(y * 100 + x) * 4 + 3];
    assert.equal(alpha(5, 5), 255, "the rect the fill was given");
    assert.equal(alpha(50, 50), 0, "nothing the path grew afterwards");
  });

  test("reads a dash pattern as it was when the call was made", () => {
    // The same rule for the one kind of value nothing can watch: an array is
    // ordinary JavaScript, so `dashes[1] = 0` crosses nothing that could hand
    // the batch over first. The record keeps a copy rather than the array.
    const canvas = new Canvas(100, 100);
    const ctx = canvas.getContext("2d");
    const dashes = [1, 1000]; // one gap, wider than the line is long
    ctx.setLineDash(dashes);
    dashes[1] = 0; // solid, were the record reading it now

    ctx.lineWidth = 10;
    ctx.beginPath();
    ctx.moveTo(0, 50);
    ctx.lineTo(100, 50);
    ctx.stroke();

    const pixels = canvas.toBufferSync("raw");
    assert.equal(pixels[(50 * 100 + 50) * 4 + 3], 0, "still inside the gap");
  });
});
