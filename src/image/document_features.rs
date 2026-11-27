//! What Skia's SVG module asks of a vector backend when it draws a document,
//! judged from the document's XML.
//!
//! The module draws some of what a document says through a layer -- see
//! `applyOpacity`, `applyMask` and `applyFilter` in
//! `modules/svg/src/SkSVGRenderContext.cpp`: an `opacity` below 1 on anything
//! but a leaf painting only its fill or only its stroke, a `mask`, and a
//! `filter`. Skia's SVG device has no layers and drops whatever is drawn
//! inside one, so a drawn document holding one exported as nothing where the
//! canvas showed it. The device also writes some paint as another: a
//! `<pattern>`, which the module paints as a picture shader; a gradient whose
//! `spreadMethod` repeats or reflects, which it writes with none, so padded;
//! and a `radialGradient` whose focus is off its centre, which it writes with
//! its circles swapped. A document's draw is filed with what these ask, so
//! that an export embeds it as pixels -- see `VectorFeatures::SVG_CANNOT` --
//! and writes the rest of a document as vectors.
//!
//! Only what the module reads is read here: a presentation attribute and a
//! `style` declaration, the declaration winning, and no `<style>` element,
//! which the module ignores; and each gradient's own attributes, since the
//! module takes only the stops through a gradient's `href`. A document this
//! cannot read through is judged to ask for a layer, so that it is embedded
//! rather than exported as whatever the device keeps of it.

use std::collections::HashSet;

use quick_xml::{Reader, events::Event};

use crate::export::VectorFeatures;

use super::colr_text::{attributes_of, opacity, property};

/// The elements whose characters are children to the module, which counts
/// an element a leaf where it has none (`SkSVGNode::render`), so that an
/// `opacity` on one is always drawn through a layer.
const TEXT_CONTENT: [&str; 3] = ["text", "tspan", "textPath"];

/// What SVG 1.1 section 13.2.4 makes a gradient's `spreadMethod` where it
/// names none, and the one value the device writes as it is.
const PAD: &str = "pad";

/// What SVG 1.1 section 13.2.3 makes a `radialGradient`'s `cx` and `cy`
/// where it states none, which its `fx` and `fy` take in turn.
const CENTRE: &str = "50%";

/// What drawing the document `bytes` asks of a vector backend.
pub(crate) fn features_of(bytes: &[u8]) -> VectorFeatures {
    judge(&String::from_utf8_lossy(bytes)).unwrap_or(VectorFeatures::LAYER)
}

/// An open element: whether its fill and its stroke paint, which its
/// children inherit, and whether it has a child, with the `opacity` it
/// states below 1 where it states one.
struct Open {
    fills: bool,
    strokes: bool,
    has_child: bool,
    pending: Option<Pending>,
}

/// An element's `opacity` below 1, judged once whether it has a child is
/// known.
struct Pending {
    /// Whether the module can count the element a leaf: whether it is not
    /// text content.
    leaf_kind: bool,
    fills: bool,
    strokes: bool,
    filtered: bool,
}

impl Pending {
    /// Whether the module draws the element through a layer, which it does
    /// unless the element is a leaf painting one of its fill and its stroke,
    /// with no filter: then the opacity is one draw's, and it folds it into
    /// that draw's paint.
    fn layered(&self, has_child: bool) -> bool {
        let leaf = self.leaf_kind && !has_child;
        !(leaf && (self.fills ^ self.strokes) && !self.filtered)
    }
}

/// What the document `text` asks of a vector backend, or `None` where its
/// XML cannot be read through.
fn judge(text: &str) -> Option<VectorFeatures> {
    let mut reader = Reader::from_str(text);
    let mut features = VectorFeatures::PLAIN;
    let mut open: Vec<Open> = Vec::new();
    let mut patterns = HashSet::new();
    let mut painted_from = HashSet::new();
    loop {
        let (element, empty) = match reader.read_event().ok()? {
            Event::Start(element) => (element, false),
            Event::Empty(element) => (element, true),
            Event::End(_) => {
                let closed = open.pop()?;
                if closed
                    .pending
                    .is_some_and(|pending| pending.layered(closed.has_child))
                {
                    features = features.with(VectorFeatures::LAYER);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = element.local_name();
        let name = name.as_ref();
        let attributes = attributes_of(&element)?;
        let style = attributes
            .iter()
            .find(|(key, _)| *key == "style")
            .map(|(_, value)| value.as_ref());
        let get = |key: &str| property(&attributes, style, key);
        let own = |key: &str| {
            attributes
                .iter()
                .find(|(attribute, _)| *attribute == key)
                .map(|(_, value)| value.trim())
        };
        let (inherits_fill, inherits_stroke) = open
            .last()
            .map_or((true, false), |parent| (parent.fills, parent.strokes));
        let fills = paints(get("fill"), inherits_fill);
        let strokes = paints(get("stroke"), inherits_stroke);
        for paint in [get("fill"), get("stroke")].into_iter().flatten() {
            if let Some(id) = referenced(paint) {
                painted_from.insert(id.to_string());
            }
        }
        let filtered = get("filter").is_some_and(is_reference);
        if filtered {
            features = features.with(VectorFeatures::IMAGE_FILTER);
        }
        if get("mask").is_some_and(is_reference) {
            features = features.with(VectorFeatures::LAYER);
        }
        if name == "pattern"
            && let Some(id) = own("id")
        {
            patterns.insert(id.to_string());
        }
        if (name == "linearGradient" || name == "radialGradient")
            && own("spreadMethod").is_some_and(|spread| spread != PAD)
        {
            features = features.with(VectorFeatures::EXOTIC_SHADER);
        }
        if name == "radialGradient" && off_centre(&own) {
            features = features.with(VectorFeatures::EXOTIC_SHADER);
        }
        let pending = get("opacity")
            .and_then(opacity)
            .filter(|alpha| *alpha < 1.0)
            .map(|_| Pending {
                leaf_kind: !TEXT_CONTENT.contains(&name),
                fills,
                strokes,
                filtered,
            });
        if let Some(parent) = open.last_mut() {
            parent.has_child = true;
        }
        match empty {
            true => {
                if pending.is_some_and(|pending| pending.layered(false)) {
                    features = features.with(VectorFeatures::LAYER);
                }
            }
            false => open.push(Open {
                fills,
                strokes,
                has_child: false,
                pending,
            }),
        }
    }
    if painted_from.iter().any(|id| patterns.contains(id)) {
        features = features.with(VectorFeatures::EXOTIC_SHADER);
    }
    Some(features)
}

/// Whether a `fill` or `stroke` of `value` paints, `inherited` saying
/// whether the parent's does: SVG paints the fill and not the stroke where
/// nothing says otherwise, and `inherit` takes the parent's.
fn paints(value: Option<&str>, inherited: bool) -> bool {
    match value {
        None | Some("inherit") => inherited,
        Some("none") => false,
        Some(_) => true,
    }
}

/// Whether `value` names something to apply through `url(...)`, as a
/// `filter` or a `mask` the module draws through a layer does.
fn is_reference(value: &str) -> bool {
    value.trim_start().starts_with("url(")
}

/// The id a paint's `url(#id)` names, its fallback after it ignored.
fn referenced(paint: &str) -> Option<&str> {
    let inside = paint.trim_start().strip_prefix("url(")?;
    let (target, _) = inside.split_once(')')?;
    target
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .strip_prefix('#')
}

/// Whether a `radialGradient` whose attributes `own` reads places its focus
/// off its centre: an `fx` or `fy` other than its `cx` or `cy`.
fn off_centre<'a>(own: &impl Fn(&str) -> Option<&'a str>) -> bool {
    let centre = |key| own(key).unwrap_or(CENTRE);
    [("fx", "cx"), ("fy", "cy")]
        .into_iter()
        .any(|(focus, centre_key)| {
            own(focus).is_some_and(|focus| focus != centre(centre_key))
        })
}

#[cfg(test)]
mod tests {
    use skia_safe::{FontMgr, Rect, Size, svg};

    use super::*;

    /// `body` as a document 200 by 80.
    fn document(body: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="80">{body}</svg>"#
        )
    }

    /// A rect filled red.
    const RECT: &str = r#"<rect width="20" height="20" fill="red"/>"#;

    /// The stops of a gradient from red to blue.
    const STOPS: &str = r#"<stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/>"#;

    /// What drawing [`document`] of `body` asks of a vector backend.
    fn judged(body: &str) -> VectorFeatures {
        features_of(document(body).as_bytes())
    }

    // Each case was measured: drawn into a canvas and exported as SVG, the
    // layered and the exotic come back as nothing or as other paint, and the
    // plain as the canvas drew them.
    #[test]
    fn a_layer_the_module_opens_is_noted() {
        let layered = [
            format!(r#"<g opacity="0.5">{RECT}{RECT}</g>"#),
            format!(r#"<g opacity="0.5">{RECT}</g>"#),
            format!(r#"<svg opacity="0.5">{RECT}</svg>"#),
            r#"<circle r="9" fill="red" stroke="black" opacity="0.5"/>"#.into(),
            r#"<g stroke="black"><circle r="9" opacity="0.5"/></g>"#.into(),
            r#"<circle r="9" style="opacity:50%;stroke:black"/>"#.into(),
            r#"<text x="1" y="9" opacity="0.5">A</text>"#.into(),
            format!(r#"<g mask="url(#m)">{RECT}</g>"#),
            r#"<circle r="9" mask="url(#m)"/>"#.into(),
        ];
        for body in layered {
            assert!(judged(&body).refused_by(VectorFeatures::LAYER), "{body}");
        }
        for body in [
            format!(r#"<g filter="url(#f)">{RECT}</g>"#),
            format!(r#"<g style="filter: url(#f)">{RECT}</g>"#),
        ] {
            assert!(
                judged(&body).refused_by(VectorFeatures::IMAGE_FILTER),
                "{body}"
            );
        }
    }

    #[test]
    fn paint_the_device_writes_otherwise_is_noted() {
        let exotic = [
            format!(
                r#"<defs><pattern id="p" width="4" height="4">{RECT}</pattern></defs><rect width="9" height="9" fill="url(#p)"/>"#
            ),
            format!(
                r#"<defs><pattern id="p" width="4" height="4">{RECT}</pattern></defs><rect width="9" height="9" style="stroke:url('#p') red"/>"#
            ),
            format!(
                r#"<linearGradient spreadMethod="reflect">{STOPS}</linearGradient>"#
            ),
            format!(
                r#"<radialGradient spreadMethod="repeat">{STOPS}</radialGradient>"#
            ),
            format!(r#"<radialGradient fx="0.7">{STOPS}</radialGradient>"#),
            format!(
                r#"<radialGradient cx="0.5" fy="20%">{STOPS}</radialGradient>"#
            ),
        ];
        for body in exotic {
            assert!(
                judged(&body).refused_by(VectorFeatures::EXOTIC_SHADER),
                "{body}"
            );
        }
    }

    // The controls: what the module draws without a layer, and paint the
    // device writes as it is, each measured to export as the canvas drew it.
    #[test]
    fn what_the_device_writes_as_drawn_is_plain() {
        let plain = [
            format!("{RECT}{RECT}"),
            r#"<circle r="9" fill="red" opacity="0.5"/>"#.into(),
            r#"<circle r="9" fill="none" stroke="black" opacity="0.5"/>"#
                .into(),
            r##"<use href="#u" opacity="0.5"/>"##.into(),
            format!(r#"<g filter="none" mask="none">{RECT}</g>"#),
            format!(
                r#"<g clip-path="url(#c)" style="mix-blend-mode:multiply;isolation:isolate">{RECT}</g>"#
            ),
            format!(r#"<g opacity="1" fill-opacity="0.5">{RECT}</g>"#),
            format!(
                r#"<style>g {{ opacity: 0.5; filter: url(#f) }}</style><g>{RECT}</g>"#
            ),
            format!(
                r#"<radialGradient spreadMethod="pad">{STOPS}</radialGradient>"#
            ),
            format!(
                r#"<radialGradient cx="0.4" fx="0.4" fr="0.2">{STOPS}</radialGradient>"#
            ),
            format!(
                r##"<radialGradient id="t" href="#u">{STOPS}</radialGradient>"##
            ),
            format!(
                r#"<defs><pattern id="p" width="4" height="4">{RECT}</pattern></defs>{RECT}"#
            ),
        ];
        for body in plain {
            assert_eq!(judged(&body), VectorFeatures::PLAIN, "{body}");
        }
    }

    #[test]
    fn a_document_that_cannot_be_read_through_is_embedded() {
        assert_eq!(features_of(b"<svg><g></svg>"), VectorFeatures::LAYER);
    }

    /// `body` drawn by the module into Skia's SVG device, as it writes it.
    fn written(body: &str) -> String {
        let source = document(body);
        let mut dom = svg::Dom::from_bytes(source.as_bytes(), FontMgr::new())
            .expect("a document");
        dom.set_container_size(Size::new(200.0, 80.0));
        let device = svg::Canvas::new(Rect::from_wh(200.0, 80.0), None);
        dom.render(&device);
        String::from_utf8_lossy(device.end().as_bytes()).into_owned()
    }

    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test skia_svg_writes_no_spread_method
    /// The device writes a gradient's tile mode nowhere, so a repeating or
    /// reflecting one is read back padded; this fails once it writes one.
    #[test]
    fn skia_svg_writes_no_spread_method() {
        let out = written(&format!(
            r#"<defs><linearGradient id="g" x2="0.2" spreadMethod="reflect">{STOPS}</linearGradient></defs><rect width="200" height="80" fill="url(#g)"/>"#
        ));
        assert!(out.contains("<linearGradient"), "{out}");
        assert!(!out.contains("spreadMethod"), "{out}");
    }

    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test skia_svg_swaps_two_point_circles
    /// The device writes a two-point conical gradient's end circle as the
    /// `radialGradient`'s focal circle, where SVG puts offset 0, and its
    /// start circle as `cx`, `cy` and `r`, with the stops in their order: a
    /// focus at (110, 40) in a circle at (100, 40) of radius 30 comes back
    /// with the two exchanged. This fails once the device writes the circles
    /// where SVG reads them.
    #[test]
    fn skia_svg_swaps_two_point_circles() {
        let out = written(&format!(
            r#"<defs><radialGradient id="g" gradientUnits="userSpaceOnUse" cx="100" cy="40" r="30" fx="110" fy="40">{STOPS}</radialGradient></defs><rect width="200" height="80" fill="url(#g)"/>"#
        ));
        assert!(
            out.contains(r#"fx="100" fy="40" fr="30" cx="110" cy="40" r="0""#),
            "{out}"
        );
    }
}
