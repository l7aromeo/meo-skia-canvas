//! A CanvasKit colour filter folded into the flat colour it filters, for a
//! page's pixels.
//!
//! On a float page that keeps a recording for its pixels, a colour filter
//! holding a colour is built for those pixels as a runtime effect --
//! `filter::onto::exact_blend` -- which Skia lowers into raster-pipeline
//! stages for every draw and runs for every pixel. Over a flat colour its
//! output is one colour wherever the draw lands, since Skia applies a colour
//! filter to the source before coverage: so the filter is run once, on one
//! pixel, and the draw paints what it made. That is what Skia does with its
//! own blend filter over a flat colour (`SkPaintPriv::RemoveColorFilter`),
//! and the pixel it reads is the pipeline's own arithmetic, so the fold lands
//! where the filter would, to the bit.
//!
//! What a fold made is kept per thread, in a direct-mapped table keyed by
//! everything the pixel depends on, and a page that alternates colours under
//! one filter finds each where it left it. The table holds at most
//! [`FOLDS_KEPT`] folds, each a 1x1 image shader or a colour filter.

use std::cell::RefCell;

use skia_safe::{
    AlphaType, BlendMode, Color4f, ColorFilter, ColorSpace, ColorType, Data,
    ImageInfo, Paint, PaintStyle, RuntimeEffect, SamplingOptions, Shader,
    TileMode,
};

use crate::{pixels::PixelColorSpace, started};

/// How many folds a thread keeps: a slot each, the slot taken from a hash of
/// the key. A power of two, so that the hash's top bits name a slot.
const FOLDS_KEPT: usize = 4096;
const _: () = assert!(FOLDS_KEPT.is_power_of_two());

/// 2^64 divided by the golden ratio, rounded to odd: Knuth's multiplier
/// for hashing by multiplication (TAOCP vol. 3, section 6.4), whose top bits
/// spread keys that differ only in their low ones.
const GOLDEN_RATIO_64: u64 = 0x9E37_79B9_7F4A_7C15;

/// What a fold depends on: the page's space; the filter, by its identity
/// (`PageFilter::identity`), which no other filter takes; the flat colour
/// filtered, unpremultiplied in the page's space, as bits; the paint's
/// alpha, as bits; and for text, the plain colour whose glyph contrast it
/// keeps, as bits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FoldKey {
    pub space: PixelColorSpace,
    pub filter: u64,
    pub rgb: [u32; 3],
    pub alpha: u32,
    pub text: Option<[u32; 3]>,
}

impl FoldKey {
    /// The key for `rgb` at `alpha`, through the filter `filter`, on a page
    /// in `space`; `text`, the plain colour, for a text draw.
    pub fn new(
        space: PixelColorSpace,
        filter: u64,
        rgb: [f32; 3],
        alpha: f32,
        text: Option<Color4f>,
    ) -> Self {
        Self {
            space,
            filter,
            rgb: rgb.map(f32::to_bits),
            alpha: alpha.to_bits(),
            text: text
                .map(|plain| [plain.r, plain.g, plain.b].map(f32::to_bits)),
        }
    }

    /// The slot the key is kept in: its fields folded into one word, then
    /// spread by [`GOLDEN_RATIO_64`] and cut to the table's size. Two keys
    /// in one slot cost each other a fold, never a wrong colour, since the
    /// whole key is compared.
    fn slot(&self) -> usize {
        let words = [
            self.space as u64,
            self.filter,
            u64::from(self.rgb[0]) | u64::from(self.rgb[1]) << 32,
            u64::from(self.rgb[2]) | u64::from(self.alpha) << 32,
            self.text.map_or(u64::MAX, |[r, g, b]| {
                (u64::from(r) | u64::from(g) << 32)
                    ^ u64::from(b).rotate_left(17)
            }),
        ];
        let folded = words.into_iter().fold(0u64, |hash, word| {
            (hash ^ word).wrapping_mul(GOLDEN_RATIO_64)
        });
        (folded.wrapping_mul(GOLDEN_RATIO_64)
            >> (u64::BITS - FOLDS_KEPT.trailing_zeros())) as usize
    }
}

/// What a draw paints in place of its colour and the filter over it.
#[derive(Clone)]
pub(crate) enum Folded {
    /// For a fill or stroke: the filtered colour, premultiplied, as a shader
    /// the paint draws at an alpha of one.
    Shader(Shader),
    /// For text: the filtered colour as a colour filter in place of the
    /// paint's, which keeps the colour shader Skia sets a glyph mask's
    /// contrast from.
    Filter(ColorFilter),
}

thread_local! {
    /// The folds this thread has made -- see [`FOLDS_KEPT`]. Made on the
    /// first fold, since most threads never make one.
    static FOLDS: RefCell<Vec<Option<(FoldKey, Folded)>>> =
        const { RefCell::new(Vec::new()) };

    /// [`CONSTANT_SKSL`], compiled once per thread: a `RuntimeEffect` is not
    /// `Send`.
    static CONSTANT: Option<RuntimeEffect> =
        RuntimeEffect::make_for_color_filter(CONSTANT_SKSL, None).ok();
}

#[cfg(test)]
thread_local! {
    /// Set by a test to draw every filter as it is, unfolded: the route the
    /// fold is held to.
    pub(crate) static FOLD_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// How many folds this thread has run, for a test counting them.
    pub(crate) static FOLDS_RUN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// The key of the fold this thread last asked for, for a test that
    /// needs the key a page builds rather than one built by hand.
    pub(crate) static LAST_KEY: std::cell::Cell<Option<FoldKey>> = const { std::cell::Cell::new(None) };
}

/// SkSL for a [`Folded::Filter`]: `colour`, premultiplied, whatever the
/// filter is handed. A uniform is not converted, so it is the colour the
/// fold read, as the page's space holds it.
const CONSTANT_SKSL: &str = "uniform float4 colour; \
    half4 main(half4 c) { return colour; }";

/// The fold for `key`, as a thread kept it or as `make` makes it, kept for
/// the next draw that asks. `None` where a fold could not be made, and the
/// draw is drawn unfolded.
pub(crate) fn folded(
    key: FoldKey,
    make: impl FnOnce() -> Option<Folded>,
) -> Option<Folded> {
    #[cfg(test)]
    {
        LAST_KEY.set(Some(key));
        if FOLD_OFF.get() {
            return None;
        }
    }
    let slot = key.slot();
    if let Some(folded) = FOLDS.with_borrow(|folds| match folds.get(slot) {
        Some(Some((kept, folded))) if *kept == key => Some(folded.clone()),
        _ => None,
    }) {
        return Some(folded);
    }
    let made = make()?;
    FOLDS.with_borrow_mut(|folds| {
        if folds.is_empty() {
            folds.resize(FOLDS_KEPT, None);
        }
        folds[slot] = Some((key, made.clone()));
    });
    Some(made)
}

/// The colour `paint` -- a flat colour, and the colour filter over it --
/// paints a pixel it covers whole on a page in `space`, premultiplied: drawn
/// onto one pixel of floats in that space, replacing it, and read back.
/// `paint` is the paint as the page would draw it, after `Faint::drawable`.
pub(crate) fn fold_pixel(
    paint: &Paint,
    space: &ColorSpace,
) -> Option<[f32; 4]> {
    #[cfg(test)]
    FOLDS_RUN.set(FOLDS_RUN.get() + 1);
    let info = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF32,
        AlphaType::Premul,
        Some(space.clone()),
    );
    let mut surface = started::raster(&info, None, None)?;
    let mut paint = paint.clone();
    paint
        .set_blend_mode(BlendMode::Src)
        .set_anti_alias(false)
        .set_style(PaintStyle::Fill)
        .set_path_effect(None)
        .set_mask_filter(None)
        .set_image_filter(None);
    surface.canvas().draw_paint(&paint);
    let mut pixel = [0u8; 16];
    surface
        .read_pixels(&info, &mut pixel, info.min_row_bytes(), (0, 0))
        .then_some(())?;
    let (channels, _) = pixel.as_chunks::<4>();
    Some(std::array::from_fn(|c| f32::from_le_bytes(channels[c])))
}

/// `premul`, a colour premultiplied in `space`, as a shader: a 1x1 image in
/// that space, repeated, which a page in it draws by the identity.
pub(crate) fn premul_shader(
    premul: [f32; 4],
    space: &ColorSpace,
) -> Option<Shader> {
    let info = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF32,
        AlphaType::Premul,
        Some(space.clone()),
    );
    let pixel: Vec<u8> = premul.iter().flat_map(|c| c.to_le_bytes()).collect();
    started::raster_from_data(
        &info,
        Data::new_copy(&pixel),
        info.min_row_bytes(),
    )?
    .to_shader(
        (TileMode::Repeat, TileMode::Repeat),
        SamplingOptions::default(),
        None,
    )
}

/// `premul` as a colour filter replacing whatever it is handed -- see
/// [`CONSTANT_SKSL`].
pub(crate) fn constant_filter(premul: [f32; 4]) -> Option<ColorFilter> {
    let uniforms: Vec<u8> =
        premul.iter().flat_map(|c| c.to_ne_bytes()).collect();
    CONSTANT.with(|effect| {
        effect
            .as_ref()?
            .make_color_filter(Data::new_copy(&uniforms), None)
    })
}

#[cfg(test)]
mod tests {
    use super::{FOLD_OFF, FOLDS_RUN, LAST_KEY};
    use crate::prelude::*;

    /// The float pages that keep a recording for pixels, where a filter is
    /// folded.
    const PAGES: [(PixelColorSpace, PixelDepth); 4] = [
        (PixelColorSpace::DisplayP3, PixelDepth::F16),
        (PixelColorSpace::DisplayP3, PixelDepth::F32),
        (PixelColorSpace::Rec2020, PixelDepth::F32),
        (PixelColorSpace::SrgbLinear, PixelDepth::F16),
    ];

    /// The modes the filters blend in: channel by channel, non-separable,
    /// one that replaces the colour and one that keeps the destination's.
    const MODES: [crate::paint::BlendMode; 5] = [
        crate::paint::BlendMode::Multiply,
        crate::paint::BlendMode::Screen,
        crate::paint::BlendMode::Hue,
        crate::paint::BlendMode::Copy,
        crate::paint::BlendMode::DestinationIn,
    ];

    /// Colours to draw and to filter with: opaque, translucent, faint
    /// enough for `Faint::drawable` to step in, and outside sRGB.
    fn colours() -> [RgbaLinear; 4] {
        [
            RgbaLinear::from_srgb8(230, 26, 102, 1.0),
            RgbaLinear::from_srgb8(77, 179, 51, 0.3),
            RgbaLinear::from_srgb8(20, 120, 220, 0.001),
            RgbaLinear::from_css("color(display-p3 0.1 0.85 0.3 / 0.65)")
                .expect("a colour"),
        ]
    }

    const OSWALD: &str =
        "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

    /// Every byte of a page in `space` and `depth` after a circle, whose
    /// edge is partly covered, and text, both in `colour` through a blend
    /// filter in `filter` and `mode`. `None` where Skia builds no filter
    /// for a blend that changes nothing, which `ColorFilter::blend` refuses.
    fn drawn(
        (space, depth): (PixelColorSpace, PixelDepth),
        colour: RgbaLinear,
        filter: RgbaLinear,
        mode: crate::paint::BlendMode,
    ) -> Option<Vec<u8>> {
        let mut canvas = Canvas::with_options(
            48.0,
            32.0,
            CanvasOptions {
                color_type: depth,
                color_space: space,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        let filter = ColorFilter::blend(filter, mode).ok()?;
        ctx.set_color_filter(Some(&filter));
        ctx.set_fill_style(colour);
        ctx.begin_path();
        ctx.arc(12.0, 12.0, 9.3, 0.0, std::f32::consts::TAU, false)
            .expect("an arc");
        ctx.fill(FillRule::NonZero);
        assert!(ctx.set_font_css("14px FoldOswald"), "a font");
        ctx.fill_text("Hm", 24.0, 26.0, None);
        Some(
            canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("the pixels"),
        )
    }

    /// `draw` with the fold off: the per-pixel route.
    fn unfolded<T>(draw: impl FnOnce() -> T) -> T {
        FOLD_OFF.set(true);
        let drawn = draw();
        FOLD_OFF.set(false);
        drawn
    }

    /// A filter folded into a flat colour draws what the filter draws over
    /// it pixel by pixel, to the bit, on F16 and F32 pages: at a circle's
    /// partly covered edge and in a glyph, where Skia applies the filter
    /// before coverage, and at an alpha too faint for eight bits.
    #[test]
    fn a_folded_filter_draws_what_the_filter_draws() {
        FontLibrary::new()
            .use_paths(Some("FoldOswald"), [OSWALD])
            .expect("Oswald");
        let folds_before = FOLDS_RUN.get();
        let (mut missed, mut compared) = (Vec::new(), 0);
        for page in PAGES {
            for mode in MODES {
                for colour in colours() {
                    for filter in colours() {
                        let Some(folded) = drawn(page, colour, filter, mode)
                        else {
                            continue;
                        };
                        compared += 1;
                        let plain =
                            unfolded(|| drawn(page, colour, filter, mode));
                        if Some(folded) != plain {
                            missed.push(format!(
                                "{page:?} {mode:?} {colour:?} through {filter:?}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(compared > PAGES.len() * MODES.len(), "{compared} compared");
        assert!(FOLDS_RUN.get() > folds_before, "nothing was folded");
        assert!(
            missed.is_empty(),
            "{} missed, first {}",
            missed.len(),
            missed[0]
        );
    }

    /// A rectangle in `colour` through `filter` on a Display P3 F32 page, as
    /// many times as `colours` names colours, each in turn.
    fn rects(colours: &[RgbaLinear], filter: &ColorFilter) -> Canvas {
        let mut canvas = Canvas::with_options(
            8.0,
            8.0,
            CanvasOptions {
                color_type: PixelDepth::F32,
                color_space: PixelColorSpace::DisplayP3,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_color_filter(Some(filter));
        for &colour in colours {
            ctx.set_fill_style(colour);
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        canvas
    }

    /// A filter drawn a hundred times over one colour onto a page is folded
    /// once, and over two colours in turn, twice: the fold is kept for each.
    #[test]
    fn a_filter_is_folded_once_for_each_colour_it_filters() {
        let filter = ColorFilter::blend(
            RgbaLinear::from_srgb8(200, 30, 60, 0.7),
            crate::paint::BlendMode::Multiply,
        )
        .expect("a filter");
        let [a, b, ..] = colours();
        let before = FOLDS_RUN.get();
        rects(&[a; 100], &filter);
        assert_eq!(FOLDS_RUN.get() - before, 1, "one colour");
        let before = FOLDS_RUN.get();
        let alternating: Vec<_> = (0..100).map(|i| [a, b][i % 2]).collect();
        rects(
            &alternating,
            &ColorFilter::blend(
                RgbaLinear::from_srgb8(200, 30, 60, 0.7),
                crate::paint::BlendMode::Screen,
            )
            .expect("a filter"),
        );
        assert_eq!(FOLDS_RUN.get() - before, 2, "two colours in turn");
    }

    /// The bytes of a 24x16 page in `space` after a rectangle in `colour`
    /// at `alpha` through `filter`, then `text` written over it in the same.
    fn page(
        space: PixelColorSpace,
        colour: &str,
        alpha: f32,
        filter: &ColorFilter,
        text: bool,
    ) -> Vec<u8> {
        let mut canvas = Canvas::with_options(
            24.0,
            16.0,
            CanvasOptions {
                color_type: PixelDepth::F32,
                color_space: space,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_color_filter(Some(filter));
        ctx.set_fill_style(RgbaLinear::from_css(colour).expect("a colour"));
        ctx.set_global_alpha(alpha);
        ctx.fill_rect(0.0, 0.0, 12.0, 16.0);
        if text {
            assert!(ctx.set_font_css("14px FoldOswald"), "a font");
            ctx.fill_text("Hm", 6.0, 13.0, None);
        }
        canvas
            .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
            .expect("the pixels")
    }

    /// Draws that differ in one thing a fold depends on -- the page's
    /// space, with the same components in it; the paint's alpha; the
    /// filter; a fill against text -- each take a fold of their own, drawn
    /// in turn on one thread: each is folded, and draws what it draws
    /// unfolded.
    #[test]
    fn a_fold_is_kept_apart_by_everything_it_depends_on() {
        FontLibrary::new()
            .use_paths(Some("FoldOswald"), [OSWALD])
            .expect("Oswald");
        let blend = |mode| {
            ColorFilter::blend(RgbaLinear::from_srgb8(200, 30, 60, 0.7), mode)
                .expect("a filter")
        };
        let multiply = blend(crate::paint::BlendMode::Multiply);
        let screen = blend(crate::paint::BlendMode::Screen);
        let (p3, rec2020) = (
            "color(display-p3 0.25 0.5 0.75)",
            "color(rec2020 0.25 0.5 0.75)",
        );
        let draws = [
            (PixelColorSpace::DisplayP3, p3, 1.0, &multiply, false),
            (PixelColorSpace::Rec2020, rec2020, 1.0, &multiply, false),
            (PixelColorSpace::DisplayP3, p3, 0.5, &multiply, false),
            (PixelColorSpace::DisplayP3, p3, 1.0, &screen, false),
            (PixelColorSpace::DisplayP3, p3, 1.0, &multiply, true),
        ];
        for (i, &(space, colour, alpha, filter, text)) in
            draws.iter().enumerate()
        {
            let before = FOLDS_RUN.get();
            let folded = page(space, colour, alpha, filter, text);
            // Each fill differs from every one before it but the last, whose
            // fill is the first's, kept, and whose text folds once.
            assert_eq!(FOLDS_RUN.get() - before, 1, "draw {i} folds once");
            let plain = unfolded(|| page(space, colour, alpha, filter, text));
            assert!(folded == plain, "draw {i} draws what it draws unfolded");
        }
    }

    /// Two colours whose folds share a slot each draw their own, drawn in
    /// turn: the slot is taken by whichever came last, and the whole key is
    /// compared, so neither is taken for the other. Four draws alternating
    /// the two fold four times, which they would not unless each evicted
    /// the other.
    #[test]
    fn colours_sharing_a_slot_each_take_their_own_fold() {
        let filter = ColorFilter::blend(
            RgbaLinear::from_srgb8(10, 200, 90, 0.9),
            crate::paint::BlendMode::Multiply,
        )
        .expect("a filter");
        let colour = |green: u32| {
            RgbaLinear::from_css(&format!(
                "rgb(64 {} 128)",
                green as f32 / 256.0
            ))
            .expect("a colour")
        };
        // The slot of the key the page builds for `green`, drawn with the
        // fold off so that the search keeps nothing.
        let slot = |green| {
            unfolded(|| rects(&[colour(green)], &filter));
            LAST_KEY.get().expect("a key").slot()
        };
        let first = slot(0);
        // 65536 greens, a 256th of a level apart, all miss 4096 slots
        // with odds of about e^-16; the search stops at the first that
        // does not, about 4096 draws in.
        let other = (1..1 << 16)
            .find(|&green| slot(green) == first)
            .expect("two keys in one slot");
        let order = [colour(0), colour(other), colour(0), colour(other)];
        let before = FOLDS_RUN.get();
        rects(&order, &filter);
        assert_eq!(FOLDS_RUN.get() - before, 4, "each evicts the other");
        let pixels = |canvas: &mut Canvas| {
            canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("the pixels")
        };
        for (i, &colour) in order.iter().enumerate() {
            let mut folded = rects(&order[..=i], &filter);
            let mut plain = unfolded(|| rects(&[colour], &filter));
            assert_eq!(pixels(&mut folded), pixels(&mut plain), "draw {i}");
        }
    }
}
