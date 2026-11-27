// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, loadImage } = require("../../lib");

// A document's opacities land through `drawImage` as Chrome 159 lands them:
// each paint's alpha on a byte, the product of its colour's alpha, its
// `fill-opacity` or `stroke-opacity`, and an `opacity` folded into that one
// draw -- a leaf's own, and a group's over one shape -- rounded once. The
// pixels' recording holds Skia's SVG module to that (`BytePaintAlpha`, from
// containers/skia-svg-byte-paint-alpha.patch). Without it a flood landed a
// tie a level high and a group over a translucent shape rounded twice.
//
// The reference is Chrome's headless shell drawing each document as an
// `<img>` onto a 2D canvas: black at an alpha over an opaque red ground,
// read at one pixel. One row differs, and says why.

/** The alphas measured, among them the ties 0.5 and 0.3 (76.5 of 255). */
const ALPHAS = [0.5, 0.3, 0.6, 0.2];

/** Each side of every document and canvas, in pixels. */
const SIDE = 4;

/** A rectangle covering the document, with `attributes`. */
const rect = (attributes) =>
  `<rect width="${SIDE}" height="${SIDE}" ${attributes}/>`;

/** How black is laid at `a` over the red ground: `[defs, body]`. */
const WAYS = {
  opacity: (a) => ["", rect(`fill="black" opacity="${a}"`)],
  "fill-opacity": (a) => ["", rect(`fill="black" fill-opacity="${a}"`)],
  "stroke-opacity": (a) => [
    "",
    `<rect x="-2" y="-2" width="8" height="8" fill="none" stroke="black" stroke-width="12" stroke-opacity="${a}"/>`,
  ],
  "stop-opacity": (a) => [
    `<linearGradient id="g"><stop offset="0" stop-color="black" stop-opacity="${a}"/><stop offset="1" stop-color="black" stop-opacity="${a}"/></linearGradient>`,
    rect(`fill="url(#g)"`),
  ],
  "flood-opacity": (a) => [
    `<filter id="f" x="0" y="0" width="1" height="1"><feFlood flood-color="black" flood-opacity="${a}"/></filter>`,
    rect(`fill="black" filter="url(#f)"`),
  ],
  "colour alpha": (a) => ["", rect(`fill="rgba(0,0,0,${a})"`)],
  "gradient fill-opacity": (a) => [
    `<linearGradient id="h"><stop offset="0" stop-color="black"/><stop offset="1" stop-color="black"/></linearGradient>`,
    rect(`fill="url(#h)" fill-opacity="${a}"`),
  ],
  "group over one shape": (a) => [
    "",
    `<g opacity="${a}">${rect(`fill="black"`)}</g>`,
  ],
  "group over two shapes": (a) => [
    "",
    `<g opacity="${a}">${rect(`fill="black"`)}${rect(`fill="rgb(0,0,255)"`)}</g>`,
  ],
  "group over a half fill": (a) => [
    "",
    `<g opacity="${a}">${rect(`fill="black" fill-opacity="0.5"`)}</g>`,
  ],
};

/** Chrome 159's red, green and blue for each way, at each of `ALPHAS`. */
const CHROME = {
  opacity: [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "fill-opacity": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "stroke-opacity": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "stop-opacity": [
    [128, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "flood-opacity": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "colour alpha": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "gradient fill-opacity": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "group over one shape": [
    [127, 0, 0],
    [178, 0, 0],
    [102, 0, 0],
    [204, 0, 0],
  ],
  "group over two shapes": [
    [126, 0, 128],
    [177, 0, 77],
    [101, 0, 153],
    [203, 0, 51],
  ],
  "group over a half fill": [
    [191, 0, 0],
    [217, 0, 0],
    [178, 0, 0],
    [229, 0, 0],
  ],
};

/**
 * Where this lands apart from Chrome, what it lands and why. 127.5 is an
 * exact tie: a gradient's stops stay in float in both, and the canvas's
 * eight-bit pipeline lands the tie 127 where Chrome's lands 128.
 */
const APART = {
  "stop-opacity 0.5": [[127, 0, 0], "an exact tie, 127.5, rounded apart"],
};

/** The document laying black over red `way` at `a`. */
const documentOf = (way, a) => {
  const [defs, body] = WAYS[way](a);
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${SIDE}" height="${SIDE}">` +
    `<defs>${defs}</defs>${rect(`fill="rgb(255,0,0)"`)}${body}</svg>`
  );
};

describe("an SVG's opacities", () => {
  test("land each paint's alpha on a byte, as Chrome does", async () => {
    const wrong = [];
    for (const way of Object.keys(WAYS)) {
      for (const [i, a] of ALPHAS.entries()) {
        const image = await loadImage(Buffer.from(documentOf(way, a)));
        const canvas = new Canvas(SIDE, SIDE);
        canvas.gpu = false;
        const ctx = canvas.getContext("2d");
        ctx.drawImage(image, 0, 0);
        const got = Array.from(ctx.getImageData(1, 1, 1, 1).data.slice(0, 3));
        const apart = APART[`${way} ${a}`];
        const want = apart ? apart[0] : CHROME[way][i];
        if (got.join() != want.join()) {
          wrong.push(`${way} at ${a}: ${got}, expected ${want}`);
        }
      }
    }
    assert.deepEqual(wrong, []);
  });
});
