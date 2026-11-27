//! The switch under which Skia's SVG module paints with an eight-bit alpha,
//! as a browser does, for a document played into raster pixels.
//!
//! A browser paints an SVG with each paint's alpha on a byte: the product of
//! the colour's alpha, its `fill-opacity` or `stroke-opacity`, and an
//! `opacity` folded into that one draw, rounded once. Skia's module keeps the
//! product in float, which a half-float surface then keeps too, so a tie --
//! black at 0.5 over red, exactly 127.5 -- lands 128 where Chrome lands 127.
//! It is the canvas's own rule: docs/browser-differences.md takes the
//! backdrop's share at the alpha rounded to a byte, as Chrome's is.
//!
//! `containers/skia-svg-byte-paint-alpha.patch` adds a per-thread switch,
//! `meo_svg_byte_paint_alpha`, which [`BytePaintAlpha`] holds on for as long
//! as it lives. Under it `SkSVGRenderContext::commonPaint` and `<feFlood>`'s
//! colour round their alpha to a byte, and a `<g>` or `<svg>` whose one child
//! is a shape hands its opacity to that shape's paint rather than laying a
//! layer, so the product rounds once. It is held only where a document is
//! played into raster pixels, never into a PDF or an SVG, which write the
//! document's opacities as it states them.
//!
//! Without the patch, as on crates.io's Skia, holding it does nothing.

use std::cell::Cell;

thread_local! {
    /// How many holds are alive on this thread. The switch is on while any
    /// is, so a hold made inside another does not turn it off for the rest
    /// of the outer one.
    static HOLDS: Cell<u32> = const { Cell::new(0) };
}

/// Holds this thread's SVG paints on an eight-bit alpha until dropped -- see
/// the module's documentation. Dropped on every exit, a panic unwinding
/// through a playback included, so the next drawing on the thread is not
/// left rounding.
pub(crate) struct BytePaintAlpha {
    /// Keeps the hold on the thread that made it: the switch is per thread.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl BytePaintAlpha {
    /// Turns the switch on for this thread, where it is not already.
    pub(crate) fn hold() -> Self {
        HOLDS.with(|holds| {
            if holds.get() == 0 {
                switch(true);
            }
            holds.set(holds.get() + 1);
        });
        Self {
            _not_send: std::marker::PhantomData,
        }
    }

    /// Whether a hold is alive on this thread.
    #[cfg(test)]
    pub(crate) fn held() -> bool {
        HOLDS.with(|holds| holds.get() > 0)
    }
}

impl Drop for BytePaintAlpha {
    fn drop(&mut self) {
        HOLDS.with(|holds| {
            let left = holds.get().saturating_sub(1);
            holds.set(left);
            if left == 0 {
                switch(false);
            }
        });
    }
}

#[cfg(meo_svg_byte_paint_alpha)]
unsafe extern "C" {
    /// Sets the switch for the calling thread. Defined in
    /// `modules/svg/src/SkSVGRenderContext.cpp` by
    /// `containers/skia-svg-byte-paint-alpha.patch`.
    fn meo_svg_byte_paint_alpha(on: bool);
}

/// Sets Skia's switch for this thread.
#[cfg(meo_svg_byte_paint_alpha)]
fn switch(on: bool) {
    // SAFETY: the function stores `on` in a `thread_local bool` and does
    // nothing else; it reads no pointer and cannot fail. The cfg is set only
    // where build.rs found the patch's definition in the Skia linked.
    unsafe { meo_svg_byte_paint_alpha(on) }
}

/// Without the patch there is no switch to set.
#[cfg(not(meo_svg_byte_paint_alpha))]
fn switch(_on: bool) {}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, Color4f, ColorSpace, ColorType, ImageInfo,
        image::CachingHint,
    };

    use std::{cell::Cell, panic::AssertUnwindSafe};

    use super::BytePaintAlpha;
    use crate::{
        image::{Image, Svg, render_info},
        started,
    };

    /// Whether Skia rounds a paint's alpha under a hold here.
    const ROUNDS: bool = cfg!(meo_svg_byte_paint_alpha);

    /// Black at 0.5 over red: the red left is exactly 127.5.
    const TIE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">
<rect width="1" height="1" fill="red"/>
<rect width="1" height="1" fill="black" fill-opacity="0.5"/>
</svg>"#;

    /// What a half-float surface keeps of [`TIE`] unrounded, read as a byte.
    const UNROUNDED: u8 = 128;

    /// What it keeps with the paint's alpha on a byte, 128 of 255.
    const ON_A_BYTE: u8 = 127;

    /// The red [`TIE`]'s module draws straight onto rasterize's surface,
    /// with whatever this thread's switch says -- no hold of its own.
    fn red() -> u8 {
        let svg = Svg::parse(TIE).expect("the document parses");
        let mut surface =
            started::raster(&render_info(1, 1), None, None).expect("a surface");
        surface.canvas().clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
        svg.dom.render(surface.canvas());
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut px = [0u8; 4];
        assert!(surface.read_pixels(&info, &mut px, 4, (0, 0)));
        px[0]
    }

    /// The red a hold lands: rounded where Skia carries the patch.
    fn held_red() -> u8 {
        if ROUNDS { ON_A_BYTE } else { UNROUNDED }
    }

    #[test]
    fn a_hold_rounds_and_its_drop_lets_the_switch_go() {
        assert_eq!(red(), UNROUNDED, "before any hold");
        {
            let _hold = BytePaintAlpha::hold();
            assert_eq!(red(), held_red(), "under a hold");
        }
        assert!(!BytePaintAlpha::held());
        assert_eq!(red(), UNROUNDED, "after the hold");
    }

    #[test]
    fn a_panic_inside_a_hold_lets_the_switch_go() {
        // Read under the hold and asserted outside it: an assertion inside
        // would panic into the `catch_unwind` and pass for the wrong reason.
        let under = Cell::new(None);
        let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _hold = BytePaintAlpha::hold();
            under.set(Some(red()));
            panic!("a playback failing under the hold");
        }));
        assert!(unwound.is_err(), "the playback panicked");
        assert_eq!(under.get(), Some(held_red()), "under the hold");
        assert!(!BytePaintAlpha::held());
        assert_eq!(red(), UNROUNDED, "the next draw on the thread");
    }

    #[test]
    fn a_hold_inside_another_keeps_the_switch_on() {
        let _outer = BytePaintAlpha::hold();
        drop(BytePaintAlpha::hold());
        assert!(BytePaintAlpha::held());
        assert_eq!(red(), held_red(), "after the inner hold");
    }

    #[test]
    fn a_rasterize_leaves_the_switch_off_for_the_next_draw() {
        let mut svg = Svg::parse(TIE).expect("the document parses");
        let first = svg.rasterize(1, 1).expect("rasterizes");
        let second = svg.rasterize(1, 1).expect("rasterizes again");
        assert!(!BytePaintAlpha::held());
        assert_eq!(red(), UNROUNDED, "a draw after the rasterize");
        let bytes = |image: &Image| {
            let info = ImageInfo::new(
                (1, 1),
                ColorType::RGBA8888,
                AlphaType::Premul,
                ColorSpace::new_srgb(),
            );
            let mut px = [0u8; 4];
            assert!(image.inner.read_pixels(
                &info,
                &mut px,
                4,
                (0, 0),
                CachingHint::Allow,
            ));
            px
        };
        assert_eq!(bytes(&first)[0], held_red(), "the rasterize itself");
        assert_eq!(bytes(&first), bytes(&second), "a second rasterize");
    }
}
