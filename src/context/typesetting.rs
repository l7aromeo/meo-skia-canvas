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
//!
//! Equal settings build the same typography, so a build is also found by
//! its settings, on this thread, from any context: a page drawn into a
//! fresh canvas each time finds the typography the last canvas built, and
//! with it every run kept under it -- see `node::typography::LaidOutKey`,
//! which tells typographies apart by identity. Without that, each canvas
//! laid out every string it drew twice before drawing it from what it kept.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Deref,
    rc::Rc,
};

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
///
/// Two equal settings build the same typography, which is what lets a build
/// be found by its settings -- see [`found_or_built`].
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

impl PartialEq for TextSettings {
    /// Every setting, and of the two Skia styles what Skia's own equality
    /// leaves out: `TextStyle::equals` does not compare the typeface, which
    /// `set_font` puts in the character style beside the families it was
    /// resolved from, and `ParagraphStyle`'s `==` leaves out the strut, the
    /// line limit and the hinting. Each is compared here, since a typography
    /// found for settings that differ in one would lay text out in the
    /// other's. Skia's styles are opaque handles that cannot be
    /// destructured, so what their equality leaves out is named from Skia's
    /// own `TextStyle::equals` and `ParagraphStyle::operator==`, in
    /// `modules/skparagraph`.
    fn eq(&self, other: &Self) -> bool {
        // Destructured whole, so that a setting added later is a compile
        // error here rather than one two contexts could differ in unseen.
        let Self {
            char_style,
            graf_style,
            text_baseline,
            letter_spacing,
            word_spacing,
            text_decoration,
            text_wrap,
            line_height,
            font_hinting,
            font_kerning,
            text_rendering,
            variations,
            oblique_angle,
            weight,
        } = self;
        let face = |style: &TextStyle| style.typeface().map(|f| f.unique_id());
        let theirs = &other.graf_style;
        *char_style == other.char_style
            && face(char_style) == face(&other.char_style)
            && graf_style == theirs
            && graf_style.strut_style() == theirs.strut_style()
            && graf_style.max_lines() == theirs.max_lines()
            && graf_style.hinting_is_on() == theirs.hinting_is_on()
            && graf_style.text_height_behavior()
                == theirs.text_height_behavior()
            && graf_style.apply_rounding_hack() == theirs.apply_rounding_hack()
            && *text_baseline == other.text_baseline
            && *letter_spacing == other.letter_spacing
            && *word_spacing == other.word_spacing
            && *text_decoration == other.text_decoration
            && *text_wrap == other.text_wrap
            && *line_height == other.line_height
            && *font_hinting == other.font_hinting
            && *font_kerning == other.font_kerning
            && *text_rendering == other.text_rendering
            && *variations == other.variations
            && *oblique_angle == other.oblique_angle
            && *weight == other.weight
    }
}

impl TextSettings {
    /// A hash of the settings cheap to read, for finding a build without
    /// comparing every setting against every one kept: equal settings hash
    /// alike, and two that collide are told apart by `==`.
    fn bucket(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        let style = self.char_style.font_style();
        self.char_style.font_size().to_bits().hash(&mut hasher);
        (*style.weight(), *style.width(), style.slant() as i32)
            .hash(&mut hasher);
        self.char_style
            .font_families()
            .iter()
            .for_each(|f| f.hash(&mut hasher));
        self.text_wrap.hash(&mut hasher);
        self.line_height.map(f32::to_bits).hash(&mut hasher);
        self.font_hinting.hash(&mut hasher);
        self.font_kerning.hash(&mut hasher);
        self.text_rendering.hash(&mut hasher);
        self.oblique_angle.map(f32::to_bits).hash(&mut hasher);
        self.weight.map(f32::to_bits).hash(&mut hasher);
        self.variations
            .iter()
            .for_each(|(tag, value)| (tag, value.to_bits()).hash(&mut hasher));
        hasher.finish()
    }
}

/// How many builds a thread can find by their settings. Each holds its
/// typography, a copy of its settings and the fonts it was built against,
/// about 2 KB in all beside fonts the library holds anyway; a page uses a
/// handful of text styles, and 64 is many pages' worth while staying a
/// short scan when a setting changes.
const FINDABLE_TYPOGRAPHIES: usize = 64;

/// A build this thread can find by its settings, held: a canvas drawing a
/// string once and dropped at once leaves nothing else holding it, and a
/// build held only while something drew with it was found again or not by
/// where the allocator put the next one -- see
/// `a_typography_is_found_whatever_the_allocator_does_with_its_address`.
/// The oldest goes past [`FINDABLE_TYPOGRAPHIES`], and all of them on a font
/// registration and on the idle release -- see [`forget_typographies`].
struct Findable {
    bucket: u64,
    settings: TextSettings,
    epoch: Rc<FontEpoch>,
    typography: Rc<Typography>,
}

thread_local! {
    /// The builds this thread can find by their settings, the most recently
    /// built or found last.
    static FINDABLE: RefCell<VecDeque<Findable>> = RefCell::default();
}

/// The typography for `settings` in `epoch`: one this thread built for
/// equal settings in that epoch, or else what `build` makes, findable from
/// then on.
fn found_or_built(
    settings: &TextSettings,
    epoch: &Rc<FontEpoch>,
    build: impl FnOnce() -> Typography,
) -> Rc<Typography> {
    let bucket = settings.bucket();
    let found = FINDABLE.with(|findable| {
        let mut findable = findable.borrow_mut();
        let at = findable.iter().position(|entry| {
            entry.bucket == bucket
                && Rc::ptr_eq(&entry.epoch, epoch)
                && entry.settings == *settings
        })?;
        let entry = findable.remove(at)?;
        let typography = Some(Rc::clone(&entry.typography));
        findable.push_back(entry);
        typography
    });
    if let Some(typography) = found {
        return typography;
    }
    // Built with the table free: building reads the registered fonts, and
    // nothing it reaches is held here.
    #[cfg(any(test, debug_assertions))]
    BUILDS.set(BUILDS.get() + 1);
    let typography = Rc::new(build());
    FINDABLE.with(|findable| {
        let mut findable = findable.borrow_mut();
        if findable.len() >= FINDABLE_TYPOGRAPHIES {
            findable.pop_front();
        }
        findable.push_back(Findable {
            bucket,
            settings: settings.clone(),
            epoch: Rc::clone(epoch),
            typography: Rc::clone(&typography),
        });
    });
    typography
}

/// Lets every findable build go, with the fonts they hold: called where the
/// kept runs go -- see `node::typography::forget_laid_out`.
pub(crate) fn forget_typographies() {
    let _ = FINDABLE.try_with(|findable| {
        if let Ok(mut findable) = findable.try_borrow_mut() {
            findable.clear();
        }
    });
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
    /// where it was built in that epoch; otherwise one this thread built for
    /// equal settings in that epoch, from any context -- see
    /// [`found_or_built`] -- or what `build` makes. Kept for next time.
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
        let typography = found_or_built(&self.settings, &epoch, build);
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
            // A change often returns to settings drawn earlier in this loop,
            // whose build is still held and would be found by them: let the
            // findable builds go first, so that what is counted is the kept
            // build letting go. Finding is tested apart, above.
            super::forget_typographies();
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

    /// What fresh canvases build while each sets `font` and draws `text`
    /// `times` times: the typographies, and the runs kept.
    fn across_canvases(
        font: &str,
        text: &str,
        canvases: usize,
    ) -> (usize, usize) {
        use crate::node::typography::laid_out_held;
        let (before, kept) = (BUILDS.get(), laid_out_held().0);
        for _ in 0..canvases {
            let mut canvas = Canvas::new(400.0, 120.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(font));
            for _ in 0..3 {
                ctx.fill_text(text, 0.0, 20.0, None);
            }
        }
        (BUILDS.get() - before, laid_out_held().0 - kept)
    }

    #[test]
    fn a_run_kept_on_one_canvas_is_found_on_the_next() {
        // A page drawn into a fresh canvas each time, as a renderer does:
        // the settings are the same, so the typography is, and the run kept
        // on the first canvas draws on the others.
        const FAMILY: &str = "InternedAcrossCanvases";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        crate::node::typography::forget_laid_out();
        let (built, kept) =
            across_canvases(&format!("15px {FAMILY}"), "row", 3);
        assert_eq!(built, 1, "typographies built");
        assert_eq!(kept, 1, "runs kept");
    }

    #[test]
    fn equal_settings_share_a_typography_and_others_build_their_own() {
        // Three settings, each on a fresh canvas drawn often enough for its
        // run to be kept, then each again: the first round builds one
        // apiece, and the second finds them, held by the runs kept.
        const FAMILY: &str = "InternedSettings";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let font = format!("15px {FAMILY}");
        let larger = format!("16px {FAMILY}");
        let settings: [(&str, &str); 3] =
            [(&font, "0px"), (&larger, "0px"), (&font, "2px")];
        let round = || {
            let before = BUILDS.get();
            for (font, spacing) in settings {
                let mut canvas = Canvas::new(400.0, 120.0);
                let ctx = canvas.context();
                assert!(ctx.set_font_css(font));
                ctx.set_letter_spacing_css(spacing).expect("a spacing");
                for _ in 0..3 {
                    ctx.fill_text("row", 0.0, 20.0, None);
                }
            }
            BUILDS.get() - before
        };
        assert_eq!(round(), 3, "each setting builds its own");
        assert_eq!(round(), 0, "and each is found again");
    }

    #[test]
    fn settings_skia_calls_equal_are_told_apart_where_they_lay_out_apart() {
        // Each pair differs where Skia's equality does not look, and is told
        // apart here, since a build found for one would lay the other out
        // in it. The second assertion is the control: were Skia to start
        // comparing one of these itself, it would fail, and the extra
        // comparison could go.
        use skia_safe::{Data, FontMgr, textlayout::StrutStyle};

        use super::TextSettings;
        use crate::context::State;

        let base: TextSettings = (*State::default().text).clone();
        assert!(base == base.clone(), "a copy is equal");
        let file = std::fs::read(OSWALD).expect("the Oswald test font");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&file), None)
            .expect("Oswald loads");
        let mut typeface = base.clone();
        typeface.char_style.set_typeface(face);
        let mut limited = base.clone();
        limited.graf_style.set_max_lines(3);
        let mut strut = base.clone();
        let mut enabled = StrutStyle::new();
        enabled.set_strut_enabled(true);
        strut.graf_style.set_strut_style(enabled);
        let mut unhinted = base.clone();
        unhinted.graf_style.turn_hinting_off();
        for (what, other) in [
            ("typeface", typeface),
            ("line limit", limited),
            ("strut", strut),
            ("hinting", unhinted),
        ] {
            assert!(base != other, "{what}: told apart");
            assert!(
                base.char_style == other.char_style
                    && base.graf_style == other.graf_style,
                "{what}: Skia's equality tells these apart itself"
            );
        }
    }

    #[test]
    fn a_build_is_found_only_in_the_epoch_it_was_built_in() {
        // Every registration empties the table as it bumps the epoch, so
        // this holds the epoch check to its own account: a build made in an
        // epoch that has since passed, filed after the bump, is not what
        // the current epoch's settings find.
        use std::rc::Rc;

        use super::found_or_built;
        use crate::{context::State, node::font_library::FontLibrary};

        const FAMILY: &str = "InternedEpochCheck";
        let state = State::default();
        let epoch = || FontLibrary::with_shared(|library| library.epoch());
        let passed = epoch();
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let current = epoch();
        assert!(!Rc::ptr_eq(&passed, &current), "the registration bumps it");
        let before = BUILDS.get();
        let old =
            found_or_built(&state.text, &passed, || state.build_typography());
        let new =
            found_or_built(&state.text, &current, || state.build_typography());
        assert_eq!(BUILDS.get() - before, 2, "one build in each epoch");
        assert!(!Rc::ptr_eq(&old, &new), "the passed epoch's is not found");
    }

    #[test]
    fn a_typography_is_let_go_with_the_fonts() {
        // Found by the next canvas when nothing drew with it in between,
        // and let go where the fonts change and on the idle release, both
        // of which forget the kept runs.
        const FAMILY: &str = "InternedLetGo";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let font = format!("15px {FAMILY}");
        let once = || {
            let before = BUILDS.get();
            let mut canvas = Canvas::new(400.0, 120.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&font));
            ctx.fill_text("once", 0.0, 20.0, None);
            BUILDS.get() - before
        };
        assert_eq!(once(), 1, "the first builds");
        assert_eq!(once(), 0, "the next finds it, nothing having held it");
        crate::node::typography::forget_laid_out();
        assert_eq!(once(), 1, "let go with the kept runs, it is built again");
    }

    /// The typographies fresh canvases build while each draws `text` once
    /// and is dropped at once, with `between` run after each canvas goes.
    fn drawn_once_on_fresh_canvases(
        font: &str,
        canvases: usize,
        between: impl Fn(),
    ) -> usize {
        let before = BUILDS.get();
        for _ in 0..canvases {
            let mut canvas = Canvas::new(400.0, 120.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(font));
            ctx.fill_text("drawn once", 0.0, 20.0, None);
            drop(canvas);
            between();
        }
        BUILDS.get() - before
    }

    #[test]
    fn a_typography_is_found_whatever_the_allocator_does_with_its_address() {
        // Canvases that each draw a string once and are dropped at once, as
        // a server drawing a response per canvas does: the typography the
        // first built is found by the others. With blocks the size of its
        // allocation taken between the canvases, so that the next one's
        // cannot be put where the last one's was, it is found all the same:
        // finding a build must not rest on where the allocator puts it.
        use std::alloc::{Layout, alloc, dealloc};

        use super::Typography;
        use crate::node::typography::forget_laid_out;

        const FAMILY: &str = "InternedAddress";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let font = format!("15px {FAMILY}");
        // An `Rc`'s allocation: its two counts beside the value.
        let layout = Layout::new::<(usize, usize, Typography)>();
        let held = std::cell::RefCell::new(Vec::new());
        let block = || {
            for _ in 0..64 {
                // SAFETY: the layout is a real type's, so not zero-sized,
                // and each block is freed below with the same layout.
                let at = unsafe { alloc(layout) };
                assert!(!at.is_null(), "a block is allocated");
                held.borrow_mut().push(at);
            }
        };
        forget_laid_out();
        let as_it_falls = drawn_once_on_fresh_canvases(&font, 3, || {});
        forget_laid_out();
        let blocked = drawn_once_on_fresh_canvases(&font, 3, block);
        for at in held.take() {
            // SAFETY: allocated above with this layout, freed once.
            unsafe { dealloc(at, layout) };
        }
        assert_eq!(
            (as_it_falls, blocked),
            (1, 1),
            "typographies built: addresses as they fall, the freed one taken"
        );
    }

    #[test]
    fn past_the_count_the_oldest_typography_goes() {
        // Held, the builds are bounded: one past the count lets the oldest
        // go, which is built again, while the newest is still found.
        use super::FINDABLE_TYPOGRAPHIES;
        use crate::node::typography::forget_laid_out;

        const FAMILY: &str = "InternedCount";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let drawn = |px: usize| {
            let before = BUILDS.get();
            let mut canvas = Canvas::new(400.0, 120.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&format!("{px}px {FAMILY}")));
            ctx.fill_text("counted", 0.0, 20.0, None);
            BUILDS.get() - before
        };
        forget_laid_out();
        let sizes = 10..10 + FINDABLE_TYPOGRAPHIES + 1;
        let built: usize = sizes.clone().map(drawn).sum();
        assert_eq!(built, FINDABLE_TYPOGRAPHIES + 1, "each size builds once");
        assert_eq!(drawn(sizes.end - 1), 0, "the newest is found");
        assert_eq!(drawn(sizes.start), 1, "the oldest has gone");
    }

    #[test]
    fn a_font_registered_since_builds_the_typography_again() {
        // The registered fonts are an input: a typography shared before a
        // registration is not found after it.
        const FAMILY: &str = "InternedEpoch";
        let library = Library::new();
        library
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let font = format!("15px {FAMILY}");
        assert_eq!(across_canvases(&font, "row", 1).0, 1, "the first builds");
        assert_eq!(across_canvases(&font, "row", 1).0, 0, "the next finds it");
        library
            .register_font_from_path("InternedEpochOther", OSWALD)
            .expect("a bundled face registers");
        assert_eq!(across_canvases(&font, "row", 1).0, 1, "built again");
    }
}
