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
import { surfaceOf } from "./rustdoc-surface.mjs";

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
const { paths, external_crates: crates } = doc;

// The walk, and the reasoning for why it is a walk rather than a `paths`
// filter or an HTML grep, now live in `rustdoc-surface.mjs`: a second tool
// needed the same answer, and two tools deciding "what is public" separately
// is a pair that drifts.
const { item, reachable, nameOf } = surfaceOf(doc);

// Which crate a referenced id belongs to. Local to this check: the shared
// walk answers reachability, not provenance.
const crateOf = (id) => {
  const entry = paths[String(id)];
  return entry?.crate_id
    ? (crates[String(entry.crate_id)]?.name ?? null)
    : null;
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
