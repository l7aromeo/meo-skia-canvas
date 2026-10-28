# Recursive fixtures

Five files from Recursive, `Recursive[CASL,CRSV,MONO,slnt,wght].ttf` from
github.com/google/fonts, under the SIL Open Font License in `OFL.txt`. Each
holds only the glyphs the oblique tests draw, and each was made with
`hb-subset` at `CASL=0,MONO=0,CRSV=0,wght=400`:

- `Recursive-slnt-subset.ttf`: `I` and `H`, keeping the `slnt` axis, -15 to 0.
- `Recursive-upright-subset.ttf`: `I` and `H`, instanced at `slnt=0`.
- `Recursive-upright-H-subset.ttf`: `H` alone, instanced at `slnt=0`, so an
  `I` falls back to another family.
- `Recursive-oblique12-subset.ttf`: `I` and `H`, instanced at `slnt=-12`, then
  flagged oblique -- `OS/2` `fsSelection` bit 9 set and bit 6, `REGULAR`,
  cleared -- with a `post` `italicAngle` of -12.

The last is Recursive instanced at `slnt` -12 and re-flagged, not a shipping
face. It stands in for a family with a true oblique face, which no font in
this tree has.

- `Recursive-upright-strikeout-subset.ttf`: `Recursive-upright-subset.ttf`
  with one value changed, `OS/2` `yStrikeoutSize` from 72 to 144, twice the
  `post` `underlineThickness` of 72, and its table checksums and
  `checkSumAdjustment` recomputed. Every other bundled face has the two
  thicknesses equal, which hides a line-through drawn at the underline's
  thickness or scaled by their ratio.

A modified version may keep the name: `OFL.txt` reserves no font name.
