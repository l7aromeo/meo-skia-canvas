// @ts-check

//
// The window layer, as far as it can be reached without a window.
//
// `Window` opens one in its constructor, so a machine with no display cannot
// build one and CI has none — the event loop, the resize behaviour and the
// drawing that goes with them have no automated coverage and this does not
// give them any. What it does cover is the half that is a pure decision: the
// two vocabularies a window validates its options against, which decide
// whether a value is taken or quietly left alone, and which nothing else was
// pinning.
//

"use strict";

const { assert, describe, test } = require("../runner"),
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
});
