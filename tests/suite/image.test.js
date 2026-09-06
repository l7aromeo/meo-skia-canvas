// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, ImageData, loadImage } = require("../../lib");

/** An SVG document as a data URL, so no fixture file is involved. */
const svg = (attributes) =>
  "data:image/svg+xml;base64," +
  Buffer.from(
    `<svg xmlns="http://www.w3.org/2000/svg" ${attributes}>` +
      `<rect width="4" height="4" fill="#000"/></svg>`,
  ).toString("base64");

describe("an SVG with no size of its own", () => {
  test("is contained in the default object size", async () => {
    // CSS's default object size for a replaced element is 300 by 150, and an
    // undimensioned document is contained in it: whichever bound the aspect
    // ratio reaches first is the one that binds. Hanging the ratio from the
    // height instead left the width unbounded, so the 4:1 row below was 600
    // wide where a browser gives 300.
    //
    // 2:1 is the ratio at which the two rules agree, which is why it has to
    // sit beside a wider one and a taller one rather than alone.
    for (let [viewBox, expected] of [
      ["0 0 40 10", [300, 75]],
      ["0 0 40 20", [300, 150]],
      ["0 0 16 16", [150, 150]],
      ["0 0 10 40", [37.5, 150]],
    ]) {
      let image = await loadImage(svg(`viewBox="${viewBox}"`));
      assert.deepEqual([image.width, image.height], expected, viewBox);
    }
  });

  test("takes that size unchanged when it states no ratio", async () => {
    let bare = await loadImage(svg(""));
    assert.deepEqual([bare.width, bare.height], [300, 150]);
  });

  test("survives a viewBox with a zero side", async () => {
    // Dividing by it produced Infinity, 0 and NaN widths, which reached every
    // caller sizing a surface from the result.
    for (let viewBox of ["0 0 40 0", "0 0 0 40", "0 0 0 0"]) {
      let image = await loadImage(svg(`viewBox="${viewBox}"`));
      assert.ok(
        Number.isFinite(image.width) && Number.isFinite(image.height),
        `viewBox="${viewBox}" gave ${image.width}x${image.height}`,
      );
      assert.deepEqual([image.width, image.height], [300, 150], viewBox);
    }
  });

  test("takes the missing dimension from the ratio, not from itself", async () => {
    // A document stating one dimension used to square it -- a rule of this
    // crate's own that no clause names. CSS derives the missing side from the
    // aspect ratio, and from the default object size when there is no ratio.
    for (let [attributes, expected] of [
      ['width="100" viewBox="0 0 40 10"', [100, 25]],
      ['height="100" viewBox="0 0 40 10"', [400, 100]],
      ['width="100"', [100, 150]],
      ['height="100"', [300, 100]],
    ]) {
      let image = await loadImage(svg(attributes));
      assert.deepEqual([image.width, image.height], expected, attributes);
    }
  });

  test("a stated size is still read as stated", async () => {
    // The fallback must not reach a document that says what it wants.
    let sized = await loadImage(svg('width="40" height="20"'));
    assert.deepEqual([sized.width, sized.height], [40, 20]);
  });
});

describe("a refusal takes the type the standard names", () => {
  /** The name and constructor of whatever `run` throws. */
  const thrown = (run) => {
    try {
      run();
      return "no throw";
    } catch (error) {
      return `${error.constructor.name}/${error.name}`;
    }
  };

  test("a zero dimension is an IndexSizeError, whichever door it came in", () => {
    // "If either the sw or sh arguments are zero, then throw an
    // "IndexSizeError" DOMException." Every entry point builds its buffer
    // through the one `ImageData` constructor, so all three answer alike --
    // they answered with a RangeError, and `getImageData(0, 0, 0, 0)` with a
    // TypeError about buffer length, which is internal arithmetic rather than
    // anything the caller wrote.
    let ctx = new Canvas(8, 8).getContext("2d");
    for (let [what, run] of [
      ["getImageData(0,0,0,0)", () => ctx.getImageData(0, 0, 0, 0)],
      ["getImageData(0,0,0,5)", () => ctx.getImageData(0, 0, 0, 5)],
      ["getImageData(0,0,5,0)", () => ctx.getImageData(0, 0, 5, 0)],
      ["createImageData(0,0)", () => ctx.createImageData(0, 0)],
      ["createImageData(2,0)", () => ctx.createImageData(2, 0)],
      ["new ImageData(0,0)", () => new ImageData(0, 0)],
      ["new ImageData(2,0)", () => new ImageData(2, 0)],
    ])
      assert.equal(thrown(run), "DOMException/IndexSizeError", what);
  });

  test("a buffer that cannot describe whole pixels is an InvalidStateError", () => {
    // Two different refusals where there was one. A length that is not a
    // whole number of pixels is `InvalidStateError`; a length that is whole
    // but does not match the dimensions asked for is `IndexSizeError`. Both
    // were one TypeError.
    assert.equal(
      thrown(() => new ImageData(new Uint8ClampedArray(6), 1)),
      "DOMException/InvalidStateError",
      "six bytes is not a whole number of four-byte pixels",
    );
    assert.equal(
      thrown(() => new ImageData(new Uint8ClampedArray(8), 3)),
      "DOMException/IndexSizeError",
      "two pixels is whole, and is not three across",
    );
  });

  test("an unknown pattern repetition is a SyntaxError", () => {
    // "If repetition is not identical to one of "repeat", "repeat-x",
    // "repeat-y", or "no-repeat", then throw a "SyntaxError" DOMException."
    // A different clause from the one above, naming a different exception --
    // which is why these are not one family with one answer.
    let ctx = new Canvas(8, 8).getContext("2d");
    assert.equal(
      thrown(() => ctx.createPattern(new Canvas(2, 2), "bogus")),
      "DOMException/SyntaxError",
    );
  });

  test("the refusals the standard does not name are left alone", () => {
    // The controls. An unrecognised `colorSpace` is a value outside an
    // enumeration, which WebIDL makes a TypeError -- rule 2, not rule 1 --
    // and the two cases that are not refusals at all must stay silent.
    let ctx = new Canvas(8, 8).getContext("2d");
    assert.equal(
      thrown(() => new ImageData(2, 2, { colorSpace: "bogus" })),
      "TypeError/TypeError",
    );
    assert.equal(
      thrown(() => ctx.getImageData(0, 0, -2, -2)),
      "no throw",
      "a negative size normalises rather than refusing",
    );
    assert.equal(
      thrown(() => ctx.createPattern(new Canvas(2, 2), null)),
      "no throw",
      "null repetition means repeat",
    );
    assert.equal(
      thrown(() => new ImageData(2, 2)),
      "no throw",
    );
  });
});
