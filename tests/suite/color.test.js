// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary, ParagraphBuilder } = require("../../lib");

describe("createImageData", () => {
  test("inherits the canvas's colour space", () => {
    // The standard says so directly: "Initialize newImageData given the
    // absolute magnitude of sw, the absolute magnitude of sh, settings, and
    // defaultColorSpace set to this's color space." This overload passed its
    // settings straight through and let `ImageData` default the space, so a
    // Display P3 canvas handed back a buffer labelled sRGB.
    let ctx = new Canvas(8, 8, { colorSpace: "display-p3" }).getContext("2d");
    assert.equal(ctx.createImageData(2, 2).colorSpace, "display-p3");

    let plain = new Canvas(8, 8).getContext("2d");
    assert.equal(plain.createImageData(2, 2).colorSpace, "srgb");
  });

  test("inherits the canvas's pixel format too", () => {
    // Not the standard's requirement -- `colorType` is this crate's -- but
    // `getImageData` and the cloning overload both inherit it, and a
    // `createImageData` that did not would describe a different buffer from
    // the one `getImageData` returns for the same canvas.
    let ctx = new Canvas(8, 8, { colorType: "RGBAF16" }).getContext("2d");
    assert.equal(ctx.createImageData(2, 2).colorType, "RGBAF16");
    assert.equal(ctx.getImageData(0, 0, 2, 2).colorType, "RGBAF16");
  });

  test("labels the buffer so putImageData reads the bytes as written", () => {
    // The label is not decoration: it decides how `putImageData` interprets
    // the components. Mislabelled sRGB, these P3 bytes were converted into P3
    // on the way in and landed on [215, 69, 50].
    let canvas = new Canvas(4, 4, { colorSpace: "display-p3" });
    let ctx = canvas.getContext("2d");

    let buffer = ctx.createImageData(2, 2);
    for (let i = 0; i < buffer.data.length; i += 4)
      buffer.data.set([234, 51, 35, 255], i);
    ctx.putImageData(buffer, 0, 0);

    assert.deepEqual(
      Array.from(
        ctx.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data,
      ),
      [234, 51, 35, 255],
    );
  });

  test("the two forms that were already right are unchanged", () => {
    // These are the controls that localised the defect to the size overload:
    // both already inherited, so a fix that moved them would have been
    // fixing the wrong thing.
    let canvas = new Canvas(4, 4, { colorSpace: "display-p3" });
    let ctx = canvas.getContext("2d");

    let source = ctx.getImageData(0, 0, 2, 2);
    assert.equal(source.colorSpace, "display-p3");
    assert.equal(
      ctx.createImageData(source).colorSpace,
      "display-p3",
      "the cloning overload takes its source's space",
    );

    assert.equal(
      ctx.createImageData(2, 2, { colorSpace: "srgb" }).colorSpace,
      "srgb",
      "an explicit setting still wins over the canvas",
    );
  });
});

describe("colour forms no CSS defines", () => {
  // `hwba()`, `hsv()` and `hsva()`, absolute or relative, and hex without its
  // `#` are no CSS colours, and Chrome 153 ignores each as a `fillStyle`. The
  // setter ignores them as HTML says it ignores any value that does not
  // parse, leaving the style as it was.
  test("are ignored, leaving the style as it was", () => {
    const ctx = new Canvas(4, 4).getContext("2d");
    for (const css of [
      "hwba(0 0% 0%)",
      "hsv(0 100% 100%)",
      "hsva(0 100% 100% / 0.5)",
      "ff0000",
      "f00",
      "hwba(from red h w b)",
      "hsv(from red h s v)",
      "rgb (255 0 0)",
      "lab (50 40 30)",
      "color (srgb 1 0 0)",
    ]) {
      ctx.fillStyle = "#123456";
      ctx.fillStyle = css;
      assert.equal(ctx.fillStyle, "#123456", css);
      ctx.strokeStyle = "#123456";
      ctx.strokeStyle = css;
      assert.equal(ctx.strokeStyle, "#123456", css);
    }
    ctx.fillStyle = "hwb(0 0% 0%)";
    assert.equal(ctx.fillStyle, "#ff0000", "hwb() itself still parses");
  });
});

describe("legacy colour syntax CSS Color 4 does not allow", () => {
  // Chrome 153 ignores each as a `fillStyle`, and so does the setter here,
  // as HTML says it ignores any value that does not parse.
  test("is ignored, leaving the style as it was", () => {
    const ctx = new Canvas(4, 4).getContext("2d");
    for (const css of [
      "rgb(255, 0%, 0)",
      "rgb(255,0 0)",
      "hsl(120, 100, 50)",
      "rgb(none, 0, 0)",
      "rgb(255, 0, 0, none)",
      "hwb(120, 0%, 0%)",
      "lab(50, 40, 30)",
      "oklch(0.6, 0.2, 30)",
      "color(srgb 1, 0, 0)",
    ]) {
      ctx.fillStyle = "#123456";
      ctx.fillStyle = css;
      assert.equal(ctx.fillStyle, "#123456", css);
    }
    ctx.fillStyle = "rgb(255, 0, 0)";
    assert.equal(ctx.fillStyle, "#ff0000", "lawful legacy syntax parses");
  });
});

describe("a [r, g, b, a] float array", () => {
  // sRGB components from 0 to 1 with straight alpha on every canvas, so an
  // array paints what `color(srgb r g b / a)` paints. `fillStyle` read it in
  // the canvas's own space, so on a P3 canvas `[1, 0, 0, 1]` was P3 red, and
  // a paragraph read it as linear light, so the same array painted a third
  // colour there.
  //
  // Outside sRGB and inside P3: red above one, green below zero. An sRGB
  // canvas clips it and a wide one does not, so a reader that takes the
  // canvas's frame, or clips on the way in, lands somewhere else.
  const ARRAY = [1.1, -0.1, 0.05, 1];
  const CSS = "color(srgb 1.1 -0.1 0.05)";
  const SPACES = ["srgb", "display-p3", "rec2020"];
  // An eight-bit Rec. 2020 canvas steps coarser than P3, so a colour comes
  // back from it a few levels from where it went in.
  const levels = (space) => (space == "rec2020" ? 4 : 1);
  const within = (got, want, space) =>
    got.every((v, i) => Math.abs(v - want[i]) <= levels(space));

  // The pixel at (30, 20), read in `readIn`, Display P3 unless given, so a
  // wide colour shows.
  function sample(space, draw, readIn = "display-p3") {
    let canvas = new Canvas(60, 40, { colorSpace: space });
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    draw(ctx);
    return Array.from(
      ctx.getImageData(30, 20, 1, 1, { colorSpace: readIn }).data,
    );
  }
  const fill = (colour) => (ctx) => {
    ctx.fillStyle = colour;
    ctx.fillRect(0, 0, 60, 40);
  };
  const paragraph = (textStyle) => (ctx) => {
    let pb = ParagraphBuilder.Make({
      textStyle: { fontSize: 40, ...textStyle },
    });
    pb.addText("MMMMMMMMMM");
    let built = pb.build();
    built.layout(1000);
    ctx.drawParagraph(built, 0, 0);
  };

  test("is the sRGB colour its components name, on every canvas", () => {
    for (let space of SPACES) {
      let got = sample(space, fill([0.2, 0.5, 0.8, 1]), "srgb");
      assert.ok(
        within(got, [51, 128, 204, 255], space),
        `on ${space}: ${got}, want 51, 128, 204`,
      );
    }
  });

  test("is the colour the same color(srgb) string names, in every reader", () => {
    let onSrgb = sample("srgb", fill(CSS));
    let onP3 = sample("display-p3", fill(CSS));
    // The separation the rest of this test relies on: if a wide canvas
    // showed this colour as an sRGB one does, the cells below could not
    // tell the frames apart.
    assert.ok(
      Math.abs(onP3[0] - onSrgb[0]) > 8,
      `${onP3} on P3 against ${onSrgb} on sRGB`,
    );
    for (let space of SPACES) {
      let want = sample(space, fill(CSS));
      let readers = {
        fillStyle: fill(ARRAY),
        strokeStyle: (ctx) => {
          ctx.strokeStyle = ARRAY;
          ctx.lineWidth = 40;
          ctx.beginPath();
          ctx.moveTo(0, 20);
          ctx.lineTo(60, 20);
          ctx.stroke();
        },
        addColorStop: (ctx) => {
          let gradient = ctx.createLinearGradient(0, 0, 60, 0);
          gradient.addColorStop(0, ARRAY);
          gradient.addColorStop(1, ARRAY);
          ctx.fillStyle = gradient;
          ctx.fillRect(0, 0, 60, 40);
        },
        "a paragraph's backgroundColor": paragraph({
          color: [0, 0, 0, 0],
          backgroundColor: ARRAY,
        }),
      };
      for (let [name, draw] of Object.entries(readers)) {
        let got = sample(space, draw);
        assert.ok(
          within(got, want, space),
          `${name} on ${space}: ${got}, the string paints ${want}`,
        );
      }
    }
  });

  test("has straight alpha", () => {
    // Half-transparent: the array is the string's colour at half alpha.
    // Read as premultiplied, it would be twice as bright.
    for (let space of SPACES) {
      let want = sample(space, fill("color(srgb 0.2 0.5 0.8 / 0.5)"));
      let got = sample(space, fill([0.2, 0.5, 0.8, 0.5]));
      assert.ok(
        within(got, want, space),
        `on ${space}: ${got}, the string paints ${want}`,
      );
    }
  });

  test("is the same colour in a paragraph as in a fill", () => {
    // A paragraph's text colour, here a glyph's ink, against a fill.
    let ink = (colour) => {
      let canvas = new Canvas(200, 80);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      paragraph({ fontSize: 80, color: colour })(ctx);
      let { data } = ctx.getImageData(0, 0, 200, 80);
      // The most opaque pixel is wholly inside a stroke of the M.
      let best = 0;
      for (let i = 0; i < data.length; i += 4)
        if (data[i + 3] > data[best + 3]) best = i;
      return Array.from(data.slice(best, best + 4));
    };
    let colour = [0.2, 0.5, 0.8, 1];
    assert.deepEqual(ink(colour), sample("srgb", fill(colour), "srgb"));
  });

  test("decorates and shadows text as the same string does", () => {
    // Drawn in the colour as named, whichever form named it.
    let pixels = (colour) => {
      let canvas = new Canvas(200, 80);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      let pb = ParagraphBuilder.Make({
        textStyle: {
          fontSize: 40,
          color: "black",
          decoration: 1,
          decorationColor: colour,
          shadows: [{ color: colour, offset: [6, 6] }],
        },
      });
      pb.addText("MMMM");
      let built = pb.build();
      built.layout(200);
      ctx.drawParagraph(built, 0, 0);
      return Array.from(ctx.getImageData(0, 0, 200, 80).data);
    };
    let fromString = pixels(CSS);
    assert.ok(
      fromString.some((v, i) => i % 4 == 0 && v > 200),
      "the decoration and shadow are drawn, in a red",
    );
    assert.deepEqual(pixels(ARRAY), fromString);
    // And against a byte, since the comparison above holds for any reading
    // both forms share.
    let grey = 128 / 255;
    assert.deepEqual(pixels([grey, grey, grey, 1]), pixels("#808080"));
  });

  // SkParagraph holds these two in eight bits, and they were drawn in that:
  // on a float canvas an alpha of 0.3 came out at 77/255.
  test("decorates and shadows text at the precision the style names", () => {
    let strongest = (textStyle, x) => {
      let canvas = new Canvas(64, 64, { colorType: "RGBAF32" });
      let ctx = canvas.getContext("2d");
      let pb = ParagraphBuilder.Make({ textStyle });
      pb.addText("HH");
      let built = pb.build();
      built.layout(400);
      ctx.drawParagraph(built, x, 0);
      let { data } = ctx.getImageData(0, 0, 64, 64, { colorType: "RGBAF32" });
      let floats = new Float32Array(
        data.buffer,
        data.byteOffset,
        data.length / 4,
      );
      let alpha = 0;
      for (let i = 3; i < floats.length; i += 4)
        alpha = Math.max(alpha, floats[i]);
      return alpha;
    };
    let faint = "rgb(0 0 0 / 0.3)";
    let base = { fontSize: 40, color: "transparent" };
    let line = strongest(
      {
        ...base,
        decoration: 1,
        decorationColor: faint,
        decorationThickness: 4,
      },
      0,
    );
    let shadow = strongest(
      { ...base, shadows: [{ color: faint, offset: [200, 0] }] },
      -200,
    );
    for (let [what, alpha] of [
      ["line", line],
      ["shadow", shadow],
    ]) {
      assert.ok(Math.abs(alpha - 0.3) < 1e-4, `the ${what} at ${alpha}`);
    }
  });
});

describe("a wide CSS colour", () => {
  // `shadowColor` and a texture's colour take a string only, as they did,
  // and keep the space it names: they went through eight-bit sRGB, so P3 red
  // was clipped even on a P3 canvas.
  test("reaches a wide canvas through every string-only reader", () => {
    let canvas = new Canvas(60, 40, { colorSpace: "display-p3" });
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    let read = () =>
      Array.from(
        ctx.getImageData(30, 20, 1, 1, { colorSpace: "display-p3" }).data,
      );

    ctx.shadowColor = "color(display-p3 1 0 0)";
    ctx.shadowOffsetX = 30;
    ctx.fillRect(0, 0, 30, 40);
    assert.deepEqual(read(), [255, 0, 0, 255], "shadowColor");
    assert.equal(ctx.shadowColor, "color(display-p3 1 0 0)");

    ctx.shadowColor = "transparent";
    // Lines wider than their spacing, so the texture covers the pixel.
    ctx.fillStyle = ctx.createTexture(4, {
      color: "color(display-p3 1 0 0)",
      line: 12,
    });
    ctx.fillRect(0, 0, 60, 40);
    assert.deepEqual(read(), [255, 0, 0, 255], "a texture's color");

    // And the matte under an export, on a canvas with nothing drawn.
    let blank = new Canvas(2, 2, { colorSpace: "display-p3" });
    assert.deepEqual(
      Array.from(
        blank
          .toBufferSync("raw", {
            colorSpace: "display-p3",
            matte: "color(display-p3 1 0 0)",
          })
          .slice(0, 4),
      ),
      [255, 0, 0, 255],
      "an export's matte",
    );
  });
});

describe("a paragraph on a float canvas", () => {
  // SkParagraph paints in the paints it was built with, which Skia holds
  // converted to sRGB through an approximate transfer function: on an sRGB
  // F32 canvas `color(display-p3 0.3 0.6 0.2)` landed 2.4e-4 from a fill of
  // it.
  test("lands in its colours as a fill does", () => {
    FontLibrary.use("ColourOswald", [
      "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
    ]);
    let side = 120;
    let page = (colorSpace) => {
      let canvas = new Canvas(side, side, { colorType: "RGBAF32", colorSpace });
      canvas.gpu = false;
      return canvas.getContext("2d");
    };
    // Every pixel covered whole, premultiplied floats in the page's space.
    let covered = (ctx, colorSpace) => {
      let { data } = ctx.getImageData(0, 0, side, side, {
        colorType: "RGBAF32",
        colorSpace,
      });
      let floats = new Float32Array(
        data.buffer,
        data.byteOffset,
        data.length / 4,
      );
      let whole = [];
      for (let i = 0; i < floats.length; i += 4)
        if (floats[i + 3] == 1)
          whole.push(Array.from(floats.subarray(i, i + 4)));
      return whole;
    };
    let clear = "transparent";
    // Each of a style's colours, drawn so that it alone covers some of the
    // page whole, and how far up the paragraph is drawn.
    let sites = {
      color: (c) => [{ color: c }, -20],
      // An outline thick enough to cover some of the page whole.
      strokeWidth: (c) => [{ color: c, strokeWidth: 40 }, -20],
      foregroundColor: (c) => [{ color: "red", foregroundColor: c }, -20],
      backgroundColor: (c) => [{ color: clear, backgroundColor: c }, -20],
      shadow: (c) => [
        { color: clear, shadows: [{ color: c, offset: [0, 400] }] },
        -420,
      ],
      decorationColor: (c) => [
        {
          color: clear,
          decoration: 1,
          decorationColor: c,
          decorationThickness: 30,
        },
        -80,
      ],
    };
    let wrong = [];
    for (let space of ["srgb", "display-p3"]) {
      for (let colour of [
        "rgb(20 120 220)",
        "color(display-p3 0.3 0.6 0.2)",
        "color(srgb-linear 0.5 0.25 0.75)",
      ]) {
        let fill = page(space);
        fill.fillStyle = colour;
        fill.fillRect(0, 0, side, side);
        let want = covered(fill, space)[0];
        for (let [name, site] of Object.entries(sites)) {
          let [style, y] = site(colour);
          let pb = ParagraphBuilder.Make({
            textStyle: {
              fontFamilies: ["ColourOswald"],
              fontSize: 140,
              fontWeight: 700,
              ...style,
            },
          });
          pb.addText("M");
          let built = pb.build();
          built.layout(1000);
          let ctx = page(space);
          ctx.drawParagraph(built, -10, y);
          let got = covered(ctx, space);
          if (got.length == 0)
            wrong.push(`${space} ${colour} ${name}: nothing covered`);
          else if (got.some((p) => p.some((v, i) => v !== want[i])))
            wrong.push(
              `${space} ${colour} ${name}: ${got.find((p) => p.some((v, i) => v !== want[i]))} for ${want}`,
            );
        }
      }
    }
    assert.deepEqual(wrong, []);
  });
});
