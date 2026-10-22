//! The JavaScript parameter forms the crate spells as methods of their own:
//! `DOMMatrix.scale` about an origin, and `Path2D.addPath` under a
//! transform, which the crate's `PathBuilder` takes. Expected values were
//! measured in Chrome 152.

use meo_skia_canvas::prelude::*;

fn parts(m: Affine) -> [f32; 6] {
    [m.a, m.b, m.c, m.d, m.tx, m.ty]
}

#[test]
fn scale_about_is_dom_matrix_scale_with_an_origin() {
    let origin = Point { x: 10.0, y: 20.0 };
    // `new DOMMatrix().scale(2, 3, 1, 10, 20)`.
    assert_eq!(
        parts(Affine::scale_about(2.0, 3.0, origin)),
        [2.0, 0.0, 0.0, 3.0, -10.0, -40.0]
    );
    // `new DOMMatrix([1, 2, 3, 4, 5, 6]).scale(2, 3, 1, 10, 20)`, which
    // composes the scale on the right, as `multiply` does.
    let m = Affine {
        a: 1.0,
        b: 2.0,
        c: 3.0,
        d: 4.0,
        tx: 5.0,
        ty: 6.0,
    };
    assert_eq!(
        parts(m.multiply(&Affine::scale_about(2.0, 3.0, origin))),
        [2.0, 4.0, 9.0, 12.0, -125.0, -174.0]
    );
}

#[test]
fn the_origin_of_a_scale_stays_where_it_is() {
    let origin = Point { x: 10.0, y: 20.0 };
    let m = Affine::scale_about(2.0, 3.0, origin);
    assert_eq!(
        (
            m.a * origin.x + m.c * origin.y + m.tx,
            m.b * origin.x + m.d * origin.y + m.ty
        ),
        (origin.x, origin.y)
    );
    assert_eq!(
        parts(Affine::scale_about(2.0, 3.0, Point { x: 0.0, y: 0.0 })),
        parts(Affine::scale(2.0, 3.0)),
        "about the origin it is the plain scale"
    );
}

#[test]
fn add_path_transformed_is_add_path_with_a_transform() {
    // `outer.addPath(inner, {a: 2, d: 3, e: 20, f: 30})` with `inner` a
    // 10px square at the origin, and `outer` already holding a 1px square.
    let mut inner = PathBuilder::new();
    inner.rect(0.0, 0.0, 10.0, 10.0);
    let inner = inner.build(FillRule::NonZero);
    let mut outer = PathBuilder::new();
    outer.rect(100.0, 100.0, 1.0, 1.0);
    outer.add_path_transformed(
        &inner,
        &Affine {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 3.0,
            tx: 20.0,
            ty: 30.0,
        },
    );
    let outer = outer.build(FillRule::NonZero);
    let probe = [
        (25.0, 35.0, true),
        (39.0, 59.0, true),
        (41.0, 35.0, false),
        (21.0, 61.0, false),
        (5.0, 5.0, false),
        (100.5, 100.5, true),
    ];
    for (x, y, inside) in probe {
        assert_eq!(outer.contains(x, y), inside, "({x}, {y})");
    }
}

#[test]
fn add_path_transformed_turns_the_added_path() {
    // `addPath(inner, new DOMMatrix().rotate(90))`: the square swings into
    // negative x.
    let mut inner = PathBuilder::new();
    inner.rect(0.0, 0.0, 10.0, 10.0);
    let inner = inner.build(FillRule::NonZero);
    let mut turned = PathBuilder::new();
    turned.add_path_transformed(&inner, &Affine::rotation_degrees(90.0));
    let turned = turned.build(FillRule::NonZero);
    assert!(turned.contains(-5.0, 5.0));
    assert!(!turned.contains(5.0, 5.0));
    assert!(!inner.contains(-5.0, 5.0), "the source path is not changed");
}
