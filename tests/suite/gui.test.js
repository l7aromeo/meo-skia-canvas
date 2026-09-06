// @ts-check

//
// The window layer, in two halves that run in different places.
//
// Most of it needs no event loop and no display. A `Window` is built,
// configured and closed without a native window ever opening -- that waits
// for the app to launch -- so its defaults, its option validation and its
// canvas are all checked on the real object, everywhere the suite runs, and
// the process still exits on its own afterwards.
//
// The rest needs a display, and now has one somewhere: the Linux job
// installs a software Vulkan device and a virtual screen, so `App.launch()`,
// the event loop and the `geom` payload it delivers are exercised there.
// Those tests skip where no window can be built, and that leg sets
// `MEO_SKIA_CANVAS_REQUIRE_WINDOWING` so a skip is a failure rather than a
// quiet pass.
//
// Still uncovered, and not pretended otherwise: what a resize does to the
// canvas, and what `fit` means once there are pixels on screen.
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

// A leg that installs a software GPU and a virtual display has to fail when
// they do not work, rather than skipping quietly and reporting green -- which
// is the exact failure that leg exists to fix, arriving one level up.
//
// The two ways it can go wrong are the silent ones. Missing X client
// libraries fail loudly, because `new Window` succeeds and `App.launch` does
// not; a missing Vulkan device or a missing display make the constructor
// throw, and every test below would skip. Only the leg that promises
// windowing sets this.
if (noWindowing && process.env.MEO_SKIA_CANVAS_REQUIRE_WINDOWING) {
  throw new Error(
    `windowing was required and is not available: ${noWindowing}`,
  );
}

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
      // Eight names is short enough to list, so the message lists them
      // rather than naming a type to go and look up.
      assert.throws(
        () => (window.fit = "nonsense"),
        /"contain-x".*"scale-down"/,
        "an unknown fit is refused, and the message names the set",
      );
      window.fit = "cover";
      assert.equal(window.fit, "cover", "a known one is taken");

      assert.throws(
        () => (window.cursor = "not-a-cursor"),
        /CursorStyle/,
        "anything we did not remove falls back to naming the type",
      );
      // The message, not just the type. `"hand"` and `"arrow"` are the two
      // names the declarations used to list and the runtime never took, so
      // the callers this release breaks are exactly the ones passing them --
      // and they were already getting the default cursor in silence. Naming
      // the replacement is what turns the refusal into a repair. Asserted on
      // the text because a check for `TypeError` alone still passes with the
      // hints deleted.
      assert.throws(
        () => (window.cursor = "hand"),
        /use "pointer"/,
        "`hand` is refused, and the message names the CSS name to use",
      );
      assert.throws(
        () => (window.cursor = "arrow"),
        /use "default"/,
        "`arrow` likewise",
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

describe("the window and app refusals, without opening a window", () => {
  // These run everywhere, including a runner with no display, and that is
  // the point: every other test in this file is skipped when a `Window`
  // cannot be built, so the nine refusals added to this path were exercised
  // on one developer machine and nowhere else.
  //
  // What makes them portable is an ordering property of the constructor
  // rather than a stub. The `Object.assign` in `Window`'s constructor fires
  // the option setters, and the `open` event that reaches the native
  // `openWindow` -- the call that needs a GPU and fails on CI with "No
  // windowing support" -- is emitted after it. A refused option therefore
  // throws before anything native is touched.
  //
  // Named by construct rather than by line: this comment carried
  // `gui.js:384` and the line had moved by the same afternoon.
  //
  // Each case asserts that, by counting `open` events across the call. A
  // case that opened a window would both hang a headless runner and prove
  // the ordering had changed, so the count is the portability check rather
  // than a detail.
  const refuses = (label, error, pattern, build) =>
    test(label, () => {
      // `App.windows` rather than the internal event emitter: it is the
      // public reading of the same fact, and a window that opened would
      // appear here.
      let before = App.windows.length;
      assert.throws(build, error, pattern);
      assert.equal(
        App.windows.length,
        before,
        "no window was opened, so this runs anywhere",
      );
    });

  refuses(
    "a fit mode it does not know",
    TypeError,
    /FitStyle keyword/,
    () => new Window(1, 1, { fit: "bogus" }),
  );
  refuses(
    "a cursor it does not know",
    TypeError,
    /CursorStyle keyword/,
    () => new Window(1, 1, { cursor: "bogus" }),
  );
  refuses(
    "a page the canvas does not have",
    RangeError,
    /between 1 and/,
    () => new Window(1, 1, { page: 99 }),
  );
  refuses(
    "a size that is not a number",
    RangeError,
    /finite number/,
    () => new Window(1, 1, { width: NaN }),
  );
  refuses(
    "a position that is not a number",
    RangeError,
    /finite number/,
    () => new Window(1, 1, { left: "x" }),
  );
  refuses("a frame rate below one", RangeError, /at least 1/, () => {
    App.fps = 0;
  });
  refuses("a frame rate that is not a number", RangeError, /at least 1/, () => {
    App.fps = NaN;
  });
  refuses(
    "an event loop mode it does not know",
    TypeError,
    /"native" or "node"/,
    () => {
      App.eventLoop = "bogus";
    },
  );

  refuses(
    "a canvas of the wrong kind",
    TypeError,
    /Expected a Canvas/,
    () => new Window(1, 1, { canvas: 42 }),
  );

  test("and a canvas it can use is still accepted", () => {
    // The control for the refusal above: the check must not reject a real
    // `Canvas`, and the only way to tell the two apart without a display is
    // that this one gets *past* validation. It reaches the native open and
    // fails there on a machine with no GPU, so what is asserted is which
    // error arrives -- never a `TypeError` about the canvas.
    let reached = true;
    try {
      new Window(1, 1, { canvas: new Canvas(8, 8), visible: false }).close();
    } catch (e) {
      reached = !/Expected a Canvas/.test(e.message);
    }
    assert.ok(reached, "a real Canvas passes the constructor's check");
  });
});

describe("the event loop, where a window can be opened", () => {
  // `#dispatch` and the `geom` payload it parses had no test anywhere. The
  // reasoning that `left` and `top` accept `null` -- an unplaced window
  // reports no position -- came from reading the payload's type, and a throw
  // inside `#dispatch` has no caller to receive it: it takes the loop with
  // it. This runs the loop for a few frames and reads what arrives.
  //
  // Skipped where no window can be built. On CI that is every leg except the
  // one that installs a software Vulkan device and a virtual display, which
  // is why that leg exists.
  test(
    "delivers frames and a geometry the setters accept",
    { skip: noWindowing },
    async () => {
      let win = new Window(120, 80, { visible: false }),
        frames = [];

      win.on("frame", () => {
        frames.push({ left: win.left, top: win.top, page: win.page });
        if (frames.length >= 2) win.close();
      });

      // A window that never reports a frame would otherwise hold the loop
      // until the runner's own timeout. This closes it, which ends `launch`
      // in that case.
      //
      // It does not cover the other one: if `close()` returns and `launch`
      // still never settles, this fires and the `await` below hangs anyway.
      // `--test-timeout` on the command is what bounds that, because
      // `--test-force-exit` acts only once a run completes and `node --test`
      // has no per-test default -- measured, not assumed.
      let guard = setTimeout(() => {
        if (!win.closed) win.close();
      }, 10_000);

      App.eventLoop = "node";
      App.fps = 10;
      try {
        await App.launch();
      } finally {
        clearTimeout(guard);
      }

      assert.ok(
        frames.length >= 1,
        `the loop delivered no frames: ${JSON.stringify(frames)}`,
      );
      for (let { left, top, page } of frames) {
        // The values `geom` carries reach these setters, and the setters
        // accept them -- `left` and `top` are `null` before a window is
        // placed and a number afterwards, which is the case `finiteOr`'s
        // `unset` argument exists for.
        assert.ok(
          left === null || left === undefined || Number.isFinite(left),
          `left came back as ${JSON.stringify(left)}`,
        );
        assert.ok(
          top === null || top === undefined || Number.isFinite(top),
          `top came back as ${JSON.stringify(top)}`,
        );
        assert.ok(
          Number.isFinite(page) && page >= 1,
          `page came back as ${JSON.stringify(page)}`,
        );
      }
    },
  );
});
