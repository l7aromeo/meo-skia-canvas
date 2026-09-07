//
// The runnable half of the parity gate: a self-test, then the real check.
//
// The self-test exists because a gate that has never refused a real omission
// is a green that means nothing. Each case below is a tree the gate MUST
// reject, plus one it must accept -- without the accepting case the whole set
// is satisfied by a gate that fails everything.
//
import { readFileSync } from "node:fs";
import { check, report, normalise, displayName } from "./check-parity.mjs";
import { parseManifest } from "./parity/toml.mjs";

const RULES = JSON.parse(
  readFileSync(new URL("./parity/rules.json", import.meta.url), "utf8"),
);

const surface = (name, ids) => ({
  surface: name,
  generated_from: "self-test fixture",
  items: [...ids].sort().map((id) => ({ id, kind: "method", owner: null })),
});

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
  const cases = [
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
  ] of cases) {
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
      rules: override ? { ...RULES, ...override } : RULES,
    });
    const got = [...new Set(problems.map((p) => p.kind))].sort();
    const want = [...wantKinds].sort();
    if (got.join(",") !== want.join(",")) {
      console.error(
        `  self-test FAILED: ${label} -- wanted [${want}], got [${got}]`,
      );
      bad += 1;
    }
  }

  // The naming rule has to actually pair something, or every case above is
  // satisfied by a gate that pairs nothing and registers everything by hand.
  const paired = displayName("Context2D::fill_rect", RULES, {});
  if (paired !== "CanvasRenderingContext2D.fillRect") {
    console.error(
      `  self-test FAILED: the naming rule does not pair, got '${paired}'`,
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
    if (!normalise(id, RULES, {}).has(wanted)) {
      console.error(
        `  self-test FAILED: '${id}' does not claim '${wanted}', got [${[...normalise(id, RULES, {})]}]`,
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
    if (!normalise(id, RULES, {}).has(wanted)) {
      console.error(`  self-test FAILED: '${id}' does not claim '${wanted}'`);
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
  if (!collided.some((p) => p.kind === "collision")) {
    console.error(
      "  self-test FAILED: two ids normalising alike were not refused",
    );
    bad += 1;
  }

  if (bad > 0) process.exit(1);
  console.log(
    `self-test: ${cases.length + 15} cases; each of unregistered, stale and ` +
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
