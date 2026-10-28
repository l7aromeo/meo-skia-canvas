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
//! So does the family list the build's style looks its faces up by, filed
//! with the library so that a lookup compares a reference rather than
//! hashing the family names -- see `FontLibrary::canvas_list`. It is kept
//! beside the build rather than in it, because the build's identity keys
//! the runs kept for drawing, and a list filed again must not let them go.

use std::{cell::Cell, ops::Deref, rc::Rc};

use skia_safe::{
    FourByteTag,
    textlayout::{ParagraphStyle, TextStyle},
};

use crate::{
    context2d::{FontKerning, TextRendering},
    font::FontFamily,
    node::{
        family_lists::{FiledList, ListRef},
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
    /// The font's families as the font was parsed, which the style's family
    /// names were resolved from, or `None` before a font is set, when the
    /// style carries Skia's default family and its faces are looked up by
    /// that name.
    pub families: Option<Vec<FontFamily>>,
}

/// A build of the typography, and the fonts it was built against.
#[derive(Clone)]
struct Kept {
    epoch: Rc<FontEpoch>,
    typography: Rc<Typography>,
    /// The list the typography's style looks its faces up by, filed the
    /// first time it was asked for -- see [`Typesetting::filed_list`].
    filed: Option<FiledList>,
}

/// [`TextSettings`], readable through `Deref`, with the typography last
/// built from them.
///
/// There is no `DerefMut`: a change goes through [`edit`](Self::edit).
pub struct Typesetting {
    settings: TextSettings,
    kept: Cell<Option<Kept>>,
}

impl Typesetting {
    pub fn new(settings: TextSettings) -> Self {
        Self {
            settings,
            kept: Cell::new(None),
        }
    }

    /// The settings, to change; whatever was built from them goes.
    pub fn edit(&mut self) -> &mut TextSettings {
        *self.kept.get_mut() = None;
        &mut self.settings
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
        let typography = Rc::new(build());
        self.kept.set(Some(Kept {
            epoch,
            typography: typography.clone(),
            filed: None,
        }));
        typography
    }

    /// The family list the kept typography's style looks its faces up by:
    /// the one filed for it, where `is_current` says the library still
    /// holds it, and otherwise what `file` files for the families now -- or
    /// `None`, before a font is set or a typography built.
    ///
    /// A list goes stale with the same fonts registered only when the
    /// library's arena reaches its bound, and the families then resolve to
    /// the names the style already carries, so the style is left as it is.
    /// A registration changes the fonts' epoch instead, and the typography
    /// built for the new one files a list of its own.
    pub(crate) fn filed_list(
        &self,
        is_current: impl FnOnce(ListRef) -> bool,
        file: impl FnOnce(&[FontFamily]) -> FiledList,
    ) -> Option<FiledList> {
        let families = self.settings.families.as_deref()?;
        let mut kept = self.kept.take()?;
        let filed = match kept.filed.take() {
            Some(filed) if is_current(filed.list) => filed,
            _ => file(families),
        };
        kept.filed = Some(filed.clone());
        self.kept.set(Some(kept));
        Some(filed)
    }
}

impl Clone for Typesetting {
    /// A copy of the settings, sharing the build: a saved state is drawn
    /// with what the current one would have built.
    fn clone(&self) -> Self {
        let kept = self.kept.take();
        self.kept.set(kept.clone());
        Self {
            settings: self.settings.clone(),
            kept: Cell::new(kept),
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
    use crate::{
        Canvas, Context2D, FontLibrary as Library,
        font::FontFamily,
        node::font_library::{FontLibrary, MATCHES_KEPT},
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

    /// Whether the list `ctx`'s typography was last drawn with is one the
    /// library still holds, or `None` where it has filed none. Read off the
    /// kept slot, so that it does not go through what it checks.
    fn current(ctx: &Context2D) -> Option<bool> {
        let text = &ctx.inner.state.text;
        let kept = text.kept.take();
        let answer =
            kept.as_ref()
                .and_then(|kept| kept.filed.as_ref())
                .map(|filed| {
                    FontLibrary::with_shared(|lib| lib.is_current(filed.list))
                });
        text.kept.set(kept);
        answer
    }

    #[test]
    fn a_context_whose_font_is_set_files_its_family_list() {
        const FAMILY: &str = "FiledListSet";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let mut unset = Canvas::new(400.0, 120.0);
        drawn(&mut unset);
        assert_eq!(current(unset.context()), None, "none before a font is set");
        let mut set = Canvas::new(400.0, 120.0);
        assert!(set.context().set_font_css(&format!("40px {FAMILY}")));
        drawn(&mut set);
        assert_eq!(current(set.context()), Some(true), "filed once it is");
    }

    #[test]
    fn a_list_gone_stale_at_the_bound_is_filed_again_and_draws_the_same() {
        const FAMILY: &str = "FiledListStale";
        Library::new()
            .register_font_from_path(FAMILY, OSWALD)
            .expect("a bundled face registers");
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&format!("40px {FAMILY}")));
        let before = drawn(&mut canvas);
        assert_eq!(current(canvas.context()), Some(true));
        FontLibrary::with_shared(|lib| {
            for at in 0..=MATCHES_KEPT {
                let flood = format!("FiledListFlood{at}");
                lib.canvas_list(&[FontFamily::from(flood.as_str())]);
            }
        });
        assert_eq!(current(canvas.context()), Some(false), "past the bound");
        let after = drawn(&mut canvas);
        assert_eq!(current(canvas.context()), Some(true), "filed again");
        assert!(after == before, "and drawn the same");
    }
}
