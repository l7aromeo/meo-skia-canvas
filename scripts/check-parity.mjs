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
 * Every holder a member declared on `holder` is reachable through.
 *
 * The npm declarations follow WebIDL and split one class across mixin
 * interfaces, so `fillRect` is declared on `CanvasRect` and reached on
 * `CanvasRenderingContext2D`. The ids keep the declaring interface, because
 * closing over `extends` in the id would move 142 of them the day someone
 * adds one `extends` clause and a pure refactor would read as a surface
 * change. Matching is the other half of that trade: it closes over `extends`
 * here, so the id stays stable and the pairing still follows reachability.
 *
 * **Derived, not hand-maintained.** A table mapping `Context2D` to its dozen
 * mixins would need a line per mixin and would be the one place rot could
 * hide -- add a mixin, forget the line, and its members silently stop
 * pairing. `heritage` already states the relation, so a new mixin needs no
 * edit here. What remains for the hand-written table is only the holders
 * whose *names* differ between the surfaces.
 *
 * `CanvasPath` is extended by both `CanvasRenderingContext2D` and `Path2D`,
 * and that is not an ambiguity to resolve: its members really are reachable
 * from both, so both names are returned and the id pairs under either.
 */
export function reachableHolders(holder, heritage) {
  const names = new Set([holder]);
  const queue = [holder];
  while (queue.length > 0) {
    const at = queue.pop();
    for (const [child, parents] of Object.entries(heritage ?? {})) {
      if (parents.includes(at) && !names.has(child)) {
        names.add(child);
        queue.push(child);
      }
    }
  }
  return names;
}

/**
 * Every way one member name may be spelled on the other surface.
 *
 * Additive throughout: the name as written is always among them, so a rule
 * can only add a pairing, never remove the obvious one.
 */
function spellings(member, holder, rules) {
  const pascal = (s) => s[0].toUpperCase() + s.slice(1);
  const out = new Set([member, camel(member), pascal(camel(member))]);

  // `Make` + PascalCase, on the four holders that spell their constructors
  // that way. Scoped, so it cannot invent a pairing elsewhere.
  if ((rules.make_prefix_holders ?? []).includes(holder)) {
    for (const name of [...out]) out.add("Make" + pascal(name));
  }

  // Acronym casing runs LAST, over every spelling produced above. **Do not
  // reorder these two steps.**
  //
  // It ran before the `Make` prefix, which left `ColorFilter::hsla_matrix`
  // claiming `MakeHslaMatrix` where npm writes `MakeHSLAMatrix`. The rule was
  // present, correct, and applied to a string a later step then rewrote --
  // which is not a missing rule, and nothing about the symptom says which of
  // the two it was. That is why `uncovered` is worth its own class: it
  // reported thirteen of these as missing *rules* rather than as thirteen
  // missing capabilities, and the second reading sends a reader off to build
  // features that already exist.
  for (const name of [...out]) {
    let fixed = name;
    for (const acronym of rules.acronyms ?? []) {
      const titled = acronym[0] + acronym.slice(1).toLowerCase();
      fixed = fixed.split(titled).join(acronym);
    }
    out.add(fixed);
  }
  return out;
}

/**
 * The cross-surface names an id claims -- a set, because a member reachable
 * through several holders claims one name per holder. Two ids whose sets
 * intersect are an auto-pair; a bare type name claims its own spelling.
 */
export function normalise(id, rules, heritage) {
  const sep = id.includes("::") ? "::" : ".";
  const at = id.indexOf(sep);
  if (at === -1) return new Set([id]);
  const owner = id.slice(0, at);
  const raw = id.slice(at + sep.length);

  // Both the name as written and the name with a declared overload suffix
  // removed, rather than the stripped form alone.
  //
  // Stripping unconditionally is destructive and the real surface proves it:
  // `_path` is a declared suffix so that `fill_path` reaches `fill`, and it
  // also eats `close_path` into `close`, `begin_path` into `begin` and
  // `is_point_in_path` into `is_point_in`. Those three then pair with the
  // wrong member or with nothing. Offering both candidates keeps the
  // overload intent without losing the name a caller actually writes.
  const members = new Set([raw]);
  for (const suffix of rules.overload_suffixes) {
    if (raw.endsWith(suffix) && raw.length > suffix.length) {
      members.add(raw.slice(0, -suffix.length));
    }
  }
  // A trailing `Sync` is an npm-only affix: the crate is synchronous and
  // `_sync` appears in none of its ids, so the pair is a JavaScript event
  // loop concern rather than a different capability.
  for (const suffix of rules.strip_suffixes ?? []) {
    for (const m of [...members]) {
      if (m.endsWith(suffix) && m.length > suffix.length) {
        members.add(m.slice(0, -suffix.length));
      }
    }
  }

  const holders = new Set();
  for (const reachable of reachableHolders(owner, heritage)) {
    holders.add(rules.owner_aliases[reachable] ?? reachable);
  }

  const names = new Set();
  for (const h of holders) {
    for (const m of members) {
      const aliased = rules.member_aliases[m];
      for (const spelling of aliased ? [aliased] : spellings(m, h, rules)) {
        names.add(h + "." + spelling);
      }
    }
  }
  return names;
}

/** The one name to show a reader: the most-derived holder, or the id's own. */
export function displayName(id, rules, heritage) {
  const names = [...normalise(id, rules, heritage)];
  return names.length === 1
    ? names[0]
    : names.sort((a, b) => a.length - b.length)[0];
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

function describeNearMiss(
  id,
  rules,
  heritage,
  others,
  otherHeritage,
  otherSurface,
) {
  // Compared on the member half. A shared owner prefix like
  // `CanvasRenderingContext2D.` is 25 identical characters that drown the
  // part a reader is judging, and it made `set_letter_spacing` report
  // `fillRect` as its nearest name.
  const near = nearest(
    member(displayName(id, rules, heritage)),
    others.map((o) => member(displayName(o, rules, otherHeritage))),
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

/**
 * Whether two ids differ only by a suffix the rules declare as an overload.
 *
 * `draw_image_sized` and `draw_image_region` collapsing onto `drawImage` is
 * the point of those suffixes, not a fault: three arities are one capability.
 * A collapse the rules do not explain is the fault -- two unrelated items
 * landing on one name would pair one of them wrongly and hide a real gap.
 */
function sameBarOverloadSuffix(a, b, rules) {
  const strip = (id) => {
    for (const suffix of [
      ...rules.overload_suffixes,
      ...(rules.strip_suffixes ?? []),
    ]) {
      if (id.endsWith(suffix) && id.length > suffix.length) {
        return id.slice(0, -suffix.length);
      }
    }
    return id;
  };
  return strip(a) === strip(b) && a !== b;
}

/**
 * Whether two ids are one capability written as a field and as a method.
 *
 * **The separator is contract, not cosmetics.** `::` for associated items and
 * `.` for fields is what a Rust programmer writes, the extractor emits it,
 * and the interchange contract states it -- so normalising the two together
 * is not a tidy-up, it reintroduces the collision this function exists to
 * excuse. `Font` has both the field `Font.slant` and the builder
 * `Font::slant`. Those are
 * one capability with two spellings, the same relation npm collapses when it
 * folds a getter and a setter of one name into a single item, and reporting
 * them as a collision would leave the gate red on a correct crate.
 */
function fieldAndItsMethod(a, b) {
  const split = (id) => {
    const sep = id.includes("::") ? "::" : ".";
    const at = id.indexOf(sep);
    return at === -1 ? null : [id.slice(0, at), sep, id.slice(at + sep.length)];
  };
  const [x, y] = [split(a), split(b)];
  return (
    x !== null && y !== null && x[0] === y[0] && x[2] === y[2] && x[1] !== y[1]
  );
}

export function check({ rust, npm, manifest, rules: given }) {
  const problems = [];
  const note = (kind, id, detail) => problems.push({ kind, id, detail });

  // Holder pairings come from the crate's own `js_names` re-exports, which
  // `src/lib.rs` introduces with "Every item here is a re-export, not a new
  // type". The extractor emits that mapping; deriving it here means the
  // hand-written table is empty, and someone who adds a renamed re-export
  // cannot create a silently unmapped holder by forgetting a line, because
  // there is no line to forget. Same argument as the heritage closure.
  //
  // **A declared rename says the TYPES are one. It does not say the members
  // pair**, and this must not suppress the member-level report: `Shader as
  // CanvasGradient` is declared and the two share no member at all. After the
  // alias that reports six real one-sided members instead of two unmapped
  // holders, which is the truer statement, not a quieter one.
  //
  // A hand-written entry still wins, so a rename the crate does not declare
  // can be added without touching the extractor.
  const rules = {
    ...given,
    owner_aliases: { ...(rust.renames ?? {}), ...(given.owner_aliases ?? {}) },
  };

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

  const rustNames = new Map(
    rustIds.map((id) => [id, normalise(id, rules, rust.heritage)]),
  );
  const npmNames = new Map(
    npmIds.map((id) => [id, normalise(id, rules, npm.heritage)]),
  );

  // A rule mapping two ids on one surface onto one name would pair one of
  // them wrongly and suppress its report, so it is refused outright.
  //
  // Two clashes are NOT that, and both occur in the real surface.
  //
  // A shared mixin: `CanvasPath.lineTo` claims both
  // `CanvasRenderingContext2D.lineTo` and `Path2D.lineTo` because it really
  // is reachable through both.
  //
  // A redeclaration along the chain: `DOMPoint extends DOMPointReadOnly` and
  // restates `x` to widen it from readonly, so `DOMPoint.x` and
  // `DOMPointReadOnly.x` both claim `DOMPoint.x`. `CanvasRenderingContext2D`
  // restates `measureText` the same way. That is one capability written
  // twice in an inheritance chain, not two capabilities colliding, and
  // reporting it would leave the gate permanently red on a correct tree.
  //
  // So a clash is a collision only between holders NOT related by
  // inheritance -- which is exactly the case a careless rule produces.
  const related = (a, b, heritage) =>
    reachableHolders(a, heritage).has(b) ||
    reachableHolders(b, heritage).has(a);
  const holderOf = (id) => {
    const sep = id.includes("::") ? "::" : ".";
    const at = id.indexOf(sep);
    return at === -1 ? null : id.slice(0, at);
  };
  for (const [surface, ids, names, heritage] of [
    ["rust", rustIds, rustNames, rust.heritage],
    ["npm", npmIds, npmNames, npm.heritage],
  ]) {
    // EVERY claimant of a name is kept, and each new id is compared against
    // all of them rather than against the previous one.
    //
    // Keeping only the last made the verdict depend on how the holders are
    // spelled. Three ids claiming one name were compared as two adjacent
    // pairs and never as three, so where a redeclaring child sorted BETWEEN
    // two unrelated declarers both comparisons were excused by the
    // inheritance clause and the clash disappeared -- while the same graph
    // with the child sorting last was caught. Same structure, opposite
    // answers, decided by a name.
    const claimed = new Map();
    for (const id of ids) {
      for (const n of names.get(id)) {
        const previous = claimed.get(n) ?? [];
        for (const other of previous) {
          if (other === id) continue;
          const [x, y] = [holderOf(other), holderOf(id)];
          const redeclared =
            x !== null && y !== null && x !== y && related(x, y, heritage);
          if (
            !redeclared &&
            !sameBarOverloadSuffix(other, id, rules) &&
            !fieldAndItsMethod(other, id)
          ) {
            note(
              "collision",
              n,
              `${surface}: '${other}' and '${id}' both claim this name, and their holders are unrelated by inheritance`,
            );
          }
        }
        if (!previous.includes(id)) previous.push(id);
        claimed.set(n, previous);
      }
    }
  }
  if (problems.some((p) => p.kind === "collision")) return problems;

  const index = (names) => {
    const byName = new Map();
    for (const [id, set] of names) for (const n of set) byName.set(n, id);
    return byName;
  };
  const npmByName = index(npmNames);
  const rustByName = index(rustNames);
  const pairs = (names, other) => [...names].some((n) => other.has(n));
  const autoRust = new Set(
    rustIds.filter((id) => pairs(rustNames.get(id), npmByName)),
  );
  const autoNpm = new Set(
    npmIds.filter((id) => pairs(npmNames.get(id), rustByName)),
  );

  const namedRust = new Set(manifest.flatMap((e) => e.rust));
  const namedNpm = new Set(manifest.flatMap((e) => e.npm));

  // Exhaustiveness for holders, the same rule everything else obeys. A holder
  // on one surface that pairs with nothing on the other, and whose members
  // are not registered, would otherwise report every one of its members
  // separately -- fifty rows for one missing mapping.
  const holdersOf = (ids, names, other) => {
    const unmatched = new Map();
    for (const id of ids) {
      const owner = id.includes("::")
        ? id.slice(0, id.indexOf("::"))
        : id.includes(".")
          ? id.slice(0, id.indexOf("."))
          : null;
      if (owner === null) continue;
      if (pairs(names.get(id), other)) {
        unmatched.delete(owner);
        continue;
      }
      if (!unmatched.has(owner)) unmatched.set(owner, []);
      unmatched.get(owner).push(id);
    }
    return unmatched;
  };
  for (const [surface, ids, names, other] of [
    ["rust", rustIds, rustNames, npmByName],
    ["npm", npmIds, npmNames, rustByName],
  ]) {
    for (const [owner, members] of holdersOf(ids, names, other)) {
      const registered = surface === "rust" ? namedRust : namedNpm;
      const allRegistered = members.every((id) => registered.has(id));
      if (members.length >= 3 && !allRegistered) {
        note(
          "unmapped",
          owner,
          `${surface}: none of its ${members.length} members pair with the other surface. ` +
            `If this holder is named differently there, add it to owner_aliases in ` +
            `scripts/parity/rules.json -- one line fixes all ${members.length} rather than ` +
            `${members.length} capability entries.`,
        );
      }
    }
  }

  // A name that matches the other surface only when case is ignored is a
  // MISSING RULE, not a missing capability, and the two must not share a
  // heading. `to_data_url` camel-cases to `toDataUrl` where npm writes
  // `toDataURL`; reported as `unregistered` that reads as a feature to go and
  // build, and a reader sent to add something that already exists learns to
  // dismiss the gate.
  const fold = (names) => {
    const m = new Map();
    for (const [, set] of names) for (const n of set) m.set(n.toLowerCase(), n);
    return m;
  };
  const foldedNpm = fold(npmNames);
  const foldedRust = fold(rustNames);

  const classify = (id, names, folded, otherSurface, describe) => {
    for (const n of names) {
      const hit = folded.get(n.toLowerCase());
      if (hit !== undefined) {
        note(
          "uncovered",
          id,
          `no rule reaches '${hit}' on the ${otherSurface} side, though the two differ only in case. ` +
            `That is a missing rule rather than a missing capability -- add the casing to ` +
            `'acronyms' in scripts/parity/rules.json, not an entry to the manifest.`,
        );
        return;
      }
    }
    note("unregistered", id, describe());
  };

  for (const id of rustIds) {
    if (!autoRust.has(id) && !namedRust.has(id)) {
      classify(id, rustNames.get(id), foldedNpm, "npm", () =>
        describeNearMiss(id, rules, rust.heritage, npmIds, npm.heritage, "npm"),
      );
    }
  }
  for (const id of npmIds) {
    if (!autoNpm.has(id) && !namedNpm.has(id)) {
      classify(id, npmNames.get(id), foldedRust, "rust", () =>
        describeNearMiss(
          id,
          rules,
          npm.heritage,
          rustIds,
          rust.heritage,
          "rust",
        ),
      );
    }
  }

  // A rule naming something no extractor produced fails exactly as a manifest
  // entry does. This keeps the rules file from becoming the place a gap is
  // silenced: of twelve member-level exceptions proposed for it, zero were
  // naming problems and four were capability gaps. An alias for one of those
  // would have reported agreement on members that do not exist, in the one
  // layer no extractor guard can see.
  const npmHolders = new Set(
    npmIds.flatMap((id) =>
      id.includes(".") ? [id.slice(0, id.indexOf("."))] : [],
    ),
  );
  // Only the hand-written ones. A derived rename naming a holder npm does not
  // have is not a stale rule -- it is a Rust type the binding does not expose,
  // and its members report as unregistered, which is the right answer.
  for (const [from, to] of Object.entries(given.owner_aliases ?? {})) {
    if (!npmHolders.has(to)) {
      note(
        "stale",
        `owner_aliases.${from}`,
        `maps to holder '${to}', which no npm extractor produced. An alias to a ` +
          `holder that does not exist pairs nothing and says nothing.`,
      );
    }
  }
  const npmMembers = new Set(
    npmIds.flatMap((id) =>
      id.includes(".") ? [id.slice(id.indexOf(".") + 1)] : [],
    ),
  );
  for (const [from, to] of Object.entries(rules.member_aliases ?? {})) {
    if (!npmMembers.has(to)) {
      note(
        "stale",
        `member_aliases.${from}`,
        `maps to member '${to}', which no npm extractor produced. If the ` +
          `capability is genuinely absent it belongs in the manifest with a reason, ` +
          `not here -- an alias reports agreement on something that is not there.`,
      );
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
    const names = empty === "rust" ? npmNames : rustNames;
    for (const id of present) {
      for (const n of names.get(id) ?? []) {
        if (other.has(n)) {
          note(
            "unexplained",
            entry.name,
            `says its ${empty} side is empty, but '${id}' pairs to '${other.get(n)}', which exists`,
          );
          break;
        }
      }
    }
  }
  return problems;
}

export function report(problems) {
  const order = [
    "input",
    "collision",
    "uncovered",
    "unmapped",
    "unregistered",
    "stale",
    "unexplained",
  ];
  const headline = {
    input: "the extracted lists break the interchange contract",
    collision:
      "two ids normalise to one name, so a rule would pair one of them wrongly",
    uncovered:
      "the capability is on both sides; no rule reaches the other spelling",
    unmapped:
      "a holder whose members pair with nothing -- likely one missing alias",
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
