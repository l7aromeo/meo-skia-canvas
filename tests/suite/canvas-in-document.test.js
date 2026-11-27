// @ts-check

"use strict";

// A canvas drawn into another with `drawImage` is its own pixels on the
// page, and what it drew in a PDF or an SVG of the page.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

/** The source's side. */
const SIDE = [160, 90];

/** A CPU canvas holding an ellipse and a line of text. */
const source = () => {
  const canvas = new Canvas(SIDE[0], SIDE[1]);
  canvas.gpu = false;
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "rgb(40,90,160)";
  ctx.beginPath();
  ctx.ellipse(80, 45, 60, 25, 0, 0, 2 * Math.PI);
  ctx.fill();
  ctx.fillStyle = "rgb(200,40,30)";
  ctx.font = "18px sans-serif";
  ctx.fillText("Vector", 40, 52);
  return canvas;
};

/** A CPU canvas with `drawn` drawn into it with `drawImage`. */
const drawnInto = (/** @type {Canvas} */ drawn) => {
  const canvas = new Canvas(SIDE[0] + 20, SIDE[1] + 20);
  canvas.gpu = false;
  canvas.getContext("2d").drawImage(drawn, 10, 10);
  return canvas;
};

describe("A canvas drawn in with drawImage, in a PDF or an SVG", () => {
  test("is written as what it drew, not as an image", () => {
    const page = drawnInto(source());
    const svg = page.toBufferSync("svg").toString("utf8");
    assert.equal(svg.match(/<image/g), null, "no image in the SVG");
    assert.match(svg, /<path/, "the ellipse as a path");
    assert.match(svg, /<text/, "the text as text");
    const pdf = page.toBufferSync("pdf").toString("latin1");
    assert.equal(pdf.match(/\/Subtype \/Image/g), null, "no image in the PDF");
    assert.match(pdf, /\/Type \/Font/, "the text as text in the PDF");
  });

  test("is written so two levels deep", () => {
    const svg = drawnInto(drawnInto(source()))
      .toBufferSync("svg")
      .toString("utf8");
    assert.equal(svg.match(/<image/g), null, "no image in the SVG");
    assert.match(svg, /<text/, "the text as text");
  });

  test("lands its pixels on the page as an image of it does", async () => {
    const drawn = source();
    const page = drawnInto(drawn);
    const { loadImage } = require("../../lib");
    const image = await loadImage(drawn.toBufferSync("png"));
    const alone = new Canvas(SIDE[0] + 20, SIDE[1] + 20);
    alone.gpu = false;
    alone.getContext("2d").drawImage(image, 10, 10);
    const read = (/** @type {Canvas} */ canvas) =>
      canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height)
        .data;
    assert.deepEqual(read(page), read(alone));
  });
});
