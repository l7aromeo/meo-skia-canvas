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
  const items = new Map(); // id -> {id, kind, owner}
  const heritage = {};
  const put = (id, kind, owner) => {
    // First writer wins: a name declared twice (declaration merging) is one
    // item to a caller, and the kinds only differ in the report.
    if (!items.has(id)) items.set(id, { id, kind, owner });
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
      put(`${holder}.${name}`, memberKind(member), holder);
      // A union written on the property itself, with no named type to hang an
      // id on: `lineDashFit: "move" | "turn" | "follow"`. Same blind spot as a
      // named union, and five entries in another lane's manifest exist only
      // because these had no ids.
      for (const value of unionMembers(member.type))
        put(`${holder}.${name}.${value}`, "variant", `${holder}.${name}`);
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

  ts.forEachChild(source, (node) => {
    const kind = TOP_LEVEL_KIND[ts.SyntaxKind[node.kind]];
    if (kind) {
      const name = node.name?.getText(source);
      if (!name) return;
      put(name, kind, null);
      for (const member of unionMembers(node.type))
        put(`${name}.${member}`, "variant", name);
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
        put(name, "const", null);
        // `declare const TextDecoration: { readonly Underline: 0x1 }` is a
        // namespace of members; walking only the name loses all of them.
        if (declaration.type && ts.isTypeLiteralNode(declaration.type))
          recordMembers(name, declaration.type.members);
      }
  });

  return { items: [...items.values()], heritage };
};

/** The spec-shaped payload, with the assertions the spec requires. */
export const npmPayload = (entry) => {
  const { items, heritage } = npmSurface(entry);
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
