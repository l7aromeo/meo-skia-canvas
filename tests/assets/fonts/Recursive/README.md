# Recursive fixtures

Four subsets of Recursive, `Recursive[CASL,CRSV,MONO,slnt,wght].ttf` from
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
