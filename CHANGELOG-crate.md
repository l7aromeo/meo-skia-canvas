# Changelog — the Rust crate

Changes to the crate `meo-skia-canvas`, published on crates.io and versioned
independently of the npm package.

> Sibling files: the npm addon's is [CHANGELOG-npm.md](CHANGELOG-npm.md), and
> [CHANGELOG.md](CHANGELOG.md) is the index and the combined history from
> before the two were separated. The channels number separately and are not
> comparable version for version — the crate starts at `0.2.0`, npm continues
> `phyron-skia-canvas`'s from `3.6.0`.
>
> **A change that affects both surfaces appears in both files**, written for
> each audience rather than copied. `TextDirection` gaining `Inherit` is the
> same commit as `ctx.direction` reporting `"inherit"`, and a Rust reader
> should not have to work that out from a JavaScript property name.
>
> Releases before the split are in [CHANGELOG.md](CHANGELOG.md).

## 📦 ⟩ [UNRELEASED] ⟩ September 7, 2026

**The version is not yet decided.** Three entries below break the public API,
so this is not a patch.

### Breaking

- **`Font::italic: bool` becomes `Font::slant: FontSlant`.** `Normal`,
  `Italic` and `Oblique` are three values where a boolean carried two, and
  oblique was unreachable from Rust — the binding had parsed the keyword since
  before this release. A caller setting `font.italic = true` writes
  `font.slant = FontSlant::Italic`.

- **`TextDirection` gains `Inherit`, and it is the new default.** The Canvas
  standard makes `inherit` the initial value of the direction attribute and a
  state it holds, naming the surrounding document's direction — which a canvas
  does not have. `Context2D::direction()` on a fresh context returns
  `TextDirection::Inherit` where it returned `LeftToRight`. **Nothing about
  layout moves**: with no document to inherit from, text still lays out left to
  right. A `match` over the enum gains an arm; the type is not
  `#[non_exhaustive]`.

  _The same change reaches JavaScript as `ctx.direction` reporting `"inherit"`,
  where it is the release's one silent break._

- **`Error::InvalidRadius` is split out of `Error::InvalidRect`.** A negative
  radius reported the rectangle it had built rather than the radius that was
  wrong. `Error` is not `#[non_exhaustive]`, so an exhaustive `match` gains an
  arm, and code matching `InvalidRect` for a radius will now miss it.

### Added

- **`Affine::inverse` and `Affine::multiply`.** A Rust caller could not invert
  or compose a transform without reaching for `skia_safe`, which the crate's
  own API guarantee forbids surfacing.

### Changed

- **`oblique` selects the italic face rather than the upright one.** Skia's
  matcher does not fall back from oblique to italic, so a family with no
  oblique face returned upright: `Font::new("Times", 64.0)` with
  `FontSlant::Oblique` painted exactly what `FontSlant::Normal` paints. CSS
  Fonts 4 and Chrome 148 both use the italic face. The slant a caller sets is
  stored and reported unchanged.

  _The same rule reaches JavaScript through `ctx.font` and the paragraph API._

- **An SVG stating one dimension takes the other from its `viewBox`.**

- **`Context2D::round_rect` starts its contour where the standard does.**

### Fixed

- **`Window { visible: false }` opens a hidden window** — feature `window`. It
  opened a visible one that took focus; the option was honoured at
  construction and discarded a step later.

### Internal

- **Nine enum parsers produce this crate's types rather than Skia's**, across
  ten parsers. Nothing observable changed on either surface: the public enums
  carry the same variants they did at `rust-v0.15.0`, and the parsers are not
  reachable from Rust at any feature set.

---

## Entries still to be assigned a channel

Scaffold note, not a released section. These are in the npm file and were not
verified as crate-reachable either way, so they have not been moved or copied:

- `lab()` and `lch()` resolving against D50 adapted to D65
- Colour converted the way CSS Color 4 defines it
- Text measured and drawn the way the standard describes
- `outlineText` and the half-kern
- Pixel reads bounded in the space they are measured in
- Value conversion (`canvas.width`)
- Exception types

Each needs the same treatment the others got: reach it from a Rust consumer
with default features, or establish that it cannot be reached.
