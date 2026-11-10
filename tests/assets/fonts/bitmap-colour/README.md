# bitmap-colour fixture

`sbix.ttf`, `cbdt.ttf` and `SampleSVG.ttf`, from the skia-bindings 0.156.0
crate's `skia/resources/fonts/`, unmodified, under Skia's BSD 3-Clause
licence in `LICENSE`, that crate's `skia/LICENSE`. Skia draws them as its
emoji samples (`tools/fonts/FontToolUtils.cpp`, `EmojiSample`): `sbix.ttf`
and `cbdt.ttf` with `😀 ♢`, `SampleSVG.ttf` with `abcdefghij`.

Their colour glyphs are held in the tables a platform rasterizes itself: PNG
images in `sbix` and in `CBDT`, and SVG documents in `SVG `. They stand in
for the emoji fonts that use those tables -- Apple Color Emoji is an `sbix`
font, Noto Color Emoji a `CBDT` one. CoreText loads no face from `cbdt.ttf`,
which has no outlines, and draws `SampleSVG.ttf`'s glyphs in the fill.
