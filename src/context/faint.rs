//! What a paint too faint for eight bits draws on a page that can hold it.
//!
//! An alpha under half of one 255th rounds to zero in eight bits, and Skia
//! drops a source-over draw whose paint alpha does that: `SkPaint` decides
//! `nothingToDraw` on `getAlpha()`, the alpha rounded to a byte, whatever
//! the pixels the draw lands in. On an F16 or F32 page the draw has
//! somewhere to go -- a `globalAlpha` of 0.001 is a level F16 holds to a
//! two-thousandth -- so there the paint is drawn opaque through a colour
//! filter that scales what it paints by its alpha, which is the same
//! arithmetic with nothing for the skip to round away.

use std::borrow::Cow;

use skia_safe::{BlendMode, Color4f, ColorType, Paint, color_filters};

use crate::context::page::ExportOptions;

/// Whether a page's pixels hold a float alpha, and so whether a paint Skia
/// would drop as too faint has to be drawn another way -- see
/// [`Faint::drawable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Faint {
    /// An eight-bit alpha, where a paint under half of one 255th draws
    /// nothing it could keep, and is left to Skia's skip.
    Dropped,
    /// A float alpha, which keeps such a draw.
    Kept,
}

impl Faint {
    /// What a canvas of `color_type` does with a faint paint: what the
    /// surface it composites in does, which is N32 for every format but the
    /// float ones -- see `ExportOptions::compositing_color_type`.
    pub fn of(color_type: ColorType) -> Self {
        let asked = ExportOptions {
            surface_color_type: color_type,
            ..ExportOptions::default()
        };
        match asked.compositing_color_type() {
            ColorType::N32 => Faint::Dropped,
            _ => Faint::Kept,
        }
    }

    /// `paint` as it has to be handed to Skia to draw what it paints: as it
    /// is, unless these pixels keep a faint draw and Skia would drop this
    /// one, in which case its alpha moves into a `DstIn` colour filter run
    /// before any it has, and the paint itself is opaque.
    ///
    /// Asked of the paint at the draw that uses it, after anything that
    /// sets its colour or its alpha, because a colour set on a paint
    /// already moved over replaces an alpha the filter would still apply.
    ///
    /// Skia's own `nothing_to_draw` decides it, so what it skips is exactly
    /// what is redrawn. A paint with an image filter it does not skip, but
    /// draws into a layer of its own and drops the faint draw inside that;
    /// this leaves such a paint alone, because Skia's raster backend gives a
    /// filtered layer on a float page back in steps of one 255th, so the
    /// redrawn shape would come back as nothing all the same. A shadow,
    /// which is drawn that way, carries its alpha in its colour instead --
    /// see `Context2D::paint_for_shadow`.
    ///
    /// An eight-bit page keeps Skia's skip. There the scaled draw leaves
    /// every byte as the skip does for a colour inside `[0, 1]`, on the CPU
    /// engine at least, but not for an extended one, whose faint draw moves
    /// a level.
    pub fn drawable(self, paint: &Paint) -> Cow<'_, Paint> {
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
        // Re-check: cargo test a_faint_colour_fills with `drawable` returning
        // the paint as it is.
        //
        // `SkPaint::nothingToDraw` tests the alpha rounded to eight bits,
        // so on float pixels a source-over draw under half of one 255th is
        // dropped although the pixels could hold it.
        let alpha = paint.alpha_f();
        if self == Faint::Dropped || paint.alpha() != 0 || alpha <= 0.0 {
            return Cow::Borrowed(paint);
        }
        if !paint.nothing_to_draw() {
            return Cow::Borrowed(paint);
        }
        let Some(scale) = color_filters::blend_with_color_space(
            Color4f::new(0.0, 0.0, 0.0, alpha),
            None,
            BlendMode::DstIn,
        ) else {
            return Cow::Borrowed(paint);
        };
        let filter = match paint.color_filter() {
            Some(filter) => filter.composed(scale),
            None => Some(scale),
        };
        let Some(filter) = filter else {
            return Cow::Borrowed(paint);
        };
        let mut drawn = paint.clone();
        drawn.set_alpha_f(1.0).set_color_filter(filter);
        Cow::Owned(drawn)
    }
}
