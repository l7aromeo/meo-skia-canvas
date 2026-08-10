---
description: Assemble styled, wrappable text into a Paragraph
---

# ParagraphBuilder

> `ParagraphBuilder` assembles runs of differently-styled text and hands back a [`Paragraph`][paragraph] to lay out and draw. It exists because [`fillText()`][fillText] can only take one style for the whole string — a builder can push and pop styles mid-sentence, reserve space for inline graphics, and produce text that wraps to a measured width. 🧪

| Styles                     | Content                          | Output              |
| -------------------------- | -------------------------------- | ------------------- |
| [new ParagraphBuilder()][pb_new] | [addText()][pb_addtext]     | [build()][pb_build] |
| [ParagraphBuilder.Make()][pb_make] | [addPlaceholder()][pb_addplaceholder] |       |
| [pushStyle()][pb_pushstyle] |                                 |                     |
| [pop()][pb_pop]            |                                  |                     |

## Building a paragraph

Every method except [`build()`][pb_build] returns the builder, so a whole paragraph can be assembled in one expression:

```js
const paragraph = new ParagraphBuilder({ textStyle: { fontSize: 16 } })
  .addText("hello")
  .build();

paragraph.layout(320);
ctx.drawParagraph(paragraph, 20, 20);
```

Text is shaped with the process-global [`FontLibrary`][fontlibrary]. CanvasKit takes a `FontMgr` argument here; this build has no per-builder equivalent, so the parameter is omitted rather than accepted and ignored.

A builder is single-use. [`build()`][pb_build] consumes it, and calling `build()` a second time throws an `Error`.

---

## Methods

### `new ParagraphBuilder()`

```js returns="ParagraphBuilder"
new ParagraphBuilder(style);
```

```js returns="ParagraphBuilder"
ParagraphBuilder.Make(style);
```

The constructor and the static are equivalent; the static exists for CanvasKit parity. The optional `style` object sets defaults for the paragraph as a whole:

| Field                 | Meaning                                                                                                   |
| --------------------- | --------------------------------------------------------------------------------------------------------- |
| `textStyle`           | The base [text style](#pushstyle) every run inherits                                                       |
| `textAlign`           | `"left"`, `"right"`, `"center"`, `"justify"`, `"start"` or `"end"`                                          |
| `textDirection`       | `"ltr"` or `"rtl"`                                                                                          |
| `maxLines`            | Stop laying out after this many lines                                                                       |
| `ellipsis`            | String appended to the last line when `maxLines` cut the text short                                          |
| `strutStyle`          | A fixed line box, described below                                                                            |
| `textHeightBehavior`  | Leading trim: `0` keep all, `1` drop the first line's ascent, `2` drop the last line's descent, `3` drop both |

`textAlign` and `textDirection` are matched case-insensitively, and an unrecognised value is **ignored rather than rejected** — `textAlign: "centre"` lays out left-aligned with no warning.

#### strutStyle

A strut is a fixed line box that does not depend on which fonts the runs happen to use, so line spacing stays identical whether or not a line contains a taller glyph. Supplying the object enables it unless `enabled` is explicitly `false`.

| Field              | Meaning                                                        |
| ------------------ | -------------------------------------------------------------- |
| `enabled`          | Set to `false` to describe a strut without applying it          |
| `fontFamilies`     | Families whose metrics define the line box                      |
| `fontSize`         | Size those metrics are taken at                                 |
| `heightMultiplier` | Line-height multiplier for the strut box                        |
| `leading`          | Extra leading as a multiple of the strut font size              |
| `forceStrutHeight` | Clamp every line to the strut height rather than treating it as a minimum |
| `halfLeading`      | Split the leading half above and half below the text            |

With a 14 px font, `{ fontSize: 14, heightMultiplier: 2, forceStrutHeight: true }` gives every line a height of 28 instead of 14.

### `pushStyle()`

```js returns="ParagraphBuilder"
pushStyle(style);
```

Pushes a text style onto the builder's stack. Everything added afterwards uses it until a matching [`pop()`][pb_pop].

| Field                                              | Meaning                                                                            |
| -------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `fontSize`                                          | Size in pixels                                                                      |
| `fontFamilies`                                      | Array of family names, tried in order                                               |
| `fontStyle`                                         | `{ weight, width, slant }`                                                          |
| `color`                                             | Fill color for the glyphs                                                           |
| `foregroundColor`                                   | The same glyph paint, and applied after `color` if both are set                     |
| `backgroundColor`                                   | Color painted behind the run                                                        |
| `letterSpacing` / `wordSpacing`                     | Extra space per letter and per word, in pixels                                      |
| `heightMultiplier`                                  | Line-height multiplier for runs in this style                                       |
| `halfLeading`                                       | Centre the run's leading in its line box                                            |
| `decoration`                                        | Bitmask of `TextDecoration` flags, combined with `\|`                               |
| `decorationStyle`                                   | One `TextDecorationStyle` value                                                     |
| `decorationColor`                                   | Color of the decoration lines; falls back to the text color                         |
| `decorationThickness`                               | Multiplier on the font's own decoration thickness                                   |
| `shadows`                                           | Array of `{ color, offset: [dx, dy], blurRadius }`                                  |
| `fontVariations`                                    | Variable-font axis positions, `{ axis, value }` with a 4-character OpenType tag      |
| `fontFeatures`                                      | OpenType features, `{ name, value }` — `smcp`, `liga`, `onum`, `tnum`, `ss01`, …     |

Colors accept either a CSS string or a `[r, g, b, a]` array of premultiplied linear-light floats.

`TextDecoration` and `TextDecorationStyle` are exported alongside the classes. The decoration flags combine, the styles do not:

```js
builder.pushStyle({
  fontStyle: { weight: 700 },
  color: "#c0392b",
  decoration: TextDecoration.Underline | TextDecoration.LineThrough,
  decorationStyle: TextDecorationStyle.Wavy,
  shadows: [{ color: "rgba(0,0,0,0.35)", offset: [1, 1], blurRadius: 2 }],
});
builder.addText("emphasised");
builder.pop();
```

`fontVariations` entries are clamped to the typeface's declared range for that axis, and entries naming an axis the typeface does not expose are dropped. Because `ParagraphBuilder` reads its font collection once at construction, variations set on the paragraph style apply to the whole paragraph; per-run axis changes are not supported.

### `pop()`

```js returns="ParagraphBuilder"
pop();
```

Discards the innermost pushed style. Calling it with nothing on the stack is not an error.

### `addText()`

```js returns="ParagraphBuilder"
addText(text);
```

Appends `text` in the current style. Newlines in the string are hard breaks.

### `addPlaceholder()`

```js returns="ParagraphBuilder"
addPlaceholder(width, height, align, baseline, offset);
```

Reserves a `width × height` rectangle in the text flow for something you draw yourself — an inline icon, an image, a rendered formula. The text wraps around it and [`getRectsForPlaceholders()`][getrectsforplaceholders] reports where each one landed, in insertion order.

:::warning[Changed in 4.2.0]
`align` and `baseline` were read and discarded before 4.2.0, so every placeholder sat on the baseline whatever you passed. They are now honoured, and a value outside either set throws a `TypeError` rather than falling back to the default.
:::

`align` is one of the `PlaceholderAlignment` constants:

| Value                            | Position                                                    |
| -------------------------------- | ----------------------------------------------------------- |
| `PlaceholderAlignment.Baseline`  | The placeholder's own baseline meets the text's              |
| `PlaceholderAlignment.AboveBaseline` | Bottom edge sits on the baseline                        |
| `PlaceholderAlignment.BelowBaseline` | Top edge hangs from the baseline                        |
| `PlaceholderAlignment.Top`       | Top edge aligns with the top of the line                     |
| `PlaceholderAlignment.Bottom`    | Bottom edge aligns with the bottom of the line               |
| `PlaceholderAlignment.Middle`    | Centred vertically in the line                               |

Against a line taller than the placeholder — a 16 px box in a 72 px line — the six alignments produce five distinct positions. `Baseline` and `BelowBaseline` coincide when `offset` is `0`, because at that offset the placeholder's baseline _is_ its top edge:

| Alignment       | Top edge |
| --------------- | -------- |
| `Top`           | 0.00     |
| `Middle`        | 28.00    |
| `AboveBaseline` | 39.44    |
| `Baseline`      | 55.44    |
| `BelowBaseline` | 55.44    |
| `Bottom`        | 56.00    |

`baseline` picks which of the line's baselines to measure against — `TextBaseline.Alphabetic` or `TextBaseline.Ideographic`. It affects all three baseline-relative alignments, not just `Baseline`: switching the same 72 px example to `Ideographic` moves `Baseline` and `BelowBaseline` from `55.44` to `64.00` and `AboveBaseline` from `39.44` to `47.72`. `Top`, `Bottom` and `Middle` ignore it.

`offset` is the distance from the placeholder's top edge down to its own baseline, so raising it lifts the box: with the same 72 px line, offsets of `0`, `8` and `16` put the top edge at `55.44`, `47.44` and `39.44`.

```js
builder.addText("before ");
builder.addPlaceholder(40, 20, PlaceholderAlignment.Middle);
builder.addText(" after");
```

### `build()`

```js returns="Paragraph"
build();
```

Finishes the paragraph and returns it. The builder is consumed — a second `build()` throws an `Error`.

The result is not yet measurable or drawable. Call [`layout()`][layout] on it with a wrap width first; until then every getter reports zero and [`drawParagraph()`][drawparagraph] silently paints nothing.

<!-- references_begin -->

[pb_new]: #new-paragraphbuilder
[pb_make]: #new-paragraphbuilder
[pb_pushstyle]: #pushstyle
[pb_pop]: #pop
[pb_addtext]: #addtext
[pb_addplaceholder]: #addplaceholder
[pb_build]: #build
[paragraph]: paragraph.md
[layout]: paragraph.md#layout
[getrectsforplaceholders]: paragraph.md#getrectsforplaceholders
[drawparagraph]: paragraph.md#drawing-a-paragraph
[fontlibrary]: font-library.md
[fillText]: context.md#filltext--stroketext

<!-- references_end -->
