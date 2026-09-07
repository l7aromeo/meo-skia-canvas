//
// Presence parity between the Rust crate and the npm addon.
//
// Fails when a capability exists on one surface and is neither present on the
// other nor registered as a deliberate single-surface decision. It fires at
// the moment of adding rather than as an inventory somebody reads later:
// adding an item fails until it is registered, and removing one fails until
// its entry goes.
//
// THREE FAILURES, ALL FATAL, per the interchange contract:
//
//   unregistered  an extracted id in no manifest entry and matched by no rule
//   stale         an id named in an entry that no extractor produced
//   unexplained   an entry with an empty side and no `why`
//
// The third direction is what stops the register rotting. `stale` is the
// reverse of `unregistered`, so the manifest cannot drift from the code in
// either direction without the gate saying so.
//
// WHAT THIS CANNOT SEE. It proves the two surfaces agree with each other, not
// that either is complete. A capability forgotten on *both* sides is
// extracted by neither, named by no entry, and passes in silence. Nothing
// here can find that: it needs a source outside both surfaces, which is what
// the Canvas standard and the reference tables are for.
//

/** snake_case to camelCase, applied to the member half of an id only. */
const camel = (s) => s.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());

/**
 * The cross-surface name an id claims. Two ids normalising alike is what an
 * auto-pair means; a bare type name claims its own spelling.
 */
export function normalise(id, rules) {
  const sep = id.includes("::") ? "::" : ".";
  const at = id.indexOf(sep);
  if (at === -1) return id;
  const owner = id.slice(0, at);
  let member = id.slice(at + sep.length);
  for (const suffix of rules.overload_suffixes) {
    if (member.endsWith(suffix) && member.length > suffix.length) {
      member = member.slice(0, -suffix.length);
      break;
    }
  }
  member = rules.member_aliases[member] ?? camel(member);
  const ownerName = rules.owner_aliases[owner] ?? owner;
  return ownerName + "." + member;
}

const distance = (a, b) => {
  const d = Array.from({ length: a.length + 1 }, (_, i) => [
    i,
    ...Array(b.length).fill(0),
  ]);
  for (let j = 0; j <= b.length; j += 1) d[0][j] = j;
  for (let i = 1; i <= a.length; i += 1) {
    for (let j = 1; j <= b.length; j += 1) {
      d[i][j] = Math.min(
        d[i - 1][j] + 1,
        d[i][j - 1] + 1,
        d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
  }
  return d[a.length][b.length];
};

/** The closest name on the other surface, so a reader can tell a spelling slip from a real absence. */
function nearest(target, candidates) {
  let best = null;
  let bestAt = Infinity;
  for (const c of candidates) {
    const d = distance(target.toLowerCase(), c.toLowerCase());
    if (d < bestAt) {
      best = c;
      bestAt = d;
    }
  }
  // Deliberately tight. A suggestion 13 characters away is noise presented as
  // help, and "likely a real absence" is the more useful answer when nothing
  // is close.
  const tolerance = Math.min(4, Math.ceil(target.length / 3));
  return best === null || bestAt > tolerance
    ? null
    : { id: best, distance: bestAt };
}

/** The member half of a normalised name -- what actually distinguishes two ids. */
const member = (name) =>
  name.includes(".") ? name.slice(name.indexOf(".") + 1) : name;

function describeNearMiss(id, rules, others, otherSurface) {
  // Compared on the member half. A shared owner prefix like
  // `CanvasRenderingContext2D.` is 25 identical characters that drown the
  // part a reader is judging, and it made `set_letter_spacing` report
  // `fillRect` as its nearest name.
  const near = nearest(
    member(normalise(id, rules)),
    others.map((o) => member(normalise(o, rules))),
  );
  if (near === null) {
    return `nothing on the ${otherSurface} side resembles it, so this is likely a real absence`;
  }
  const plural = near.distance === 1 ? "" : "s";
  return (
    `closest ${otherSurface} name is '${near.id}', ${near.distance} character${plural} away; ` +
    `check whether that is a spelling difference before adding an entry`
  );
}

export function check({ rust, npm, manifest, rules }) {
  const problems = [];
  const note = (kind, id, detail) => problems.push({ kind, id, detail });

  // The extractors assert these themselves, so a violation means an extractor
  // is broken and every count below is untrustworthy -- an empty one most of
  // all, since it would otherwise read as agreement.
  for (const [surface, list] of [
    ["rust", rust],
    ["npm", npm],
  ]) {
    const ids = list.items.map((i) => i.id);
    if (ids.length === 0) {
      note("input", surface, "extractor produced no items at all");
    }
    const sorted = [...ids].sort();
    if (ids.join(" ") !== sorted.join(" ")) {
      note(
        "input",
        surface,
        "items are not sorted by id, which the contract requires",
      );
    }
    const dupes = ids.filter((id, i) => ids[i - 1] === id);
    if (dupes.length > 0) {
      note(
        "input",
        surface,
        `duplicate ids: ${[...new Set(dupes)].join(", ")}`,
      );
    }
  }
  if (problems.length > 0) return problems;

  const rustIds = rust.items.map((i) => i.id);
  const npmIds = npm.items.map((i) => i.id);

  // A rule mapping two ids on one surface onto one name would pair one of
  // them wrongly and suppress its report, so it is refused outright.
  for (const [surface, ids] of [
    ["rust", rustIds],
    ["npm", npmIds],
  ]) {
    const claimed = new Map();
    for (const id of ids) {
      const n = normalise(id, rules);
      if (claimed.has(n) && claimed.get(n) !== id) {
        note(
          "collision",
          n,
          `${surface}: '${claimed.get(n)}' and '${id}' both normalise to this`,
        );
      }
      claimed.set(n, id);
    }
  }
  if (problems.some((p) => p.kind === "collision")) return problems;

  const npmByName = new Map(npmIds.map((id) => [normalise(id, rules), id]));
  const rustByName = new Map(rustIds.map((id) => [normalise(id, rules), id]));
  const autoRust = new Set(
    rustIds.filter((id) => npmByName.has(normalise(id, rules))),
  );
  const autoNpm = new Set(
    npmIds.filter((id) => rustByName.has(normalise(id, rules))),
  );

  const namedRust = new Set(manifest.flatMap((e) => e.rust));
  const namedNpm = new Set(manifest.flatMap((e) => e.npm));

  for (const id of rustIds) {
    if (!autoRust.has(id) && !namedRust.has(id)) {
      note("unregistered", id, describeNearMiss(id, rules, npmIds, "npm"));
    }
  }
  for (const id of npmIds) {
    if (!autoNpm.has(id) && !namedNpm.has(id)) {
      note("unregistered", id, describeNearMiss(id, rules, rustIds, "rust"));
    }
  }

  const rustSet = new Set(rustIds);
  const npmSet = new Set(npmIds);
  for (const entry of manifest) {
    for (const id of entry.rust) {
      if (!rustSet.has(id)) {
        note(
          "stale",
          id,
          `named by capability '${entry.name}', produced by no Rust extractor`,
        );
      }
    }
    for (const id of entry.npm) {
      if (!npmSet.has(id)) {
        note(
          "stale",
          id,
          `named by capability '${entry.name}', produced by no npm extractor`,
        );
      }
    }

    const empty =
      entry.rust.length === 0 ? "rust" : entry.npm.length === 0 ? "npm" : null;
    if (empty === null) continue;

    if (!entry.why || entry.why.trim().length < 20) {
      note(
        "unexplained",
        entry.name,
        `its ${empty} side is empty and there is no 'why' saying where the capability lives instead`,
      );
      continue;
    }
    // A `why` asserting one surface has none, while the other side's id pairs
    // to something that does exist over there, is an explanation gone stale.
    const present = empty === "rust" ? entry.npm : entry.rust;
    const other = empty === "rust" ? rustByName : npmByName;
    for (const id of present) {
      const n = normalise(id, rules);
      if (other.has(n)) {
        note(
          "unexplained",
          entry.name,
          `says its ${empty} side is empty, but '${id}' pairs to '${other.get(n)}', which exists`,
        );
      }
    }
  }
  return problems;
}

export function report(problems) {
  const order = ["input", "collision", "unregistered", "stale", "unexplained"];
  const headline = {
    input: "the extracted lists break the interchange contract",
    collision:
      "two ids normalise to one name, so a rule would pair one of them wrongly",
    unregistered:
      "on one surface, in no capability entry, and matched by no rule",
    stale: "named by a capability entry but produced by no extractor",
    unexplained: "a capability with one side empty and no usable reason",
  };
  const lines = [];
  for (const kind of order) {
    const of = problems.filter((p) => p.kind === kind);
    if (of.length === 0) continue;
    lines.push("");
    lines.push(`${kind} (${of.length}) -- ${headline[kind]}`);
    for (const p of of) {
      lines.push(`  ${p.id}`);
      lines.push(`      ${p.detail}`);
    }
  }
  return lines.join("\n");
}
