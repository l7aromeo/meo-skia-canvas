// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary } = require("../../lib");

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

describe("actualBoundingBox reports the ink, not the advance", () => {
  // Every string reported `0` and `width`, which is the advance box reflected
  // rather than any measurement of glyphs -- including strings where that is
  // geometrically impossible.
  //
  // On the bundled face. A first version named Helvetica and asserted that
  // `"AVA"` overhangs its advance, which is a kerning fact about one family:
  // true where it was written and false on CI's freetype leg by a hundredth
  // of a pixel. A leading space carries the same claim without depending on
  // the face at all.
  FontLibrary.use("Raleway", [
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
  ]);

  const measured = (text) => {
    const ctx = new Canvas(400, 100).getContext("2d");
    ctx.font = "48px Raleway";
    return ctx.measureText(text);
  };

  test("a leading space puts the ink inside the advance at both ends", () => {
    // The space is inked by nothing, so the first mark is right of the origin
    // and `left` has to be negative; `H` has a right sidebearing, so the last
    // mark is left of the advance and `right` has to be under `width`. The
    // advance box gives exactly 0 and `width` for both.
    const m = measured(" H");
    assert.ok(
      m.actualBoundingBoxLeft < 0,
      `a leading space starts right of the origin: ${m.actualBoundingBoxLeft}`,
    );
    assert.ok(
      m.actualBoundingBoxRight < m.width,
      `the last mark is inside the advance: ${m.actualBoundingBoxRight} against ${m.width}`,
    );

    // The anchor, pinned to a number rather than to a relation between
    // measurements: an advance comes from the font's own metrics where a
    // bound comes from the rasteriser, so a value survives CI's legs where a
    // relation between two of them need not. Without it, an implementation
    // reporting nonsense in both would satisfy the two assertions above.
    // Not `nearEqual`, which is fixed at 0.005: CI's freetype leg reports
    // 47.66 against this machine's 47.68, so an advance is not
    // rasteriser-independent even on a pinned face. 0.1 is five times that
    // drift and two orders of magnitude below the regression it guards --
    // `width` taking the ink box would report 30.00 for `" H"`, which inks
    // from 15.258 to 45.258.
    assert.ok(
      Math.abs(m.width - 47.68) < 0.1,
      `the advance is unchanged: ${m.width} against 47.68`,
    );
  });

  test("and the vertical pair is unchanged, being ink already", () => {
    // `Ascent`/`Descent` were always taken from the glyph bounds, so they do
    // not move with this and are not part of it. A descender is what says so:
    // it reports a descent the flat-bottomed strings do not.
    const j = measured("j"),
      h = measured("H");
    assert.ok(
      j.actualBoundingBoxDescent > h.actualBoundingBoxDescent,
      `j descends below H: ${j.actualBoundingBoxDescent} against ${h.actualBoundingBoxDescent}`,
    );
  });
});

describe("ctx.font reports the serialized form, not the parsed one", () => {
  // HTML: "On getting, the font attribute must return the serialized form of
  // the current font of the context (with no 'line-height' component)". CSS
  // shorthand serialisation then omits every component sitting at its initial
  // value. All three rules were being broken at once: a line height appeared
  // that no browser returns, `normal 400` was emitted where nothing should
  // be, and 700 was spelled as the number rather than as `bold`.
  const ctx = () => new Canvas(10, 10).getContext("2d");

  test("the four cases measured against Chrome 148", () => {
    // The right-hand column is what Chrome 148 returns for the same
    // assignment, read off `document.createElement("canvas").getContext("2d")`.
    for (const [set, expected] of [
      ["24px/2 Helvetica", "24px Helvetica"],
      ["16px Helvetica", "16px Helvetica"],
      ["bold 16px Helvetica", "bold 16px Helvetica"],
      ["italic bold 24px Helvetica", "italic bold 24px Helvetica"],
    ]) {
      const c = ctx();
      c.font = set;
      assert.equal(c.font, expected, set);
    }
  });

  test("every component drops at its initial value, and 700 is bold", () => {
    for (const [set, expected] of [
      ["normal normal 400 normal 16px serif", "16px serif"],
      ["700 16px serif", "bold 16px serif"],
      ["300 16px serif", "300 16px serif"],
      ["1000 16px serif", "1000 16px serif"],
      ["italic 16px serif", "italic 16px serif"],
      ["small-caps 16px serif", "small-caps 16px serif"],
      ["16px/1.5 serif", "16px serif"],
      ["16px/24px serif", "16px serif"],
      // Quoting survives, and so does the fallback list.
      ["300 12px Comic Sans, serif", '300 12px "Comic Sans", serif'],
    ]) {
      const c = ctx();
      c.font = set;
      assert.equal(c.font, expected, set);
    }
  });

  test("three divergences from Chrome, all keeping the round trip whole", () => {
    // There is no single browser answer to compare against: Chrome's canvas
    // serialiser normalises or drops what Chrome's own CSS serialiser leaves
    // alone. The same string through both, Chrome 148:
    //
    //   input                         ctx.font           div.style.font
    //   oblique 20px serif            italic 20px …      oblique 20px …
    //   condensed 16px Helvetica      16px Helvetica     condensed 16px …
    //   italic small-caps bold 16px   italic bold        italic small-caps
    //     serif                         small-caps …       bold …
    //
    // The last row is the order question, and `style.font` settles it: it
    // normalises both input orders to style, variant, weight, stretch, which
    // is what CSSOM specifies for a `||` shorthand and what this emits. It
    // settles the order only -- being a specified-value serialisation, it
    // says nothing about dropping a component at its initial value, which is
    // what the tests above cover.
    //
    // The other two rows are kept because this applies what Chrome discards.
    // Chrome does not apply a stretch at all: condensed, expanded and `50%`
    // all measure 120.9453125 for the same Arial string, while here the
    // stretch selects a face, and `oblique` and `italic` are separate faces
    // to the matcher. Dropping either from the getter would make
    // `ctx.font = ctx.font` lossy for something that renders differently.
    //
    // So a differential run against a browser will flag these three rows and
    // only these three.
    const c = ctx();
    c.font = "condensed 16px serif";
    assert.equal(c.font, "condensed 16px serif");
    c.font = "oblique 20px serif";
    assert.equal(c.font, "oblique 20px serif");
    c.font = "italic bold small-caps 16px serif";
    assert.equal(c.font, "italic small-caps bold 16px serif");
  });

  test("assigning the getter back is a no-op", () => {
    // The reason the three rules are worth following. In a browser
    // `ctx.font = ctx.font` changes nothing; here it reparsed a different
    // string, so anything that stored, compared or restored the value drifted.
    for (const set of [
      "16px serif",
      "bold italic 20px serif",
      "condensed 300 12px serif",
      "16px/24px serif",
    ]) {
      const c = ctx();
      c.font = set;
      const once = c.font;
      // Through the variable rather than `c.font = c.font`, which is the
      // same assignment and a `no-self-assign` lint error.
      c.font = once;
      assert.equal(c.font, once, `${set} is stable under reassignment`);
    }
  });

  test("a line height still reaches the layout it was set for", () => {
    // The serialized form cannot be the addon's cache key, and this is what
    // says so: the key is the canonical string, which keeps the line height,
    // so these two resolve separately. Were the key the string above, both
    // would name `16px serif` and the second would be laid out with the
    // first's leading.
    const wrapped = (font) => {
      const c = ctx();
      c.textWrap = true;
      c.font = font;
      return c
        .measureText("one two three four five six", 40)
        .lines.map((line) => line.y);
    };
    const tight = wrapped("16px/24px serif"),
      loose = wrapped("16px/64px serif");
    assert.equal(tight.length, loose.length, "same number of lines");
    assert.ok(
      loose[1] - loose[0] > (tight[1] - tight[0]) * 2,
      `64px leading spaces the lines further than 24px: ${loose[1] - loose[0]} against ${tight[1] - tight[0]}`,
    );
  });
});

describe("kerning stops at a word boundary", () => {
  /** A context at a size where Arial's kern pairs are well clear of rounding. */
  const measured = () => {
    const ctx = new Canvas(8, 8).getContext("2d");
    ctx.font = "24px Arial";
    return (text) => ctx.measureText(text).width;
  };

  test("a kern pair does not reach across the space between two words", () => {
    // Chrome kerns pairs and never kerns across a space: its `"A V"` is
    // exactly `w("A") + w(" ") + w("V")` for every pair measured, including
    // the ones it kerns tight. This applied the pair across the space, so
    // `"A V"` came out 1.33 short at 24px.
    //
    // Asserted against a control rather than against the sum directly:
    // measuring a whole string and summing three `measureText` calls differ
    // by about 0.01 whatever the string, kerning or not, so the sum is not
    // exact enough to assert on. What must be true is that a kerning pair
    // and a non-kerning one are offset from their sums by the *same* amount.
    const w = measured();
    const offset = (x, y) => w(`${x} ${y}`) - (w(x) + w(" ") + w(y));
    const control = offset("n", "n"); // no kern pair between n and n

    for (const [x, y] of [
      ["A", "V"],
      ["A", "T"],
      ["A", "W"],
      ["V", "A"],
      ["T", "o"],
      ["L", "T"],
      ["P", "A"],
      ["F", "A"],
    ])
      assert.ok(
        Math.abs(offset(x, y) - control) < 0.02,
        `"${x} ${y}" carries ${(offset(x, y) - control).toFixed(3)} of kerning across the space`,
      );
  });

  test("but it still applies inside a word", () => {
    // The half that a blanket `kern = 0` would have broken: turning the
    // feature off for the whole run fixes the spaced case and flattens this
    // one, so a test that only checked spacing would pass for a worse fix.
    const w = measured();
    for (const pair of ["AV", "AT", "AW", "VA", "To", "LT", "PA", "FA"])
      assert.ok(
        w(pair) < w(pair[0]) + w(pair[1]) - 0.5,
        `${pair} lost its kern pair`,
      );
  });

  test("a hard break separates words as a space does", () => {
    // Every hard break is a space by the time a single line is shaped, but
    // wrapping mode lays them out as themselves, so both are named as
    // separators. Chrome suppresses kerning across either.
    const w = measured();
    assert.ok(
      Math.abs(w("A\nV") - w("A V")) < 0.02,
      "a newline kerns differently from a space",
    );
  });

  test("what is drawn follows what was measured", () => {
    // Asserted from ink rather than from reported glyph positions: those
    // carry half of each glyph's own preceding kern (#131) and the painter
    // is the half that is right, so the raster is the trustworthy oracle
    // here and `extended_visit` is not.
    const lastInkColumn = (draw) => {
      const canvas = new Canvas(400, 60);
      const ctx = canvas.getContext("2d");
      ctx.clearRect(0, 0, 400, 60);
      ctx.fillStyle = "#000";
      ctx.font = "48px Arial";
      draw(ctx);
      const { data } = ctx.getImageData(0, 0, 400, 60);
      let last = -1;
      for (let x = 0; x < 400; x++)
        for (let y = 0; y < 60; y++)
          if (data[(y * 400 + x) * 4 + 3] > 0) {
            last = x;
            break;
          }
      return last;
    };

    const ctx = new Canvas(8, 8).getContext("2d");
    ctx.font = "48px Arial";
    const advance = (text) => ctx.measureText(text).width;

    assert.equal(
      lastInkColumn((c) => c.fillText("A V", 10, 45)),
      lastInkColumn((c) =>
        c.fillText("V", 10 + advance("A") + advance(" "), 45),
      ),
      "the V is not painted where an unkerned advance puts it",
    );
  });
});

describe("outlineText draws what fillText draws", () => {
  // `Paragraph::get_path_at` and `extended_visit` place a glyph half of its
  // own preceding kern to the right of where `Paragraph::paint` draws it, so
  // the path `outlineText` returned did not fill as the text it came from and
  // `actualBoundingBoxRight` was wide by the same half. Reported upstream; the
  // recovery and its guard are documented on `painted_positions`.
  //
  // The fault needs a face that kerns through the legacy `kern` table. On
  // macOS `Helvetica` is one; where it resolves to a GPOS-kerned substitute
  // these assertions hold without the fix doing anything, so this is real
  // coverage on some platforms and a consistency check on the rest. It is
  // written against rasterised ink on purpose: the reported positions are
  // exactly what was broken, so a test asserting them would have encoded the
  // defect as expected behaviour.
  const W = 2000,
    H = 800;

  const inked = (ctx, draw) => {
    ctx.clearRect(0, 0, W, H);
    ctx.fillStyle = "black";
    draw();
    const d = ctx.getImageData(0, 0, W, H).data;
    let lo = W,
      hi = -1;
    for (let i = 3; i < d.length; i += 4)
      if (d[i] > 0) {
        const x = ((i - 3) / 4) % W;
        if (x < lo) lo = x;
        if (x > hi) hi = x;
      }
    return [lo, hi];
  };

  test("a kerned pair fills identically through both routes", () => {
    const ctx = new Canvas(W, H).getContext("2d");
    ctx.font = "480px Helvetica";
    // "HH" is the control: no kern pair, so the two routes agree whatever
    // Skia does with kerning, and a failure there is a broken harness rather
    // than a regression in this fix.
    for (const text of ["To", "AV", "Wave To", "HH"]) {
      const drawn = inked(ctx, () => ctx.fillText(text, 0, 600));
      const path = ctx.outlineText(text);
      const filled = inked(ctx, () => {
        ctx.save();
        ctx.translate(0, 600);
        ctx.fill(path);
        ctx.restore();
      });
      assert.deepEqual(filled, drawn, `${text} fills as it draws`);
    }
  });

  test("and the ink box no longer carries the half kern", () => {
    // The row #83's tests could not see. `" H"`, `"H"` and `"j"` carry no
    // kerned pair between them, so every assertion in that fix was blind to
    // this class -- the ink box of a kerned string was wide by half a kern on
    // top of the rounding gap that is still open.
    //
    // Asserted as a relation rather than a pinned number, because the amount
    // depends on the face: the ink must end before the advance does, and for
    // a pair that kerns it must not exceed the unkerned ink either.
    const ctx = new Canvas(200, 100).getContext("2d");
    ctx.font = "48px Helvetica";
    const kerned = ctx.measureText("To");
    const apart = ctx.measureText("T").width + ctx.measureText("o").width;
    assert.ok(
      kerned.actualBoundingBoxRight <= kerned.width,
      `ink ends within the advance: ${kerned.actualBoundingBoxRight} against ${kerned.width}`,
    );
    assert.ok(
      kerned.actualBoundingBoxRight < apart,
      `a kerned pair inks less far than an unkerned one: ${kerned.actualBoundingBoxRight} against ${apart}`,
    );
  });
});

describe("actualBoundingBox reports the outline, not the pixel grid", () => {
  // `info.bounds()` hands back the *rasterisation* box: the glyph outline
  // rounded outwards to the pixel grid and padded for the mask. At 48px that
  // is exactly `floor - 1` and `ceil + 1` on every glyph measured, across
  // Helvetica, Times, Arial and Courier New; at 480px the margin is wider and
  // scales with the size. Either way it is lossy, so the box was 1.2 to 1.9
  // wide on each side and could not be corrected arithmetically -- 3.773
  // cannot be recovered from 2. The bounds now come from the glyph outlines.
  //
  // The vertical pair carried a second rounding on top: `info.origin().y` is
  // the run's baseline snapped to a whole pixel, 37 against 36.960938 at
  // 48px, which shifted the whole box down by the difference.
  const ctx = () => new Canvas(400, 120).getContext("2d");

  test("both routes to the ink agree", () => {
    // `outlineText` builds a path from the same glyph outlines at the same
    // positions, so the two have to describe the same ink. Asserted as an
    // agreement rather than against pinned numbers because the face differs
    // by platform -- `Helvetica` resolves to a substitute on Linux -- and
    // this relation holds whatever it resolves to.
    const c = ctx();
    c.font = "48px Helvetica";
    for (const text of ["H", "j", "x", "To", "AVA", "Hjgy"]) {
      const m = c.measureText(text),
        b = c.outlineText(text).bounds;
      assert.nearEqual(-m.actualBoundingBoxLeft, b.left, `${text} left`);
      assert.nearEqual(m.actualBoundingBoxRight, b.right, `${text} right`);
      assert.nearEqual(-m.actualBoundingBoxAscent, b.top, `${text} top`);
      assert.nearEqual(m.actualBoundingBoxDescent, b.bottom, `${text} bottom`);
    }
  });

  test("and the box is subpixel, not snapped to the grid", () => {
    // The signature of the defect, and face-independent: the horizontal
    // bounds used to be whole numbers, because the box was the pixel grid the
    // glyph was rasterised into. A real outline lands off the grid.
    //
    // One glyph, and the horizontal pair only. Two earlier versions of this
    // test passed against the rasterisation box, which is the thing it exists
    // to reject: the vertical pair is offset by the baseline and the second
    // glyph of any string sits at a fractional advance, so both come back
    // non-integer whichever box they were measured from. A single glyph at
    // the origin is the only case where an integer box stays integer.
    const c = ctx();
    c.font = "48px Helvetica";
    const m = c.measureText("H");
    const horizontal = [m.actualBoundingBoxLeft, m.actualBoundingBoxRight];
    assert.ok(
      horizontal.some((v) => !Number.isInteger(v)),
      `a horizontal bound is off the pixel grid: ${horizontal.join(", ")}`,
    );
  });

  test("a wider glyph inks further than a narrower one", () => {
    // A relation the rasterisation box also satisfied, kept as the control:
    // if this fails the measurement is broken in a way the two tests above
    // would not localise.
    const c = ctx();
    c.font = "48px Helvetica";
    assert.ok(
      c.measureText("W").actualBoundingBoxRight >
        c.measureText("i").actualBoundingBoxRight,
      "W inks further right than i",
    );
  });
});
