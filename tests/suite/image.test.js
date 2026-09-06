// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { loadImage } = require("../../lib");

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
