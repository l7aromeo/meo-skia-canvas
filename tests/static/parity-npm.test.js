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
    for (const item of payload.items) {
      assert.equal(typeof item.id, "string");
      assert.ok(item.kind, `${item.id} has no kind`);
      if (item.owner !== null) assert.ok(item.id.startsWith(`${item.owner}.`));
    }
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
      ids = new Set(
        npmSurface(path.join(__dirname, "../../lib/index.d.ts")).items.map(
          (item) => item.id,
        ),
      );

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
    // comment. Zero is the only right answer.
    assert.equal(
      [...ids].filter((id) => id.startsWith("KeyboardEventProps.")).length,
      0,
      "invented union members for a type that has none",
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
      { items, heritage } = npmSurface(
        path.join(__dirname, "../../lib/index.d.ts"),
      ),
      ids = new Set(items.map((item) => item.id));

    assert.deepEqual(heritage.GlobalCompositeOperation, [
      "CanvasCompositeOperation",
      "CompositeExtension",
    ]);

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
