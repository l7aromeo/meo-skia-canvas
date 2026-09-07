//
// The runnable half of the parity gate: a self-test, then the real check.
//
// The self-test exists because a gate that has never refused a real omission
// is a green that means nothing. Each case below is a tree the gate MUST
// reject, plus one it must accept -- without the accepting case the whole set
// is satisfied by a gate that fails everything.
//
import { readFileSync } from "node:fs";
import { check, report, normalise } from "./check-parity.mjs";
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
  ];

  let bad = 0;
  for (const [label, rustIds, npmIds, manifestText, wantKinds] of cases) {
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
      rules: RULES,
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
  const paired = normalise("Context2D::fill_rect", RULES);
  if (paired !== "CanvasRenderingContext2D.fillRect") {
    console.error(
      `  self-test FAILED: the naming rule does not pair, got '${paired}'`,
    );
    bad += 1;
  }
  // And a rule that collapsed two ids onto one name must be refused.
  const collided = check({
    rust: surface("rust", [
      "Context2D::draw_image_sized",
      "Context2D::draw_image_region",
    ]),
    npm: surface("npm", ["CanvasRenderingContext2D.drawImage"]),
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
    `self-test: ${cases.length + 2} cases; each of unregistered, stale and ` +
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
