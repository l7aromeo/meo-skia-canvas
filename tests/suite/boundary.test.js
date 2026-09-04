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

const { execFileSync } = require("child_process"),
  { assert, describe, test } = require("../runner"),
  { Canvas, Image, Path2D } = require("../../lib"),
  { CanvasRenderingContext2D: Ctx } = require("../../lib/classes/context"),
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
  set_fontStretchText: ["condensed"],
  fillTextAt: ["Wg"],
  fillTextIn: ["Wg"],
  strokeTextAt: ["Wg"],
  strokeTextIn: ["Wg"],
};

/** A dash pattern, for a verb that takes a list of numbers. */
const sampleNumbers = () => [4, 2, 6];

/** Something to paint, for a verb that takes an image.
 *
 * Built once and shared: the two paths have to be handed the same pixels,
 * and encoding a PNG per argument would say nothing extra.
 */
let sample;
function sampleImage() {
  if (!sample) {
    const canvas = new Canvas(64, 64);
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#c05621";
    ctx.fillRect(0, 0, 64, 64);
    ctx.fillStyle = "#2b6cb0";
    ctx.fillRect(8, 8, 24, 40); // off-centre, so a crop is not symmetric
    sample = new Image(canvas.toBufferSync("png"));
    assert.ok(sample.complete, "the sample image decoded");
  }
  return sample;
}

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
    if (arg.kind === "image") return sampleImage();
    if (arg.kind === "numbers") return sampleNumbers();
    if (arg.kind === "text") {
      assert.ok(
        texts.length,
        `${verb} takes a string; add one to TEXT_VALUES in this test`,
      );
      return texts.shift();
    }
    assert.ok(
      arg.kind === "" || arg.kind === "wide" || arg.kind === "non_negative",
      `${verb} takes a \`${arg.kind}\` argument; teach this test what to pass one`,
    );
    // Small, positive and distinct: positive because a radius may not be
    // negative, distinct so an argument landing in the wrong slot shows.
    return 4 + i * 7;
  });
  if (spec.flag) args.push(true);
  return args;
}

describe("The JavaScript/Rust boundary", () => {
  test("draws the same path whether a verb is called or recorded", () => {
    const table = native.Path2D_verbTable();
    assert.ok(Object.keys(table).length >= 10, "the table is published");

    for (const [verb, spec] of Object.entries(table)) {
      const args = sampleArgs(verb, spec);

      // How a caller reaches this verb, where that is not a method of the
      // same name: a wrapper choosing between shapes.
      const REACHED_BY = {
        appendPath: (path, [other]) => path.addPath(other),
        roundRectUniform: (path, at) => path.roundRect(...at),
      };

      // Recorded: the public method, which writes into the batch.
      const recorded = new Path2D();
      recorded.moveTo(1, 1);
      if (REACHED_BY[verb]) REACHED_BY[verb](recorded, args);
      else recorded[verb](...args);

      // Called: the exported entry point, reached directly so nothing is
      // batched on the way.
      const called = new Path2D();
      called.moveTo(1, 1);
      called.d; // drain the moveTo before going around the recorder
      native[`Path2D_${verb}`](
        called[BOXED],
        ...args.map((a) => (a instanceof Path2D ? a[BOXED] : a)),
      );

      assert.equal(recorded.d, called.d, `${verb} draws the same either way`);
    }
  });

  test("draws the same page whether a verb is called or recorded", () => {
    const table = native.CanvasRenderingContext2D_verbTable();
    assert.ok(Object.keys(table).length >= 28, "the table is published");

    const ctx0 = new Canvas(1, 1).getContext("2d");
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
        drawImageAt: (ctx, [image, ...at]) => ctx.drawImage(image, ...at),
        drawImageIn: (ctx, [image, ...at]) => ctx.drawImage(image, ...at),
        drawImageCropped: (ctx, [image, ...at]) => ctx.drawImage(image, ...at),
        fillTextAt: (ctx, [text, ...at]) => ctx.fillText(text, ...at),
        fillTextIn: (ctx, [text, ...at]) => ctx.fillText(text, ...at),
        strokeTextAt: (ctx, [text, ...at]) => ctx.strokeText(text, ...at),
        strokeTextIn: (ctx, [text, ...at]) => ctx.strokeText(text, ...at),
        saveLayerAlpha: (ctx, [alpha]) => ctx.saveLayer(alpha),
      };
      // A verb the class does not declare is reached by a wrapper, and the
      // wrapper is where its arguments are checked -- so a verb with neither
      // a method of its own nor an entry above has no route a caller could
      // take, and nothing would be testing the one they do take.
      assert.ok(
        REACHED_BY[verb] || property || typeof ctx0[verb] === "function",
        `${verb} has no public route; add one to REACHED_BY in this test`,
      );

      const recorded = shot((ctx) => {
        if (REACHED_BY[verb]) REACHED_BY[verb](ctx, args);
        else if (property) ctx[property.replace(/Text$/, "")] = args[0];
        else ctx[verb](...args);
      });

      const called = shot((ctx) => {
        ctx.lineWidth; // drain anything the setup recorded
        native[`CanvasRenderingContext2D_${verb}`](
          ctx[BOXED],
          ...args.map((a) =>
            a instanceof Path2D || a instanceof Image ? a[BOXED] : a,
          ),
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

  test("leaves a text shape a record cannot hold to the call", () => {
    // `fillText(text, x, y, undefined)` is the call treating a fourth
    // argument as absent. A record cannot: an unusable number makes the
    // decoder drop the whole record, so the text would not be drawn at all
    // rather than drawn unbounded. The wrapper has to send that shape the
    // long way, and this is what says it still does.
    const shot = (apply) => {
      const canvas = new Canvas(120, 60);
      const ctx = canvas.getContext("2d");
      ctx.font = "20px Helvetica";
      apply(ctx);
      return canvas.toBufferSync("raw").reduce((sum, byte) => sum + byte, 0);
    };
    const unbounded = shot((ctx) => ctx.fillText("Wg", 4, 40));
    assert.ok(unbounded > 0, "there is text to compare");
    assert.equal(
      shot((ctx) => ctx.fillText("Wg", 4, 40, undefined)),
      unbounded,
      "an undefined width draws what no width draws",
    );
    assert.notEqual(
      shot((ctx) => ctx.fillText("Wg", 4, 40, 12)),
      unbounded,
      "and a width that is a number still narrows it",
    );
  });

  test("says the same thing either way when strict mode is on", () => {
    // Strict mode is read once, when `lib/classes/neon.js` loads, and the
    // generated writers bake it in when they are made -- so this cannot be
    // switched on from inside a test and has to be a second process.
    //
    // What it is for: Rust marks a message it only wants raised in strict
    // mode, and the mark is taken off on the way out. A recorded verb raises
    // the message itself and never had one to take off, so the two agreed
    // only as long as nobody looked at the string.
    const script = `
      const { Canvas } = require(${JSON.stringify(require.resolve("../../lib"))});
      const said = (apply) => {
        try { apply(new Canvas(60, 60).getContext("2d")); return null }
        catch (error) { return error.message }
      };
      console.log(JSON.stringify({
        recorded: said((ctx) => ctx.fillText("Wg", NaN, 40)),
        called: said((ctx) => ctx.fillText("Wg", NaN, 40, undefined)),
        rect: said((ctx) => ctx.fillRect(0, NaN, 10, 10)),
      }));
    `;
    const said = JSON.parse(
      execFileSync(process.execPath, ["-e", script], {
        encoding: "utf8",
        env: { ...process.env, SKIA_CANVAS_STRICT: "1" },
      }),
    );
    assert.equal(
      said.recorded,
      "Expected a number for `x` as 2nd arg",
      "no marker survives into what a caller reads",
    );
    assert.equal(said.called, said.recorded, "the call says the same thing");
    assert.match(said.rect, /^Expected a number for `y`/);
  });

  test("reads the object doing the recording as its own queue leaves it", () => {
    // The batch lands before any of it is applied, so a record pointing at
    // the object that is recording would read it as it was before its own
    // queue -- which is not "as it was when the call was made", it is
    // earlier than that. Both shapes it takes are here.
    const canvas = new Canvas(80, 40);
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#ff0000";
    ctx.fillRect(0, 0, 40, 40); // recorded, still pending
    ctx.drawImage(canvas, 40, 0); // has to copy the red, not the blank

    const pixels = canvas.toBufferSync("raw");
    const at = (x, y) => [
      ...pixels.subarray((y * 80 + x) * 4, (y * 80 + x) * 4 + 4),
    ];
    assert.deepEqual(
      at(50, 20),
      [255, 0, 0, 255],
      "the copy has the rect in it",
    );

    const path = new Path2D();
    path.moveTo(0, 0);
    path.lineTo(10, 10); // recorded, still pending
    path.addPath(path); // has to append what is queued, not nothing

    const doubled = new Path2D();
    doubled.moveTo(0, 0);
    doubled.lineTo(10, 10);
    doubled.bounds; // drain, so the append below is not the case under test
    const twin = new Path2D();
    twin.moveTo(0, 0);
    twin.lineTo(10, 10);
    doubled.addPath(twin);
    assert.equal(path.d, doubled.d, "a path added to itself doubled");
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

  test("reads an image as it was when the call was made", async () => {
    // An `Image` is the one object in a batch that does not drain when it is
    // read: a sprite loop asks every one of them for `complete`, and
    // draining there would end the batch on every call. It drains where its
    // pixels are replaced instead, which is the only moment that matters and
    // arrives here on a later tick than the call did.
    const paint = (color) => {
      const canvas = new Canvas(20, 20);
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = color;
      ctx.fillRect(0, 0, 20, 20);
      return canvas.toBufferSync("png");
    };
    const asURL = (png) => `data:image/png;base64,${png.toString("base64")}`;

    const image = new Image(paint("#ff0000"));
    assert.ok(image.complete, "the first image decoded");
    // Painted before the record, not after it: `paint` draws on a canvas of
    // its own, and recording into a second object hands over the first --
    // which would drain the batch by accident and prove nothing.
    const replacement = asURL(paint("#0000ff"));

    const canvas = new Canvas(20, 20);
    const ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);

    image.src = replacement;
    await image.decode();

    const pixels = canvas.toBufferSync("raw");
    assert.deepEqual(
      [...pixels.subarray(0, 3)],
      [255, 0, 0],
      "the pixels the call was given, not the ones that replaced them",
    );
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

  test("carries a drawing longer than the buffer it records into", () => {
    // The arena is a fixed 8192 numbers and a batch that will not fit is
    // handed over where it stands, so a real drawing crosses that boundary
    // several times a frame. What has to hold is that the seam is invisible:
    // the same drawing, chopped up by drains at other places, is the same
    // drawing.
    const N = 5000; // well past the arena at two numbers a segment

    const straight = new Path2D();
    const drained = new Path2D();
    for (let i = 0; i < N; i++) {
      straight.lineTo(i % 300, (i * 7) % 300);
      drained.lineTo(i % 300, (i * 7) % 300);
      if (i % 97 === 0) drained.bounds; // a read here, not there
    }
    assert.equal(straight.d, drained.d, "a path across the seam");
    assert.ok(straight.d.length > 30000, "the path really is long");

    // And with the lane beside it in use, so a colour recorded before the
    // seam is still the colour after it.
    const shot = (drain) => {
      const canvas = new Canvas(200, 200);
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      const marks = [];
      for (let i = 0; i < 8; i++) {
        const mark = new Path2D();
        mark.rect(i * 20, 0, 12, 12);
        marks.push(mark);
      }
      for (let i = 0; i < N; i++) {
        ctx.fillStyle = `hsl(${i % 360} 70% 50%)`;
        ctx.globalAlpha = 0.5 + (i % 10) / 40;
        ctx.fillRect(i % 190, (i * 3) % 190, 6, 6);
        ctx.fill(marks[i % marks.length]);
        if (drain && i % 89 === 0) ctx.canvas.width;
      }
      return canvas.toBufferSync("raw").toString("base64");
    };
    assert.equal(shot(false), shot(true), "colours and paths across the seam");
  });

  test("hands the queue over to an export that never asked for it", async () => {
    // An asynchronous export takes the handle and then awaits. Nothing in
    // the caller's code drains the batch, so the accessor the export goes
    // through is the only thing that can.
    const canvas = new Canvas(40, 40);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#ff0000";
    ctx.fillRect(0, 0, 40, 40);

    const pixels = await canvas.toBuffer("raw");
    assert.deepEqual(
      [...pixels.subarray((20 * 40 + 20) * 4, (20 * 40 + 20) * 4 + 4)],
      [255, 0, 0, 255],
      "the export drew what was still queued",
    );
  });

  test("leaves a page's queue with the page when another is added", () => {
    // `newPage` starts a second context while the first may still have verbs
    // waiting. They belong to the page that recorded them.
    const canvas = new Canvas(40, 40);
    canvas.gpu = false;
    const first = canvas.getContext("2d");
    first.fillStyle = "#ff0000";
    first.fillRect(0, 0, 40, 40); // queued, and never drained by hand

    const second = canvas.newPage();
    second.fillStyle = "#0000ff";
    second.fillRect(0, 0, 40, 40);

    const [one, two] = canvas.pages;
    assert.deepEqual(
      [...one.getImageData(20, 20, 1, 1).data],
      [255, 0, 0, 255],
      "the first page kept its own",
    );
    assert.deepEqual(
      [...two.getImageData(20, 20, 1, 1).data],
      [0, 0, 255, 255],
      "and the second is its own",
    );
  });

  test("gives an export the page as it stood when the export began", async () => {
    // Exports in flight while the drawing carries on. Each takes its own
    // copy of the pages when it is called, so what it writes is the canvas
    // at that moment and not at the moment it finishes -- and the queue is
    // drained into that copy rather than left for whichever export lands
    // first.
    const canvas = new Canvas(40, 40);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    const middle = (pixels) => [
      ...pixels.subarray((20 * 40 + 20) * 4, (20 * 40 + 20) * 4 + 4),
    ];

    ctx.fillStyle = "#ff0000";
    ctx.fillRect(0, 0, 40, 40);
    const red = canvas.toBuffer("raw"); // started here, not awaited
    ctx.fillStyle = "#0000ff";
    ctx.fillRect(0, 0, 40, 40);
    const blue = canvas.toBuffer("raw");
    ctx.fillStyle = "#00ff00";
    ctx.fillRect(0, 0, 40, 40);

    const [first, second] = await Promise.all([red, blue]);
    assert.deepEqual(middle(first), [255, 0, 0, 255], "the first export");
    assert.deepEqual(middle(second), [0, 0, 255, 255], "the second");
    assert.deepEqual(
      middle(canvas.toBufferSync("raw")),
      [0, 255, 0, 255],
      "and the canvas kept going",
    );
  });

  test("keeps thirty-two exports apart while the canvas is written to", async () => {
    // The same thing at the scale a server has: every export in flight at
    // once, each against a canvas that has moved on since.
    const canvas = new Canvas(60, 60);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");

    const flight = [];
    for (let i = 0; i < 32; i++) {
      ctx.fillStyle = `rgb(${i * 8} 0 0)`;
      ctx.fillRect(0, 0, 60, 60);
      flight.push(
        canvas.toBuffer("raw").then((pixels) => pixels[(30 * 60 + 30) * 4]),
      );
    }

    assert.deepEqual(
      await Promise.all(flight),
      Array.from({ length: 32 }, (_, i) => i * 8),
      "each export wrote its own moment",
    );
  });

  test("draws a path the caller has let go of", () => {
    // A record holds the `Path2D` until the batch lands, so the caller
    // dropping every reference of its own cannot take the geometry with it.
    const canvas = new Canvas(40, 40);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#00ff00";
    ((path) => ctx.fill(path))(
      (() => {
        const path = new Path2D();
        path.rect(0, 0, 20, 20);
        return path;
      })(),
    );
    global.gc?.();

    const pixels = canvas.toBufferSync("raw");
    assert.deepEqual(
      [...pixels.subarray((10 * 40 + 10) * 4, (10 * 40 + 10) * 4 + 4)],
      [0, 255, 0, 255],
    );
  });

  test("ignores what it cannot use without losing its place", () => {
    // One bad argument at a time is the easy case. This is bad calls mixed
    // through good ones across the seam the arena hands over at: a refused
    // call has to leave the batch exactly as it was, and an ignored one has
    // to leave it too.
    const clean = new Path2D();
    const littered = new Path2D();
    for (let i = 0; i < 5000; i++) {
      clean.lineTo(i % 300, (i * 7) % 300);
      littered.lineTo(i % 300, (i * 7) % 300);
      if (i % 13 === 0) littered.lineTo(NaN, 10);
      if (i % 29 === 0) littered.lineTo("nope", {});
      if (i % 37 === 0) {
        assert.throws(
          () => littered.arc(1, 1, -1, 0, 1),
          /Radius value must be positive/,
        );
      }
    }
    assert.equal(littered.d, clean.d, "nothing usable was lost or added");
  });

  test("accounts for every public operation on a recording class", () => {
    // The question this answers is "did anything get added without deciding
    // whether it belongs in a batch". It reads the classes rather than a
    // list, so a method added tomorrow shows up here tomorrow, and fails
    // until somebody has said which of the three it is.
    //
    // Written after an audit by hand missed three: `textTracking`, which is
    // a removed property that no longer crosses at all, and `lineDashMarker`
    // and `Path2D.d`, which could both be recorded and are not.
    const CROSSES = {
      // Answers the caller, so it cannot wait in a queue.
      "isContextLost()": "answers now",
      "getTransform()": "answers now",
      "createProjection()": "answers now",
      "isPointInPath()": "answers now",
      "isPointInStroke()": "answers now",
      "createPattern()": "answers now",
      "createLinearGradient()": "answers now",
      "createRadialGradient()": "answers now",
      "createConicGradient()": "answers now",
      "createTexture()": "answers now",
      "getLineDash()": "answers now",
      "createImageData()": "answers now",
      "getImageData()": "answers now",
      "measureText()": "answers now",
      "outlineText()": "answers now",
      "contains()": "answers now",
      "points()": "answers now",
      // Answers with a new path of its own.
      "interpolate()": "answers with a path",
      "complement()": "answers with a path",
      "difference()": "answers with a path",
      "intersect()": "answers with a path",
      "union()": "answers with a path",
      "xor()": "answers with a path",
      "jitter()": "answers with a path",
      "simplify()": "answers with a path",
      "unwind()": "answers with a path",
      "offset()": "answers with a path",
      // Caught by this test the first time it ran: an audit by hand had
      // called it a wrapper for `roundRectUniform`, on the strength of the
      // name starting the same way.
      "round()": "answers with a path",
      "transform()": "answers with a path",
      "trim()": "answers with a path",
      // Carries something no slot holds.
      "putImageData()": "pixels a caller can change without crossing",
      "drawCanvas()": "wants its source as a picture, not as pixels",
      "drawParagraph()": "a Paragraph, and no slot resolves one",
      "currentTransform =": "a matrix object",
      "font =": "a parsed object; and the crossing is not its cost",
      "letterSpacing =": "a parsed object",
      "wordSpacing =": "a parsed object",
      "fontVariant =": "a parsed object",
      "fontVariantCaps =": "reads and rewrites fontVariant",
      "textDecoration =": "a parsed object",
      "fontVariationSettings =": "a parsed object",
      "filter =": "a parsed object",
      "colorFilter =": "a boxed handle no slot resolves",
      "imageFilter =": "a boxed handle no slot resolves",
      "maskFilter =": "a boxed handle no slot resolves",
      // Could be recorded, and is not.
      "lineDashMarker =": "a Path2D or null, and a slot cannot hold the null",
      "d =": "a whole path replaced, which a drawing does once if at all",
      // Not a crossing at all.
      "textTracking =": "removed; it only warns",
    };

    // Reached by a wrapper that picks the verb matching the call's shape.
    const WRAPPED = {
      "fill()": true,
      "stroke()": true,
      "clip()": true,
      "roundRect()": true,
      "setTransform()": true,
      "drawImage()": true,
      "fillText()": true,
      "strokeText()": true,
      "saveLayer()": true,
      "addPath()": true,
      "setLineDash()": true,
    };

    for (const [klass, table] of [
      [Ctx, native.CanvasRenderingContext2D_verbTable()],
      [Path2D, native.Path2D_verbTable()],
    ]) {
      const verbs = new Set(Object.keys(table));
      const properties = new Set(
        [...verbs]
          .filter((verb) => verb.startsWith("set_"))
          .map((verb) => verb.slice(4).replace(/Text$/, "")),
      );

      for (const key of Object.getOwnPropertyNames(klass.prototype)) {
        if (key === "constructor") continue;
        const spec = Object.getOwnPropertyDescriptor(klass.prototype, key);
        const isMethod = typeof spec.value === "function";
        if (!isMethod && !spec.set) continue; // a read-only property

        const op = isMethod ? `${key}()` : `${key} =`;
        const recorded = isMethod ? verbs.has(key) : properties.has(key);
        assert.ok(
          recorded || WRAPPED[op] || CROSSES[op],
          `${klass.name}.${op} is neither recorded nor accounted for — ` +
            `add it to WRAPPED or to CROSSES with the reason it crosses`,
        );
      }
    }
  });
});
