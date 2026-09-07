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
