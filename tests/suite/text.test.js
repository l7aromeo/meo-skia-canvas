// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas } = require("../../lib");

describe("letterSpacing measures a space after every character", () => {
  // CSS adds `letter-spacing` after each character including the last, so an
  // `n`-character run is `n` spaces wider than an unspaced one. This measured
  // `n - 1`: a whole space was subtracted from the laid-out box, so a
  // one-character string at any spacing measured exactly what it measures at
  // no spacing at all.
  const ctx = () => {
    const c = new Canvas(600, 80),
      ctx = c.getContext("2d");
    ctx.font = "40px Helvetica";
    return ctx;
  };

  // Where the first and last inked columns fall for a left-aligned draw at
  // x=0, so the rendering can be compared separately from the advance.
  const ink = (ctx, text) => {
    ctx.clearRect(0, 0, 600, 80);
    ctx.fillStyle = "black";
    ctx.fillText(text, 0, 60);
    const d = ctx.getImageData(0, 0, 600, 80).data;
    let lo = 600,
      hi = -1;
    for (let i = 3; i < d.length; i += 4)
      if (d[i] > 0) {
        const x = ((i - 3) / 4) % 600;
        if (x < lo) lo = x;
        if (x > hi) hi = x;
      }
    return hi < 0 ? null : [lo, hi];
  };

  test("one space per character, including a string of one", () => {
    const c = ctx();
    // The anchor: the unspaced widths are what the spaced ones are measured
    // against, so they have to be established first. Without this the loop
    // below is satisfied by any implementation that adds a constant, or by
    // one that adds nothing to either side of the subtraction.
    c.letterSpacing = "0px";
    const bare = ["a", "ab", "abc", "abcd"].map((s) => c.measureText(s).width);
    assert.ok(
      bare[3] > bare[0],
      `unspaced widths must grow with the string: ${bare}`,
    );

    c.letterSpacing = "10px";
    ["a", "ab", "abc", "abcd"].forEach((s, i) => {
      // A one-character string is the case that separates the two rules: at
      // `n - 1` it gains nothing at all.
      assert.nearEqual(
        c.measureText(s).width,
        bare[i] + 10 * s.length,
        `"${s}" gains one space per character`,
      );
    });
  });

  test("and the drawn glyphs do not move when it changes", () => {
    // The subtraction only ever reached the reported box. Asserting that the
    // ink is unchanged is what keeps a future fix from paying for the width
    // by shifting the text: the two halves have to move together or not at
    // all, and here they do not.
    const c = ctx();
    c.letterSpacing = "0px";
    const unspaced = ink(c, "abcd");
    c.letterSpacing = "10px";
    const spaced = ink(c, "abcd");

    assert.equal(
      unspaced[0],
      spaced[0],
      "the first glyph starts in the same place",
    );
    assert.ok(
      spaced[1] > unspaced[1],
      `spacing widens the run: ${spaced} against ${unspaced}`,
    );
  });
});

describe("textAlign counts the trailing letter-space", () => {
  // Under CSS the space `letter-spacing` puts after the last character is
  // part of the inline box, so aligning that box moves the glyphs: centred
  // text sits half a space left of the anchor and right-aligned text a whole
  // space left. `alignment_offset` compensated for the half-space Skia puts
  // *before* the first character and for nothing after the last, so the ink
  // did not move at all -- at 40px with 10px spacing, centred `"abcd"` had
  // its midpoint at the anchor whatever the spacing, where Chrome moves it 5
  // pixels left.
  //
  // #81 fixed the other half of this, in `measureText`. That was the reported
  // number only; this is where the glyphs go, and it was wrong before that
  // fix and after it.
  const W = 400,
    H = 80,
    AT = 200;

  // The first and last inked columns of one draw. Positions rather than
  // widths: what moves here is where the run sits, not how wide it is.
  const ink = (align, spacing, text) => {
    const canvas = new Canvas(W, H),
      ctx = canvas.getContext("2d");
    ctx.font = "40px Helvetica";
    ctx.textAlign = align;
    ctx.letterSpacing = `${spacing}px`;
    ctx.fillStyle = "black";
    ctx.fillText(text, AT, 60);
    const d = ctx.getImageData(0, 0, W, H).data;
    let l = W,
      r = -1;
    for (let py = 0; py < H; py++)
      for (let px = 0; px < W; px++)
        if (d[(py * W + px) * 4 + 3] !== 0) {
          if (px < l) l = px;
          if (px > r) r = px;
        }
    return { l, r, mid: (l + r) / 2 };
  };

  // Chrome 148, `"a"` at 40px Helvetica drawn at x=200 -- one character, so
  // the shift is the whole of what spacing does and no advance arithmetic is
  // mixed into it:
  //
  //     align    0px        10px       20px
  //     left     l=201      l=201      l=201
  //     center   mid=200    mid=195    mid=190
  //     right    r=199      r=189      r=179
  //
  // Asserted as the shift between spacings rather than as those positions,
  // so the row says the same thing under any face.
  [
    ["a", "one character, where the shift is the whole effect"],
    ["abcd", "and four, where the advance grows underneath it"],
  ].forEach(([text, what]) => {
    test(`centred text moves half a space left per unit -- ${what}`, () => {
      const at0 = ink("center", 0, text),
        at10 = ink("center", 10, text),
        at20 = ink("center", 20, text);
      assert.equal(at10.mid - at0.mid, -5, "half of 10px");
      assert.equal(at20.mid - at0.mid, -10, "half of 20px");
    });

    test(`right-aligned text moves a whole space left -- ${what}`, () => {
      const at0 = ink("right", 0, text),
        at10 = ink("right", 10, text),
        at20 = ink("right", 20, text);
      assert.equal(at10.r - at0.r, -10, "all of 10px");
      assert.equal(at20.r - at0.r, -20, "all of 20px");
    });

    test(`left-aligned text does not move -- ${what}`, () => {
      // The half-space Skia adds before the first character is still
      // compensated, and this is what says so: the correction for the
      // trailing space must not be applied here as well.
      const at0 = ink("left", 0, text);
      assert.equal(ink("left", 10, text).l, at0.l, "10px");
      assert.equal(ink("left", 20, text).l, at0.l, "20px");
    });
  });
});

describe("maxWidth condenses the run instead of wrapping it", () => {
  // `maxWidth` reached `paragraph.layout()` as a wrapping width, and
  // `max_lines(1)` then discarded everything past the first line -- so
  // `fillText("Hello maxWidth world", 4, 60, 193)` painted byte for byte what
  // `fillText("Hello", 4, 60)` paints. Two of three words gone, through a
  // documented parameter.
  const W = 600,
    H = 120,
    TEXT = "Hello maxWidth world";

  const ctx = () => {
    const c = new Canvas(W, H),
      ctx = c.getContext("2d");
    ctx.font = "48px Helvetica";
    return ctx;
  };

  // The inked box of one draw, and how many pixels it covers. The count is
  // what separates a condensed run from a truncated one: a narrower box alone
  // is what dropping words also gives.
  const ink = (ctx, draw) => {
    ctx.clearRect(0, 0, W, H);
    ctx.fillStyle = "black";
    draw(ctx);
    const d = ctx.getImageData(0, 0, W, H).data;
    let l = W,
      r = -1,
      t = H,
      b = -1,
      n = 0;
    for (let py = 0; py < H; py++)
      for (let px = 0; px < W; px++)
        if (d[(py * W + px) * 4 + 3] !== 0) {
          n++;
          if (px < l) l = px;
          if (px > r) r = px;
          if (py < t) t = py;
          if (py > b) b = py;
        }
    return r < 0 ? null : { l, r, t, b, n, w: r - l + 1, h: b - t + 1 };
  };

  test("the condensed run is the whole run under a horizontal scale", () => {
    const c = ctx();
    const advance = c.measureText(TEXT).width,
      factor = 193 / advance;

    // The reference is built without `maxWidth` at all: the same string drawn
    // through the transform the standard's condensation *is*. That makes this
    // a differential against an independent expression of the rule rather
    // than against a number this code produced, so it cannot be satisfied by
    // agreeing with itself.
    const reference = ink(c, (x) => {
      x.save();
      x.translate(20, 80);
      x.scale(factor, 1);
      x.fillText(TEXT, 0, 0);
      x.restore();
    });
    const condensed = ink(c, (x) => x.fillText(TEXT, 20, 80, 193));

    assert.deepEqual(
      condensed,
      reference,
      "a condensed draw is the run scaled about its anchor",
    );

    // What that rules out, said plainly: the run used to be wrapped at
    // `maxWidth` and everything past the first line discarded, which paints a
    // box of about the right width out of a fraction of the glyphs -- 2736
    // inked pixels here against 3481.
    const truncated = ink(c, (x) => x.fillText("Hello", 20, 80));
    assert.ok(
      condensed.n > truncated.n,
      `all of the text is drawn: ${condensed.n} inked against ${truncated.n} for the first word alone`,
    );
  });

  test("the squeeze is horizontal and by the ratio asked for", () => {
    const c = ctx();
    const advance = c.measureText(TEXT).width;
    const un = ink(c, (x) => x.fillText(TEXT, 20, 80));

    // Chrome condenses by `maxWidth / measureText(text).width` and leaves the
    // inked height alone -- at 200px it draws a half-width `strokeText("H")`
    // with 6-pixel stems and a 12-pixel crossbar. Asserted as a ratio rather
    // than as pixel counts so it says the same thing under any face.
    [0.75, 0.5, 0.25].forEach((factor) => {
      const cn = ink(c, (x) => x.fillText(TEXT, 20, 80, advance * factor));
      // Two pixels of slack, which is the box quantising twice -- the ratio
      // itself is exact, and this is the only thing measuring it in pixels.
      assert.ok(
        Math.abs(cn.w / un.w - factor) < 2 / un.w,
        `condensed to ${factor} of the advance: ${cn.w} against ${un.w}`,
      );
      // A pixel of slack for the coverage the narrower stems land on, which
      // is Chrome's answer too: it also draws this string 37 rows tall
      // unconstrained and 36 condensed. The exact statement is in the
      // measurement test below, where the ascent and descent do not move at
      // all.
      assert.ok(
        Math.abs(cn.h - un.h) <= 1,
        `the inked height does not move at ${factor}: ${cn.h} against ${un.h}`,
      );
    });
  });

  test("a width the run already fits changes nothing", () => {
    const c = ctx();
    const advance = c.measureText(TEXT).width;
    const un = ink(c, (x) => x.fillText(TEXT, 20, 80));

    // The identity that makes the factor the right one: a run constrained to
    // its own measured width must be the unconstrained draw, pixel for pixel.
    // A condensation computed from any other quantity fails here.
    assert.deepEqual(
      ink(c, (x) => x.fillText(TEXT, 20, 80, advance)),
      un,
      "constraining to its own width is a no-op",
    );
    assert.deepEqual(
      ink(c, (x) => x.fillText(TEXT, 20, 80, advance * 2)),
      un,
      "so is a width it is nowhere near",
    );
  });

  test("a width of zero or less draws nothing at all", () => {
    const c = ctx();
    // "If maxWidth was provided but is less than or equal to zero or equal to
    // NaN, then return an empty array" -- the text preparation algorithm, and
    // what Chrome does: no pixel is inked for either.
    [0, -5].forEach((bad) => {
      assert.equal(
        ink(c, (x) => x.fillText(TEXT, 20, 80, bad)),
        null,
        `maxWidth ${bad} inks nothing`,
      );
      assert.equal(
        c.measureText(TEXT, bad).width,
        0,
        `maxWidth ${bad} measures nothing`,
      );
    });

    // `NaN` draws nothing too, but by the older rule that a draw with a
    // non-finite argument is a no-op -- which the JavaScript layer applies
    // before the binding is reached, so the width never becomes a
    // condensation at all. Asserted here because the outcome is the one this
    // test is about; the mechanism is a different one and stays that way.
    assert.equal(
      ink(c, (x) => x.fillText(TEXT, 20, 80, NaN)),
      null,
      "a NaN width inks nothing either",
    );
  });

  test("measuring and outlining condense with the drawing", () => {
    const c = ctx();
    const full = c.measureText(TEXT),
      half = c.measureText(TEXT, full.width / 2);

    assert.nearEqual(half.width, full.width / 2);
    assert.equal(
      half.actualBoundingBoxAscent,
      full.actualBoundingBoxAscent,
      "the ascent does not move",
    );
    assert.equal(
      half.actualBoundingBoxDescent,
      full.actualBoundingBoxDescent,
      "nor the descent",
    );
    // The horizontal pair halves with everything else. It reads the layout
    // box today and the ink box once #83 lands, and those are two different
    // accumulators -- so this is also the guard that the second one is
    // squeezed when it arrives.
    assert.nearEqual(
      half.actualBoundingBoxRight,
      full.actualBoundingBoxRight / 2,
    );
    assert.nearEqual(half.lines[0].width, full.lines[0].width / 2);
    assert.nearEqual(
      half.lines[0].runs[0].width,
      full.lines[0].runs[0].width / 2,
    );

    // The outline has to be the shape the draw paints, or `outlineText` is a
    // different text operation from `fillText`.
    const wide = c.outlineText(TEXT).bounds,
      thin = c.outlineText(TEXT, full.width / 2).bounds;
    assert.nearEqual(thin.width, wide.width / 2);
    assert.equal(thin.height, wide.height, "without changing height");
  });

  test("with textWrap on it is still a wrap width", () => {
    // This fork's extension, and the only reading under which a paragraph may
    // break. The fix must not reach it: `maxWidth` condenses only where the
    // Canvas standard says it does.
    const c = ctx();
    c.textWrap = true;
    const wrapped = c.measureText(TEXT, 200);
    assert.ok(
      wrapped.lines.length > 1,
      `wrapping still breaks the run: ${wrapped.lines.length} lines`,
    );
    assert.ok(
      wrapped.width <= 200,
      `and still honours the width: ${wrapped.width}`,
    );
  });
});
