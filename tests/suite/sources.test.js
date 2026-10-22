// @ts-check

"use strict";

// What the loaders accept as bytes already in memory.

const fs = require("fs"),
  { assert, describe, test } = require("../runner"),
  { Canvas, Image, loadImage, loadImageData } = require("../../lib");

const PATH = "tests/assets/images/pentagon.png",
  PNG = fs.readFileSync(PATH);

/** What an `Image` holds, read back through a canvas of its size. */
const pixels = (img) => {
  let canvas = new Canvas(img.width, img.height),
    ctx = canvas.getContext("2d");
  ctx.drawImage(img, 0, 0);
  return createDigest(ctx.getImageData(0, 0, img.width, img.height).data);
};
const createDigest = (data) =>
  require("crypto").createHash("sha256").update(data).digest("hex");

// The fixture's bytes copied into a larger buffer at an offset, so a view
// that ignored `byteOffset` or `byteLength` would read the wrong bytes.
const OFFSET = 3;
const padded = () => {
  let memory = new ArrayBuffer(PNG.length + OFFSET + 5);
  new Uint8Array(memory, OFFSET, PNG.length).set(PNG);
  return memory;
};
// A copy of exactly the fixture's bytes in memory of its own.
const exact = () =>
  PNG.buffer.slice(PNG.byteOffset, PNG.byteOffset + PNG.byteLength);

// Every shape that holds bytes one to an element, each holding the fixture.
const BYTE_VIEWS = {
  Uint8Array: () => new Uint8Array(padded(), OFFSET, PNG.length),
  Uint8ClampedArray: () => new Uint8ClampedArray(padded(), OFFSET, PNG.length),
  Int8Array: () => new Int8Array(padded(), OFFSET, PNG.length),
  DataView: () => new DataView(padded(), OFFSET, PNG.length),
  ArrayBuffer: exact,
  SharedArrayBuffer: () => {
    let shared = new SharedArrayBuffer(PNG.length);
    new Uint8Array(shared).set(PNG);
    return shared;
  },
};

describe("Bytes in any byte-sized view load as a Buffer of them does", () => {
  test("through loadImage", async () => {
    let expected = pixels(await loadImage(PNG)),
      got = {};
    for (let [shape, make] of Object.entries(BYTE_VIEWS))
      got[shape] = await loadImage(/** @type {any} */ (make())).then(
        (img) => (pixels(img) == expected ? "same pixels" : "different"),
        (e) => `${e.name}: ${e.message}`,
      );
    assert.deepEqual(
      got,
      Object.fromEntries(
        Object.keys(BYTE_VIEWS).map((shape) => [shape, "same pixels"]),
      ),
    );
  });

  test("through new Image and img.src", async () => {
    let expected = pixels(new Image(PNG)),
      got = {};
    for (let [shape, make] of Object.entries(BYTE_VIEWS)) {
      let constructed;
      try {
        constructed = pixels(new Image(/** @type {any} */ (make())));
      } catch (e) {
        constructed = `${e.name}: ${e.message}`;
      }
      let img = new Image(),
        assigned = await new Promise((ok) => {
          img.onload = () => ok(pixels(img));
          img.onerror = (e) => ok(`error: ${e?.message ?? e}`);
          img.src = /** @type {any} */ (make());
        });
      got[shape] = { constructed, assigned };
    }
    assert.deepEqual(
      got,
      Object.fromEntries(
        Object.keys(BYTE_VIEWS).map((shape) => [
          shape,
          { constructed: expected, assigned: expected },
        ]),
      ),
    );
  });

  test("through loadImageData, as the pixels they are", async () => {
    let bytes = Uint8Array.from({ length: 16 }, (_, i) => i + 1),
      data = await loadImageData(bytes, 2, 2);
    assert.deepEqual([...data.data], [...bytes]);
  });

  test("a path is still read as a path", async () => {
    let img = await loadImage(PATH);
    assert.equal(pixels(img), pixels(await loadImage(PNG)));
  });
});

// A typed array whose elements are wider than a byte says two things at once:
// its elements, which is what a Uint16Array of 16-bit pixels means, and its
// memory, which is what an encoded image in one would mean. Neither reading is
// safe to guess, so it is refused with the view that says which.
describe("A typed array of wider elements is refused, naming the fix", () => {
  const WIDE = {
    Uint16Array: () => new Uint16Array(8),
    Float32Array: () => new Float32Array(4),
    BigInt64Array: () => new BigInt64Array(2),
  };

  test("by every loader, before anything is read", async () => {
    let got = {};
    for (let [shape, make] of Object.entries(WIDE)) {
      let constructed;
      try {
        new Image(/** @type {any} */ (make()));
        constructed = "accepted";
      } catch (e) {
        constructed = e.name;
      }
      got[shape] = {
        loadImage: await loadImage(/** @type {any} */ (make())).then(
          () => "loaded",
          (e) =>
            `${e.name}${/new Uint8Array\(/.test(e.message) ? " naming the fix" : `: ${e.message}`}`,
        ),
        loadImageData: await loadImageData(
          /** @type {any} */ (make()),
          2,
          2,
        ).then(
          () => "loaded",
          (e) => e.name,
        ),
        constructed,
      };
    }
    assert.deepEqual(
      got,
      Object.fromEntries(
        Object.keys(WIDE).map((shape) => [
          shape,
          {
            loadImage: "TypeError naming the fix",
            loadImageData: "TypeError",
            constructed: "TypeError",
          },
        ]),
      ),
    );
  });
});
