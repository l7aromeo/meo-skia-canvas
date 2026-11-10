# COLR references

What Chrome 158.0.8127.18 (`chrome-headless-shell`, macOS, Apple Silicon) draws for the COLR test fonts, against which `tests/suite/colour-fonts.test.js` holds the glyphs this library draws itself. Chrome makes a COLR web font through Fontations on Apple platforms (`WebFontTypefaceFactory`, `web_font_typeface_factory.cc`), never CoreText, so these are not CoreText's pixels.

Each `.png` is lossless. Each `.json` beside it says how it was drawn: the font, the size, the colour, the grid, and the code point in each cell. A cell's glyph is drawn at `x` from the cell's left and on a baseline `baseline` pixels from its top. The two test-glyph layouts also list, under `withoutColourPaint`, the code points whose glyph the font's `COLR` table gives no paint, read from the font with skrifa: the platform draws those, not this library's painter, so the comparisons leave their cells out.

- `chrome-test-glyphs-48px.png`: every code point `test_glyphs-glyf_colr_1_variable.ttf` maps, drawn with `fillText` on a 2D canvas created with `willReadFrequently` and read with `getImageData`. That is Chrome's CPU raster.
- `chrome-test-glyphs-48px-varied.png`: the same grid with `font-variation-settings` as the `.json` gives. It is drawn as SVG `<text>` and taken with `--screenshot`, since Chrome's 2D canvas has no way to set a font's axes. Chrome's headless screenshot is the same CPU raster: a circle and a triangle drawn both ways came out byte-identical.
- `chrome-colr-v0.png`: `COLR-palettes-test-font.ttf` (a COLR table of version 0), "A", "AB" and "ABC" at 16, 30, 48 and 100px in `#000`, a 2D canvas as above.

The font was loaded with the `FontFace` API from the same files as `tests/assets/fonts`, served over HTTP, and each page waited on `document.fonts`.
