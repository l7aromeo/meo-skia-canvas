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
  { Canvas, Window } = require("../../lib"),
  css = require("../../lib/classes/css");

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

  test("is built with its options applied and its canvas attached", () => {
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
  });

  test("takes a canvas it is given rather than making one", () => {
    const canvas = new Canvas(64, 48);
    const window = new Window({ canvas, visible: false });
    try {
      assert.equal(window.canvas, canvas, "the same canvas, not a copy");
      assert.equal(window.width, 64);
      assert.equal(window.height, 48);
    } finally {
      window.close();
    }
  });

  test("keeps the setting it had when given one it does not know", () => {
    // The same rule as a canvas property: a value it cannot use leaves the
    // old one in place rather than throwing. Checked on the window itself
    // and not only on the vocabulary behind it, because the setter is what
    // decides to consult that vocabulary at all.
    const window = new Window(120, 90, { visible: false });
    try {
      const fit = window.fit;
      const cursor = window.cursor;

      window.fit = "nonsense";
      assert.equal(window.fit, fit, "an unknown fit changed nothing");
      window.fit = "cover";
      assert.equal(window.fit, "cover", "a known one was taken");

      window.cursor = "not-a-cursor";
      assert.equal(window.cursor, cursor, "an unknown cursor changed nothing");
      window.cursor = "crosshair";
      assert.equal(window.cursor, "crosshair", "a known one was taken");

      window.title = "renamed";
      assert.equal(window.title, "renamed");
    } finally {
      window.close();
    }
  });
});
