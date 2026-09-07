//
// The Rust half of the parity gate: every publicly reachable item, as the
// interchange contract shapes it.
//
// Usage:  node scripts/extract-rust-surface.mjs <rustdoc-json> [out.json]
//
// WHAT AN ITEM IS, and it is a choice rather than a reading of the data.
//
// An item is a name a consumer writes in order to use the crate. That rule
// decides every case below, and the cases are what make it worth stating:
//
//   included   structs, enums, traits, type aliases, constants, free
//              functions, public fields, enum variants, associated consts
//              and types, and methods on INHERENT impls
//
//   excluded   modules and impl blocks, which are containers and not names
//              anyone writes as a capability
//
//   excluded   methods on TRAIT impls -- `Clone::clone`, `Debug::fmt`,
//              `From::from`. 409 of the 850 reachable functions are these,
//              nearly all derived. The name belongs to the trait, not to
//              this crate, and a consumer learns it once for all of Rust.
//              Pairing them would demand 409 npm counterparts that cannot
//              exist and would bury the manifest in explanations.
//
// That last exclusion is the one to argue with, so the count is printed on
// every run rather than left implicit: an extractor that drops things
// quietly is how a parity gate reports agreement it never checked.
//
// RE-EXPORTS UNDER A SECOND NAME ARE THEIR OWN ITEMS. `js_names` renames
// seven types -- `Affine` as `DOMMatrix`, `Context2D` as
// `CanvasRenderingContext2D`, and five more -- and those are exactly the
// names the npm surface uses. Emitting only the canonical name would leave
// every one of them needing a `why`, when the pairing is direct; emitting
// only the alias would hide a name a Rust caller really writes. So both, and
// the manifest lists both on the rust side of one capability.
//
// A re-export that does NOT rename is not a second item. `prelude` re-exports
// the crate under the same names, and two ids for one name would be a
// duplicate the contract forbids.
//

import { readFileSync, writeFileSync } from "fs";
import { surfaceOf } from "./rustdoc-surface.mjs";

// Under `target/` on purpose: this is derived from the rustdoc JSON and is
// as disposable as it is. A `cargo clean` takes both, which is right -- the
// gate regenerates rather than reading whatever was left behind, and a stale
// surface silently paired against a current manifest is exactly the failure
// this whole exercise exists to prevent.
const [, , jsonPath, outPath = "target/parity-rust.json"] = process.argv;
if (!jsonPath) {
  console.error(
    "usage: node scripts/extract-rust-surface.mjs <rustdoc-json> [out.json]",
  );
  process.exit(2);
}

const doc = JSON.parse(readFileSync(jsonPath, "utf8"));
const { item, reachable, owner, paths } = surfaceOf(doc);

const kindOf = (node) => Object.keys(node?.inner ?? {})[0];

// The nearest ancestor rustdoc gives a path to. For a method that is the
// impl's type; for a field or variant, the struct or enum. `paths` is
// unreliable as a filter -- it lists crate-internal items -- but as a lookup
// for something the walk already reached it is exactly right.
const namedAncestor = (id) => {
  for (let at = owner.get(id); at !== undefined; at = owner.get(at)) {
    const path = paths[String(at)]?.path;
    if (path) return path[path.length - 1];
  }
  return null;
};

// True when this function hangs off an `impl Trait for Type` rather than an
// inherent `impl Type`.
const inTraitImpl = (id) => {
  const parent = item(owner.get(id));
  return kindOf(parent) === "impl" && parent.inner.impl.trait != null;
};

const items = new Map();
const add = (id, kind, ownerName) => {
  if (items.has(id)) return;
  items.set(id, { id, kind, owner: ownerName ?? null });
};

let traitImplMethods = 0;
let aliases = 0;
const renames = {};

for (const id of reachable) {
  const node = item(id);
  const kind = kindOf(node);
  if (!kind || kind === "module" || kind === "impl") continue;

  // A re-export contributes a name only when it renames.
  //
  // The MAPPING is emitted too, not just the second name. `js_names` states
  // which canonical type each alias is -- "Every item here is a re-export,
  // not a new type" -- and that is exactly the holder pairing the gate would
  // otherwise need written by hand, one line per rename, in the one place a
  // forgotten line makes a silently unmapped holder. Derived, there is no
  // line to forget, and a rename added later pairs on its own.
  //
  // It says the TYPES are one. It does not say the members pair, and the gate
  // must keep reporting those: `Shader as CanvasGradient` is declared here
  // and the two share no member at all.
  if (kind === "use") {
    const use = node.inner.use;
    const target = item(use.id);
    if (use.is_glob || !target?.name || target.name === use.name) continue;
    aliases++;
    renames[target.name] = use.name;
    add(use.name, kindOf(target), null);
    continue;
  }

  const name = node.name;
  if (!name) continue;

  if (kind === "function") {
    if (inTraitImpl(id)) {
      traitImplMethods++;
      continue;
    }
    const on = namedAncestor(id);
    // A free function has no owning type and is named on its own.
    add(on ? `${on}::${name}` : name, on ? "method" : "function", on);
    continue;
  }

  if (kind === "struct_field") {
    const on = namedAncestor(id);
    // A dot, not `::`, because that is what the contract's manifest example
    // writes for a field -- `GradientInterpolation.space` -- while its item
    // example writes `Context2D::fill_rect` for a method. Matching the
    // contract matters more than matching itself.
    add(on ? `${on}.${name}` : name, "field", on);
    continue;
  }

  if (kind === "variant") {
    const on = namedAncestor(id);
    add(on ? `${on}::${name}` : name, "variant", on);
    continue;
  }

  if (kind === "assoc_const" || kind === "assoc_type") {
    if (inTraitImpl(id)) continue;
    const on = namedAncestor(id);
    add(on ? `${on}::${name}` : name, kind, on);
    continue;
  }

  add(name, kind, null);
}

const sorted = [...items.values()].sort((a, b) => (a.id < b.id ? -1 : 1));

// The contract asks the extractor to assert its own output, so that an empty
// or duplicated list downstream cannot read as agreement.
if (sorted.length === 0) {
  console.error("the surface is empty, which cannot be right");
  process.exit(1);
}
for (let i = 1; i < sorted.length; i++) {
  if (sorted[i].id === sorted[i - 1].id) {
    console.error(`duplicate id: ${sorted[i].id}`);
    process.exit(1);
  }
  if (sorted[i].id < sorted[i - 1].id) {
    console.error(`unsorted at ${sorted[i].id}`);
    process.exit(1);
  }
}

writeFileSync(
  outPath,
  JSON.stringify(
    { surface: "rust", generated_from: jsonPath, items: sorted, renames },
    null,
    2,
  ) + "\n",
);

const byKind = {};
for (const { kind } of sorted) byKind[kind] = (byKind[kind] ?? 0) + 1;
console.log(
  `${sorted.length} public Rust items -> ${outPath}\n` +
    Object.entries(byKind)
      .sort()
      .map(([k, n]) => `  ${n} ${k}`)
      .join("\n") +
    `\n  (${traitImplMethods} trait-impl methods excluded, ` +
    `${aliases} renaming re-exports included)`,
);
