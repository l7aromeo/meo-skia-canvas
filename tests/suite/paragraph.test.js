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
