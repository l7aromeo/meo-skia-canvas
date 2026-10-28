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
        }));
        typography
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
