// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { Canvas, FontLibrary, ParagraphBuilder } = require("../../lib");

// The families the machine itself holds, captured before any test registers
// one. Describe bodies run at collection, so a snapshot taken further down
// would already carry this file's own registrations and could not be used to
// pick a name the *system* owns.
const SYSTEM_FAMILIES = FontLibrary.families.slice();

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
  // the current font of the context (with no 'line-height' component)".
  // CSSOM's shorthand serialisation then omits every component sitting at
  // its initial value, writes the rest in the grammar's order, a weight as
  // its computed number and a family name as a string.
  const ctx = () => new Canvas(10, 10).getContext("2d");

  test("the standard's own example", () => {
    // From the HTML standard's note on the `font` getter.
    const c = ctx();
    c.font = "italic 400 12px/2 Unknown Font, sans-serif";
    assert.equal(c.font, 'italic 12px "Unknown Font", sans-serif');
  });

  test("every component drops at its initial value, and 700 is its number", () => {
    for (const [set, expected] of [
      ["24px/2 Helvetica", '24px "Helvetica"'],
      ["normal normal 400 normal 16px serif", "16px serif"],
      ["bold 16px Helvetica", '700 16px "Helvetica"'],
      ["700 16px serif", "700 16px serif"],
      ["300 16px serif", "300 16px serif"],
      ["1000 16px serif", "1000 16px serif"],
      ["italic 16px serif", "italic 16px serif"],
      ["small-caps 16px serif", "small-caps 16px serif"],
      ["16px/1.5 serif", "16px serif"],
      ["16px/24px serif", "16px serif"],
      ["300 12px Comic Sans, serif", '300 12px "Comic Sans", serif'],
    ]) {
      const c = ctx();
      c.font = set;
      assert.equal(c.font, expected, set);
    }
  });

  test("stretch, oblique and small-caps keep their place in the grammar", () => {
    // CSS Fonts 4: canonical order "per grammar", style, variant, weight,
    // stretch, size, family; the computed style is "the keyword specified,
    // plus angle in degrees if specified", and 14deg is what the keyword
    // alone means. Chrome's canvas reads these differently -- see
    // docs/browser-differences.md and the `chrome` column of
    // tests/assets/cases/css-fonts.json.
    const c = ctx();
    c.font = "condensed 16px serif";
    assert.equal(c.font, "condensed 16px serif");
    assert.equal(c.fontStretch, "condensed");
    c.font = "oblique 20px serif";
    assert.equal(c.font, "oblique 20px serif");
    c.font = "oblique 14deg 20px serif";
    assert.equal(c.font, "oblique 20px serif");
    c.font = "oblique 10deg 20px serif";
    assert.equal(c.font, "oblique 10deg 20px serif");
    c.font = "italic bold small-caps 16px serif";
    assert.equal(c.font, "italic small-caps 700 16px serif");
  });

  test("fontStretch and fontVariantCaps leave the string alone", () => {
    // The standard keeps them as values of their own beside the font.
    const c = ctx();
    c.font = "16px serif";
    c.fontStretch = "condensed";
    c.fontVariantCaps = "small-caps";
    assert.equal(c.font, "16px serif");
  });

  test("a negative zero reads back as zero", () => {
    // The shortest form of negative zero is `0`.
    const c = ctx();
    c.font = "-0px X";
    assert.equal(c.font, '0px "X"');
    c.font = "oblique -0deg 16px X";
    assert.equal(c.font, 'oblique 0deg 16px "X"');
  });

  test("a number too small for a float is still not a zero", () => {
    // CSS lets only zero go without a unit, and `1e-50` is not zero
    // though a 32-bit float holds it as one.
    const c = ctx();
    for (const css of ["1e-50", "-1e-50"]) {
      c.letterSpacing = "3px";
      c.letterSpacing = css;
      assert.equal(c.letterSpacing, "3px", `letterSpacing ${css}`);
      c.wordSpacing = "3px";
      c.wordSpacing = css;
      assert.equal(c.wordSpacing, "3px", `wordSpacing ${css}`);
      c.font = "16px serif";
      c.font = `${css} serif`;
      assert.equal(c.font, "16px serif", `font size ${css}`);
    }
    c.letterSpacing = "0e5";
    assert.equal(c.letterSpacing, "0px");
  });

  test("assigning the getter back is a no-op", () => {
    // The reason the serialisation rules are worth following. In a browser
    // `ctx.font = ctx.font` changes nothing; here it reparsed a different
    // string, so anything that stored, compared or restored the value drifted.
    for (const set of [
      "16px serif",
      "bold italic 20px serif",
      "condensed 300 12px serif",
      "oblique 0deg 12px Comic Sans, serif",
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
  // `Paragraph::get_path_at` and `extended_visit` place each glyph where its
  // advances put it, without the offset the shaper gave it, where
  // `Paragraph::paint` adds it. So the path `outlineText` was built from did
  // not fill as the text it came from wherever the shaper moved a glyph off
  // its advance: half of each kern from a legacy `kern` table, which HarfBuzz
  // carries as an offset, and every mark it stacks or sets over a base. The
  // glyphs are now read from what `Paragraph::paint` draws -- see
  // `glyph_runs`.
  //
  // The kerning half needs a face that kerns through the legacy `kern`
  // table. On macOS `Helvetica` is one; where it resolves to a GPOS-kerned
  // substitute those assertions hold whatever is done, so they are real
  // coverage on some platforms and a consistency check on the rest. The
  // marks are bundled Raleway's, and real coverage everywhere. Both are
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

  test("stacked marks fill and measure where they are drawn", () => {
    // A base with no precomposed accented glyph, so its marks are placed by
    // the shaper's offsets alone: the acute and the diaeresis over the "p"
    // stack one above the other.
    FontLibrary.use("RalewayMarks", [
      "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    ]);
    const ctx = new Canvas(W, H).getContext("2d");
    ctx.font = "240px RalewayMarks";
    const box = (draw) => {
      ctx.clearRect(0, 0, W, H);
      ctx.fillStyle = "black";
      draw();
      const d = ctx.getImageData(0, 0, W, H).data;
      let [left, right, top, bottom] = [W, -1, H, -1];
      for (let i = 3; i < d.length; i += 4)
        if (d[i] > 0) {
          const x = ((i - 3) / 4) % W,
            y = Math.floor((i - 3) / 4 / W);
          [left, right] = [Math.min(left, x), Math.max(right, x)];
          [top, bottom] = [Math.min(top, y), Math.max(bottom, y)];
        }
      return { left, right, top, bottom };
    };
    const text = "p\u0301\u0308";
    const drawn = box(() => ctx.fillText(text, 100, 600));
    const filled = box(() => {
      ctx.save();
      ctx.translate(100, 600);
      ctx.fill(ctx.outlineText(text));
      ctx.restore();
    });
    // Within a pixel, which is how far a filled outline's antialiased edge
    // and a glyph mask's part; a mark out of place is tens of pixels off.
    for (const edge of ["left", "right", "top", "bottom"])
      assert.ok(
        Math.abs(filled[edge] - drawn[edge]) <= 1,
        `the outline fills as the text draws at its ${edge}: ${filled[edge]} against ${drawn[edge]}`,
      );
    // And the ink box is the outline's, which the fill above ties to the
    // drawing.
    const m = ctx.measureText(text),
      bounds = ctx.outlineText(text).bounds;
    assert.nearEqual(-m.actualBoundingBoxAscent, bounds.top, "ink top");
    assert.nearEqual(m.actualBoundingBoxRight, bounds.right, "ink right");
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

describe("a face kerned through its legacy kern table outlines as it draws", () => {
  // HarfBuzz carries half of each kern from a font's legacy `kern` table as
  // an offset on the glyph after it, and `outlineText` reads its glyphs from
  // the text blobs `fillText` paints, offsets included -- see `glyph_runs`.
  // Verdana kerns through that table too, despite its GPOS, because its GPOS
  // has no kern feature; Arial kerns through GPOS and is the control.
  //
  // The scale of the failure where the offsets are dropped, at 192px --
  // longest differing stretch on "To", "AV" and "Wave To":
  //
  //     Helvetica   11, 7, 11        Verdana   9, 4, 10        Arial   1, 0, 1
  //
  // so the assertion below has teeth on a platform carrying these faces. On
  // one where they resolve to GPOS-kerned substitutes it is a consistency
  // check, and a clean run says nothing about the legacy table there.
  const SIZE = 192;
  const W = 2400,
    H = 400;

  const longestDifferingStretch = (ctx, text) => {
    const inked = (draw) => {
      ctx.clearRect(0, 0, W, H);
      ctx.fillStyle = "black";
      draw();
      const d = ctx.getImageData(0, 0, W, H).data,
        cols = new Uint8Array(W);
      for (let i = 3; i < d.length; i += 4)
        if (d[i] > 128) cols[((i - 3) / 4) % W] = 1;
      return cols;
    };
    const drawn = inked(() => ctx.fillText(text, 0, 300));
    const path = ctx.outlineText(text);
    const filled = inked(() => {
      ctx.save();
      ctx.translate(0, 300);
      ctx.fill(path);
      ctx.restore();
    });
    let run = 0,
      worst = 0;
    for (let i = 0; i < W; i++) {
      if (drawn[i] !== filled[i]) worst = Math.max(worst, ++run);
      else run = 0;
    }
    return worst;
  };

  test("outlineText draws what fillText draws, on every face", () => {
    const ctx = new Canvas(W, H).getContext("2d");
    for (const family of ["Helvetica", "Verdana", "Arial"]) {
      ctx.font = `${SIZE}px ${family}`;
      for (const text of ["To", "AV", "Wave To"]) {
        // Two, not zero: a glyph edge landing between pixels leaves a column
        // of antialiasing that differs between a filled path and painted
        // text, and that is about one column per edge whatever the face.
        assert.ok(
          longestDifferingStretch(ctx, text) <= 2,
          `${family} ${JSON.stringify(text)}: the outline and the drawn text describe the same ink`,
        );
      }
    }
  });
});

/**
 * How close a width here comes to Chrome's: a tenth of the hundredth
 * SkParagraph's rounding hack rounded widths to. What is left is the two
 * engines' sums of the same advances, 0.00014 at most across the cases
 * measured, against up to 0.005 from the rounding.
 */
const CHROME_FLOAT = 0.0005;

/**
 * Where Chrome's widths were measured: macOS, whose CoreText gives the
 * advances these tests hold. FreeType's differ in the thousandths, so
 * elsewhere the widths are held to properties no rounding survives instead.
 */
const CHROME_MEASURED_HERE =
  process.platform === "darwin"
    ? false
    : "Chrome 156's widths were measured on macOS, through CoreText";

/**
 * How far an `f32` sum of advances may drift from the same advances summed
 * another way: a few units in the last place of widths of a few hundred
 * pixels, and well under the hundredth that rounding moved them by.
 */
const F32_SUM = 0.0002;

describe("measureText's width is the run's advance", () => {
  FontLibrary.use("Oswald", [
    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
  ]);

  // Chrome 152 at 40px Oswald, loaded as a FontFace with its weight range:
  // the advance of every glyph and space, letter spacing after each and
  // word spacing between words, summed and never clamped.
  // On any platform, the width with spacing is the width without it plus the
  // spacing: letter spacing after every character, word spacing at each
  // space between words, never clamped. On macOS, also Chrome 156's widths
  // to the float, within CHROME_FLOAT, where SkParagraph's rounding to
  // hundredths put the width as much as half of one away.
  test("width is the signed advance, trailing spaces included", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = "40px Oswald";
    const unspaced = (text) => {
      ctx.letterSpacing = "0px";
      ctx.wordSpacing = "0px";
      return ctx.measureText(text).width;
    };
    for (const [letter, word, text, chrome] of [
      ["-23.12px", "0px", "abcd", -23.4400177],
      ["-30px", "0px", "ab", -25.08001708984375],
      ["-50px", "0px", "a", -32.96000671386719],
      ["-100px", "0px", "abcd", -330.96002197265625],
      ["0px", "-100px", "a b", -55.92002868652344],
      ["-10px", "0px", " ", -0.8400115966796875],
      ["-50px", "0px", "ab ", -105.92002868652344],
      ["-10px", "0px", "a ", 6.199981689453125],
      ["0px", "0px", "a  ", 35.35997009277344],
    ]) {
      const plain = unspaced(text);
      ctx.letterSpacing = letter;
      ctx.wordSpacing = word;
      const { width } = ctx.measureText(text);
      const what = `${letter} ${word} ${JSON.stringify(text)}: ${width}`;
      const between = text.trim().split(" ").length - 1;
      const spaced =
        plain + parseFloat(letter) * text.length + parseFloat(word) * between;
      assert.ok(
        Math.abs(width - spaced) <= F32_SUM,
        `${what}, unspaced ${plain} plus the spacing ${spaced}`,
      );
      if (!CHROME_MEASURED_HERE) {
        assert.ok(
          Math.abs(width - chrome) <= CHROME_FLOAT,
          `${what}, Chrome ${chrome}`,
        );
      }
    }
  });

  // On any platform: a run of one digit is that digit's width times its
  // length, at sizes whose advances are not whole hundredths. Widths rounded
  // to hundredths broke this, each run rounded on its own.
  test("a run of one digit is its width times its length", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    for (const size of [9.5, 12.3, 17.7, 23.7, 31.9]) {
      ctx.font = `${size}px Oswald`;
      const one = ctx.measureText("0").width;
      const seven = ctx.measureText("0000000").width;
      assert.ok(
        Math.abs(seven - 7 * one) <= 0.0001,
        `${size}px: ${seven} against 7 x ${one}`,
      );
    }
  });

  // At sizes whose advances are not whole hundredths, the width is Chrome
  // 156's to the float.
  test(
    "width is Chrome's at fractional sizes",
    { skip: CHROME_MEASURED_HERE },
    () => {
      const ctx = new Canvas(10, 10).getContext("2d");
      for (const [size, text, chrome] of [
        [24, "Hamburgefonstiv", 148.1759033203125],
        [24, "quick brown fox", 137.2559051513672],
        [23.7, "Hamburgefonstiv", 146.32371520996094],
        [23.7, "Hambu", 60.52976989746094],
        [31.9, "Hamburgefonstiv", 196.9504852294922],
        [31.9, "quick brown fox", 182.43600463867188],
        [12, "Hamburgefonstiv", 74.08787536621094],
        [9.5, "quick brown fox", 54.33038330078125],
      ]) {
        ctx.font = `${size}px Oswald`;
        const { width } = ctx.measureText(text);
        assert.ok(
          Math.abs(width - chrome) <= CHROME_FLOAT,
          `${size}px ${JSON.stringify(text)}: ${width}, Chrome ${chrome}`,
        );
      }
    },
  );

  // A letter spacing whose half is not a hundredth: rounded laid-out boxes
  // with half the spacing at each end lost a hundredth at 10px. Chrome 152
  // and 156 on macOS measure "00" at 5.17px spacing as below, to the float.
  test("width keeps the spacing whole at every size", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.letterSpacing = "5.17px";
    for (const [size, chrome] of [
      [7, 17.577972412109375],
      [10, 20.67999267578125],
      [13, 23.781982421875],
      [40, 51.699981689453125],
      [100, 113.739990234375],
    ]) {
      ctx.font = `${size}px Oswald`;
      const one = ctx.measureText("0").width;
      const seven = ctx.measureText("0000000").width;
      const { width } = ctx.measureText("00");
      // On any platform: seven spaced digits are seven times one, where each
      // width rounded to hundredths on its own broke it.
      assert.ok(
        Math.abs(seven - 7 * one) <= F32_SUM,
        `${size}px: ${seven} against 7 x ${one}`,
      );
      if (!CHROME_MEASURED_HERE) {
        assert.ok(
          Math.abs(width - chrome) <= CHROME_FLOAT,
          `${size}px: ${width}, Chrome ${chrome}`,
        );
      }
    }
  });

  // The width is the run's advance, so where the run is aligned cannot move
  // it, as it cannot in Chrome. Measured from the laid-out boxes, a
  // centred run's edges were rounded at different places from a left-
  // aligned one's, and 7px Oswald measured "1234.5" 17.4766 centred.
  test("width does not depend on the alignment", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    for (const font of ["7px Oswald", "13px Oswald", "40px Oswald"]) {
      ctx.font = font;
      for (const text of ["1234.5", "AV To", "Hamburgefonstiv"]) {
        const widths = ["left", "center", "right"].map((align) => {
          ctx.textAlign = align;
          return ctx.measureText(text).width;
        });
        assert.deepEqual(
          widths,
          [widths[0], widths[0], widths[0]],
          `${font} ${JSON.stringify(text)}`,
        );
      }
    }
  });

  // A run that falls back from one family to another is laid out face by
  // face, and its advance follows the weight each face draws at. Chrome
  // 152, with Montserrat's 200, 400 and 700 faces and Raleway's variable
  // face loaded as FontFace with their weights: "ЖЖ" falls back to Raleway,
  // which reaches 900 where Montserrat stops at 700, so at -60px the run is
  // 6.9 longer at 900 than at 700 over the same Latin. Within a hundredth
  // and a half: SkParagraph keeps hundredths, and FreeType's advances for
  // these faces sum a hundredth below CoreText's at 700 -- 176.30 against
  // 176.31 with no spacing at all, where Chrome's is 176.3113.
  const MONTSERRAT = "tests/assets/fonts/montserrat-latin/montserrat-v30-latin";
  FontLibrary.use("MontserratAdvance", [
    `${MONTSERRAT}-200.woff2`,
    `${MONTSERRAT}-regular.woff2`,
    `${MONTSERRAT}-700.woff2`,
  ]);
  FontLibrary.use("RalewayAdvance", [
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
  ]);

  test("a negative advance follows each face's own weight", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    for (const [weight, letter, text, chrome] of [
      [900, "-60px", "ABЖЖ", -56.78401184082031],
      [700, "-60px", "ABЖЖ", -63.68867492675781],
      [400, "-60px", "ABЖЖ", -74.8394775390625],
      [900, "-60px", "AB ЖЖ", -103.20001220703125],
      [900, "-30px", "ABЖЖ", 63.21598815917969],
    ]) {
      ctx.font = `${weight} 48px MontserratAdvance, RalewayAdvance`;
      ctx.letterSpacing = letter;
      const { width } = ctx.measureText(text);
      assert.ok(
        Math.abs(width - chrome) <= 0.015,
        `${weight} ${letter} ${JSON.stringify(text)}: ${width}, Chrome ${chrome}`,
      );
    }
  });

  // A negative advance does not move the ink box: it is where the glyphs'
  // ink is. Chrome's is not -- Blink's plain_text_node.cc moves the left
  // edge of such a run's box out to the end of the advance, so `a` at -50px
  // reports a left of 32.96 though its ink begins 1.68 right of the origin,
  // and a lone space, which has no ink, reports 0.84.
  // docs/browser-differences.md records it. Each value below is the union of
  // the glyphs' own ink, each placed at the sum of the advances before it and
  // their spacing, then shifted by the alignment; the right edges are
  // Chrome's too.
  test("a negative advance's ink box is where the ink is", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = "40px Oswald";
    for (const [letter, word, text, align, direction, left, right] of [
      ["-23.12px", "0px", "abcd", "start", "ltr", 16.28, 15.48],
      ["-50px", "0px", "a", "start", "ltr", -1.68, 15.48],
      ["-100px", "0px", "abcd", "start", "ltr", 246.92, 15.48],
      ["-30px", "0px", "ab", "start", "ltr", 10.72, 15.48],
      ["-23.12px", "0px", "abcd", "right", "ltr", -7.16, 38.92],
      ["-50px", "0px", "a", "right", "ltr", -34.64, 48.44],
      ["-23.12px", "0px", "abcd", "center", "ltr", 4.56, 27.2],
      ["-50px", "0px", "a", "center", "ltr", -18.16, 31.96],
      ["-23.12px", "0px", "abcd", "start", "rtl", -7.16, 38.92],
      ["-10px", "0px", " ", "start", "ltr", 0, 0],
      // The ink reaches past the advance here, so Chrome agrees.
      ["0px", "-100px", "a b", "start", "ltr", 71.56, 15.48],
    ]) {
      ctx.letterSpacing = letter;
      ctx.wordSpacing = word;
      ctx.textAlign = align;
      ctx.direction = direction;
      const m = ctx.measureText(text),
        what = `${letter} ${word} ${JSON.stringify(text)} ${align} ${direction}`;
      assert.ok(
        Math.abs(m.actualBoundingBoxLeft - left) <= 0.02,
        `${what} left: ${m.actualBoundingBoxLeft}, ink ${left}`,
      );
      assert.ok(
        Math.abs(m.actualBoundingBoxRight - right) <= 0.02,
        `${what} right: ${m.actualBoundingBoxRight}, ink ${right}`,
      );
    }
  });
});

describe("a fractional font-weight is drawn at its fraction", () => {
  FontLibrary.use("FractionOswald", [
    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
  ]);
  const FONT = (weight) => `${weight} 1000px FractionOswald`;
  const ctx = () => new Canvas(10, 10).getContext("2d");

  // CSS Fonts 4 makes `font-weight` a <number> from 1 to 1000. Chrome 156
  // (chrome-headless-shell 156.0.8078.4, macOS) instances Oswald at each of
  // these, which it holds to a quarter and so draws as written: the "0"
  // measures as below at 1000px, where 512 measures 535.105 and 513
  // 535.194.
  const CHROME = [
    [450, 525.515869140625],
    [512.25, 535.12744140625],
    [512.5, 535.1495971679688],
    [512.75, 535.1717529296875],
  ];

  test("ctx.font reads the weight back as written", () => {
    // CSSOM serialises the weight as its number, shortest form. Chrome's
    // canvas getter truncates it -- "512.5" reads back "512" --
    // docs/browser-differences.md says so.
    for (const [set, expected] of [
      ["512.5 16px serif", "512.5 16px serif"],
      ["550.7 16px serif", "550.7 16px serif"],
      ["999.9 16px serif", "999.9 16px serif"],
      ["calc(512.5) 16px serif", "512.5 16px serif"],
      ["512 16px serif", "512 16px serif"],
    ]) {
      const c = ctx();
      c.font = set;
      assert.equal(c.font, expected, set);
    }
  });

  test("`ch` follows the fraction, as the face's tables give it", () => {
    // One `ch` of letter spacing after each of two zeros, so the spacing's
    // share of the width is two `ch`. The tables quantise the weight's
    // normalised coordinate to 2.14 as OpenType requires, where Chrome's
    // CoreText does not: within 0.003 here. Each width is held to a
    // hundredth by SkParagraph, so half of one in the difference.
    const c = ctx();
    for (const [weight, chrome] of CHROME) {
      c.font = FONT(weight);
      c.letterSpacing = "0px";
      const plain = c.measureText("00").width;
      c.letterSpacing = "1ch";
      const ch = (c.measureText("00").width - plain) / 2;
      assert.ok(
        Math.abs(ch - chrome) <= 0.01,
        `${weight}: 1ch ${ch}, Chrome ${chrome}`,
      );
    }
  });

  test("the run is laid out at the fraction", () => {
    // FreeType rounds each `HVAR` advance delta to a whole font unit, so on
    // Linux the "0" advances by whole units at every weight, where CoreText
    // follows the interpolation as Chrome does -- see the UPSTREAM note at
    // the width in `Typesetter::measure`. `ch` above takes its advance from
    // the tables and is exact on both. Other platforms are not measured.
    const c = ctx();
    for (const [weight, chrome] of CHROME) {
      c.font = FONT(weight);
      const { width } = c.measureText("0");
      const expected = {
        darwin: chrome,
        linux: Math.round(chrome),
      }[process.platform];
      if (expected === undefined) continue;
      assert.ok(
        Math.abs(width - expected) <= 0.005,
        `${process.platform} ${weight}: ${width}, expected ${expected}`,
      );
    }
  });

  test("a paragraph's fontStyle weight keeps its fraction", () => {
    const laidOut = (weight) => {
      const pb = ParagraphBuilder.Make({
        textStyle: {
          fontSize: 1000,
          fontFamilies: ["FractionOswald"],
          fontStyle: { weight },
        },
      });
      pb.addText("0");
      const paragraph = pb.build();
      paragraph.layout(10000);
      return paragraph.getMaxIntrinsicWidth();
    };
    for (const [weight, chrome] of CHROME) {
      const expected = {
        darwin: chrome,
        linux: Math.round(chrome),
      }[process.platform];
      if (expected === undefined) continue;
      const width = laidOut(weight);
      assert.ok(
        Math.abs(width - expected) <= 0.005,
        `${process.platform} ${weight}: ${width}, expected ${expected}`,
      );
    }
  });
});

describe("measureText reports the laid-out height", () => {
  // `lines[].height` is the ink join of one line, so summing it does not
  // give the laid-out box -- which is the reason `height` is reported
  // rather than left to be derived.
  //
  // No pixel count is pinned here. The line box belongs to the face that
  // answered, and one family name does not name one face: `24px
  // Helvetica` lays out at 24 per line where Helvetica is installed and at
  // 28 where the request falls back to a metric-compatible substitute,
  // which is what Linux does. Measured across seven families and a name
  // that resolves to nothing, the per-line box ranges from 24 to 33 while
  // both claims below hold in every one.
  const TEXT =
    "hello there this wraps onto several lines indeed and keeps going";

  const laidOut = (ctx, width) => {
    const metrics = ctx.measureText(TEXT, width);
    return {
      lines: metrics.lines.length,
      height: metrics.height,
      ink: metrics.lines.reduce((total, line) => total + line.height, 0),
    };
  };

  test("counts the line box, not the ink", () => {
    const ctx = new Canvas(400, 400).getContext("2d");
    ctx.font = "24px Helvetica";
    ctx.textWrap = true;

    const narrow = laidOut(ctx, 100),
      wide = laidOut(ctx, 220);

    // Or the per-line comparison below holds for the boring reason.
    assert.ok(
      narrow.lines > wide.lines,
      `the two widths must wrap differently, got ${narrow.lines} and ${wide.lines}`,
    );

    for (const [name, measured] of [
      ["narrow", narrow],
      ["wide", wide],
    ])
      assert.ok(
        measured.height > measured.ink,
        `${name}: the ink join ${measured.ink} should fall short of the ` +
          `line boxes ${measured.height}`,
      );

    // The height counts line boxes, so dividing it by the line count gives
    // the same box at either width. The ink join does not: it follows
    // which glyphs happened to land on each line.
    assert.equal(
      narrow.height / narrow.lines,
      wide.height / wide.lines,
      "the line box should not depend on how the text was wrapped",
    );
  });
});

describe("`oblique` is not a spelling of italic", () => {
  // Skia's matcher does not fall back from oblique to italic: asking for
  // `Slant::Oblique` on a family with no oblique face returned the upright
  // one, so `oblique 64px Times` painted exactly what `64px Times` paints.
  // Chrome 148 renders it as the italic face. CSS Fonts 4 asks for an
  // oblique face first and an italic one before an upright one, so italic is
  // the right fallback and upright is not.
  //
  // Measured as ink rather than compared to a golden: the three faces differ
  // in coverage and centroid, and asserting oblique matches italic *and*
  // differs from upright is what separates "falls back correctly" from
  // "renders nothing special".
  const ink = (font) => {
    const canvas = new Canvas(240, 120),
      ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, 240, 120);
    ctx.font = font;
    ctx.fillStyle = "black";
    ctx.fillText("Aa", 5, 90);
    const data = ctx.getImageData(0, 0, 240, 120).data;
    let inked = 0,
      sumX = 0;
    for (let i = 0; i < 240 * 120; i++)
      if (data[i * 4] < 128) {
        inked++;
        sumX += i % 240;
      }
    return `${inked}:${(sumX / inked).toFixed(1)}`;
  };

  for (const family of ["Times", "Helvetica"]) {
    test(`\`oblique\` shears the upright face in ${family}`, () => {
      const upright = ink(`64px ${family}`),
        italic = ink(`italic 64px ${family}`),
        oblique = ink(`oblique 64px ${family}`);

      // The control: without it, a family whose italic and upright happen to
      // ink alike would make the assertions below pass for no reason.
      assert.notEqual(
        italic,
        upright,
        `${family} italic must differ from upright`,
      );
      // CSS Fonts 4 shears a face before it takes an italic for an oblique
      // request, and these families have no oblique face; Chrome draws the
      // italic instead, which docs/browser-differences.md records.
      assert.notEqual(
        oblique,
        italic,
        `${family} oblique must not use the italic face`,
      );
      assert.notEqual(
        oblique,
        upright,
        `${family} oblique must not render upright`,
      );
    });
  }

  test("the keyword reads back as itself", () => {
    // CSS Fonts 4 makes the computed value "the keyword specified", so the
    // string keeps `oblique`. Chrome reads it back as `italic`;
    // docs/browser-differences.md records that.
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = "oblique 64px Times";
    assert.match(ctx.font, /^oblique /);
  });
});

describe("a generic family is the face Chrome's default names", () => {
  // Chromium's default for each generic, in
  // chrome/app/resources/locale_settings_{mac,linux,win}.grd: on macOS
  // IDS_SANS_SERIF_FONT_FAMILY is Helvetica and IDS_SERIF_FONT_FAMILY
  // Times, on Linux and Windows Arial and Times New Roman. On Linux Chrome
  // asks fontconfig for those names and draws what it substitutes, and the
  // generic here does the same, so on every platform the generic is the
  // face its Chrome default resolves to here.
  const CHROME_DEFAULT = {
    darwin: { "sans-serif": "Helvetica", serif: "Times" },
    linux: { "sans-serif": "Arial", serif: "Times New Roman" },
    win32: { "sans-serif": "Arial", serif: "Times New Roman" },
  }[process.platform];
  const family = (font) => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = font;
    return ctx.measureText("Hamburgefonstiv").lines[0].runs[0].family;
  };

  test(
    "sans-serif is Chrome's default sans-serif",
    { skip: CHROME_DEFAULT ? false : "no Chrome default known here" },
    () => {
      assert.equal(
        family("40px sans-serif"),
        family(`40px ${CHROME_DEFAULT["sans-serif"]}`),
      );
    },
  );

  // Chrome 156 on macOS, each measured in a profile of its own: the canvas
  // default font and sans-serif at two more sizes, which are its Helvetica.
  test(
    "sans-serif measures as Chrome's",
    {
      skip:
        process.platform === "darwin"
          ? false
          : "Chrome 156's widths were measured on macOS",
    },
    () => {
      const ctx = new Canvas(10, 10).getContext("2d");
      assert.equal(ctx.font, "10px sans-serif");
      for (const [font, chrome] of [
        [null, 75.5908203125],
        ["16px sans-serif", 120.9453125],
        ["40px sans-serif", 302.36328125],
      ]) {
        if (font) ctx.font = font;
        const { width } = ctx.measureText("Hamburgefonstiv");
        assert.ok(
          Math.abs(width - chrome) <= CHROME_FLOAT,
          `${font ?? "the default font"}: ${width}, Chrome ${chrome}`,
        );
      }
    },
  );
});

describe("a registration claims the generic family it names", () => {
  const { createHash } = require("crypto"),
    FACE = "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    OTHER = "tests/assets/fonts/Monoton-Regular.woff";

  // Which face rendered cannot be settled by an extent -- two faces can share
  // a bounding box and cannot share every pixel. The sample carries
  // ascenders, a descender and a kerning pair for that reason.
  const painted = (family) => {
    let canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.font = `36px ${family}`;
    ctx.fillText("Wgq AVA", 5, 45);
    return createHash("sha256")
      .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
      .digest("hex");
  };

  test("a face registered as a generic is what the generic paints", () => {
    // The curated stack, before anything claims the name.
    let curated = painted("sans-serif");

    // A control name nothing else can answer for, so the test knows what the
    // face itself looks like. Comparing the generic against the generic
    // cannot tell "the registration won" from "both fell back to the same
    // thing", which is how this defect stayed invisible.
    FontLibrary.use("RalewayUnderAControlName", [FACE]);
    let face = painted("RalewayUnderAControlName");
    assert.notEqual(
      face,
      curated,
      "the control renders the same as the curated stack, so this test cannot discriminate",
    );

    FontLibrary.use("sans-serif", [FACE]);
    assert.equal(painted("sans-serif"), face);
  });

  test("a generic nobody claims still resolves to the curated stack", () => {
    // `serif` is untouched by the test above, and must be unaffected by it.
    let before = painted("serif");
    FontLibrary.use("AnotherControlName", [FACE]);
    assert.equal(painted("serif"), before);
  });

  test("the last registration under a name is the one that answers", () => {
    FontLibrary.use("FirstControl", [FACE]);
    FontLibrary.use("SecondControl", [OTHER]);
    let first = painted("FirstControl"),
      second = painted("SecondControl");
    assert.notEqual(first, second, "the two faces render alike");

    // Sequentially, and within one call: the last face wins either way. Pinned
    // rather than left to be discovered, since nothing about the signature
    // says which of several faces under one alias answers.
    FontLibrary.use("monospace", [FACE]);
    assert.equal(painted("monospace"), first);
    FontLibrary.use("monospace", [OTHER]);
    assert.equal(painted("monospace"), second);
    FontLibrary.use("cursive", [FACE, OTHER]);
    assert.equal(painted("cursive"), second);
  });
});

describe("a family name matches by its full case folding", () => {
  // CSS Fonts 4 section 5.1 names Unicode's Default Caseless Matching: the
  // full folding, so `ß` matches `SS` and a final `ς` matches `Σ`, without
  // the Turkic `ı` and without normalising.
  // Oswald, not Raleway: an earlier test registers Raleway as `sans-serif`,
  // which is what a name nothing answers draws in, and the control below
  // must tell the two apart.
  const { createHash } = require("crypto"),
    FACE = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
    FAMILY = "Straße Σίσυφος Iris Café";
  FontLibrary.use(FAMILY, [FACE]);

  /** @param {string} family quoted, so it is a family name, never a generic */
  const painted = (family) => paintedIn(`"${family}"`);
  /** @param {string} list a family list as `ctx.font` reads it */
  const paintedIn = (list) => {
    let canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.font = `36px ${list}`;
    ctx.fillText("Wgq AVA", 5, 45);
    return createHash("sha256")
      .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
      .digest("hex");
  };

  test("in any case, and with ß as SS", () => {
    const face = painted(FAMILY),
      fallback = painted("NoSuchFamilyAnywhere");
    assert.notEqual(face, fallback, "the control");
    for (const family of [
      "STRASSE ΣΊΣΥΦΟΣ IRIS CAFÉ",
      "strasse σίσυφοσ iris café",
      "sTRAßE σΊσυφος iRIS cafÉ",
    ])
      assert.equal(painted(family), face, family);
  });

  test("FontLibrary.has and FontLibrary.family match the same way", () => {
    for (const asked of [FAMILY, "STRASSE ΣΊΣΥΦΟΣ IRIS CAFÉ"]) {
      assert.equal(FontLibrary.has(asked), true, asked);
      // Reported under the name it was registered by, whatever it was
      // asked for by.
      assert.equal(FontLibrary.family(asked)?.family, FAMILY, asked);
    }
    for (const asked of [
      "Straße Σίσυφος ırıs Café",
      "Strase Σίσυφος Iris Café",
    ]) {
      assert.equal(FontLibrary.has(asked), false, asked);
      assert.equal(FontLibrary.family(asked), undefined, asked);
    }
  });

  test("asked for in another case before it is registered", () => {
    // A spelling resolved while nothing answered it must not be kept as a
    // miss once a registration under another case does.
    const late = "Spät Registriert Σ",
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    /** The pixels `ctx` paints in `family`, on the same context each time. */
    const paint = (/** @type {string} */ family) => {
      ctx.clearRect(0, 0, 320, 60);
      ctx.font = `36px "${family}"`;
      ctx.fillText("Wgq AVA", 5, 45);
      return createHash("sha256")
        .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
        .digest("hex");
    };
    const before = paint("SPÄT REGISTRIERT σ");
    assert.equal(before, painted("NoSuchFamilyAnywhere"), "a miss at first");
    FontLibrary.use(late, [FACE]);
    assert.equal(paint("SPÄT REGISTRIERT σ"), painted(FAMILY), "then a hit");
  });

  test("but a generic keyword only in ASCII case", () => {
    // Whether a name is the generic is a question about a CSS keyword, which
    // matches in ASCII case alone. A long s folds to s, so `ſerif` matches a
    // family `serif` but registering it must not claim the generic `serif`.
    const generic = paintedIn("serif");
    assert.notEqual(generic, painted(FAMILY), "the control");
    FontLibrary.use("ſerif", [FACE]);
    assert.equal(paintedIn("serif"), generic, "the generic keeps its stack");
    assert.equal(painted("ſERIF"), painted(FAMILY), "the family answers");
  });

  test("but not with a Turkic i or a decomposed letter", () => {
    const fallback = painted("NoSuchFamilyAnywhere");
    for (const family of [
      "Straße Σίσυφος ırıs Café",
      "Straße Σίσυφος Iris Café",
      "Strase Σίσυφος Iris Café",
    ])
      assert.equal(painted(family), fallback, family);
  });
});

describe("a claimed name is also registered under a private alias", () => {
  const { createHash } = require("crypto"),
    { loadImage } = require("../../lib"),
    FACE = "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    // A face distinct from anything earlier tests register, so the claimed
    // name cannot accidentally match whatever an unresolved family falls back
    // to -- those tests register under `sans-serif` and `system-ui`, which is
    // what the fallback is chosen from.
    OTHER_FACE = "tests/assets/fonts/Monoton-Regular.woff",
    // The contract between the provider and the SVG rewrite. A test that
    // breaks when this changes is doing its job: the rewrite derives the same
    // name and the two have to agree.
    PREFIX = "meo-skia-canvas private family: ";

  const painted = (family) => {
    let canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.font = `36px "${family}"`;
    ctx.fillText("Wgq AVA", 5, 45);
    return createHash("sha256")
      .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
      .digest("hex");
  };

  const svg = async (family) => {
    let image = await loadImage(
        "data:image/svg+xml;base64," +
          Buffer.from(
            `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="60">` +
              `<text x="5" y="45" font-size="36" font-family="${family}" fill="#000">Wgq&#160;AVA</text></svg>`,
          ).toString("base64"),
      ),
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    return createHash("sha256")
      .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
      .digest("hex");
  };

  test("the private alias reaches the face, and so does the claimed name", async () => {
    // A name the system already holds is the case the private alias exists
    // for: SVG text resolves from one provider, and a claim on a name the
    // system also has would otherwise put two faces in one family and leave
    // `matchStyle` to choose. Rewriting the document to an alias the system
    // cannot hold gives the caller's face a family of its own.
    //
    // Both halves are asserted because they are built by different code. The
    // alias reaching the face is this registration's doing. The claimed name
    // reaching it is the SVG rewrite's, which substitutes the alias into the
    // document before Skia parses it -- so the caller never writes the alias
    // and never sees it.
    //
    // The name is taken from what this machine holds rather than written in.
    // `Helvetica` was hard-coded, and on a machine without it `svg(name)`
    // rendered the *fallback* -- which an earlier test in this file makes
    // Raleway, by claiming `sans-serif`, the name the fallback is chosen
    // from. The pre-registration render and the post-registration one were
    // then the same face and the first assertion compared Raleway with
    // itself. Failed on the AlmaLinux container, which holds three DejaVu
    // families and no Helvetica.
    const claimed = SYSTEM_FAMILIES[0];
    assert.ok(
      claimed,
      "this machine reports no system families at all, so it cannot run a " +
        "test about claiming a name the system already holds",
    );

    let systemFace = await svg(claimed);
    FontLibrary.use(claimed, [FACE]);

    assert.notEqual(
      await svg(PREFIX + claimed),
      systemFace,
      `the alias has to reach the registered face, or nothing below matters ` +
        `-- claimed "${claimed}", one of ${SYSTEM_FAMILIES.length} the ` +
        `system holds`,
    );
    assert.equal(
      await svg(claimed),
      await svg(PREFIX + claimed),
      "and the claimed name reaches the same face, through the rewrite",
    );
  });

  test("the private alias is invisible to everything a caller reads", () => {
    FontLibrary.use("AClaimedName", [OTHER_FACE]);
    let names = FontLibrary.families;

    assert.deepEqual(
      names.filter((name) => name.startsWith(PREFIX)),
      [],
      "a private alias reached FontLibrary.families",
    );
    assert.equal(FontLibrary.family(PREFIX + "AClaimedName"), undefined);
    assert.deepEqual(
      FontLibrary.use("AnotherClaimedName", [FACE]).map((font) => font.family),
      ["AnotherClaimedName"],
      "use() reported a family the caller did not ask for",
    );

    // Canvas has no rewrite to send the private alias, so the provider it
    // builds does not carry one. Registering it there would make the alias a
    // family a caller can type that resolves to neither the registered face
    // nor a fallback -- so this asserts it resolves as an unmentioned name
    // does, and fails if the two providers are ever made to match.
    // Compared against the claimed name rather than against an unknown one:
    // earlier tests here register faces under `sans-serif` and `system-ui`,
    // which is what an unresolved family falls back to, so a fallback
    // baseline measures whatever those tests last did.
    assert.notEqual(
      painted(PREFIX + "AClaimedName"),
      painted("AClaimedName"),
      "the canvas provider resolves the private alias, so the two providers have been made to match",
    );

    // The inverse, because a fix that hid the private alias by dropping the
    // registration would pass every assertion above.
    assert.ok(names.includes("AClaimedName"));
    let ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = "36px AClaimedName";
    assert.equal(ctx.font, '36px "AClaimedName"');
  });
});

describe("a missing italic or bold is synthesised where Chrome synthesises it", () => {
  // Every expectation here was read from Chrome 152 on the same fonts,
  // loaded as FontFace: Oswald declared with its weight range, so `bold`
  // instances it, and Monoton, a static face of one weight.
  FontLibrary.use("Oswald", [
    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
  ]);
  FontLibrary.use("Monoton", ["tests/assets/fonts/Monoton-Regular.woff"]);

  const near = (actual, expected, tolerance, message) =>
    assert.ok(
      Math.abs(actual - expected) <= tolerance,
      `${message}: ${actual}, expected ${expected} within ${tolerance}`,
    );

  // The stem of an `I`: the slope of its left edge, fitted over the middle
  // seventy percent of its rows, and its mean width.
  const stem = (font) => {
    const ctx = new Canvas(300, 300).getContext("2d");
    ctx.font = font;
    ctx.fillText("I", 100, 250);
    const data = ctx.getImageData(0, 0, 300, 300).data,
      rows = [];
    for (let y = 0; y < 300; y++) {
      let left = null,
        right = null;
      for (let x = 0; x < 300; x++)
        if (data[(y * 300 + x) * 4 + 3] >= 128) {
          left ??= x;
          right = x;
        }
      if (left !== null) rows.push([y, left, right]);
    }
    const inner = rows.slice(
        Math.floor(rows.length * 0.15),
        Math.floor(rows.length * 0.85),
      ),
      n = inner.length,
      meanY = inner.reduce((sum, [y]) => sum + y, 0) / n,
      meanX = inner.reduce((sum, [, x]) => sum + x, 0) / n;
    let num = 0,
      den = 0;
    for (const [y, x] of inner) {
      num += (y - meanY) * (x - meanX);
      den += (y - meanY) ** 2;
    }
    const width = inner.reduce((sum, [, l, r]) => sum + r - l + 1, 0) / n;
    return { slope: num / den, width };
  };

  const ink = (font) => {
    const ctx = new Canvas(400, 160).getContext("2d");
    ctx.font = font;
    ctx.fillText("IHI", 20, 130);
    const data = ctx.getImageData(0, 0, 400, 160).data;
    let sum = 0;
    for (let i = 3; i < data.length; i += 4) sum += data[i];
    return sum;
  };

  const bounds = (font, text) => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = font;
    const m = ctx.measureText(text);
    return [m.actualBoundingBoxLeft, m.actualBoundingBoxRight];
  };

  test("an italic with no italic face is skewed by a quarter", () => {
    // Chrome: -0.2504 at 200px, with the stem's width unchanged.
    for (const font of [
      "italic 200px Oswald",
      "oblique 200px Oswald",
      "bold italic 200px Oswald",
    ]) {
      near(stem(font).slope, -0.25, 0.005, font);
    }
    near(stem("200px Oswald").slope, 0, 0.001, "the control");
    near(
      stem("italic 200px Oswald").width,
      stem("200px Oswald").width,
      1,
      "skewed, not widened",
    );
  });

  test("a variable face instanced at bold gets no faux bold over it", () => {
    // Chrome's bold italic stem is 36.62 wide against 36 for bold: the
    // `wght` instance, skewed, and not emboldened on top. And 900, past the
    // end of Oswald's `wght` axis at 700, paints exactly as 700 -- the one
    // case where the face is lighter than asked and still not faked.
    near(
      stem("bold italic 200px Oswald").width,
      stem("bold 200px Oswald").width,
      1,
      "bold italic",
    );
    near(
      stem("900 200px Oswald").width,
      stem("bold 200px Oswald").width,
      0.5,
      "900",
    );
  });

  // Chrome 152 fakes a bold over a static face exactly when the weight
  // asked for is 600 or more and the face is under 600, with one amount of
  // emboldening whatever the weight: a Montserrat face of weight 200 or 400
  // inks the same at 600 as at 900, and more than at 500, and its 700 face
  // inks the same at every weight. Skia's own rule -- 600 or more, and 200
  // or more above the face -- would embolden the 700 face at 900 and leave
  // a 500 face plain at 600.
  const MONTSERRAT = "tests/assets/fonts/montserrat-latin/montserrat-v30-latin";
  FontLibrary.use("MontserratLight", [`${MONTSERRAT}-200.woff2`]);
  FontLibrary.use("MontserratRegular", [`${MONTSERRAT}-regular.woff2`]);
  FontLibrary.use("MontserratBold", [`${MONTSERRAT}-700.woff2`]);
  FontLibrary.use("Montserrat", [
    `${MONTSERRAT}-200.woff2`,
    `${MONTSERRAT}-regular.woff2`,
    `${MONTSERRAT}-700.woff2`,
  ]);
  FontLibrary.use("Raleway", [
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
  ]);

  const drawn = (font, text, x = 4) => {
    const ctx = new Canvas(600, 100).getContext("2d");
    ctx.font = font;
    ctx.fillText(text, x, 70);
    return ctx.getImageData(0, 0, 600, 100).data;
  };
  const same = (a, b) => Buffer.from(a).equals(Buffer.from(b));

  test("a static face is faked bold as Chrome fakes it", () => {
    for (const [family, plain, bold] of [
      ["MontserratLight", 500, 600],
      ["MontserratRegular", 500, 600],
    ]) {
      const at = (weight) => drawn(`${weight} 48px ${family}`, "Hamburg");
      assert.ok(same(at(plain), at(400)), `${family} ${plain} is plain`);
      assert.ok(!same(at(bold), at(plain)), `${family} ${bold} is faked`);
      assert.ok(same(at(bold), at(900)), `${family}: one amount of bold`);
    }
    for (const weight of [600, 800, 900]) {
      assert.ok(
        same(
          drawn(`${weight} 48px MontserratBold`, "Hamburg"),
          drawn("700 48px MontserratBold", "Hamburg"),
        ),
        `a 700 face is not faked at ${weight}`,
      );
    }
  });

  // Each run is judged against its own face. "ЖЖ" falls back from
  // Montserrat, which has no Cyrillic, to Raleway, whose `wght` reaches 900;
  // Chrome inks the Latin exactly as plain Montserrat 700 beside it and the
  // Cyrillic as Raleway's 900 instance. One weight for the whole run would
  // be 900, and fake a bold over the Montserrat.
  test("each run of a family list is judged against its own face", () => {
    const ctx = new Canvas(10, 10).getContext("2d");
    ctx.font = "900 48px Montserrat, Raleway";
    const split = 4 + ctx.measureText("AB").width;
    const region = (data, from, to) => {
      const rows = [];
      for (let y = 0; y < 100; y++)
        rows.push(
          Buffer.from(data.subarray((y * 600 + from) * 4, (y * 600 + to) * 4)),
        );
      return Buffer.concat(rows);
    };
    const mixed = drawn("900 48px Montserrat, Raleway", "ABЖЖ"),
      left = Math.floor(split) - 2,
      right = Math.ceil(split) + 2;
    assert.ok(
      region(mixed, 0, left).equals(
        region(drawn("700 48px Montserrat", "AB"), 0, left),
      ),
      "the Latin is Montserrat 700, not emboldened",
    );
    assert.ok(
      region(mixed, right, 600).equals(
        region(drawn("900 48px Raleway", "ЖЖ", split), right, 600),
      ),
      "the Cyrillic is Raleway at 900",
    );
  });

  // Glyphs a static face lacks fall back to the next family, and a variable
  // face there is instanced at the weight asked for and not emboldened past
  // its axis, as on its own. Chrome 152 inks Cyrillic Oswald the same at
  // `900 48px Monoton, Oswald` as at `700 48px Oswald`, 3092 pixels, while
  // the Monoton glyphs beside it are still faked bold.
  test("a variable face after a static one is not emboldened past its axis", () => {
    const pixels = (font, text) => {
      const ctx = new Canvas(600, 100).getContext("2d");
      ctx.font = font;
      ctx.fillText(text, 4, 70);
      return Buffer.from(ctx.getImageData(0, 0, 600, 100).data);
    };
    assert.ok(
      pixels("900 48px Monoton, Oswald", "ЖЖЖ").equals(
        pixels("700 48px Oswald", "ЖЖЖ"),
      ),
      "the fallback Oswald draws as its 700 instance",
    );
    assert.ok(
      pixels("900 48px Monoton, Oswald", "AB").equals(
        pixels("900 48px Monoton", "AB"),
      ),
      "the Monoton glyphs are faked bold as on their own",
    );
    assert.ok(
      !pixels("900 48px Monoton", "AB").equals(pixels("48px Monoton", "AB")),
      "the control: bold Monoton is not plain Monoton",
    );
  });

  // A run of marks alone has no advance, and is still a run of its face.
  // Here the acute and diaeresis fall back from a face holding only "H" to
  // Montserrat's 700 face, which Chrome does not embolden at 900 -- see
  // "a static face is faked bold as Chrome fakes it". The run was found by
  // the text under its middle, and a run with no width has nothing under
  // it, so it kept the 900 Skia fakes a bold from: 27 per cent more ink.
  test("marks alone in a fallback face are judged against that face", () => {
    FontLibrary.use("RecursiveH", [
      "tests/assets/fonts/Recursive/Recursive-upright-H-subset.ttf",
    ]);
    FontLibrary.use("Montserrat700", [
      "tests/assets/fonts/montserrat-latin/montserrat-v30-latin-700.woff2",
    ]);
    const pixels = (font, text) => {
      const ctx = new Canvas(400, 200).getContext("2d");
      ctx.font = font;
      ctx.fillText(text, 100, 150);
      return Buffer.from(ctx.getImageData(0, 0, 400, 200).data);
    };
    for (const text of ["\u0301\u0308", "x\u0301\u0308"])
      assert.ok(
        pixels("900 80px RecursiveH, Montserrat700", text).equals(
          pixels("900 80px Montserrat700", text),
        ),
        `${JSON.stringify(text)} draws as Montserrat 700 does on its own`,
      );
  });

  // A family with an italic face of its own: the italic is drawn as it
  // is, and skewed again it would lean by its own slope plus a quarter.
  // Raleway ships one, so the case runs wherever the suite does. Chrome
  // 152, the two files loaded as one family: -0.2139 italic, -0.2134 bold
  // italic.
  FontLibrary.use("RalewayWithItalic", [
    "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf",
  ]);

  test("a face that is itself slanted is not skewed again", () => {
    near(
      stem("italic 200px RalewayWithItalic").slope,
      -0.2139,
      0.005,
      "italic",
    );
    near(
      stem("bold italic 200px RalewayWithItalic").slope,
      -0.2134,
      0.005,
      "bold italic",
    );
    near(stem("200px RalewayWithItalic").slope, 0, 0.001, "the control");
  });

  // An `oblique` angle is drawn as CSS Fonts 4 section 5.2 matches it: an
  // oblique face of the angle's sign where the family has one, a `slnt`
  // axis set to the angle where it has that, and otherwise the upright face
  // sheared to the angle -- before, and so instead of, any italic face. The
  // slope of a shear is tan(angle), negative for a clockwise lean in these
  // pixel rows. Chrome draws none of this; docs/browser-differences.md has
  // its numbers.
  const tan = (degrees) => Math.tan((degrees * Math.PI) / 180);
  const R = "tests/assets/fonts/Recursive";
  FontLibrary.use("RecursiveSlnt", [`${R}/Recursive-slnt-subset.ttf`]);
  FontLibrary.use("RecursiveStatic", [
    `${R}/Recursive-upright-subset.ttf`,
    `${R}/Recursive-oblique12-subset.ttf`,
  ]);
  FontLibrary.use("RecursiveOblique12", [
    `${R}/Recursive-oblique12-subset.ttf`,
  ]);

  test("an oblique angle shears an upright face to the angle", () => {
    for (const family of ["Oswald", "RalewayWithItalic"]) {
      for (const angle of [0.5, 10, 13.9, 14, 30, -14]) {
        const font = `oblique ${angle}deg 200px ${family}`;
        near(stem(font).slope, -tan(angle), 0.005, font);
      }
      near(
        stem(`oblique 0deg 200px ${family}`).slope,
        0,
        0.001,
        `${family} 0deg`,
      );
    }
  });

  const pixels = (font, setup = () => {}) => {
    const ctx = new Canvas(300, 300).getContext("2d");
    ctx.font = font;
    setup(ctx);
    ctx.fillText("IH", 60, 250);
    return Buffer.from(ctx.getImageData(0, 0, 300, 300).data);
  };

  test("a slnt axis is set to the angle, as far as the axis goes", () => {
    // The same face at the same `slnt`, asked for directly: OpenType counts
    // the axis counter-clockwise, so it is the angle negated, and an angle
    // past its end sets it as far as the axis goes.
    for (const [angle, slnt] of [
      [10, -10],
      [14, -14],
      [30, -15],
      [90, -15],
    ]) {
      assert.ok(
        pixels(`oblique ${angle}deg 200px RecursiveSlnt`).equals(
          pixels("200px RecursiveSlnt", (ctx) => {
            ctx.fontVariationSettings = `"slnt" ${slnt}`;
          }),
        ),
        `${angle}deg is slnt ${slnt}`,
      );
    }
    // The axis reaches no negative angle, so that one is sheared.
    near(
      stem("oblique -14deg 200px RecursiveSlnt").slope,
      -tan(-14),
      0.005,
      "-14deg",
    );
    assert.ok(
      pixels("oblique 0deg 200px RecursiveSlnt").equals(
        pixels("200px RecursiveSlnt"),
      ),
      "0deg is upright and unsheared",
    );
  });

  test("an oblique face of the angle's sign is drawn as it is", () => {
    // RecursiveStatic is two static instances of Recursive: the upright one
    // and slnt -12 flagged oblique, with an italicAngle of -12. Every
    // positive angle takes the oblique face unsheared, the only oblique
    // value of that sign the family has; a negative one finds none, and the
    // upright face is sheared.
    const oblique = pixels("200px RecursiveOblique12");
    for (const angle of [0.5, 10, 14, 30, 90]) {
      assert.ok(
        pixels(`oblique ${angle}deg 200px RecursiveStatic`).equals(oblique),
        `${angle}deg draws the oblique face`,
      );
    }
    near(
      stem("oblique -14deg 200px RecursiveStatic").slope,
      -tan(-14),
      0.005,
      "-14deg",
    );
    assert.ok(
      pixels("oblique 0deg 200px RecursiveStatic").equals(
        pixels("200px RecursiveStatic"),
      ),
      "0deg is upright and unsheared",
    );
  });

  // A sheared glyph is drawn in a sheared font, not under a sheared canvas:
  // the canvas transform carries the paint too, so a gradient, a pattern, a
  // shadow's blur and a stroke's pen would lean with the glyphs. Each is
  // held to the paint in user space, or to the same outline drawn as a path.
  test("a gradient is not sheared with the glyphs", () => {
    const W = 510,
      H = 120;
    const opaque = (ctx, check) => {
      const d = ctx.getImageData(0, 0, W, H).data;
      let n = 0,
        off = 0;
      for (let y = 0; y < H; y++)
        for (let x = 0; x < W; x++) {
          const i = (y * W + x) * 4;
          if (d[i + 3] !== 255) continue;
          n++;
          off += check(d, i, x);
        }
      return { n, off: off / n };
    };
    for (const angle of [14, 20, 50]) {
      const font = `bold oblique ${angle}deg 90px Oswald`;

      // The gradient's red is the canvas x it was drawn at.
      const g = new Canvas(W, H).getContext("2d"),
        ramp = g.createLinearGradient(0, 0, W - 1, 0);
      ramp.addColorStop(0, "rgb(0,0,0)");
      ramp.addColorStop(1, "rgb(255,0,0)");
      g.fillStyle = ramp;
      g.font = font;
      g.fillText("IIIIIIII", 150, 100);
      const ramped = opaque(g, (d, i, x) => d[i] - (x / (W - 1)) * 255);
      assert.ok(ramped.n > 0, "the control: the text is drawn");
      assert.ok(
        Math.abs(ramped.off) < 2,
        `${font}: a gradient stays where it was set, red off by ${ramped.off}`,
      );
    }
  });

  test("a pattern is not sheared with the glyphs", () => {
    const W = 510,
      H = 120;
    const opaque = (ctx, check) => {
      const d = ctx.getImageData(0, 0, W, H).data;
      let n = 0,
        off = 0;
      for (let y = 0; y < H; y++)
        for (let x = 0; x < W; x++) {
          const i = (y * W + x) * 4;
          if (d[i + 3] !== 255) continue;
          n++;
          off += check(d, i, x);
        }
      return { n, off: off / n };
    };
    for (const angle of [14, 20, 50]) {
      const font = `bold oblique ${angle}deg 90px Oswald`;

      // Stripes five pixels wide, red then blue, from the canvas's x = 0.
      const tile = new Canvas(10, 1).getContext("2d");
      tile.fillStyle = "red";
      tile.fillRect(0, 0, 5, 1);
      tile.fillStyle = "blue";
      tile.fillRect(5, 0, 5, 1);
      const p = new Canvas(W, H).getContext("2d");
      p.imageSmoothingEnabled = false;
      p.fillStyle = p.createPattern(tile.canvas, "repeat");
      p.font = font;
      p.fillText("IIIIIIII", 150, 100);
      const striped = opaque(p, (d, i, x) =>
        Number(d[i] > d[i + 2] !== x % 10 < 5),
      );
      assert.ok(
        striped.off < 0.01,
        `${font}: a pattern stays where it was set, ${striped.off} of its pixels moved`,
      );
    }
  });

  test("a shadow's blur is not sheared with the glyphs", () => {
    // The spread of a shadow across one row, against the same text drawn
    // first and given its shadow as an image.
    const W = 700,
      H = 260,
      ROW = 110,
      AWAY = 300;
    const spread = (ctx) => {
      const d = ctx.getImageData(0, 0, W, H).data;
      let m = 0,
        s = 0,
        s2 = 0;
      for (let x = AWAY; x < W; x++) {
        const a = d[(ROW * W + x) * 4 + 3];
        m += a;
        s += a * x;
        s2 += a * x * x;
      }
      return Math.sqrt(s2 / m - (s / m) ** 2);
    };
    const shadowed = (ctx) => {
      ctx.shadowColor = "blue";
      ctx.shadowBlur = 40;
      ctx.shadowOffsetX = AWAY;
    };
    for (const angle of [30, 60]) {
      const font = `bold oblique ${angle}deg 100px Oswald`;
      const direct = new Canvas(W, H).getContext("2d");
      direct.font = font;
      shadowed(direct);
      direct.fillText("I", 100, 150);
      const text = new Canvas(W, H).getContext("2d");
      text.font = font;
      text.fillText("I", 100, 150);
      const reference = new Canvas(W, H).getContext("2d");
      shadowed(reference);
      reference.drawImage(text.canvas, 0, 0);
      near(spread(direct), spread(reference), 0.5, `${font}: shadow spread`);
    }
  });

  test("a stroke's pen is not sheared with the glyphs", () => {
    // strokeText against stroking the outline outlineText returns, which
    // is the sheared outline under an unsheared pen. Text coverage goes
    // through the text gamma and a path's does not, so both canvases take
    // a gamma of 1 and no contrast, which leaves coverage as it is, and the
    // two compare by shape alone.
    const W = 700,
      H = 260,
      linear = { textGamma: 1, textContrast: 0 };
    for (const angle of [30, 60]) {
      const font = `bold oblique ${angle}deg 100px Oswald`;
      const stroked = new Canvas(W, H, linear).getContext("2d"),
        outlined = new Canvas(W, H, linear).getContext("2d");
      for (const ctx of [stroked, outlined]) {
        ctx.font = font;
        ctx.lineWidth = 12;
      }
      stroked.strokeText("IH", 150, 220);
      const outline = outlined.outlineText("IH");
      outlined.translate(150, 220);
      outlined.stroke(outline);
      const ink = (ctx) =>
        ctx
          .getImageData(0, 0, W, H)
          .data.reduce((sum, v, i) => (i % 4 === 3 ? sum + v : sum), 0);
      const a = ink(stroked),
        b = ink(outlined);
      assert.ok(
        Math.abs(a - b) / b < 0.02,
        `${font}: strokeText inks ${a / 255}, the stroked outline ${b / 255}`,
      );
    }
  });

  test("a shear to a right angle draws nothing, and nearer one a sliver", () => {
    // As the angle nears 90deg a sheared glyph thins towards its baseline,
    // so its coverage of any finite region tends to nothing: 90deg is that
    // limit, and 89.9deg is still a faint sliver on the way to it.
    const metrics = (font) => {
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.font = font;
      ctx.fillText("IH", 60, 250);
      const m = ctx.measureText("IH"),
        inked = ctx
          .getImageData(0, 0, 300, 300)
          .data.filter((_, i) => i % 4 === 3 && _ > 0).length;
      return {
        inked,
        m,
        outline: ctx.outlineText("IH").bounds,
        numbers: [
          m.width,
          m.actualBoundingBoxLeft,
          m.actualBoundingBoxRight,
          m.actualBoundingBoxAscent,
          m.actualBoundingBoxDescent,
          ...Object.values(ctx.outlineText("IH").bounds),
        ],
      };
    };
    const upright = metrics("200px Oswald"),
      blank = (() => {
        const ctx = new Canvas(10, 10).getContext("2d");
        ctx.font = "200px Oswald";
        return ctx.measureText("  ");
      })(),
      nothing = new Canvas(10, 10).getContext("2d").outlineText("").bounds;
    assert.ok(upright.inked > 0, "the control paints");
    for (const angle of [90, -90]) {
      const flat = metrics(`oblique ${angle}deg 200px Oswald`);
      assert.equal(flat.inked, 0, `${angle}deg paints nothing`);
      assert.equal(
        flat.m.width,
        upright.m.width,
        `${angle}deg advances as upright`,
      );
      for (const key of [
        "actualBoundingBoxLeft",
        "actualBoundingBoxRight",
        "actualBoundingBoxAscent",
        "actualBoundingBoxDescent",
      ])
        assert.equal(
          flat.m[key],
          blank[key],
          `${angle}deg ${key} is a blank's`,
        );
      assert.deepEqual(
        flat.outline,
        nothing,
        `${angle}deg has an empty outline`,
      );
      assert.ok(flat.numbers.every(Number.isFinite), `${angle}deg: no NaN`);
    }
    for (const angle of [89.9, -89.9]) {
      const sliver = metrics(`oblique ${angle}deg 200px Oswald`);
      assert.ok(sliver.inked > 0, `${angle}deg still paints`);
      assert.ok(
        sliver.inked < upright.inked,
        `${angle}deg paints less than upright: ${sliver.inked} of ${upright.inked}`,
      );
      assert.equal(
        sliver.m.width,
        upright.m.width,
        `${angle}deg advances as upright`,
      );
      assert.ok(sliver.numbers.every(Number.isFinite), `${angle}deg: no NaN`);
    }
  });

  // The slope of the `index`th stem, from the left, over the rows of
  // `ctx` that cross exactly `count` stems.
  const nthStemSlope = (ctx, count, index) => {
    const { width, height } = ctx.canvas,
      data = ctx.getImageData(0, 0, width, height).data,
      edges = [];
    for (let y = 0; y < height; y++) {
      const starts = [];
      for (let x = 0, on = false; x < width; x++) {
        const now = data[(y * width + x) * 4 + 3] >= 128;
        if (now && !on) starts.push(x);
        on = now;
      }
      if (starts.length === count) edges.push([y, starts[index]]);
    }
    const inner = edges.slice(
        Math.floor(edges.length * 0.15),
        Math.floor(edges.length * 0.85),
      ),
      n = inner.length,
      my = inner.reduce((sum, [y]) => sum + y, 0) / n,
      mx = inner.reduce((sum, [, x]) => sum + x, 0) / n;
    let num = 0,
      den = 0;
    for (const [y, x] of inner) {
      num += (y - my) * (x - mx);
      den += (y - my) ** 2;
    }
    return num / den;
  };

  test("a fallback run between two of another face is sheared once", () => {
    // Recursive's oblique face draws the `I`s and has no `l`, which falls
    // back to Oswald, upright, and is sheared: by tan(14deg) and not by
    // that and a fake italic on top, on one line and on the second line of
    // a wrapped one.
    const one = new Canvas(500, 220).getContext("2d");
    one.font = "oblique 150px RecursiveStatic, Oswald";
    one.fillText("IlI", 40, 180);
    near(nthStemSlope(one, 3, 1), -tan(14), 0.01, "the fallback l");
  });

  test("a wrapped fallback run is sheared once on the line it lands on", () => {
    // As above, with the run on the second line of a wrapped one, where
    // SkParagraph reports the run's text offsets from its first line.
    const two = new Canvas(500, 480).getContext("2d");
    two.font = "oblique 150px RecursiveStatic, Oswald";
    two.textWrap = true;
    two.fillText("II IlI", 40, 180, 300);
    near(nthStemSlope(two, 3, 1), -tan(14), 0.01, "the fallback l, wrapped");
  });

  test("a fallback face that leans already is not sheared again", () => {
    // The upright instance with no `I`, so an `I` falls back to the slnt
    // instance, which leans by itself: sheared on top, it would slope twice.
    FontLibrary.use("RecursiveWithoutI", [
      `${R}/Recursive-upright-H-subset.ttf`,
    ]);
    near(
      stem("oblique 10deg 200px RecursiveWithoutI, RecursiveSlnt").slope,
      stem("oblique 10deg 200px RecursiveSlnt").slope,
      0.005,
      "the fallback leans as the slnt instance does alone",
    );
    near(
      stem("oblique 10deg 200px RecursiveWithoutI, RecursiveSlnt").slope,
      -tan(10),
      0.005,
      "and that is the angle asked for",
    );
  });

  test("a static face lighter than asked is faked bold from 600", () => {
    // Chrome: 1,442,282 at 400 and 500, 2,237,980 at 600, bold and 900.
    const regular = ink("100px Monoton");
    assert.equal(ink("500 100px Monoton"), regular, "500 is not faked");
    for (const font of ["600 100px Monoton", "bold 100px Monoton"])
      assert.ok(ink(font) > regular * 1.4, `${font} is emboldened`);
    assert.equal(ink("900 100px Monoton"), ink("bold 100px Monoton"));
  });

  test("measureText reports the synthesised ink as Chrome does", () => {
    // Each pair Chrome 152's actualBoundingBoxLeft and Right; the advance
    // is unchanged by either synthesis, as Chrome leaves it.
    for (const [font, text, chrome] of [
      ["italic 40px Oswald", "I", [-2.76, 15.3]],
      ["bold italic 40px Oswald", "I", [-2.44, 17.7]],
      ["bold 100px Monoton", "IHI", [-2.979, 154.736]],
      ["italic 100px Monoton", "IHI", [-4.541, 173.389]],
      ["bold italic 100px Monoton", "IHI", [-2.54, 175.39]],
    ]) {
      const [left, right] = bounds(font, text);
      near(left, chrome[0], 0.01, `${font} left`);
      near(right, chrome[1], 0.01, `${font} right`);
    }
  });
});

describe("a quoted generic keyword is a family name", () => {
  // CSS Fonts 4 section 2.1: the generic keywords "must not be quoted", so
  // `"serif"` in quotes names a family called serif, never the generic.
  // Last in this file: one test registers a family under `serif`, which
  // claims the generic for whatever draws after it.
  const { createHash } = require("crypto"),
    FACE = "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";
  const painted = (font) => {
    const canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.font = font;
    ctx.fillText("Wgq AVA", 5, 45);
    return createHash("sha256")
      .update(Buffer.from(ctx.getImageData(0, 0, 320, 60).data))
      .digest("hex");
  };

  test("with no family of that name, the list moves on", () => {
    // fontconfig answers `serif`, `sans-serif` and `monospace` with faces
    // of its own choosing, which a quoted name must not take. Reset first:
    // the tests above register families under `sans-serif` and
    // `monospace`, and a family of that name exists once they have.
    FontLibrary.reset();
    const missing = painted("36px NoSuchFamilyQuoted");
    for (const font of [
      '36px "serif"',
      "36px 'SERIF'",
      '36px "sans-serif"',
      '36px "monospace"',
    ]) {
      assert.equal(painted(font), missing, font);
    }
    // The control: the generics themselves draw something else, or the
    // comparison above could not tell the two apart.
    assert.notEqual(painted("36px serif"), missing, "bare serif");
    // What a missing family draws is the library's `sans-serif`, the canvas
    // default family: pinned to that family rather than to another
    // fallback, since the comparisons above move with whatever it is.
    assert.equal(
      painted("36px sans-serif"),
      missing,
      "a missing family draws the library's sans-serif",
    );
    FontLibrary.use("QuotedNextFamily", [FACE]);
    assert.equal(
      painted('36px "serif", QuotedNextFamily'),
      painted("36px QuotedNextFamily"),
      "the next family in the list draws",
    );
  });

  test("a family registered under the name draws, from the next draw on", () => {
    const missing = painted("36px NoSuchFamilyQuoted"),
      face = painted("36px QuotedNextFamily");
    assert.equal(painted('36px "serif"'), missing, "before it is registered");
    // Registered under `serif`, which is also a claim on the generic: both
    // spellings draw it.
    FontLibrary.use("serif", [FACE]);
    assert.equal(painted('36px "serif"'), face, "quoted, after");
    assert.equal(painted("36px serif"), face, "bare, the claim");
    // A family name matches without regard to case, so the quoted name in
    // capitals is the same family.
    assert.equal(painted('36px "SERIF"'), face, "quoted, in capitals");
    FontLibrary.reset();
  });

  test("a registered family matches its name in any case", () => {
    // CSS Fonts 4 section 5.1: "User agents must match these names case
    // insensitively". The provider the library registers faces with matches
    // a name exactly, so `oswaldx` missed a family registered as `OswaldX`
    // and drew whatever came next.
    FontLibrary.use("CaseRegistered", [FACE]);
    const face = painted("36px CaseRegistered");
    assert.notEqual(face, painted("36px NoSuchFamilyQuoted"), "the control");
    for (const font of [
      "36px caseregistered",
      "36px CASEREGISTERED",
      '36px "caseRegistered"',
    ]) {
      assert.equal(painted(font), face, font);
    }
    // And in a paragraph, whose `fontFamilies` go the same way.
    const laidOut = (family) => {
      const pb = ParagraphBuilder.Make({
        textStyle: { fontSize: 36, fontFamilies: [family] },
      });
      pb.addText("Wgq AVA");
      const paragraph = pb.build();
      paragraph.layout(1000);
      return paragraph.getMaxIntrinsicWidth();
    };
    const registered = laidOut("CaseRegistered");
    assert.notEqual(
      registered,
      laidOut("NoSuchFamilyQuoted"),
      "paragraph control",
    );
    assert.equal(
      laidOut("caseregistered"),
      registered,
      "paragraph, lower case",
    );
    FontLibrary.reset();
  });
});
