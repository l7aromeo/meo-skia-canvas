//! `Context2D::filter_is_none` and `css_filter_is_none`: whether a filter
//! value is `none`, which is what decides a stacking context in CSS.
//!
//! The split between `none` and a filter that changes nothing was measured
//! in Chrome 156 (chrome-headless-shell 156.0.8078.47), by whether a
//! `z-index: -1` child paints above its parent's background: `none`,
//! `NONE`, `  none  ` and `/* x */ none /* y */` make no stacking context;
//! `blur(0px)`, `brightness(1)` and `blur(2px)` each make one.

use meo_skia_canvas::prelude::*;

/// Spellings of `none`, each of which made no stacking context in Chrome.
const NONE: [&str; 6] = [
    "none",
    "NONE",
    "  none  ",
    "\tNone\n",
    "/* x */ none /* y */",
    "/**/none",
];

/// Filters that change nothing or something, each of which made a stacking
/// context in Chrome or is the same kind of value as one that did.
const NOT_NONE: [&str; 7] = [
    "blur(0px)",
    "brightness(1)",
    "blur(2px)",
    "drop-shadow(0 0 0 transparent)",
    "blur(1em)",
    "blur(1rem) drop-shadow(1ex 1ch 1cap black)",
    "blur(2px",
];

#[test]
fn a_context_starts_with_no_filter() {
    let mut canvas = Canvas::new(4.0, 4.0);
    assert!(canvas.context().filter_is_none());
}

#[test]
fn every_spelling_of_none_is_none() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    for css in NONE {
        ctx.set_filter_css("blur(2px)").expect("a filter");
        ctx.set_filter_css(css).expect("none");
        assert!(ctx.filter_is_none(), "{css:?} on a context");
        assert!(css_filter_is_none(css).expect("none"), "{css:?}");
    }
}

#[test]
fn a_filter_that_changes_nothing_is_not_none() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    for css in NOT_NONE {
        ctx.set_filter_css("none").expect("none");
        ctx.set_filter_css(css).expect("a filter");
        assert!(!ctx.filter_is_none(), "{css:?} on a context");
        assert!(!css_filter_is_none(css).expect("a filter"), "{css:?}");
    }
}

#[test]
fn the_typed_setter_answers_the_same() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    ctx.set_filter(&[FilterOp::Blur(0.0)]).expect("a filter");
    assert!(!ctx.filter_is_none());
    ctx.set_filter(&[]).expect("none");
    assert!(ctx.filter_is_none());
}

#[test]
fn refusals_are_set_filter_css_refusals() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    let refused = [
        "",
        "   ",
        "/* only a comment */",
        "none blur(1px)",
        "blur(1px) none",
        "blur(-1px)",
        "nonsense(1)",
        "blur(1px) brightness(-1)",
    ];
    for css in refused {
        ctx.set_filter_css("blur(2px)").expect("a filter");
        let setter = ctx.set_filter_css(css).expect_err("refused");
        let string = css_filter_is_none(css).expect_err("refused");
        assert_eq!(string.to_string(), setter.to_string(), "{css:?}");
        assert_eq!(
            std::mem::discriminant(&string),
            std::mem::discriminant(&setter),
            "{css:?}"
        );
        // A refusal leaves the chain as it was.
        assert!(!ctx.filter_is_none(), "{css:?} cleared the chain");
        assert_eq!(ctx.filter(), "blur(2px)", "{css:?}");
    }
}
