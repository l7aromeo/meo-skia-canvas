// @ts-check

"use strict";

const { createHash } = require("crypto"),
  { assert, describe, test } = require("../runner"),
  { Canvas, ImageData, loadImage, FontLibrary } = require("../../lib");

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

describe("an SVG containing text", () => {
  // Registration is global to this process, so the aliases below are chosen
  // not to collide with anything else here. Nothing else in this file renders
  // text.
  const FACE = "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";

  // Read before registering anything: `families` reports registered aliases
  // alongside system ones, so this has to be captured while it is still only
  // the system's.
  const SYSTEM = FontLibrary.families.slice();

  /** The first of `names` the system has, or its first family at all. */
  const systemFamily = (...names) =>
    names.find((name) => SYSTEM.includes(name)) ?? SYSTEM[0];

  // Which families exist is a property of the machine, so the two names below
  // are chosen from what is actually installed rather than assumed. A
  // hard-coded `Helvetica` passes here and inverts on a Linux runner that
  // does not have it: the system would not own the name, the registered face
  // would win, and the collision test would fail for the wrong reason.
  const COLLIDING = systemFamily(
    "Helvetica",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
    "Nimbus Sans",
  );
  const OTHER_SYSTEM = systemFamily(
    "Courier",
    "Courier New",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Nimbus Mono PS",
  );

  FontLibrary.use("UniqueTestFace", [FACE]);
  FontLibrary.use(COLLIDING, [FACE]);

  /** A document stating `family`, or stating no `font-family` for `null`. */
  const document = (family) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="60">` +
        `<text x="5" y="45" font-size="40"` +
        (family === null ? "" : ` font-family="${family}"`) +
        ` fill="#000">Wgq&#160;AVA</text></svg>`,
    ).toString("base64");

  /**
   * A hash of every pixel the document paints.
   *
   * Which face rendered cannot be settled by an extent: two faces with
   * similar metrics share a bounding box and cannot share every pixel. The
   * sample carries ascenders, a descender and a kerning pair for the same
   * reason.
   */
  const rendering = async (family) => {
    let image = await loadImage(document(family)),
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60);
    return createHash("sha256")
      .update(Buffer.from(data.buffer))
      .digest("hex")
      .slice(0, 12);
  };

  // Nothing else in this suite renders SVG text and no SVG fixture contains a
  // `<text>` element, which is why a process kill here went unnoticed through
  // a release. The Rust suite renders SVG through `FontMgr::new()`, the one
  // font manager that does not reach the fault, so only a test on this side
  // can cover it.
  test("renders when the family cannot be resolved", async () => {
    // The trigger is a family that does not resolve, not an absent one: this
    // killed the process for a document naming any font the machine lacks.
    let missing = await rendering("ZzzNoSuchFamilyAnywhere");
    let absent = await rendering(null);
    assert.equal(
      missing,
      absent,
      "both fall back to the same face, and neither takes the process down",
    );

    let canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(await loadImage(document(null)), 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60);
    assert.ok(
      data.some((_, i) => i % 4 === 3 && data[i] > 0),
      "the fallback has to paint something, or this passes on a blank page",
    );
  });

  test("uses a registered face whose name the system does not have", async () => {
    // The half that must not regress. `font_mgr` composes the system manager
    // ahead of registered faces, so this says the composition still finds
    // them.
    let registered = await rendering("UniqueTestFace"),
      fallback = await rendering("ZzzNoSuchFamilyAnywhere"),
      system = await rendering(OTHER_SYSTEM);

    assert.notEqual(
      registered,
      fallback,
      "a registered face is not the fallback, or registration did nothing",
    );
    assert.notEqual(
      registered,
      system,
      "the control: this comparison can tell two faces apart",
    );
  });

  test("prefers the system face where a registered name collides", async () => {
    // A behaviour change, recorded rather than left to be discovered. A face
    // registered under a name a system family already has no longer shadows
    // it -- the cost of ordering the system manager first, and confined to
    // SVG text. The colliding name is registered above to the same file as
    // `UniqueTestFace`, so the two would render identically if the
    // registration won.
    //
    // Only that they differ is asserted. Which face the collision resolves to
    // is the system's business and varies by machine, and equating it with
    // the fallback would hold only where the colliding family happens to be
    // the one the system falls back to.
    let collided = await rendering(COLLIDING),
      registered = await rendering("UniqueTestFace");

    assert.notEqual(
      collided,
      registered,
      `the registered face does not win ${COLLIDING}, which the system has`,
    );
  });
});

describe("an SVG's text positions resolve at the dpi CSS fixes", () => {
  const document = (x) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="60">` +
        `<text x="${x}" y="45" font-size="20" fill="#000">|</text></svg>`,
    ).toString("base64");

  /** The first and last columns the document inks. */
  const columns = async (x) => {
    let image = await loadImage(document(x)),
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60),
      lit = (col) => {
        for (let y = 0; y < 60; y++)
          if (data[(y * 320 + col) * 4 + 3] > 0) return true;
        return false;
      },
      inked = [];
    for (let col = 0; col < 320; col++) if (lit(col)) inked.push(col);
    return [inked[0], inked[inked.length - 1]];
  };

  // `x`, `y`, `dx` and `dy` on a text element are the four attributes the
  // parsed document cannot express -- skia-safe exposes them for reading only
  // -- so they are rewritten in the XML before Skia sees it. `loadImage`
  // reaches that through its own door rather than through `Svg::parse`, and
  // no Rust test executes this path.
  test("an inch positions text at 96 pixels, not 90", async () => {
    assert.deepEqual(await columns("1in"), await columns("96"));
    assert.notDeepEqual(
      await columns("1in"),
      await columns("90"),
      "the control: 90 is Skia's own answer for an inch and has to differ",
    );
  });

  test("a list of positions converts each item", async () => {
    // The case a scan over the document text could not have handled.
    let [first] = await columns("1in 2in");
    let [expected] = await columns("96 192");
    assert.equal(first, expected);
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
