// @ts-check

"use strict";

// A canvas drawn into another with `drawImage` is its own pixels on the
// page, and what it drew in a PDF or an SVG of the page. So is one drawn
// with `drawCanvas` that itself holds a drawn canvas or an image of an SVG,
// whose draws are not replayed into the page.

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

/** The source's side. */
const SIDE = [160, 90];

/** How many replays a canvas may cost for a document to be written from
 * what it drew rather than from its image: `DOCUMENT_REPLAY_BUDGET` in
 * `src/context/mod.rs`. */
const REPLAY_BUDGET = 64;

/** How many pages of images a page holds before it flattens:
 * `RECORDED_IMAGE_PAGES` in `src/context/mod.rs`. */
const RECORDED_IMAGE_PAGES = 4;

/** Frames drawn, enough for several flattens. */
const FRAMES = 24;

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

  // Past the replay budget a canvas is written as its image. A page it is
  // drawn into every frame, never cleared, writes what it drew before its
  // last flatten as the page's own pixels, so the images it writes level
  // off rather than growing by one a frame.
  test("levels off on a page drawn into past the replay budget", () => {
    const tiny = new Canvas(4, 4);
    tiny.gpu = false;
    tiny.getContext("2d").fillRect(0, 0, 4, 4);
    const drawn = new Canvas(SIDE[0], SIDE[1]);
    drawn.gpu = false;
    const sctx = drawn.getContext("2d");
    for (let n = 0; n <= REPLAY_BUDGET; n++) {
      sctx.drawImage(tiny, (n * 5) % SIDE[0], 0);
    }
    const page = new Canvas(SIDE[0], SIDE[1]);
    page.gpu = false;
    const ctx = page.getContext("2d");
    ctx.globalAlpha = 0.5;
    for (let frame = 0; frame < FRAMES; frame++) {
      sctx.fillRect((frame * 7) % SIDE[0], 40, 7, 40);
      ctx.drawImage(drawn, 0, 0);
    }
    const svg = page.toBufferSync("svg").toString("utf8");
    const images = svg.match(/<image/g)?.length ?? 0;
    assert.ok(images > 0, "written as images past the budget");
    assert.ok(
      images <= RECORDED_IMAGE_PAGES,
      `${images} images written for ${FRAMES} frames`,
    );
  });
});

describe("A canvas holding a drawn canvas or an SVG, drawn in with drawCanvas, in a PDF or an SVG", () => {
  /** An SVG of one rectangle, as `loadImage` reads one. */
  const SVG = Buffer.from(
    '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="60">' +
      '<rect x="10" y="10" width="80" height="40" fill="rgb(40,90,160)"/></svg>',
  );

  /** A CPU canvas with `drawn` drawn into it with `drawCanvas`. */
  const drawnWithDrawCanvas = (/** @type {Canvas} */ drawn) => {
    const canvas = new Canvas(SIDE[0] + 40, SIDE[1] + 40);
    canvas.gpu = false;
    canvas.getContext("2d").drawCanvas(drawn, 10, 10);
    return canvas;
  };

  test("is written as what it drew, for a canvas holding a canvas", () => {
    const page = drawnWithDrawCanvas(drawnInto(source()));
    const svg = page.toBufferSync("svg").toString("utf8");
    assert.equal(svg.match(/<image/g), null, "no image in the SVG");
    assert.match(svg, /<text/, "the text as text");
    const pdf = page.toBufferSync("pdf").toString("latin1");
    assert.equal(pdf.match(/\/Subtype \/Image/g), null, "no image in the PDF");
    assert.match(pdf, /\/Type \/Font/, "the text as text in the PDF");
  });

  test("is written as what it drew, for a canvas holding an SVG", async () => {
    const { loadImage } = require("../../lib");
    const drawn = new Canvas(SIDE[0], SIDE[1]);
    drawn.gpu = false;
    drawn.getContext("2d").drawImage(await loadImage(SVG), 0, 0);
    const page = drawnWithDrawCanvas(drawn);
    const svg = page.toBufferSync("svg").toString("utf8");
    assert.equal(svg.match(/<image/g), null, "no image in the SVG");
    assert.match(svg, /<rect fill="#285AA0"/, "the rectangle as a rect");
    const pdf = page.toBufferSync("pdf").toString("latin1");
    assert.equal(pdf.match(/\/Subtype \/Image/g), null, "no image in the PDF");
  });
});
