// @ts-check

"use strict";

const { assert, describe, test } = require("../runner"),
  fs = require("fs"),
  os = require("os"),
  path = require("path");

const EXTRACTOR = path.join(
  __dirname,
  "../../scripts/api-surface/npm-items.mjs",
);

// The extractor is ESM and this suite is CommonJS, so it arrives by dynamic
// import. Loaded once: parsing the declarations is the expensive part.
const loaded = import(`file://${EXTRACTOR}`);

const scratch = (contents) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "parity-npm-")),
    file = path.join(dir, "index.d.ts");
  fs.writeFileSync(file, contents);
  return file;
};

describe("the npm parity surface", () => {
  test("holds the shape the interchange contract fixes", async () => {
    const { npmPayload } = await loaded,
      payload = npmPayload(path.join(__dirname, "../../lib/index.d.ts"));

    assert.equal(payload.surface, "npm");
    assert.equal(payload.generated_from, "lib/index.d.ts");
    assert.ok(payload.items.length > 0, "empty surface");

    // Asserted by the extractor too. Repeated here because the contract
    // says a downstream empty or duplicated result must not be mistakable
    // for agreement, and a self-assertion that nothing re-checks is one
    // edit away from being removed silently.
    const ids = payload.items.map((item) => item.id);
    assert.deepEqual(ids, [...ids].sort(), "not sorted by id");
    assert.equal(new Set(ids).size, ids.length, "duplicate ids");
    // `id` is composed from `owner` and `member`, not recovered from it. A
    // consumer that re-splits has to know the separator convention, and that
    // is where several counting errors came from on the other surface. This
    // asserts the two can never disagree, which is what makes the fields
    // worth carrying rather than a second thing to keep in step.
    let owned = 0;
    for (const item of payload.items) {
      assert.equal(typeof item.id, "string");
      assert.ok(item.kind, `${item.id} has no kind`);
      assert.equal(
        item.owner === null,
        item.member === null,
        `${item.id}: owner and member disagree on nullness`,
      );
      if (item.owner === null) continue;
      owned++;
      assert.equal(item.id, `${item.owner}.${item.member}`);
    }
    // Without this the loop above passes on a payload where every item is
    // top-level and the composition is never exercised.
    assert.ok(owned > 500, `only ${owned} items carry an owner`);
  });

  test("reaches declarations that carry no export keyword", async () => {
    // `export` is not the test for reachability: measured on tsc 5.9.3, an
    // unexported top-level type and an unexported interface both import
    // cleanly from the package by name where an absent name fails TS2305.
    // Filtering on the keyword would drop 54 declarations a caller can
    // write today, and drop them silently.
    const { npmPayload } = await loaded,
      ids = new Set(
        npmPayload(path.join(__dirname, "../../lib/index.d.ts")).items.map(
          (item) => item.id,
        ),
      );

    assert.ok(ids.has("GradientColorSpace"), "unexported type alias missing");
    assert.ok(ids.has("DOMPointInit"), "unexported interface missing");
    assert.ok(ids.has("ExportFormat"), "exported type alias missing");
  });

  test("emits union members, so an enum variant has something to pair with", async () => {
    // Without this a union is one id: `BlendMode` was 1 against the Rust
    // enum's 30, so no variant of any enum could ever pair -- 85% of one
    // lane's ids in the parity gate.
    //
    // Lane A's control pair, kept: one union that must parse and one name
    // that must not exist. Their first reader matched nothing and reported
    // all 27 unions absent, `BlendMode` included -- a broken instrument
    // reporting a clean tree, as 27 confident and false findings.
    const { npmSurface } = await loaded,
      items = npmSurface(path.join(__dirname, "../../lib/index.d.ts")).items,
      ids = new Set(items.map((item) => item.id));

    assert.ok(ids.has("BlendMode.source-over"), "union member missing");
    assert.ok(!ids.has("BlendMode.no-such-blend-mode"), "invented a member");

    // The third control, which is the one that fails for reading the AST
    // rather than matching quotes. A regex over the declaration text also
    // finds quoted strings in prose: it reports 53 members here where there
    // are 52, the extra being the word "destination" inside a comment.
    assert.equal(
      [...ids].filter((id) => id.startsWith("BlendMode.")).length,
      52,
      "BlendMode member count -- 53 means comments are being read as members",
    );

    // The fourth, and the sharper half of the same point.
    // `KeyboardEventProps` is a type literal with no union in it at all, and
    // a quote-matching reader invents two members from an example in its doc
    // comment. Zero is the only right answer -- but zero *union* members, not
    // zero members: it has eight properties, which the object-literal reader
    // now emits. Counting ids under that holder would have passed for the
    // wrong reason once that reader landed, so this counts variants.
    assert.equal(
      items.filter(
        (item) =>
          item.owner === "KeyboardEventProps" && item.kind === "variant",
      ).length,
      0,
      "invented union members for a type that has none",
    );
    // The control on the control: the holder is not empty, so a bug that
    // dropped it entirely could not satisfy the assertion above by accident.
    assert.equal(
      items.filter((item) => item.owner === "KeyboardEventProps").length,
      8,
    );

    // A count is a weak control: Lane A's asserted "at least 50 members" and
    // passed over a list of 53 containing five that do not exist. Exact
    // membership and no-duplicates are what actually catch that, so both are
    // here. `CompositeExtension` is one of the three the quote-matching
    // reader got wrong -- it reported 7 members against these 3.
    const composite = [...ids]
      .filter((id) => id.startsWith("CompositeExtension."))
      .sort();
    assert.deepEqual(composite, [
      "CompositeExtension.clear",
      "CompositeExtension.destination",
      "CompositeExtension.modulate",
    ]);

    // Spelling aliases each keep their own id. A caller can write either, so
    // folding them would hide which spellings exist; the manifest can pair
    // both to one Rust variant.
    assert.ok(ids.has("ColorSpace.display-p3") && ids.has("ColorSpace.p3"));
  });

  test("emits a union written on the property itself", async () => {
    // These have no named type, so nothing carried an id for them. Five
    // entries in another lane's manifest existed only as workarounds for
    // that, and are deletable now.
    const { npmSurface } = await loaded,
      ids = new Set(
        npmSurface(path.join(__dirname, "../../lib/index.d.ts")).items.map(
          (item) => item.id,
        ),
      );

    assert.ok(ids.has("CanvasRenderingContext2D.lineDashFit.turn"));
    assert.ok(ids.has("ExportOptions.chromaSampling.4:2:0"));

    // Lane A's fourth control, and the one that matters most here:
    // `repetition: string | null` is a plain string, so a reader that "found"
    // a union there would invent members that do not exist. None is the only
    // right answer, and this is the assertion that fails if the reader is
    // widened to accept any union rather than string literals.
    assert.equal(
      [...ids].filter((id) => id.includes(".repetition.")).length,
      0,
      "invented union members for a plain string",
    );
  });

  test("records a union of unions as a relation, not as members", async () => {
    // `GlobalCompositeOperation` is `CanvasCompositeOperation |
    // CompositeExtension` -- the string-side analogue of `extends`, so it
    // belongs in the same map. Flattening it would emit all 29 members a
    // second time under a holder that declares none of them, which is the
    // same duplication that member-to-declaring-interface attribution exists
    // to avoid.
    const { npmSurface } = await loaded,
      { items, heritage, alternatives } = npmSurface(
        path.join(__dirname, "../../lib/index.d.ts"),
      ),
      ids = new Set(items.map((item) => item.id));

    assert.deepEqual(heritage.GlobalCompositeOperation, [
      "CanvasCompositeOperation",
      "CompositeExtension",
    ]);

    // `heritage` is read downstream as "reaches the parent's members", so an
    // arm listed there lets its members claim the parent's names. That holds
    // for a union of literal unions and fails for a union of types:
    // `CanvasPatternSource = Canvas | Image | ImageData` says a value may be
    // any of the three, not that the three inherit from it. Listing it made
    // `Canvas.height` and `Image.height` both claim
    // `CanvasPatternSource.height`, and since those two holders are
    // unrelated, 52 collisions in the combined gate.
    //
    // Asserted over the payload rather than by naming the four types I know
    // about, so it covers the ones that would collide only once someone adds
    // a member: `Matrix` and `ColorMatrix` are the same shape and are clean
    // today by luck rather than by correctness.
    // The invariant is that the two relations are exclusive. `heritage` is
    // walked for member reachability and `alternatives` is not, so a name in
    // both would let its members claim the parent's names by the back door --
    // which is the defect the split exists to prevent, and it is what the
    // union-of-type-references emit produced.
    //
    // Deliberately NOT "every arm must be a known holder": `Image extends
    // EventEmitter`, which comes from "stream" and is legitimately absent
    // from this payload, and that version of the check fired on it. It would
    // not have caught the original defect either -- `Canvas`, `Image` and
    // `ImageData` are all real holders with members, which is exactly why
    // putting them in `heritage` was so damaging.
    for (const name of Object.keys(heritage))
      assert.ok(
        !(name in alternatives),
        `${name} is in both heritage and alternatives`,
      );

    // Containment, which does confer membership, in both its forms.
    assert.ok("GlobalCompositeOperation" in heritage); // union of literal unions
    assert.deepEqual(heritage.WindowOptions, ["CanvasOptions"]); // intersection

    // Alternation, which confers none.
    for (const name of ["CanvasPatternSource", "CanvasDrawable", "Matrix"])
      assert.ok(
        name in alternatives && !(name in heritage),
        `${name} should be alternation, not containment`,
      );

    // The information is kept, in a field that says what it means.
    assert.deepEqual(alternatives.CanvasPatternSource, [
      "Canvas",
      "Image",
      "ImageData",
    ]);
    assert.ok(!("CanvasPatternSource" in heritage));

    // The control, and the one that fails if the relation is flattened into
    // ids: the composite holder declares no members of its own, while both
    // of its arms do.
    assert.equal(
      [...ids].filter((id) => id.startsWith("GlobalCompositeOperation."))
        .length,
      0,
      "flattened a union of unions into duplicate member ids",
    );
    assert.ok(ids.has("CompositeExtension.modulate"));
    assert.ok(ids.has("CanvasCompositeOperation.source-over"));
  });

  test("emits members of a type alias whose body is an object", async () => {
    // `type X = { ... }` declares members exactly as `interface X { ... }`
    // does and a caller cannot tell which was used, but the walker descended
    // only through `node.members`, which a type alias does not have. Four
    // holders reported as memberless and 48 declared members never reached
    // the payload.
    //
    // The cost was not the count. `WindowOptions` was one of the four, so
    // `WindowSpec -> WindowOptions` could never be measured -- the holder
    // looked empty, so no overlap existed to find, and the alias went to the
    // wrong npm holder instead.
    const { npmSurface } = await loaded,
      { items, heritage } = npmSurface(
        path.join(__dirname, "../../lib/index.d.ts"),
      ),
      count = (holder) => items.filter((item) => item.owner === holder).length;

    // Must parse: the three plain object literals and the intersection.
    assert.equal(count("KeyboardEventProps"), 8);
    assert.equal(count("MouseEventProps"), 10);
    assert.equal(count("WindowEvents"), 16);
    assert.equal(count("WindowOptions"), 14);

    // An intersection confers membership where a union of named types does
    // not, so its named arm is containment and belongs in `heritage`.
    assert.deepEqual(heritage.WindowOptions, ["CanvasOptions"]);

    // Must NOT double-count: `ImageDataSettings` is an `interface`, already
    // read through the members path. If the new reader also walked it the
    // uniqueness assertion would fire, so this is the cheaper statement of
    // the same thing.
    assert.equal(count("ImageDataSettings"), 2);

    // And the negative that keeps the reader honest: a type alias whose body
    // is a union, not an object, must contribute no members of this kind.
    // Without it, a reader that treated every alias as an object would pass
    // everything above.
    assert.equal(
      items.filter(
        (item) => item.owner === "BlendMode" && item.kind !== "variant",
      ).length,
      0,
      "read a union alias as an object literal",
    );
  });

  test("emits members of a type written inline", async () => {
    // The same blind spot one level down. These three are reachable and were
    // invisible, which is also what made a brace-matching probe elsewhere
    // count them as members of the holder above.
    const { npmSurface } = await loaded,
      ids = new Set(
        npmSurface(path.join(__dirname, "../../lib/index.d.ts")).items.map(
          (item) => item.id,
        ),
      );
    for (const key of ["weight", "width", "slant"])
      assert.ok(
        ids.has(`TextStyleInput.fontStyle.${key}`),
        `nested member ${key} missing`,
      );
    // The container is still an item in its own right.
    assert.ok(ids.has("TextStyleInput.fontStyle"));
  });

  test("tracks a member appearing and disappearing", async () => {
    // A list that only grows is not tracking a surface. Both directions, on
    // a synthetic file so the assertion does not depend on what the real
    // declarations happen to contain today.
    const { npmSurface } = await loaded,
      withMember = scratch(
        `interface Widget { spin(turns: number): void; wobble: boolean; }\n`,
      ),
      withoutMember = scratch(`interface Widget { wobble: boolean; }\n`);

    const before = new Set(npmSurface(withMember).items.map((i) => i.id)),
      after = new Set(npmSurface(withoutMember).items.map((i) => i.id));

    assert.ok(before.has("Widget.spin"), "added member not extracted");
    assert.ok(!after.has("Widget.spin"), "removed member still extracted");
    // The control: something that did not change must not move, or the two
    // runs could differ for any reason at all and this would still pass.
    assert.ok(before.has("Widget.wobble") && after.has("Widget.wobble"));
    assert.ok(before.has("Widget") && after.has("Widget"));
  });

  test("collapses what is one capability and keeps what is two", async () => {
    const { npmSurface } = await loaded,
      file = scratch(
        [
          "interface Gadget {",
          "  poke(): void;",
          "  poke(times: number): void;", // overload
          "  get level(): number;",
          "  set level(value: number);", // accessor pair
          "  new (): Gadget;",
          "  (): void;",
          "  [key: string]: unknown;",
          "}",
          "type Alone = 1 | 2;",
        ].join("\n"),
      ),
      ids = npmSurface(file).items.map((i) => i.id);

    assert.equal(ids.filter((id) => id === "Gadget.poke").length, 1);
    assert.equal(ids.filter((id) => id === "Gadget.level").length, 1);
    // Unnamed signatures get an id rather than being dropped -- an
    // extractor that quietly omits things is how a parity gate reports
    // agreement it never checked.
    assert.ok(ids.includes("Gadget.new"));
    assert.ok(ids.includes("Gadget.()"));
    assert.ok(ids.includes("Gadget.[]"));
    assert.ok(ids.includes("Alone"));
  });

  test("attributes a member to the interface that declares it", async () => {
    // Deliberate deviation from the contract's example, which writes the
    // inheriting holder. Closing over `extends` would report 744 members
    // where 602 are declared, and one added `extends` clause would change
    // 142 ids at once -- a refactor reading as a surface change. The
    // closure stays recoverable through `heritage`.
    const { npmSurface } = await loaded,
      file = scratch(
        [
          "interface Base { shared(): void; }",
          "interface Derived extends Base { own(): void; }",
        ].join("\n"),
      ),
      { items, heritage } = npmSurface(file),
      ids = items.map((i) => i.id);

    assert.ok(ids.includes("Base.shared"));
    assert.ok(!ids.includes("Derived.shared"), "member attributed twice");
    assert.ok(ids.includes("Derived.own"));
    assert.deepEqual(heritage.Derived, ["Base"]);
  });
});
