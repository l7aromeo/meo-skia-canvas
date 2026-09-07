//
// The npm half of the parity gate: every declaration a consumer of
// `lib/index.d.ts` can reach, in the shape `PARITY-SPEC.md` fixes.
//
// ## What counts as reachable
//
// Every top-level declaration, exported or not. `export` is not the test:
// measured on tsc 5.9.3, an unexported top-level `type` and an unexported
// `interface` both import cleanly from the package by name, while an absent
// name fails `TS2305`. Filtering on the keyword would drop 30 interfaces and
// 24 type aliases a caller can write today.
//
// ## Granularity, and why each rule is the way it is
//
// An id has to be stable across runs or every downstream diff is noise, so
// each rule below is chosen for stability first.
//
//   * One item per top-level declaration, `owner` null.
//   * One item per named member, `Holder.member`, attributed to the holder
//     that DECLARES it rather than to every holder that inherits it. These
//     declarations follow WebIDL and split one class across a dozen mixin
//     interfaces, so closing over `extends` would report 744 members where
//     602 are declared -- and, worse, adding a single `extends` clause would
//     change 142 ids at once, making a pure refactor read as a surface
//     change. `PARITY-SPEC.md`'s example writes the inheriting holder
//     (`CanvasRenderingContext2D.fillRect`); this is a deliberate deviation
//     and the closure is recoverable from `heritage` in the output.
//   * Overloads collapse to one item. Two signatures of one name are one
//     capability, and the count of them is not stable.
//   * A getter and a setter of one name collapse to one item, for the same
//     reason: a property is one capability however it is declared.
//   * A constructor or construct signature becomes `Holder.new`, so that
//     "you can build this" has an id rather than being dropped.
//   * Call and index signatures are the one thing with no name to match on.
//     They are emitted as `Holder.()` and `Holder.[]` rather than skipped,
//     because an extractor that quietly drops things is how a parity gate
//     reports agreement it never checked.
//   * Symbol-named members are skipped. A symbol is not a name a caller
//     writes, and the Rust side has nothing that could pair with one.
//   * A string union emits one item per member, `Union.member`. Without this
//     a union is a single id, so `BlendMode` is 1 against the Rust enum's 30
//     and no variant of any enum can ever pair -- 85% of one lane's ids.
//     Spelling aliases (`p3` beside `display-p3`) each get their own id: a
//     caller can write either, and the manifest can pair both to one Rust
//     variant, where folding them would hide which spellings exist.
//   * A nested type literal emits its members too, `Holder.outer.inner`.
//     Same blind spot one level down: `fontStyle: { weight, width, slant }`
//     is three reachable members that were previously invisible.
//   * A union written inline on a property emits its members the same way,
//     `Holder.member.value`. A union on a *parameter* does not, because
//     parameters are not items -- see the note on positional arguments.
//
// Union members are read from the AST rather than by matching quotes. A regex
// over the declaration text finds quoted strings in doc comments as well:
// measured against this file it reports 53 members for `BlendMode` where
// there are 52 -- the extra is the word `"destination"` inside a comment --
// and it reports 2 members for `KeyboardEventProps`, which is a type literal
// with no union in it at all, inventing both from an example in prose.
//
import { createRequire } from "module";
import { readFileSync, writeFileSync } from "fs";

const require = createRequire(import.meta.url);
const ts = require("typescript");

const TOP_LEVEL_KIND = {
  ClassDeclaration: "class",
  InterfaceDeclaration: "interface",
  TypeAliasDeclaration: "type",
  EnumDeclaration: "enum",
  FunctionDeclaration: "function",
};

/** Every reachable declaration in `entry`, plus the heritage graph. */
export const npmSurface = (entry) => {
  const source = ts.createSourceFile(
    entry,
    readFileSync(entry, "utf8"),
    ts.ScriptTarget.Latest,
    true,
  );
  const items = new Map(); // id -> {id, kind, owner, member}
  const heritage = {};
  // `alternatives` records that a value may be any of these types. It is NOT
  // inheritance and must not be walked for member reachability.
  //
  // That sentence is the defect it was created by. These arms were briefly in
  // `heritage`, which a consumer walks so that a member claims its holder's
  // descendants' names -- correct for `extends`, and false here. It made
  // `Canvas.height` and `Image.height` both claim `CanvasDrawable.height`,
  // and since those two holders are unrelated the gate reported 52
  // collisions across two such unions. The check was right; the input was
  // wrong.
  //
  // So a reader wanting to walk this should stop and ask instead. The
  // containment that does hold -- a union of string literals genuinely
  // contains its arms' values -- is in `heritage`, and the test asserts that
  // every arm there carries variants of its own.
  const alternatives = {};
  // `id` is composed from `owner` and `member` rather than the two being
  // recovered from it. A consumer that re-splits an id has to know the
  // separator convention, and that is where several counting errors came
  // from -- the Rust surface uses `.` for a field and `::` for an associated
  // item, and an instrument that split on one saw almost nothing. Both parts
  // are known here at emit time, so throwing them away and asking every
  // reader to reconstruct them is the avoidable half of that.
  const put = (owner, member, kind) => {
    const id = owner === null ? member : `${owner}.${member}`;
    // First writer wins: a name declared twice (declaration merging) is one
    // item to a caller, and the kinds only differ in the report.
    if (!items.has(id))
      items.set(id, {
        id,
        kind,
        owner,
        member: owner === null ? null : member,
      });
  };

  const memberName = (member) => {
    if (
      ts.isConstructorDeclaration(member) ||
      ts.isConstructSignatureDeclaration(member)
    )
      return "new";
    if (ts.isCallSignatureDeclaration(member)) return "()";
    if (ts.isIndexSignatureDeclaration(member)) return "[]";
    if (!member.name) return undefined;
    if (ts.isComputedPropertyName(member.name)) return undefined; // symbol
    return member.name.getText(source).replace(/^["'`]|["'`]$/g, "");
  };
  const memberKind = (member) =>
    ts.isMethodDeclaration(member) ||
    ts.isMethodSignature(member) ||
    ts.isFunctionLike(member)
      ? "method"
      : "property";

  const recordMembers = (holder, members) => {
    for (const member of members) {
      const name = memberName(member);
      if (!name) continue;
      put(holder, name, memberKind(member));
      // A union written on the property itself, with no named type to hang an
      // id on: `lineDashFit: "move" | "turn" | "follow"`. Same blind spot as a
      // named union, and five entries in another lane's manifest exist only
      // because these had no ids.
      for (const value of unionMembers(member.type))
        put(`${holder}.${name}`, value, "variant");
      // A property whose type is written inline carries reachable members of
      // its own. Recursed rather than flattened so the id says where they
      // live, and depth is bounded by the declaration's own nesting.
      if (member.type && ts.isTypeLiteralNode(member.type))
        recordMembers(`${holder}.${name}`, member.type.members);
    }
  };

  // The string members of a union type, or none for anything else. A numeric
  // union (`bitDepth?: 8 | 10 | 12`) is deliberately not read here: its
  // members are not names, and half-parsing them would put digits in the id
  // space where nothing can pair with them.
  const unionMembers = (type) => {
    if (!type) return [];
    const parts = ts.isUnionTypeNode(type) ? type.types : [type];
    return parts
      .filter((part) => ts.isLiteralTypeNode(part))
      .map((part) => part.literal)
      .filter((literal) => ts.isStringLiteral(literal))
      .map((literal) => literal.text);
  };

  // The named types a union is built from, as opposed to its literals.
  const unionReferences = (type) =>
    type && ts.isUnionTypeNode(type)
      ? type.types
          .filter((part) => ts.isTypeReferenceNode(part))
          .map((part) => part.typeName.getText(source))
      : [];

  // Whether a type is itself a union of string literals. This decides which
  // field a union's arms go in, and the distinction is not cosmetic:
  // `heritage` is read as "reaches the parent's members", so an arm listed
  // there lets its members claim the parent's names.
  //
  // That holds for a union of literal unions and fails for a union of types.
  // A value valid in `CompositeExtension` is valid in
  // `GlobalCompositeOperation`, so the containment is real. But
  // `CanvasPatternSource = Canvas | Image | ImageData` says a value may be
  // any of the three, not that the three inherit from it -- `Canvas.height`
  // is a member of one of the things the type may be, not a member of the
  // type. Putting those in `heritage` made `Canvas.height` and `Image.height`
  // both claim `CanvasPatternSource.height`, and since the two holders are
  // unrelated that is 52 collisions across the two such unions.
  const isLiteralUnion = (type) => {
    if (!type) return false;
    const parts = ts.isUnionTypeNode(type) ? type.types : [type];
    return (
      parts.length > 0 &&
      parts.every(
        (part) =>
          ts.isLiteralTypeNode(part) && ts.isStringLiteral(part.literal),
      )
    );
  };

  // Collected first: an arm may be declared after the union that names it,
  // so deciding containment during a single forward pass would depend on
  // declaration order.
  const literalUnions = new Map();
  ts.forEachChild(source, (node) => {
    if (ts.isTypeAliasDeclaration(node))
      literalUnions.set(node.name.getText(source), node.type);
  });

  ts.forEachChild(source, (node) => {
    const kind = TOP_LEVEL_KIND[ts.SyntaxKind[node.kind]];
    if (kind) {
      const name = node.name?.getText(source);
      if (!name) return;
      put(null, name, kind);
      // `type X = { ... }` declares members exactly as `interface X { ... }`
      // does, and a caller cannot tell which was used. The walker descended
      // into interfaces and classes through `node.members`, which a type alias
      // does not have -- its body hangs off `node.type` -- so four of these
      // reported as memberless and 48 declared members never reached the
      // payload. `WindowOptions` was one of them, which is why
      // `WindowSpec -> WindowOptions` was never considered: the holder looked
      // empty, so no member overlap could be measured against it.
      // An intersection is the other half of the same shape:
      // `WindowOptions = { ... } & CanvasOptions` declares its own members and
      // inherits the rest. Unlike a *union* of named types, which confers no
      // membership and goes in `alternatives`, an intersection confers all of
      // it -- so its named arms belong in `heritage`, exactly as `extends`
      // does. This is why `WindowOptions` stayed empty after the type-literal
      // reader landed: its body is an intersection, not a literal.
      for (const part of node.type && ts.isIntersectionTypeNode(node.type)
        ? node.type.types
        : [node.type]) {
        if (part && ts.isTypeLiteralNode(part))
          recordMembers(name, part.members);
        else if (
          part &&
          ts.isTypeReferenceNode(part) &&
          ts.isIntersectionTypeNode(node.type)
        )
          heritage[name] = [
            ...(heritage[name] ?? []),
            part.typeName.getText(source),
          ];
      }
      for (const member of unionMembers(node.type))
        put(name, member, "variant");
      // A union of other unions -- `GlobalCompositeOperation` is
      // `CanvasCompositeOperation | CompositeExtension` -- is the string-side
      // analogue of `extends`, so it goes in the same map rather than being
      // flattened. Emitting its 29 members here as well would duplicate every
      // one of them under a second holder, which is the same reason members
      // are attributed to the interface that declares them.
      const referenced = unionReferences(node.type);
      if (referenced.length) {
        const containment = referenced.every((arm) =>
          isLiteralUnion(literalUnions.get(arm)),
        );
        // Kept either way -- what a union is built from is worth recording --
        // but only containment goes in the field the gate reads as
        // inheritance. `alternatives` says "may be one of these", which is
        // what a union of types actually means.
        if (containment)
          heritage[name] = [...(heritage[name] ?? []), ...referenced];
        else alternatives[name] = referenced;
      }
      if (node.members) recordMembers(name, node.members);
      if (node.heritageClauses)
        heritage[name] = node.heritageClauses.flatMap((clause) =>
          clause.types.map((type) => type.expression.getText(source)),
        );
      return;
    }
    if (ts.isVariableStatement(node))
      for (const declaration of node.declarationList.declarations) {
        const name = declaration.name.getText(source);
        put(null, name, "const");
        // `declare const TextDecoration: { readonly Underline: 0x1 }` is a
        // namespace of members; walking only the name loses all of them.
        if (declaration.type && ts.isTypeLiteralNode(declaration.type))
          recordMembers(name, declaration.type.members);
      }
  });

  return { items: [...items.values()], heritage, alternatives };
};

/** The spec-shaped payload, with the assertions the spec requires. */
export const npmPayload = (entry) => {
  const { items, heritage, alternatives } = npmSurface(entry);
  const sorted = [...items].sort((a, b) =>
    a.id < b.id ? -1 : a.id > b.id ? 1 : 0,
  );

  // Asserted here rather than downstream: an empty or duplicated list must
  // not be mistakable for agreement by whatever consumes this.
  if (sorted.length === 0) throw new Error("npm surface came back empty");
  const seen = new Set();
  for (const item of sorted) {
    if (seen.has(item.id)) throw new Error(`duplicate id: ${item.id}`);
    seen.add(item.id);
  }
  for (let i = 1; i < sorted.length; i++)
    if (!(sorted[i - 1].id < sorted[i].id))
      throw new Error(`not sorted at ${sorted[i].id}`);

  return {
    surface: "npm",
    generated_from: "lib/index.d.ts",
    items: sorted,
    heritage,
    alternatives,
  };
};

if (import.meta.url === `file://${process.argv[1]}`) {
  const [entry, out] = process.argv.slice(2);
  if (!entry || !out) {
    console.error("usage: npm-items.mjs <lib/index.d.ts> <out.json>");
    process.exit(2);
  }
  const payload = npmPayload(entry);
  writeFileSync(out, JSON.stringify(payload, null, 2) + "\n");
  console.log(`npm surface: ${payload.items.length} items -> ${out}`);
}
