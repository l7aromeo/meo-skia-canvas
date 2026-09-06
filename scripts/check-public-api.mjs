//
// Fails when a public signature exposes a type from a dependency the crate is
// meant to hide.
//
// Two things make the obvious implementations wrong, both found by injecting a
// deliberate `pub fn(skia_safe::Matrix) -> skia_safe::Color` and checking that
// the detector actually caught it:
//
//   - Grepping rustdoc's HTML finds nothing. When the dependency is not
//     documented alongside, rustdoc renders `skia_safe::Color` in a signature
//     as a bare `Color` with no link, so the crate name never appears. That
//     check reported "0 leaks across 179 pages" on a tree that leaked.
//
//   - Trusting rustdoc's `paths` map over-reports. It contains crate-internal
//     items too: `context` is `pub(crate)` and absent from the index entirely,
//     yet `context::page::ExportOptions` and its fields are listed there, so a
//     `paths`-based filter flags seven items no consumer can name.
//
// So: read the JSON, and decide what is public by walking the module tree down
// from the crate root. An item counts only if it is reachable that way.
//
// Usage:  node scripts/check-public-api.mjs <path-to-rustdoc-json>
//

import { readFileSync } from "fs";

const FORBIDDEN = ["skia_safe", "neon"];

// Empty, and worth keeping that way. `gui` was exempt while four of its
// methods passed Skia's Matrix, SurfaceProps and Color through; they now take
// and return the crate's own `Affine` and a CSS string, or are `pub(crate)`.
//
// The list stays because an exemption should be a visible decision rather than
// a silent skip -- but adding to it means the README's claim, and the crate
// docs', are false again for whatever gets added.
const EXEMPT_MODULES = [];

const jsonPath = process.argv[2];
if (!jsonPath) {
  console.error("usage: node scripts/check-public-api.mjs <rustdoc-json>");
  process.exit(2);
}

const doc = JSON.parse(readFileSync(jsonPath, "utf8"));
const { index, paths, external_crates: crates } = doc;

const item = (id) => index[String(id)];

// Children an item can contribute to the public surface. Fields and variants
// count because a `pub` field is as much a signature as a return type, and
// `impls` count because that is where methods live -- without them the walk
// reaches no method at all, which was a hole wide enough to pass the four
// `gui` methods that leaked while the tree reported clean.
//
// A variant is a node in its own right, and its payload hangs below it: a
// struct variant keeps field ids at `kind.struct.fields`, a tuple variant a
// (nullable) id list at `kind.tuple`. Reaching the variant and stopping there
// checks nothing, because the ids that name types are one level further down
// -- `UiEvent::Mouse` alone yields six fields the walk never saw. Tuple
// structs hide their fields the same way, under `kind.tuple` rather than
// `kind.plain.fields`; `FontAxisTag` and `ColorMatrix` are the two currently
// public, `ThreadBound` being the third and reachable from nowhere because
// `gpu` is `pub(crate)`.
//
// Both were invisible until a check of the checker: a `skia_safe::Matrix`
// planted in `UiEvent::Mouse.point` still reported the tree clean.
const fieldsOfVariantKind = (kind) => [
  ...(kind?.struct?.fields ?? []),
  // Tuple entries are null where the field is not public, so the nulls are
  // dropped rather than looked up.
  ...(kind?.tuple ?? []).filter((id) => id !== null),
];

const childrenOf = (node) => {
  const inner = node?.inner;
  if (!inner || typeof inner !== "object") return [];
  const [kind] = Object.keys(inner);
  const body = inner[kind];
  switch (kind) {
    case "module":
      return body.items ?? [];
    case "struct":
      return [
        ...(body.kind?.plain?.fields ?? []),
        ...(body.kind?.tuple ?? []).filter((id) => id !== null),
        ...(body.items ?? []),
        ...(body.impls ?? []),
      ];
    case "enum":
      return [
        ...(body.variants ?? []),
        ...(body.items ?? []),
        ...(body.impls ?? []),
      ];
    case "variant":
      return fieldsOfVariantKind(body.kind);
    case "union":
      return [...(body.fields ?? []), ...(body.impls ?? [])];
    case "trait":
      return body.items ?? [];
    case "impl":
      return body.items ?? [];
    default:
      return [];
  }
};

// Reachability from the crate root, which is what "public API" means. A
// `pub(crate)` module is not in the index, so nothing under it is ever walked.
const owner = new Map();
const reachable = new Set([doc.root]);
const queue = [doc.root];

while (queue.length) {
  const id = queue.shift();
  for (const child of childrenOf(item(id))) {
    if (reachable.has(child)) continue;
    reachable.add(child);
    owner.set(child, id);
    queue.push(child);
  }
}

const crateOf = (id) => {
  const entry = paths[String(id)];
  return entry?.crate_id
    ? (crates[String(entry.crate_id)]?.name ?? null)
    : null;
};

// Prefer rustdoc's own path; fall back to the walk for fields and variants,
// which have no `paths` entry of their own.
const nameOf = (id) => {
  const viaPaths = paths[String(id)]?.path;
  if (viaPaths) return viaPaths.join("::");
  const parent = owner.get(id);
  // An impl block has no name and no path of its own; a method under it reads
  // as `Type::method`, so the impl contributes nothing but its parent.
  const self = item(id)?.name;
  if (parent === undefined) return self ?? `<${id}>`;
  return self ? `${nameOf(parent)}::${self}` : nameOf(parent);
};

const referencedIds = (node, out = new Set()) => {
  if (Array.isArray(node)) node.forEach((n) => referencedIds(n, out));
  else if (node && typeof node === "object") {
    if (typeof node.resolved_path?.id === "number")
      out.add(node.resolved_path.id);
    Object.values(node).forEach((v) => referencedIds(v, out));
  }
  return out;
};

const exempt = (name) =>
  EXEMPT_MODULES.some((m) =>
    name.startsWith(`${doc.index[String(doc.root)]?.name ?? ""}::${m}::`),
  );

const leaks = new Set();
let checked = 0;

for (const id of reachable) {
  const node = item(id);
  if (!node?.inner || typeof node.inner !== "object") continue;

  const [kind] = Object.keys(node.inner);
  if (kind === "module") continue;

  const name = nameOf(id);
  if (exempt(name)) continue;
  checked++;

  for (const ref of referencedIds(node.inner)) {
    const crate = crateOf(ref);
    if (crate && FORBIDDEN.includes(crate))
      leaks.add(`${name}  ->  ${paths[String(ref)].path.join("::")}`);
  }
}

if (leaks.size) {
  console.error(
    `${leaks.size} public signature(s) expose a hidden dependency:\n`,
  );
  for (const leak of [...leaks].sort()) console.error("  " + leak);
  console.error(
    "\nWrap the type, or add to EXEMPT_MODULES if it is deliberate.",
  );
  process.exit(1);
}

console.log(
  `${checked} public items carry no ${FORBIDDEN.join(" or ")} type` +
    (EXEMPT_MODULES.length ? ` (exempt: ${EXEMPT_MODULES.join(", ")})` : ""),
);
