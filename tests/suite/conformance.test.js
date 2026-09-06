// @ts-check

"use strict";

const fs = require("fs"),
  tmp = require("tmp"),
  path = require("path"),
  { assert, describe, test, beforeEach, afterEach } = require("../runner"),
  {
    Canvas,
    ColorFilter,
    DOMMatrix,
    DOMPoint,
    DOMRect,
    Image,
    ImageData,
    ImageFilter,
    MaskFilter,
    Paragraph,
    ParagraphBuilder,
    Shader,
    TextMetrics,
    loadImage,
  } = require("../../lib");

// Behaviour the browser Canvas defines, that the declaration files already
// promised, and that the runtime got wrong. Each of these typechecked against
// lib/index.d.ts and then threw, returned undefined, or silently produced NaN.
describe("browser conformance", () => {
  describe("CanvasRenderingContext2D", () => {
    /** @type {Canvas} */ let canvas;
    /** @type {any} */ let ctx;

    beforeEach(() => {
      canvas = new Canvas(64, 64);
      ctx = canvas.getContext("2d");
    });

    test("setTransform() with no arguments resets to identity", () => {
      ctx.translate(11, 13);
      ctx.scale(2, 3);
      ctx.setTransform();

      let m = ctx.getTransform();
      assert.equal(m.isIdentity, true);
      assert.equal(m.e, 0);
      assert.equal(m.f, 0);
    });

    test("setTransform() still accepts the six-argument form", () => {
      ctx.setTransform(2, 0, 0, 2, 3, 4);

      let m = ctx.getTransform();
      assert.equal(m.a, 2);
      assert.equal(m.e, 3);
      assert.equal(m.f, 4);
    });

    test("createImageData() clones the dimensions of an ImageData", () => {
      let source = ctx.createImageData(7, 5);
      source.data[0] = 255;

      let clone = ctx.createImageData(source);
      assert.equal(clone.width, 7);
      assert.equal(clone.height, 5);
      // A clone copies the dimensions, not the pixels.
      assert.equal(clone.data[0], 0);
    });

    test("createImageData() still accepts width and height", () => {
      let data = ctx.createImageData(7, 5);
      assert.equal(data.width, 7);
      assert.equal(data.height, 5);
    });
  });

  describe("DOMMatrix", () => {
    test("invertSelf() inverts in place and returns itself", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 0, 0]),
        returned = m.invertSelf();

      assert.equal(m.a, 0.5);
      assert.equal(m.d, 0.25);
      assert.equal(returned, m);
    });

    test("inverse() leaves the receiver untouched", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 10, 20]),
        inverted = m.inverse();

      assert.equal(inverted.a, 0.5);
      assert.equal(m.a, 2);
      assert.equal(m.d, 4);
      assert.equal(m.e, 10);
    });

    test("inverse() round-trips a point", () => {
      let m = new DOMMatrix([2, 0, 0, 4, 10, 20]),
        there = m.transformPoint({ x: 7, y: 9 }),
        back = m.inverse().transformPoint(there);

      assert.ok(Math.abs(back.x - 7) < 1e-9);
      assert.ok(Math.abs(back.y - 9) < 1e-9);
    });

    test("inverse() of a singular matrix is all-NaN and not 2D", () => {
      let m = new DOMMatrix([0, 0, 0, 0, 0, 0]).inverse();

      assert.equal(m.is2D, false);
      assert.ok(Number.isNaN(m.a));
      assert.ok(Number.isNaN(m.f));
    });

    test("multiply() accepts a plain DOMMatrixInit", () => {
      let m = new DOMMatrix().multiply({ a: 2, b: 0, c: 0, d: 3, e: 0, f: 0 });

      assert.equal(m.a, 2);
      assert.equal(m.d, 3);
    });

    test("multiply() with no argument is the identity", () => {
      assert.equal(new DOMMatrix([3, 0, 0, 3, 0, 0]).multiply().a, 3);
    });

    test("transformPoint() defaults the omitted DOMPointInit fields", () => {
      let p = new DOMMatrix().transformPoint({ x: 3, y: 4 });

      assert.equal(p.x, 3);
      assert.equal(p.y, 4);
      assert.equal(p.z, 0);
      assert.equal(p.w, 1);
    });

    test("transformPoint() with no argument is the origin", () => {
      let p = new DOMMatrix().transformPoint();

      assert.equal(p.x, 0);
      assert.equal(p.w, 1);
    });
  });

  // https://drafts.csswg.org/geometry/#dom-domrectreadonly-top -- each edge is
  // the NaN-safe min/max of the coordinate and coordinate+extent. Returning
  // `y` and `x + width` directly is only correct for non-negative extents.
  describe("DOMRect edges", () => {
    test("are unchanged for positive extents", () => {
      let r = new DOMRect(10, 10, 20, 15);

      assert.equal(r.left, 10);
      assert.equal(r.right, 30);
      assert.equal(r.top, 10);
      assert.equal(r.bottom, 25);
    });

    test("normalize negative extents rather than inverting", () => {
      let r = new DOMRect(10, 10, -6, -4);

      assert.equal(r.left, 4);
      assert.equal(r.right, 10);
      assert.equal(r.top, 6);
      assert.equal(r.bottom, 10);
      // The point of the spec rule: an edge pair can never come out reversed.
      assert.ok(r.left <= r.right);
      assert.ok(r.top <= r.bottom);
    });

    test("propagate NaN", () => {
      let r = new DOMRect(NaN, 10, 5, 5);

      assert.ok(Number.isNaN(r.left));
      assert.ok(Number.isNaN(r.right));
    });

    test("toJSON reports the normalized edges", () => {
      let json = new DOMRect(10, 10, -6, -4).toJSON();

      assert.equal(json.left, 4);
      assert.equal(json.right, 10);
    });

    // The edges are prototype accessors; x/y/width/height are own properties.
    // Spread has always copied the latter, and callers rely on it.
    test("spread still yields the stored fields", () => {
      assert.deepStrictEqual(
        { ...new DOMRect(1, 2, 3, 4) },
        { x: 1, y: 2, width: 3, height: 4 },
      );
    });
  });

  describe("static factories default their argument", () => {
    test("DOMPoint.fromPoint()", () => {
      let p = DOMPoint.fromPoint();
      assert.equal(p.x, 0);
      assert.equal(p.w, 1);
    });

    test("DOMRect.fromRect()", () => {
      let r = DOMRect.fromRect();
      assert.equal(r.x, 0);
      assert.equal(r.width, 0);
    });

    test("DOMMatrix.fromMatrix()", () => {
      assert.equal(DOMMatrix.fromMatrix().isIdentity, true);
    });
  });
});

describe("Canvas", () => {
  /** @type {any} */ let dir;

  beforeEach(() => (dir = tmp.dirSync().name));
  afterEach(() => fs.rmSync(dir, { recursive: true, force: true }));

  // The deprecation shims forwarded to the new method but dropped its return
  // value, so `await canvas.saveAs(...)` resolved before the write finished.
  test("saveAs() resolves only once the file is written", async () => {
    let canvas = new Canvas(16, 16),
      dst = path.join(dir, "out.png");

    await canvas.saveAs(dst);
    assert.equal(fs.existsSync(dst), true);
  });

  test("toDataURLSync() returns the data URL", () => {
    let url = new Canvas(16, 16).toDataURLSync("png");
    assert.equal(typeof url, "string");
    assert.ok(url.startsWith("data:image/png;base64,"));
  });

  // `gpu` reported the global default rather than what the constructor
  // selected, so it disagreed with `engine.renderer` for the whole life of the
  // canvas.
  //
  // The fix is native, so this fails against the previous release's binary --
  // which is exactly what `ci.yml` runs the current JS against. That failure
  // is true, not spurious: the published binary really does report the wrong
  // engine. It clears when the release carrying the fix ships, and until then
  // it is the documented cost of landing a native change (see AGENTS.md).
  //
  // Deliberately not skipped. Every gate cheap enough to write here also
  // matched a genuine regression, so skipping would have silenced the one
  // case this test exists for.
  //
  // Only visible on a host with a GPU: where none is reachable the old default
  // was CPU anyway, which is why this passed on Linux and failed on macOS.
  test("gpu agrees with the selected renderer", () => {
    let cpu = new Canvas(8, 8, { gpu: false });
    assert.equal(cpu.gpu, false);
    assert.equal(cpu.engine.renderer, "CPU");
  });
});

// `drawParagraph` reaches Skia's `Paragraph::paint`, which draws with the text
// styles' own paints. The context's paint state has to be applied around it or
// it is silently dropped -- so `globalAlpha` did nothing and every blend mode
// behaved as source-over. Native fix, so these fail against a binary that
// predates it, as the engine test above does.
describe("drawParagraph honours canvas paint state", () => {
  function paragraph() {
    let builder = ParagraphBuilder.Make({
      textStyle: { fontSize: 24, color: [0, 0, 0, 1] },
    });
    builder.addText("XXXX");

    let para = builder.build();
    para.layout(200);
    return para;
  }

  // Counts pixels by kind over a red backdrop the glyphs are drawn onto.
  function draw({ alpha = 1, op = "source-over" } = {}) {
    let canvas = new Canvas(120, 40),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "red";
    ctx.fillRect(0, 0, 120, 40);
    ctx.globalAlpha = alpha;
    ctx.globalCompositeOperation = op;
    ctx.drawParagraph(paragraph(), 2, 2);

    let data = ctx.getImageData(0, 0, 120, 40).data,
      tally = { red: 0, glyph: 0, transparent: 0 };

    for (let i = 0; i < data.length; i += 4) {
      if (data[i + 3] === 0) tally.transparent++;
      else if (data[i] > 200 && data[i + 1] < 60) tally.red++;
      else tally.glyph++;
    }
    return tally;
  }

  test("globalAlpha fades the glyphs", () => {
    let opaque = draw({ alpha: 1 }),
      faded = draw({ alpha: 0.5 });

    assert.ok(opaque.glyph > 0, "baseline should draw glyphs");
    // Half-opacity glyphs blend toward the red backdrop, so fewer pixels read
    // as glyph-coloured than at full opacity.
    assert.ok(
      faded.glyph < opaque.glyph,
      `expected fewer glyph pixels at 0.5 alpha, got ${faded.glyph} vs ${opaque.glyph}`,
    );
  });

  test("destination-out erases where the glyphs land", () => {
    let out = draw({ op: "destination-out" });

    assert.equal(out.glyph, 0);
    assert.ok(out.transparent > 0, "glyph area should be punched out");
  });

  test("copy discards what was already there", () => {
    let copied = draw({ op: "copy" });

    assert.equal(copied.red, 0);
    assert.ok(copied.glyph > 0);
  });

  test("the default path is unchanged", () => {
    let plain = draw();

    assert.ok(plain.red > 0 && plain.glyph > 0);
    assert.equal(plain.transparent, 0);
  });
});

// Wide-gamut and HDR output: the canvas composites in the named space and
// exports carry it. Fourteen names, seven spaces, each with an alias.
describe("colorSpace", () => {
  test("reports the canonical name for every alias", () => {
    let canonical = {
      srgb: "srgb",
      linear: "srgb-linear",
      "srgb-linear": "srgb-linear",
      p3: "display-p3",
      "display-p3": "display-p3",
      bt2020: "rec2020",
      rec2020: "rec2020",
      hdr10: "rec2020-pq",
      "rec2020-pq": "rec2020-pq",
      hlg: "rec2020-hlg",
      "rec2020-hlg": "rec2020-hlg",
    };

    for (let [asked, expected] of Object.entries(canonical)) {
      assert.equal(
        new Canvas(4, 4, { colorSpace: asked }).colorSpace,
        expected,
        `${asked} should report ${expected}`,
      );
    }
  });

  test("defaults to srgb", () => {
    assert.equal(new Canvas(4, 4).colorSpace, "srgb");
  });

  // Chrome throws on an invalid colorSpace. Quietly substituting sRGB meant a
  // caller could ask for HDR10, get none, and have nothing to go on.
  test("throws on a name it does not know", () => {
    for (let bad of ["nonsense", "displayp3", "SRGB"]) {
      assert.throws(
        () => new Canvas(4, 4, { colorSpace: bad }),
        /Unknown colorSpace/,
        `${bad} should be rejected`,
      );
    }
  });

  // The JS side keeps its own list of valid names, in lib/classes/imagery.js,
  // because `new ImageData()` has to reject a bad one where the mistake is.
  // Exercising every name through both sides is what stops that list drifting
  // from the parser in src/node/utils.rs.
  test("every name reads pixels back", () => {
    let names = [
      "srgb",
      "srgb-linear",
      "linear",
      "display-p3",
      "p3",
      "display-p3-linear",
      "p3-linear",
      "rec2020",
      "bt2020",
      "rec2020-linear",
      "bt2020-linear",
      "rec2020-pq",
      "hdr10",
      "rec2020-hlg",
      "hlg",
    ];

    let canvas = new Canvas(4, 4),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "rgb(255,0,0)";
    ctx.fillRect(0, 0, 4, 4);

    for (let colorSpace of names) {
      let data = ctx.getImageData(0, 0, 1, 1, { colorSpace }).data;
      assert.equal(data.length, 4, `${colorSpace} should return one pixel`);
      assert.equal(data[3], 255, `${colorSpace} should stay opaque`);
    }
  });

  // Same red, expressed in a wider gamut, is a smaller number: sRGB's most
  // saturated red sits inside P3 and well inside Rec. 2020.
  test("a wider space converts the values", () => {
    let canvas = new Canvas(4, 4),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "rgb(255,0,0)";
    ctx.fillRect(0, 0, 4, 4);

    let srgb = ctx.getImageData(0, 0, 1, 1, { colorSpace: "srgb" }).data,
      p3 = ctx.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data;

    assert.equal(srgb[0], 255);
    assert.ok(p3[0] < srgb[0], "red should be less saturated in P3");
    assert.ok(p3[1] > 0, "P3 red needs some green");
  });

  test("ImageData rejects a space it does not know", () => {
    assert.throws(
      () => new ImageData(2, 2, { colorSpace: "bogus" }),
      /Unsupported colorSpace/,
    );
  });

  test("exports carry the space", () => {
    function png(colorSpace) {
      let canvas = new Canvas(8, 8, { colorSpace }),
        ctx = canvas.getContext("2d");

      ctx.fillStyle = "red";
      ctx.fillRect(0, 0, 8, 8);
      return canvas.toBufferSync("png").toString("latin1");
    }

    // sRGB gets the compact sRGB chunk; a wide space needs a full ICC profile.
    assert.ok(png("srgb").includes("sRGB"));
    assert.ok(png("display-p3").includes("iCCP"));
    assert.ok(png("rec2020").includes("iCCP"));
  });
});

// Declared in the types since before this fork, never implemented -- upstream
// still ships both declarations against no implementation.
describe("declared API that had no implementation", () => {
  test("Canvas.contexts maps a canvas to its contexts", () => {
    let canvas = new Canvas(16, 16);
    canvas.getContext("2d");

    assert.ok(Canvas.contexts instanceof WeakMap);
    assert.equal(Canvas.contexts.get(canvas).length, 1);

    // Holds the live array, so later pages show up without re-registering.
    canvas.newPage(16, 16);
    assert.equal(Canvas.contexts.get(canvas).length, 2);
  });

  test("toSharpSync() returns the same image as toSharp()", async () => {
    let canvas = new Canvas(32, 20),
      ctx = canvas.getContext("2d");

    ctx.fillStyle = "#3366cc";
    ctx.fillRect(0, 0, 32, 20);
    ctx.fillStyle = "#ffcc00";
    ctx.fillRect(4, 4, 10, 8);

    // Compare decoded pixels, not encoded PNG bytes: the byte stream depends
    // on sharp's encoder and is not stable run to run, which made an earlier
    // version of this test flaky for reasons that had nothing to do with the
    // canvas. Sequential, so the two reads cannot interleave.
    let asynchronous = await canvas.toSharp().raw().toBuffer(),
      synchronous = await canvas.toSharpSync().raw().toBuffer();

    assert.equal(synchronous.length, asynchronous.length);
    assert.equal(Buffer.compare(asynchronous, synchronous), 0);
  });
});

// `new` used to hand back an instance with no boxed struct behind it. Because
// every consumer in context.js gates on `instanceof`, that forgery passed
// validation and then did nothing: `ctx.fillStyle = new Shader()` was accepted,
// read back as "#000000", and filled black without ever raising an error.
//
// The constructors allocate now, so the only way to hold one of these is to
// hold a real one.
describe("factory-backed classes", () => {
  // MakeDropShadowOnly -> "drop-shadow-only", MakeSRGBToLinearGamma ->
  // "srgb-to-linear-gamma". Every kind name derives this way, so the tables in
  // filter.js cannot drift from the statics without this failing.
  let kindFromFactory = (name) =>
    name
      .replace(/^Make/, "")
      .replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2")
      .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
      .toLowerCase();

  // Ask a constructor which kinds it accepts by reading the refusal it prints.
  let kindsAccepted = (cls) => {
    try {
      new cls(" no such kind");
      assert.fail(`${cls.name} accepted a nonsense kind`);
    } catch (error) {
      return [...error.message.matchAll(/"([a-z0-9-]+)"/g)]
        .map((m) => m[1])
        .sort();
    }
  };

  describe("every static factory has a matching constructor kind", () => {
    // "luma" is the one name that does not derive mechanically: strict
    // derivation gives "luma-color-filter", which repeats the class name.
    for (let [cls, exceptions] of [
      [ColorFilter, { "luma-color-filter": "luma" }],
      [ImageFilter, {}],
      [Shader, {}],
    ]) {
      test(cls.name, () => {
        let expected = Object.getOwnPropertyNames(cls)
          .filter((name) => name.startsWith("Make"))
          .map(
            (name) =>
              exceptions[kindFromFactory(name)] ?? kindFromFactory(name),
          )
          .sort();

        assert.ok(
          expected.length > 1,
          `expected several ${cls.name} factories`,
        );
        assert.deepStrictEqual(
          kindsAccepted(cls),
          expected,
          `${cls.name}'s kind table and its Make* methods have drifted apart`,
        );
      });
    }
  });

  describe("a constructed instance reaches the render", () => {
    test("Shader paints noise rather than falling back to black", () => {
      let ctx = new Canvas(40, 40).getContext("2d");
      ctx.fillStyle = new Shader("turbulence", 0.08, 0.08, 4, 0);
      ctx.fillRect(0, 0, 40, 40);

      let data = ctx.getImageData(0, 0, 40, 40).data,
        colors = new Set();
      for (let i = 0; i < data.length; i += 4) {
        colors.add(`${data[i]},${data[i + 1]},${data[i + 2]}`);
      }

      // A forged shader used to leave a single flat colour here.
      assert.ok(
        colors.size > 50,
        `expected noise, got ${colors.size} distinct colours`,
      );
    });

    test("MaskFilter blurs the edge it is given", () => {
      let ctx = new Canvas(40, 40).getContext("2d");
      ctx.maskFilter = new MaskFilter("normal", 6);
      ctx.fillStyle = "white";
      ctx.fillRect(12, 12, 16, 16);

      let alpha = ctx.getImageData(12, 12, 1, 1).data[3];
      assert.ok(
        alpha > 0 && alpha < 255,
        `expected a soft edge, got alpha ${alpha}`,
      );
    });

    test("ImageFilter spreads ink beyond the shape", () => {
      let ctx = new Canvas(40, 40).getContext("2d");
      ctx.imageFilter = new ImageFilter("blur", 3, 3);
      ctx.fillStyle = "white";
      ctx.fillRect(14, 14, 12, 12);

      // A pixel just outside the rect, which only a blur can reach.
      assert.ok(ctx.getImageData(14, 13, 1, 1).data[3] > 0);
    });

    test("ColorFilter transforms the colour drawn", () => {
      let ctx = new Canvas(20, 20).getContext("2d");
      ctx.colorFilter = new ColorFilter("luma");
      ctx.fillStyle = "red";
      ctx.fillRect(0, 0, 20, 20);

      let [r, g, b] = ctx.getImageData(5, 5, 1, 1).data;
      assert.notDeepStrictEqual([r, g, b], [255, 0, 0], "filter was ignored");
    });

    test("ParagraphBuilder builds a paragraph that lays out and draws", () => {
      let para = new ParagraphBuilder({ textStyle: { fontSize: 16 } })
        .addText("hello")
        .build();
      para.layout(200);
      assert.ok(para.getHeight() > 0);

      let ctx = new Canvas(200, 60).getContext("2d");
      ctx.fillStyle = "white";
      ctx.drawParagraph(para, 0, 0);

      let data = ctx.getImageData(0, 0, 200, 60).data,
        inked = false;
      for (let i = 3; i < data.length && !inked; i += 4) inked = data[i] > 0;
      assert.ok(inked, "drawParagraph left the canvas blank");
    });
  });

  describe("arguments Skia rejects throw rather than yielding a shell", () => {
    for (let [label, build] of [
      ["MaskFilter with sigma 0", () => new MaskFilter("normal", 0)],
      ["MaskFilter with negative sigma", () => new MaskFilter("normal", -1)],
      ["Shader with an unknown kind", () => new Shader("bogus", 1, 1, 1, 1)],
      ["ColorFilter with an unknown kind", () => new ColorFilter("bogus")],
      ["ImageFilter with an unknown kind", () => new ImageFilter("bogus")],
    ]) {
      test(label, () => assert.throws(build, TypeError));
    }

    // An invalid enum in a method argument is a TypeError under WebIDL, and
    // the declarations type all of these as string unions. Substituting a
    // default made the union a lie: MakeBlur("bogus", 4) returned a normal
    // blur and MakeBlend("colorDodge", ...) composited source-over.
    //
    // `globalCompositeOperation` is deliberately not in this list -- the
    // Canvas standard requires it to ignore a name it does not recognise,
    // which is why the standard parser stayed separate from the filter one.
    describe("an unrecognised enum name is refused, not substituted", () => {
      for (let [label, build] of [
        ["blur style", () => MaskFilter.MakeBlur("bogus", 4)],
        ["blur style, via new", () => new MaskFilter("bogus", 4)],
        [
          "blend mode on a ColorFilter",
          () => ColorFilter.MakeBlend("red", "bogus"),
        ],
        ["blend mode on an ImageFilter", () => ImageFilter.MakeBlend("bogus")],
        ["tile mode", () => ImageFilter.MakeBlur(4, 4, "bogus")],
        ["colour channel", () => ImageFilter.MakeDisplacementMap("Z", "A", 1)],
        [
          "colour string",
          () => ColorFilter.MakeBlend("notacolour", "multiply"),
        ],
      ]) {
        test(label, () => assert.throws(build, TypeError));
      }

      test("an omitted optional argument still takes its default", () => {
        assert.ok(ImageFilter.MakeBlur(4, 4) !== null);
        assert.ok(ImageFilter.MakeBlur(4, 4, null) !== null);
        assert.ok(ImageFilter.MakeBlur(4, 4, undefined) !== null);
      });

      test("the message quotes the name as it was typed", () => {
        assert.throws(
          () => ImageFilter.MakeDisplacementMap("Zed", "A", 1),
          /"Zed"/,
        );
      });
    });

    // The camelCase spellings were matched against an already-lowercased
    // string, so every one of them fell through to the default: "colorDodge"
    // composited source-over rather than dodging, silently.
    test("CanvasKit's camelCase blend names resolve", () => {
      let render = (mode) => {
        let ctx = new Canvas(8, 8).getContext("2d");
        ctx.fillStyle = "#404040";
        ctx.fillRect(0, 0, 8, 8);
        ctx.imageFilter = ImageFilter.MakeBlend(mode, null, null);
        ctx.fillStyle = "#8080ff";
        ctx.fillRect(0, 0, 8, 8);
        return [...ctx.getImageData(4, 4, 1, 1).data].join();
      };

      assert.equal(render("colorDodge"), render("color-dodge"));
      assert.notEqual(render("colorDodge"), render("source-over"));
      assert.equal(render("srcOver"), render("source-over"));
    });

    // The two filter files each carried their own blend-mode table, and
    // neither matched the standard one. Consolidating them dropped eight
    // spellings on the first attempt -- both files had accepted the short
    // hyphenated forms, and only this list caught it.
    test("every spelling either file accepted still resolves", () => {
      let spellings = [
        "src-over",
        "dst-over",
        "src-in",
        "dst-in",
        "src-out",
        "dst-out",
        "src-atop",
        "dst-atop",
        "srcover",
        "srcOver",
        "source-over",
        "src",
        "dst",
        "plus",
        "plus-lighter",
        "lighter",
        "colorDodge",
        "color-dodge",
      ];

      // Resolving the name is the property under test, so this asks only
      // whether the parser recognised it. A recognised mode can still yield
      // null: Skia declines the degenerate combinations -- a solid-colour
      // blend under "dst-in" discards the colour, leaving nothing to build --
      // and that is a different answer from "no such name".
      for (let mode of spellings) {
        assert.doesNotThrow(
          () => ColorFilter.MakeBlend("red", mode),
          `ColorFilter.MakeBlend rejected "${mode}"`,
        );
        assert.doesNotThrow(
          () => ImageFilter.MakeBlend(mode),
          `ImageFilter.MakeBlend rejected "${mode}"`,
        );
      }
    });

    // Colours took the same treatment as the enums: a typo used to paint with
    // a colour the caller never chose -- black for a shadow, white for the
    // multiply term of a lighting filter.
    test("an unparseable colour is refused wherever one is taken", () => {
      assert.throws(
        () => ColorFilter.MakeBlend("bogus", "multiply"),
        TypeError,
      );
      assert.throws(
        () => ColorFilter.MakeLighting("white", "bogus"),
        TypeError,
      );
      assert.throws(
        () => ImageFilter.MakeDropShadow(2, 2, 3, 3, "bogus"),
        TypeError,
      );

      // The array form and an omitted colour are unaffected.
      assert.ok(ImageFilter.MakeDropShadow(2, 2, 3, 3, [0, 0, 0, 1]) !== null);
      assert.ok(ImageFilter.MakeDropShadow(2, 2, 3, 3) !== null);
    });

    // Raw neon downcast failures named no parameter: "failed to downcast any
    // to number" told a caller nothing about which argument was wrong.
    test("a missing argument names the parameter it wanted", () => {
      assert.throws(() => new MaskFilter(), /sigma/);
      assert.throws(() => MaskFilter.MakeBlur(), /sigma/);
    });

    // The kind tables route through the statics, so the argument checking the
    // factories already carried has to survive the trip.
    test("the factories' own validation still applies", () => {
      // A sequence of the wrong length is a `TypeError` -- AGENTS.md's rule 3
      // -- and this asserted a `RangeError`, which is what the factory raised
      // before the rule reached it. The two rows below it separate the length
      // from the type: `null` is not a sequence at all, and used to be told
      // the same thing.
      assert.throws(() => new ColorFilter("matrix", [1, 2, 3]), {
        name: "TypeError",
        message: /got 3/,
      });
      assert.throws(() => new ColorFilter("matrix", null), TypeError);
      assert.throws(() => new ColorFilter("compose", 1, 2), TypeError);
      assert.throws(() => new ImageFilter("color-filter", 42), TypeError);
    });
  });

  describe("results of an operation are not constructible", () => {
    // The browser has no TextMetrics constructor either, and a paragraph
    // cannot be described by arguments -- a builder can carry several styled
    // runs, which text plus one style could not express.
    test("new Paragraph() throws", () =>
      assert.throws(() => new Paragraph(), TypeError));

    test("new TextMetrics() throws", () =>
      assert.throws(() => new TextMetrics(), TypeError));

    test("but measureText and build still produce them", () => {
      let ctx = new Canvas(100, 50).getContext("2d");
      assert.ok(ctx.measureText("hello") instanceof TextMetrics);

      let para = new ParagraphBuilder({}).addText("hi").build();
      assert.ok(para instanceof Paragraph);
    });
  });
});

// Standard members that were declared-but-commented, or absent entirely, while
// the machinery to answer them was already here.
describe("standard members", () => {
  test("isContextLost() is false", () => {
    // There is no compositor to lose a backing store to. A canvas either has
    // its surface or its construction failed.
    assert.equal(new Canvas(8, 8).getContext("2d").isContextLost(), false);
  });

  test("naturalWidth and naturalHeight report the intrinsic size", async () => {
    let canvas = new Canvas(23, 17),
      image = await loadImage(await canvas.toBuffer("png"));

    assert.equal(image.naturalWidth, 23);
    assert.equal(image.naturalHeight, 17);
    // No layout here, so these are the same measurement as width/height.
    assert.equal(image.naturalWidth, image.width);
    assert.equal(image.naturalHeight, image.height);
  });

  test("complete is derived, not assignable", () => {
    let image = new Image();
    assert.equal(image.complete, false);

    // A getter with no setter, which the declaration used to offer as a
    // settable field. Asserted on the descriptor rather than by assigning,
    // because the symptom depends on the caller: strict mode throws where
    // sloppy mode discards the write in silence.
    let accessor = Object.getOwnPropertyDescriptor(
      Object.getPrototypeOf(image),
      "complete",
    );
    assert.equal(typeof accessor?.get, "function");
    assert.equal(accessor?.set, undefined);
  });

  describe("toBlob", () => {
    test("hands a Blob of the requested type to its callback", async () => {
      let canvas = new Canvas(12, 12),
        ctx = canvas.getContext("2d");
      ctx.fillStyle = "red";
      ctx.fillRect(0, 0, 12, 12);

      let blob = await new Promise((resolve) => canvas.toBlob(resolve));
      assert.ok(blob instanceof Blob);
      assert.equal(blob.type, "image/png");
      assert.ok(blob.size > 0);

      let jpeg = await new Promise((resolve) =>
        canvas.toBlob(resolve, "image/jpeg", 0.5),
      );
      assert.equal(jpeg.type, "image/jpeg");
    });

    test("requires a callback", () => {
      assert.throws(() => new Canvas(8, 8).toBlob(), TypeError);
    });
  });

  describe("fontVariantCaps", () => {
    /** @type {any} */ let ctx;
    beforeEach(() => (ctx = new Canvas(16, 16).getContext("2d")));

    test("defaults to normal", () => {
      assert.equal(ctx.fontVariantCaps, "normal");
    });

    test("round-trips every value the standard defines", () => {
      for (let caps of [
        "small-caps",
        "all-small-caps",
        "petite-caps",
        "all-petite-caps",
        "unicase",
        "titling-caps",
      ]) {
        ctx.fontVariantCaps = caps;
        assert.equal(ctx.fontVariantCaps, caps);
      }
    });

    // It is the longhand of fontVariant, so it owns the caps token and
    // nothing else.
    test("leaves the other variant axes alone", () => {
      ctx.fontVariant = "small-caps oldstyle-nums";
      assert.equal(ctx.fontVariantCaps, "small-caps");

      ctx.fontVariantCaps = "titling-caps";
      assert.match(ctx.fontVariant, /oldstyle-nums/);
      assert.match(ctx.fontVariant, /titling-caps/);

      ctx.fontVariantCaps = "normal";
      assert.equal(ctx.fontVariantCaps, "normal");
      assert.match(ctx.fontVariant, /oldstyle-nums/);
    });

    test("ignores a value it does not recognise", () => {
      ctx.fontVariantCaps = "small-caps";
      ctx.fontVariantCaps = "bogus";
      assert.equal(ctx.fontVariantCaps, "small-caps");
    });

    // `normal` is what the getter returns by default, and setting it threw:
    // the parser returned `variants` where every other path returns
    // `variant`, so a variant could be set but never cleared.
    test("fontVariant accepts normal", () => {
      ctx.fontVariant = "small-caps";
      ctx.fontVariant = "normal";
      assert.equal(ctx.fontVariant, "normal");
    });
  });
});
