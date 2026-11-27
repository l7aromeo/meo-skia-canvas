//! Text in an SVG document that is set in a face with a `COLR` table, taken
//! out of what Skia's SVG module draws and drawn by the canvas's own text
//! route instead (#663).
//!
//! The module draws `<text>` through the typeface, which on macOS is
//! CoreText's: CoreText on macOS 15 and 26 draws a COLRv1 glyph as nothing,
//! and on 27.2 a COLRv0 one. FreeType and DirectWrite draw both, in other
//! colours than Chrome's -- see `font::colr`. The canvas draws a colour glyph
//! from its paint graph on every platform, as Chrome draws an SVG image's
//! text, so a `<text>` holding a run in such a face is drawn through
//! [`Context2D::fill_text`] and agrees with `fillText` by construction.
//!
//! The document around the text is still the module's. It is drawn in
//! segments: everything before the first such `<text>`, the text, everything
//! between it and the next, and so on, each segment a parse of the same
//! document with every graphics element outside it set to `display: none`.
//! The text is laid out here, as SVG 2's text layout places it: each
//! addressable character at the position its `x`, `y`, `dx` and `dy` lists
//! give it, a text chunk at each absolute position, anchored by its own
//! advance.
//!
//! A `<text>` this cannot lay out as Chrome does is left to the module: one
//! under a group whose opacity, filter, clip, mask, blend mode or isolation a
//! segment would apply to half its content, one on a path or rotated per
//! character, decorated, stroked, spaced, or under any container but a `<g>`.
//! So is every text in a document that `<use>`s an element in its own flow,
//! which a segment would hide from the clone too. docs/browser-differences.md
//! lists them.

use std::{borrow::Cow, collections::HashSet, ops::Range, rc::Rc};

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use skia_safe::{
    Canvas as SkCanvas, Matrix, Point, Rect, Size as SkSize,
    canvas::SaveLayerRec,
    svg::{
        self, Length, LengthUnit,
        preserve_aspect_ratio::{Align, Scale},
    },
};

use crate::{
    canvas::Canvas,
    color::RgbaLinear,
    context::layer::Target,
    context2d::{Context2D, TextRendering},
    export::VectorFeatures,
    font_library::family_behind_alias,
    geometry::Affine,
    text::TextAlign,
};

use super::{
    CSS_INITIAL_FONT_SIZE, length_in_px, stated_font_size, style_declaration,
};

/// The family a `<text>` naming none is set in: SVG text takes CSS's initial
/// `font-family`, which Chrome draws in the standard serif.
const INITIAL_FAMILY: &str = "serif";

/// CSS Fonts 4's initial `font-weight`, `normal`.
const NORMAL_WEIGHT: u16 = 400;

/// The weight `bold` names, CSS Fonts 4 section 2.2.
const BOLD_WEIGHT: u16 = 700;

/// The elements that draw something themselves where they stand in the
/// document: what a segment hides when the element lies outside it.
const GRAPHICS_ELEMENTS: [&str; 10] = [
    "rect", "circle", "ellipse", "line", "polyline", "polygon", "path",
    "image", "text", "use",
];

/// The elements whose content is drawn where it is referenced, or never,
/// rather than where it stands: nothing inside one is in the flow a segment
/// cuts.
const RESOURCE_ELEMENTS: [&str; 14] = [
    "defs",
    "clipPath",
    "mask",
    "pattern",
    "marker",
    "symbol",
    "linearGradient",
    "radialGradient",
    "filter",
    "style",
    "script",
    "title",
    "desc",
    "metadata",
];

/// Properties that make an element composite its content as a group. A
/// segment holding part of such a group's content would composite that part
/// alone, so a text under one is left to the module.
const GROUP_PROPERTIES: [&str; 6] = [
    "opacity",
    "filter",
    "clip-path",
    "mask",
    "mix-blend-mode",
    "isolation",
];

/// Properties of a `<text>` or a `<tspan>` this layout does not carry out:
/// one stated, to anything but its initial value, leaves the text to the
/// module.
const UNSUPPORTED_TEXT_PROPERTIES: [&str; 15] = [
    "rotate",
    "textLength",
    "writing-mode",
    "direction",
    "unicode-bidi",
    "baseline-shift",
    "dominant-baseline",
    "alignment-baseline",
    "text-decoration",
    "letter-spacing",
    "word-spacing",
    "font-variant",
    "font-feature-settings",
    "font-variation-settings",
    "text-rendering",
];

/// The values each of [`UNSUPPORTED_TEXT_PROPERTIES`] may hold and still be
/// laid out here, since they are what the property says when it says
/// nothing.
const INITIAL_VALUES: [&str; 4] = ["normal", "auto", "none", "0"];

/// Which point of a text chunk its position names: SVG 2 `text-anchor`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Anchor {
    Start,
    Middle,
    End,
}

/// A fill as the cascade leaves it.
#[derive(Clone, Debug, PartialEq)]
enum Fill {
    None,
    /// A colour as written, in any form a canvas `fillStyle` reads.
    Colour(Rc<str>),
    /// `currentcolor`, and the `color` declared nearest above it below the
    /// root, or `None` for the root's own, which
    /// [`Svg::set_current_color`](super::Svg::set_current_color) replaces.
    Current(Option<Rc<str>>),
}

/// What an element passes to the elements inside it.
#[derive(Clone)]
struct Inherited {
    font_size: f32,
    family: Option<Rc<str>>,
    weight: u16,
    slant: Rc<str>,
    stretch: Option<Rc<str>>,
    fill: Fill,
    fill_opacity: f32,
    stroked: bool,
    color: Option<Rc<str>>,
    anchor: Anchor,
    visible: bool,
    preserve: bool,
    /// Inside an element whose content is drawn where it is referenced.
    resource: bool,
    /// Under a container a segment cannot cut through.
    blocked: bool,
    /// The element's own `transform` and those of its ancestors, outermost
    /// first, in the root's user space.
    transform: Matrix,
}

/// A run of characters laid out together: in one style, and with no
/// position given to any character after its first.
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    font: Rc<str>,
    fill: Fill,
    fill_opacity: f32,
    visible: bool,
    anchor: Anchor,
    x: Option<f32>,
    y: Option<f32>,
    dx: f32,
    dy: f32,
}

/// One `<text>`, ready to lay out and draw.
#[derive(Clone, Debug)]
pub(crate) struct TextDraw {
    transform: Matrix,
    opacity: f32,
    pieces: Vec<Piece>,
}

/// An element of the document, as far as cutting it into segments needs.
struct Element {
    /// Where an attribute is written into its start tag: just after its
    /// name.
    name_end: usize,
    /// The value of its own `display` attribute, which hiding it replaces
    /// rather than repeats.
    display: Option<Range<usize>>,
    /// The graphics elements it holds, itself included, by their order in
    /// the document.
    leaves: Range<usize>,
    /// Outside every resource, so drawn where it stands.
    in_flow: bool,
    parent: Option<usize>,
}

/// A `<text>` that may be drawn by the canvas, if it holds a colour glyph.
pub(crate) struct Candidate {
    element: usize,
    leaf: usize,
    pub(crate) draw: TextDraw,
}

/// A document read for its text in colour faces.
pub(crate) struct Plan {
    elements: Vec<Element>,
    pub(crate) candidates: Vec<Candidate>,
}

/// How a document is drawn once its colour text is taken out: the module's
/// segments and the texts between them, in paint order.
pub(crate) enum Part {
    /// The rewritten document with everything outside one segment hidden.
    Segment(Vec<u8>),
    Text(TextDraw),
}

/// A `<text>` being read.
struct TextBuild {
    element: usize,
    leaf: usize,
    depth: usize,
    transform: Matrix,
    opacity: f32,
    /// Each addressable character, with the index of its style in `styles`.
    chars: Vec<(char, usize)>,
    /// The style of each run of text read, with the font it is set in.
    styles: Vec<(Inherited, Rc<str>)>,
    /// The position lists of the text and each `tspan` open in it, with the
    /// index of the first character each covers.
    lists: Vec<Lists>,
    /// Every element in the text that set position lists, closed or not, so
    /// that a character finds the innermost one covering it.
    finished: Vec<Lists>,
    /// The last character added was a space a later one collapses into.
    after_space: bool,
    /// Something in it this layout cannot draw as Chrome does.
    refused: bool,
    /// Inside an element of the text whose content is not drawn.
    skipping: usize,
    /// For each element open inside the text, whether it counts in
    /// `skipping`.
    opened: Vec<bool>,
}

#[derive(Clone, Default)]
struct Lists {
    /// The characters the element holds.
    start: usize,
    end: usize,
    depth: usize,
    x: Vec<f32>,
    y: Vec<f32>,
    dx: Vec<f32>,
    dy: Vec<f32>,
}

/// An attribute or `style` declaration of `element`, the declaration
/// winning, as CSS has a declaration win over a presentation attribute.
pub(super) fn property<'a>(
    attributes: &'a [(&str, Cow<str>)],
    style: Option<&'a str>,
    name: &str,
) -> Option<&'a str> {
    style_declaration(style, name).or_else(|| {
        attributes
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.trim())
    })
}

/// The attributes of `element`, unescaped, or `None` where one cannot be.
pub(super) fn attributes_of<'a>(
    element: &'a BytesStart<'a>,
) -> Option<Vec<(&'a str, Cow<'a, str>)>> {
    element
        .attributes()
        .map(|attribute| {
            let attribute = attribute.ok()?;
            let value =
                attribute.normalized_value(XmlVersion::Implicit1_0).ok()?;
            Some((attribute.key.into_inner(), value))
        })
        .collect()
}

/// A list of numbers in user units, as `x`, `y`, `dx` and `dy` take, or
/// `None` where an item is in a unit this cannot place -- a percentage of a
/// viewport, which the rewrite before this leaves as written.
fn number_list(value: &str) -> Option<Vec<f32>> {
    value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|item| !item.is_empty())
        .map(|item| item.strip_suffix("px").unwrap_or(item).parse().ok())
        .collect()
}

/// The numbers inside `name(...)` of a transform list.
fn arguments(inside: &str) -> Option<Vec<f32>> {
    number_list(inside).filter(|args| args.iter().all(|n| n.is_finite()))
}

/// An SVG `transform` attribute as a matrix, or `None` where it does not
/// parse: SVG 1.1 section 7.6, each function applied in the order written,
/// the last one nearest the content.
fn transform_list(value: &str) -> Option<Matrix> {
    let mut matrix = Matrix::new_identity();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let args = arguments(&rest[open + 1..close])?;
        let step = match (name, args.as_slice()) {
            ("matrix", &[a, b, c, d, e, f]) => {
                Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0)
            }
            ("translate", &[x]) => Matrix::translate((x, 0.0)),
            ("translate", &[x, y]) => Matrix::translate((x, y)),
            ("scale", &[s]) => Matrix::scale((s, s)),
            ("scale", &[x, y]) => Matrix::scale((x, y)),
            ("rotate", &[a]) => Matrix::rotate_deg(a),
            ("rotate", &[a, x, y]) => Matrix::rotate_deg_pivot(a, (x, y)),
            ("skewX", &[a]) => Matrix::skew((a.to_radians().tan(), 0.0)),
            ("skewY", &[a]) => Matrix::skew((0.0, a.to_radians().tan())),
            _ => return None,
        };
        matrix.pre_concat(&step);
        rest = rest[close + 1..].trim_start();
    }
    Some(matrix)
}

/// A `font-weight` against the weight it inherits: CSS Fonts 4 section 2.2,
/// whose `bolder` and `lighter` step through the table it gives.
fn weight(value: &str, inherited: u16) -> Option<u16> {
    Some(match value {
        "normal" => NORMAL_WEIGHT,
        "bold" => BOLD_WEIGHT,
        "bolder" => match inherited {
            0..350 => 400,
            350..550 => 700,
            _ => 900,
        },
        "lighter" => match inherited {
            0..550 => 100,
            550..750 => 400,
            _ => 700,
        },
        number => number
            .parse::<f32>()
            .ok()
            .filter(|w| (1.0..=1000.0).contains(w))? as u16,
    })
}

/// A `font-family` list as the rewrite before this writes it, with each
/// private alias the SVG font manager is asked for by -- see
/// `font_library::private_alias` -- put back as the family a canvas names.
fn canvas_families(written: &str) -> String {
    written
        .split(',')
        .map(|family| {
            let name = family.trim().trim_matches(['"', '\'']);
            match family_behind_alias(name) {
                Some(stated) => format!("\"{stated}\""),
                None => family.trim().to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The CSS `font` shorthand for a piece in `style`.
fn font_shorthand(style: &Inherited) -> Rc<str> {
    let family = style
        .family
        .as_deref()
        .map_or(Cow::Borrowed(INITIAL_FAMILY), |written| {
            Cow::Owned(canvas_families(written))
        });
    let stretch = style.stretch.as_deref().unwrap_or("normal");
    format!(
        "{} {} {stretch} {}px {family}",
        style.slant, style.weight, style.font_size
    )
    .into()
}

impl TextBuild {
    /// Adds `text`, read inside an element in `style`, collapsing its white
    /// space as SVG 2 does under `white-space: normal` -- each newline, tab
    /// and run of spaces to one space, none at the start of the text -- or,
    /// under `xml:space="preserve"`, turning newlines and tabs into spaces and
    /// keeping every one.
    fn add(&mut self, text: &str, style: &Inherited) {
        if self.skipping > 0 {
            return;
        }
        let index = self.styles.len();
        self.styles.push((style.clone(), font_shorthand(style)));
        for c in text.chars() {
            let space = matches!(c, ' ' | '\n' | '\r' | '\t');
            if space && !style.preserve {
                if self.after_space || self.chars.is_empty() {
                    continue;
                }
                self.after_space = true;
            } else {
                self.after_space = false;
            }
            self.chars.push((if space { ' ' } else { c }, index));
        }
    }

    /// Opens the position lists of an element of the text.
    fn open_lists(
        &mut self,
        attributes: &[(&str, Cow<'_, str>)],
        depth: usize,
    ) {
        let mut lists = Lists {
            start: self.chars.len(),
            end: usize::MAX,
            depth,
            ..Lists::default()
        };
        for (key, value) in attributes {
            let target = match *key {
                "x" => &mut lists.x,
                "y" => &mut lists.y,
                "dx" => &mut lists.dx,
                "dy" => &mut lists.dy,
                _ => continue,
            };
            match number_list(value) {
                Some(numbers) => *target = numbers,
                None => self.refused = true,
            }
        }
        self.lists.push(lists);
    }

    /// Closes the lists the element at `depth` opened.
    fn close_lists(&mut self, depth: usize) {
        if self.lists.last().is_some_and(|lists| lists.depth == depth)
            && let Some(mut lists) = self.lists.pop()
        {
            lists.end = self.chars.len();
            self.finished.push(lists);
        }
    }

    /// The value at character `index` of whichever list `pick` reads, from
    /// the innermost element covering that character with a value there.
    ///
    /// `finished` holds every element's lists in the order they closed, so
    /// an inner element comes before the element holding it.
    fn value_at(
        &self,
        index: usize,
        pick: fn(&Lists) -> &Vec<f32>,
    ) -> Option<f32> {
        self.finished
            .iter()
            .filter(|lists| (lists.start..lists.end).contains(&index))
            .find_map(|lists| pick(lists).get(index - lists.start).copied())
    }

    /// The text, cut into pieces, or `None` where it is not drawn here.
    fn finish(mut self) -> Option<TextDraw> {
        if self.refused {
            return None;
        }
        // A collapsible space at the end of the text is dropped, as one at
        // its start was.
        if self.after_space {
            self.chars.pop();
        }
        while let Some(mut lists) = self.lists.pop() {
            lists.end = self.chars.len();
            self.finished.push(lists);
        }
        let mut pieces: Vec<Piece> = Vec::new();
        for (index, &(c, style)) in self.chars.iter().enumerate() {
            let x = self.value_at(index, |l| &l.x);
            let y = self.value_at(index, |l| &l.y);
            let dx = self.value_at(index, |l| &l.dx).unwrap_or(0.0);
            let dy = self.value_at(index, |l| &l.dy).unwrap_or(0.0);
            let (style, font) = &self.styles[style];
            let font = font.clone();
            let placed = index == 0
                || x.is_some()
                || y.is_some()
                || dx != 0.0
                || dy != 0.0;
            let continues = !placed
                && pieces.last().is_some_and(|last| {
                    last.font == font
                        && last.visible == style.visible
                        && last.fill_opacity == style.fill_opacity
                        && last.fill == style.fill
                });
            match pieces.last_mut() {
                Some(last) if continues => last.text.push(c),
                _ => pieces.push(Piece {
                    text: c.to_string(),
                    font,
                    fill: style.fill.clone(),
                    fill_opacity: style.fill_opacity,
                    visible: style.visible,
                    anchor: style.anchor,
                    // The first character of a text starts at its origin
                    // where nothing places it.
                    x: x.or((index == 0).then_some(0.0)),
                    y: y.or((index == 0).then_some(0.0)),
                    dx,
                    dy,
                }),
            }
        }
        (!pieces.is_empty()).then_some(TextDraw {
            transform: self.transform,
            opacity: self.opacity,
            pieces,
        })
    }
}

/// Reads the colour text candidates of `xml`, the document as the rewrite
/// before this leaves it, or `None` where the document cannot be cut into
/// segments at all.
pub(crate) fn plan(xml: &[u8]) -> Option<Plan> {
    let text = std::str::from_utf8(xml).ok()?;
    // Most documents hold no text, and those cost nothing more to parse.
    if !text.contains("<text") {
        return None;
    }
    let mut reader = Reader::from_str(text);
    let mut elements: Vec<Element> = Vec::new();
    let mut candidates = Vec::new();
    // Each open element's index and what it passes on.
    let mut open: Vec<(usize, Inherited)> = Vec::new();
    let mut leaves = 0;
    let mut building: Option<TextBuild> = None;
    let mut use_targets = Vec::new();
    let mut flow_ids = HashSet::new();
    let mut root_seen = false;

    loop {
        let (element, empty) = match reader.read_event() {
            Ok(Event::Start(element)) => (element, false),
            Ok(Event::Empty(element)) => (element, true),
            Ok(Event::End(_)) => {
                let (index, _) = open.pop()?;
                let depth = open.len();
                elements[index].leaves.end = leaves;
                if let Some(build) = building.as_mut() {
                    if build.depth == depth {
                        let build = building.take()?;
                        let (element, leaf) = (build.element, build.leaf);
                        if let Some(draw) = build.finish() {
                            candidates.push(Candidate {
                                element,
                                leaf,
                                draw,
                            });
                        }
                    } else {
                        build.close_lists(depth);
                        if build.opened.pop() == Some(true) {
                            build.skipping -= 1;
                        }
                    }
                }
                continue;
            }
            Ok(Event::Text(chunk)) => {
                if let (Some(build), Some((_, style))) =
                    (building.as_mut(), open.last())
                {
                    build.add(&chunk.xml10_content(), style);
                }
                continue;
            }
            Ok(Event::CData(chunk)) => {
                if let (Some(build), Some((_, style))) =
                    (building.as_mut(), open.last())
                {
                    build.add(&chunk.xml10_content(), style);
                }
                continue;
            }
            Ok(Event::GeneralRef(reference)) => {
                if let (Some(build), Some((_, style))) =
                    (building.as_mut(), open.last())
                {
                    match predefined_or_char(&reference.xml10_content()) {
                        Some(c) => build.add(c.encode_utf8(&mut [0; 4]), style),
                        None => build.refused = true,
                    }
                }
                continue;
            }
            Ok(Event::Eof) => break,
            Ok(_) => continue,
            Err(_) => return None,
        };

        let name = element.name().as_ref().to_string();
        let attributes = attributes_of(&element)?;
        let style = attributes
            .iter()
            .find(|(key, _)| *key == "style")
            .map(|(_, value)| value.as_ref());
        let get = |key: &str| property(&attributes, style, key);
        let name_end =
            super::borrowed_range(text, element.name().as_ref())?.end;
        let display = element
            .attributes()
            .flatten()
            .find(|a| a.key.as_ref() == "display")
            .and_then(|a| match a.value {
                Cow::Borrowed(raw) => super::borrowed_range(text, raw),
                Cow::Owned(_) => None,
            });
        let parent = open.last().map(|(index, _)| *index);
        let mut own = match open.last() {
            Some((_, inherited)) => inherited.clone(),
            None => Inherited {
                font_size: CSS_INITIAL_FONT_SIZE,
                family: None,
                weight: NORMAL_WEIGHT,
                slant: "normal".into(),
                stretch: None,
                fill: Fill::Colour("#000".into()),
                fill_opacity: 1.0,
                stroked: false,
                color: None,
                anchor: Anchor::Start,
                visible: true,
                preserve: false,
                resource: false,
                blocked: false,
                transform: Matrix::new_identity(),
            },
        };
        let is_root = !root_seen;
        root_seen = true;

        // The cascade, over what the parent passes on.
        if let Some(px) =
            stated_font_size(get("font-size").unwrap_or_default(), style)
                .and_then(|stated| length_in_px(stated, own.font_size, 0.5))
        {
            own.font_size = px;
        }
        if let Some(family) = get("font-family") {
            own.family = Some(family.into());
        }
        if let Some(value) = get("font-weight") {
            own.weight = weight(value, own.weight).unwrap_or(own.weight);
        }
        if let Some(value) = get("font-style") {
            own.slant = value.into();
        }
        if let Some(value) = get("font-stretch") {
            own.stretch = Some(value.into());
        }
        if let Some(value) = get("color").filter(|v| *v != "inherit") {
            // The root's is the one the override replaces.
            own.color = (!is_root).then(|| value.into());
        }
        if let Some(value) = get("fill").filter(|v| *v != "inherit") {
            own.fill = match value {
                "none" => Fill::None,
                v if v.eq_ignore_ascii_case("currentcolor") => {
                    Fill::Current(own.color.clone())
                }
                v => Fill::Colour(v.into()),
            };
        }
        if let Some(value) = get("fill-opacity") {
            own.fill_opacity = opacity(value).unwrap_or(own.fill_opacity);
        }
        if let Some(value) = get("stroke") {
            own.stroked = value != "none";
        }
        if let Some(value) = get("text-anchor") {
            own.anchor = match value {
                "middle" => Anchor::Middle,
                "end" => Anchor::End,
                _ => Anchor::Start,
            };
        }
        if let Some(value) = get("visibility") {
            own.visible = value == "visible";
        }
        if let Some(value) = get("xml:space") {
            own.preserve = value == "preserve";
        }
        own.resource |= RESOURCE_ELEMENTS.contains(&name.as_str());
        let groups = GROUP_PROPERTIES.iter().any(|property| {
            get(property)
                .is_some_and(|v| v != "none" && opacity(v) != Some(1.0))
        });
        let transform = match get("transform") {
            Some(value) => transform_list(value),
            None => Some(Matrix::new_identity()),
        };
        if !own.resource && building.is_none() {
            let container_ok = is_root || name == "g" || name == "text";
            own.blocked |= !container_ok
                || (groups && name != "text")
                || transform.is_none()
                || (is_root && get("transform").is_some())
                || get("display") == Some("none");
            if let Some(transform) = transform.as_ref() {
                own.transform.pre_concat(transform);
            }
        }

        let index = elements.len();
        let in_flow = !own.resource && building.is_none();
        let leaf = GRAPHICS_ELEMENTS.contains(&name.as_str()) && in_flow;
        elements.push(Element {
            name_end,
            display,
            leaves: leaves..leaves,
            in_flow,
            parent,
        });
        if in_flow && let Some(id) = get("id") {
            flow_ids.insert(id.to_string());
        }
        if name == "use" {
            let href = attributes
                .iter()
                .find(|(key, _)| *key == "xlink:href" || *key == "href")
                .map(|(_, value)| value.as_ref());
            if let Some(target) = href.and_then(|h| h.strip_prefix('#')) {
                use_targets.push(target.to_string());
            }
        }

        let depth = open.len();
        if let Some(build) = building.as_mut() {
            let skips_before = build.skipping;
            if name == "tspan" && build.skipping == 0 {
                let refused = UNSUPPORTED_TEXT_PROPERTIES.iter().any(|p| {
                    get(p).is_some_and(|v| !INITIAL_VALUES.contains(&v))
                }) || groups
                    || get("transform").is_some()
                    || get("display").is_some_and(|v| v != "inline")
                    || own.stroked
                    || matches!(&own.fill, Fill::Colour(c) if c.starts_with("url("));
                build.refused |= refused;
                build.open_lists(&attributes, depth);
            } else if ["title", "desc", "metadata"].contains(&name.as_str())
                || build.skipping > 0
            {
                build.skipping += 1;
            } else {
                // `textPath`, `a`, or anything else inside a text.
                build.refused = true;
                build.skipping += 1;
            }
            let skips = build.skipping > skips_before;
            if empty {
                build.close_lists(depth);
                build.skipping = skips_before;
            } else {
                build.opened.push(skips);
            }
        } else if name == "text" && in_flow {
            let refused = own.blocked
                || UNSUPPORTED_TEXT_PROPERTIES.iter().any(|p| {
                    get(p).is_some_and(|v| !INITIAL_VALUES.contains(&v))
                })
                || ["filter", "clip-path", "mask", "mix-blend-mode"]
                    .iter()
                    .any(|p| get(p).is_some_and(|v| v != "none"))
                || own.stroked
                || matches!(&own.fill, Fill::Colour(c) if c.starts_with("url("));
            let mut build = TextBuild {
                element: index,
                leaf: leaves,
                depth,
                transform: own.transform,
                opacity: get("opacity").and_then(opacity).unwrap_or(1.0),
                chars: Vec::new(),
                styles: Vec::new(),
                lists: Vec::new(),
                finished: Vec::new(),
                after_space: false,
                refused,
                skipping: 0,
                opened: Vec::new(),
            };
            build.open_lists(&attributes, depth);
            if !empty {
                building = Some(build);
            }
        }

        if leaf {
            leaves += 1;
        }
        if empty {
            elements[index].leaves.end = leaves;
        } else {
            open.push((index, own));
        }
    }

    // A `<use>` of an element in the flow draws a clone of it, which a
    // segment hiding the original would hide too.
    if use_targets.iter().any(|target| flow_ids.contains(target)) {
        return None;
    }
    Some(Plan {
        elements,
        candidates,
    })
}

/// The character an entity reference in text stands for: one of XML's five
/// predefined entities or a character reference. Any other names an entity a
/// DTD declares, which this does not read.
fn predefined_or_char(reference: &str) -> Option<char> {
    match reference {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        number => {
            let number = number.strip_prefix('#')?;
            let code = match number.strip_prefix('x') {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// An opacity, as a number or a percentage, clamped as CSS clamps one.
pub(super) fn opacity(value: &str) -> Option<f32> {
    let value = value.trim();
    let number = match value.strip_suffix('%') {
        Some(percent) => percent.trim().parse::<f32>().ok()? / 100.0,
        None => value.parse::<f32>().ok()?,
    };
    number.is_finite().then(|| number.clamp(0.0, 1.0))
}

/// Whether each of `candidates` sets a glyph in a colour face, as the canvas
/// lays it out: its families, its weight and slant, and the faces its
/// characters fall back to where those lack them. A candidate whose font the
/// canvas does not read is left to the module.
pub(crate) fn set_in_colour(candidates: &[Candidate]) -> Vec<bool> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut canvas = Canvas::new(1.0, 1.0);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_text_rendering(TextRendering::GeometricPrecision);
    candidates
        .iter()
        .map(|candidate| {
            let mut colour = false;
            candidate.draw.runs().all(|(text, font)| {
                let read = ctx.set_font_css(font);
                colour |= read && ctx.inner.sets_colour_glyphs(text);
                read
            }) && colour
        })
        .collect()
}

impl Plan {
    /// The document cut around the candidates `colour` says to draw, in paint
    /// order, or `None` where it says to draw none.
    pub(crate) fn parts(
        &self,
        xml: &[u8],
        colour: &[bool],
    ) -> Option<Vec<Part>> {
        let routed: Vec<&Candidate> = self
            .candidates
            .iter()
            .zip(colour)
            .filter_map(|(candidate, &colour)| colour.then_some(candidate))
            .collect();
        if routed.is_empty() {
            return None;
        }
        let texts: HashSet<usize> =
            routed.iter().map(|candidate| candidate.element).collect();
        let mut parts = Vec::new();
        let mut from = 0;
        for bound in routed.iter().map(Some).chain(std::iter::once(None)) {
            let to = bound.map_or(usize::MAX, |candidate| candidate.leaf);
            if let Some(segment) = self.segment(xml, from..to, &texts) {
                parts.push(Part::Segment(segment));
            }
            if let Some(candidate) = bound {
                parts.push(Part::Text(candidate.draw.clone()));
                from = candidate.leaf + 1;
            }
        }
        Some(parts)
    }

    /// The document with everything in its flow outside the graphics
    /// elements `range` holds set to `display: none`, the routed `texts`
    /// included, or `None` where nothing in it would draw.
    fn segment(
        &self,
        xml: &[u8],
        range: Range<usize>,
        texts: &HashSet<usize>,
    ) -> Option<Vec<u8>> {
        let within = |leaves: &Range<usize>| {
            range.start <= leaves.start && leaves.end <= range.end
        };
        let holds = |leaves: &Range<usize>| {
            leaves.start < range.end && range.start < leaves.end
        };
        let mut splices: Vec<(Range<usize>, &str)> = Vec::new();
        let mut hidden = vec![false; self.elements.len()];
        let mut draws = false;
        for (index, element) in self.elements.iter().enumerate() {
            if !element.in_flow {
                continue;
            }
            let parent_hidden = element.parent.is_some_and(|p| hidden[p]);
            if parent_hidden {
                hidden[index] = true;
                continue;
            }
            let leaves = &element.leaves;
            // An element holding no graphics element lies at one point in
            // the order -- after as many as precede it -- and belongs to the
            // segment that point falls in.
            let empty = leaves.is_empty();
            let keep = !texts.contains(&index)
                && if empty {
                    element.parent.is_none()
                        || (range.start <= leaves.start
                            && leaves.start <= range.end)
                } else {
                    holds(leaves)
                };
            if keep {
                draws |= element.parent.is_some() && (empty || within(leaves));
                continue;
            }
            hidden[index] = true;
            match &element.display {
                Some(value) => splices.push((value.clone(), "none")),
                None => splices.push((
                    element.name_end..element.name_end,
                    r#" display="none""#,
                )),
            }
        }
        if !draws {
            return None;
        }
        splices.sort_by_key(|(range, _)| range.start);
        let mut out = xml.to_vec();
        for (range, with) in splices.into_iter().rev() {
            out.splice(range, with.bytes());
        }
        Some(out)
    }
}

/// A length on the root against the viewport extent `along`: a percentage
/// of it, or the number as written, which the rewrite has left in user units.
fn resolved(length: &Length, along: f32) -> f32 {
    match length.unit {
        LengthUnit::Percentage => length.value * along / 100.0,
        _ => length.value,
    }
}

/// The matrix Skia's root `<svg>` applies to its content for a container of
/// `container`: its `viewBox` fitted into its own width and height by its
/// `preserveAspectRatio` -- `SkSVGSVG::onPrepareToRender` and
/// `SkSVGNode::ComputeViewboxMatrix`. The outermost element's `x` and `y`
/// are ignored, as SVG 2 says and Skia does.
fn root_matrix(root: &svg::Svg, container: SkSize) -> Matrix {
    let (w, h) = (container.width, container.height);
    let viewport =
        Rect::from_wh(resolved(root.width(), w), resolved(root.height(), h));
    let Some(view_box) = root.view_box() else {
        return Matrix::new_identity();
    };
    if view_box.is_empty() || viewport.is_empty() {
        return Matrix::scale((0.0, 0.0));
    }
    let par = root.preserve_aspect_ratio();
    let (sx, sy) = (
        viewport.width() / view_box.width(),
        viewport.height() / view_box.height(),
    );
    // `SkSVGPreserveAspectRatio::Align` packs the x alignment in its two low
    // bits and the y alignment in the next two; `kNone` is 0x10.
    let align = par.align as u32;
    let none = align == Align::None as u32;
    let (sx, sy) = if none {
        (sx, sy)
    } else {
        let s = match par.scale {
            Scale::Meet => sx.min(sy),
            Scale::Slice => sx.max(sy),
        };
        (s, s)
    };
    let coefficient = |bits: u32| match bits & 0x03 {
        0 => 0.0,
        1 => 0.5,
        _ => 1.0,
    };
    let (cx, cy) = if none {
        (0.0, 0.0)
    } else {
        (coefficient(align), coefficient(align >> 2))
    };
    let tx =
        -view_box.x() * sx + cx * (viewport.width() - view_box.width() * sx);
    let ty =
        -view_box.y() * sy + cy * (viewport.height() - view_box.height() * sy);
    Matrix::new_all(sx, 0.0, tx, 0.0, sy, ty, 0.0, 0.0, 1.0)
}

impl TextDraw {
    /// Lays the text out and draws it onto `target`, under the matrix the
    /// target holds now and the root's for a container of `container`, with
    /// `currentcolor` resolving to `current` at the root: as the page
    /// `fill_text` drew it on is played for `kind`, so that a document is
    /// drawn as one draws the same text drawn by `fill_text`.
    ///
    /// Says what the drawing asks of a vector backend: what `fill_text` asked
    /// of the page it drew on -- the layer a colour glyph is composited
    /// through, where a document draws it through one -- and the layer
    /// `opacity` is drawn through, so that a document a backend cannot write
    /// it into embeds it as pixels, as it does the same text drawn by
    /// `fill_text` -- see `export::VectorFeatures`.
    pub(crate) fn draw(
        &self,
        target: &SkCanvas,
        root: &svg::Svg,
        container: SkSize,
        current: RgbaLinear,
        kind: Target,
    ) -> VectorFeatures {
        let size = target.base_layer_size();
        if size.width <= 0 || size.height <= 0 {
            return VectorFeatures::PLAIN;
        }
        let mut device = target.local_to_device_as_3x3();
        device.pre_concat(&root_matrix(root, container));
        device.pre_concat(&self.transform);

        let mut canvas = Canvas::new(size.width as f32, size.height as f32);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.set_text_rendering(TextRendering::GeometricPrecision);
        ctx.set_text_align(TextAlign::Left);
        ctx.set_transform(affine(&device));

        for (piece, origin) in self.layout(ctx) {
            let fill = match &piece.fill {
                Fill::None => continue,
                Fill::Colour(colour) | Fill::Current(Some(colour)) => {
                    RgbaLinear::from_css(colour)
                }
                Fill::Current(None) => Some(current),
            };
            let (true, Some(fill)) = (piece.visible, fill) else {
                continue;
            };
            let k = piece.fill_opacity;
            ctx.set_fill_style(RgbaLinear {
                r: fill.r * k,
                g: fill.g * k,
                b: fill.b * k,
                a: fill.a * k,
            });
            ctx.set_font_css(&piece.font);
            ctx.fill_text(&piece.text, origin.x, origin.y, None);
        }

        let page = ctx.inner.page();
        let picture = match kind {
            Target::Pixels => page.picture(None),
            Target::Document => page.document_picture(),
        };
        let Some(picture) = picture else {
            return VectorFeatures::PLAIN;
        };
        let mut features = page.vector_features();
        target.save();
        target.reset_matrix();
        if self.opacity < 1.0 {
            features = features.with(VectorFeatures::LAYER);
            let alpha = skia_safe::Paint::default()
                .set_alpha_f(self.opacity)
                .clone();
            target.save_layer(&SaveLayerRec::default().paint(&alpha));
            target.draw_picture(&picture, None, None);
            target.restore();
        } else {
            target.draw_picture(&picture, None, None);
        }
        target.restore();
        features
    }

    /// Each piece with the origin it is drawn from: SVG 2 section 11.8.3's
    /// layout, each chunk -- the characters from one absolutely positioned
    /// to the next -- shifted by its anchor and its own advance.
    fn layout<'a>(&'a self, ctx: &mut Context2D) -> Vec<(&'a Piece, Point)> {
        let mut placed: Vec<(&Piece, Point)> = Vec::new();
        let mut chunk_start = 0;
        let mut pen = Point::new(0.0, 0.0);
        let mut chunk_x = 0.0;
        let close = |placed: &mut Vec<(&Piece, Point)>,
                     from: usize,
                     start: f32,
                     end: f32| {
            let Some(&(first, _)) = placed.get(from) else {
                return;
            };
            let shift = match first.anchor {
                Anchor::Start => 0.0,
                Anchor::Middle => (end - start) / 2.0,
                Anchor::End => end - start,
            };
            for (_, origin) in &mut placed[from..] {
                origin.x -= shift;
            }
        };
        for piece in &self.pieces {
            if piece.x.is_some() || piece.y.is_some() {
                if !placed.is_empty() {
                    close(&mut placed, chunk_start, chunk_x, pen.x);
                }
                pen = Point::new(
                    piece.x.unwrap_or(pen.x),
                    piece.y.unwrap_or(pen.y),
                );
                chunk_start = placed.len();
                chunk_x = pen.x;
            }
            pen.offset((piece.dx, piece.dy));
            placed.push((piece, pen));
            ctx.set_font_css(&piece.font);
            pen.x += ctx.measure_text(&piece.text, None).width;
        }
        close(&mut placed, chunk_start, chunk_x, pen.x);
        placed
    }

    /// Each piece's text with the font it is set in, for a caller asking
    /// whether any of it is set in a colour face.
    pub(crate) fn runs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pieces
            .iter()
            .map(|piece| (piece.text.as_str(), piece.font.as_ref()))
    }
}

/// `matrix` as the canvas takes one.
fn affine(matrix: &Matrix) -> Affine {
    Affine {
        a: matrix.scale_x(),
        b: matrix.skew_y(),
        c: matrix.skew_x(),
        d: matrix.scale_y(),
        tx: matrix.translate_x(),
        ty: matrix.translate_y(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(body: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">{body}</svg>"#
        )
    }

    fn pieces(body: &str) -> Vec<Piece> {
        let plan = plan(document(body).as_bytes()).expect("a plan");
        assert_eq!(plan.candidates.len(), 1, "{body}");
        plan.candidates[0].draw.pieces.clone()
    }

    fn texts(pieces: &[Piece]) -> Vec<&str> {
        pieces.iter().map(|piece| piece.text.as_str()).collect()
    }

    #[test]
    fn a_transform_list_applies_its_last_function_nearest_the_content() {
        let matrix =
            transform_list("translate(10, 20) scale(2)").expect("parses");
        assert_eq!(matrix.map_point((1.0, 1.0)), Point::new(12.0, 22.0));
        let rotated = transform_list("rotate(90 10 10)").expect("parses");
        let point = rotated.map_point((20.0, 10.0));
        assert!((point.x - 10.0).abs() < 1e-4 && (point.y - 20.0).abs() < 1e-4);
        assert!(transform_list("translate(1 2").is_none());
        assert!(transform_list("spin(4)").is_none());
    }

    #[test]
    fn white_space_collapses_across_elements_and_drops_at_both_ends() {
        let got =
            pieces("<text x=\"1\" y=\"2\">  A \n\t <tspan> B</tspan>  </text>");
        assert_eq!(texts(&got), ["A B"]);
        let kept =
            pieces(r#"<text x="1" y="2" xml:space="preserve">  A  B</text>"#);
        assert_eq!(texts(&kept), ["  A  B"]);
    }

    /// An element's position list reaches its own characters only: the
    /// second value of a one-character `tspan` places nothing after it.
    #[test]
    fn a_position_list_stops_at_the_end_of_its_element() {
        let got =
            pieces(r#"<text x="5" y="9">A<tspan x="40 80">B</tspan>C</text>"#);
        assert_eq!(texts(&got), ["A", "BC"]);
        assert_eq!(got[1].x, Some(40.0));
    }

    /// Each value of a list on the text places one addressable character,
    /// and a character a `dx` moves starts a piece of its own.
    #[test]
    fn each_listed_position_starts_a_piece() {
        let got = pieces(r#"<text x="1 20" y="9" dx="0 0 3">ABCD</text>"#);
        assert_eq!(texts(&got), ["A", "B", "CD"]);
        assert_eq!((got[1].x, got[2].x, got[2].dx), (Some(20.0), None, 3.0));
    }

    #[test]
    fn a_character_reference_is_the_character_it_names() {
        let got = pieces(r#"<text x="1" y="9">&#xf0100;&amp;&#65;</text>"#);
        assert_eq!(texts(&got), ["\u{f0100}&A"]);
    }

    /// A text the layout here cannot place as Chrome does is no candidate,
    /// and stays the module's.
    #[test]
    fn a_text_this_cannot_lay_out_is_left_to_the_module() {
        for body in [
            r#"<g opacity="0.5"><text x="1" y="9">A</text></g>"#,
            r#"<g filter="url(#f)"><text x="1" y="9">A</text></g>"#,
            r#"<a><text x="1" y="9">A</text></a>"#,
            r#"<text x="1" y="9" rotate="10">A</text>"#,
            r#"<text x="1" y="9" stroke="red">A</text>"#,
            r#"<text x="1" y="9" fill="url(#g)">A</text>"#,
            r##"<text x="1" y="9"><textPath href="#p">A</textPath></text>"##,
            r#"<text x="1" y="9" letter-spacing="2">A</text>"#,
            r#"<text x="10%" y="9">A</text>"#,
            r#"<defs><text id="t" x="1" y="9">A</text></defs>"#,
        ] {
            let plan = plan(document(body).as_bytes()).expect("a plan");
            assert!(plan.candidates.is_empty(), "{body}");
        }
        // The controls: the same texts with nothing in the way.
        for body in [
            r#"<g transform="translate(3 4)"><text x="1" y="9">A</text></g>"#,
            r#"<text x="1" y="9" opacity="0.5" letter-spacing="normal">A</text>"#,
        ] {
            let plan = plan(document(body).as_bytes()).expect("a plan");
            assert_eq!(plan.candidates.len(), 1, "{body}");
        }
    }

    /// A `<use>` of an element in the flow draws a clone a segment would
    /// hide along with it, so such a document is not cut at all.
    #[test]
    fn a_document_using_an_element_in_its_flow_is_not_cut() {
        let used = r##"<rect id="r" width="4" height="4"/><use href="#r" x="9"/><text x="1" y="9">A</text>"##;
        assert!(plan(document(used).as_bytes()).is_none());
        let defined = r##"<defs><rect id="r" width="4" height="4"/></defs><use href="#r" x="9"/><text x="1" y="9">A</text>"##;
        assert!(plan(document(defined).as_bytes()).is_some());
    }

    /// Each segment hides every graphics element outside it, the routed
    /// text in all of them, and replaces a `display` already written rather
    /// than repeating the attribute.
    #[test]
    fn a_segment_hides_what_lies_outside_it() {
        let xml = document(
            r#"<rect id="a" width="1" height="1"/><text x="1" y="9">A</text><g><circle id="b" r="1"/><path id="c" display="inline" d="M0 0"/></g>"#,
        );
        let plan = plan(xml.as_bytes()).expect("a plan");
        let parts = plan.parts(xml.as_bytes(), &[true]).expect("parts");
        let segments: Vec<String> = parts
            .iter()
            .filter_map(|part| match part {
                Part::Segment(bytes) => {
                    Some(String::from_utf8(bytes.clone()).expect("UTF-8"))
                }
                Part::Text(_) => None,
            })
            .collect();
        assert_eq!(parts.len(), 3, "before, the text, after");
        assert!(matches!(parts[1], Part::Text(_)));
        let [before, after] = segments.as_slice() else {
            panic!("two segments: {segments:?}");
        };
        assert!(before.contains(r#"<rect id="a" width"#), "{before}");
        assert!(before.contains(r#"<text display="none""#), "{before}");
        assert!(before.contains(r#"<g display="none">"#), "{before}");
        assert!(after.contains(r#"<rect display="none" id="a""#), "{after}");
        assert!(after.contains(r#"<text display="none""#), "{after}");
        assert!(after.contains(r#"<circle id="b""#), "{after}");
        assert!(after.contains(r#"display="inline""#), "{after}");
        assert_eq!(after.matches("display=").count(), 3, "{after}");
    }

    #[test]
    fn a_text_set_in_no_colour_face_cuts_nothing() {
        let xml = document(r#"<text x="1" y="9">A</text>"#);
        let plan = plan(xml.as_bytes()).expect("a plan");
        assert!(plan.parts(xml.as_bytes(), &[false]).is_none());
    }
}
