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
function spellings(member, holder, rules, declared, surface) {
  const pascal = (s) => s[0].toUpperCase() + s.slice(1);
  const out = new Set([member, camel(member), pascal(camel(member))]);

  // A Rust `set_x` also claims `x`. npm spells the pair as one property and
  // its own extractor collapses a getter and a setter into a single item, so
  // `Context2D::set_fill_style` has to reach `fillStyle` or it reports as a
  // gap. 46 of the 81 setters pair once this exists; the 35 that do not are
  // real crate-only capabilities and stay visible, which is the argument for
  // the rule rather than a cost of it.
  if (member.startsWith("set_") && member.length > 4) {
    const bare = member.slice(4);
    out.add(bare);
    out.add(camel(bare));
  }

  // A Rust enum variant also claims the kebab-cased spelling npm uses for the
  // same value: `BlendMode::ColorBurn` claims `color-burn`.
  //
  // ADDITIVE rather than ordered. A proposed this as "fold case and compare
  // exactly, kebab only if that finds nothing", measured because kebab alone
  // claims 168 of 183 variants while exact-first claims 175 -- the seven
  // recovered are acronym-heavy, `PixelDepth::R8UNorm` kebabbing to something
  // that matches nothing while both surfaces already spell it identically.
  // Claiming both spellings gets the same 175 without an ordering, because
  // the identifier as written is claimed by every member here anyway. An
  // ordering would have to be evaluated against the other side's names;
  // claiming both does not, and two claims are what every other rule makes.
  //
  // RUST SIDE ONLY, and the direction is part of the rule rather than an
  // optimisation. Applied to npm as well it fires on that surface's own
  // aliases: `BlendMode` declares `colorBurn` AND `color-burn` as two
  // spellings of one value, so kebabbing the first produces the second and
  // the two collide. Twelve of those appeared the moment real union ids
  // existed, and every one was my rule reporting npm's deliberate aliases as
  // a conflict.
  if (surface === "rust" && /[a-z0-9][A-Z]/.test(member)) {
    out.add(member.replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase());
  }

  // An npm `getX` also claims `x` -- UNLESS the same holder declares `x` too.
  //
  // That exclusion exists for exactly one member in the surface:
  // `CanvasTransform` has both `getTransform` and `transform`, which is the
  // case AGENTS.md documents as the one place the Canvas API keeps both. Do
  // not simplify the condition away: without it `getTransform` would claim
  // `transform`, which the real `transform` already claims, and one of the
  // two would pair wrongly.
  //
  // A condition rather than a list of holders, so the next holder to grow a
  // `getX` pairs on its own instead of waiting for someone to remember it.
  if (/^get[A-Z]/.test(member)) {
    const bare = member[3].toLowerCase() + member.slice(4);
    if (!declared?.has(bare)) {
      out.add(bare);
      out.add(camel(bare));
    }
  }

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
export function normalise(id, rules, heritage, declared, surface) {
  // A name that some OTHER holder is aliased onto belongs to that holder; see
  // the note on the holder loop below for why the written name yields to it.
  const claimedByAnother = (name) =>
    Object.entries(rules.owner_aliases ?? {}).some(
      ([from, to]) => to === name && from !== name,
    );

  const sep = id.includes("::") ? "::" : ".";
  const at = id.indexOf(sep);
  if (at === -1) {
    // A bare type id is the holder itself, so it claims whatever name an
    // owner alias gives that holder -- `Cursor` claims `CursorStyle` for the
    // same reason `Cursor.EResize` does. Without this an alias pairs every
    // member of a type and leaves the type unpaired on both surfaces, which
    // is two ids per alias arriving in the manifest as a capability that is
    // plainly present on both sides.
    //
    // Additive, like every other rule: the name as written is still claimed,
    // so a type spelled identically on both surfaces still pairs with no
    // alias in sight.
    const own = new Set();
    const aliased = surface === "rust" ? rules.owner_aliases[id] : undefined;
    if (aliased !== undefined) own.add(aliased);
    if (aliased === undefined || !claimedByAnother(id)) own.add(id);
    return own;
  }
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

  // An owner alias ADDS the holder name it maps to; it does not replace the
  // one written. The file's own header says every rule is additive and gives
  // that as the property which makes a wrong rule safe -- owner aliases were
  // the one rule that did not obey it, and the cost was concrete: an alias
  // keyed off Rust `BlendMode` discarded its existing pairings against npm
  // `BlendMode`, so an alias for the blend unions had to be keyed from the
  // npm side purely to work around this.
  //
  // RUST SIDE ONLY, for the same reason the kebab rule above is. An owner
  // alias says "this Rust holder is that npm holder"; applied to an npm id it
  // renames npm's own holder to another npm holder's name. The declared
  // renames make that visible -- npm declares BOTH `Shader` and
  // `CanvasGradient`, so npm `Shader.x` claimed `CanvasGradient.x` and
  // collided with the real one.
  //
  // THE WRITTEN NAME YIELDS WHEN SOMEONE ELSE IS ALIASED ONTO IT. Additivity
  // and crossing aliases pull against each other, and the surface has a case
  // of each. npm declares `BlendMode` and Rust's `BlendMode` is aliased to
  // `GlobalCompositeOperation`: keeping the written name is the whole point
  // there, since the existing `BlendMode` pairings must survive. npm also
  // declares `TextBaseline`, and Rust's `TextBaseline` is aliased to
  // `CanvasTextBaseline` -- but there npm's `TextBaseline` means Rust's
  // `PlaceholderBaseline`, which claims it through an alias of its own, and
  // the written name collides with it.
  //
  // What separates them is whether any OTHER holder is aliased onto the name.
  // If one is, that holder is what the name denotes and the one aliased away
  // from it has no business still claiming it. Nothing is aliased onto
  // `BlendMode`, so it keeps its name; `PlaceholderBaseline` is aliased onto
  // `TextBaseline`, so Rust's `TextBaseline` gives it up.
  const holders = new Set();
  for (const reachable of reachableHolders(owner, heritage)) {
    const aliased =
      surface === "rust" ? rules.owner_aliases[reachable] : undefined;
    if (aliased !== undefined) holders.add(aliased);
    if (aliased === undefined || !claimedByAnother(reachable)) {
      holders.add(reachable);
    }
  }

  const names = new Set();
  for (const h of holders) {
    for (const m of members) {
      // A member alias is keyed `Holder.member` first and by the bare member
      // name second, and it ADDS a spelling rather than replacing the derived
      // ones -- both halves for the same reason the file's header gives.
      //
      // A bare key is global, and a member name is not: `height` is declared
      // on sixteen Rust holders, so `StrutStyle.height -> heightMultiplier`
      // written bare also rewrites `Canvas.height`, `Image.height` and
      // `Rect.height`, none of which npm spells that way. `F16` and `F32` are
      // worse than they look -- they are `PixelDepth` variants and also
      // function keys on `Key`. Seven of the twenty aliases the population
      // pass proposed collide this way, and a bare-keyed table has nowhere to
      // say which holder was meant.
      //
      // Replacing is the same hazard the overload suffixes above already
      // avoid: it drops the name as written, so an id that would otherwise
      // have paired on its own spelling stops pairing the moment an alias is
      // added for some other holder's member of the same name.
      const spelt = spellings(m, h, rules, declared, surface);
      const aliased =
        rules.member_aliases[owner + "." + m] ?? rules.member_aliases[m];
      if (aliased !== undefined) spelt.add(aliased);
      for (const spelling of spelt) {
        names.add(h + "." + spelling);
      }
    }
  }
  return names;
}

/** The one name to show a reader: the most-derived holder, or the id's own. */
export function displayName(id, rules, heritage, declared, surface) {
  const names = [...normalise(id, rules, heritage, declared, surface)];
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
 * Whether two ids are one capability written as a reader and its setter.
 *
 * npm collapses a getter and a setter of one name into a single item -- its
 * extractor says so -- so it has one `fillStyle` where the crate has
 * `fill_style` and `set_fill_style`. Both must claim the npm name or one of
 * them reports as a gap, and both claiming it is not a collision for the same
 * reason a field and its builder are not. `AGENTS.md` calls these "JS
 * property accessors exported in matching pairs".
 *
 * Narrow on purpose: same holder, and one member exactly `set_` plus the
 * other. 61 of the 81 setters have a reader on the same holder, so without
 * this the setter rule would produce 61 collisions on a correct crate.
 */
function readerAndItsSetter(a, b) {
  const split = (id) => {
    const sep = id.includes("::") ? "::" : ".";
    const at = id.indexOf(sep);
    return at === -1 ? null : [id.slice(0, at), id.slice(at + sep.length)];
  };
  const [x, y] = [split(a), split(b)];
  if (x === null || y === null || x[0] !== y[0]) return false;
  return `set_${x[1]}` === y[1] || `set_${y[1]}` === x[1];
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

  // What each holder declares, so the getter rule can ask whether the bare
  // name is already taken on that holder rather than being told which holders
  // to skip.
  const declaredOn = (ids) => {
    const by = new Map();
    for (const id of ids) {
      const sep = id.includes("::") ? "::" : ".";
      const at = id.indexOf(sep);
      if (at === -1) continue;
      const owner = id.slice(0, at);
      if (!by.has(owner)) by.set(owner, new Set());
      by.get(owner).add(id.slice(at + sep.length));
    }
    return by;
  };
  const rustDeclared = declaredOn(rustIds);
  const npmDeclared = declaredOn(npmIds);
  const ownerOf = (id) => {
    const sep = id.includes("::") ? "::" : ".";
    const at = id.indexOf(sep);
    return at === -1 ? null : id.slice(0, at);
  };

  const rustNames = new Map(
    rustIds.map((id) => [
      id,
      normalise(
        id,
        rules,
        rust.heritage,
        rustDeclared.get(ownerOf(id)),
        "rust",
      ),
    ]),
  );
  const npmNames = new Map(
    npmIds.map((id) => [
      id,
      normalise(id, rules, npm.heritage, npmDeclared.get(ownerOf(id)), "npm"),
    ]),
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
  /**
   * Whether two ids are one type under the two names the crate declares for
   * it -- `Affine` and `DOMMatrix`, say.
   *
   * `src/lib.rs` re-exports seven types under a second name and says so, and
   * the extractor emits both names, so the surface carries two bare ids for
   * one type. Once a bare id claims its alias they both claim the second
   * name, and the collision clause's premise fails: there is no wrong one to
   * pair, because pairing either pairs the same type.
   *
   * Narrow deliberately. It reads the DECLARED renames rather than the
   * merged alias table, so a hand-written alias whose target happens to be a
   * real second type still collides -- which is a genuine clash and the case
   * the check exists for.
   */
  const aDeclaredRenameAndItsTarget = (a, b, renames = {}) =>
    renames[a] === b || renames[b] === a;

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
            !fieldAndItsMethod(other, id) &&
            !readerAndItsSetter(other, id) &&
            !aDeclaredRenameAndItsTarget(other, id, rust.renames)
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

  // AN ENTRY NAMING A BARE HOLDER COVERS THAT HOLDER'S MEMBERS. `rust =
  // ["Key"]` registered the id `Key` and left all 195 `Key::` variants
  // unregistered -- so sixteen live entries covered eighteen holders on paper
  // and a few hundred rows in fact. It is the mirror of the alias defect
  // above: a holder-level alias did not pair the bare type, a holder-level
  // entry did not register the members.
  //
  // This is an entailment rather than a heuristic. A member only ever pairs
  // under `Holder.member`, so if the holder reaches no name whose holder-part
  // exists on the other surface, no member of it can pair. Deliberately NOT
  // the neighbouring rule refused further down -- "an unregistered member
  // whose holder pairs is probably an extra spelling" -- which is a guess
  // about vocabulary. This is the case where the holder pairs with nothing
  // and a human has written down why.
  //
  // Scoped to one-sided entries. An entry with ids on both sides is making a
  // narrower claim about the ids it lists, and widening it to whole holders
  // would silence members nobody looked at.
  //
  // An entry that also lists members OF that holder is not making a holder
  // claim: the author has said which ids they mean, and naming the bare type
  // beside them is naming one more id, not waiving the rest. The readonly
  // geometry halves are the case -- `DOMPoint extends DOMPointReadOnly`, so
  // `x` and `y` pair through the heritage closure and only the four members
  // with no Rust counterpart are listed.
  const holderClaims = (side, otherSide) => {
    const out = new Set();
    for (const entry of manifest) {
      if ((entry[otherSide] ?? []).length > 0) continue;
      const ids = entry[side] ?? [];
      const explicit = new Set(ids.map(holderOf).filter((h) => h !== null));
      for (const id of ids) {
        if (id.includes(".") || id.includes("::")) continue;
        if (!explicit.has(id)) out.add(id);
      }
    }
    return out;
  };
  // Whether ANY member of a holder pairs, computed per holder in one pass.
  //
  // Not read off `holdersOf` below: that deletes an owner when a member pairs
  // and re-adds it for a later member that does not, so its membership
  // depends on the order ids arrive in. For a coverage claim that has to be
  // exact, and "some member paired" is the whole question.
  const holderHasAPair = (ids, names, other) => {
    const out = new Map();
    for (const id of ids) {
      const owner = holderOf(id);
      if (owner === null) continue;
      out.set(owner, (out.get(owner) ?? false) || pairs(names.get(id), other));
    }
    return out;
  };
  const rustPaired = holderHasAPair(rustIds, rustNames, npmByName);
  const npmPaired = holderHasAPair(npmIds, npmNames, rustByName);
  const rustHolderClaims = holderClaims("rust", "npm");
  const npmHolderClaims = holderClaims("npm", "rust");

  // VERIFIED, NOT TRUSTED. The day a member of a claimed holder does pair,
  // the entailment stops holding and the entry is asserting something about
  // the surface that is no longer true. It fails as `stale` so someone
  // revisits it -- the alternative is that a real gap arrives inside a holder
  // whose entry silently keeps absorbing it.
  const coversMembersOf = (holder, claims, paired) =>
    claims.has(holder) && paired.get(holder) !== true;
  for (const [claims, paired, surface] of [
    [rustHolderClaims, rustPaired, "rust"],
    [npmHolderClaims, npmPaired, "npm"],
  ]) {
    for (const holder of claims) {
      if (paired.get(holder) === true) {
        note(
          "stale",
          holder,
          `a capability entry names the ${surface} holder '${holder}' with the other side ` +
            `empty, which covers its members only while none of them pair. One does now, so ` +
            `the entry is claiming something about the surface that has stopped being true. ` +
            `Name the ids it still covers, or revisit the capability.`,
        );
      }
    }
  }

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

  // A PAIRING THE GATE MADE FOR ITSELF, checked. Every other control in this
  // file examines a pairing somebody proposed -- an alias, an entry, a rule --
  // and those get examined precisely because they were proposed. A pairing
  // made by name is asserted by nobody, and when it is wrong it is a false
  // NEGATIVE: it does not add a row, it removes two. Both holders are marked
  // accounted for and the members that do not match report as ordinary
  // residue, which is what residue looks like anyway.
  //
  // The real instance: npm's `TextBaseline` is the paragraph placeholder
  // baseline and its `CanvasTextBaseline` is the canvas one, while Rust's
  // `TextBaseline` is the canvas one. Two of six variants coincide, so the
  // gate paired them and contradicted a finding an earlier audit had already
  // made by hand.
  //
  // THE FLOOR IS ON THE ALTERNATIVE, NOT ON THE PAIR, and that is forced by
  // the data rather than chosen. Measured over the 61 by-name pairs in the
  // real surface, `TextBaseline` overlaps its by-name counterpart by 0.33 --
  // higher than `Canvas` at 0.22 and `Image` at 0.13, both of which pair
  // correctly. So "the pair is weak" flags the good ones and misses this one.
  // What separates it is that a DIFFERENT holder does better: 0.50 against
  // `CanvasTextBaseline`.
  //
  // "Better" alone is not enough either. `Image` scores 0.13 against its own
  // counterpart and 0.14 against `DOMRectInit`, on width and height, so the
  // alternative must also be strong in absolute terms. At 0.40 the check
  // reports two holders on this surface and stays silent on `Canvas`,
  // `Image`, `Path2D`, and on `CanvasFontStretch`, where the alternative ties
  // rather than beats.
  //
  // Overlap is symmetric, over the UNION rather than the smaller side.
  // Dividing by the smaller side scores every two-member holder perfectly,
  // which is how an earlier form of this instrument proposed `Point -> Canvas`.
  const SUSPECT_FLOOR = 0.4;
  // A holder too small to carry evidence is not judged. Overlap over the
  // union already kills the worst of it -- dividing by the smaller side
  // scores every two-member holder perfectly -- but at three names a single
  // coincidence still moves the ratio further than any real signal does, and
  // the first thing this check reported was a three-name fixture where a
  // deliberate rename made a neighbour look like the better match.
  //
  // The floor is on the holder being judged and on the ALTERNATIVE, never on
  // the by-name counterpart: npm's placeholder `TextBaseline` declares just
  // two members, and that smallness is the whole reason the false pair was
  // cheap to make.
  //
  // Both constants are pinned by cases, in the direction each can drift.
  // Lower `SUSPECT_FLOOR` and a deliberately weak alternative starts being
  // reported; raise it and `TextBaseline` stops being. `SUSPECT_MIN_NAMES` is
  // pinned only INDIRECTLY -- lowering it to 2 trips the holder-scoped member
  // alias case, which fails naming something else entirely. If that case is
  // ever changed, this constant loses its only guard.
  const SUSPECT_MIN_NAMES = 4;
  const byHolder = (names) => {
    const out = new Map();
    for (const [, set] of names) {
      for (const n of set) {
        const dot = n.indexOf(".");
        if (dot === -1) continue;
        const h = n.slice(0, dot);
        if (!out.has(h)) out.set(h, new Set());
        out.get(h).add(n.slice(dot + 1));
      }
    }
    return out;
  };
  const overlap = (a, b) => {
    let hit = 0;
    for (const x of a) if (b.has(x)) hit += 1;
    const union = a.size + b.size - hit;
    return union === 0 ? 0 : hit / union;
  };
  const rustHeld = byHolder(rustNames);
  const npmHeld = byHolder(npmNames);
  // A holder the manifest already names is not a surprise: an entry saying
  // one Rust type answers two npm ones is exactly this relation, written
  // down. `Path2DBounds` and `DOMPointInit` both score better than the
  // by-name pair and both are registered, so reporting them is noise.
  const accountedFor = new Set(
    manifest.flatMap((e) =>
      [...(e.rust ?? []), ...(e.npm ?? [])].map((id) =>
        id.includes(".") ? id.slice(0, id.indexOf(".")) : id,
      ),
    ),
  );
  for (const [holder, mine] of rustHeld) {
    const theirs = npmHeld.get(holder);
    if (theirs === undefined) continue;
    const byName = overlap(mine, theirs);
    let best = null;
    for (const [other, members] of npmHeld) {
      if (other === holder || accountedFor.has(other)) continue;
      const score = overlap(mine, members);
      if (score > (best?.score ?? -1)) best = { other, score };
    }
    if (mine.size < SUSPECT_MIN_NAMES) continue;
    if (
      best !== null &&
      best.score > byName &&
      best.score >= SUSPECT_FLOOR &&
      (npmHeld.get(best.other)?.size ?? 0) >= SUSPECT_MIN_NAMES
    ) {
      note(
        "suspect-pair",
        holder,
        `paired with npm '${holder}' by name alone, sharing ${byName.toFixed(2)} of their members, ` +
          `while npm '${best.other}' shares ${best.score.toFixed(2)}. Nobody proposed the by-name pair, ` +
          `so nothing else here examines it -- and if it is wrong, both holders read as accounted for ` +
          `and their real counterparts report as ordinary residue. Confirm it, or alias to '${best.other}'.`,
      );
    }
  }

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
    if (
      !autoRust.has(id) &&
      !namedRust.has(id) &&
      !coversMembersOf(holderOf(id), rustHolderClaims, rustPaired)
    ) {
      classify(id, rustNames.get(id), foldedNpm, "npm", () =>
        describeNearMiss(id, rules, rust.heritage, npmIds, npm.heritage, "npm"),
      );
    }
  }
  // DO NOT classify an unregistered member as "probably fine" because its
  // holder pairs. It was tried and it is the exact inverse of the point.
  //
  // The tempting rule is: an unregistered member whose holder pairs, and
  // which has other paired members beside it, is an extra spelling rather
  // than a gap. It is the obvious shape, and it describes precisely the case
  // this gate exists for -- "I forgot the npm side" happens inside a holder
  // that already works. 318 of the unregistered ids sit in holders that pair,
  // and those are the ones worth reading. The self-test refuses it.
  //
  // The real problem it was reaching for is narrower and is not solvable
  // here: `BlendMode` spells one value `srcOver`, `src-over` AND
  // `source-over`, so two of the three land in `unregistered` looking like
  // capabilities the crate lacks. A spelling fold gets `srcOver` and
  // `src-over` together and never reaches `source-over`, because no
  // transformation makes `src` into `source`.
  //
  // **That distinction needs vocabulary knowledge, so no classifier can carry
  // it.** A version narrowed until it was correct fired on nothing, and a
  // classifier that cannot fire is worse than none: it reads as coverage of
  // the risk it names, and the next reader stops looking. The protection is
  // the manifest, or the heading on the number -- `unregistered` does not
  // mean "missing from the crate", and whoever prints the count has to say so.

  for (const id of npmIds) {
    if (
      !autoNpm.has(id) &&
      !namedNpm.has(id) &&
      !coversMembersOf(holderOf(id), npmHolderClaims, npmPaired)
    ) {
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
  // A bare id is a holder too. Deriving the set from dotted ids alone means a
  // declared type with no members of its own is not counted, and an alias
  // naming it then reports as `stale` -- a rule refused for pointing at
  // something that is right there. Same omission as the one in `normalise`
  // above: the type id IS the holder.
  const npmHolders = new Set(
    npmIds.map((id) => (id.includes(".") ? id.slice(0, id.indexOf(".")) : id)),
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

    // An entry pairing two ids whose HOLDERS do not otherwise pair has to say
    // why. That is the one mechanical check available over a mis-targeted
    // pairing, and there is a real instance: `BlendMode::Copy` was matched
    // against a `GlobalCompositeOperation` spelling because `copy` is a
    // convincing member name, while `BlendMode` pairs with `BlendMode`.
    //
    // The literals cannot be checked -- 51 of them are declared under more
    // than one union type and a report over those would be permanently red --
    // but the holder mismatch is visible without knowing what `copy` means,
    // and it is precisely the part a reviewer skips because the member name
    // matches so well.
    //
    // A `why` is the escape rather than a refusal, because crossing holders
    // is sometimes right: Rust `Rect` answers to `DOMRect` AND to
    // `Path2DBounds`, and only the first pairs by name.
    if (entry.rust.length > 0 && entry.npm.length > 0) {
      const holdersOfSide = (ids) =>
        new Set(ids.map((id) => ownerOf(id)).filter((h) => h !== null));
      const rustHolders = holdersOfSide(entry.rust);
      const npmHolders = holdersOfSide(entry.npm);
      const pairsSomewhere = [...rustHolders].some((rh) =>
        npmHolders.has(rules.owner_aliases[rh] ?? rh),
      );
      const bare = rustHolders.size === 0 || npmHolders.size === 0;
      if (!bare && !pairsSomewhere && !entry.why) {
        note(
          "unexplained",
          entry.name,
          `pairs ${[...rustHolders].join(", ")} with ${[...npmHolders].join(", ")}, ` +
            `which do not otherwise pair. Crossing holders is sometimes right -- one Rust ` +
            `type can answer to two npm ones -- but it is also how a variant gets matched ` +
            `against a convincing member name in the wrong union, so it needs a 'why'.`,
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
    "suspect-pair",
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
    "suspect-pair":
      "two holders paired by name, but a differently-named one matches better",
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
