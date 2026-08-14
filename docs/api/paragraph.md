---
description: A shaped block of text you can measure, hit-test, and draw
---

# Paragraph

> A `Paragraph` is a block of styled text that has been broken into lines. It is the only text object in the library that can wrap, carry more than one style, and report per-line metrics. You do not construct one directly — build it with a [`ParagraphBuilder`][paragraphbuilder], call [`layout()`][layout] to break it into lines, then paint it with [`drawParagraph()`][drawparagraph]. 🧪

| Lifecycle                        | Block metrics                              | Line metrics                             | Hit testing                                          |
| -------------------------------- | ------------------------------------------ | ---------------------------------------- | ---------------------------------------------------- |
| [layout()][layout]               | [getHeight()][getheight]                   | [getNumberOfLines()][getnumberoflines]   | [getGlyphPositionAtCoordinate()][getglyphposition]   |
| [drawParagraph()][drawparagraph] | [getMaxWidth()][getmaxwidth]               | [didExceedMaxLines()][didexceedmaxlines] | [getRectsForRange()][getrectsforrange]               |
|                                  | [getLongestLine()][getlongestline]         | [getLineMetrics()][getlinemetrics]       | [getRectsForPlaceholders()][getrectsforplaceholders] |
|                                  | [getMaxIntrinsicWidth()][getintrinsic]     |                                          | [getUnresolvedCodepoints()][getunresolved]           |
|                                  | [getMinIntrinsicWidth()][getintrinsic]     |                                          |                                                      |
|                                  | [getAlphabeticBaseline()][getbaselines]    |                                          |                                                      |
|                                  | [getIdeographicBaseline()][getbaselines]   |                                          |                                                      |
|                                  | [getFirstLineAscent() 🧪][getfirstlineascent] |                                       |                                                      |

## Creating a paragraph

`new Paragraph()` throws a `TypeError`. A paragraph can carry several styled runs and inline placeholders, so there is no set of constructor arguments that would describe one; it comes out of [`ParagraphBuilder.build()`][build] instead.

```js
const paragraph = new ParagraphBuilder({ textStyle: { fontSize: 16 } })
  .addText("Wrapped, styled, measured text.")
  .build();
```

## Laying it out

Nothing on this page reports anything useful until `layout()` has run.

### `layout()`

```js returns="void"
layout(width);
```

Breaks the text into lines at `width` pixels. Required before measuring or drawing: every getter below returns `0` or an empty array on a paragraph that has never been laid out, and [`drawParagraph()`][drawparagraph] paints nothing without throwing.

It is safe to call again with a different width to re-wrap the same paragraph.

```js
// "Wrapped, styled, measured text." at 16px Helvetica
paragraph.layout(120);
paragraph.getHeight(); // → 32
paragraph.getNumberOfLines(); // → 2

paragraph.layout(600);
paragraph.getHeight(); // → 16
paragraph.getNumberOfLines(); // → 1
```

## Drawing a paragraph

```js returns="void"
ctx.drawParagraph(paragraph, x, y);
```

`drawParagraph()` lives on the [context][context], not on the paragraph. Three things about it differ from [`fillText()`][fillText]:

- **`layout()` must have run.** An un-laid-out paragraph has no lines to draw, and this paints nothing rather than throwing.
- **`(x, y)` is the top-left corner of the text block**, not a baseline. The same coordinates passed to `fillText()` put the text roughly one line higher.
- **The context's color settings do not apply.** The transform, the clip and `globalAlpha` all take effect, but `fillStyle` and `strokeStyle` are ignored — color and decoration come from the text styles the paragraph was built with.

---

## Block metrics

### `getHeight()`

```js returns="number"
getHeight();
```

Total height of all laid-out lines, in pixels.

### `getMaxWidth()`

```js returns="number"
getMaxWidth();
```

The width that was passed to [`layout()`][layout].

### `getLongestLine()`

```js returns="number"
getLongestLine();
```

Width of the widest line actually produced. This is what you want for a tight bounding box — it is normally smaller than `getMaxWidth()`.

### `getMaxIntrinsicWidth()` & `getMinIntrinsicWidth()`

```js returns="number"
getMaxIntrinsicWidth();
getMinIntrinsicWidth();
```

The widths at which the text would stop changing shape: the max is the width needed to fit every line without wrapping, the min is the width of the widest single unbreakable word. Laying out anywhere between the two produces a different set of line breaks; outside it, nothing moves.

### `getAlphabeticBaseline()` & `getIdeographicBaseline()`

```js returns="number"
getAlphabeticBaseline();
getIdeographicBaseline();
```

Distance from the top of the paragraph down to the first line's alphabetic and ideographic baselines. Add these to the `y` you pass to [`drawParagraph()`][drawparagraph] to line other drawing up with the first line of text.

### `getFirstLineAscent()`

```js returns="number"
getFirstLineAscent();
```

The distance from the paragraph's top edge to the first line's baseline — what to add to a `y` coordinate to place a paragraph by its baseline rather than by its top, the way [`fillText()`][fillText] places a string:

```js
let ascent = paragraph.getFirstLineAscent();
ctx.drawParagraph(paragraph, x, y - ascent); // baseline sits at `y`
```

This is the same number as `getLineMetrics()[0].ascent`, without having to build the whole metrics array for it. An empty paragraph returns `0`, having no first line to measure.

Not part of CanvasKit's `Paragraph`.

---

## Line metrics

### `getNumberOfLines()`

```js returns="number"
getNumberOfLines();
```

How many lines the layout produced.

### `didExceedMaxLines()`

```js returns="boolean"
didExceedMaxLines();
```

Whether layout dropped content because of the paragraph style's `maxLines`. Combined with an `ellipsis`, this is how you tell truncated text from text that simply fit.

### `getLineMetrics()`

```js returns="LineMetrics[]"
getLineMetrics();
```

One object per line, each with:

| Field                     | Meaning                                                     |
| ------------------------- | ----------------------------------------------------------- |
| `lineNumber`              | Zero-based index of the line                                |
| `startIndex` / `endIndex` | Range of the source text on this line                       |
| `endExcludingWhitespaces` | End index with trailing whitespace trimmed                  |
| `endIncludingNewline`     | End index including the line break, if any                  |
| `isHardBreak`             | Whether the line ended at a newline rather than at the wrap  |
| `ascent` / `descent`      | Distance from the baseline up and down                      |
| `height`                  | Line height                                                 |
| `width`                   | Width of the text on this line                              |
| `left`                    | Left edge of the line, which alignment moves                |
| `baseline`                | Distance from the top of the **paragraph** down to this line's baseline |

`baseline` accumulates down the paragraph rather than restarting per line: with 16 px single-spaced lines it reads `12.32`, `28.32`, `44.32`, and so on.

All indices are UTF-16 code units, matching JavaScript string indexing.

---

## Hit testing

### `getGlyphPositionAtCoordinate()`

```js returns="{ pos, affinity }"
getGlyphPositionAtCoordinate(x, y);
```

Maps a point in the paragraph's own coordinate space to a text index. `pos` is the nearest character boundary and `affinity` says which side of it the point fell on: `1` (downstream) in the leading half of a glyph, `0` (upstream) in the trailing half. Both halves of a glyph therefore describe the same glyph, from opposite ends.

```js
// "abc def" at 20px Helvetica, where the "a" spans roughly x 0–11
paragraph.getGlyphPositionAtCoordinate(0, 10); // → { pos: 0, affinity: 1 }
paragraph.getGlyphPositionAtCoordinate(9, 10); // → { pos: 1, affinity: 0 }
```

### `getRectsForRange()`

```js returns="TextBox[]"
getRectsForRange(start, end, (hStyle = 0), (wStyle = 0));
```

Boxes covering the text from `start` to `end` — one per line the range spans, and one per direction run within a line. `start` and `end` are UTF-16 indices; an empty range returns `[]` and an `end` past the text is clamped.

Each `TextBox` is `{ rect, direction }`, where `rect` is `[left, top, right, bottom]` and `direction` is `1` for left-to-right or `0` for right-to-left.

`hStyle` chooses how tall the boxes are and `wStyle` how wide:

| `hStyle` | Skia mode                    | Height                                          |
| -------- | ---------------------------- | ----------------------------------------------- |
| `0`      | `Tight`                      | Around the glyphs only                          |
| `1`      | `Max`                        | The full line box                               |
| `2`      | `IncludeLineSpacingMiddle`   | Tight, plus line spacing split between the lines |
| `3`      | `IncludeLineSpacingTop`      | Tight, plus the line spacing above              |
| `4`      | `IncludeLineSpacingBottom`   | Tight, plus the line spacing below              |
| `5`      | `Strut`                      | The strut's line box                            |

| `wStyle` | Skia mode | Width                                                                                 |
| -------- | --------- | ------------------------------------------------------------------------------------- |
| `0`      | `Tight`   | Around the glyphs only                                                                |
| `1`      | `Max`     | Extended to the line's full width, which adds a box for the part the range did not reach |

Values outside these sets fall back to `0` rather than throwing.

### `getRectsForPlaceholders()`

```js returns="TextBox[]"
getRectsForPlaceholders();
```

Where each [`addPlaceholder()`][addplaceholder] reservation ended up, in insertion order and in the same `{ rect, direction }` shape. This is how you find out where to draw the icon or image the placeholder was standing in for.

```js
const [box] = paragraph.getRectsForPlaceholders();
const [left, top, right, bottom] = box.rect;
ctx.drawImage(icon, x + left, y + top, right - left, bottom - top);
```

### `getUnresolvedCodepoints()`

```js returns="number[]"
getUnresolvedCodepoints();
```

Codepoints that no font in the collection could supply a glyph for — the ones that would render as tofu. Intended for validating automated multi-language renders, where a missing script is otherwise easy to ship without noticing. The array is empty when every codepoint resolved, which includes anything the system's fallback fonts covered.

<!-- references_begin -->

[layout]: #layout
[drawparagraph]: #drawing-a-paragraph
[getheight]: #getheight
[getmaxwidth]: #getmaxwidth
[getlongestline]: #getlongestline
[getintrinsic]: #getmaxintrinsicwidth--getminintrinsicwidth
[getbaselines]: #getalphabeticbaseline--getideographicbaseline
[getfirstlineascent]: #getfirstlineascent
[getnumberoflines]: #getnumberoflines
[didexceedmaxlines]: #didexceedmaxlines
[getlinemetrics]: #getlinemetrics
[getglyphposition]: #getglyphpositionatcoordinate
[getrectsforrange]: #getrectsforrange
[getrectsforplaceholders]: #getrectsforplaceholders
[getunresolved]: #getunresolvedcodepoints
[paragraphbuilder]: paragraph-builder.md
[build]: paragraph-builder.md#build
[addplaceholder]: paragraph-builder.md#addplaceholder
[context]: context.md
[fillText]: context.md#filltext--stroketext

<!-- references_end -->
