//! A run of text measures its glyphs' advances in font units times the size,
//! over the em, on every platform.
//!
//! CoreText and DirectWrite give that at the size asked for. Skia's FreeType
//! port, which Linux draws through, sets a face's size in 26.6, truncated --
//! docs/browser-differences.md -- and gave the units times a 16.16 scale
//! FreeType rounds once: a 1000-unit em at 16px advanced 16 + 2/65536, and
//! the excess grew with every glyph on a line (#695).
//! `containers/skia-freetype-advance.patch` scales the unscaled advance by
//! the face's 26.6 size and rounds once to 16.16, as HarfBuzz scales one.
//!
//! A crate build's Skia does not carry the patch, and
//! `cfg(meo_freetype_advance)`, from `build.rs`, says whether this build
//! compiled it. On Linux without it the test holds the build to FreeType's
//! excess instead, so the day crates.io's Skia advances exactly, the test
//! says so.

use meo_skia_canvas::prelude::*;

/// A static face with a 1000-unit em.
const RECURSIVE: &str =
    "tests/assets/fonts/Recursive/Recursive-upright-subset.ttf";

/// A variable face with a 1000-unit em, at its default instance, where
/// `HVAR` moves no advance.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

/// Both faces' em, in font units.
const UNITS_PER_EM: f64 = 1000.0;

/// The size a run's units are read at. FreeType's 16.16 scale for a
/// 1000-unit em there, 125 × 64 × 65536 / 1000 = 524,288, is exact, so
/// every build measures the units times it. At 1000px a Linux build read
/// "I" as 499.999 units.
const UNIT_SIZE: f32 = 125.0;

/// Sizes a 16.16 scale rounds, and 13⅓, which 26.6 cannot hold.
const SIZES: [f32; 5] = [16.0, 13.0, 12.5, 37.0, 40.0 / 3.0];

/// The size a run is laid out at: on FreeType the 26.6 one, truncated, that
/// Skia sets the face to; elsewhere the size asked for.
fn laid_out(size: f32) -> f64 {
    /// 26.6 fixed point's steps to a pixel.
    const SIXTY_FOURTHS: f64 = 64.0;
    if cfg!(target_os = "linux") {
        (f64::from(size) * SIXTY_FOURTHS).trunc() / SIXTY_FOURTHS
    } else {
        f64::from(size)
    }
}

/// What a width may be off the exact one: half a 16.16 unit for each glyph,
/// where rounding each advance once leaves it, and four `f32` steps of the
/// width.
fn allowance(glyphs: usize, exact: f64) -> f64 {
    glyphs as f64 / 131_072.0 + exact * f64::from(f32::EPSILON) * 4.0
}

/// How far each run measures off its units times the size it is laid out
/// at, over the em, with the allowance it had, at each of [`SIZES`].
fn readings() -> Vec<(String, f64, f64)> {
    let library = FontLibrary::new();
    library
        .register_font_from_path("AdvanceRecursive", RECURSIVE)
        .expect("Recursive registers");
    library
        .register_font_from_path("AdvanceOswald", OSWALD)
        .expect("Oswald registers");
    let mut canvas = Canvas::new(100.0, 100.0);
    let ctx = canvas.context();
    let mut found = Vec::new();
    for family in ["AdvanceRecursive", "AdvanceOswald"] {
        for text in ["I", "H", "IIIIII", "HIHIHIHIHIHI"] {
            ctx.set_font(&Font::new(family, UNIT_SIZE));
            let units = f64::from(ctx.measure_text(text, None).width)
                * UNITS_PER_EM
                / f64::from(UNIT_SIZE);
            for size in SIZES {
                ctx.set_font(&Font::new(family, size));
                let got = f64::from(ctx.measure_text(text, None).width);
                let exact = units * laid_out(size) / UNITS_PER_EM;
                found.push((
                    format!("{family} {text:?} at {size}px: {got} for {exact}"),
                    (got - exact).abs(),
                    allowance(text.chars().count(), exact),
                ));
            }
        }
    }
    found
}

#[test]
fn a_glyph_advances_its_units_times_the_size() {
    let readings = readings();
    let off: Vec<_> = readings
        .iter()
        .filter(|(_, error, allowed)| error > allowed)
        .map(|(case, ..)| case.as_str())
        .collect();
    if cfg!(target_os = "linux") && !cfg!(meo_freetype_advance) {
        assert!(
            !off.is_empty(),
            "every run measures its units times the size: FreeType's advance \
             is exact in this Skia, and the patch is no longer needed",
        );
    } else {
        assert!(
            off.is_empty(),
            "measured off the units times the size: {off:#?}"
        );
    }
}
