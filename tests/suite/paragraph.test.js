// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  { ParagraphBuilder } = require("../../lib");

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

  test("a grapheme cluster is selected and hit-tested whole", () => {
    // A family emoji is one cluster built from three code points joined by
    // zero-width joiners -- eight UTF-16 units. Selecting it must cover it
    // once, and a hit inside it must land on a boundary rather than between
    // the joined parts, or a caret can be placed in the middle of a glyph.
    // Cluster boundaries come from ICU segmentation, so this holds whatever
    // font supplies the emoji.
    let text = "A\u{1F468}\u200D\u{1F469}\u200D\u{1F467}B",
      paragraph = laidOut({ textStyle: { fontSize: 20 } }, text, 1000),
      end = text.length - 1; // everything but the trailing "B"

    let rects = paragraph.getRectsForRange(1, end);
    assert.equal(rects.length, 1, "one cluster, one rect");

    let inside = paragraph.getGlyphPositionAtCoordinate(
      (rects[0].rect[0] + rects[0].rect[2]) / 2,
      10,
    ).pos;
    assert.ok(
      inside === 1 || inside === end,
      `a hit inside the cluster landed at ${inside}, between its joined parts`,
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
  const { Canvas, RectHeightStyle, RectWidthStyle } = require("../../lib");

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

  test("baselineShift is not offered, because Skia would ignore it", () => {
    // `TextStyle::baseline_shift` exists in Skia and this binding could set
    // it in one line, which is why it looked like a missing key. It is not
    // offered because it does nothing here: setting it through the paragraph
    // path moved neither the layout nor a drawn pixel at -40, 0, 40 or 120,
    // while `letterSpacing` through the same parser moved the box as
    // expected. The canvas surface only appears to honour it because
    // `Context2D` reads the field back and offsets the draw itself, in
    // `typography.rs` -- so the field is a carrier there, not an effect.
    //
    // This test records the measurement rather than the conclusion: if a
    // Skia bump starts applying it, this fails and the key becomes worth
    // adding.
    let boxOf = (style) => {
      let pb = ParagraphBuilder.Make({
        textStyle: Object.assign({ fontSize: 32, color: "black" }, style),
      });
      pb.addText("Hxy");
      let paragraph = pb.build();
      paragraph.layout(300);
      return paragraph.getRectsForRange(0, 3)[0].rect.join(",");
    };
    assert.equal(
      boxOf({ baselineShift: 40 }),
      boxOf({}),
      "Skia started honouring baselineShift -- offer the key now",
    );
  });
});
