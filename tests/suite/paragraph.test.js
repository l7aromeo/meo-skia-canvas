// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { execFileSync } = require("child_process"),
  { Canvas, ParagraphBuilder, FontLibrary } = require("../../lib");

// Long enough to wrap several times at the widths used below.
const PROSE =
  "The quick brown fox jumps over the lazy dog, and then keeps on running " +
  "well past the edge of the box it was given to run in.";

function laidOut(style, text = PROSE, width = 120) {
  let pb = ParagraphBuilder.Make(style);
  pb.addText(text);
  let paragraph = pb.build();
  paragraph.layout(width);
  return paragraph;
}

describe("ParagraphBuilder", () => {
  test("accepts an empty or partial style", () => {
    assert.ok(ParagraphBuilder.Make({}));
    assert.ok(ParagraphBuilder.Make({ textAlign: "center" }));
    assert.ok(ParagraphBuilder.Make());
  });

  test("wraps text across multiple lines", () => {
    let paragraph = laidOut({ textStyle: { fontSize: 16 } });
    assert.ok(
      paragraph.getNumberOfLines() > 1,
      `expected wrapping, got ${paragraph.getNumberOfLines()} line(s)`,
    );
    assert.equal(paragraph.getMaxWidth(), 120);
    assert.ok(paragraph.getLongestLine() <= 120, "no line exceeds the width");
  });

  test("honours maxLines and reports the overflow", () => {
    let unbounded = laidOut({ textStyle: { fontSize: 16 } }),
      bounded = laidOut({ maxLines: 3, textStyle: { fontSize: 16 } });

    assert.ok(
      unbounded.getNumberOfLines() > 3,
      "the fixture must overflow 3 lines for this to mean anything",
    );
    assert.equal(bounded.getNumberOfLines(), 3);
    assert.equal(bounded.didExceedMaxLines(), true);
    assert.equal(unbounded.didExceedMaxLines(), false);
    assert.ok(
      bounded.getHeight() < unbounded.getHeight(),
      "clipping to 3 lines should shorten the paragraph",
    );
  });

  test("scales with fontSize", () => {
    let small = laidOut({ maxLines: 1, textStyle: { fontSize: 12 } }),
      large = laidOut({ maxLines: 1, textStyle: { fontSize: 36 } });
    assert.ok(
      large.getHeight() > small.getHeight(),
      `36px (${large.getHeight()}) should be taller than 12px (${small.getHeight()})`,
    );
  });

  test("positions a short line differently for left, center and right", () => {
    let left = (align) => {
      let paragraph = laidOut(
        { textAlign: align, textStyle: { fontSize: 12 } },
        "Hi",
        200,
      );
      return paragraph.getRectsForRange(0, 2)[0].rect[0];
    };
    let l = left("left"),
      c = left("center"),
      r = left("right");
    assert.ok(
      l < c && c < r,
      `expected left < center < right, got ${l},${c},${r}`,
    );
    assert.ok(l < 1, "left-aligned text should start at the origin");
  });

  test("applies a pushed style only until pop()", () => {
    let build = (push) => {
      let pb = ParagraphBuilder.Make({ textStyle: { fontSize: 10 } });
      if (push) pb.pushStyle({ fontSize: 40 });
      pb.addText("AAAA");
      if (push) pb.pop();
      pb.addText("BBBB");
      let paragraph = pb.build();
      paragraph.layout(1000);
      return paragraph;
    };
    // The pushed run is 4x the base size, so the line box has to grow.
    assert.ok(
      build(true).getHeight() > build(false).getHeight(),
      "pushStyle should affect layout",
    );
    // ...and the text after pop() returns to the base size, so the line is
    // shorter than it would be with the large style applied throughout.
    let pb = ParagraphBuilder.Make({ textStyle: { fontSize: 10 } });
    pb.pushStyle({ fontSize: 40 });
    pb.addText("AAAABBBB");
    let allLarge = pb.build();
    allLarge.layout(1000);
    assert.ok(
      build(true).getLongestLine() < allLarge.getLongestLine(),
      "pop() should return to the base size",
    );
  });

  test("an unrecognised style key is refused in strict mode", () => {
    // How the locale gap stayed invisible: `{ locale: "ja" }` built a
    // paragraph, laid it out and changed nothing, because the parser reads
    // the keys it knows and never looks at the rest. A misspelling behaves
    // the same way, so `fontsize` silently leaves the size alone.
    //
    // Tolerant by default, as the Canvas API is about values it does not
    // recognise; loud under `SKIA_CANVAS_STRICT`, which is the flag this
    // tree already uses to separate "ignore it" from "tell me".
    assert.ok(
      ParagraphBuilder.Make({ textStyle: { fontSize: 16, nonsense: 1 } }),
      "an unknown key is tolerated by default",
    );

    // A second process, because the flag is read when the module loads.
    const script = `
      const { ParagraphBuilder } = require(${JSON.stringify(require.resolve("../../lib"))});
      const said = {};
      for (const [label, style] of [
        ["unknown", { fontSize: 16, nonsense: 1 }],
        ["misspelled", { fontsize: 16 }],
        ["known", { fontSize: 16, locale: "ja" }],
      ]) {
        try { ParagraphBuilder.Make({ textStyle: style }); said[label] = null }
        catch (error) { said[label] = error.message }
      }
      console.log(JSON.stringify(said));
    `;
    const said = JSON.parse(
      execFileSync(process.execPath, ["-e", script], {
        encoding: "utf8",
        env: { ...process.env, SKIA_CANVAS_STRICT: "1" },
      }),
    );
    assert.match(
      String(said.unknown),
      /nonsense/,
      "strict mode should name the key it did not recognise",
    );
    assert.match(
      String(said.misspelled),
      /fontsize/,
      "a misspelling is the case this exists for",
    );
    assert.equal(said.known, null, "a key it does know is not refused");
  });

  test("a stroke width outlines the glyphs instead of filling them", () => {
    // Outlined text -- what CSS calls `-webkit-text-stroke`. Skia's text takes
    // one paint and paints one way, so a run is filled or stroked, never
    // both; a caller wanting both draws the paragraph twice in the order they
    // want, which is what makes `paint-order` expressible.
    //
    // Counting ink cannot tell them apart, because a heavy stroke inks more
    // than a fill. What can is how many times a line crosses ink: a filled
    // "O" is two bands, its left and right sides, while a stroked one is
    // four, because each side becomes an inner and an outer edge with paper
    // between. That holds whatever the font and wherever it places the glyph.
    const W = 220,
      H = 140,
      ROW = 87;
    const bands = (textStyle) => {
      let canvas = new Canvas(W, H);
      canvas.gpu = false;
      let ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, W, H);
      let pb = ParagraphBuilder.Make({
        textStyle: { fontSize: 120, color: "black", ...textStyle },
      });
      pb.addText("O");
      let paragraph = pb.build();
      paragraph.layout(W);
      ctx.drawParagraph(paragraph, 10, 10);

      let { data } = ctx.getImageData(0, 0, W, H),
        crossings = 0,
        inside = false;
      for (let x = 0; x < W; x++) {
        let ink = data[(ROW * W + x) * 4] < 128;
        if (ink && !inside) crossings++;
        inside = ink;
      }
      return crossings;
    };

    assert.equal(bands({}), 2, "a filled O crosses ink twice");
    assert.equal(
      bands({ strokeWidth: 3 }),
      4,
      "a stroked O crosses it four times, once per edge",
    );

    // Not positive is ignored rather than refused, as `lineWidth` is and as a
    // browser does -- Skia would read zero as a hairline instead.
    assert.equal(bands({ strokeWidth: 0 }), 2, "zero leaves the glyphs filled");
    assert.equal(bands({ strokeWidth: -2 }), 2, "and so does a negative width");
  });

  test("locale decides which language's glyphs a shared codepoint gets", () => {
    // Han unification: 直骨今 are one set of codepoints with different
    // letterforms in Japanese and in Simplified Chinese. Which a reader
    // should see is a property of the text's language, not of the
    // characters, so it cannot be inferred -- the caller has to say. Without
    // a locale the fallback picks one, and a Japanese document silently gets
    // Chinese shapes.
    //
    // Naming a font instead works and is not a substitute: it gives up
    // fallback for every codepoint the named font lacks.
    const HAN = "直骨今";
    const laid = (locale) => {
      let pb = ParagraphBuilder.Make({ textStyle: { fontSize: 24, locale } });
      pb.addText(HAN);
      let paragraph = pb.build();
      paragraph.layout(2000);
      return paragraph;
    };

    // Accepting the key and ignoring it is the failure this guards against,
    // so the assertion is that the two differ, not that either has a
    // particular width.
    assert.ok(laid("ja"), "a locale is accepted");

    const bothPresent =
      FontLibrary.has("Hiragino Sans") && FontLibrary.has("PingFang SC");
    if (!bothPresent) return; // no Japanese and Chinese faces to tell apart

    assert.notEqual(
      laid("ja").getLongestLine(),
      laid("zh-Hans").getLongestLine(),
      "the same codepoints laid out identically for Japanese and Chinese, " +
        "so the locale reached nothing",
    );
  });

  test("hit-testing walks the line in order and clamps at both ends", () => {
    // `getGlyphPositionAtCoordinate` is one of two things here a browser
    // canvas has no equivalent for, and a wrong answer is invisible: the
    // glyphs still paint correctly, the click just lands on the wrong
    // character. Nothing about rendering catches that, so it is asserted
    // directly.
    let paragraph = laidOut({ textStyle: { fontSize: 20 } }, "ABCDEFGH", 1000),
      at = (x) => paragraph.getGlyphPositionAtCoordinate(x, 10).pos,
      width = paragraph.getLongestLine();

    assert.equal(at(-50), 0, "left of the line clamps to the first position");
    assert.equal(at(width + 50), 8, "right of it clamps past the last");

    // Monotonic across the run. Exact widths belong to the font, so this
    // asserts the ordering rather than any particular coordinate.
    let seen = [];
    for (let i = 0; i <= 10; i++) seen.push(at((width * i) / 10));
    for (let i = 1; i < seen.length; i++)
      assert.ok(
        seen[i] >= seen[i - 1],
        `position went backwards across the line: ${seen.join(",")}`,
      );
    assert.ok(seen[seen.length - 1] > seen[0], "the sweep covered the run");
  });

  test("a right-to-left run is laid out and hit-tested right to left", () => {
    // Bidi comes from ICU rather than from the font, so the direction and the
    // ordering hold wherever this runs; the coordinates do not, and are not
    // asserted.
    let paragraph = laidOut({ textStyle: { fontSize: 20 } }, "שלום", 1000),
      rects = paragraph.getRectsForRange(0, 2);

    assert.ok(rects.length >= 1, "a range inside the run has a rect");
    assert.equal(rects[0].direction, 0, "the run reports right-to-left");

    // The first characters sit at the right-hand end, so a hit near the left
    // edge lands later in the string than one near the right.
    let width = paragraph.getLongestLine(),
      left = paragraph.getGlyphPositionAtCoordinate(width * 0.1, 10).pos,
      right = paragraph.getGlyphPositionAtCoordinate(width * 0.9, 10).pos;
    assert.ok(
      left > right,
      `right-to-left: leftmost hit ${left} should be later than rightmost ${right}`,
    );
  });

  test("a selection spanning a direction change is more than one rect", () => {
    // The case a naive implementation gets wrong. A range crossing from a
    // left-to-right run into a right-to-left one is not contiguous on screen,
    // so it cannot be described by a single rectangle -- and the pieces carry
    // the direction they came from.
    let paragraph = laidOut(
        { textStyle: { fontSize: 20 } },
        "abc שלום def",
        1000,
      ),
      rects = paragraph.getRectsForRange(2, 7);

    assert.ok(
      rects.length >= 2,
      `a bidi selection needs a rect per run, got ${rects.length}`,
    );
    let directions = new Set(rects.map((r) => r.direction));
    assert.equal(
      directions.size,
      2,
      "the pieces should not all report the same direction",
    );
  });

  test("a hit never lands between the halves of a surrogate pair", () => {
    // A family emoji is one grapheme cluster built from three code points
    // joined by zero-width joiners -- eight UTF-16 units. Selecting the
    // cluster covers it once.
    //
    // Where a hit inside it lands is a property of the font, not of
    // segmentation. `getGlyphPositionAtCoordinate` reports the boundary of a
    // shaped glyph cluster, and font fallback resolves the emoji and the
    // joiners separately whenever no single font covers the whole sequence --
    // so the sequence ligates to one glyph where it is covered and breaks at
    // each joiner where it is not. Both are code point boundaries and both
    // are correct. What has to hold under every font is that the position is
    // a code point boundary at all: a caret between the halves of a surrogate
    // pair indexes half a character.
    let text = "A\u{1F468}\u200D\u{1F469}\u200D\u{1F467}B",
      paragraph = laidOut({ textStyle: { fontSize: 20 } }, text, 1000),
      end = text.length - 1; // everything but the trailing "B"

    let rects = paragraph.getRectsForRange(1, end);
    assert.equal(rects.length, 1, "one cluster, one rect");

    // The offset one unit into each two-unit code point.
    let split = new Set(),
      at = 0;
    for (let ch of text) {
      if (ch.length === 2) split.add(at + 1);
      at += ch.length;
    }

    let inside = paragraph.getGlyphPositionAtCoordinate(
      (rects[0].rect[0] + rects[0].rect[2]) / 2,
      10,
    ).pos;
    assert.ok(
      inside >= 1 && inside <= end && !split.has(inside),
      `a hit inside the cluster landed at ${inside}, ${
        split.has(inside)
          ? "between the halves of a surrogate pair"
          : "outside the cluster"
      }`,
    );
  });

  test("reports line metrics per line", () => {
    let paragraph = laidOut({ textStyle: { fontSize: 16 } }),
      metrics = paragraph.getLineMetrics();
    assert.equal(metrics.length, paragraph.getNumberOfLines());
    assert.ok(metrics.every((m) => m.height > 0));
  });
});

describe("TextDecoration", () => {
  const { Canvas, TextDecoration, TextDecorationStyle } = require("../../lib");
  const { fontTables } = require("../support/font-tables.js");
  const OSWALD = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

  // A paragraph in 96px Oswald drawn at four times the size, for its
  // pixels: SCALE device pixels to each of the paragraph's, so a line's
  // edges read to a fraction of one.
  const SCALE = 4,
    drawn = (pieces, { w = 700, h = 260 } = {}) => {
      FontLibrary.use("DecorParagraphOswald", [OSWALD]);
      const base = {
          fontFamilies: ["DecorParagraphOswald"],
          fontSize: 96,
          color: "black",
        },
        pb = ParagraphBuilder.Make({ textStyle: base });
      for (const [text, style] of pieces) {
        pb.pushStyle({ ...base, ...style });
        pb.addText(text);
        pb.pop();
      }
      const p = pb.build();
      p.layout(10000);
      const ctx = new Canvas(w * SCALE, h * SCALE).getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, w * SCALE, h * SCALE);
      ctx.scale(SCALE, SCALE);
      ctx.drawParagraph(p, 20, 20);
      return {
        p,
        data: ctx.getImageData(0, 0, w * SCALE, h * SCALE).data,
        width: w * SCALE,
        height: h * SCALE,
      };
    };

  // The rows of the column at user `x` that are not white, as [top, bottom]
  // spans in user pixels, a partly covered row counting for its part.
  const columnSpans = ({ data, width, height }, x, channel = 0) => {
    const col = Math.floor(x * SCALE),
      cover = (y) => (255 - data[(y * width + col) * 4 + channel]) / 255,
      out = [];
    let start = null;
    for (let y = 0; y <= height; y++) {
      const c = y < height ? cover(y) : 0;
      if (c > 0 && start === null) start = y;
      if (c === 0 && start !== null) {
        out.push([
          (start + 1 - cover(start)) / SCALE,
          (y - 1 + cover(y - 1)) / SCALE,
        ]);
        start = null;
      }
    }
    return out;
  };

  // The underline's place is the font's: the top of the line where the
  // `post` table puts it, below the baseline, and its thickness. The
  // baseline is read from the `H` beside it, whose flat foot stands on it.
  test("a paragraph's underline hangs from the post table's top", () => {
    const m = fontTables(OSWALD).at(96),
      { p, ...image } = drawn([
        ["H      H", { decoration: TextDecoration.Underline }],
      ]);
    // The `H`'s stem, the tallest span of ink in its first 40 pixels, and
    // the middle of the spaces.
    let stem = [0, 0];
    for (let x = 20; x < 60; x += 1 / SCALE) {
      const [first] = columnSpans(image, x);
      if (first && first[1] - first[0] > stem[1] - stem[0]) stem = first;
    }
    const baseline = stem[1],
      gap = columnSpans(image, 20 + p.getMaxIntrinsicWidth() / 2);
    assert.equal(gap.length, 1, JSON.stringify(gap));
    const [top, bottom] = gap[0];
    assert.ok(
      Math.abs(top - baseline + m.underlineTop) <= 0.1,
      `top ${(top - baseline).toFixed(3)} below the baseline, the font says ${(-m.underlineTop).toFixed(3)}`,
    );
    assert.ok(
      Math.abs(bottom - top - m.underlineThickness) <= 0.1,
      `thickness ${(bottom - top).toFixed(3)}, the font says ${m.underlineThickness.toFixed(3)}`,
    );
  });

  // CSS Text Decoration 4 section 5.1: an underline goes beneath the text.
  test("a paragraph's underline is drawn beneath its glyph", () => {
    const m = fontTables(OSWALD).at(96),
      { p, ...image } = drawn([
        [
          "p",
          {
            decoration: TextDecoration.Underline,
            decorationColor: "red",
            decorationThickness: 4,
          },
        ],
      ]);
    // Across the underline's band, below the baseline, where the
    // descender's stem crosses it: black, the glyph's, rather than red.
    const baseline = 20 + p.getAlphabeticBaseline(),
      row = Math.floor((baseline - m.underlineTop + 4) * SCALE),
      { data, width } = image;
    let black = 0,
      red = 0;
    for (let x = 0; x < width; x++) {
      const i = (row * width + x) * 4;
      if (data[i] < 40 && data[i + 1] < 40) black++;
      if (data[i] > 215 && data[i + 1] < 40) red++;
    }
    assert.ok(red > 0, "the underline is drawn");
    assert.ok(black > 0, "the descender shows over the underline");
  });

  // CSS Text Decoration 4, text-shadow: "Each layer shadows the element's
  // text and all its text decorations". A shadow far below its spaces,
  // which have no ink, is the underline's alone.
  test("a paragraph's text shadow shadows its underline", () => {
    const m = fontTables(OSWALD).at(96),
      DROP = 60,
      { p, ...image } = drawn([
        [
          "H      H",
          {
            decoration: TextDecoration.Underline,
            shadows: [{ color: "blue", offset: [0, DROP], blurRadius: 0 }],
          },
        ],
      ]);
    const baseline = 20 + p.getAlphabeticBaseline(),
      x = 20 + p.getMaxIntrinsicWidth() / 2,
      blue = columnSpans(image, x, 0).filter(
        ([top]) => top > baseline + DROP - m.underlineTop - 1,
      );
    assert.equal(
      blue.length,
      1,
      `the underline's shadow: ${JSON.stringify(blue)}`,
    );
  });

  // The row through the middle of the underline's band, in device pixels.
  const underlineRow = (p) => {
    const m = fontTables(OSWALD).at(96);
    return Math.floor(
      (20 +
        p.getAlphabeticBaseline() -
        m.underlineTop +
        m.underlineThickness / 2) *
        SCALE,
    );
  };

  // The user-space x of the first and last red pixel in device row `row`.
  const redReach = ({ data, width }, row) => {
    let first = null,
      last = null;
    for (let x = 0; x < width; x++) {
      const i = (row * width + x) * 4;
      if (data[i] > 215 && data[i + 1] < 40 && data[i + 2] < 40) {
        first ??= x;
        last = x;
      }
    }
    return first === null ? null : [first / SCALE, (last + 1) / SCALE];
  };

  // SkParagraph shapes two styles in one font as one run, so a run can hold
  // decorated text and plain text both; the line follows the text.
  test("a paragraph's underline starts and stops where its style does", () => {
    const red = {
        decoration: TextDecoration.Underline,
        decorationColor: "red",
      },
      width = (text) => drawn([[text, {}]]).p.getMaxIntrinsicWidth(),
      plain = width("H    "),
      lined = width("    H");
    for (const [pieces, from, to] of [
      [
        [
          ["H    ", {}],
          ["    H", red],
        ],
        20 + plain,
        20 + plain + lined,
      ],
      [
        [
          ["H    ", red],
          ["    H", {}],
        ],
        20,
        20 + plain,
      ],
    ]) {
      const { p, ...image } = drawn(pieces),
        reach = redReach(image, underlineRow(p));
      assert.ok(reach, `${JSON.stringify(pieces)}: an underline is drawn`);
      assert.ok(
        Math.abs(reach[0] - from) <= 1 && Math.abs(reach[1] - to) <= 1,
        `${JSON.stringify(pieces)}: from ${reach[0]} to ${reach[1]}, the text from ${from.toFixed(2)} to ${to.toFixed(2)}`,
      );
    }
  });

  // CSS 2 Appendix E paints a box's background before its decorations, so
  // the background does not cover the underline.
  test("a paragraph's underline is drawn over its background", () => {
    const { p, ...image } = drawn([
        [
          "H      H",
          {
            decoration: TextDecoration.Underline,
            decorationColor: "red",
            backgroundColor: "yellow",
          },
        ],
      ]),
      row = underlineRow(p),
      x = Math.floor((20 + p.getMaxIntrinsicWidth() / 2) * SCALE),
      i = (row * image.width + x) * 4;
    assert.deepEqual(
      [...image.data.slice(i, i + 3)],
      [255, 0, 0],
      "the underline, red, over the yellow",
    );
  });

  // A text shadow goes over its box's background, as SkParagraph draws it,
  // upright or sheared, with or without a decoration beside it.
  test("a paragraph's shadow is drawn over its background", () => {
    for (const [name, extra] of [
      ["plain", {}],
      ["underlined", { decoration: TextDecoration.Underline }],
      ["oblique", { fontStyle: { slant: 2 } }],
    ]) {
      const { p, ...image } = drawn([
          [
            "H      H",
            {
              ...extra,
              backgroundColor: "yellow",
              shadows: [{ color: "red", offset: [12, 0], blurRadius: 0 }],
            },
          ],
        ]),
        row = Math.floor((20 + p.getAlphabeticBaseline() - 30) * SCALE),
        // The first `H`'s shadow, inside the box; the last one's falls
        // past the background's end.
        reach = redReach(image, row);
      assert.ok(
        reach && reach[0] < 20 + p.getMaxIntrinsicWidth() / 2,
        `${name}: the shadow shows over the yellow, ${JSON.stringify(reach)}`,
      );
    }
  });

  test("exposes frozen constants with the expected bit values", () => {
    assert.deepEqual(
      { ...TextDecoration },
      {
        NoDecoration: 0x0,
        Underline: 0x1,
        Overline: 0x2,
        LineThrough: 0x4,
      },
    );
    assert.deepEqual(
      { ...TextDecorationStyle },
      {
        Solid: 0,
        Double: 1,
        Dotted: 2,
        Dashed: 3,
        Wavy: 4,
      },
    );
    assert.ok(Object.isFrozen(TextDecoration));
    assert.ok(Object.isFrozen(TextDecorationStyle));
    // Bit flags, so they must be combinable without collision.
    assert.equal(TextDecoration.Underline | TextDecoration.LineThrough, 0x5);
  });

  // Ink count, rather than exact pixels: a decoration adds a rule to the glyphs,
  // so it can only add coverage. Which pixels it lands on is Skia's business.
  const inkFor = (decoration) => {
    let canvas = new Canvas(200, 60);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, 200, 60);

    let pb = ParagraphBuilder.Make({
      textStyle: {
        fontSize: 24,
        color: "black",
        decoration,
        decorationStyle: TextDecorationStyle.Solid,
        decorationThickness: 2,
      },
    });
    pb.addText("Slug");
    let paragraph = pb.build();
    paragraph.layout(200);
    ctx.drawParagraph(paragraph, 10, 10);

    let { data } = ctx.getImageData(0, 0, 200, 60),
      inked = 0;
    // Any non-white pixel, not just dark ones: a coloured decoration still counts.
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] < 250 || data[i + 1] < 250 || data[i + 2] < 250) inked++;
    }
    return inked;
  };

  test("a decoration defaults to the text color", () => {
    // Skia defaults the decoration color to transparent, so an underline with no
    // explicit decorationColor used to draw nothing at all.
    let plain = inkFor(TextDecoration.NoDecoration),
      underlined = inkFor(TextDecoration.Underline);
    assert.ok(
      underlined > plain,
      `underline drew nothing: ${underlined} vs ${plain} inked pixels`,
    );
  });

  test("underline and line-through add ink to the glyphs", () => {
    let plain = inkFor(TextDecoration.NoDecoration);
    assert.ok(plain > 0, "the text itself should render");
    assert.ok(
      inkFor(TextDecoration.Underline) > plain,
      "underline should add ink",
    );
    assert.ok(
      inkFor(TextDecoration.LineThrough) > plain,
      "line-through should add ink",
    );
    assert.ok(
      inkFor(TextDecoration.Underline | TextDecoration.LineThrough) >
        inkFor(TextDecoration.Underline),
      "combining two decorations should add more ink than one",
    );
  });
});

// `align` and `baseline` were read and discarded, so a placeholder always sat
// on the baseline no matter what was asked for.
describe("addPlaceholder", () => {
  const { PlaceholderAlignment, TextBaseline } = require("../../lib");

  // A line taller than the placeholder, so the alignments cannot coincide
  // simply because the placeholder sets the line height.
  function topEdge(align) {
    let builder = new ParagraphBuilder({ textStyle: { fontSize: 72 } });
    builder.addText("Ag");
    builder.addPlaceholder(16, 16, align, TextBaseline.Alphabetic, 0);
    builder.addText("Ag");

    let paragraph = builder.build();
    paragraph.layout(600);

    let placed = paragraph.getRectsForPlaceholders()[0];
    return Math.round((placed.rect || placed)[1]);
  }

  test("align moves the placeholder", () => {
    let positions = Object.values(PlaceholderAlignment).map(topEdge);

    // Baseline and BelowBaseline coincide at offset 0 -- the placeholder's
    // baseline is its top edge there -- so five distinct positions out of six
    // is the correct answer, not four or one.
    assert.equal(
      new Set(positions).size,
      5,
      `expected the alignments to differ, got ${positions.join()}`,
    );
    assert.ok(
      topEdge(PlaceholderAlignment.Top) < topEdge(PlaceholderAlignment.Middle),
      "Top should sit above Middle",
    );
    assert.ok(
      topEdge(PlaceholderAlignment.Middle) <
        topEdge(PlaceholderAlignment.Bottom),
      "Middle should sit above Bottom",
    );
  });

  test("a value outside either set throws", () => {
    let builder = () => new ParagraphBuilder({});

    assert.throws(() => builder().addPlaceholder(10, 10, 9), TypeError);
    assert.throws(() => builder().addPlaceholder(10, 10, 0, 7), TypeError);
  });

  test("omitting them still lays out on the baseline", () => {
    assert.equal(topEdge(undefined), topEdge(PlaceholderAlignment.Baseline));
  });
});

describe("paragraph shadows", () => {
  const { Canvas } = require("../../lib");

  const W = 420,
    H = 160,
    SIZE = 64,
    FAMILY = "Helvetica",
    // Far enough that the shadow clears the glyph entirely, so what is
    // measured below is the shadow alone rather than the two overlapping.
    AWAY = 200;

  // The horizontal reach of a black shadow cast by white text on white: the
  // glyph itself leaves no ink, so every non-white pixel belongs to the
  // shadow. Width rather than position, because the two paths below place
  // their baselines differently and only the spread is being compared --
  // same glyph at the same size, so the difference between two widths is
  // twice the blur's reach.
  const shadowWidth = (paint) => {
    let canvas = new Canvas(W, H);
    canvas.gpu = false;
    let ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, W, H);
    paint(ctx);

    let { data } = ctx.getImageData(0, 0, W, H),
      left = W,
      right = -1;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        // Anything off pure white is shadow. A wide threshold would clip the
        // faint tail, which is exactly the part that grows with sigma.
        if (data[(y * W + x) * 4] < 250) {
          if (x < left) left = x;
          if (x > right) right = x;
        }
      }
    }
    return right < 0 ? 0 : right - left + 1;
  };

  const viaContext = (blur) => (ctx) => {
    ctx.font = `${SIZE}px ${FAMILY}`;
    ctx.fillStyle = "white";
    ctx.shadowColor = "black";
    ctx.shadowBlur = blur;
    ctx.shadowOffsetX = AWAY;
    ctx.fillText("M", 10, 100);
  };

  const viaParagraph = (blur) => (ctx) => {
    let pb = ParagraphBuilder.Make({
      textStyle: {
        fontSize: SIZE,
        fontFamilies: [FAMILY],
        color: "white",
        shadows: [{ color: "black", offset: [AWAY, 0], blurRadius: blur }],
      },
    });
    pb.addText("M");
    let paragraph = pb.build();
    paragraph.layout(W);
    ctx.drawParagraph(paragraph, 10, 20);
  };

  test("blurRadius means what shadowBlur means", () => {
    // The two paths reach Skia's `sigma` by different routes -- the context
    // halves the radius per the CSS definition, and the paragraph binding did
    // not -- so the same number blurred twice as far on a paragraph. Nothing
    // caught it because neither side had ever been measured against the
    // other, and either alone looks like a shadow.
    for (const blur of [12, 24]) {
      let fromContext = shadowWidth(viaContext(blur)),
        fromParagraph = shadowWidth(viaParagraph(blur));
      assert.ok(fromContext > 0 && fromParagraph > 0, "both should cast one");
      assert.ok(
        Math.abs(fromContext - fromParagraph) <= 3,
        `blurRadius ${blur} spread ${fromParagraph}px against ` +
          `shadowBlur ${blur}'s ${fromContext}px`,
      );
    }
  });

  test("the measurement can tell two blurs apart", () => {
    // Guards the test above rather than the code: a comparison that cannot
    // distinguish 12 from 24 would pass whatever the binding did with it.
    assert.ok(
      shadowWidth(viaParagraph(24)) > shadowWidth(viaParagraph(12)) + 5,
      "a wider blur should visibly spread further",
    );
  });
});

describe("the constants and keys JS was missing", () => {
  const { RectHeightStyle, RectWidthStyle } = require("../../lib");

  test("the rect styles are exported like the other text constants", () => {
    // `getRectsForRange` took bare integers while TextDecoration,
    // TextDecorationStyle, PlaceholderAlignment and TextBaseline were all
    // exported by name, so these two were the ones outside the pattern.
    assert.equal(typeof RectHeightStyle, "object");
    assert.equal(typeof RectWidthStyle, "object");
    assert.equal(RectHeightStyle.Tight, 0);
    assert.equal(RectHeightStyle.Strut, 5);
    assert.equal(RectWidthStyle.Max, 1);
    // Frozen, like the four beside them: a caller mutating a shared constant
    // would change it for everyone in the process.
    assert.ok(Object.isFrozen(RectHeightStyle));
    assert.ok(Object.isFrozen(RectWidthStyle));
  });

  test("the height styles are not all the same rectangle", () => {
    // The values have to reach Skia, not merely exist. Tight covers the
    // glyphs and Max the line box, so they only differ when the line is
    // taller than what is drawn on it -- `heightMultiplier` is what makes
    // that true. Without it every style measured 43.72 and this compared
    // nothing, which is how the first version of this test passed while
    // proving no more than that the numbers were accepted.
    let paragraph = laidOut(
      { textStyle: { fontSize: 16, heightMultiplier: 3 } },
      PROSE,
      120,
    );
    let tight = paragraph.getRectsForRange(0, 20, RectHeightStyle.Tight),
      max = paragraph.getRectsForRange(0, 20, RectHeightStyle.Max);
    assert.ok(tight.length > 0 && max.length > 0, "both should return boxes");

    const height = (boxes) =>
      boxes.reduce((sum, b) => sum + (b.rect[3] - b.rect[1]), 0);
    assert.ok(
      height(max) > height(tight),
      `Max (${height(max)}) should be taller than Tight (${height(tight)})`,
    );
  });
});

describe("baselineShift", () => {
  const SHIFT = 25;

  // The shift has to be measured against a run that did not move. A
  // paragraph whose every run carries the same shift renders
  // pixel-identically to one with none: Skia shifts the glyphs and the
  // paragraph's own alphabetic baseline together, and the two cancel. So a
  // single-run test here passes whether or not the key is wired to
  // anything.
  //
  // Spaces around the shifted glyph because the bands below are split on
  // blank columns, and in a face whose "x2x" glyphs touch there is no blank
  // column to split on -- the three bands come back as two and the
  // comparison silently reads the wrong pair. Helvetica and Times both do
  // this without the spaces.
  const tops = (shift) => {
    const canvas = new Canvas(400, 260),
      ctx = canvas.getContext("2d"),
      base = { fontSize: 48, color: "black" };
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, 400, 260);

    const builder = new ParagraphBuilder({ textStyle: base });
    builder.pushStyle(base);
    builder.addText("x   ");
    builder.pop();
    builder.pushStyle(
      shift === null ? base : { ...base, baselineShift: shift },
    );
    builder.addText("2");
    builder.pop();
    builder.pushStyle(base);
    builder.addText("   x");

    const paragraph = builder.build();
    paragraph.layout(380);
    ctx.drawParagraph(paragraph, 10, 150);

    // Top inked row per glyph, split into bands by the blank columns
    // between them, so the middle glyph can be read on its own.
    const data = ctx.getImageData(0, 0, 400, 260).data,
      inked = (x, y) => data[(y * 400 + x) * 4] < 128,
      columns = [];
    for (let x = 0; x < 400; x++)
      for (let y = 0; y < 260; y++)
        if (inked(x, y)) {
          columns.push(x);
          break;
        }

    const groups = [[columns[0]]];
    for (let i = 1; i < columns.length; i++) {
      if (columns[i] - columns[i - 1] > 2) groups.push([]);
      groups.at(-1).push(columns[i]);
    }

    const rows = groups.map((group) => {
      for (let y = 0; y < 260; y++)
        for (const x of group) if (inked(x, y)) return y;
      return null;
    });

    assert.equal(
      rows.length,
      3,
      "the three glyphs must land in three bands, or the rows below are not " +
        "the glyphs they are named after",
    );

    // The digit's own row against its unshifted neighbour's, in the same
    // image. Absolute rows are a property of the face -- this pair sits 12
    // apart under Core Text and 3 apart in Georgia -- and of the line box,
    // which grows when a run is lifted. Their difference is neither.
    return rows[1] - rows[0];
  };

  test("moves one run off the baseline its neighbours keep", () => {
    const plain = tops(null);

    assert.equal(
      tops(-SHIFT) - plain,
      -SHIFT,
      "a negative shift lifts the run that far above its neighbours",
    );
    assert.equal(
      tops(SHIFT) - plain,
      SHIFT,
      "a positive shift drops it that far below them",
    );
  });
});

describe("the numeric style codes", () => {
  test("refuse a code outside the set rather than defaulting", () => {
    // These used to end in `_ => Solid`, `_ => All`, `_ => Tight` -- a code
    // outside the set silently became the default, so a caller reading a
    // constant off the wrong object got the default style and no reason. The
    // enums they parse into are now the crate's own, which is what makes the
    // set closed: a new variant is a compile error rather than another value
    // quietly folded into the catch-all.
    assert.throws(
      () => ParagraphBuilder.Make({ textStyle: { decorationStyle: 9 } }),
      /decorationStyle 9/,
      "an unknown decoration style is refused and named",
    );
    assert.throws(
      () => ParagraphBuilder.Make({ textHeightBehavior: 9 }),
      /textHeightBehavior 9/,
      "an unknown height behaviour likewise",
    );

    const builder = ParagraphBuilder.Make({ textStyle: { fontSize: 16 } });
    builder.addText("hi");
    const paragraph = builder.build();
    paragraph.layout(100);
    assert.throws(
      () => paragraph.getRectsForRange(0, 2, 9, 0),
      /rect height style 9/,
      "and an unknown rect height style, at a different entry point",
    );

    // The valid codes are unaffected, including the zero each catch-all used
    // to stand in for -- which is the arm a refusal could most easily have
    // swallowed.
    assert.ok(ParagraphBuilder.Make({ textStyle: { decorationStyle: 0 } }));
    assert.ok(ParagraphBuilder.Make({ textHeightBehavior: 0 }));
    assert.ok(paragraph.getRectsForRange(0, 2, 0, 0));
  });
});

describe("ParagraphBuilder optical size", () => {
  // Chrome 152 widths of `Hamburgefonstiv` in Amstelvar, whose `opsz` axis
  // runs 10 to 72: measureText with `opsz` left to follow the font size,
  // and a DOM span with `"opsz" 30` (in 1/64 px layout units).
  const AT_SIZE = { 12: 100.7189, 48: 352.2238 },
    AT_OPSZ_30 = { 12: 93.015625, 48: 372.0625 },
    FAMILY = "AmstelvarOptical",
    // FreeType interpolates the advances up to 0.03 from where CoreText and
    // Chrome on macOS put them; one step along the axis moves the width at
    // least 0.32.
    TOLERANCE = 0.05;

  const widthOf = (base, span) => {
    FontLibrary.use(FAMILY, "tests/assets/fonts/AmstelvarAlpha-VF.ttf");
    try {
      const builder = ParagraphBuilder.Make({
        textStyle: { fontFamilies: [FAMILY], ...base },
      });
      if (span) builder.pushStyle({ fontFamilies: [FAMILY], ...span });
      builder.addText("Hamburgefonstiv");
      const paragraph = builder.build();
      paragraph.layout(10000);
      return paragraph.getMaxIntrinsicWidth();
    } finally {
      FontLibrary.reset();
    }
  };

  const near = (width, expected, what) =>
    assert.ok(
      Math.abs(width - expected) < TOLERANCE,
      `${what} measured ${width} against Chrome's ${expected}`,
    );

  test("instances opsz at each style's own font size", () => {
    near(widthOf({ fontSize: 48 }), AT_SIZE[48], "a 48px paragraph");
    near(
      widthOf({ fontSize: 12 }, { fontSize: 48 }),
      AT_SIZE[48],
      "a 48px span in a 12px paragraph",
    );
    near(
      widthOf({ fontSize: 48 }, { fontSize: 12 }),
      AT_SIZE[12],
      "a 12px span in a 48px paragraph",
    );
  });

  test("lets an explicit opsz win over the font size", () => {
    const opsz30 = { fontVariations: [{ axis: "opsz", value: 30 }] };
    near(
      widthOf({ fontSize: 48, ...opsz30 }),
      AT_OPSZ_30[48],
      'a 48px paragraph at "opsz" 30',
    );
    near(
      widthOf({ fontSize: 48, ...opsz30 }, { fontSize: 12 }),
      AT_OPSZ_30[12],
      'a 12px span in a 48px paragraph at "opsz" 30',
    );
  });
});

describe("ParagraphBuilder span axes", () => {
  // A pushed style's `fontVariations` replace the paragraph's, as a nested
  // `font-variation-settings` replaces its parent's in CSS and as the
  // crate's `ParagraphBuilder::push_style` does; a style with none of its
  // own, or an empty list, takes the paragraph's, and one set to `"normal"`
  // has none, as `font-variation-settings: normal` does. Its weight supplies
  // `wght` unless the list in force names it. Measured against a paragraph built in the span's style
  // outright, which instances its face through the same library.
  const FAMILY = "AmstelvarSpanAxes",
    TEXT = "Hamburgefonstiv",
    wdth50 = [{ axis: "wdth", value: 50 }],
    wght700 = [{ axis: "wght", value: 700 }],
    wght300 = [{ axis: "wght", value: 300 }];

  const style = (weight, fontVariations) => ({
    fontFamilies: [FAMILY],
    fontSize: 48,
    fontStyle: { weight },
    fontVariations,
  });

  const widthOf = (base, span) => {
    FontLibrary.use(FAMILY, "tests/assets/fonts/AmstelvarAlpha-VF.ttf");
    try {
      const builder = ParagraphBuilder.Make({ textStyle: base });
      if (span) builder.pushStyle(span);
      builder.addText(TEXT);
      const paragraph = builder.build();
      paragraph.layout(10000);
      return paragraph.getMaxIntrinsicWidth();
    } finally {
      FontLibrary.reset();
    }
  };

  test("lays a span out at its own axes", () => {
    assert.ok(
      Math.abs(
        widthOf(style(400, wght700)) -
          widthOf(style(400, [...wdth50, ...wght700])),
      ) > 1,
      "wdth 50 has to move the width for the first case to mean anything",
    );
    const cases = [
      [
        "a span's wght 700 under a paragraph's wdth 50",
        style(400, wdth50),
        style(400, wght700),
        style(400, wght700),
      ],
      [
        "a span's weight 700 and wdth 50 under a paragraph's wght 300",
        style(400, wght300),
        style(700, wdth50),
        style(700, wdth50),
      ],
      [
        "a span with no axes under a paragraph's wdth 50",
        style(400, wdth50),
        style(400, undefined),
        style(400, wdth50),
      ],
      [
        "a span with an empty list under a paragraph's wdth 50",
        style(400, wdth50),
        style(400, []),
        style(400, wdth50),
      ],
      [
        "a span set to normal under a paragraph's wdth 50",
        style(400, wdth50),
        style(400, "normal"),
        style(400, undefined),
      ],
      [
        "a span set to NORMAL, in any case, under a paragraph's wdth 50",
        style(400, wdth50),
        style(400, "NORMAL"),
        style(400, undefined),
      ],
      [
        "a paragraph set to normal",
        style(400, "normal"),
        undefined,
        style(400, undefined),
      ],
    ];
    for (const [what, base, span, alone] of cases) {
      const pushed = widthOf(base, span),
        expected = widthOf(alone);
      assert.ok(
        Math.abs(pushed - expected) < 1e-3,
        `${what} measured ${pushed} against ${expected}`,
      );
    }
  });

  test("refuses a keyword other than normal", () => {
    assert.throws(
      () => widthOf(style(400, wdth50), style(400, "none")),
      (error) =>
        error instanceof TypeError &&
        /"normal" or a list/.test(error.message) &&
        /"none"/.test(error.message),
    );
  });
});

describe("an oblique range in a Paragraph draws as canvas `oblique` text does", () => {
  // CSS Fonts 4's rule, the one a canvas follows: a family's face that
  // leans as asked, and otherwise its upright face sheared by tan(14deg),
  // before any italic. So a range asking for oblique on Raleway, which has
  // an italic, is sheared as one on Oswald, which has none, and draws
  // exactly what canvas text in `oblique` draws.
  // Registered by each test rather than once here: tests above call
  // `FontLibrary.reset()`, and a family gone by the time a test runs would
  // leave both sides of each comparison in the same fallback face.
  const R = "tests/assets/fonts/Recursive";
  const register = () => {
    FontLibrary.use("ParagraphOswald", [
      "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
    ]);
    FontLibrary.use("ParagraphRaleway", [
      "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
      "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf",
    ]);
    // And the two Recursive fixtures: one with a `slnt` axis, set to -14,
    // and one whose oblique face is drawn as it is -- see the README beside
    // them.
    FontLibrary.use("ParagraphRecursiveSlnt", [
      `${R}/Recursive-slnt-subset.ttf`,
    ]);
    FontLibrary.use("ParagraphRecursiveStatic", [
      `${R}/Recursive-upright-subset.ttf`,
      `${R}/Recursive-oblique12-subset.ttf`,
    ]);
    for (const family of [...FAMILIES, ...LEANING])
      assert.ok(FontLibrary.has(family), `${family} is registered`);
  };
  const FAMILIES = ["ParagraphOswald", "ParagraphRaleway"],
    LEANING = ["ParagraphRecursiveSlnt", "ParagraphRecursiveStatic"],
    W = 400,
    H = 300,
    UPRIGHT = 0,
    ITALIC = 1,
    OBLIQUE = 2,
    TAN_14 = Math.tan((14 * Math.PI) / 180);

  // A paragraph of `pieces`, each `[text, style]`, drawn with its first
  // baseline at y = 250, the baseline the canvas text below is drawn on.
  const paragraph = (family, pieces, width = 1000) => {
    const base = { fontFamilies: [family], fontSize: 200, color: "black" },
      pb = ParagraphBuilder.Make({ textStyle: base });
    for (const [text, style] of pieces) {
      pb.pushStyle({ ...base, ...style });
      pb.addText(text);
      pb.pop();
    }
    const p = pb.build();
    p.layout(width);
    const ctx = new Canvas(W, H * 2).getContext("2d");
    ctx.drawParagraph(p, 60, 250 - p.getAlphabeticBaseline());
    const data = ctx.getImageData(0, 0, W, H * 2).data;
    data.lines = p.getNumberOfLines();
    return data;
  };
  const slanted = (slant) => ({ fontStyle: { slant } });
  const text = (font, words) => {
    const ctx = new Canvas(W, H * 2).getContext("2d");
    ctx.font = font;
    ctx.fillText(words, 60, 250);
    return ctx.getImageData(0, 0, W, H * 2).data;
  };
  const same = (a, b) => Buffer.from(a).equals(Buffer.from(b));
  // Rows `from` to `to` of a picture, for comparing one line of a paragraph.
  const rows = (data, from, to) =>
    Buffer.from(data.subarray(from * W * 4, to * W * 4));

  // The slope of the left edge of the first inked column of each row,
  // fitted over the middle seventy percent of the rows.
  const slope = (data) => {
    const edge = [];
    for (let y = 0; y < H; y++)
      for (let x = 0; x < W; x++)
        if (data[(y * W + x) * 4 + 3] >= 128) {
          edge.push([y, x]);
          break;
        }
    const inner = edge.slice(
        Math.floor(edge.length * 0.15),
        Math.floor(edge.length * 0.85),
      ),
      n = inner.length,
      my = inner.reduce((s, [y]) => s + y, 0) / n,
      mx = inner.reduce((s, [, x]) => s + x, 0) / n;
    let num = 0,
      den = 0;
    for (const [y, x] of inner) {
      num += (y - my) * (x - mx);
      den += (y - my) ** 2;
    }
    return num / den;
  };

  test("pixel for pixel, in a family with an italic and in one without", () => {
    register();
    for (const family of FAMILIES) {
      // The control: without it a scene that is not like for like would
      // make the comparison below mean nothing.
      assert.ok(
        same(
          paragraph(family, [["IH", slanted(UPRIGHT)]]),
          text(`200px ${family}`, "IH"),
        ),
        `${family}: an upright paragraph draws what upright text draws`,
      );
      const oblique = paragraph(family, [["IH", slanted(OBLIQUE)]]);
      assert.ok(
        same(oblique, text(`oblique 200px ${family}`, "IH")),
        `${family}: an oblique range draws what oblique text draws`,
      );
      assert.ok(
        !same(oblique, paragraph(family, [["IH", slanted(ITALIC)]])),
        `${family}: oblique is not the italic`,
      );
    }
  });

  test("a face that leans as asked is drawn as canvas text draws it", () => {
    register();
    for (const family of LEANING) {
      const oblique = paragraph(family, [["IH", slanted(OBLIQUE)]]);
      assert.ok(
        same(oblique, text(`oblique 200px ${family}`, "IH")),
        `${family}: an oblique range draws what oblique text draws`,
      );
      assert.ok(
        !same(oblique, paragraph(family, [["IH", slanted(UPRIGHT)]])),
        `${family}: and leans`,
      );
    }
  });

  test("sheared by tan(14deg)", () => {
    register();
    for (const family of FAMILIES) {
      const measured = slope(paragraph(family, [["I", slanted(OBLIQUE)]]));
      assert.ok(
        Math.abs(measured + TAN_14) <= 0.005,
        `${family}: slope ${measured}, expected ${-TAN_14}`,
      );
    }
  });

  test("a range is sheared on whichever line it lands", () => {
    register();
    // Two words in one font are one run, and the second wraps: its glyphs
    // are the run continued on a second line, the case SkParagraph reports
    // the wrong text offsets for. Each line has to be what the paragraph
    // all in the one slant draws there.
    const family = "ParagraphOswald",
      width = 130,
      mixed = paragraph(
        family,
        [
          ["II ", slanted(UPRIGHT)],
          ["II", slanted(OBLIQUE)],
        ],
        width,
      ),
      upright = paragraph(family, [["II II", slanted(UPRIGHT)]], width),
      oblique = paragraph(family, [["II II", slanted(OBLIQUE)]], width),
      second = H; // the second line's rows start below the first's
    for (const [name, data] of Object.entries({ mixed, upright, oblique }))
      assert.equal(data.lines, 2, `${name} wraps once`);
    assert.ok(
      !rows(upright, second, H * 2).equals(rows(oblique, second, H * 2)),
      "the control: the two slants differ on the second line",
    );
    assert.ok(
      rows(mixed, 0, second).equals(rows(upright, 0, second)),
      "the first line is upright",
    );
    assert.ok(
      rows(mixed, second, H * 2).equals(rows(oblique, second, H * 2)),
      "the second line is sheared",
    );
  });

  // A paragraph of `pieces`, each `[text, style]` over a shared base, laid
  // out wide and drawn at (20, 10) on white; the RGBA it leaves.
  const PLACEHOLDER = Symbol("placeholder");
  const onWhite = (family, size, pieces, w = W, h = H) => {
    const base = { fontFamilies: [family], fontSize: size, color: "black" },
      pb = ParagraphBuilder.Make({ textStyle: base });
    for (const piece of pieces) {
      // A placeholder box, 40 pixels square, where a piece says so.
      if (piece === PLACEHOLDER) {
        pb.addPlaceholder(40, 40);
        continue;
      }
      const [text, style] = piece;
      pb.pushStyle({ ...base, ...style });
      pb.addText(text);
      pb.pop();
    }
    const p = pb.build();
    p.layout(2000);
    const ctx = new Canvas(w, h).getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, w, h);
    ctx.drawParagraph(p, 20, 10);
    return ctx.getImageData(0, 0, w, h).data;
  };
  const ink = (data) => {
    let sum = 0;
    for (let i = 0; i < data.length; i += 4) sum += 255 - data[i + 1];
    return sum;
  };

  test("an oblique range breaks a ligature at its edge", () => {
    register();
    // An oblique is a change of font, and shaping breaks there: no
    // ligature forms across the edge, so the paragraph inks what its pieces
    // ink shaped on their own. Across it, "ffi" was formed and then either
    // clipped away by the transparent half or drawn twice.
    const family = "ParagraphRaleway",
      red = { color: "#c00" };
    for (const pieces of [
      [["ef"], ["fi", { ...slanted(OBLIQUE), ...red }], ["ffle"]],
      [["ef", { ...slanted(OBLIQUE), ...red }], ["fi"]],
    ]) {
      const whole = ink(
          onWhite(
            family,
            80,
            pieces.map(([t, st]) => [t, st ?? {}]),
            600,
            140,
          ),
        ),
        parts = pieces
          .map(([t, st]) => ink(onWhite(family, 80, [[t, st ?? {}]], 600, 140)))
          .reduce((a, b) => a + b, 0);
      const name = pieces.map(([t]) => t).join("|");
      assert.ok(parts > 0, "the control: the pieces ink");
      assert.ok(
        Math.abs(whole - parts) / parts < 0.03,
        `${name}: the paragraph inks ${whole}, its pieces ${parts}`,
      );
    }
  });

  test("a mark split from its base by an oblique edge stays on its base", () => {
    // A combining mark belongs to the grapheme of the base before it, and
    // the base's style takes the whole cluster: the mark is drawn with its
    // base, whichever side of the edge each was given.
    register();
    const family = "ParagraphRaleway",
      oblique = { ...slanted(OBLIQUE), color: "#c00" },
      ACUTE = "\u0301";
    for (const [split, joined] of [
      // The base upright, the mark given to the oblique range after it.
      [
        [["cafe"], [`${ACUTE} x`, oblique]],
        [[`cafe${ACUTE}`], [" x", oblique]],
      ],
      // The base oblique, the mark given to the upright range after it.
      [
        [["caf"], ["e", oblique], [`${ACUTE} x`]],
        [["caf"], [`e${ACUTE}`, oblique], [" x"]],
      ],
    ]) {
      const drawn = (pieces) =>
        Buffer.from(
          onWhite(
            family,
            80,
            pieces.map(([t, st]) => [t, st ?? {}]),
            600,
            140,
          ),
        );
      assert.ok(
        drawn(split).equals(drawn(joined)),
        `${split.map(([t]) => t).join("|")} draws as ${joined.map(([t]) => t).join("|")}`,
      );
    }
  });

  // Pixels by colour: red, blue and black ink, on white.
  const colours = (data) => {
    let red = 0,
      blue = 0,
      black = 0;
    for (let i = 0; i < data.length; i += 4) {
      const [r, g, b] = [data[i], data[i + 1], data[i + 2]];
      if (r > 150 && g < 80 && b < 80) red++;
      else if (b > 150 && r < 80 && g < 80) blue++;
      else if (r < 80 && g < 80 && b < 80) black++;
    }
    return { red, blue, black };
  };

  test("a mark after a placeholder is shaped alone, and moves nothing else", () => {
    // A placeholder stands in the text as U+FFFC, which a following mark
    // joins in one grapheme. There is no base to keep the mark with, so
    // it stays with its own text, shaped alone as SkParagraph shapes it --
    // and no text after it is lost or moved.
    register();
    const family = "ParagraphRaleway",
      ACUTE = "\u0301",
      red = { ...slanted(OBLIQUE), color: "red" },
      blue = { ...slanted(OBLIQUE), color: "blue" };
    const oblique = ink(
        onWhite(family, 60, [PLACEHOLDER, [`${ACUTE}x`, red]], 600, 140),
      ),
      upright = ink(
        onWhite(family, 60, [PLACEHOLDER, [`${ACUTE}x`, {}]], 600, 140),
      );
    assert.ok(upright > 0, "the control: the upright mark and x ink");
    assert.ok(
      Math.abs(oblique - upright) / upright < 0.15,
      `an oblique mark and x ink ${oblique}, upright ${upright}`,
    );
    // And with text before the placeholder, whose edge with the mark is
    // not an edge: the U+FFFC lies between them.
    const after = ink(
        onWhite(family, 60, [["a"], PLACEHOLDER, [`${ACUTE}x`, red]], 600, 140),
      ),
      afterUpright = ink(
        onWhite(family, 60, [["a"], PLACEHOLDER, [`${ACUTE}x`, {}]], 600, 140),
      );
    assert.ok(
      Math.abs(after - afterUpright) / afterUpright < 0.15,
      `after "a" and a placeholder: ${after}, upright ${afterUpright}`,
    );

    const withMark = colours(
        onWhite(
          family,
          60,
          [PLACEHOLDER, [`${ACUTE}HH`, red], [" HH ", {}], ["HHHH", blue]],
          600,
          140,
        ),
      ),
      without = colours(
        onWhite(
          family,
          60,
          [PLACEHOLDER, ["HH", red], [" HH ", {}], ["HHHH", blue]],
          600,
          140,
        ),
      );
    for (const colour of ["black", "blue"])
      assert.equal(
        withMark[colour],
        without[colour],
        `${colour} is what the same text inks without the mark`,
      );
    assert.ok(
      Math.abs(withMark.red - without.red) / without.red < 0.15,
      `red ${withMark.red} against ${without.red} without the mark`,
    );
  });

  test("an oblique range elsewhere does not restyle an upright mark", () => {
    // Edges move only where one side is oblique; a black "e" with a red
    // accent keeps its red accent whether or not text after it is oblique.
    register();
    const family = "ParagraphRaleway",
      ACUTE = "\u0301",
      pieces = (after) => [
        ["caf"],
        ["e"],
        [ACUTE, { color: "red" }],
        [" x", after],
      ];
    const plain = colours(onWhite(family, 80, pieces({}), 600, 140)),
      later = colours(onWhite(family, 80, pieces(slanted(OBLIQUE)), 600, 140));
    assert.ok(plain.red > 0, "the control: the accent is red");
    assert.equal(later.red, plain.red, "and stays red");
  });

  test("two oblique ranges in one run keep their own paints", () => {
    // Adjacent oblique ranges in one font are one run; each glyph has to
    // be matched to its own range for its colour. Each pixel is held to the
    // whole text drawn in the colour it came out in: FreeType adjusts a
    // glyph's edge coverage by its paint's luminance, so a blue edge is up
    // to 13 levels from a red one, where the same colour matches exactly.
    register();
    const family = "ParagraphOswald",
      oblique = slanted(OBLIQUE),
      inRed = onWhite(family, 100, [["IHIH", { ...oblique, color: "red" }]]),
      inBlue = onWhite(family, 100, [["IHIH", { ...oblique, color: "blue" }]]),
      two = onWhite(family, 100, [
        ["IH", { ...oblique, color: "red" }],
        ["IH", { ...oblique, color: "blue" }],
      ]);
    let red = 0,
      blue = 0,
      off = 0;
    for (let i = 0; i < two.length; i += 4) {
      if (inRed[i + 1] > 250 && two[i + 1] > 250) continue;
      const isRed = two[i] > two[i + 2],
        expected = isRed ? inRed : inBlue;
      if (isRed) red++;
      else blue++;
      if (two[i + 1] !== expected[i + 1]) off++;
    }
    assert.ok(red > 0 && blue > 0, `both paints: ${red} red, ${blue} blue`);
    assert.equal(off, 0, "every pixel is its own colour's");
  });

  test("a sheared range's shadow is drawn under every glyph", () => {
    register();
    // SkParagraph paints every shadow on a line before any glyph, and every
    // decoration after every glyph, whichever range each belongs to.
    const family = "ParagraphOswald";
    const uprightOnly = onWhite(family, 100, [
        ["HH"],
        ["HH", { ...slanted(OBLIQUE), color: "transparent" }],
      ]),
      shadowed = onWhite(family, 100, [
        ["HH"],
        [
          "HH",
          {
            ...slanted(OBLIQUE),
            color: "transparent",
            shadows: [{ color: "blue", offset: [-90, 0], blurRadius: 0 }],
          },
        ],
      ]);
    let glyph = 0,
      covered = 0;
    for (let i = 0; i < uprightOnly.length; i += 4)
      if (uprightOnly[i] < 10 && uprightOnly[i + 2] < 10) {
        glyph++;
        if (shadowed[i + 2] > 200 && shadowed[i] < 50) covered++;
      }
    assert.ok(glyph > 0, "the control: the upright glyphs are drawn");
    assert.equal(
      covered,
      0,
      "no upright glyph pixel is under the oblique shadow",
    );
  });

  // CSS Text Decoration 4 section 5.1, after CSS 2 Appendix E: underlines
  // and overlines go beneath the text, line-through over it. So a sheared
  // glyph covers an underline and is crossed by a line-through.
  test("an upright line-through is drawn over a sheared glyph", () => {
    register();
    const family = "ParagraphOswald";
    // An upright line-through beside an oblique glyph that leans into it.
    // The struck range ends in a transparent glyph, since a line's trailing
    // spaces are not struck.
    const LINE_THROUGH = 4,
      struck = [
        [
          "   I",
          {
            color: "transparent",
            decoration: LINE_THROUGH,
            decorationColor: "red",
            decorationThickness: 4,
          },
        ],
      ],
      glyphOnly = onWhite(family, 160, [
        ["I", slanted(OBLIQUE)],
        ["   I", { color: "transparent" }],
      ]),
      lineOnly = onWhite(family, 160, [
        ["I", { ...slanted(OBLIQUE), color: "transparent" }],
        ...struck,
      ]),
      both = onWhite(family, 160, [["I", slanted(OBLIQUE)], ...struck]);
    let crossing = 0,
      red = 0;
    for (let i = 0; i < both.length; i += 4)
      if (glyphOnly[i] < 10 && lineOnly[i] > 200 && lineOnly[i + 1] < 50) {
        crossing++;
        if (both[i] > 200 && both[i + 1] < 50) red++;
      }
    assert.ok(crossing > 0, "the control: the glyph leans across the line");
    assert.equal(red, crossing, "the line is drawn over the glyph");
  });

  test("an upright underline is drawn beneath a sheared glyph", () => {
    register();
    const family = "ParagraphOswald",
      UNDERLINE = 1;
    // A sheared, underlined `p`: below the baseline its descender crosses
    // the level line. Drawn three ways -- the glyph alone, the line alone,
    // both -- so the pixels where they cross are known.
    const line = {
        decoration: UNDERLINE,
        decorationColor: "red",
        decorationThickness: 6,
      },
      glyphOnly = onWhite(family, 160, [["p", slanted(OBLIQUE)]]),
      lineOnly = onWhite(family, 160, [
        ["p", { ...slanted(OBLIQUE), ...line, color: "transparent" }],
      ]),
      both = onWhite(family, 160, [["p", { ...slanted(OBLIQUE), ...line }]]);
    let crossing = 0,
      black = 0;
    for (let i = 0; i < both.length; i += 4)
      if (glyphOnly[i] < 10 && lineOnly[i] > 200 && lineOnly[i + 1] < 50) {
        crossing++;
        if (both[i] < 10 && both[i + 1] < 10) black++;
      }
    assert.ok(crossing > 0, "the control: the descender crosses the line");
    assert.equal(black, crossing, "the glyph is drawn over the line");
  });

  test("decorations stay level and shadows lean with their glyphs", () => {
    register();
    const family = "ParagraphOswald",
      UNDERLINE = 1;
    // Below the baseline, where an `I` has no ink, only the underline is.
    const under = (data) => rows(data, 252, H);
    const underlined = (slant) =>
      paragraph(family, [
        [
          "I",
          { ...slanted(slant), decoration: UNDERLINE, decorationColor: "red" },
        ],
      ]);
    assert.ok(
      under(underlined(UPRIGHT)).some((v) => v > 0),
      "the control: the underline is drawn",
    );
    assert.ok(
      under(underlined(OBLIQUE)).equals(under(underlined(UPRIGHT))),
      "an oblique range is underlined as an upright one is",
    );

    // The shadow alone, under a transparent glyph: it leans as its glyph
    // does, and an upright range's does not.
    const shadow = (slant) =>
      paragraph(family, [
        [
          "I",
          {
            ...slanted(slant),
            color: "transparent",
            shadows: [{ color: "blue", offset: [40, 0], blurRadius: 0 }],
          },
        ],
      ]);
    const leaning = slope(shadow(OBLIQUE)),
      standing = slope(shadow(UPRIGHT));
    assert.ok(
      Math.abs(leaning + TAN_14) <= 0.005,
      `an oblique range's shadow slopes ${leaning}, expected ${-TAN_14}`,
    );
    assert.ok(Math.abs(standing) <= 0.005, `an upright one ${standing}`);
  });
});
