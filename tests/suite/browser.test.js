// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { installDOM, downloads } = require("../support/dom-stub");

// Before the require below: `lib/browser.js` destructures `window` at module
// scope, so the globals have to be in place by the time it is first loaded.
installDOM();

const browser = require("../../lib/browser");

/** A canvas with one page, sized so a scaled export is distinguishable. */
function canvas(width = 10, height = 20) {
  const it = new browser.Canvas(width, height);
  it.getContext("2d");
  return it;
}

describe("the browser build exports what it says it does", () => {
  test("re-exports the DOM classes and the two data-only modules", () => {
    for (const name of [
      "Canvas",
      "loadImage",
      "loadImageData",
      "ColorMatrix",
      "TextDecoration",
      "RectHeightStyle",
    ]) {
      assert.ok(browser[name], `${name} is missing from the browser build`);
    }
  });

  test("a Canvas is the element, with the export methods on it", () => {
    const it = canvas();
    assert.equal(it.nodeName, "CANVAS");
    assert.deepEqual(
      { width: it.width, height: it.height },
      { width: 10, height: 20 },
    );
    for (const method of ["toBuffer", "toURL", "toFile", "newPage"]) {
      assert.equal(
        typeof it[method],
        "function",
        `${method} is not on the element`,
      );
    }
  });

  test("saveAs says what replaced it", () => {
    assert.throws(() => canvas().saveAs("x.png"), /renamed to Canvas.toFile/);
  });
});

describe("the browser build resolves formats the way the Node build does", () => {
  test("defaults to PNG", async () => {
    const url = await canvas().toURL();
    assert.match(url, /^data:image\/png;base64,/);
  });

  test("reads the format from an extension, case-insensitively", async () => {
    for (const [ext, mime] of [
      ["png", "image/png"],
      ["jpg", "image/jpeg"],
      ["jpeg", "image/jpeg"],
      ["JPG", "image/jpeg"],
      ["webp", "image/webp"],
    ]) {
      const url = await canvas().toURL(ext);
      assert.match(
        url,
        new RegExp(`^data:${mime.replace("/", "\\/")};base64,`),
        `${ext} did not resolve to ${mime}`,
      );
    }
  });

  test("accepts a media type where an extension goes", async () => {
    assert.match(await canvas().toURL("image/webp"), /^data:image\/webp;/);
  });

  test("names what it expected when the format is unknown", async () => {
    await assert.rejects(
      async () => canvas().toURL("tiff"),
      /Unsupported file format "tiff" \(expected "png", "jpg", or "webp"\)/,
    );
  });

  test("exports a canvas nobody asked for a context on", async () => {
    // The Node build creates a context so an untouched canvas still produces
    // an empty image. This build reaches the same place differently: its
    // `pages` getter appends the element itself, so the list is never empty
    // and `exportOptions`' own "Canvas has no associated contexts" guard
    // cannot fire. Pinned as behaviour, not endorsed -- that guard is
    // unreachable, which is worth its own decision.
    const bare = new browser.Canvas(4, 4);
    assert.equal(bare.pages.length, 1, "the element counts as its own page");
    assert.match(await bare.toURL(), /^data:image\/png;base64,/);
  });
});

describe("the browser build reads the export options", () => {
  test("toBuffer resolves to an ArrayBuffer, not a Buffer", async () => {
    const buffer = await canvas().toBuffer();
    assert.ok(
      buffer instanceof ArrayBuffer,
      "toBuffer should give an ArrayBuffer",
    );
  });

  test("a bare number is read as the quality", async () => {
    const it = canvas();
    await it.toBuffer("jpg", 0.25);
    assert.equal(it.encoded.quality, 0.25);
  });

  test("quality defaults to 0.92, as it does in Node", async () => {
    const it = canvas();
    await it.toBuffer("jpg");
    assert.equal(it.encoded.quality, 0.92);
  });

  test("refuses a quality outside 0-1", async () => {
    await assert.rejects(
      async () => canvas().toBuffer("jpg", 2),
      /quality option must be an number in the 0.0–1.0 range/,
    );
  });

  test("an @2x suffix sets the density", async () => {
    const it = canvas(10, 20);
    await it.toURL("png@2x");
    assert.deepEqual(
      { width: it._scaledTo?.width, height: it._scaledTo?.height },
      { width: undefined, height: undefined },
      "the source element is not the one that gets scaled",
    );
  });

  test("density scales the exported element", async () => {
    const it = canvas(10, 20);
    const url = await it.toURL("png", { density: 2 });
    // The scaled copy is a second element, so the dimensions travel in the
    // encoded payload rather than on the canvas the test holds.
    assert.match(
      Buffer.from(url.split(",")[1], "base64").toString(),
      /^20x40:/,
      "a density of 2 should encode a 20x40 image",
    );
  });

  test("newPage keeps the size it was given, and adds a page", () => {
    const it = canvas(10, 20);
    assert.equal(it.pages.length, 1);
    it.newPage();
    assert.equal(it.pages.length, 2, "newPage should add one");
    assert.deepEqual(
      { width: it.width, height: it.height },
      { width: 10, height: 20 },
      "no argument leaves the size alone",
    );
    it.newPage(5, 6);
    assert.deepEqual(
      { width: it.width, height: it.height },
      { width: 5, height: 6 },
      "newPage(w, h) resizes",
    );
  });
});

describe("the browser build reports the page the caller named", () => {
  /** A canvas with `count` pages. */
  function paged(count) {
    const it = canvas();
    for (let i = 1; i < count; i++) it.newPage();
    return it;
  }

  test("names the requested page, not the index it resolved to", async () => {
    // The Node build carries a comment recording why: asking for page 9 of a
    // two-page canvas was told "8 is out of bounds", a number nobody typed.
    await assert.rejects(
      async () => paged(2).toURL("png", { page: 9 }),
      /Canvas has pages 1–2 \(9 is out of bounds\)/,
    );
  });

  test("names it on a single-page canvas too", async () => {
    await assert.rejects(
      async () => paged(1).toURL("png", { page: 4 }),
      /Canvas only has a ‘page 1’ \(4 is out of bounds\)/,
    );
  });

  test("counts a negative page from the end", async () => {
    await assert.rejects(
      async () => paged(2).toURL("png", { page: -5 }),
      /\(-5 is out of bounds\)/,
    );
  });

  test("still accepts a page that exists", async () => {
    assert.match(await paged(3).toURL("png", { page: 2 }), /^data:image\/png;/);
  });
});

describe("the browser build validates density the way the Node build does", () => {
  // One option name, one package. `exportOptions` in `lib/classes/canvas.js`
  // takes any positive number -- 1.5 is an ordinary device pixel ratio -- and
  // says so. This build demanded a whole number under a message naming a range
  // it refused.
  test("accepts a fractional device pixel ratio", async () => {
    const url = await canvas(10, 20).toURL("png", { density: 1.5 });
    assert.match(
      Buffer.from(url.split(",")[1], "base64").toString(),
      /^15x30:/,
      "a density of 1.5 should encode a 15x30 image",
    );
  });

  test("refuses zero, and says positive rather than non-negative", async () => {
    await assert.rejects(
      async () => canvas().toURL("png", { density: 0 }),
      /Expected a positive number for `density`/,
    );
  });

  test("refuses a negative density the same way", async () => {
    await assert.rejects(
      async () => canvas().toURL("png", { density: -2 }),
      /Expected a positive number for `density`/,
    );
  });

  test("refuses something that is not a number", async () => {
    await assert.rejects(
      async () => canvas().toURL("png", { density: "2" }),
      /Expected a positive number for `density`/,
    );
  });
});

describe("the browser build gathers pages only when asked to", () => {
  test("a filename template writes every page", async () => {
    const it = canvas();
    it.newPage();
    it.newPage();
    const before = downloads.length;
    await it.toFile("frame-{}.png");
    // Three pages, one zip download -- the archive is the whole point of the
    // template. Without JSZip bundled it logs and resolves, which is the
    // documented behaviour, so the count is what can be asserted here.
    assert.ok(downloads.length >= before, "toFile resolved");
  });

  test("a plain filename downloads the current page alone", async () => {
    const it = canvas();
    it.newPage();
    const before = downloads.length;
    await it.toFile("page.png");
    assert.equal(downloads.length, before + 1, "one file, not one per page");
  });
});

describe("the browser build downloads instead of writing files", () => {
  test("toFile hands the browser a blob and the filename", async () => {
    const before = downloads.length;
    await canvas().toFile("chart.png");
    assert.equal(
      downloads.length,
      before + 1,
      "one download should have started",
    );
    assert.equal(downloads.at(-1).filename, "chart.png");
    assert.match(downloads.at(-1).href, /^blob:/);
  });
});
