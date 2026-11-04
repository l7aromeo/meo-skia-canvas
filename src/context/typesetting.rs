//! What a context's text is set in, and the typography built from it, kept
//! until one of its settings changes.
//!
//! Building the typography clones the context's text and paragraph styles,
//! works the spacings out against the font, and asks Skia for the font's
//! metrics to place the baseline -- on every `fillText`, `strokeText` and
//! `measureText`, though the settings rarely change between two of them. So
//! it is built once and shared, and the settings it is built from are
//! reachable only through [`Typesetting`]: reading them is a plain borrow,
//! and changing one goes through [`Typesetting::edit`], which lets the kept
//! typography go. A setter cannot change a setting and leave a stale build
//! behind, because there is no other way in.
//!
//! The registered fonts are an input too: a spacing in `ch` or `ex` is
//! resolved against the face the font resolves to, which a registration can
//! change without any setting moving. The build therefore keeps the
//! `FontEpoch` it was made in, and one made in an earlier epoch is built
//! again.

use std::{cell::Cell, ops::Deref, rc::Rc};

use skia_safe::{
    FourByteTag,
    textlayout::{ParagraphStyle, TextStyle},
};

use crate::{
    context2d::{FontKerning, TextRendering},
    node::{
        font_library::FontEpoch,
        typography::{Baseline, DecorationStyle, Spacing},
    },
};

/// A context's typography as text is laid out with it: the run's text
/// style, the paragraph style, the decoration, the baseline, and whether
/// the run wraps.
pub(crate) type Typography =
    (TextStyle, ParagraphStyle, DecorationStyle, Baseline, bool);

/// Every context setting the typography is built from.
#[derive(Clone)]
pub struct TextSettings {
    pub char_style: TextStyle,
    pub graf_style: ParagraphStyle,
    pub text_baseline: Baseline,
    pub letter_spacing: Spacing,
    pub word_spacing: Spacing,
    pub text_decoration: DecorationStyle,
    pub text_wrap: bool,
    /// The font's line height in pixels, or `None` for the face's own
    /// spacing. Only wrapped text has lines to space.
    pub line_height: Option<f32>,
    pub font_hinting: bool,
    pub font_kerning: FontKerning,
    pub text_rendering: TextRendering,
    /// The font's variation axes, which instance the face a spacing in `ch`
    /// or `ex` is resolved against.
    pub variations: Vec<(FourByteTag, f32)>,
    /// The angle an `oblique` font is drawn at, in CSS degrees, or `None`
    /// where the font is not oblique. Not read by the typography itself,
    /// but kept with the settings it is built from so that what is laid out
    /// from both follows a change to either -- see
    /// `node::typography::LaidOutKey`.
    pub oblique_angle: Option<f32>,
    /// The font's weight where it is not a whole number, which the
    /// character style's `FontStyle` cannot hold, or `None`. Kept beside
    /// `oblique_angle` for the same reason -- see
    /// `node::typography::with_style_axes`.
    pub weight: Option<f32>,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// How many typographies this thread has built, for holding a setter
    /// that changes nothing to building none. Kept by tests and debug
    /// builds alone, which export it as `__typographyBuilds`.
    pub(crate) static BUILDS: Cell<usize> = const { Cell::new(0) };
}

/// A build of the typography, and the fonts it was built against.
#[derive(Clone)]
struct Kept {
    epoch: Rc<FontEpoch>,
    typography: Rc<Typography>,
}

/// [`TextSettings`], readable through `Deref`, with the typography last
/// built from them.
///
/// The build is held in a slot shared with every copy made since the
/// settings last changed -- see the `Clone` impl -- since equal settings
/// build the same typography. So a typography built inside a `save()` is
/// the one the outer state draws with too, and a run laid out under it is
/// kept for both. Were each copy to build its own, every draw inside a save
/// of its own would be a new typography, and nothing would be kept between
/// them. A copy
/// that changes a setting takes a slot of its own, leaving the others'.
///
/// There is no `DerefMut`: a change goes through [`edit`](Self::edit).
pub struct Typesetting {
    settings: TextSettings,
    kept: Rc<Cell<Option<Kept>>>,
}

impl Typesetting {
    pub fn new(settings: TextSettings) -> Self {
        Self {
            settings,
            kept: Rc::default(),
        }
    }

    /// The settings, to change; this copy lets go of the build it shared,
    /// which the others keep.
    pub fn edit(&mut self) -> &mut TextSettings {
        self.kept = Rc::default();
        &mut self.settings
    }

    /// Sets the setting `field` picks to `value`, letting whatever was built
    /// go only where that changes it: setting a value already held, as a
    /// drawing loop does before every draw, leaves the build kept.
    pub fn set<T: PartialEq>(
        &mut self,
        field: impl FnOnce(&mut TextSettings) -> &mut T,
        value: T,
    ) {
        let slot = field(&mut self.settings);
        if *slot != value {
            *slot = value;
            self.kept = Rc::default();
        }
    }

    /// The typography built from these settings in `epoch`: the one kept,
    /// where it was built in that epoch, and otherwise what `build` makes,
    /// kept for next time.
    pub(crate) fn typography(
        &self,
        epoch: Rc<FontEpoch>,
        build: impl FnOnce() -> Typography,
    ) -> Rc<Typography> {
        if let Some(kept) = self.kept.take()
            && Rc::ptr_eq(&kept.epoch, &epoch)
        {
            let typography = kept.typography.clone();
            self.kept.set(Some(kept));
            return typography;
        }
        #[cfg(any(test, debug_assertions))]
        BUILDS.set(BUILDS.get() + 1);
        let typography = Rc::new(build());
        self.kept.set(Some(Kept {
            epoch,
            typography: typography.clone(),
        }));
        typography
    }
}

impl Clone for Typesetting {
    /// A copy of the settings, sharing the slot the build is held in: a
    /// saved state is drawn with what the current one built or will build,
    /// and what it builds first is the current one's too.
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            kept: Rc::clone(&self.kept),
        }
    }
}

impl Deref for Typesetting {
    type Target = TextSettings;

    fn deref(&self) -> &TextSettings {
        &self.settings
    }
}

#[cfg(test)]
mod tests {
    use super::BUILDS;
    use crate::{
        Canvas, Context2D, FontKerning, FontLibrary as Library, FontStretch,
        TextAlign, TextBaseline, TextDirection, TextRendering,
    };

    const OSWALD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
    );

    /// The canvas's pixels after a line of text.
    fn drawn(canvas: &mut Canvas) -> Vec<u8> {
        let ctx = canvas.context();
        ctx.clear_rect(0.0, 0.0, 400.0, 120.0);
        ctx.fill_text("Hamburgefonstiv AV", 10.0, 70.0, None);
        ctx.get_image_data(0.0, 0.0, 400.0, 120.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec()
    }

    /// How many typographies the thread builds while `step` runs, each
    /// followed by a draw.
    fn builds(canvas: &mut Canvas, step: impl Fn(&mut Context2D)) -> usize {
        let before = BUILDS.get();
        step(canvas.context());
        drawn(canvas);
        BUILDS.get() - before
    }

    #[test]
    fn text_drawn_inside_saves_shares_one_typography() {
        // Each draw inside a save of its own: one typography is built and
        // one run kept for all of them, as for the same draws without the
        // saves -- not one of each a draw, which `restore()` threw away.
        use crate::node::typography::{forget_laid_out, laid_out_held};
        const FAMILY: &str = "SavedTypography";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("40px {FAMILY}")));
        let (before, kept) = (BUILDS.get(), laid_out_held().0);
        for i in 0..50 {
            ctx.save();
            ctx.set_fill_style_css(if i % 2 == 0 { "red" } else { "blue" })
                .expect("a colour");
            ctx.fill_text("Ag", (i % 20) as f32 * 15.3, 60.6, None);
            ctx.restore();
        }
        assert_eq!(BUILDS.get() - before, 1, "typographies built");
        assert_eq!(laid_out_held().0 - kept, 1, "runs kept");
    }

    #[test]
    fn a_setting_changed_inside_a_save_leaves_the_outer_typography() {
        // The saved state's build is the outer one's while their settings
        // agree, and its own once it changes one: the outer state draws in
        // its own font after `restore()`, building nothing more.
        const FAMILY: &str = "OuterTypography";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let outer = format!("40px {FAMILY}");
        let mut alone = Canvas::new(400.0, 120.0);
        assert!(alone.context().set_font_css(&outer));
        let want = drawn(&mut alone);
        // A font, which goes through `Typesetting::edit`, and a single
        // setting, which goes through `Typesetting::set`.
        type Change = fn(&mut Context2D);
        let changes: [(&str, Change); 2] = [
            ("font", |ctx| assert!(ctx.set_font_css("24px serif"))),
            ("letter spacing", |ctx| {
                ctx.set_letter_spacing_css("7px").expect("a spacing");
            }),
        ];
        for (name, change) in changes {
            let mut canvas = Canvas::new(400.0, 120.0);
            assert!(canvas.context().set_font_css(&outer));
            drawn(&mut canvas);
            let inner = builds(&mut canvas, |ctx| {
                ctx.save();
                change(ctx);
            });
            assert_eq!(inner, 1, "{name}: the changed setting builds its own");
            let restored = builds(&mut canvas, |ctx| ctx.restore());
            assert_eq!(restored, 0, "{name}: the outer typography was let go");
            assert_eq!(
                drawn(&mut canvas),
                want,
                "{name}: the outer state draws in its own settings"
            );
        }
    }

    #[test]
    fn a_setting_set_to_what_it_holds_builds_nothing() {
        // Each setter twice to the value it holds and once to another: the
        // repeats build no typography, and the change builds one.
        const FAMILY: &str = "UnchangedSettings";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let font = format!("40px {FAMILY}");
        let respelled = format!("40px   {FAMILY}");
        let other = format!("41px {FAMILY}");
        type Set = Box<dyn Fn(&mut Context2D)>;
        let setters: Vec<(&str, Set, Set)> = vec![
            (
                "font",
                Box::new(move |ctx| assert!(ctx.set_font_css(&font))),
                Box::new(move |ctx| assert!(ctx.set_font_css(&other))),
            ),
            (
                "font respelled",
                Box::new(move |ctx| assert!(ctx.set_font_css(&respelled))),
                Box::new(|ctx| assert!(ctx.set_font_css("39px serif"))),
            ),
            (
                "letter spacing",
                Box::new(|ctx| ctx.set_letter_spacing_css("2px").expect("s")),
                Box::new(|ctx| ctx.set_letter_spacing_css("3px").expect("s")),
            ),
            (
                "word spacing",
                Box::new(|ctx| ctx.set_word_spacing_css("4px").expect("s")),
                Box::new(|ctx| ctx.set_word_spacing_css("5px").expect("s")),
            ),
            (
                "decoration",
                Box::new(|ctx| {
                    ctx.set_text_decoration_css("underline").expect("d")
                }),
                Box::new(|ctx| {
                    ctx.set_text_decoration_css("overline").expect("d")
                }),
            ),
            (
                "align",
                Box::new(|ctx| ctx.set_text_align(TextAlign::Center)),
                Box::new(|ctx| ctx.set_text_align(TextAlign::Right)),
            ),
            (
                "baseline",
                Box::new(|ctx| ctx.set_text_baseline(TextBaseline::Middle)),
                Box::new(|ctx| ctx.set_text_baseline(TextBaseline::Top)),
            ),
            (
                "direction",
                Box::new(|ctx| ctx.set_direction(TextDirection::RightToLeft)),
                Box::new(|ctx| ctx.set_direction(TextDirection::LeftToRight)),
            ),
            (
                "kerning",
                Box::new(|ctx| ctx.set_font_kerning(FontKerning::None)),
                Box::new(|ctx| ctx.set_font_kerning(FontKerning::Normal)),
            ),
            (
                "rendering",
                Box::new(|ctx| {
                    ctx.set_text_rendering(TextRendering::OptimizeSpeed)
                }),
                Box::new(|ctx| ctx.set_text_rendering(TextRendering::Auto)),
            ),
            (
                "hinting",
                Box::new(|ctx| ctx.set_font_hinting(false)),
                Box::new(|ctx| ctx.set_font_hinting(true)),
            ),
            (
                "wrap",
                Box::new(|ctx| ctx.set_text_wrap(true)),
                Box::new(|ctx| ctx.set_text_wrap(false)),
            ),
            (
                "stretch",
                Box::new(|ctx| ctx.set_font_stretch(FontStretch::Condensed)),
                Box::new(|ctx| ctx.set_font_stretch(FontStretch::Expanded)),
            ),
        ];
        let mut canvas = Canvas::new(400.0, 120.0);
        for (name, same, changed) in &setters {
            builds(&mut canvas, same);
            assert_eq!(builds(&mut canvas, same), 0, "{name}: set again");
            assert_eq!(builds(&mut canvas, same), 0, "{name}: and again");
            assert_eq!(builds(&mut canvas, changed), 1, "{name}: changed");
        }
    }

    #[test]
    fn a_font_set_again_after_its_family_changes_is_drawn_in_the_new_face() {
        // The same string after a registration: the names are the same and
        // the face is not, so the font applies anew and draws as a fresh
        // context does.
        const FAMILY: &str = "UnchangedFontFace";
        let amstelvar = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/AmstelvarAlpha-VF.ttf"
        );
        let register = |file: &str| {
            Library::new()
                .register_font_from_path(FAMILY, file)
                .expect("a bundled face registers");
        };
        register(OSWALD);
        let font = format!("40px {FAMILY}");
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&font));
        let before = drawn(&mut canvas);
        register(amstelvar);
        assert!(canvas.context().set_font_css(&font));
        let after = drawn(&mut canvas);
        let mut fresh = Canvas::new(400.0, 120.0);
        assert!(fresh.context().set_font_css(&font));
        assert!(after != before, "the new face shows");
        assert!(after == drawn(&mut fresh), "as a fresh context draws it");
    }
}
