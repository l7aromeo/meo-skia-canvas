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
      if (name) put(`${holder}.${name}`, memberKind(member), holder);
    }
  };

  ts.forEachChild(source, (node) => {
    const kind = TOP_LEVEL_KIND[ts.SyntaxKind[node.kind]];
    if (kind) {
      const name = node.name?.getText(source);
      if (!name) return;
      put(name, kind, null);
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
