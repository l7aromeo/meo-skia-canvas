// @ts-check

//
// The window layer, as far as it can be reached without an event loop.
//
// More of it than expected. A `Window` is built, configured and closed
// without a native window ever opening — that waits for the app to launch —
// so its defaults, its option validation and its canvas can all be checked
// on the real object, and the process still exits on its own afterwards,
// which is the part that would otherwise hang a test run.
//
// What is still uncovered, and is not pretended otherwise: `App.launch()`
// and everything downstream of it. The event loop, what a resize does to the
// canvas, what `fit` means once there are pixels on screen. Those need a
// display, and CI has none.
//

"use strict";

const { assert, describe, test } = require("../runner"),
  { App, Canvas, Window } = require("../../lib"),
  css = require("../../lib/classes/css");

// Whether a `Window` can be built here at all, and why not when it cannot.
//
// Opening one validates that there is a GPU to draw with, so on a machine
// without one -- a CI runner, a container, a server -- the constructor
// refuses and every test below that builds a window would fail for the
// environment rather than for the code. Asked once, by building the smallest
// window there is and closing it.
const noWindowing = (() => {
  try {
    new Window(1, 1, { visible: false }).close();
    return false;
  } catch (e) {
    return `no windowing support here: ${e.message}`;
  }
})();

describe("Window options", () => {
  test("takes the fit modes it documents and no others", () => {
    // `set fit(mode)` keeps the old mode when this says no, rather than
    // throwing — the Canvas API's rule for a property given something it
    // cannot use, and this layer follows it.
    for (const mode of [
      "none",
      "contain-x",
      "contain-y",
      "contain",
      "cover",
      "fill",
      "scale-down",
      "resize",
    ]) {
      assert.equal(css.fit(mode), true, `${mode} should be a fit mode`);
    }

    for (const mode of [
      "contain-z",
      "COVER",
      " cover",
      "cover ",
      "",
      "stretch",
      null,
      undefined,
      42,
      {},
      [],
    ]) {
      assert.equal(css.fit(mode), false, `${String(mode)} is not one`);
    }
  });

  test("takes the cursor names it documents and no others", () => {
    for (const icon of ["default", "none", "pointer", "crosshair", "grab"]) {
      assert.equal(css.cursor(icon), true, `${icon} should be a cursor`);
    }

    for (const icon of ["Pointer", "finger", "", null, undefined, 7, {}]) {
      assert.equal(css.cursor(icon), false, `${String(icon)} is not one`);
    }
  });

  test(
    "is built with its options applied and its canvas attached",
    { skip: noWindowing },
    () => {
      const window = new Window(200, 150, { title: "probe", visible: false });
      try {
        assert.equal(window.width, 200);
        assert.equal(window.height, 150);
        assert.equal(window.title, "probe");
        assert.ok(window.canvas instanceof Canvas, "it made itself a canvas");
        assert.equal(window.canvas.width, 200);
        assert.equal(window.canvas.height, 150);
      } finally {
        window.close();
      }
    },
  );

  test(
    "takes a canvas it is given rather than making one",
    { skip: noWindowing },
    () => {
      const canvas = new Canvas(64, 48);
      const window = new Window({ canvas, visible: false });
      try {
        assert.equal(window.canvas, canvas, "the same canvas, not a copy");
        assert.equal(window.width, 64);
        assert.equal(window.height, 48);
      } finally {
        window.close();
      }
    },
  );

  test(
    "refuses a bad cursor given to the constructor too",
    { skip: noWindowing },
    () => {
      // The constructor applies its options with `Object.assign`, which runs
      // the same setter, so the refusal reaches this path without the
      // constructor knowing anything about cursors. Worth asserting because
      // nothing at the call site says so.
      assert.throws(
        () => new Window(120, 90, { visible: false, cursor: "hand" }),
        TypeError,
        "a cursor the setter refuses is refused at construction",
      );

      const window = new Window(120, 90, {
        visible: false,
        cursor: "pointer",
      });
      try {
        assert.equal(window.cursor, "pointer", "a known one is taken");
      } finally {
        window.close();
      }
    },
  );

  test("refuses an app setting it cannot use", { skip: noWindowing }, () => {
    // Same rule, different receiver -- the defect is the silent discard, not
    // which object carries it. `eventLoop` is an enumeration (rule 2), `fps`
    // a number outside a permitted set (rule 4).
    assert.throws(
      () => (App.eventLoop = "nonsense"),
      TypeError,
      "an unknown event-loop mode is refused",
    );
    assert.throws(
      () => (App.fps = 0),
      RangeError,
      "a rate below one frame a second is refused",
    );
    assert.throws(() => (App.fps = NaN), RangeError);
  });

  test("refuses every setting it cannot use", { skip: noWindowing }, () => {
    // These used to keep the value they had and say nothing, which is
    // indistinguishable from success at the call site. The Canvas API does
    // ignore a value it cannot use, but that is spec-mandated for canvas
    // properties and `Window` is in no standard at all -- so AGENTS.md's
    // rules apply unchanged: a value outside an enumeration is a
    // `TypeError` (rule 2) and a number outside a permitted set is a
    // `RangeError` (rule 4).
    const window = new Window(120, 90, { visible: false });
    try {
      assert.throws(
        () => (window.fit = "nonsense"),
        TypeError,
        "an unknown fit is refused",
      );
      window.fit = "cover";
      assert.equal(window.fit, "cover", "a known one is taken");

      assert.throws(
        () => (window.cursor = "not-a-cursor"),
        TypeError,
        "an unknown cursor is refused",
      );
      assert.throws(
        () => (window.cursor = "hand"),
        TypeError,
        "`hand` is not one of them, whatever the declarations once said",
      );
      window.cursor = "pointer";
      assert.equal(
        window.cursor,
        "pointer",
        "`pointer` is the CSS name and is taken",
      );

      for (const prop of ["left", "top", "width", "height"]) {
        assert.throws(
          () => (window[prop] = NaN),
          RangeError,
          `a non-finite \`${prop}\` is refused`,
        );
        assert.throws(() => (window[prop] = Infinity), RangeError);
        window[prop] = 42;
        assert.equal(window[prop], 42, `a finite \`${prop}\` is taken`);
      }

      // The position, and only the position, has an unset state, and this is
      // pinned rather than reasoned because a future edit could quietly take
      // it away. `left` and `top` are `Option<f32>` on the Rust side, so an
      // unplaced window's position arrives as `null` in the event loop's
      // `geom` payload (`window_mgr.rs`, `get_geometry`) and as `undefined`
      // from this constructor. `#dispatch` assigns both through these
      // setters, and a throw there has no caller to receive it -- it takes
      // the loop with it.
      //
      // The loop itself needs a display and is not covered. The setter on
      // that path is, which is where the refusal would land.
      for (const unset of [null, undefined]) {
        window.left = 10;
        window.left = unset;
        assert.equal(window.left, unset, "an unset `left` is accepted");
        window.top = unset;
        assert.equal(window.top, unset, "an unset `top` is accepted");
      }
      assert.throws(
        () => (window.left = NaN),
        RangeError,
        "unset is not the same as unusable",
      );

      // `width` and `height` are plain `f32` there and never arrive unset.
      assert.throws(() => (window.width = null), RangeError);
      assert.throws(() => (window.height = undefined), RangeError);

      assert.throws(
        () => (window.page = 99),
        RangeError,
        "a page the canvas does not have is refused",
      );
      assert.throws(
        () => (window.canvas = 42),
        TypeError,
        "a value of the wrong kind entirely is a TypeError",
      );

      window.title = "renamed";
      assert.equal(window.title, "renamed");
    } finally {
      window.close();
    }
  });
});
