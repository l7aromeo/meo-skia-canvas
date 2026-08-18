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
};

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
});
