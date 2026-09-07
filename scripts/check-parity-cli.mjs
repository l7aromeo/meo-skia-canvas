//
// The runnable half of the parity gate: a self-test, then the real check.
//
// The self-test exists because a gate that has never refused a real omission
// is a green that means nothing.
//
// **A FIXTURE MUST CARRY EVERY FIELD THE REAL PAYLOAD DOES.** The gate derives
// holder pairings from `renames` in the Rust payload, so a rust fixture
// without them is not a smaller version of the real tree -- it is one where
// `Context2D` pairs with nothing, and every case built on it quietly changes
// meaning. That was caught only because emptying the hand-written alias table
// turned eleven cases red at once; a partial move would have left some
// fixtures meaningful and the rest testing a tree that cannot exist, with
// nothing to announce it. Add a field to an extractor, add it here. Each case below is a tree the gate MUST
// reject, plus one it must accept -- without the accepting case the whole set
// is satisfied by a gate that fails everything.
//
import { readFileSync } from "node:fs";
import { check, report, normalise, displayName } from "./check-parity.mjs";
import { parseManifest } from "./parity/toml.mjs";

const RULES = JSON.parse(
  readFileSync(new URL("./parity/rules.json", import.meta.url), "utf8"),
);

// The real Rust payload carries the `js_names` renames and the gate derives
// its holder pairings from them, so a fixture without them is not a smaller
// version of the real thing -- it is a tree where `Context2D` pairs with
// nothing. Every rust fixture states the one rename its ids need.
const RENAMES = { Context2D: "CanvasRenderingContext2D" };

const surface = (name, ids) => ({
  surface: name,
  generated_from: "self-test fixture",
  ...(name === "rust" ? { renames: RENAMES } : {}),
  items: [...ids].sort().map((id) => ({ id, kind: "method", owner: null })),
});

// For the direct `normalise`/`displayName` calls, which take rules rather
// than a payload.
const RULES_WITH_RENAMES = { ...RULES, owner_aliases: RENAMES };

// A tree that is already correct: two capabilities pair by the naming rule,
// one is a registered single-surface decision. Everything else varies from
// this.
const BASE_RUST = [
  "Canvas::to_buffer",
  "Context2D::fill_rect",
  "Error::InvalidRadius",
];
const BASE_NPM = ["Canvas.toBuffer", "CanvasRenderingContext2D.fillRect"];
const BASE_MANIFEST = `
[[capability]]
name = "invalid-radius error variant"
rust = ["Error::InvalidRadius"]
npm  = []
why  = "Error is not exported to JavaScript; a bad radius throws IndexSizeError
        from the binding, which is registered as its own capability."
`;

function selfTest() {
  // COUNTED, NOT STATED. This was `cases.length + 25`, and the 25 was a
  // literal: already wrong before three more fixes added cases, and unable to
  // move when coverage moves. A number that cannot go up is not a measurement
  // of coverage, it is a decoration on one.
  //
  // Every assertion increments at its own site, including the ones inside
  // loops, so a case that runs per id counts per id.
  let cases = 0;
  const table = [
    ["the registered tree passes", BASE_RUST, BASE_NPM, BASE_MANIFEST, []],
    [
      "a Rust item added and not registered",
      [...BASE_RUST, "Context2D::set_letter_spacing"],
      BASE_NPM,
      BASE_MANIFEST,
      ["unregistered"],
    ],
    [
      "an npm item added and not registered",
      BASE_RUST,
      [...BASE_NPM, "CanvasRenderingContext2D.letterSpacing"],
      BASE_MANIFEST,
      ["unregistered"],
    ],
    [
      "an entry naming an id no extractor produced",
      BASE_RUST,
      BASE_NPM,
      BASE_MANIFEST +
        `
[[capability]]
name = "a capability that was removed"
rust = ["Context2D::vanished"]
npm  = ["CanvasRenderingContext2D.vanished"]
`,
      ["stale"],
    ],
    [
      "an empty side with no reason",
      BASE_RUST,
      BASE_NPM,
      `
[[capability]]
name = "invalid-radius error variant"
rust = ["Error::InvalidRadius"]
npm  = []
`,
      ["unexplained"],
    ],
    [
      "a reason that is too thin to be a reason",
      BASE_RUST,
      BASE_NPM,
      `
[[capability]]
name = "invalid-radius error variant"
rust = ["Error::InvalidRadius"]
npm  = []
why  = "internal"
`,
      ["unexplained"],
    ],
    [
      "a why that is contradicted by an item that does exist",
      BASE_RUST,
      BASE_NPM,
      BASE_MANIFEST +
        `
[[capability]]
name = "buffer export"
rust = []
npm  = ["Canvas.toBuffer"]
why  = "this is only in the binding, or so this sentence claims while the Rust
        side plainly has Canvas::to_buffer sitting right there."
`,
      ["unexplained"],
    ],
    [
      "an extractor that produced nothing",
      [],
      BASE_NPM,
      BASE_MANIFEST,
      ["input"],
    ],
    [
      "an extractor that produced an unsorted list",
      null, // built unsorted below
      BASE_NPM,
      BASE_MANIFEST,
      ["input"],
    ],
    [
      "a spelling no rule reaches is a missing rule, not a missing capability",
      [...BASE_RUST, "Canvas::to_data_thing"],
      [...BASE_NPM, "Canvas.toDataTHING"],
      BASE_MANIFEST,
      ["uncovered"],
    ],
    [
      // Required explicitly: map a holder to a WRONG counterpart that really
      // exists, and the gate must go red. A wrong holder pair is the one
      // failure with no natural signal -- it does not throw, it makes two
      // real capabilities look paired and the gate then reports agreement it
      // never checked. If this case were to pass, the mapping is not being
      // consulted at all.
      "a holder mapped to a real but wrong counterpart",
      BASE_RUST,
      BASE_NPM,
      BASE_MANIFEST,
      ["unregistered"],
      { owner_aliases: { Context2D: "Canvas" } },
    ],
    [
      "an owner alias naming a holder no extractor produced",
      BASE_RUST,
      BASE_NPM,
      BASE_MANIFEST,
      // `unregistered` too, and correctly: aliasing the holder to something
      // that does not exist also stops its members pairing. The point of the
      // case is that `stale` names the alias rather than leaving a reader to
      // infer it from the members that fell over.
      ["stale", "unregistered"],
      { owner_aliases: { Context2D: "NoSuchHolder" } },
    ],
    [
      "a member alias naming a member no extractor produced",
      [...BASE_RUST, "Affine::skew"],
      BASE_NPM,
      BASE_MANIFEST,
      ["stale", "unregistered"],
      { member_aliases: { skew: "skewSelf" } },
    ],
  ];

  let bad = 0;
  for (const [
    label,
    rustIds,
    npmIds,
    manifestText,
    wantKinds,
    override,
  ] of table) {
    const rust =
      rustIds === null
        ? {
            surface: "rust",
            generated_from: "self-test fixture",
            items: ["Context2D::fill_rect", "Canvas::to_buffer"].map((id) => ({
              id,
            })),
          }
        : surface("rust", rustIds);
    const problems = check({
      rust,
      npm: surface("npm", npmIds),
      manifest: parseManifest(manifestText, "self-test"),
      // The alias TABLES are emptied and the override supplies whatever a
      // case needs. These fixtures are three-item payloads, so every real
      // alias names a holder or member they do not contain and reports as
      // `stale` -- twenty of them at once, on every case, drowning the class
      // each case exists to provoke. That is a property of the fixture, not
      // of the rule under test.
      //
      // Nothing is lost by it: an alias naming something no extractor
      // produced is exactly what the REAL run's `stale` check catches,
      // against the real payload, which is the only place the question means
      // anything. The engine is what the fixtures test.
      rules: { ...RULES, owner_aliases: {}, member_aliases: {}, ...override },
    });
    const got = [...new Set(problems.map((p) => p.kind))].sort();
    const want = [...wantKinds].sort();
    cases += 1;
    if (got.join(",") !== want.join(",")) {
      console.error(
        `  self-test FAILED: ${label} -- wanted [${want}], got [${got}]`,
      );
      bad += 1;
    }
  }

  // The naming rule has to actually pair something, or every case above is
  // satisfied by a gate that pairs nothing and registers everything by hand.
  //
  // Asserted on the claimed set rather than on `displayName`, which returns
  // the shortest of the names an id claims. Owner aliases ADD rather than
  // replace, so `Context2D::fill_rect` now claims its own spelling as well
  // and that one is shorter -- a change in which name gets shown, not in
  // whether the rule pairs, and the sentence above says which of those this
  // case is about.
  const paired = normalise(
    "Context2D::fill_rect",
    RULES_WITH_RENAMES,
    {},
    undefined,
    "rust",
  );
  cases += 1;
  if (!paired.has("CanvasRenderingContext2D.fillRect")) {
    console.error(
      `  self-test FAILED: the naming rule does not pair, got [${[...paired]}]`,
    );
    bad += 1;
  }

  // And `displayName` picks ONE of those names for a message. Its choice
  // moved when owner aliases became additive -- silently, because nothing
  // pinned it -- so it is pinned now: the shortest, which is the id's own
  // spelling once an alias adds a longer one beside it.
  const shown = displayName(
    "Context2D::fill_rect",
    RULES_WITH_RENAMES,
    {},
    undefined,
    "rust",
  );
  cases += 1;
  if (shown !== "Context2D.fillRect") {
    console.error(
      `  self-test FAILED: displayName did not pick the shortest claimed name, got '${shown}'`,
    );
    bad += 1;
  }
  // The heritage closure is the whole reason the holder table is not
  // hand-maintained, so it needs its own case. `fillRect` is DECLARED on the
  // mixin `CanvasRect` and REACHED on `CanvasRenderingContext2D`; without the
  // closure it pairs with nothing and every context method reports missing.
  const MIXINS = {
    CanvasRenderingContext2D: ["CanvasRect"],
    Path2D: ["CanvasPath"],
  };
  const throughMixin = normalise("CanvasRect.fillRect", RULES, MIXINS);
  cases += 1;
  if (!throughMixin.has("CanvasRenderingContext2D.fillRect")) {
    console.error(
      `  self-test FAILED: a mixin member does not claim its reachable holder, got [${[...throughMixin]}]`,
    );
    bad += 1;
  }
  // A mixin extended by two holders claims both, because its members really
  // are reachable from both. Collapsing that to one would hide a gap.
  const shared = normalise("CanvasPath.lineTo", RULES, {
    CanvasRenderingContext2D: ["CanvasPath"],
    Path2D: ["CanvasPath"],
  });
  if (
    !shared.has("CanvasRenderingContext2D.lineTo") ||
    !shared.has("Path2D.lineTo")
  ) {
    console.error(
      `  self-test FAILED: a shared mixin does not claim both holders, got [${[...shared]}]`,
    );
    bad += 1;
  }
  // And the closure must not invent a pair between unrelated holders.
  cases += 1;
  if (normalise("Path2D.lineTo", RULES, MIXINS).has("Canvas.lineTo")) {
    console.error("  self-test FAILED: the closure paired unrelated holders");
    bad += 1;
  }

  // A declared suffix must not eat a name that merely ends in it. `_path` is
  // a suffix so `fill_path` reaches `fill`, and stripping it unconditionally
  // turned `close_path` into `close`, `begin_path` into `begin` and
  // `is_point_in_path` into `is_point_in` -- found on the real surface, where
  // all three then failed to pair.
  for (const [id, wanted] of [
    ["Context2D::close_path", "CanvasRenderingContext2D.closePath"],
    ["Context2D::begin_path", "CanvasRenderingContext2D.beginPath"],
    ["Context2D::is_point_in_path", "CanvasRenderingContext2D.isPointInPath"],
    ["Context2D::fill_path", "CanvasRenderingContext2D.fill"],
  ]) {
    cases += 1;
    if (!normalise(id, RULES_WITH_RENAMES, {}, undefined, "rust").has(wanted)) {
      console.error(
        `  self-test FAILED: '${id}' does not claim '${wanted}', got [${[...normalise(id, RULES_WITH_RENAMES, {}, undefined, "rust")]}]`,
      );
      bad += 1;
    }
  }

  // Acronym casing must survive the `Make` prefix. The two rules ran in the
  // other order, so `hsla_matrix` claimed `MakeHslaMatrix` where npm writes
  // `MakeHSLAMatrix`: the rule was present and applied to a string a later
  // step then rewrote. Real data caught it as `uncovered`, which is the
  // class working, but the ordering belongs pinned here.
  for (const [id, wanted] of [
    ["ColorFilter::hsla_matrix", "ColorFilter.MakeHSLAMatrix"],
    ["ColorFilter::srgb_to_linear_gamma", "ColorFilter.MakeSRGBToLinearGamma"],
    ["Canvas::to_data_url", "Canvas.toDataURL"],
  ]) {
    cases += 1;
    if (!normalise(id, RULES_WITH_RENAMES, {}, undefined, "rust").has(wanted)) {
      console.error(`  self-test FAILED: '${id}' does not claim '${wanted}'`);
      bad += 1;
    }
  }

  // The mis-targeted pairing: `BlendMode::Copy` matched against a
  // `GlobalCompositeOperation` spelling. `copy` is a convincing member name
  // and both halves read correctly on their own; what gives it away is that
  // the holders do not pair.
  //
  // IT WAS TAKEN FROM THE REAL SURFACE AND THE SURFACE HAS SINCE MOVED. Those
  // two holders now pair, by an alias measured at +32 with no collisions, and
  // `Copy` against `src` is a member alias beside it -- so the example is a
  // correct pairing today and the case would pass for the wrong reason. The
  // alias tables are emptied to keep the premise the case was written for:
  // two holders that do not pair. The shape it guards is unchanged.
  const misTargeted = check({
    rust: surface("rust", ["BlendMode::Copy"]),
    npm: surface("npm", ["GlobalCompositeOperation.copy"]),
    manifest: parseManifest(
      `
[[capability]]
name = "copy composite"
rust = ["BlendMode::Copy"]
npm  = ["GlobalCompositeOperation.copy"]
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!misTargeted.some((p) => p.kind === "unexplained")) {
    console.error(
      "  self-test FAILED: an entry pairing unpaired holders was accepted without a reason",
    );
    bad += 1;
  }
  // And the same entry with a reason is accepted, or the check refuses the
  // legitimate n:m case -- one Rust type answering to two npm ones.
  const explained = check({
    rust: surface("rust", ["Rect::bottom"]),
    npm: surface("npm", ["Path2DBounds.bottom"]),
    manifest: parseManifest(
      `
[[capability]]
name = "path bounds"
rust = ["Rect::bottom"]
npm  = ["Path2DBounds.bottom"]
why  = "Rust Rect answers to DOMRect by a declared rename and to Path2DBounds
        as what Path2D::bounds returns; only the first pairs by name."
`,
      "self-test",
    ),
    rules: RULES,
  });
  cases += 1;
  if (explained.some((p) => p.kind === "unexplained")) {
    console.error(
      "  self-test FAILED: a reasoned cross-holder entry was refused",
    );
    bad += 1;
  }

  // An entry may cover PART of a holder, and the rest of that holder still
  // pairs normally. Entries name ids rather than holders, so this works
  // today -- the case exists because it is exactly what would stop working
  // if the manifest lookup were ever made holder-keyed for speed, and
  // nothing else here would notice.
  //
  // Both of the shapes the options lane found rest on it: one Rust holder
  // answering to several npm holders, and two Rust members answering to one
  // npm member.
  const partial = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      // The holders must pair, or `blur` fails for that reason instead and
      // the case proves nothing about partial coverage. A first version of
      // this omitted the rename and reported three problems, none of which
      // was the one under test.
      renames: { TextShadow: "TextShadowInput" },
      items: [
        { id: "TextShadow::blur" },
        { id: "TextShadow::offset_x" },
        { id: "TextShadow::offset_y" },
      ],
    },
    npm: surface("npm", ["TextShadowInput.blur", "TextShadowInput.offset"]),
    manifest: parseManifest(
      `
[[capability]]
name = "shadow offset"
rust = ["TextShadow::offset_x", "TextShadow::offset_y"]
npm  = ["TextShadowInput.offset"]
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  // `blur` is outside the entry and pairs by name; the two offsets are inside
  // it. Nothing should be reported either way.
  cases += 1;
  if (partial.length > 0) {
    console.error(
      `  self-test FAILED: an entry covering part of a holder broke the rest of it, got ${JSON.stringify(partial.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A member alias is scoped to its holder, and the case that proves it is
  // the one a bare key gets wrong: two Rust holders declaring the same member
  // name, where only one of them is spelled differently on the npm side.
  // `height` is the real instance -- sixteen Rust holders declare it and only
  // `StrutStyle`'s is npm's `heightMultiplier`.
  const scoped = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: { Strut: "StrutIn", Box: "BoxIn" },
      items: [{ id: "Box::height" }, { id: "Strut::height" }],
    },
    npm: surface("npm", ["BoxIn.height", "StrutIn.heightMultiplier"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: {},
      member_aliases: { "Strut.height": "heightMultiplier" },
    },
  });
  cases += 1;
  if (scoped.length > 0) {
    console.error(
      `  self-test FAILED: a holder-scoped member alias reached another holder, got ${JSON.stringify(scoped.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And it ADDS rather than replaces: the member still claims the spelling it
  // is written with. Under replacing semantics `Solo::thing` would claim only
  // `SoloIn.other` and `SoloIn.thing` would report unregistered on both sides.
  const additive = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: { Solo: "SoloIn", Other: "OtherIn" },
      items: [{ id: "Other::other" }, { id: "Solo::thing" }],
    },
    npm: surface("npm", ["OtherIn.other", "SoloIn.thing"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: {},
      member_aliases: { "Solo.thing": "other" },
    },
  });
  cases += 1;
  if (additive.length > 0) {
    console.error(
      `  self-test FAILED: a member alias replaced the name as written, got ${JSON.stringify(additive.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A holder alias pairs the BARE TYPE ID too, not only the members reached
  // through it. An alias says two names are one holder, and the type id is
  // the holder -- so `Cursor` and `CursorStyle` are the same capability for
  // exactly the reason `Cursor.EResize` and `CursorStyle.e-resize` are.
  //
  // Nineteen aliases times two surfaces is about thirty-eight ids, and every
  // one of them would otherwise have to be written into the manifest as a
  // capability that is present on both sides.
  const bareAliased = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Cursor" }],
    },
    npm: surface("npm", ["CursorStyle"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: { Cursor: "CursorStyle" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (bareAliased.length > 0) {
    console.error(
      `  self-test FAILED: an aliased bare type id does not pair, got ${JSON.stringify(bareAliased.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // The other direction, which the fix must not cost: a type of the same name
  // on both surfaces and no alias at all still pairs on its own spelling.
  const bareUnaliased = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Point3" }],
    },
    npm: surface("npm", ["Point3"]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (bareUnaliased.length > 0) {
    console.error(
      `  self-test FAILED: an unaliased bare type id stopped pairing, got ${JSON.stringify(bareUnaliased.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A comment between entries must not be swallowed into the value above it.
  // The manifest reader is fail-closed by design; absorbing prose is the one
  // failure mode its own header argues against, because it corrupts a `why`
  // without refusing anything.
  const commented = parseManifest(
    `
[[capability]]
name = "first"
rust = ["A"]
npm  = []
why  = "A reason that is long enough to be a reason and ends here."

# A comment between two entries.

[[capability]]
name = "second"
rust = ["B"]
npm  = []
why  = "Another reason, also long enough to count as one."
`,
    "self-test",
  );
  cases += 1;
  if (commented[0].why.includes("comment")) {
    console.error(
      `  self-test FAILED: a comment was absorbed into the preceding why, got '${commented[0].why}'`,
    );
    bad += 1;
  }

  // A declared rename and its target are one type, so they are not a
  // collision -- the extractor emits `Affine` AND `DOMMatrix` for the single
  // type `src/lib.rs` re-exports under both names.
  const renamePair = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: { Affine: "DOMMatrix" },
      items: [{ id: "Affine" }, { id: "DOMMatrix" }],
    },
    npm: surface("npm", ["DOMMatrix"]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (renamePair.some((p) => p.kind === "collision")) {
    console.error(
      `  self-test FAILED: a declared rename collided with its own target, got ${JSON.stringify(renamePair.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And the excuse is narrow: a HAND-WRITTEN alias onto a name that is a real
  // second type is still a collision, which is the case the check exists for.
  const handAlias = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Other" }, { id: "Thing" }],
    },
    npm: surface("npm", ["Other", "Thing"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: { Thing: "Other" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (!handAlias.some((p) => p.kind === "collision")) {
    console.error(
      `  self-test FAILED: a hand-written alias onto a real second type was excused, got ${JSON.stringify(handAlias.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A pairing the gate makes FOR ITSELF, by name, can be wrong, and nothing
  // else in this file tests one. Every other control examines a pairing
  // somebody PROPOSED -- an alias, an entry, a rule -- and a false by-name
  // pair is a false negative: it does not add a row, it removes two, and the
  // residue it leaves reads exactly like a partial match.
  //
  // The real instance, reduced: npm declares a `TextBaseline` that is the
  // paragraph placeholder baseline and a `CanvasTextBaseline` that is the
  // canvas one. Rust's `TextBaseline` is the canvas one. Two variants
  // coincide, both holders are marked accounted for, and the four that differ
  // report as ordinary residue.
  const suspect = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "TextBaseline::Alphabetic" },
        { id: "TextBaseline::Bottom" },
        { id: "TextBaseline::Hanging" },
        { id: "TextBaseline::Ideographic" },
        { id: "TextBaseline::Middle" },
        { id: "TextBaseline::Top" },
      ],
    },
    npm: surface("npm", [
      "CanvasTextBaseline.alphabetic",
      "CanvasTextBaseline.bottom",
      "CanvasTextBaseline.hanging",
      "CanvasTextBaseline.ideographic",
      "CanvasTextBaseline.middle",
      "CanvasTextBaseline.top",
      "TextBaseline.alphabetic",
      "TextBaseline.ideographic",
    ]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!suspect.some((p) => p.kind === "suspect-pair")) {
    console.error(
      `  self-test FAILED: a by-name pair with a better-matching holder beside it was not reported, got ${JSON.stringify(suspect.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // The other direction, and it is the one that decides whether the check is
  // usable: a holder that pairs by name CORRECTLY and simply carries unpaired
  // members must stay silent. Measured on the real surface, `Canvas`, `Image`
  // and `Path2D` all pair properly and overlap their counterparts by 0.22,
  // 0.13 and 0.31 -- `TextBaseline` scores 0.33, HIGHER than two of them. So
  // a floor on the by-name overlap cannot separate them, and the floor sits
  // on the alternative instead.
  const residue = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "Shape.height" },
        { id: "Shape.only_here" },
        { id: "Shape.width" },
      ],
    },
    npm: surface("npm", [
      "Other.height",
      "Other.width",
      "Shape.height",
      "Shape.width",
    ]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (residue.some((p) => p.kind === "suspect-pair")) {
    console.error(
      `  self-test FAILED: a correct by-name pair with residue was called suspect, got ${JSON.stringify(residue.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // An entry naming a bare holder covers that holder's MEMBERS. `rust =
  // ["Key"]` registered the id `Key` and left all 195 `Key::` variants
  // unregistered, so sixteen live entries covered eighteen holders on paper
  // and a few hundred rows in fact.
  //
  // This is an entailment, not a guess: a member only ever pairs under
  // `Holder.member`, so if the holder reaches no name whose holder-part
  // exists on the other surface, no member of it can pair either.
  const holderEntry = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Key" }, { id: "Key::F16" }, { id: "Key::Shift" }],
    },
    npm: surface("npm", ["Unrelated.thing"]),
    manifest: parseManifest(
      `
[[capability]]
name = "keyboard key identity"
rust = ["Key"]
npm  = []
why  = "Rust names every key as a variant; npm passes the DOM key string
        through and declares no type for it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (holderEntry.some((p) => p.id.startsWith("Key::"))) {
    console.error(
      `  self-test FAILED: a holder entry did not cover its members, got ${JSON.stringify(holderEntry.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And the coverage is VERIFIED rather than trusted. The day a member of
  // that holder does pair, the entailment no longer holds and the entry is
  // making a claim about the surface that is no longer true -- so it fails as
  // `stale` and someone revisits it. Silent absorption is the alternative,
  // and it is how a real gap would end up registered.
  const holderEntryStale = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Key" }, { id: "Key::F16" }, { id: "Key::Shift" }],
    },
    npm: surface("npm", ["Key.F16"]),
    manifest: parseManifest(
      `
[[capability]]
name = "keyboard key identity"
rust = ["Key"]
npm  = []
why  = "Rust names every key as a variant; npm passes the DOM key string
        through and declares no type for it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!holderEntryStale.some((p) => p.kind === "stale")) {
    console.error(
      `  self-test FAILED: a holder entry kept covering members after one paired, got ${JSON.stringify(holderEntryStale.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // An owner alias ADDS a holder name; it does not replace the one written.
  // `rules.json` opens by saying every rule is additive and never replaces
  // the name as written, and gives that as the property which makes a wrong
  // rule safe. Owner aliases were the one rule that did not obey it.
  //
  // The cost is concrete: an alias keyed off Rust `BlendMode` discarded its
  // existing pairings against npm `BlendMode`, so the alias had to be keyed
  // from the npm side purely to work around the defect.
  const aliasKeeps = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Blend::ColorBurn" }],
    },
    npm: surface("npm", ["Blend.color-burn", "Wider.color-burn"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: { Blend: "Wider" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (aliasKeeps.length > 0) {
    console.error(
      `  self-test FAILED: an owner alias dropped the holder name as written, got ${JSON.stringify(aliasKeeps.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // THE FLOOR IS PINNED FROM BELOW, not only from above. The case above fires
  // on an alternative scoring 0.50 and so refuses a floor raised past that;
  // this one is the other side, and without it the floor could be lowered to
  // any value at all and no case would notice.
  //
  // `Widget` overlaps its by-name counterpart by 0.125 and the unrelated
  // `Neighbour` by 0.333 -- so it passes the "a different holder does better"
  // half and is silenced by the floor alone. Drop the floor to 0.30 and this
  // starts reporting a pair nobody should look at.
  const belowFloor = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "Widget::alpha" },
        { id: "Widget::beta" },
        { id: "Widget::delta" },
        { id: "Widget::epsilon" },
        { id: "Widget::gamma" },
      ],
    },
    npm: surface("npm", [
      "Neighbour.alpha",
      "Neighbour.beta",
      "Neighbour.gamma",
      "Neighbour.iota",
      "Neighbour.kappa",
      "Neighbour.lambda",
      "Neighbour.theta",
      "Widget.alpha",
      "Widget.eta",
      "Widget.mu",
      "Widget.zeta",
    ]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (belowFloor.some((p) => p.kind === "suspect-pair")) {
    console.error(
      `  self-test FAILED: a better-but-weak alternative was reported as suspect, got ${JSON.stringify(belowFloor.filter((p) => p.kind === "suspect-pair").map((p) => p.detail ?? p.id))}`,
    );
    bad += 1;
  }

  // TWO ALIASES THAT CROSS. npm declares a `TextBaseline` that is the
  // paragraph placeholder baseline and a `CanvasTextBaseline` that is the
  // canvas one; Rust names them the other way round. So the correct fix for
  // the false pair is two aliases whose targets swap, and both holders exist
  // under both names.
  //
  // Additivity and this case pull against each other. Keeping the written
  // name is what lets an alias add a spelling without dropping the pairings
  // it already had -- but here Rust `TextBaseline` keeping its own name
  // collides with Rust `PlaceholderBaseline`, which claims that same name
  // through its alias and is the holder npm means by it.
  const crossed = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "PlaceholderBaseline::Alphabetic" },
        { id: "PlaceholderBaseline::Ideographic" },
        { id: "TextBaseline::Alphabetic" },
        { id: "TextBaseline::Bottom" },
      ],
    },
    npm: surface("npm", [
      "CanvasTextBaseline.Alphabetic",
      "CanvasTextBaseline.Bottom",
      "TextBaseline.Alphabetic",
      "TextBaseline.Ideographic",
    ]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: {
        PlaceholderBaseline: "TextBaseline",
        TextBaseline: "CanvasTextBaseline",
      },
      member_aliases: {},
    },
  });
  cases += 1;
  if (crossed.length > 0) {
    console.error(
      `  self-test FAILED: two aliases whose targets cross do not pair, got ${JSON.stringify(crossed.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A HOLDER ALIASED ONTO A NAME ANOTHER HOLDER ALREADY OWNS NATIVELY is not
  // a collision at the bare type id. Rust has `Path2D` and `PathBuilder`
  // where npm has one `Path2D`: the members pair through the alias, and both
  // bare ids claim `Path2D` because both Rust types really do answer to it.
  // There is no wrong one to pair -- npm's `Path2D` is what each of them is
  // part of -- which is the same reason a declared rename and its target are
  // excused above.
  //
  // Narrow: one claimant must BE the name and the other must reach it by an
  // owner alias. Two holders both aliased onto a third still collide, which
  // is the `WindowSpec` shape the rules file refuses.
  const nativeAndAliased = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "Path2D" },
        { id: "PathBuilder" },
        { id: "PathBuilder.arc" },
      ],
    },
    npm: surface("npm", ["Path2D", "Path2D.arc"]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: { PathBuilder: "Path2D" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (nativeAndAliased.some((p) => p.kind === "collision")) {
    console.error(
      `  self-test FAILED: an alias onto a natively-owned name collided, got ${JSON.stringify(nativeAndAliased.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // THE NEAR-MISS LINE MUST SAY WHOSE NAME IT FOUND. It compares on the
  // member half, for a good reason -- a shared 25-character holder prefix
  // drowns the part a reader is judging -- but it then printed the member
  // half alone, so a match on an unrelated holder read as a perfect one.
  //
  // The real instance: `ImageData::premultiplied` was told its "closest npm
  // name is 'premultiplied', 0 characters away", pointing at
  // `AlphaInterpolation.premultiplied`, which is gradient alpha
  // interpolation. A reader trusting it closes a real capability gap as a
  // spelling difference, and the reporter is most confident exactly where it
  // is most wrong.
  const nearMiss = check({
    rust: surface("rust", ["ImageData::premultiplied"]),
    npm: surface("npm", ["AlphaInterpolation.premultiplied"]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  const said = nearMiss.map((p) => p.detail ?? "").join(" ");
  if (!said.includes("AlphaInterpolation")) {
    console.error(
      `  self-test FAILED: the near-miss line did not name the holder it matched, got ${JSON.stringify(said)}`,
    );
    bad += 1;
  }

  // KEBAB-CASING SPLITS AT AN ACRONYM BOUNDARY TOO. `([a-z0-9])([A-Z])`
  // needs a lowercase before the capital, so `EResize` produced `eresize`
  // where npm writes `e-resize`, and the four compass cursors reported as
  // absences. An earlier lane found the same four and put them down to its
  // own kebab function rather than to this one.
  //
  // The second pattern must not disturb the first: `ColorBurn` still gives
  // `color-burn` and `SRGB` still gives `srgb`, since neither has a capital
  // followed by a capital-then-lowercase.
  for (const [id, wanted] of [
    ["Cursor::EResize", "CursorStyle.e-resize"],
    ["Cursor::NwseResize", "CursorStyle.nwse-resize"],
    ["BlendMode::ColorBurn", "BlendMode.color-burn"],
  ]) {
    cases += 1;
    const claims = normalise(
      id,
      {
        ...RULES,
        owner_aliases: { Cursor: "CursorStyle" },
        member_aliases: {},
      },
      {},
      undefined,
      "rust",
    );
    if (!claims.has(wanted)) {
      console.error(
        `  self-test FAILED: '${id}' does not claim '${wanted}', got [${[...claims]}]`,
      );
      bad += 1;
    }
  }

  // A HOLDER WHOSE ALIAS IS ALREADY THE RECOMMENDATION IS NOT SUSPECT. The
  // check ends by saying "confirm it, or alias to X", and if the alias to X
  // is already written then somebody has looked and left the by-name pair
  // standing on purpose.
  //
  // `TextBaseline` and `BlendMode` came out differently for a reason that is
  // not about this check. Rust's `TextBaseline` YIELDS its written name to
  // `PlaceholderBaseline`, so the by-name pair stops existing and the row
  // goes with it. `BlendMode` keeps its written name deliberately -- that is
  // what additivity is for, and its 29 pairings against npm `BlendMode`
  // depend on it -- so the by-name pair survives the alias, and without this
  // the row survives forever on a holder that is correctly configured.
  const aliasedAlready = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "Blend::alpha" },
        { id: "Blend::beta" },
        { id: "Blend::delta" },
        { id: "Blend::gamma" },
      ],
    },
    npm: surface("npm", [
      "Blend.alpha",
      "Blend.eta",
      "Blend.mu",
      "Blend.zeta",
      "Wider.alpha",
      "Wider.beta",
      "Wider.delta",
      "Wider.gamma",
    ]),
    manifest: [],
    rules: {
      ...RULES,
      owner_aliases: { Blend: "Wider" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (aliasedAlready.some((p) => p.kind === "suspect-pair")) {
    console.error(
      `  self-test FAILED: a holder already aliased to the recommendation was called suspect, got ${JSON.stringify(aliasedAlready.filter((p) => p.kind === "suspect-pair").map((p) => p.id))}`,
    );
    bad += 1;
  }

  // And it comes BACK when the alias is not there, or the skip is a way to
  // silence the check by writing any alias at all.
  const notAliased = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [
        { id: "Blend::alpha" },
        { id: "Blend::beta" },
        { id: "Blend::delta" },
        { id: "Blend::gamma" },
      ],
    },
    npm: surface("npm", [
      "Blend.alpha",
      "Blend.eta",
      "Blend.mu",
      "Blend.zeta",
      "Wider.alpha",
      "Wider.beta",
      "Wider.delta",
      "Wider.gamma",
    ]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!notAliased.some((p) => p.kind === "suspect-pair")) {
    console.error(
      `  self-test FAILED: the same holder without the alias was not reported, got ${JSON.stringify(notAliased.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // AN ENTRY WHOSE IDS ALL PAIR ANYWAY DOES NOTHING, and nothing said so.
  // The manifest could only grow: every rule added makes some entry
  // unnecessary, and a redundant entry reads exactly like a load-bearing one
  // forever. The live instance was four noise-shader ids that started pairing
  // when owner aliases went additive and revived `make_prefix_holders`.
  const redundant = check({
    rust: surface("rust", ["Noise::turbulence"]),
    npm: surface("npm", ["Noise.turbulence"]),
    manifest: parseManifest(
      `
[[capability]]
name = "procedural noise"
rust = ["Noise::turbulence"]
npm  = ["Noise.turbulence"]
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!redundant.some((p) => p.kind === "redundant")) {
    console.error(
      `  self-test FAILED: an entry whose ids all pair was not reported, got ${JSON.stringify(redundant.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And an entry doing real work stays silent, or the class fires on most of
  // the manifest and says nothing.
  const loadBearing = check({
    rust: surface("rust", ["Noise::turbulence"]),
    npm: surface("npm", ["Elsewhere.somethingElse"]),
    manifest: parseManifest(
      `
[[capability]]
name = "procedural noise"
rust = ["Noise::turbulence"]
npm  = []
why  = "The crate generates noise directly; npm has no counterpart for it."

[[capability]]
name = "something else entirely"
rust = []
npm  = ["Elsewhere.somethingElse"]
why  = "Declared on the JavaScript side only, and the crate does not need it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (loadBearing.some((p) => p.kind === "redundant")) {
    console.error(
      `  self-test FAILED: an entry doing real work was called redundant, got ${JSON.stringify(loadBearing.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // AN ALIAS KEYED ON A HOLDER THE RUST SIDE DOES NOT HAVE IS INERT, and
  // must be inert in every direction rather than merely unable to do the
  // thing it says. Owner aliases apply Rust-side only, so such a line cannot
  // pair anything -- but it was still read when deciding whether some OTHER
  // holder should give up its written name, and that cost 35 pairings on the
  // real surface for one line that could not act.
  const wrongSurface = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: {},
      items: [{ id: "Blend::alpha" }],
    },
    npm: surface("npm", ["Blend.alpha", "Other.alpha", "Wider.alpha"]),
    manifest: [],
    rules: {
      ...RULES,
      // `Blend -> Wider` is the real, Rust-keyed alias. `Other -> Blend` is
      // the inert one: `Other` is an npm holder, so that line can never
      // apply. It is what makes `claimedByAnother('Blend')` true, and so what
      // makes Rust's `Blend` give up a name it should keep.
      owner_aliases: { Blend: "Wider", Other: "Blend" },
      member_aliases: {},
    },
  });
  cases += 1;
  if (
    wrongSurface.some((p) => p.id === "Blend.alpha" || p.id === "Blend::alpha")
  ) {
    console.error(
      `  self-test FAILED: an npm-keyed alias made a Rust holder yield its name, got ${JSON.stringify(wrongSurface.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And it is REPORTED, the way an alias whose target does not exist already
  // is. `rules.json` is a data file people edit, it carries 27 aliases, and
  // nothing in its shape says which surface a key belongs to -- the direction
  // is convention, not syntax. The next person to write one the wrong way
  // round would otherwise get no error and a silent loss of pairings
  // somewhere else entirely.
  cases += 1;
  if (!wrongSurface.some((p) => p.kind === "stale" && p.id.includes("Other"))) {
    console.error(
      `  self-test FAILED: an alias keyed on a non-Rust holder was not reported, got ${JSON.stringify(wrongSurface.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // AN ID NAMED BY TWO ENTRIES is the manifest's version of two holders
  // claiming one name, and nothing caught it. Adding a second live entry
  // naming an id an existing entry already registers changed no count and
  // produced no complaint of any kind.
  //
  // It matters at this size: the manifest is past the point where anyone
  // reads it end to end, and three lanes have been writing into it in
  // parallel. Two entries claiming one id means neither says where the
  // capability belongs, and the reader who follows the first `why` never
  // learns a second one exists.
  const doubled = check({
    rust: surface("rust", ["Mem::trim"]),
    npm: surface("npm", ["Unrelated.thing"]),
    manifest: parseManifest(
      `
[[capability]]
name = "releasing cached memory"
rust = ["Mem::trim"]
npm  = []
why  = "A crate-side call with no JavaScript counterpart to pair against."

[[capability]]
name = "something else that also claims it"
rust = ["Mem::trim"]
npm  = []
why  = "A second entry naming the same id, which is the defect under test."

[[capability]]
name = "the npm side"
rust = []
npm  = ["Unrelated.thing"]
why  = "Declared on the JavaScript side only, and the crate does not need it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (!doubled.some((p) => p.kind === "doubled")) {
    console.error(
      `  self-test FAILED: an id named by two entries was not reported, got ${JSON.stringify(doubled.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And the ordinary shapes stay silent: one entry naming several ids, and
  // two entries naming different members of one holder. A check that fires on
  // those fires on most of the manifest.
  const ordinary = check({
    rust: surface("rust", ["Holder::one", "Holder::three", "Holder::two"]),
    npm: surface("npm", ["Elsewhere.thing"]),
    manifest: parseManifest(
      `
[[capability]]
name = "two members of one holder"
rust = ["Holder::one", "Holder::two"]
npm  = []
why  = "Two members registered together, which is what an entry is for."

[[capability]]
name = "a different member of the same holder"
rust = ["Holder::three"]
npm  = []
why  = "The same holder, a different member, and no id in common with above."

[[capability]]
name = "the npm side"
rust = []
npm  = ["Elsewhere.thing"]
why  = "Declared on the JavaScript side only, and the crate does not need it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (ordinary.some((p) => p.kind === "doubled")) {
    console.error(
      `  self-test FAILED: an ordinary entry shape was called doubled, got ${JSON.stringify(ordinary.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // AN ID AN ENTRY NAMES THAT ALREADY PAIRS falls between the two checks
  // above. `redundant` fires only when EVERY id in an entry pairs unaided, so
  // an entry whose other ids are load-bearing is correctly not redundant;
  // `doubled` is entry against entry. An id covered by a rule AND an entry
  // trips neither, and the entry carries it for ever.
  //
  // The live instance: `PixelExportOptions.depth` is aliased to `colorType`
  // in `rules.json` and pairs, and is also named by the `raw pixel buffer
  // layout` entry, whose other ids do not pair.
  const mixed = check({
    rust: surface("rust", ["Opts::depth", "Opts::only_here"]),
    npm: surface("npm", ["Opts.depth"]),
    manifest: parseManifest(
      `
[[capability]]
name = "pixel buffer layout"
rust = ["Opts::depth", "Opts::only_here"]
npm  = []
why  = "One of these pairs on its own and the other has no counterpart."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (
    !mixed.some((p) => p.kind === "already-paired" && p.id === "Opts::depth")
  ) {
    console.error(
      `  self-test FAILED: an entry id that already pairs was not reported, got ${JSON.stringify(mixed.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }
  cases += 1;
  if (
    mixed.some((p) => p.kind === "already-paired" && p.id === "Opts::only_here")
  ) {
    console.error(
      `  self-test FAILED: an entry id that pairs with nothing was called already-paired`,
    );
    bad += 1;
  }

  // A wholly redundant entry stays `redundant` rather than becoming a run of
  // per-id rows: the useful instruction there is to delete the entry, not to
  // trim each of its ids.
  const whole = check({
    rust: surface("rust", ["Noise::turbulence"]),
    npm: surface("npm", ["Noise.turbulence"]),
    manifest: parseManifest(
      `
[[capability]]
name = "procedural noise"
rust = ["Noise::turbulence"]
npm  = ["Noise.turbulence"]
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (whole.some((p) => p.kind === "already-paired")) {
    console.error(
      `  self-test FAILED: a wholly redundant entry was reported per id as well, got ${JSON.stringify(whole.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // A BARE HOLDER WHOSE MEMBERS ALL PAIR IS NOT A COVERAGE CLAIM. Naming a
  // holder in a one-sided entry covers its members, and the day one pairs the
  // claim fails as `stale` -- but if they ALL pair there is nothing to cover
  // and the id is simply an id. Reporting that sends a reader to revisit an
  // entry that is doing exactly what it says.
  const coveredNothing = check({
    rust: surface("rust", ["Opts::density"]),
    npm: surface("npm", ["Opts", "Opts.density"]),
    manifest: parseManifest(
      `
[[capability]]
name = "an options type npm declares and Rust does not"
rust = []
npm  = ["Opts"]
why  = "npm names a type for the options where the crate takes them as
        arguments, so the type itself has no Rust counterpart."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (coveredNothing.some((p) => p.kind === "stale")) {
    console.error(
      `  self-test FAILED: a holder whose members all pair was reported stale, got ${JSON.stringify(coveredNothing.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // AN ENTRY THAT NAMES THE WRONG ONE OF TWO IDS SHARING A NAME. The Rust
  // extractor writes a field as `Holder.member` and a method as
  // `Holder::member`, so a holder can carry both -- eight do. An entry naming
  // one of them reads correctly, registers a real id, and leaves the other
  // unregistered while looking like it covered the capability.
  //
  // Nothing else reaches it. `stale` cannot fire because the id exists,
  // `already-paired` is about an id that pairs, and `doubled` compares
  // entries. It surfaced only because one holder happened to appear twice in
  // one lane's slice.
  const wrongTwin = check({
    rust: surface("rust", ["Face.height", "Face::height", "Face::other"]),
    npm: surface("npm", ["Elsewhere.thing"]),
    manifest: parseManifest(
      `
[[capability]]
name = "a metric the crate carries"
rust = ["Face.height", "Face::other"]
npm  = []
why  = "The crate exposes this and the declaration does not name it at all."

[[capability]]
name = "the npm side"
rust = []
npm  = ["Elsewhere.thing"]
why  = "Declared on the JavaScript side only, and the crate does not need it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (
    !wrongTwin.some((p) => p.kind === "wrong-of-two" && p.id === "Face::height")
  ) {
    console.error(
      `  self-test FAILED: an entry naming one of two same-named ids was not reported, got ${JSON.stringify(wrongTwin.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // And it stays silent where a holder has only one spelling, which is nearly
  // all of them -- a check firing on those fires on the whole manifest.
  const singleSpelling = check({
    rust: surface("rust", ["Face.height", "Face::other"]),
    npm: surface("npm", ["Elsewhere.thing"]),
    manifest: parseManifest(
      `
[[capability]]
name = "a metric the crate carries"
rust = ["Face.height", "Face::other"]
npm  = []
why  = "The crate exposes this and the declaration does not name it at all."

[[capability]]
name = "the npm side"
rust = []
npm  = ["Elsewhere.thing"]
why  = "Declared on the JavaScript side only, and the crate does not need it."
`,
      "self-test",
    ),
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (singleSpelling.some((p) => p.kind === "wrong-of-two")) {
    console.error(
      `  self-test FAILED: a holder with one spelling was reported, got ${JSON.stringify(singleSpelling.map((p) => `${p.kind}:${p.id}`))}`,
    );
    bad += 1;
  }

  // The setter rule FIRES, on a case measured in the real surface rather than
  // invented: `Pattern::set_transform` has its counterpart already as
  // `CanvasPattern.setTransform`. A naming rule that silently matches nothing
  // looks exactly like one that works, and this is the case that separates
  // them. It exercises the declared rename at the same time.
  const setters = check({
    rust: {
      surface: "rust",
      generated_from: "self-test fixture",
      renames: { Pattern: "CanvasPattern" },
      items: [{ id: "Pattern::set_transform" }],
    },
    npm: surface("npm", ["CanvasPattern.setTransform"]),
    manifest: [],
    rules: { ...RULES, owner_aliases: {}, member_aliases: {} },
  });
  cases += 1;
  if (setters.length > 0) {
    console.error(
      `  self-test FAILED: the setter rule did not pair Pattern::set_transform, got ${JSON.stringify(setters.map((p) => p.kind))}`,
    );
    bad += 1;
  }

  // A reader and its setter both claim the npm name, and that is not a
  // collision. 61 of the 81 real setters have a reader on the same holder, so
  // without the excuse the rule above would redden a correct crate.
  const pair = check({
    rust: surface("rust", [
      "Context2D::fill_style",
      "Context2D::set_fill_style",
    ]),
    npm: surface("npm", ["CanvasRenderingContext2D.fillStyle"]),
    manifest: [],
    rules: RULES,
  });
  cases += 1;
  if (pair.some((p) => p.kind === "collision")) {
    console.error(
      "  self-test FAILED: a reader and its setter were reported as colliding",
    );
    bad += 1;
  }

  // The getter condition excludes the one holder that declares both spellings.
  // `CanvasTransform` has `getTransform` AND `transform`; letting the first
  // claim the second would give two npm ids one name.
  const both = normalise(
    "CanvasTransform.getTransform",
    RULES,
    {},
    new Set(["getTransform", "transform"]),
  );
  cases += 1;
  if (both.has("CanvasTransform.transform")) {
    console.error(
      "  self-test FAILED: getTransform claimed `transform` on a holder that declares both",
    );
    bad += 1;
  }
  const alone = normalise(
    "Paragraph.getHeight",
    RULES,
    {},
    new Set(["getHeight"]),
  );
  cases += 1;
  if (!alone.has("Paragraph.height")) {
    console.error(
      "  self-test FAILED: getHeight did not claim `height` where the holder declares only the getter",
    );
    bad += 1;
  }

  // A declared rename pairs holders with no hand-written alias, AND does not
  // silence the members underneath it. Both halves, because the second is the
  // failure the whole design exists to prevent: `Shader as CanvasGradient` is
  // declared in the crate and the two share no member at all, so an alias
  // that suppressed the member report would turn six real one-sided members
  // into agreement.
  const renamed = {
    surface: "rust",
    generated_from: "self-test fixture",
    renames: { Affine: "DOMMatrix", Shader: "CanvasGradient" },
    items: [{ id: "Affine::multiply" }, { id: "Shader::linear_gradient" }].sort(
      (a, b) => a.id.localeCompare(b.id),
    ),
  };
  const against = surface("npm", [
    "CanvasGradient.addColorStop",
    "DOMMatrix.multiply",
  ]);
  const derived = check({
    rust: renamed,
    npm: against,
    manifest: [],
    rules: { ...RULES, owner_aliases: {} },
  });
  cases += 1;
  if (derived.some((p) => p.id === "Affine::multiply")) {
    console.error(
      "  self-test FAILED: a declared rename did not pair Affine::multiply with DOMMatrix.multiply",
    );
    bad += 1;
  }
  for (const id of ["Shader::linear_gradient", "CanvasGradient.addColorStop"]) {
    cases += 1;
    if (!derived.some((p) => p.id === id && p.kind === "unregistered")) {
      console.error(
        `  self-test FAILED: the rename silenced '${id}', which pairs with nothing`,
      );
      bad += 1;
    }
  }

  // The collision verdict must not depend on how the holders are spelled.
  //
  // Both trees below are the same graph: one child extends two unrelated
  // declarers, and all three declare `foo`. Only the names differ, which
  // changes the sort order and so which pairs end up adjacent. Keeping just
  // the last claimant per name compared three ids as two adjacent pairs and
  // never as three, so `Mid` sorting between `Alpha` and `Zeta` had both
  // comparisons excused by the inheritance clause while `Xchild` sorting
  // after `Alpha` and `Beta` was caught.
  //
  // Both namings, deliberately: one alone pins the accident rather than the
  // rule, and it is the naming that passes which would have shipped.
  for (const [label, child, parents] of [
    ["the child sorting in the middle", "Mid", ["Alpha", "Zeta"]],
    ["the child sorting last", "Xchild", ["Alpha", "Beta"]],
  ]) {
    const ids = [...parents.map((p) => `${p}::foo`), `${child}::foo`];
    const ordered = {
      surface: "rust",
      generated_from: "self-test fixture",
      heritage: { [child]: parents },
      items: [...ids].sort().map((id) => ({ id, kind: "method", owner: null })),
    };
    const found = check({
      rust: ordered,
      npm: surface("npm", ["Unrelated.bar"]),
      manifest: [],
      rules: RULES,
    }).filter((p) => p.kind === "collision");
    cases += 1;
    if (found.length === 0) {
      console.error(
        `  self-test FAILED: two unrelated declarers of one name went unreported with ${label}`,
      );
      bad += 1;
    }
  }

  // A collapse the rules explain is allowed: `_sized` and `_region` exist so
  // three drawImage arities read as one capability.
  const overloads = check({
    rust: surface("rust", [
      "Context2D::draw_image_sized",
      "Context2D::draw_image_region",
    ]),
    npm: surface("npm", ["CanvasRenderingContext2D.drawImage"]),
    manifest: [],
    rules: RULES,
  });
  cases += 1;
  if (overloads.some((p) => p.kind === "collision")) {
    console.error(
      "  self-test FAILED: a declared overload collapse was refused",
    );
    bad += 1;
  }
  // A collapse the rules do NOT explain must be refused.
  const collided = check({
    rust: surface("rust", ["Context2D::fill_rect", "Context2D::fillRect"]),
    npm: surface("npm", ["CanvasRenderingContext2D.fillRect"]),
    manifest: [],
    rules: RULES,
  });
  cases += 1;
  if (!collided.some((p) => p.kind === "collision")) {
    console.error(
      "  self-test FAILED: two ids normalising alike were not refused",
    );
    bad += 1;
  }

  if (bad > 0) process.exit(1);
  console.log(
    `self-test: ${cases} cases; each of unregistered, stale and ` +
      `unexplained is provoked, and a correct tree still passes`,
  );
}

const args = process.argv.slice(2);
if (args.includes("--self-test")) {
  selfTest();
} else {
  const path = (flag, fallback) => {
    const at = args.indexOf(flag);
    return at === -1 ? fallback : args[at + 1];
  };
  const rustPath = path("--rust", "target/parity-rust.json");
  const npmPath = path("--npm", "target/parity-npm.json");
  const manifestPath = path("--manifest", "parity.toml");

  const problems = check({
    rust: JSON.parse(readFileSync(rustPath, "utf8")),
    npm: JSON.parse(readFileSync(npmPath, "utf8")),
    manifest: parseManifest(readFileSync(manifestPath, "utf8"), manifestPath),
    rules: RULES,
  });

  if (problems.length > 0) {
    console.error("parity gate: the two surfaces are not accounted for");
    console.error(report(problems));
    console.error(
      "\nEvery id above is either missing from a surface or missing from " +
        `${manifestPath}. Add the item, or register the capability with a ` +
        "reason saying where it lives instead.",
    );
    process.exit(1);
  }
  console.log(
    `parity gate: every extracted id is paired or registered (${manifestPath})`,
  );
}
