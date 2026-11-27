// @ts-check

"use strict";

const { createHash } = require("crypto"),
  { assert, describe, test } = require("../runner"),
  { Canvas, ImageData, loadImage, FontLibrary } = require("../../lib");

/** An SVG document as a data URL, so no fixture file is involved. */
const svg = (attributes) =>
  "data:image/svg+xml;base64," +
  Buffer.from(
    `<svg xmlns="http://www.w3.org/2000/svg" ${attributes}>` +
      `<rect width="4" height="4" fill="#000"/></svg>`,
  ).toString("base64");

describe("an SVG with no size of its own", () => {
  test("is contained in the default object size", async () => {
    // CSS's default object size for a replaced element is 300 by 150, and an
    // undimensioned document is contained in it: whichever bound the aspect
    // ratio reaches first is the one that binds. Hanging the ratio from the
    // height instead left the width unbounded, so the 4:1 row below was 600
    // wide where a browser gives 300.
    //
    // 2:1 is the ratio at which the two rules agree, which is why it has to
    // sit beside a wider one and a taller one rather than alone.
    for (let [viewBox, expected] of [
      ["0 0 40 10", [300, 75]],
      ["0 0 40 20", [300, 150]],
      ["0 0 16 16", [150, 150]],
      ["0 0 10 40", [37.5, 150]],
    ]) {
      let image = await loadImage(svg(`viewBox="${viewBox}"`));
      assert.deepEqual([image.width, image.height], expected, viewBox);
    }
  });

  test("takes that size unchanged when it states no ratio", async () => {
    let bare = await loadImage(svg(""));
    assert.deepEqual([bare.width, bare.height], [300, 150]);
  });

  test("survives a viewBox with a zero side", async () => {
    // Dividing by it produced Infinity, 0 and NaN widths, which reached every
    // caller sizing a surface from the result.
    for (let viewBox of ["0 0 40 0", "0 0 0 40", "0 0 0 0"]) {
      let image = await loadImage(svg(`viewBox="${viewBox}"`));
      assert.ok(
        Number.isFinite(image.width) && Number.isFinite(image.height),
        `viewBox="${viewBox}" gave ${image.width}x${image.height}`,
      );
      assert.deepEqual([image.width, image.height], [300, 150], viewBox);
    }
  });

  test("takes the missing dimension from the ratio, not from itself", async () => {
    // A document stating one dimension used to square it -- a rule of this
    // crate's own that no clause names. CSS derives the missing side from the
    // aspect ratio, and from the default object size when there is no ratio.
    for (let [attributes, expected] of [
      ['width="100" viewBox="0 0 40 10"', [100, 25]],
      ['height="100" viewBox="0 0 40 10"', [400, 100]],
      ['width="100"', [100, 150]],
      ['height="100"', [300, 100]],
    ]) {
      let image = await loadImage(svg(attributes));
      assert.deepEqual([image.width, image.height], expected, attributes);
    }
  });

  test("a stated size is still read as stated", async () => {
    // The fallback must not reach a document that says what it wants.
    let sized = await loadImage(svg('width="40" height="20"'));
    assert.deepEqual([sized.width, sized.height], [40, 20]);
  });
});

describe("an SVG containing text", () => {
  // Registration is global to this process, so the aliases below are chosen
  // not to collide with anything else here. Nothing else in this file renders
  // text.
  const FACE = "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf";

  // Read before registering anything: `families` reports registered aliases
  // alongside system ones, so this has to be captured while it is still only
  // the system's.
  const SYSTEM = FontLibrary.families.slice();

  /** The first of `names` the system has, or its first family at all. */
  const systemFamily = (...names) =>
    names.find((name) => SYSTEM.includes(name)) ?? SYSTEM[0];

  // Which families exist is a property of the machine, so the two names below
  // are chosen from what is actually installed rather than assumed. A
  // hard-coded `Helvetica` passes here and inverts on a Linux runner that
  // does not have it: the system would not own the name, the registered face
  // would win, and the collision test would fail for the wrong reason.
  const COLLIDING = systemFamily(
    "Helvetica",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
    "Nimbus Sans",
  );
  // Distinct from `COLLIDING`, which `systemFamily` alone does not guarantee:
  // it falls back to `SYSTEM[0]` when it recognises none of the names, and on
  // a machine holding only DejaVu that is the same family `COLLIDING` picked.
  // The comparison below then registers a face under that name and asks
  // whether it differs from itself. Failed exactly that way on the AlmaLinux
  // container, which holds three DejaVu families and none of the monospace
  // names offered here.
  const OTHER_SYSTEM = (() => {
    const preferred = systemFamily(
      "Courier",
      "Courier New",
      "DejaVu Sans Mono",
      "Liberation Mono",
      "Nimbus Mono PS",
    );
    return preferred !== COLLIDING
      ? preferred
      : SYSTEM.find((name) => name !== COLLIDING);
  })();

  FontLibrary.use("UniqueTestFace", [FACE]);
  FontLibrary.use(COLLIDING, [FACE]);

  /** A document stating `family`, or stating no `font-family` for `null`. */
  const document = (family, attrs) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="60">` +
        `<text x="5" y="45" font-size="40"` +
        (attrs !== undefined
          ? ` ${attrs}`
          : family === null
            ? ""
            : ` font-family="${family}"`) +
        ` fill="#000">Wgq&#160;AVA</text></svg>`,
    ).toString("base64");

  /**
   * A hash of every pixel the document paints.
   *
   * Which face rendered cannot be settled by an extent: two faces with
   * similar metrics share a bounding box and cannot share every pixel. The
   * sample carries ascenders, a descender and a kerning pair for the same
   * reason.
   */
  const rendering = async (family, attrs) => {
    let image = await loadImage(document(family, attrs)),
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60);
    return createHash("sha256")
      .update(Buffer.from(data.buffer))
      .digest("hex")
      .slice(0, 12);
  };

  // Nothing else in this suite renders SVG text and no SVG fixture contains a
  // `<text>` element, which is why a process kill here went unnoticed through
  // a release -- and then through a second one, where it passed on macOS and
  // musl and killed the glibc and Windows legs.
  //
  // Passing here is not the same as the fault being absent. Whether the
  // process survives depends on the machine's own font manager: this renders
  // an unresolvable family, and a manager that answers a null family absorbs
  // it. macOS does, so this test passed on a developer's machine throughout.
  // `a_declining_system_manager_does_not_take_the_null_family_down` in
  // `src/node/font_library.rs` is the one that does not depend on the
  // platform -- it substitutes a manager that answers nothing.
  test("a document's text takes the upright face whatever order its family came in", async () => {
    // An upright face and one flagged oblique only by `fsSelection` bit 9,
    // which CoreText and an unpatched DirectWrite report alike: Skia's matcher
    // keeps whichever was registered first, so the library registers the face
    // its tables call upright first. The subsets hold `I` and `H`, which is
    // what is drawn: a glyph they lack would fall back to another family for
    // every order alike.
    const R = "tests/assets/fonts/Recursive";
    const UP = `${R}/Recursive-upright-subset.ttf`,
      FLAG = `${R}/Recursive-oblique-flag-subset.ttf`;
    FontLibrary.use("SvgOrderUpright", [UP]);
    FontLibrary.use("SvgOrderFlagged", [FLAG]);
    FontLibrary.use("SvgOrderU", [UP, FLAG]);
    FontLibrary.use("SvgOrderL", [FLAG, UP]);
    const drawn = async (family) => {
      const svg =
        `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">` +
        `<text x="60" y="250" font-size="200" font-family="${family}"` +
        ` fill="#000">IH</text></svg>`;
      const image = await loadImage(
        "data:image/svg+xml;base64," + Buffer.from(svg).toString("base64"),
      );
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.drawImage(image, 0, 0);
      return Buffer.from(ctx.getImageData(0, 0, 300, 300).data);
    };
    const upright = await drawn("SvgOrderUpright");
    assert.ok(
      !upright.equals(await drawn("SvgOrderFlagged")),
      "the faces differ",
    );
    assert.ok((await drawn("SvgOrderU")).equals(upright), "upright first");
    assert.ok((await drawn("SvgOrderL")).equals(upright), "flagged first");
  });

  test("a document's italic and oblique text take a family's oblique face whatever order it came in", async () => {
    // The flagged face ties with the upright one in Skia's matcher where the
    // platform reports it upright, so a slanted `font-style` is sent to the
    // family's oblique faces by name, as canvas text is. RecursiveStatic, whose
    // oblique face reports itself, is the control. Skia's SVG text synthesises
    // no slant, so every face is drawn as it is.
    const R = "tests/assets/fonts/Recursive";
    const UP = `${R}/Recursive-upright-subset.ttf`;
    const draw = async (body) => {
      const svg =
        `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">` +
        body +
        `</svg>`;
      const image = await loadImage(
        "data:image/svg+xml;base64," + Buffer.from(svg).toString("base64"),
      );
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.drawImage(image, 0, 0);
      return Buffer.from(ctx.getImageData(0, 0, 300, 300).data);
    };
    const text = (family, attrs = "") =>
      draw(
        `<text x="60" y="250" font-size="200" font-family="${family}"${attrs} fill="#000">IH</text>`,
      );
    FontLibrary.use("SvgSlantUpright", [UP]);
    const upright = await text("SvgSlantUpright");
    const wrong = [];
    for (const [name, leaning] of [
      ["Flagged", `${R}/Recursive-oblique-flag-subset.ttf`],
      ["Oblique12", `${R}/Recursive-oblique12-subset.ttf`],
    ]) {
      FontLibrary.use(`SvgSlant${name}Alone`, [leaning]);
      const leans = await text(`SvgSlant${name}Alone`);
      for (const [order, files] of [
        ["upright first", [UP, leaning]],
        ["leaning first", [leaning, UP]],
      ]) {
        const family = `SvgSlant${name}${order === "upright first" ? "U" : "L"}`;
        FontLibrary.use(family, files);
        for (const [style, want, face] of [
          ["normal", upright, "upright"],
          ["italic", leans, "leaning"],
          ["oblique", leans, "leaning"],
        ]) {
          if (!(await text(family, ` font-style="${style}"`)).equals(want))
            wrong.push(`${name}, ${order}: ${style} draws the ${face} face`);
        }
        // Inherited, and stated in `style`, the same.
        const inherited = await draw(
          `<g font-style="italic"><text x="60" y="250" font-size="200" font-family="${family}" fill="#000">IH</text></g>`,
        );
        if (!inherited.equals(leans))
          wrong.push(
            `${name}, ${order}: an inherited italic draws the leaning face`,
          );
        if (!(await text(family, ` style="font-style:oblique"`)).equals(leans))
          wrong.push(
            `${name}, ${order}: a style declaration draws the leaning face`,
          );
        // And an upright span inside slanted text goes back to the upright face.
        const back = await draw(
          `<text x="60" y="250" font-size="200" font-family="${family}" font-style="italic" fill="#000"><tspan font-style="normal">IH</tspan></text>`,
        );
        if (!back.equals(upright))
          wrong.push(
            `${name}, ${order}: a normal span in italic text draws the upright face`,
          );
      }
    }
    // A family with an italic face beside the tie keeps it for italic.
    const RALEWAY_ITALIC =
      "tests/assets/fonts/Raleway/Raleway-Italic-VariableFont_wght.ttf";
    FontLibrary.use("SvgSlantItalicAlone", [RALEWAY_ITALIC]);
    FontLibrary.use("SvgSlantMixed", [
      `${R}/Recursive-oblique-flag-subset.ttf`,
      UP,
      RALEWAY_ITALIC,
    ]);
    if (
      !(await text("SvgSlantMixed", ' font-style="italic"')).equals(
        await text("SvgSlantItalicAlone", ' font-style="italic"'),
      )
    )
      wrong.push("with an italic face: italic draws the italic face");
    // The family's oblique faces cannot be named by a document.
    const alias =
      "meo-skia-canvas private family: oblique faces of SvgSlantFlaggedU";
    if ((await text(alias)).equals(await text("SvgSlantFlaggedAlone")))
      wrong.push("the alias reaches the oblique face by name");
    assert.deepEqual(wrong, []);
  });

  test("a document's italic text takes a family's italic face whatever its weight", async () => {
    // The canvas test of the same name, for an SVG document: an italic asked
    // of a family with an upright 400 face and an italic 900 one draws the
    // italic face, by CSS Fonts 4 section 5.2's style before weight.
    const R = "tests/assets/fonts/Recursive";
    const draw = async (family) => {
      const svg =
        `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">` +
        `<text x="60" y="250" font-size="200" font-family="${family}" font-style="italic" fill="#000">IH</text></svg>`;
      const image = await loadImage(
        "data:image/svg+xml;base64," + Buffer.from(svg).toString("base64"),
      );
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.drawImage(image, 0, 0);
      return Buffer.from(ctx.getImageData(0, 0, 300, 300).data);
    };
    FontLibrary.use("SvgWeightItalicAlone", [
      `${R}/Recursive-italic900-subset.ttf`,
    ]);
    FontLibrary.use("SvgWeightItalicMixed", [
      `${R}/Recursive-upright-subset.ttf`,
      `${R}/Recursive-italic900-subset.ttf`,
    ]);
    assert.ok(
      (await draw("SvgWeightItalicMixed")).equals(
        await draw("SvgWeightItalicAlone"),
      ),
      "italic draws the italic face",
    );
  });

  test("a document's slanted text in a family with no slanted face is slanted as canvas text is", async () => {
    // CSS Fonts 4 section 5.2 synthesises a slant a family has no face for:
    // canvas text skews `italic` by a quarter and shears `oblique <angle>` to
    // tan(angle), 14deg where none is given, back for a negative one. An SVG
    // document's text is held to the same rule. A face that leans already, by
    // its reported slant or by `fsSelection` bit 9, is drawn as it is.
    const R = "tests/assets/fonts/Recursive";
    const slope = (data) => {
      const alpha = (x, y) => data[(y * 300 + x) * 4 + 3],
        rows = [];
      for (let y = 0; y < 300; y++) {
        let left = null;
        for (let x = 0; x < 300; x++)
          if (alpha(x, y) >= 128) {
            left = x;
            break;
          }
        if (left === null) continue;
        const below = left > 0 ? alpha(left - 1, y) : 0;
        rows.push([y, left - 1 + (128 - below) / (alpha(left, y) - below)]);
      }
      const inner = rows.slice(
          Math.floor(rows.length * 0.15),
          Math.floor(rows.length * 0.85),
        ),
        n = inner.length,
        my = inner.reduce((sum, [y]) => sum + y, 0) / n,
        mx = inner.reduce((sum, [, x]) => sum + x, 0) / n;
      let num = 0,
        den = 0;
      for (const [y, x] of inner) {
        num += (y - my) * (x - mx);
        den += (y - my) ** 2;
      }
      return num / den;
    };
    // `group`, where given, is the `font-style` of a `<g>` around the text;
    // `attrs`, where given, replaces the attribute `style` would write.
    const svg = async (family, style, group, attrs) => {
      const attr =
        attrs ??
        (style === undefined
          ? ""
          : style.includes(" ")
            ? `style="font-style:${style}"`
            : `font-style="${style}"`);
      const text = `<text x="100" y="250" font-size="200" font-family="${family}" ${attr} fill="#000">I</text>`;
      const doc =
        `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">` +
        (group === undefined
          ? text
          : `<g style="font-style:${group}">${text}</g>`) +
        `</svg>`;
      const image = await loadImage(
        "data:image/svg+xml;base64," + Buffer.from(doc).toString("base64"),
      );
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.drawImage(image, 0, 0);
      return slope(ctx.getImageData(0, 0, 300, 300).data);
    };
    const canvas = (family, style) => {
      const ctx = new Canvas(300, 300).getContext("2d");
      ctx.font = `${style === "normal" ? "" : style + " "}200px ${family}`;
      ctx.fillText("I", 100, 250);
      return slope(ctx.getImageData(0, 0, 300, 300).data);
    };
    const near = (got, want) => Math.abs(got - want) <= 0.003;
    const STYLES = [
      "normal",
      "italic",
      "oblique",
      "oblique 14deg",
      "oblique 10deg",
      "oblique -14deg",
    ];
    FontLibrary.use("SvgSynthUpright", [`${R}/Recursive-upright-subset.ttf`]);
    const wrong = [];
    for (const style of STYLES) {
      const got = await svg("SvgSynthUpright", style),
        want = canvas("SvgSynthUpright", style);
      if (!near(got, want))
        wrong.push(
          `upright face, ${style}: ${got.toFixed(4)} against ${want.toFixed(4)}`,
        );
    }
    // `font-style` sets the angle with the slant, so the two inherit
    // together, and a bare `oblique` leans by 14deg whatever an ancestor's
    // angle.
    for (const [group, style, as] of [
      ["oblique 10deg", undefined, "oblique 10deg"],
      ["oblique -14deg", "inherit", "oblique -14deg"],
      ["oblique 10deg", "oblique", "oblique"],
    ]) {
      const got = await svg("SvgSynthUpright", style, group),
        want = canvas("SvgSynthUpright", as);
      if (!near(got, want))
        wrong.push(
          `upright face, ${style ?? "no font-style"} in a group of ${group}: ${got.toFixed(4)} against ${want.toFixed(4)}`,
        );
    }
    // An `inherit` after an angle on the same element inherits the angle too.
    const reinherited = await svg(
        "SvgSynthUpright",
        undefined,
        "oblique -14deg",
        `font-style="oblique 10deg" style="font-style:inherit"`,
      ),
      reinheritedWant = canvas("SvgSynthUpright", "oblique -14deg");
    if (!near(reinherited, reinheritedWant))
      wrong.push(
        `upright face, oblique 10deg then inherit in a group of oblique -14deg: ${reinherited.toFixed(4)} against ${reinheritedWant.toFixed(4)}`,
      );
    // An angle outside -90deg to 90deg makes the declaration invalid, so the
    // text keeps the style it inherits.
    const outOfRange = await svg("SvgSynthUpright", "oblique 100deg");
    if (!near(outOfRange, 0))
      wrong.push(
        `upright face, oblique 100deg: ${outOfRange.toFixed(4)} against 0`,
      );
    // Beside an oblique face, an oblique of zero or a negative angle takes the
    // upright face, sheared, and a positive one the oblique face as it is.
    FontLibrary.use("SvgSynthPair", [
      `${R}/Recursive-upright-subset.ttf`,
      `${R}/Recursive-oblique12-subset.ttf`,
    ]);
    for (const style of [
      "italic",
      "oblique 10deg",
      "oblique 0deg",
      "oblique -14deg",
    ]) {
      const got = await svg("SvgSynthPair", style),
        want = canvas("SvgSynthPair", style);
      if (!near(got, want))
        wrong.push(
          `upright and oblique faces, ${style}: ${got.toFixed(4)} against ${want.toFixed(4)}`,
        );
    }
    // Faces that lean already take no skew on top.
    for (const [name, file] of [
      ["an oblique face", `${R}/Recursive-oblique12-subset.ttf`],
      ["a face flagged oblique", `${R}/Recursive-oblique-flag-subset.ttf`],
    ]) {
      const family = `SvgSynth ${name}`;
      FontLibrary.use(family, [file]);
      const own = await svg(family, "normal");
      for (const style of ["italic", "oblique", "oblique 10deg"]) {
        const got = await svg(family, style);
        if (!near(got, own))
          wrong.push(
            `${name}, ${style}: ${got.toFixed(4)} against its own ${own.toFixed(4)}`,
          );
      }
    }
    assert.deepEqual(wrong, []);
  });

  test("renders when the family cannot be resolved", async () => {
    // The trigger is a family that does not resolve, not an absent one: this
    // killed the process for a document naming any font the machine lacks.
    let missing = await rendering("ZzzNoSuchFamilyAnywhere");
    let absent = await rendering(null);
    assert.equal(
      missing,
      absent,
      "both fall back to the same face, and neither takes the process down",
    );

    let canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(await loadImage(document(null)), 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60);
    // This says the page is not blank and nothing more. It does not say that
    // our own font manager answered: Skia applies its own fallback while
    // rasterising, so ink can appear when `FontLibrary`'s provider returned
    // nothing. Reading a pass here as evidence that a face reached
    // `ex_ratio_for` is a mistake that has already been made once -- the two
    // are different lookups.
    assert.ok(
      data.some((_, i) => i % 4 === 3 && data[i] > 0),
      "the fallback has to paint something, or this passes on a blank page",
    );
  });

  test("uses a registered face whose name the system does not have", async () => {
    // The half that must not regress: SVG text resolves from one provider,
    // and this says a face registered under a name of its own is still found
    // there.
    //
    // Two distinct system families are a requirement rather than a
    // convenience -- the second is the control, and without it the
    // comparison is between the registered face and itself. A machine that
    // holds one usable family cannot run this, and should say so rather than
    // fail as though the product were wrong.
    assert.ok(
      OTHER_SYSTEM && OTHER_SYSTEM !== COLLIDING,
      `this needs two distinct system families and the machine offers ` +
        `${SYSTEM.length}: "${COLLIDING}" was claimed, leaving nothing to ` +
        `compare it against`,
    );
    let registered = await rendering("UniqueTestFace"),
      fallback = await rendering("ZzzNoSuchFamilyAnywhere"),
      system = await rendering(OTHER_SYSTEM);

    assert.notEqual(
      registered,
      fallback,
      "a registered face is not the fallback, or registration did nothing",
    );
    assert.notEqual(
      registered,
      system,
      "the control: this comparison can tell two faces apart",
    );
  });

  test("a registered face wins a name the system also has", async () => {
    // The case this exists for. Asking the system font manager first is what
    // stops an unresolvable family taking the process down, and it cost a
    // registration under a name the system holds -- `Helvetica`, `Arial` --
    // which lost to the system face. The document's family is now rewritten
    // to a private alias only the provider knows, so the ordering stands and
    // the caller's face wins anyway.
    //
    // The colliding name is registered above to the same file as
    // `UniqueTestFace`, so winning means rendering identically to it.
    assert.ok(
      SYSTEM.includes(COLLIDING),
      `${COLLIDING} has to be a family the system itself answers, or this ` +
        `test compares nothing -- it would pass on a machine without it for ` +
        `the wrong reason`,
    );

    let collided = await rendering(COLLIDING),
      registered = await rendering("UniqueTestFace"),
      fallback = await rendering("ZzzNoSuchFamilyAnywhere");

    assert.equal(
      collided,
      registered,
      `the registered face wins ${COLLIDING}, which the system also has`,
    );
    assert.notEqual(
      registered,
      fallback,
      "the control: the registered face is not what an unknown name gets, " +
        "or the comparison above is between two fallbacks",
    );
  });

  test("a claimed family named in a style declaration wins too", async () => {
    // The declaration form goes through the same substitution as the
    // attribute, so it must reach the same face.
    assert.equal(
      await rendering(COLLIDING, `style="font-family:${COLLIDING}"`),
      await rendering("UniqueTestFace"),
    );
  });

  test("a claimed family inside a list is left as written", async () => {
    // A list means "this, and failing that that". Rewriting one item would
    // change what the others fall back to, so the walk leaves lists alone --
    // deliberately, and the rendering is therefore the system's answer for
    // the first name rather than the registered face.
    assert.notEqual(
      await rendering(`${COLLIDING}, monospace`),
      await rendering("UniqueTestFace"),
      "a list is not substituted, so the registration does not win inside one",
    );
  });
});

describe("an SVG's text positions resolve at the dpi CSS fixes", () => {
  const document = (x) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="60">` +
        `<text x="${x}" y="45" font-size="20" fill="#000">|</text></svg>`,
    ).toString("base64");

  /** The first and last columns the document inks. */
  const columns = async (x) => {
    let image = await loadImage(document(x)),
      canvas = new Canvas(320, 60),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 60),
      lit = (col) => {
        for (let y = 0; y < 60; y++)
          if (data[(y * 320 + col) * 4 + 3] > 0) return true;
        return false;
      },
      inked = [];
    for (let col = 0; col < 320; col++) if (lit(col)) inked.push(col);
    return [inked[0], inked[inked.length - 1]];
  };

  // `x`, `y`, `dx` and `dy` on a text element are the four attributes the
  // parsed document cannot express -- skia-safe exposes them for reading only
  // -- so they are rewritten in the XML before Skia sees it. `loadImage`
  // reaches that through its own door rather than through `Svg::parse`, and
  // no Rust test executes this path.
  test("an inch positions text at 96 pixels, not 90", async () => {
    assert.deepEqual(await columns("1in"), await columns("96"));
    assert.notDeepEqual(
      await columns("1in"),
      await columns("90"),
      "the control: 90 is Skia's own answer for an inch and has to differ",
    );
  });

  test("a list of positions converts each item", async () => {
    // The case a scan over the document text could not have handled.
    let [first] = await columns("1in 2in");
    let [expected] = await columns("96 192");
    assert.equal(first, expected);
  });
});

describe("an SVG's font-relative lengths", () => {
  const document = (body, root = "") =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="80" ${root}>` +
        `${body}</svg>`,
    ).toString("base64");

  const rendering = async (body, root) => {
    let image = await loadImage(document(body, root)),
      canvas = new Canvas(320, 80),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 80);
    return createHash("sha256")
      .update(Buffer.from(data.buffer))
      .digest("hex")
      .slice(0, 12);
  };

  /// The inked width of a document, in pixels, or 0 if it painted nothing.
  ///
  /// A hash says two renderings differ and nothing about how. When `4ex`
  /// comes out equal to `2em` the question is which ratio was used, and that
  /// is a number: this reports it so a failure on a machine nobody here can
  /// reach says what happened rather than only that it happened.
  //
  // Whole pixels, so the ratio it implies is quantised: at `font-size="20"`
  // a `4ex` rect is 80 * ratio wide, and one pixel is 0.0125 of ratio. Two
  // faces whose x-heights are nearer than that report the same width. What
  // was measured here is that the fallback and the registered test face both
  // ink 42px -- which says they are within one pixel of each other and does
  // NOT say they are the same face, nor what either ratio is. Fine for
  // telling 0.5 from a real ratio, which is two pixels apart; not fine for
  // telling two real faces apart.
  const inkedWidth = async (body, root) => {
    let image = await loadImage(document(body, root)),
      canvas = new Canvas(320, 80),
      ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, 0);
    let { data } = ctx.getImageData(0, 0, 320, 80),
      left = 320,
      right = -1;
    for (let y = 0; y < 80; y++) {
      for (let x = 0; x < 320; x++) {
        if (data[(y * 320 + x) * 4 + 3] > 8) {
          if (x < left) left = x;
          if (x > right) right = x;
        }
      }
    }
    return right < 0 ? 0 : right - left + 1;
  };

  const text = (attrs) => `<text x="5" y="60" ${attrs} fill="#000">Wgq</text>`;
  const rect = (attrs) =>
    `<rect x="5" y="5" height="20" ${attrs} fill="#000"/>`;

  // Skia resolves neither `em` nor `ex`: `SkSVGLengthContext::resolve` has no
  // case for them and returns 0. So each of these rendered nothing, or -- for
  // a percentage font-size -- rendered several times too large.
  test("a font-size in em resolves against the inherited size", async () => {
    assert.equal(
      await rendering(`<g font-size="20">${text(`font-size="2em"`)}</g>`),
      await rendering(text(`font-size="40"`)),
      "text at 2em of 20 is text at 40, where it used to paint nothing",
    );
  });

  test("a font-size in per cent resolves the same way", async () => {
    assert.equal(
      await rendering(`<g font-size="20">${text(`font-size="200%"`)}</g>`),
      await rendering(text(`font-size="40"`)),
    );
  });

  test("a length in em resolves against its own element's size", async () => {
    assert.equal(
      await rendering(rect(`width="2em" font-size="20"`)),
      await rendering(rect(`width="40"`)),
    );
  });

  test("a font-size in em is measured against the parent, not itself", async () => {
    // The order that has two references on one element: the `g` computes to
    // 32 against the inherited 16, and the rect's own `2em` is 64 of those.
    // Chrome reports exactly that -- computed 32px, getBBox 64.
    assert.equal(
      await rendering(`<g font-size="2em">${rect(`width="2em"`)}</g>`),
      await rendering(rect(`width="64"`)),
    );
  });

  test("a style attribute sets the size, and beats the attribute", async () => {
    // Measured through a child in `em`, so what is under test is which value
    // this library resolves the em against. Comparing the two renderings
    // directly would pass either way: Skia already honours `style` itself,
    // so the assertion would be about Skia rather than about the walk. Read
    // from the attribute instead of the declaration and the rect is 20 wide.
    assert.equal(
      await rendering(
        `<g font-size="10" style="font-size:32">${rect(`width="2em"`)}</g>`,
      ),
      await rendering(rect(`width="64"`)),
      "CSS gives the declaration precedence, so the em is 32's",
    );
  });

  test("an unstated font-size is CSS's 16, not Skia's 24", async () => {
    // The rendering change this carries. Skia's initial value is 24 --
    // `fFontSize.init(SkSVGLength(24))` -- so text stating no size came out
    // half again too large. Chrome computes 16px for the same document.
    assert.equal(
      await rendering(text("")),
      await rendering(text(`font-size="16"`)),
    );
    assert.notEqual(
      await rendering(text("")),
      await rendering(text(`font-size="24"`)),
      "the control: 24 has to be distinguishable, or this says nothing",
    );
  });

  // A document naming no family, or one the machine does not have, draws in
  // the standard font, which is serif: Chrome 156 on macOS draws such text
  // in Times, as it does `font-family="serif"`, and its 40px "Hamburg" set at
  // x = 5 inks out to x = 155.
  test("text naming no usable family is drawn in the serif", async () => {
    const hamburg = (attrs) =>
      `<text x="5" y="60" font-size="40" ${attrs} fill="#000">Hamburg</text>`;
    const serif = await rendering(hamburg(`font-family="serif"`));
    assert.equal(await rendering(hamburg("")), serif, "no family");
    assert.equal(
      await rendering(hamburg(`font-family="NoSuchFamilyAnywhere"`)),
      serif,
      "a family the machine does not have",
    );
    if (process.platform === "darwin") {
      assert.equal(await inkedWidth(hamburg("")), 155 - 5, "Chrome 156's ink");
    }
  });

  test("an ex is the drawn face's x-height, not half an em", async () => {
    // Two faces rather than one number. The defect is `ex` taken as a
    // constant half an em instead of the face's x-height, and the property
    // that separates those is that a constant cannot vary by face: if any
    // two families give `4ex` different widths at one `font-size`, `ex` is
    // being resolved rather than assumed.
    //
    // The earlier form compared `4ex` against `2em` and called equality a
    // failure, on the stated grounds that "no real face has a ratio of 0.5".
    // Windows disproves that: `Segoe UI`, its default UI face, measures
    // exactly 0.5, so `4ex` and `2em` agree there and both are correct. The
    // test failed on a true reading for two days and three of the mechanisms
    // proposed for it were built on the same wrong premise.
    //
    // This form needs no face to differ from 0.5, only two faces to differ
    // from each other -- and it says so when the machine cannot offer that,
    // rather than reading a font-poor environment as a defect.
    const rect = (attrs) =>
      `<rect x="0" y="0" height="40" ${attrs} fill="#000"/>`;
    const exWidth = (family) =>
      inkedWidth(rect(`width="4ex" font-size="20" font-family="${family}"`));

    // The em control, which does not depend on the face at all: `2em` at 20
    // is 40 whatever is resolved, so a difference below is about `ex`.
    assert.equal(
      await rendering(rect(`width="2em" font-size="20"`)),
      await rendering(rect(`width="40"`)),
      "the control: `em` resolves, so a difference below is about `ex`",
    );

    // The first pair that differs. Bounded because each candidate costs a
    // render and one differing pair is the whole of the evidence.
    const candidates = FontLibrary.families.slice(0, 12);
    let baseline = null,
      found = null;
    for (const family of candidates) {
      const width = await exWidth(family);
      if (baseline === null) {
        baseline = { family, width };
      } else if (width !== baseline.width) {
        found = { family, width };
        break;
      }
    }

    assert.ok(
      baseline,
      "this machine reports no families, so it cannot resolve an `ex` at all",
    );
    assert.ok(
      found,
      `\`4ex\` measured the same on every one of ${candidates.length} ` +
        `families tried, all ${baseline.width}px. Either \`ex\` is a ` +
        `constant -- the defect this guards -- or this machine's families ` +
        `share an x-height to within a pixel, which is 0.0125 of the ratio ` +
        `at font-size 20. Tried: ${candidates.join(", ")}`,
    );
    assert.notEqual(
      found.width,
      baseline.width,
      `"${baseline.family}" and "${found.family}" have to differ, or the ` +
        `search above returned a pair that does not`,
    );
  });

  test("a size the document states is left where it is", async () => {
    // The control the size change needs: it separates "the root default
    // moved" from "everything got smaller". A document naming 24 still
    // renders at 24.
    let stated = await rendering(text(`font-size="24"`), `font-size="24"`);
    assert.equal(stated, await rendering(text(`font-size="24"`)));
    assert.notEqual(
      stated,
      await rendering(text(`font-size="16"`)),
      "and it is not quietly the new default instead",
    );
  });
});

describe("an SVG's references", () => {
  /** A document painting red, then `body`, as a data URL. */
  const document = (body) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2">` +
        `<rect width="4" height="2" fill="red"/>${body}</svg>`,
    ).toString("base64");

  /** What `src` draws at (1, 1) on a 4x2 canvas in `colorSpace`. */
  const drawn = async (src, colorSpace = "srgb") => {
    const canvas = new Canvas(4, 2, { colorSpace });
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.drawImage(await loadImage(src), 0, 0);
    return [...ctx.getImageData(1, 1, 1, 1, { colorSpace }).data];
  };

  /** A 4x2 PNG of one colour, written by a canvas in `colorSpace`. */
  const png = (fill, colorSpace = "srgb") => {
    const canvas = new Canvas(4, 2, { colorSpace });
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = fill;
    ctx.fillRect(0, 0, 4, 2);
    return `data:image/png;base64,${canvas.toBufferSync("png").toString("base64")}`;
  };

  // Chrome 156 draws each of these blue: SVG 2's `href`, which Skia's SVG
  // module did not read, on an <image>, a <use> and a gradient inheriting
  // its stops.
  test("an href names what xlink:href does", async () => {
    const blue = png("blue");
    for (const body of [
      `<image href="${blue}" width="4" height="2"/>`,
      `<defs><rect id="a" width="4" height="2" fill="blue"/></defs><use href="#a"/>`,
      `<defs><linearGradient id="g"><stop offset="0" stop-color="blue"/>` +
        `<stop offset="1" stop-color="blue"/></linearGradient>` +
        `<linearGradient id="h" href="#g"/></defs>` +
        `<rect width="4" height="2" fill="url(#h)"/>`,
    ]) {
      assert.deepEqual(await drawn(document(body)), [0, 0, 255, 255], body);
    }
  });

  // An image keeps its colours through the document: P3 green, drawn on a
  // display-p3 canvas, reads P3 green, as Chrome 158 reads it. Clipped to
  // sRGB on the way, as the crate's `Svg::rasterize` clips it, it reads
  // 117, 251, 76.
  test("an image in a wide space keeps its colours", async () => {
    const green = png("color(display-p3 0 1 0)", "display-p3");
    assert.deepEqual(
      await drawn(
        document(`<image href="${green}" width="4" height="2"/>`),
        "display-p3",
      ),
      [0, 255, 0, 255],
    );
  });
});

describe("an SVG's paint", () => {
  /** A 4x4 document whose square `body` paints, as a data URL. */
  const document = (body) =>
    "data:image/svg+xml;base64," +
    Buffer.from(
      `<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">${body}</svg>`,
    ).toString("base64");

  /** What `src` draws at (2, 2) on a 4x4 sRGB canvas. */
  const drawn = async (src) => {
    const canvas = new Canvas(4, 4);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.drawImage(await loadImage(src), 0, 0);
    return [...ctx.getImageData(2, 2, 1, 1).data];
  };

  /** What a `fillRect` in `css` reads at (2, 2) on the same canvas. */
  const filled = (css) => {
    const canvas = new Canvas(4, 4);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = css;
    ctx.fillRect(0, 0, 4, 4);
    return [...ctx.getImageData(2, 2, 1, 1).data];
  };

  // Skia's SVG module parses paints itself and reads hex, its own names,
  // rgb() and rgba() alone, so every other form drew the initial black
  // (#815). Each is drawn as the canvas draws the same text as a fillStyle,
  // a colour outside sRGB clipped to it: Chrome 156 fills
  // color(display-p3 0 1 0) 0, 255, 0 on an sRGB canvas.
  test("is the colour it names, in each form a fillStyle reads", async () => {
    const forms = [
      "#00cc0080",
      "rebeccapurple",
      "transparent",
      "rgb(0 204 0 / 50%)",
      "hsl(120 100% 40%)",
      "hwb(120 0% 20%)",
      "lab(70 -60 50)",
      "oklch(0.7 0.18 140)",
      "color(srgb-linear 0 0.5 0)",
      "color(display-p3 0 1 0)",
      "color(display-p3 1 0.5 0 / 0.5)",
    ];
    const bodies = (value) => [
      `<rect width="4" height="4" fill="${value}"/>`,
      `<rect width="4" height="4" style="fill: ${value}"/>`,
      `<g color="${value}"><rect width="4" height="4" fill="currentcolor"/></g>`,
      `<defs><linearGradient id="g"><stop offset="0" stop-color="${value}"/>` +
        `<stop offset="1" stop-color="${value}"/></linearGradient></defs>` +
        `<rect width="4" height="4" fill="url(#g)"/>`,
    ];
    const off = [];
    for (const form of forms) {
      const expected = filled(form);
      // A level apart where the colour is translucent: the document's
      // premultiplied raster rounds once more than the fill does.
      const allowed = expected[3] === 255 ? 0 : 1;
      for (const body of bodies(form)) {
        const got = await drawn(document(body));
        if (got.some((channel, i) => Math.abs(channel - expected[i]) > allowed))
          off.push(`${body}: ${got}, the canvas ${expected}`);
      }
    }
    assert.deepEqual(off, []);
  });

  // The control: a paint Skia reads already is drawn as the fill is.
  test("a hex paint Skia reads matches the fill", async () => {
    assert.deepEqual(
      await drawn(document(`<rect width="4" height="4" fill="#00cc00"/>`)),
      filled("#00cc00"),
    );
  });

  // A paint outside sRGB reaches a display-p3 canvas as it is, as Chrome
  // 156 reads fill="color(display-p3 0 1 0)" back as 0, 255, 0 there: the
  // paints, a gradient's stops and <feFlood>'s colour are held in extended
  // sRGB by containers/skia-svg-wide-colour.patch. `color`, and so
  // currentcolor, is an eight-bit SkColor in Skia and stays sRGB's green.
  test("keeps a wide colour on a wide canvas", async () => {
    const wide = "color(display-p3 0 1 0)";
    /** P3's green, and sRGB's green, as a display-p3 canvas stores them. */
    const [kept, clipped] = [
      [0, 255, 0, 255],
      [117, 251, 76, 255],
    ];
    const rect = (paint) => `<rect width="4" height="4" ${paint}/>`;
    const cases = [
      [rect(`fill="${wide}"`), kept],
      [rect(`style="fill: ${wide}"`), kept],
      [
        `<line x1="0" y1="2" x2="4" y2="2" stroke-width="4" stroke="${wide}"/>`,
        kept,
      ],
      [
        `<defs><linearGradient id="g"><stop offset="0" stop-color="${wide}"/>` +
          `<stop offset="1" stop-color="${wide}"/></linearGradient></defs>` +
          rect(`fill="url(#g)"`),
        kept,
      ],
      [
        `<defs><filter id="f" x="0" y="0" width="1" height="1" ` +
          `color-interpolation-filters="sRGB"><feFlood flood-color="${wide}"/>` +
          `</filter></defs>` +
          rect(`filter="url(#f)"`),
        kept,
      ],
      [`<g color="${wide}">${rect(`fill="currentcolor"`)}</g>`, clipped],
    ];
    const off = [];
    for (const [body, expected] of cases) {
      const canvas = new Canvas(4, 4, { colorSpace: "display-p3" });
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      ctx.drawImage(await loadImage(document(body)), 0, 0);
      const got = [
        ...ctx.getImageData(2, 2, 1, 1, { colorSpace: "display-p3" }).data,
      ];
      if (got.join() !== expected.join())
        off.push(`${body}: ${got}, expected ${expected}`);
    }
    assert.deepEqual(off, []);
  });

  // A line has no interior, so its fill paints nothing, as Chrome 156 draws
  // it: Skia's SVG module drew a line's fill as a hairline along it, so the
  // initial black showed with no stroke and through a translucent one
  // (#825).
  test("of a <line> paints no fill", async () => {
    const line = (paint) => `<line x1="0" y1="2" x2="4" y2="2" ${paint}/>`;
    const stroke = `stroke-width="4" stroke="rgba(0, 204, 0, 0.5)"`;
    const cases = [
      [line(""), [0, 0, 0, 0]],
      [line(`fill="red"`), [0, 0, 0, 0]],
      [line(stroke), filled("rgba(0, 204, 0, 0.5)")],
      // The control: with no fill it already draws its stroke alone.
      [line(`fill="none" ${stroke}`), filled("rgba(0, 204, 0, 0.5)")],
    ];
    const off = [];
    for (const [body, expected] of cases) {
      const got = await drawn(document(body));
      if (got.some((channel, i) => Math.abs(channel - expected[i]) > 1))
        off.push(`${body}: ${got}, expected ${expected}`);
    }
    assert.deepEqual(off, []);
  });

  // A filter's colour is sRGB like any other and is taken into the filter's
  // linear light for a linearRGB primitive, so a lone flood draws its own
  // colour in either space, as Chrome 156 draws it: Skia's SVG module took
  // the sRGB value as linear and encoded it once more, so #808080 drew 188
  // (#826).
  test("of an <feFlood> is its own colour in a linearRGB filter", async () => {
    const flood = (space, colour, opacity = "") =>
      `<defs><filter id="f" x="0" y="0" width="1" height="1"${space}>` +
      `<feFlood flood-color="${colour}"${opacity}/></filter></defs>` +
      `<rect width="4" height="4" filter="url(#f)"/>`;
    const off = [];
    for (const space of [
      "",
      ` color-interpolation-filters="linearRGB"`,
      // The control: in sRGB the colour already reaches the page unchanged.
      ` color-interpolation-filters="sRGB"`,
    ])
      for (const [colour, opacity, expected] of [
        ["#808080", "", filled("#808080")],
        ["#0c0", "", filled("#0c0")],
        ["#3a7fd0", "", filled("#3a7fd0")],
        ["#070707", "", filled("#070707")],
        ["rgba(0, 204, 0, 0.5)", "", filled("rgba(0, 204, 0, 0.5)")],
        ["#808080", ` flood-opacity="0.5"`, filled("rgb(128 128 128 / 0.5)")],
      ]) {
        const body = flood(space, colour, opacity);
        const allowed = expected[3] === 255 ? 0 : 1;
        const got = await drawn(document(body));
        if (got.some((channel, i) => Math.abs(channel - expected[i]) > allowed))
          off.push(`${body}: ${got}, expected ${expected}`);
      }
    assert.deepEqual(off, []);
  });

  test("of a light is its own colour in a linearRGB filter", async () => {
    // A flat surface lit from straight overhead, so the diffuse result is
    // the light's colour and the specular one too (N.H = 1). Skia's module
    // took `lighting-color`'s sRGB value as linear and encoded it once more:
    // diffuse #808080 read 188 where Chrome 156 reads 128. Each expected
    // value is Chrome's; #3a7fd0 lights at 59 rather than 58 because the
    // light is stored in an eight-bit layer in linear light, in both.
    const lit = (space, primitive) =>
      `<defs><filter id="f" x="0" y="0" width="1" height="1"${space}>` +
      `${primitive}</filter></defs><rect width="4" height="4" filter="url(#f)"/>`;
    const diffuse = (colour, kd = 1) =>
      `<feDiffuseLighting lighting-color="${colour}" diffuseConstant="${kd}" surfaceScale="0">` +
      `<feDistantLight azimuth="0" elevation="90"/></feDiffuseLighting>`;
    const specular = (colour) =>
      `<feSpecularLighting lighting-color="${colour}" specularConstant="1" specularExponent="1" surfaceScale="0">` +
      `<feDistantLight azimuth="0" elevation="90"/></feSpecularLighting>`;
    const linear = [
      [diffuse("#0c0"), [0, 204, 0, 255]],
      [specular("#0c0"), [0, 255, 0, 154]],
      [diffuse("#808080"), [128, 128, 128, 255]],
      [specular("#808080"), [255, 255, 255, 55]],
      [diffuse("#3a7fd0"), [59, 127, 208, 255]],
      [specular("#3a7fd0"), [74, 157, 255, 161]],
      [diffuse("#070707"), [13, 13, 13, 255]],
      // Lit in float, where Chrome rounds the light to a byte first: #111111
      // is byte 1 in linear light, and half of it Chrome draws as 0. See
      // docs/browser-differences.md.
      [diffuse("#111111", 0.5), [13, 13, 13, 255]],
      // A colour between two bytes reaches the light as it is, and Chrome
      // agrees: as the byte 18 it would light at 22.
      [diffuse("color(srgb 0.069 0.069 0.069)"), [13, 13, 13, 255]],
    ];
    const off = [];
    for (const [space, rows] of [
      ["", linear],
      [` color-interpolation-filters="linearRGB"`, linear],
      // The control: in sRGB the light was already its own colour.
      [
        ` color-interpolation-filters="sRGB"`,
        [
          [diffuse("#0c0"), [0, 204, 0, 255]],
          [specular("#0c0"), [0, 255, 0, 204]],
          [diffuse("#808080"), [128, 128, 128, 255]],
          [specular("#808080"), [255, 255, 255, 128]],
          [diffuse("#3a7fd0"), [58, 127, 208, 255]],
          [specular("#3a7fd0"), [71, 156, 255, 208]],
        ],
      ],
    ])
      for (const [primitive, expected] of rows) {
        const body = lit(space, primitive);
        const got = await drawn(document(body));
        if (got.some((channel, i) => channel !== expected[i]))
          off.push(`${body}: ${got}, expected ${expected}`);
      }
    assert.deepEqual(off, []);
  });
});

describe("a refusal takes the type the standard names", () => {
  /** The name and constructor of whatever `run` throws. */
  const thrown = (run) => {
    try {
      run();
      return "no throw";
    } catch (error) {
      return `${error.constructor.name}/${error.name}`;
    }
  };

  test("a zero dimension is an IndexSizeError, whichever door it came in", () => {
    // "If either the sw or sh arguments are zero, then throw an
    // "IndexSizeError" DOMException." Every entry point builds its buffer
    // through the one `ImageData` constructor, so all three answer alike --
    // they answered with a RangeError, and `getImageData(0, 0, 0, 0)` with a
    // TypeError about buffer length, which is internal arithmetic rather than
    // anything the caller wrote.
    let ctx = new Canvas(8, 8).getContext("2d");
    for (let [what, run] of [
      ["getImageData(0,0,0,0)", () => ctx.getImageData(0, 0, 0, 0)],
      ["getImageData(0,0,0,5)", () => ctx.getImageData(0, 0, 0, 5)],
      ["getImageData(0,0,5,0)", () => ctx.getImageData(0, 0, 5, 0)],
      ["createImageData(0,0)", () => ctx.createImageData(0, 0)],
      ["createImageData(2,0)", () => ctx.createImageData(2, 0)],
      ["new ImageData(0,0)", () => new ImageData(0, 0)],
      ["new ImageData(2,0)", () => new ImageData(2, 0)],
    ])
      assert.equal(thrown(run), "DOMException/IndexSizeError", what);
  });

  test("a buffer that cannot describe whole pixels is an InvalidStateError", () => {
    // Two different refusals where there was one. A length that is not a
    // whole number of pixels is `InvalidStateError`; a length that is whole
    // but does not match the dimensions asked for is `IndexSizeError`. Both
    // were one TypeError.
    assert.equal(
      thrown(() => new ImageData(new Uint8ClampedArray(6), 1)),
      "DOMException/InvalidStateError",
      "six bytes is not a whole number of four-byte pixels",
    );
    assert.equal(
      thrown(() => new ImageData(new Uint8ClampedArray(8), 3)),
      "DOMException/IndexSizeError",
      "two pixels is whole, and is not three across",
    );
  });

  test("an unknown pattern repetition is a SyntaxError", () => {
    // "If repetition is not identical to one of "repeat", "repeat-x",
    // "repeat-y", or "no-repeat", then throw a "SyntaxError" DOMException."
    // A different clause from the one above, naming a different exception --
    // which is why these are not one family with one answer.
    let ctx = new Canvas(8, 8).getContext("2d");
    assert.equal(
      thrown(() => ctx.createPattern(new Canvas(2, 2), "bogus")),
      "DOMException/SyntaxError",
    );
  });

  test("the refusals the standard does not name are left alone", () => {
    // The controls. An unrecognised `colorSpace` is a value outside an
    // enumeration, which WebIDL makes a TypeError -- rule 2, not rule 1 --
    // and the two cases that are not refusals at all must stay silent.
    let ctx = new Canvas(8, 8).getContext("2d");
    assert.equal(
      thrown(() => new ImageData(2, 2, { colorSpace: "bogus" })),
      "TypeError/TypeError",
    );
    assert.equal(
      thrown(() => ctx.getImageData(0, 0, -2, -2)),
      "no throw",
      "a negative size normalises rather than refusing",
    );
    assert.equal(
      thrown(() => ctx.createPattern(new Canvas(2, 2), null)),
      "no throw",
      "null repetition means repeat",
    );
    assert.equal(
      thrown(() => new ImageData(2, 2)),
      "no throw",
    );
  });
});

// Text in an SVG set in a face with a COLR table is drawn as fillText draws
// it, from the glyph's paint graph, rather than by the platform's scaler:
// CoreText on macOS 27.2 draws a COLRv0 glyph as nothing and one of COLRv1 in
// its own colours, and FreeType and DirectWrite draw both apart from Chrome
// (#663). The canvas draws the text under geometricPrecision, since the SVG
// module draws text unhinted.
describe("an SVG's text in a colour face", () => {
  const W = 200,
    H = 80;
  const fonts = {
    SvgTextColrV1:
      "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf",
    SvgTextColrV0: "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf",
    SvgTextPlain: "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
  };
  for (const [family, path] of Object.entries(fonts))
    FontLibrary.use(family, path);

  /** A CPU canvas of this size and its context. */
  const page = () => {
    const canvas = new Canvas(W, H);
    canvas.gpu = false;
    return canvas.getContext("2d");
  };
  /** The pixels of `body` as a document of this size, drawn as an image. */
  const drawn = async (body) => {
    const ctx = page();
    const xml = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">${body}</svg>`;
    ctx.drawImage(await loadImage(Buffer.from(xml)), 0, 0);
    return ctx.getImageData(0, 0, W, H).data;
  };
  /** The pixels `draw` leaves on a canvas whose text is set unhinted. */
  const painted = (draw) => {
    const ctx = page();
    ctx.textRendering = "geometricPrecision";
    draw(ctx);
    return ctx.getImageData(0, 0, W, H).data;
  };
  /** How many pixels stand more than a level apart, and by how much. */
  const apart = (a, b) => {
    let pixels = 0,
      far = 0;
    for (let i = 0; i < a.length; i += 4) {
      let d = 0;
      for (let k = 0; k < 4; k++)
        d = Math.max(d, Math.abs(a[i + k] - b[i + k]));
      if (d > 1) pixels++;
      far = Math.max(far, d);
    }
    return { pixels, far };
  };
  const inked = (data) => data.filter((_, i) => i % 4 === 3 && data[i]).length;
  const agree = (got, expected, label) => {
    assert.ok(inked(expected) > 0, `${label}: the canvas drew nothing`);
    assert.deepEqual(
      apart(got, expected).pixels,
      0,
      `${label}: ${JSON.stringify(apart(got, expected))}, the document inked ` +
        `${inked(got)} pixels and the canvas ${inked(expected)}`,
    );
  };

  test("is drawn as fillText draws it", async () => {
    for (const [family, entity, text, size] of [
      ["SvgTextColrV1", "&#xf0100;", "\u{f0100}", 72],
      ["SvgTextColrV0", "AB", "AB", 48],
    ]) {
      const got = await drawn(
        `<text x="10" y="70" font-family="${family}" font-size="${size}" ` +
          `fill="rgb(40,90,160)">${entity}</text>`,
      );
      const expected = painted((ctx) => {
        ctx.font = `${size}px ${family}`;
        ctx.fillStyle = "rgb(40,90,160)";
        ctx.fillText(text, 10, 70);
      });
      agree(got, expected, family);
    }
  });

  // The control: text in a face with no COLR table is the SVG module's
  // drawing, and agrees with fillText already.
  test("leaves plain text to the SVG module, which agrees already", async () => {
    const got = await drawn(
      `<text x="10" y="60" font-family="SvgTextPlain" font-size="48" ` +
        `fill="rgb(40,90,160)">AB</text>`,
    );
    const expected = painted((ctx) => {
      ctx.font = "48px SvgTextPlain";
      ctx.fillStyle = "rgb(40,90,160)";
      ctx.fillText("AB", 10, 60);
    });
    agree(got, expected, "plain");
  });

  test("draws a text mixing colour and plain runs whole", async () => {
    const got = await drawn(
      `<text x="10" y="60" font-family="SvgTextPlain" font-size="40" ` +
        `fill="#246">Ab<tspan font-family="SvgTextColrV1">&#xf0101;</tspan></text>`,
    );
    const expected = painted((ctx) => {
      ctx.fillStyle = "#246";
      ctx.font = "40px SvgTextPlain";
      ctx.fillText("Ab", 10, 60);
      const advance = ctx.measureText("Ab").width;
      ctx.font = "40px SvgTextColrV1";
      ctx.fillText("\u{f0101}", 10 + advance, 60);
    });
    agree(got, expected, "mixed");
  });

  // A document is written as the canvas draws. A glyph whose layers are
  // plain fills is written as those fills, from fillText and from an SVG
  // image alike, and draws back as the canvas does; one whose layer changes
  // what is under it -- a composite inside the glyph, or the text's opacity
  // -- is written as pixels from both, rather than dropped with the layer,
  // which the SVG backend has no way to write.
  // A page of floats keeps one recording, which a run shares between its
  // targets unless a document draws it otherwise, as it draws a colour glyph
  // without its layer.
  test("reaches an SVG export as fills from a page of floats", async () => {
    const floats = () => {
      const canvas = new Canvas(W, H, { colorType: "RGBAF16" });
      canvas.gpu = false;
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, W, H);
      return ctx;
    };
    const fromText = floats();
    fromText.textRendering = "geometricPrecision";
    fromText.font = "48px SvgTextColrV0";
    fromText.fillStyle = "rgb(40,90,160)";
    fromText.fillText("AB", 10, 70);
    const fromCanvas = floats();
    fromCanvas.drawCanvas(fromText.canvas, 0, 0);
    for (const [door, ctx] of [
      ["fillText", fromText],
      ["drawCanvas", fromCanvas],
    ]) {
      // The pixels first, so that the document cannot be handed the
      // recording they keep.
      const expected = ctx.getImageData(0, 0, W, H, { colorType: "rgba" }).data;
      const svg = (await ctx.canvas.toBuffer("svg")).toString();
      assert.doesNotMatch(svg, /<image\b/, `${door}: ${svg}`);
      assert.match(svg, /<path\b/, `${door}: ${svg}`);
      const back = page();
      back.drawImage(await loadImage(Buffer.from(svg)), 0, 0);
      agree(back.getImageData(0, 0, W, H).data, expected, door);
    }
  });

  test("reaches an SVG export as fillText's does", async () => {
    const count = (svg, tag) =>
      (svg.match(new RegExp(`<${tag}\\b`, "g")) ?? []).length;
    const exported = async (ctx) =>
      (await ctx.canvas.toBuffer("svg")).toString();
    // An opaque ground, so that an edge pixel a level apart premultiplied
    // is not read as several apart once its colour is divided by its alpha.
    const ground = (ctx) => {
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, W, H);
    };
    for (const [family, entity, text, size, opacity, asFills] of [
      ["SvgTextColrV1", "&#xf0b00;", "\u{f0b00}", 72, 1, true],
      ["SvgTextColrV0", "AB", "AB", 48, 1, true],
      ["SvgTextColrV1", "&#xf0600;", "\u{f0600}", 72, 1, false],
      ["SvgTextColrV0", "AB", "AB", 48, 0.5, false],
    ]) {
      const fromImage = page();
      ground(fromImage);
      fromImage.drawImage(
        await loadImage(
          Buffer.from(
            `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">` +
              `<text x="10" y="70" font-family="${family}" font-size="${size}" ` +
              `fill="rgb(40,90,160)" opacity="${opacity}">${entity}</text></svg>`,
          ),
        ),
        0,
        0,
      );
      const draw = (ctx) => {
        ground(ctx);
        ctx.globalAlpha = opacity;
        ctx.font = `${size}px ${family}`;
        ctx.fillStyle = "rgb(40,90,160)";
        ctx.fillText(text, 10, 70);
      };
      const fromText = page();
      fromText.textRendering = "geometricPrecision";
      draw(fromText);
      // The same text in a canvas drawn into another, which reaches the
      // document as the entries it holds.
      const fromCanvas = page();
      const source = page();
      source.textRendering = "geometricPrecision";
      draw(source);
      fromCanvas.drawCanvas(source.canvas, 0, 0);
      const expected = painted(draw);
      for (const [door, ctx] of [
        ["an SVG image", fromImage],
        ["fillText", fromText],
        ["drawCanvas", fromCanvas],
      ]) {
        const svg = await exported(ctx);
        const label = `${family} ${JSON.stringify(text)} at ${opacity} from ${door}`;
        if (!asFills) {
          assert.ok(count(svg, "image") > 0, `${label}: ${svg}`);
          continue;
        }
        assert.equal(count(svg, "image"), 0, `${label}: ${svg}`);
        assert.ok(count(svg, "path") > 0, `${label}: ${svg}`);
        const back = page();
        back.drawImage(await loadImage(Buffer.from(svg)), 0, 0);
        agree(back.getImageData(0, 0, W, H).data, expected, label);
      }
    }
  });
});

// Skia's SVG module draws some of a document through a layer -- an opacity on
// a group, a mask, a filter -- which Skia's SVG device drops whole, and paints
// some of it in paint the device writes as other paint: a pattern, a repeating
// gradient, a radial one whose focus is off its centre. A document holding any
// is embedded in an SVG export as pixels, and the rest stays vectors.
describe("an SVG image in an SVG export", () => {
  const W = 200,
    H = 80;
  const R = `<rect x="20" y="10" width="120" height="60" fill="rgb(40,90,160)"/>`;
  const C = `<circle cx="100" cy="40" r="30" fill="rgb(200,60,40)"/>`;
  const STOPS = `<stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/>`;
  const F = `<defs><filter id="f"><feGaussianBlur stdDeviation="3"/></filter></defs>`;
  const FILL = `<rect width="200" height="80" fill="url(#g)"/>`;
  FontLibrary.use(
    "SvgExportPlain",
    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
  );

  /** A white CPU canvas of this size and its context. */
  const page = () => {
    const canvas = new Canvas(W, H);
    canvas.gpu = false;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "white";
    ctx.fillRect(0, 0, W, H);
    return ctx;
  };
  /** A canvas holding `body` as a document of this size, and its export. */
  const exported = async (body) => {
    const ctx = page();
    const xml = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">${body}</svg>`;
    ctx.drawImage(await loadImage(Buffer.from(xml)), 0, 0);
    return [ctx, (await ctx.canvas.toBuffer("svg")).toString()];
  };
  /** How many bytes of `svg` drawn back stand more than a level from `ctx`. */
  const apart = async (svg, ctx) => {
    const back = page();
    back.drawImage(await loadImage(Buffer.from(svg)), 0, 0);
    const [a, b] = [ctx, back].map((c) => c.getImageData(0, 0, W, H).data);
    return a.filter((byte, i) => Math.abs(byte - b[i]) > 1).length;
  };

  test("embeds what the SVG device would drop or repaint", async () => {
    for (const [kind, body] of [
      ["a filter", `${F}<g filter="url(#f)">${R}</g>`],
      ["a filter declared in style", `${F}<g style="filter:url(#f)">${R}</g>`],
      [
        "a mask",
        `<defs><mask id="m"><rect width="100" height="80" fill="white"/></mask></defs><g mask="url(#m)">${R}</g>`,
      ],
      ["a group's opacity", `<g opacity="0.5">${R}${C}</g>`],
      ["a nested svg's opacity", `<svg opacity="0.5">${R}${C}</svg>`],
      [
        "the opacity of a shape both filled and stroked",
        `${R}<circle cx="100" cy="40" r="30" fill="rgb(200,60,40)" stroke="black" stroke-width="6" opacity="0.5"/>`,
      ],
      [
        "a text's opacity",
        `<text x="10" y="60" font-family="SvgExportPlain" font-size="48" fill="rgb(40,90,160)" opacity="0.5">AB</text>`,
      ],
      [
        "a pattern",
        `<defs><pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5" fill="red"/></pattern></defs><rect width="200" height="80" fill="url(#p)"/>`,
      ],
      [
        "a radial gradient's focus off its centre",
        `<defs><radialGradient id="g" fx="0.7">${STOPS}</radialGradient></defs>${FILL}`,
      ],
      [
        "a reflected gradient",
        `<defs><linearGradient id="g" x2="0.2" spreadMethod="reflect">${STOPS}</linearGradient></defs>${FILL}`,
      ],
    ]) {
      const [ctx, svg] = await exported(body);
      assert.match(svg, /<image\b/, `${kind}: ${svg}`);
      assert.equal(await apart(svg, ctx), 0, kind);
    }
  });

  // The controls: what the device writes as drawn stays vectors.
  test("writes the rest of a document as vectors", async () => {
    for (const [kind, body] of [
      ["shapes", `${R}${C}`],
      [
        "a clip-path",
        `<defs><clipPath id="c"><circle cx="80" cy="40" r="30"/></clipPath></defs><g clip-path="url(#c)">${R}</g>`,
      ],
      [
        "a mix-blend-mode, which the module does not apply",
        `${R}<circle cx="100" cy="40" r="30" fill="rgb(200,60,40)" style="mix-blend-mode:multiply"/>`,
      ],
      [
        "the opacity of a shape only filled",
        `${R}<circle cx="100" cy="40" r="30" fill="rgb(200,60,40)" opacity="0.5"/>`,
      ],
      [
        "a padded gradient",
        `<defs><linearGradient id="g">${STOPS}</linearGradient></defs>${FILL}`,
      ],
      [
        "a radial gradient from its centre",
        `<defs><radialGradient id="g">${STOPS}</radialGradient></defs>${FILL}`,
      ],
    ]) {
      const [ctx, svg] = await exported(body);
      assert.doesNotMatch(svg, /<image\b/, `${kind}: ${svg}`);
      assert.match(svg, /<(path|rect)\b/, `${kind}: ${svg}`);
      assert.equal(await apart(svg, ctx), 0, kind);
    }
  });
});
