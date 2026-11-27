//! Which face of a family a `TypefaceFontProvider` hands back for a style,
//! and the names that make it hand back the face the family's slant calls
//! for.
//!
//! The provider matches through `SkFontStyleSet::matchStyleCSS3`
//! (src/core/SkFontMgr.cpp), which scores a face on its width, its slant and
//! its weight and keeps the first face with the highest score. Each part is
//! shifted eight bits above the next, but the weight's part reaches 1000, so
//! it can outrank the slant's: in a family of an upright 400 face and an
//! italic 900 one, `italic` at weight 400 scores the upright face higher and
//! draws it, skewed. CSS Fonts 4 section 5.2 narrows a family's faces by
//! `font-style` before it looks at `font-weight`.
//!
//! So a family whose faces fall into more than one group of width and slant
//! is also registered a group at a time, each group under a
//! [`group_alias`]. Within a group only the weight differs, so the matcher
//! picks among them by weight alone. A family is asked for by the alias of
//! the group that comparing Skia's three scores in order -- width, then
//! slant, then weight -- puts first, wherever that face is not the one
//! Skia's own sum puts first, and by its own name everywhere else. The same
//! three scores decide both: which face wins within a group, and every
//! tie, are Skia's.
//!
//! The family is replaced rather than preceded by the alias. SkParagraph
//! takes one face from each family a style names and falls back from it to
//! the next family's, so a family left behind its alias would answer a
//! glyph its chosen face lacks from another of its faces.
//!
//! All of this is for a Skia without
//! `containers/skia-font-match-slant-first.patch`, which is what a crate
//! build naming no patched archives links. A build of this tree's patched
//! Skia registers no group -- see [`Filing::finish`] -- because there the
//! matcher compares the slant first itself, and the groups would only move
//! the two cases they cannot match.

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test the_scores_pick_the_face_skias_provider_picks
// on a build of crates.io's Skia, where it holds the provider to Skia's own
// sum. While it passes there the weight still outranks the slant; when it
// fails because the provider took the face the slant-first comparison
// names, the groups can go. They stand in for the patch everywhere but two
// places, which is why the patch stays: a family sent to its group reaches
// SkParagraph's character fallback, `defaultFallback`, as the group's
// private name, where it hands the first family to the platform's
// `matchFamilyStyleCharacter` as a hint, so a glyph the group lacks is not
// sought near a system family of the family's own name; and a list naming
// no family at all falls back to the collection's default families, which
// the groups leave alone, since naming one would give that hint where the
// list gave none.

use std::rc::Rc;

use skia_safe::{
    FontStyle, Typeface,
    font_style::{Slant, Weight, Width},
    textlayout::{TextStyle, TypefaceFontProvider},
};

use crate::cache_map::{CacheMap, CacheSet};

use super::{PRIVATE_ALIAS_PREFIX, private_alias, slant_index};

/// What every [`group_alias`] begins with after the private alias prefix.
const GROUP_ALIAS_LEAD: &str = "faces at width ";

/// The width score's base in `matchStyleCSS3`: a face of the width asked for
/// scores this, and each step away from it costs one.
const WIDTH_SCORE_BASE: i32 = 10;

/// The weight score of a face of the weight asked for in `matchStyleCSS3`,
/// which the comment there calls "the 'heaviest' recognized weight".
const WEIGHT_SCORE_BASE: i32 = 1000;

/// How far `matchStyleCSS3` shifts the score it has before adding the next
/// part: the width's before the slant's, then both before the weight's.
const SKIA_SCORE_SHIFT: u32 = 8;

/// The slant score `matchStyleCSS3` gives a face, by the slant asked for and
/// then the face's, each in [`slant_index`]'s order: upright, italic,
/// oblique. Its own table, `score[pattern.slant()][current.slant()]`.
const SLANT_SCORES: [[i32; 3]; 3] = [[3, 1, 2], [1, 3, 2], [1, 2, 3]];

/// The three parts of the score `matchStyleCSS3` gives a face reporting
/// `face` for a request for `asked`: its width's, its slant's and its
/// weight's, in the order they are added.
fn score_parts(asked: FontStyle, face: FontStyle) -> [i32; 3] {
    let (asked_width, face_width) = (*asked.width(), *face.width());
    let width = match asked_width <= *Width::NORMAL {
        true if face_width <= asked_width => {
            WIDTH_SCORE_BASE - asked_width + face_width
        }
        true => WIDTH_SCORE_BASE - face_width,
        false if face_width > asked_width => {
            WIDTH_SCORE_BASE + asked_width - face_width
        }
        false => face_width,
    };
    let slant =
        SLANT_SCORES[slant_index(asked.slant())][slant_index(face.slant())];
    let (asked_weight, face_weight) = (*asked.weight(), *face.weight());
    let weight = if asked_weight == face_weight {
        WEIGHT_SCORE_BASE
    } else if asked_weight < *Weight::NORMAL {
        // Lighter weights first.
        match face_weight <= asked_weight {
            true => WEIGHT_SCORE_BASE - asked_weight + face_weight,
            false => WEIGHT_SCORE_BASE - face_weight,
        }
    } else if asked_weight <= *Weight::MEDIUM {
        // Heavier up to 500, then lighter, then heavier still.
        if face_weight >= asked_weight && face_weight <= *Weight::MEDIUM {
            WEIGHT_SCORE_BASE + asked_weight - face_weight
        } else if face_weight <= asked_weight {
            WEIGHT_SCORE_BASE / 2 + face_weight
        } else {
            WEIGHT_SCORE_BASE - face_weight
        }
    } else {
        // Heavier weights first.
        match face_weight > asked_weight {
            true => WEIGHT_SCORE_BASE + asked_weight - face_weight,
            false => face_weight,
        }
    };
    [width, slant, weight]
}

/// The score `matchStyleCSS3` gives `parts`: each shifted
/// [`SKIA_SCORE_SHIFT`] bits above the next and added.
fn skia_score([width, slant, weight]: [i32; 3]) -> i32 {
    (((width << SKIA_SCORE_SHIFT) + slant) << SKIA_SCORE_SHIFT) + weight
}

/// The index of the first of `faces` with the highest `score` for `asked`,
/// as `matchStyleCSS3` keeps the first face of any it scores alike; `None`
/// for no faces.
fn first_best<K: Ord>(
    faces: &[FontStyle],
    asked: FontStyle,
    score: impl Fn([i32; 3]) -> K,
) -> Option<usize> {
    faces
        .iter()
        .map(|face| score(score_parts(asked, *face)))
        .enumerate()
        .fold(None, |best: Option<(usize, K)>, (index, key)| match best {
            Some((first, held)) if held >= key => Some((first, held)),
            _ => Some((index, key)),
        })
        .map(|(index, _)| index)
}

/// The face of `faces` a `TypefaceFontProvider` hands back for `asked`.
fn skia_pick(faces: &[FontStyle], asked: FontStyle) -> Option<usize> {
    first_best(faces, asked, skia_score)
}

/// The face of `faces` whose width, then slant, then weight scores best for
/// `asked`: the face [`StyleGroups`] has a family asked for.
fn style_first_pick(faces: &[FontStyle], asked: FontStyle) -> Option<usize> {
    first_best(faces, asked, |parts| parts)
}

/// Whether two styles fall in the same group: the same width and slant.
fn same_group(a: FontStyle, b: FontStyle) -> bool {
    a.width() == b.width() && a.slant() == b.slant()
}

/// The CSS keyword for `slant`, as a [`group_alias`] spells it.
fn slant_keyword(slant: Slant) -> &'static str {
    match slant {
        Slant::Upright => "normal",
        Slant::Italic => "italic",
        Slant::Oblique => "oblique",
    }
}

/// The name the faces of `family` reporting `style`'s width and slant are
/// registered under as well as `family`, and the name `family` is asked for
/// by where [`StyleGroups::names_for`] sends it to that group.
///
/// Private for the reasons on `oblique_alias`: no font names its family
/// with the prefix, and a caller's family starting with it is asked for by
/// `unmatched_name` unless one was registered under that name.
pub(crate) fn group_alias(family: &str, style: FontStyle) -> String {
    private_alias(&format!(
        "{GROUP_ALIAS_LEAD}{} and slant {}{GROUP_FAMILY_LEAD}{family}",
        *style.width(),
        slant_keyword(style.slant())
    ))
}

/// What comes between a [`group_alias`]'s width and slant and the family it
/// was made for. The width is a number and the slant a [`slant_keyword`],
/// so its first occurrence is the one before the family.
const GROUP_FAMILY_LEAD: &str = " of ";

/// The name the faces behind a [`group_alias`] are filed under -- a family,
/// or itself a private alias -- or `None` for a name that is not one.
pub(crate) fn family_of_group_alias(name: &str) -> Option<&str> {
    name.strip_prefix(PRIVATE_ALIAS_PREFIX)?
        .strip_prefix(GROUP_ALIAS_LEAD)?
        .split_once(GROUP_FAMILY_LEAD)
        .map(|(_, family)| family)
}

/// Whether `name` spells a [`group_alias`], which an SVG document is not let
/// name -- see `written_family` in `image`.
pub(crate) fn is_group_alias(name: &str) -> bool {
    name.strip_prefix(PRIVATE_ALIAS_PREFIX)
        .is_some_and(|rest| rest.starts_with(GROUP_ALIAS_LEAD))
}

/// The families one `TypefaceFontProvider` holds faces of more than one
/// group for, with the style each of their faces reports in the order the
/// provider holds them: what [`names_for`](Self::names_for) decides by.
#[derive(Default)]
pub(crate) struct StyleGroups {
    /// Each family of more than one group, by the name it is filed under.
    mixed: CacheMap<String, Rc<[FontStyle]>>,
    /// The family each [`group_alias`] registered belongs to.
    aliases: CacheMap<String, String>,
    /// Every name the provider files faces under, aliases aside.
    held: CacheSet<String>,
    /// The name the provider filed its first face under: the family
    /// `TypefaceFontProvider::onLegacyMakeTypeface` matches a style against
    /// where it holds no family of the name asked for.
    first: Option<String>,
}

impl StyleGroups {
    /// Registers in `provider` each face of every family `filed` holds faces
    /// of more than one group for under its [`group_alias`] too, and says
    /// what it registered.
    ///
    /// `filed` is every face `provider` holds with the name it holds it
    /// under, in the order they were registered, which is the order the
    /// matcher breaks ties in. A group's faces are registered in the same
    /// order, so they break ties among themselves as they did in the
    /// family.
    pub(crate) fn register(
        provider: &mut TypefaceFontProvider,
        filed: &[(Typeface, String)],
    ) -> Self {
        let mut families: Vec<(&str, Vec<(&Typeface, FontStyle)>)> = Vec::new();
        let mut places: CacheMap<&str, usize> = CacheMap::default();
        for (face, name) in filed {
            let place = *places.entry(name).or_insert_with(|| {
                families.push((name, Vec::new()));
                families.len() - 1
            });
            families[place].1.push((face, face.font_style()));
        }
        let mut groups = Self {
            first: filed.first().map(|(_, name)| name.clone()),
            ..Self::default()
        };
        for (name, faces) in families {
            groups.held.insert(name.to_string());
            let first = faces[0].1;
            if faces.iter().all(|(_, style)| same_group(*style, first)) {
                continue;
            }
            for (face, style) in &faces {
                let alias = group_alias(name, *style);
                provider
                    .register_typeface((*face).clone(), Some(alias.as_str()));
                groups.aliases.insert(alias, name.to_string());
            }
            let styles: Rc<[FontStyle]> =
                faces.iter().map(|(_, style)| *style).collect();
            groups.mixed.insert(name.to_string(), styles);
        }
        groups
    }

    /// Whether no family the provider holds has faces of more than one
    /// group, so that every name stands as it is.
    pub(crate) fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    /// The name to ask for the family `name` by in `style`, read back to its
    /// family first where `name` is one of its [`group_alias`]es: the alias
    /// of the group whose face scores best for `style` width first, slant
    /// next and weight last, where that face is not the one the provider's
    /// matcher picks, or the family's own name.
    fn name_for<'a>(&'a self, name: &'a str, style: FontStyle) -> NameFor<'a> {
        let family = self.aliases.get(name).map_or(name, String::as_str);
        let Some(faces) = self.mixed.get(family) else {
            return NameFor::Family(family);
        };
        match style_first_pick(faces, style) {
            Some(best) if skia_pick(faces, style) != Some(best) => {
                NameFor::Group(group_alias(family, faces[best]))
            }
            _ => NameFor::Family(family),
        }
    }

    /// The names a text style in `style` is to ask for `names` by, or `None`
    /// where every name stands as it is: each family of more than one group
    /// sent to the group CSS Fonts 4 section 5.2 narrows it to, and, given a
    /// `fallback`, its group appended to a list naming no family -- see
    /// [`fallback_for`](Self::fallback_for).
    ///
    /// Every caller goes through this or [`restyle`](Self::restyle), so the
    /// route has one place to change. The style is the one the collection is
    /// searched in, so a caller that changes a style's slant or weight after
    /// naming its families asks again: a name given here is read back to its
    /// family first.
    pub(crate) fn styled<S: AsRef<str>>(
        &self,
        names: &[S],
        style: FontStyle,
        fallback: Option<&Fallback>,
    ) -> Option<Vec<String>> {
        let mut renamed = self.names_for(names, style);
        let appended = fallback.and_then(|fallback| {
            self.fallback_for(names, style, fallback.family, fallback.resolves)
        });
        if let Some(alias) = appended {
            renamed
                .get_or_insert_with(|| {
                    names.iter().map(|name| name.as_ref().to_string()).collect()
                })
                .push(alias);
        }
        renamed
    }

    /// Sets `style`'s families to the names [`styled`](Self::styled) gives
    /// them in its own font style, where those differ.
    pub(crate) fn restyle(
        &self,
        style: &mut TextStyle,
        fallback: Option<&Fallback>,
    ) {
        if self.is_empty() {
            return;
        }
        let families: Vec<String> =
            style.font_families().iter().map(str::to_string).collect();
        if let Some(names) =
            self.styled(&families, style.font_style(), fallback)
        {
            style.set_font_families(&names);
        }
    }

    /// The names to ask for `names` by in `style` -- see
    /// [`name_for`](Self::name_for) -- or `None` where every name stands as
    /// it is.
    fn names_for<S: AsRef<str>>(
        &self,
        names: &[S],
        style: FontStyle,
    ) -> Option<Vec<String>> {
        if self.is_empty() {
            return None;
        }
        let renamed: Vec<NameFor> = names
            .iter()
            .map(|name| self.name_for(name.as_ref(), style))
            .collect();
        let changed = renamed
            .iter()
            .zip(names)
            .any(|(new, old)| new.as_str() != old.as_ref());
        changed.then(|| renamed.into_iter().map(NameFor::into_string).collect())
    }

    /// The [`group_alias`] to append to `names` in `style` where none of
    /// them names a family and `fallback` -- the first of the collection's
    /// default families -- is one [`names_for`](Self::names_for) would send
    /// to a group.
    ///
    /// SkParagraph asks for its default families, in the style asked for,
    /// only where no family a style names gives a face, and takes the first
    /// that does; so a list that names nothing is matched against
    /// `fallback`'s faces by the provider's matcher. `resolves` says
    /// whether a name the provider does not hold is one another of the
    /// collection's managers does. An empty list is left alone: appending a
    /// name to it would give the platform's character fallback a family to
    /// start from where it had none.
    fn fallback_for<S: AsRef<str>>(
        &self,
        names: &[S],
        style: FontStyle,
        fallback: &str,
        resolves: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let NameFor::Group(alias) = self.name_for(fallback, style) else {
            return None;
        };
        let named = names.iter().any(|name| {
            let name = name.as_ref();
            self.held.contains(name)
                || self.aliases.contains_key(name)
                || resolves(name)
        });
        (!names.is_empty() && !named).then_some(alias)
    }

    /// The name an SVG document is to name `name` by, for text set in
    /// `style`, where it differs from `name`.
    ///
    /// Skia's SVG text asks its provider for the family it is given by
    /// `legacyMakeTypeface`, which matches `style` against the faces filed
    /// under that name or, where the provider holds none, under the first
    /// name it filed a face under -- so a name the provider does not hold is
    /// written as that family's group, where it has one to send the style
    /// to. Otherwise as [`names_for`](Self::names_for).
    pub(crate) fn written_for(
        &self,
        name: &str,
        style: FontStyle,
    ) -> Option<String> {
        let family = self.aliases.get(name).map_or(name, String::as_str);
        if self.held.contains(family) {
            let written = self.name_for(family, style);
            return (written.as_str() != name).then(|| written.into_string());
        }
        match self.name_for(self.first.as_deref()?, style) {
            NameFor::Group(alias) => Some(alias),
            NameFor::Family(_) => None,
        }
    }

    /// The [`group_alias`] a face of `family` reporting `style` is
    /// registered under, where `family` has one: the name an instance of the
    /// face is to be found by too, since a style may ask for the family by
    /// it.
    pub(crate) fn alias_of(
        &self,
        family: &str,
        style: FontStyle,
    ) -> Option<String> {
        self.mixed
            .contains_key(family)
            .then(|| group_alias(family, style))
    }
}

/// A `TypefaceFontProvider` being filled, with every face it has been given
/// and the name it files the face under: what [`StyleGroups::register`]
/// reads once it is full.
pub(crate) struct Filing {
    provider: TypefaceFontProvider,
    filed: Vec<(Typeface, String)>,
}

impl Filing {
    /// An empty provider.
    pub(crate) fn new() -> Self {
        Self {
            provider: TypefaceFontProvider::new(),
            filed: Vec::new(),
        }
    }

    /// Registers `face` under `name`, or the family the face names without
    /// one, as `TypefaceFontProvider::register_typeface` does.
    pub(crate) fn register(&mut self, face: Typeface, name: Option<&str>) {
        let filed = name.map_or_else(|| face.family_name(), str::to_string);
        self.provider.register_typeface(face.clone(), name);
        self.filed.push((face, filed));
    }

    /// The provider, with every family of more than one group registered a
    /// group at a time too, and what was registered -- see
    /// [`StyleGroups::register`].
    ///
    /// On a Skia built from this tree's patched source the provider registers
    /// no group and the groups are empty, so that every name stands as it is
    /// and the patched build draws what it drew without them: there
    /// `containers/skia-font-match-slant-first.patch` has the matcher compare
    /// the slant before the weight itself. The groups cannot stand in for the
    /// patch everywhere, which is why the patch stays -- see the `UPSTREAM`
    /// note at the head of this module.
    pub(crate) fn finish(mut self) -> (TypefaceFontProvider, StyleGroups) {
        if cfg!(meo_skia_marker) {
            return (self.provider, StyleGroups::default());
        }
        let groups = StyleGroups::register(&mut self.provider, &self.filed);
        (self.provider, groups)
    }

    /// The provider as it stands, with no group registered.
    pub(crate) fn into_provider(self) -> TypefaceFontProvider {
        self.provider
    }
}

/// Where a family list naming no family the collection holds is matched:
/// what [`StyleGroups::styled`] appends that family's group for.
pub(crate) struct Fallback<'a> {
    /// The first of the collection's default families.
    pub(crate) family: &'a str,
    /// Whether a name the provider does not hold is one another of the
    /// collection's managers does, so that the list names a family after
    /// all.
    pub(crate) resolves: &'a dyn Fn(&str) -> bool,
}

/// What [`StyleGroups::name_for`] asks for a family by.
enum NameFor<'a> {
    /// The family's own name.
    Family(&'a str),
    /// One of its [`group_alias`]es.
    Group(String),
}

impl NameFor<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Family(name) => name,
            Self::Group(alias) => alias,
        }
    }

    fn into_string(self) -> String {
        match self {
            Self::Family(name) => name.to_string(),
            Self::Group(alias) => alias,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Fallback, Filing, StyleGroups, group_alias, is_group_alias, skia_pick,
        style_first_pick,
    };
    use skia_safe::{
        Data, FontArguments, FontMgr, FontStyle, FourByteTag, Typeface,
        font_arguments::{VariationPosition, variation_position::Coordinate},
        font_style::{Slant, Weight, Width},
        textlayout::TypefaceFontProvider,
    };

    const RALEWAY: &str =
        "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";
    const RALEWAY_ITALIC: &str =
        "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf";
    const RECURSIVE: &str = "tests/assets/fonts/Recursive";

    fn face(path: &str) -> Typeface {
        let bytes = std::fs::read(path).expect("a bundled font");
        FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("the font parses")
    }

    /// `path`, a variable font with a `wght` axis, instanced at `weight`:
    /// a face that reports that weight.
    fn at_weight(path: &str, weight: i32) -> Typeface {
        let coordinates = [Coordinate {
            axis: FourByteTag::from_chars('w', 'g', 'h', 't'),
            value: weight as f32,
        }];
        let arguments = FontArguments::new().set_variation_design_position(
            VariationPosition {
                coordinates: &coordinates,
            },
        );
        face(path)
            .clone_with_arguments(&arguments)
            .expect("Raleway instances")
    }

    fn recursive(file: &str) -> Typeface {
        face(&format!("{RECURSIVE}/Recursive-{file}-subset.ttf"))
    }

    fn style(weight: i32, slant: Slant) -> FontStyle {
        FontStyle::new(Weight::from(weight), Width::NORMAL, slant)
    }

    /// The groups `faces` make, filed under `family`, with the provider:
    /// made whatever Skia this build links, since the scores the groups
    /// decide by are this module's and not the provider's.
    fn grouped(
        family: &str,
        faces: &[&Typeface],
    ) -> (TypefaceFontProvider, StyleGroups) {
        let mut provider = TypefaceFontProvider::new();
        let filed: Vec<(Typeface, String)> = faces
            .iter()
            .map(|face| {
                provider.register_typeface((*face).clone(), Some(family));
                ((*face).clone(), family.to_string())
            })
            .collect();
        let groups = StyleGroups::register(&mut provider, &filed);
        (provider, groups)
    }

    /// Every way the scores here pick a face is a way Skia's provider picks
    /// one: the provider's answer is the face `skia_pick` names on a Skia
    /// scoring as upstream does, or the one `style_first_pick` names on one
    /// patched to compare the slant first -- the same one, case after case,
    /// and the case where the two part is met.
    ///
    /// Asserted against Skia itself rather than against a table of
    /// expected faces: a table written from the same reading of
    /// `matchStyleCSS3` as the scores would agree with them whatever that
    /// reading got wrong.
    #[test]
    fn the_scores_pick_the_face_skias_provider_picks() {
        let pool: Vec<Typeface> = [100, 300, 400, 500, 700, 900]
            .map(|weight| at_weight(RALEWAY, weight))
            .into_iter()
            .chain([100, 400, 700, 900].map(|w| at_weight(RALEWAY_ITALIC, w)))
            .collect();
        // The instances report the weights they were made at, which is what
        // makes them faces of those weights to the matcher.
        let reported: Vec<i32> = pool
            .iter()
            .map(|face| *face.font_style().weight())
            .collect();
        assert_eq!(
            reported,
            [100, 300, 400, 500, 700, 900, 100, 400, 700, 900]
        );
        assert!(
            pool[6..]
                .iter()
                .all(|f| f.font_style().slant() == Slant::Italic)
        );

        let mut sets: Vec<Vec<usize>> = Vec::new();
        for a in 0..pool.len() {
            for b in a + 1..pool.len() {
                sets.push(vec![a, b]);
                sets.push(vec![b, a]);
                for c in b + 1..pool.len() {
                    sets.push(vec![a, b, c]);
                    sets.push(vec![c, b, a]);
                }
            }
        }
        let weights = [0, 100, 250, 350, 400, 450, 500, 550, 600, 800, 1000];
        let widths =
            [Width::ULTRA_CONDENSED, Width::NORMAL, Width::SEMI_EXPANDED];
        let slants = [Slant::Upright, Slant::Italic, Slant::Oblique];
        let (mut as_skia, mut as_style_first, mut parted) = (0, 0, 0);
        for set in &sets {
            let mut provider = TypefaceFontProvider::new();
            for &index in set {
                provider.register_typeface(pool[index].clone(), Some("F"));
            }
            let styles: Vec<FontStyle> =
                set.iter().map(|&index| pool[index].font_style()).collect();
            for weight in weights {
                for width in widths {
                    for slant in slants {
                        let asked =
                            FontStyle::new(Weight::from(weight), width, slant);
                        let matched = provider
                            .match_family_style("F", asked)
                            .expect("the family answers");
                        let got = set
                            .iter()
                            .position(|&index| {
                                Typeface::equal(&pool[index], &matched)
                            })
                            .expect("one of the family's faces");
                        let skia = skia_pick(&styles, asked);
                        let first = style_first_pick(&styles, asked);
                        assert!(
                            Some(got) == skia || Some(got) == first,
                            "{set:?} at {asked:?}: Skia took {got}, the \
                             scores say {skia:?} or {first:?}",
                        );
                        if skia != first {
                            parted += 1;
                        }
                        as_skia += usize::from(Some(got) == skia);
                        as_style_first += usize::from(Some(got) == first);
                    }
                }
            }
        }
        let cases = sets.len() * weights.len() * widths.len() * slants.len();
        assert!(parted > 0, "no case tells the two scorings apart");
        // A Skia built from this tree's patched source compares the slant
        // first, through containers/skia-font-match-slant-first.patch;
        // crates.io's sums the scores as upstream does.
        let (followed, scoring) = match cfg!(meo_skia_marker) {
            true => (as_style_first, "the slant-first comparison"),
            false => (as_skia, "Skia's own sum"),
        };
        assert_eq!(
            followed, cases,
            "the provider followed {scoring} in {followed} of {cases} \
             cases, {parted} of them parting from the other scoring",
        );
    }

    /// The styles tests/assets/fonts/Recursive/README.md says every platform
    /// reports for the faces made for these tests, read through this
    /// platform's own font manager.
    #[test]
    fn the_fixtures_report_the_styles_their_readme_gives() {
        for (file, weight, slant) in [
            ("upright", 400, Slant::Upright),
            ("upright900", 900, Slant::Upright),
            ("italic900", 900, Slant::Italic),
            ("italic400", 400, Slant::Italic),
        ] {
            assert_eq!(
                recursive(file).font_style(),
                style(weight, slant),
                "{file}",
            );
        }
    }

    #[test]
    fn a_family_of_one_group_has_no_alias() {
        let upright = recursive("upright");
        let heavy = recursive("upright900");
        let (_, groups) = grouped("OneGroup", &[&upright, &heavy]);
        assert!(groups.is_empty());
        assert_eq!(
            groups.names_for(&["OneGroup"], style(400, Slant::Italic)),
            None
        );
    }

    #[test]
    fn a_family_is_sent_to_its_group_where_the_scorings_part() {
        let upright = recursive("upright");
        let italic = recursive("italic900");
        let (provider, groups) = grouped("Mixed", &[&upright, &italic]);
        let italic_group = group_alias("Mixed", italic.font_style());
        let upright_group = group_alias("Mixed", upright.font_style());
        assert_ne!(italic_group, upright_group);
        assert!(is_group_alias(&italic_group));
        assert!(!is_group_alias("Mixed"));

        // Skia's own scores take the upright 400 face for italic at 400.
        assert_eq!(
            groups.names_for(&["Before", "Mixed"], style(400, Slant::Italic)),
            Some(vec!["Before".to_string(), italic_group.clone()]),
        );
        // Where the two agree the family keeps its name.
        assert_eq!(
            groups.names_for(&["Mixed"], style(400, Slant::Upright)),
            None
        );
        assert_eq!(
            groups.names_for(&["Mixed"], style(900, Slant::Italic)),
            None
        );
        // An alias reads back to its family for a style that needs none.
        assert_eq!(
            groups.names_for(
                &[italic_group.as_str()],
                style(400, Slant::Upright)
            ),
            Some(vec!["Mixed".to_string()]),
        );
        // A name the provider does not hold stands.
        assert_eq!(
            groups.names_for(&["Elsewhere"], style(400, Slant::Italic)),
            None
        );

        // Each group holds its own faces and nothing else.
        let mut set = provider.match_family(&italic_group);
        assert_eq!(set.count(), 1);
        let held = set.new_typeface(0).expect("the italic face");
        assert!(Typeface::equal(&held, &italic));
        assert_eq!(provider.match_family(&upright_group).count(), 1);
        assert_eq!(
            groups.alias_of("Mixed", italic.font_style()),
            Some(italic_group),
        );
        assert_eq!(groups.alias_of("Elsewhere", italic.font_style()), None);
    }

    #[test]
    fn a_group_keeps_the_order_its_faces_were_registered_in() {
        // Two italic faces alike in style: the matcher keeps the first, in the
        // group as in the family.
        let upright = recursive("upright");
        let first = recursive("italic900");
        let second = at_weight(RALEWAY_ITALIC, 900);
        let (provider, groups) =
            grouped("Ordered", &[&upright, &first, &second]);
        let alias = groups
            .names_for(&["Ordered"], style(400, Slant::Italic))
            .expect("italic at 400 goes to the italic faces")
            .remove(0);
        let matched = provider
            .match_family_style(&alias, style(400, Slant::Italic))
            .expect("the group answers");
        assert!(Typeface::equal(&matched, &first));
    }

    #[test]
    fn a_list_naming_nothing_takes_the_fallbacks_group() {
        let upright = recursive("upright");
        let italic = recursive("italic900");
        let (_, groups) = grouped("Fallback", &[&upright, &italic]);
        let italic_style = style(400, Slant::Italic);
        let alias = group_alias("Fallback", italic.font_style());
        let nowhere = |_: &str| false;
        assert_eq!(
            groups.fallback_for(
                &["Missing"],
                italic_style,
                "Fallback",
                nowhere
            ),
            Some(alias),
        );
        // A name the provider or another manager holds is drawn instead.
        assert_eq!(
            groups.fallback_for(
                &["Fallback"],
                italic_style,
                "Fallback",
                nowhere
            ),
            None,
        );
        assert_eq!(
            groups.fallback_for(
                &["System"],
                italic_style,
                "Fallback",
                |name| { name == "System" }
            ),
            None,
        );
        // An empty list is left for the platform to start from nothing.
        let empty: [&str; 0] = [];
        assert_eq!(
            groups.fallback_for(&empty, italic_style, "Fallback", nowhere),
            None,
        );
        // Nor where the fallback's own scores agree.
        assert_eq!(
            groups.fallback_for(
                &["Missing"],
                style(900, Slant::Italic),
                "Fallback",
                nowhere
            ),
            None,
        );
    }

    #[test]
    fn a_list_is_styled_with_the_fallbacks_group_appended() {
        let upright = recursive("upright");
        let italic = recursive("italic900");
        let (_, groups) = grouped("Fallback", &[&upright, &italic]);
        let italic_style = style(400, Slant::Italic);
        let alias = group_alias("Fallback", italic.font_style());
        let nowhere = |_: &str| false;
        let fallback = Fallback {
            family: "Fallback",
            resolves: &nowhere,
        };
        assert_eq!(
            groups.styled(&["Missing"], italic_style, Some(&fallback)),
            Some(vec!["Missing".to_string(), alias.clone()]),
        );
        // Without a fallback only the families named are sent anywhere.
        assert_eq!(groups.styled(&["Missing"], italic_style, None), None);
        assert_eq!(
            groups.styled(&["Fallback"], italic_style, Some(&fallback)),
            Some(vec![alias]),
        );
    }

    /// The gate that keeps a patched build's output as it was: a Skia
    /// carrying `containers/skia-font-match-slant-first.patch` gets no
    /// groups from a filing, and crates.io's gets them.
    #[test]
    fn a_filing_groups_only_where_skia_is_not_patched() {
        let mut filing = Filing::new();
        filing.register(recursive("upright"), Some("Gated"));
        filing.register(recursive("italic900"), Some("Gated"));
        let (provider, groups) = filing.finish();
        let alias = group_alias("Gated", recursive("italic900").font_style());
        assert_eq!(groups.is_empty(), cfg!(meo_skia_marker));
        assert_eq!(
            provider.match_family(&alias).count() == 0,
            cfg!(meo_skia_marker),
        );
    }

    #[test]
    fn an_svg_name_the_provider_lacks_is_written_as_the_first_familys_group() {
        let upright = recursive("upright");
        let italic = recursive("italic900");
        let other = recursive("upright900");
        let mut provider = TypefaceFontProvider::new();
        let filed = [
            (upright.clone(), "First"),
            (italic.clone(), "First"),
            (other, "Second"),
        ]
        .map(|(face, name)| {
            provider.register_typeface(face.clone(), Some(name));
            (face, name.to_string())
        });
        let groups = StyleGroups::register(&mut provider, &filed);
        let italic_style = style(400, Slant::Italic);
        let alias = group_alias("First", italic.font_style());
        assert_eq!(
            groups.written_for("Missing", italic_style),
            Some(alias.clone())
        );
        assert_eq!(
            groups.written_for("First", italic_style),
            Some(alias.clone())
        );
        assert_eq!(groups.written_for("Second", italic_style), None);
        assert_eq!(
            groups.written_for(&alias, style(400, Slant::Upright)),
            Some("First".to_string()),
        );
        assert_eq!(
            groups.written_for("Missing", style(400, Slant::Upright)),
            None
        );
    }
}
